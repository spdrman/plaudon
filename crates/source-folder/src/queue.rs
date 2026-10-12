//! The debounce and coalescing queue (§4.3(2), §4.3(6), §17.4).
//!
//! Filesystem events are hints. One `cp` produces a create plus a stream of
//! modifies; an editor's atomic save produces a create, a rename and a remove;
//! a rename storm produces hundreds. All of them are keyed here by canonical
//! path, so a burst collapses into exactly one pending candidate and therefore
//! exactly one import attempt.
//!
//! The queue is also where dropped events become visible. A platform watcher
//! can overflow its kernel buffer and tell us only that it did; the counter
//! kept here is what the bounded inventory sweep uses to decide it has
//! recovery work to do, and what §17.4 reports as "fraction of missed changes
//! recovered by reconciliation".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

/// What happened to an offered hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferOutcome {
    /// A new pending candidate was created.
    Queued,
    /// An existing pending candidate absorbed the hint.
    Coalesced,
    /// The queue is full; the hint was dropped and must be recovered by the
    /// next inventory sweep.
    Overflowed,
}

/// One path waiting to be probed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCandidate {
    /// Canonical path.
    pub path: PathBuf,
    /// When the first hint for this path arrived.
    pub first_hint_ms: i64,
    /// When the most recent hint arrived.
    pub last_hint_ms: i64,
    /// How many hints were absorbed, including the first.
    pub hint_count: u32,
}

#[derive(Debug, Default)]
struct Inner {
    pending: BTreeMap<PathBuf, PendingCandidate>,
    dropped_hints: u64,
}

/// Debounces and deduplicates filesystem hints.
#[derive(Debug, Clone)]
pub struct CoalescingQueue {
    inner: Arc<Mutex<Inner>>,
    debounce_ms: i64,
    max_pending: usize,
}

impl CoalescingQueue {
    /// A queue that holds a path for `debounce_ms` after its last hint.
    #[must_use]
    pub fn new(debounce_ms: u64, max_pending: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            debounce_ms: i64::try_from(debounce_ms).unwrap_or(i64::MAX),
            max_pending: max_pending.max(1),
        }
    }

    /// Offer one hint.
    pub fn offer(&self, path: impl Into<PathBuf>, now_ms: i64) -> OfferOutcome {
        let path = path.into();
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.pending.get_mut(&path) {
            existing.last_hint_ms = now_ms;
            existing.hint_count = existing.hint_count.saturating_add(1);
            return OfferOutcome::Coalesced;
        }
        if inner.pending.len() >= self.max_pending {
            inner.dropped_hints = inner.dropped_hints.saturating_add(1);
            return OfferOutcome::Overflowed;
        }
        inner.pending.insert(
            path.clone(),
            PendingCandidate {
                path,
                first_hint_ms: now_ms,
                last_hint_ms: now_ms,
                hint_count: 1,
            },
        );
        OfferOutcome::Queued
    }

    /// Offer a path an inventory sweep observed.
    ///
    /// A sweep says "this file exists", not "this file just changed", so it
    /// must not restart the debounce window of a path that is already waiting
    /// — otherwise a sweep running on the same cadence as the drain would
    /// keep the queue permanently un-drainable.
    pub fn offer_if_absent(&self, path: impl Into<PathBuf>, now_ms: i64) -> OfferOutcome {
        let path = path.into();
        {
            let inner = self.inner.lock();
            if inner.pending.contains_key(&path) {
                return OfferOutcome::Coalesced;
            }
        }
        self.offer(path, now_ms)
    }

    /// Remove and return every candidate whose debounce window has elapsed.
    ///
    /// A path that is still receiving hints stays in the queue, which is what
    /// stops a long copy from producing an import attempt per write.
    pub fn drain_due(&self, now_ms: i64) -> Vec<PendingCandidate> {
        let mut inner = self.inner.lock();
        let due: Vec<PathBuf> = inner
            .pending
            .values()
            .filter(|candidate| now_ms.saturating_sub(candidate.last_hint_ms) >= self.debounce_ms)
            .map(|candidate| candidate.path.clone())
            .collect();
        due.into_iter()
            .filter_map(|path| inner.pending.remove(&path))
            .collect()
    }

    /// Put a candidate back because the barrier deferred it.
    ///
    /// The hint count is preserved so the coalescing metric still describes
    /// how many events the path produced.
    pub fn requeue(&self, candidate: PendingCandidate, now_ms: i64) {
        let mut inner = self.inner.lock();
        let mut candidate = candidate;
        candidate.last_hint_ms = now_ms;
        inner.pending.insert(candidate.path.clone(), candidate);
    }

    /// Drop a path without processing it.
    pub fn forget(&self, path: &Path) {
        self.inner.lock().pending.remove(path);
    }

    /// Hints discarded because the queue was full.
    #[must_use]
    pub fn dropped_hints(&self) -> u64 {
        self.inner.lock().dropped_hints
    }

    /// Record that the platform watcher itself lost events.
    pub fn record_watcher_overflow(&self, lost: u64) {
        let mut inner = self.inner.lock();
        inner.dropped_hints = inner.dropped_hints.saturating_add(lost);
    }

    /// Reset the dropped-hint counter, returning its value. Called by the
    /// inventory sweep once it has recovered.
    pub fn take_dropped_hints(&self) -> u64 {
        let mut inner = self.inner.lock();
        std::mem::take(&mut inner.dropped_hints)
    }

    /// Number of pending candidates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().pending.len()
    }

    /// Whether anything is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_for_one_path_collapses_into_one_candidate() {
        let queue = CoalescingQueue::new(50, 16);
        assert_eq!(queue.offer("/tmp/a.wav", 0), OfferOutcome::Queued);
        for tick in 1..20 {
            assert_eq!(queue.offer("/tmp/a.wav", tick), OfferOutcome::Coalesced);
        }
        assert_eq!(queue.len(), 1);
        assert!(queue.drain_due(60).is_empty(), "still inside the window");
        let due = queue.drain_due(100);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].hint_count, 20);
    }

    #[test]
    fn an_overflowing_queue_counts_what_it_dropped() {
        let queue = CoalescingQueue::new(0, 2);
        assert_eq!(queue.offer("/tmp/a.wav", 0), OfferOutcome::Queued);
        assert_eq!(queue.offer("/tmp/b.wav", 0), OfferOutcome::Queued);
        assert_eq!(queue.offer("/tmp/c.wav", 0), OfferOutcome::Overflowed);
        assert_eq!(queue.dropped_hints(), 1);
        assert_eq!(queue.take_dropped_hints(), 1);
        assert_eq!(queue.dropped_hints(), 0);
    }
}
