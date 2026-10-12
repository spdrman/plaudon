//! The event boundary between the application layer and a user interface.
//!
//! A render loop cannot await. It wakes, drains whatever happened since the
//! last frame, and paints. This module is that contract:
//!
//! * [`AppEvent`] is everything a user interface can observe — a §15.3
//!   [`UiEvent`], the outcome of a command it sent, or an instruction to
//!   resynchronise.
//! * [`channel`] builds a **bounded, coalescing** queue. Events never carry
//!   state (spec §15.3: an event says *what changed*, the UI refetches), so a
//!   second event with the same kind and subject supersedes the first instead
//!   of growing the backlog. If the backlog still overflows, the oldest UI
//!   events are dropped and a single [`ResyncReason::Overflow`] takes their
//!   place: the UI refetches everything, which is correct by construction.
//!   A command outcome is never dropped — dropping one would strand the
//!   caller with a request that silently never finished.
//! * [`Waker`] is injected, not assumed. `eframe` supplies one that calls
//!   `egui::Context::request_repaint`; tests supply a counting one; a headless
//!   run supplies [`NoopWaker`]. This is what keeps the application layer free
//!   of any user-interface dependency.
//!
//! The sequence contract of `apps/desktop/src/ipc/eventBus.ts` is preserved
//! and enforced here instead: sequences are strictly increasing, a replayed or
//! duplicate sequence is suppressed, a gap forces a full refetch, and a fresh
//! subscriber starts with [`ResyncReason::Subscribed`] because it has not
//! observed anything yet.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::command::{AppResponse, CommandId};
use crate::dto::{EventKind, UiEvent};
use crate::events::EventSink;
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::{JobId, RecordingId};

/// Something a user interface can wake on.
pub trait Waker: Send + Sync + fmt::Debug {
    /// Ask the interface to run another frame.
    ///
    /// Called from the Tokio runtime, never from the render thread, so it must
    /// not block and must not panic.
    fn wake(&self);
}

/// A waker for headless runs: nothing is waiting for a frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopWaker;

impl Waker for NoopWaker {
    fn wake(&self) {}
}

/// A waker that counts, for tests that assert the interface was woken.
#[derive(Debug, Default)]
pub struct CountingWaker {
    wakes: std::sync::atomic::AtomicU64,
}

impl CountingWaker {
    /// A fresh counter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times [`Waker::wake`] has been called.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.wakes.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Waker for CountingWaker {
    fn wake(&self) {
        self.wakes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Why the interface must refetch rather than apply an increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncReason {
    /// The receiver has just been created and has observed nothing.
    Subscribed,
    /// A sequence number was skipped: events were lost before the queue.
    SeqGap,
    /// The queue overflowed and old events were discarded.
    Overflow,
}

/// Everything a user interface can observe.
pub enum AppEvent {
    /// A §15.3 event: something changed, refetch what depends on it.
    Ui(UiEvent),
    /// The outcome of a command previously handed to the dispatcher.
    Response {
        /// The id returned by `Dispatcher::send`.
        id: CommandId,
        /// The command's published name, for logs and tests.
        command: &'static str,
        /// The typed response, or the §15.4 envelope.
        result: Result<AppResponse, ErrorEnvelope>,
    },
    /// The interface's cached state may be stale; refetch everything.
    Resync {
        /// What made the resynchronisation necessary.
        reason: ResyncReason,
        /// How many UI events were discarded (zero for other reasons).
        dropped: u64,
    },
}

/// Never prints an argument or a payload: an event summary may reach a log
/// line, and the §15.4 rule is that no path, transcript or credential does.
impl fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ui(event) => f
                .debug_struct("AppEvent::Ui")
                .field("kind", &event.kind.as_str())
                .field("seq", &event.seq)
                .finish(),
            Self::Response {
                id,
                command,
                result,
            } => f
                .debug_struct("AppEvent::Response")
                .field("id", id)
                .field("command", command)
                .field("ok", &result.is_ok())
                .finish(),
            Self::Resync { reason, dropped } => f
                .debug_struct("AppEvent::Resync")
                .field("reason", reason)
                .field("dropped", dropped)
                .finish(),
        }
    }
}

/// What makes two queued UI events interchangeable.
///
/// An event is an invalidation, so the newest one about the same subject says
/// everything the older one did.
type CoalesceKey = (EventKind, Option<RecordingId>, Option<JobId>);

const fn coalesce_key(event: &UiEvent) -> CoalesceKey {
    (event.kind, event.recording_id, event.job_id)
}

#[derive(Debug)]
struct Queue {
    events: VecDeque<AppEvent>,
    capacity: usize,
    /// Highest sequence accepted so far; 0 means "nothing observed".
    last_seq: u64,
    dropped: u64,
    coalesced: u64,
    duplicates: u64,
    closed: bool,
}

#[derive(Debug)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    waker: Mutex<Arc<dyn Waker>>,
}

impl Shared {
    fn waker(&self) -> Arc<dyn Waker> {
        Arc::clone(&self.waker.lock())
    }

    fn push(&self, event: AppEvent) {
        {
            let mut queue = self.queue.lock();
            if queue.closed {
                return;
            }
            match event {
                AppEvent::Ui(ui) => {
                    if !queue.accept_ui(ui) {
                        return;
                    }
                }
                other => queue.events.push_back(other),
            }
        }
        self.ready.notify_all();
        self.waker().wake();
    }
}

impl Queue {
    /// Returns false when the event changed nothing the receiver can observe.
    fn accept_ui(&mut self, event: UiEvent) -> bool {
        // Duplicate suppression: a sink may re-emit, and a replayed sequence
        // says nothing new.
        if event.seq <= self.last_seq {
            self.duplicates += 1;
            return false;
        }
        // A gap means events were lost before this queue; the only honest
        // recovery is a full refetch.
        let gap = self.last_seq != 0 && event.seq != self.last_seq + 1;
        self.last_seq = event.seq;
        if gap {
            self.push_resync(ResyncReason::SeqGap, 0);
        }

        let key = coalesce_key(&event);
        if let Some(slot) = self.events.iter_mut().find(|queued| match queued {
            AppEvent::Ui(queued) => coalesce_key(queued) == key,
            _ => false,
        }) {
            *slot = AppEvent::Ui(event);
            self.coalesced += 1;
            return true;
        }

        // `capacity` bounds the **UI-event** backlog. A command outcome is a
        // promise to a caller and is never dropped, and at most one
        // resynchronisation marker rides along, so the queue holds at most
        // `capacity + 1 + outstanding responses` entries.
        while self.ui_len() >= self.capacity {
            let Some(index) = self
                .events
                .iter()
                .position(|queued| matches!(queued, AppEvent::Ui(_)))
            else {
                break;
            };
            self.events.remove(index);
            self.dropped += 1;
            self.push_resync(ResyncReason::Overflow, self.dropped);
        }
        self.events.push_back(AppEvent::Ui(event));
        true
    }

    /// How many UI events are queued.
    fn ui_len(&self) -> usize {
        self.events
            .iter()
            .filter(|queued| matches!(queued, AppEvent::Ui(_)))
            .count()
    }

    /// Records one resynchronisation, folding it into a pending one so a
    /// storm of drops cannot itself overflow the queue.
    fn push_resync(&mut self, reason: ResyncReason, dropped: u64) {
        if let Some(AppEvent::Resync {
            reason: queued_reason,
            dropped: queued_dropped,
        }) = self
            .events
            .iter_mut()
            .find(|queued| matches!(queued, AppEvent::Resync { .. }))
        {
            *queued_reason = reason;
            *queued_dropped = dropped.max(*queued_dropped);
            return;
        }
        self.events.push_back(AppEvent::Resync { reason, dropped });
    }
}

/// The writing half: cloneable, non-blocking, safe to call from any task.
#[derive(Debug, Clone)]
pub struct EventSender {
    shared: Arc<Shared>,
}

impl EventSender {
    /// Queue one event and wake the interface.
    pub fn send(&self, event: AppEvent) {
        self.shared.push(event);
    }
}

/// The reading half, owned by the render loop.
#[derive(Debug)]
pub struct EventReceiver {
    shared: Arc<Shared>,
}

impl EventReceiver {
    /// Install the waker the application layer calls when something arrives.
    ///
    /// Injected rather than constructed because the interface's context does
    /// not exist until its first frame.
    pub fn set_waker(&self, waker: Arc<dyn Waker>) {
        *self.shared.waker.lock() = waker;
    }

    /// A sender for this receiver.
    #[must_use]
    pub fn sender(&self) -> EventSender {
        EventSender {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Take everything queued. Never blocks; returns an empty vector at idle,
    /// which is what lets the interface stop repainting.
    pub fn drain(&self) -> Vec<AppEvent> {
        let mut queue = self.shared.queue.lock();
        queue.events.drain(..).collect()
    }

    /// Take the next event, if one is queued.
    pub fn try_next(&self) -> Option<AppEvent> {
        let mut queue = self.shared.queue.lock();
        queue.events.pop_front()
    }

    /// Block until an event arrives or `timeout` elapses.
    ///
    /// For tests and headless callers; a render loop uses its own waker.
    pub fn next_timeout(&self, timeout: Duration) -> Option<AppEvent> {
        let deadline = std::time::Instant::now() + timeout;
        let mut queue = self.shared.queue.lock();
        loop {
            if let Some(event) = queue.events.pop_front() {
                return Some(event);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            self.shared.ready.wait_for(&mut queue, remaining);
        }
    }

    /// Queue depth, for diagnostics and back-pressure tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shared.queue.lock().events.len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Counts of what the queue had to do to stay bounded.
    #[must_use]
    pub fn stats(&self) -> ChannelStats {
        let queue = self.shared.queue.lock();
        ChannelStats {
            dropped: queue.dropped,
            coalesced: queue.coalesced,
            duplicates: queue.duplicates,
            last_seq: queue.last_seq,
        }
    }

    /// Refuse further events; queued ones remain readable.
    pub fn close(&self) {
        self.shared.queue.lock().closed = true;
    }
}

/// What the bounded queue had to do to stay bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelStats {
    /// UI events discarded because the queue was full.
    pub dropped: u64,
    /// UI events superseded by a newer event about the same subject.
    pub coalesced: u64,
    /// Events refused because their sequence had already been observed.
    pub duplicates: u64,
    /// Highest sequence observed.
    pub last_seq: u64,
}

/// Default backlog: one frame's worth of invalidations with room to spare.
///
/// Coalescing means this bounds *distinct* subjects in flight, not the event
/// rate, and it bounds UI events only: command outcomes are never dropped.
pub const DEFAULT_CAPACITY: usize = 256;

/// Build a bounded coalescing channel.
///
/// # Panics
///
/// Panics if `capacity` is zero: a queue that can hold nothing can only drop.
#[must_use]
pub fn channel(capacity: usize, waker: Arc<dyn Waker>) -> (EventSender, EventReceiver) {
    assert!(
        capacity > 0,
        "an event queue needs room for at least one event"
    );
    let mut events = VecDeque::with_capacity(capacity);
    // A subscriber has observed nothing, so its first duty is a full fetch.
    events.push_back(AppEvent::Resync {
        reason: ResyncReason::Subscribed,
        dropped: 0,
    });
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue {
            events,
            capacity,
            last_seq: 0,
            dropped: 0,
            coalesced: 0,
            duplicates: 0,
            closed: false,
        }),
        ready: Condvar::new(),
        waker: Mutex::new(waker),
    });
    (
        EventSender {
            shared: Arc::clone(&shared),
        },
        EventReceiver { shared },
    )
}

/// The [`EventSink`] that puts the §15.3 stream on the channel.
///
/// Replaces the Tauri shell's `TauriEventSink`: same stream, same ordering,
/// no webview.
#[derive(Debug, Clone)]
pub struct ChannelEventSink {
    sender: EventSender,
}

impl ChannelEventSink {
    /// Sink feeding `sender`.
    #[must_use]
    pub fn new(sender: EventSender) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl EventSink for ChannelEventSink {
    async fn emit(&self, event: UiEvent) {
        self.sender.send(AppEvent::Ui(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(seq: u64, kind: EventKind, recording: Option<RecordingId>) -> UiEvent {
        UiEvent {
            seq,
            kind,
            occurred_at: "1970-01-01T00:00:00Z".to_owned(),
            job_id: None,
            recording_id: recording,
            payload: serde_json::Map::new(),
        }
    }

    fn open(capacity: usize) -> (EventSender, EventReceiver) {
        channel(capacity, Arc::new(NoopWaker))
    }

    #[test]
    fn a_fresh_subscriber_is_told_to_fetch_everything() {
        let (_tx, rx) = open(8);
        assert!(matches!(
            rx.try_next(),
            Some(AppEvent::Resync {
                reason: ResyncReason::Subscribed,
                ..
            })
        ));
        assert!(rx.is_empty());
    }

    #[test]
    fn events_about_the_same_subject_coalesce_instead_of_growing_the_queue() {
        let (tx, rx) = open(8);
        let _ = rx.drain();
        for seq in 1..=5 {
            tx.send(AppEvent::Ui(ui(seq, EventKind::LibraryChanged, None)));
        }
        assert_eq!(rx.len(), 1, "one invalidation is all the UI needs");
        let drained = rx.drain();
        match &drained[0] {
            AppEvent::Ui(event) => assert_eq!(event.seq, 5, "the newest event survives"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(rx.stats().coalesced, 4);
    }

    #[test]
    fn different_subjects_do_not_coalesce() {
        let (tx, rx) = open(8);
        let _ = rx.drain();
        tx.send(AppEvent::Ui(ui(
            1,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        tx.send(AppEvent::Ui(ui(
            2,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        assert_eq!(rx.len(), 2);
    }

    #[test]
    fn a_replayed_sequence_is_suppressed() {
        let (tx, rx) = open(8);
        let _ = rx.drain();
        tx.send(AppEvent::Ui(ui(
            1,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        let _ = rx.drain();
        tx.send(AppEvent::Ui(ui(
            1,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        assert!(rx.is_empty());
        assert_eq!(rx.stats().duplicates, 1);
    }

    #[test]
    fn a_sequence_gap_forces_a_full_refetch() {
        let (tx, rx) = open(8);
        let _ = rx.drain();
        tx.send(AppEvent::Ui(ui(
            1,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        tx.send(AppEvent::Ui(ui(
            7,
            EventKind::RecordingImported,
            Some(RecordingId::new()),
        )));
        let events = rx.drain();
        assert!(
            events.iter().any(|event| matches!(
                event,
                AppEvent::Resync {
                    reason: ResyncReason::SeqGap,
                    ..
                }
            )),
            "a gap must be reported: {events:?}"
        );
    }

    #[test]
    fn overflow_drops_old_ui_events_and_asks_for_a_refetch() {
        let (tx, rx) = open(4);
        let _ = rx.drain();
        for seq in 1..=12 {
            tx.send(AppEvent::Ui(ui(
                seq,
                EventKind::LibraryChanged,
                Some(RecordingId::new()),
            )));
        }
        let events = rx.drain();
        let ui_events = events
            .iter()
            .filter(|event| matches!(event, AppEvent::Ui(_)))
            .count();
        assert!(ui_events <= 4, "the ui backlog stayed bounded: {ui_events}");
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::Resync {
                reason: ResyncReason::Overflow,
                ..
            }
        )));
        assert!(rx.stats().dropped > 0);
    }

    #[test]
    fn a_command_outcome_is_never_dropped() {
        let (tx, rx) = open(2);
        let _ = rx.drain();
        tx.send(AppEvent::Response {
            id: CommandId::from_raw(1),
            command: "list_recordings",
            result: Err(plaudon_domain::error::AppError::new(
                plaudon_domain::error::AppErrorCode::InternalError,
                "detail",
            )
            .to_envelope(
                &mut plaudon_domain::error::DiagnosticLog::new(),
                plaudon_domain::time::Timestamp::EPOCH,
            )),
        });
        for seq in 1..=20 {
            tx.send(AppEvent::Ui(ui(
                seq,
                EventKind::LibraryChanged,
                Some(RecordingId::new()),
            )));
        }
        let events = rx.drain();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AppEvent::Response { .. }))
                .count(),
            1,
            "the outcome survived the overflow: {events:?}"
        );
    }

    #[test]
    fn the_injected_waker_is_called_for_every_accepted_event() {
        let waker = Arc::new(CountingWaker::new());
        let (tx, rx) = channel(8, waker.clone());
        let _ = rx.drain();
        tx.send(AppEvent::Ui(ui(
            1,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        tx.send(AppEvent::Ui(ui(
            2,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        assert_eq!(waker.count(), 2);
    }

    #[test]
    fn a_waker_can_be_replaced_after_the_interface_starts() {
        let (tx, rx) = open(8);
        let waker = Arc::new(CountingWaker::new());
        rx.set_waker(waker.clone());
        tx.send(AppEvent::Ui(ui(
            1,
            EventKind::LibraryChanged,
            Some(RecordingId::new()),
        )));
        assert_eq!(waker.count(), 1);
    }

    #[test]
    fn draining_an_idle_channel_yields_nothing_so_the_interface_can_sleep() {
        let (_tx, rx) = open(8);
        let _ = rx.drain();
        assert!(rx.drain().is_empty());
        assert!(rx.next_timeout(Duration::from_millis(1)).is_none());
    }
}
