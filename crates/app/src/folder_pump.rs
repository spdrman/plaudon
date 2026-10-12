//! The watched-folder timer (parity matrix row D18).
//!
//! `plaudon_source_folder::WatchedFolderService` is poll-driven: it imports
//! what appeared in a folder when someone calls `sync`, and nothing else calls
//! it. The Tauri shell never did, so a watched folder only ever imported when
//! the user pressed a control.
//!
//! This is that caller, and it is deliberately **not** the render loop: it is a
//! task on the application's own Tokio runtime that dispatches
//! `source_sync { trigger: background }` for the folders it has been told
//! about. A render thread therefore never waits for a folder sweep, and a
//! sweep still goes through the ordinary validated command path, with the
//! ordinary job, events and rule enforcement.
//!
//! Scope, stated honestly: the pump sweeps every watched folder the library
//! stores that the user has not paused, read back through
//! [`crate::ports::SourcesPort::watched_folder_ids`] on every tick. Reading
//! it per tick rather than once at start-up is what makes a folder added in
//! this session start sweeping without a restart, and what makes a folder
//! added in a *previous* session start sweeping without the user opening the
//! sources screen. No command enumerates folders — the published IPC surface
//! is unchanged — because the timer is inside the application, not a caller
//! of it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaudon_domain::error::AppError;
use tokio::runtime::Handle;

use crate::command::{AppCommand, Dispatcher};
use crate::dto::{SourceSyncRequest, SyncTrigger};
use crate::ports::SourcesPort;

/// How often the timer fires, when the caller does not choose.
///
/// Folder changes arrive as filesystem hints the service debounces; a sweep a
/// minute keeps a folder current without turning the disk into a poll loop.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

/// Where the pump sends the sync it wants.
///
/// A trait, so the timer's behaviour is testable without a runtime, a database
/// or a set of ports.
pub trait SyncRequester: Send + Sync + 'static {
    /// Ask for one background sync of `source_id`.
    fn request_background_sync(&self, source_id: &str);
}

impl SyncRequester for Dispatcher {
    fn request_background_sync(&self, source_id: &str) {
        self.send(AppCommand::SourceSync(SourceSyncRequest {
            source_id: source_id.to_owned(),
            trigger: SyncTrigger::Background,
        }));
    }
}

/// Where the pump learns which folders there are to sweep.
///
/// Also a trait, and for the same reason: the timer is then testable against a
/// list a test writes, with no library open.
#[async_trait]
pub trait FolderRoster: Send + Sync + 'static {
    /// The folders this tick should sweep.
    ///
    /// # Errors
    ///
    /// Returns whatever the library said. The pump logs it and tries again on
    /// the next tick rather than ending: a database that is briefly busy must
    /// not stop a folder being watched for the rest of the session.
    async fn folders_to_sweep(&self) -> Result<Vec<String>, AppError>;
}

#[async_trait]
impl FolderRoster for Arc<dyn SourcesPort> {
    async fn folders_to_sweep(&self) -> Result<Vec<String>, AppError> {
        self.watched_folder_ids().await
    }
}

/// Where the pump's sweeps come from.
///
/// The shipped pump ticks on a Tokio interval ([`IntervalTicks`]); a test
/// hands it ticks it releases itself. That is what makes the timer's
/// behaviour assertable: a sweep happens exactly when the test says so, and
/// the test learns the sweep is over because the pump came back for the next
/// tick — not because a wall-clock sleep expired and the sweep probably
/// happened by now.
#[async_trait]
pub(crate) trait SweepTicks: Send + 'static {
    /// Resolve when the next sweep is due.
    ///
    /// The pump asks again only once the previous sweep has finished, so an
    /// implementation paces the pump as well as timing it.
    async fn next_sweep(&mut self);
}

/// One sweep per interval, the first one a whole interval away.
#[derive(Debug)]
pub(crate) struct IntervalTicks {
    interval: Duration,
    ticker: Option<tokio::time::Interval>,
}

impl IntervalTicks {
    /// Ticks every `interval`.
    pub(crate) fn every(interval: Duration) -> Self {
        Self {
            interval,
            ticker: None,
        }
    }
}

#[async_trait]
impl SweepTicks for IntervalTicks {
    async fn next_sweep(&mut self) {
        let interval = self.interval;
        let ticker = self.ticker.get_or_insert_with(|| {
            // Built on first use, which is inside the pump's task: a Tokio
            // interval wants the runtime's timer, and `start` is called from
            // outside the runtime. Starting it one interval out is also what
            // skips the immediate first tick a plain `interval` would give.
            tokio::time::interval_at(tokio::time::Instant::now() + interval, interval)
        });
        ticker.tick().await;
    }
}

/// A running watched-folder timer.
///
/// Dropping the pump stops it: the task observes the flag on its next tick and
/// returns, so no sweep outlives the application.
#[derive(Clone)]
pub struct WatchedFolderPump {
    stopped: Arc<AtomicBool>,
}

impl std::fmt::Debug for WatchedFolderPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchedFolderPump")
            .field("stopped", &self.stopped.load(Ordering::SeqCst))
            .finish()
    }
}

impl WatchedFolderPump {
    /// Start the timer on `runtime`.
    ///
    /// Returns immediately. The first sweep happens one `interval` later, not
    /// at once: the engine has just reconciled the library at start-up, and a
    /// sweep fired into that has nothing to find.
    #[must_use]
    pub fn start<R: SyncRequester>(
        requester: Arc<R>,
        roster: Arc<dyn FolderRoster>,
        runtime: &Handle,
        interval: Duration,
    ) -> Self {
        Self::start_on(requester, roster, runtime, IntervalTicks::every(interval))
    }

    /// Start the timer on `runtime`, sweeping whenever `ticks` says to.
    ///
    /// The seam behind [`WatchedFolderPump::start`]: driven by ticks a test
    /// owns, every one of the pump's rules is asserted exactly, with no sleep
    /// and no count that moves with how loaded the machine is.
    #[must_use]
    pub(crate) fn start_on<R: SyncRequester, T: SweepTicks>(
        requester: Arc<R>,
        roster: Arc<dyn FolderRoster>,
        runtime: &Handle,
        mut ticks: T,
    ) -> Self {
        let pump = Self {
            stopped: Arc::new(AtomicBool::new(false)),
        };
        let stopped = Arc::clone(&pump.stopped);
        runtime.spawn(async move {
            loop {
                ticks.next_sweep().await;
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                match roster.folders_to_sweep().await {
                    Ok(folders) => {
                        for source_id in folders {
                            requester.request_background_sync(&source_id);
                        }
                    }
                    Err(error) => tracing::warn!(
                        code = error.code().as_str(),
                        "the watched folders could not be listed; the next tick tries again"
                    ),
                }
            }
        });
        pump
    }

    /// Stop the timer. Idempotent.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

impl Drop for WatchedFolderPump {
    fn drop(&mut self) {
        // The last handle going away stops the task; an earlier clone going
        // away must not.
        if Arc::strong_count(&self.stopped) == 1 {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use parking_lot::Mutex;
    use plaudon_domain::error::AppErrorCode;
    use tokio::sync::{mpsc, oneshot};

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        count: AtomicUsize,
    }

    impl SyncRequester for Recorder {
        fn request_background_sync(&self, source_id: &str) {
            self.calls.lock().push(source_id.to_owned());
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A roster a test writes, standing in for the library's folder table.
    #[derive(Default)]
    struct Folders {
        ids: Mutex<Vec<String>>,
        fail: AtomicBool,
    }

    impl Folders {
        fn with(ids: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                ids: Mutex::new(ids.iter().map(|id| (*id).to_owned()).collect()),
                fail: AtomicBool::new(false),
            })
        }

        fn set(&self, ids: &[&str]) {
            *self.ids.lock() = ids.iter().map(|id| (*id).to_owned()).collect();
        }
    }

    #[async_trait]
    impl FolderRoster for Folders {
        async fn folders_to_sweep(&self) -> Result<Vec<String>, AppError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::new(
                    AppErrorCode::InternalError,
                    "the table is busy",
                ));
            }
            Ok(self.ids.lock().clone())
        }
    }

    /// The pump's task has ended, so no sweep can be asked for again.
    #[derive(Debug, PartialEq, Eq)]
    struct PumpEnded;

    /// The clock a test turns by hand.
    struct TestClock {
        requests: mpsc::UnboundedSender<oneshot::Sender<()>>,
    }

    /// The pump's end of [`TestClock`].
    ///
    /// Field order matters: when the task ends both fields go with it, and
    /// `requests` is dropped first, so a test woken by `finished` going away
    /// already sees a closed request channel.
    struct ClockTicks {
        requests: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
        finished: Option<oneshot::Sender<()>>,
    }

    fn clock() -> (TestClock, ClockTicks) {
        let (requests, incoming) = mpsc::unbounded_channel();
        (
            TestClock { requests },
            ClockTicks {
                requests: incoming,
                finished: None,
            },
        )
    }

    impl TestClock {
        /// Release exactly one sweep and resolve once that sweep is over.
        ///
        /// "Over" is observed, not guessed: the pump acknowledges a sweep by
        /// asking for its next tick, which it does only after that sweep's
        /// calls have been recorded. Every assertion below therefore runs
        /// against a pump parked waiting for this test, whatever else the
        /// machine is doing.
        ///
        /// `Err(PumpEnded)` means the task returned instead of sweeping.
        async fn sweep(&self) -> Result<(), PumpEnded> {
            let (finished, over) = oneshot::channel();
            self.requests.send(finished).map_err(|_| PumpEnded)?;
            over.await.map_err(|_| PumpEnded)
        }
    }

    #[async_trait]
    impl SweepTicks for ClockTicks {
        async fn next_sweep(&mut self) {
            // Acknowledging the previous sweep here, before waiting for the
            // next request, is what makes `TestClock::sweep` resolve only
            // once the sweep it released has finished.
            if let Some(finished) = self.finished.take() {
                let _ = finished.send(());
            }
            match self.requests.recv().await {
                Some(finished) => self.finished = Some(finished),
                // The test dropped its clock and wants no further sweep;
                // dropping the pump then ends the task.
                None => std::future::pending().await,
            }
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .expect("a runtime")
    }

    #[test]
    fn the_timer_sweeps_every_folder_the_roster_names_and_nothing_else() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let folders = Folders::with(&["folder-a", "folder-b"]);
        let (clock, ticks) = clock();
        let _pump = WatchedFolderPump::start_on(
            Arc::clone(&recorder),
            Arc::clone(&folders) as Arc<dyn FolderRoster>,
            runtime.handle(),
            ticks,
        );

        runtime
            .block_on(clock.sweep())
            .expect("the pump is running");
        assert_eq!(
            recorder.calls.lock().clone(),
            vec!["folder-a".to_owned(), "folder-b".to_owned()],
            "one tick sweeps each named folder exactly once"
        );

        // The roster is read on every tick, so a folder that stopped being
        // watched stops being swept without the pump being restarted.
        folders.set(&["folder-a"]);
        recorder.calls.lock().clear();
        runtime
            .block_on(clock.sweep())
            .expect("the pump is running");
        assert_eq!(
            recorder.calls.lock().clone(),
            vec!["folder-a".to_owned()],
            "a folder the roster dropped is not swept again"
        );
    }

    #[test]
    fn an_empty_roster_asks_for_nothing() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let (clock, ticks) = clock();
        let _pump = WatchedFolderPump::start_on(
            Arc::clone(&recorder),
            Folders::with(&[]) as Arc<dyn FolderRoster>,
            runtime.handle(),
            ticks,
        );

        for _ in 0..3 {
            runtime
                .block_on(clock.sweep())
                .expect("the pump is running");
        }
        assert_eq!(
            recorder.count.load(Ordering::SeqCst),
            0,
            "the timer never invents work"
        );
    }

    #[test]
    fn a_roster_that_fails_once_does_not_end_the_timer() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let folders = Folders::with(&["folder-a"]);
        folders.fail.store(true, Ordering::SeqCst);
        let (clock, ticks) = clock();
        let _pump = WatchedFolderPump::start_on(
            Arc::clone(&recorder),
            Arc::clone(&folders) as Arc<dyn FolderRoster>,
            runtime.handle(),
            ticks,
        );

        runtime
            .block_on(clock.sweep())
            .expect("the pump is running");
        assert_eq!(
            recorder.count.load(Ordering::SeqCst),
            0,
            "a failed read sweeps nothing"
        );

        folders.fail.store(false, Ordering::SeqCst);
        runtime
            .block_on(clock.sweep())
            .expect("the failed read did not end the task");
        assert_eq!(
            recorder.count.load(Ordering::SeqCst),
            1,
            "the timer is still running once the library answers again"
        );
    }

    #[test]
    fn stopping_the_pump_ends_the_task() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let (clock, ticks) = clock();
        let pump = WatchedFolderPump::start_on(
            Arc::clone(&recorder),
            Folders::with(&["folder-a"]) as Arc<dyn FolderRoster>,
            runtime.handle(),
            ticks,
        );
        runtime
            .block_on(clock.sweep())
            .expect("the pump is running");
        assert_eq!(recorder.count.load(Ordering::SeqCst), 1);

        pump.stop();
        // The tick after the stop sweeps nothing and the task is gone: the
        // clock says so because the pump's end of it was dropped, which is a
        // fact about the task rather than a sweep count that did not happen
        // to grow while the test was looking.
        assert_eq!(
            runtime.block_on(clock.sweep()),
            Err(PumpEnded),
            "the task returns on the first tick after the stop"
        );
        assert_eq!(
            recorder.count.load(Ordering::SeqCst),
            1,
            "a stopped pump sweeps nothing"
        );
        assert_eq!(
            runtime.block_on(clock.sweep()),
            Err(PumpEnded),
            "and it stays stopped"
        );
    }

    #[test]
    fn the_shipped_ticks_wait_a_whole_interval_before_the_first_sweep() {
        // The one test of the wall-clock tick source the application itself
        // runs with. Its assertions are one-sided — a tick can be late on a
        // loaded machine, never early — so load cannot turn it red.
        let runtime = runtime();
        let interval = Duration::from_millis(20);
        runtime.block_on(async move {
            let started = std::time::Instant::now();
            let mut ticks = IntervalTicks::every(interval);
            ticks.next_sweep().await;
            assert!(
                started.elapsed() >= interval,
                "the first sweep is an interval away, not immediate"
            );
            ticks.next_sweep().await;
            assert!(
                started.elapsed() >= interval * 2,
                "and the second is an interval after the first"
            );
        });
    }
}
