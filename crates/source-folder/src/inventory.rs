//! The bounded periodic inventory sweep (§4.3(2), §4.3(6), §17.4).
//!
//! The watcher is a latency optimisation; the sweep is the correctness
//! guarantee. Kernel event buffers overflow, network shares reconnect without
//! replaying history, and a machine can be asleep while a file is dropped in.
//! A sweep therefore re-enumerates the folder within explicit bounds — depth,
//! entries and a wall-clock budget — and feeds whatever it finds back into the
//! same coalescing queue the watcher uses.
//!
//! The sweep never imports anything itself. It only enqueues, so a long scan
//! of a large folder cannot block an import that is already in flight.
//!
//! A path that has disappeared is reported, never acted on destructively:
//! §4.3(6) says a vanished source marks the association unavailable and the
//! archived recording stays.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::clock::Clock;
use crate::config::WatchedFolderConfig;
use crate::queue::CoalescingQueue;

/// What one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InventoryReport {
    /// Entries inspected.
    pub scanned: usize,
    /// Candidates offered to the queue.
    pub enqueued: usize,
    /// Candidates the queue refused because it was full.
    pub overflowed: usize,
    /// Known paths that are no longer present. Reported, not deleted.
    pub vanished: Vec<PathBuf>,
    /// The entry cap stopped the sweep early.
    pub truncated_by_entries: bool,
    /// The time budget stopped the sweep early.
    pub truncated_by_time: bool,
    /// Entries skipped because they were deeper than the depth cap, or
    /// unreadable (a permission-denied subdirectory, a symlink loop).
    pub skipped: usize,
}

impl InventoryReport {
    /// Whether the sweep saw the whole folder.
    ///
    /// A truncated sweep must not conclude that a missing known path has
    /// vanished, so `vanished` is only populated for a complete sweep.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        !self.truncated_by_entries && !self.truncated_by_time
    }
}

/// Walk a folder within its caps and enqueue every candidate found.
///
/// `known` is the set of paths the library already tracks for this folder;
/// anything in it that the sweep did not see is reported as vanished.
pub fn sweep(
    config: &WatchedFolderConfig,
    queue: &CoalescingQueue,
    known: &BTreeSet<PathBuf>,
    clock: &dyn Clock,
) -> InventoryReport {
    let caps = config.caps();
    let started_ms = clock.now_ms();
    let budget_ms = i64::try_from(caps.inventory_time_budget_ms).unwrap_or(i64::MAX);
    let max_depth = if config.is_recursive() {
        caps.max_depth
    } else {
        1
    };

    let mut report = InventoryReport::default();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();

    // `follow_links(false)` is what makes a symlink loop finite: a link is an
    // entry, never a door. Candidates that happen to be links are rejected
    // later by the path policy.
    let walker = WalkDir::new(config.root())
        .min_depth(1)
        .max_depth(max_depth)
        .follow_links(false);

    for entry in walker {
        if report.scanned >= caps.max_entries {
            report.truncated_by_entries = true;
            break;
        }
        if clock.now_ms().saturating_sub(started_ms) > budget_ms {
            report.truncated_by_time = true;
            break;
        }
        let Ok(entry) = entry else {
            report.skipped += 1;
            continue;
        };
        report.scanned += 1;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if config.validate_candidate(&path).is_err() {
            report.skipped += 1;
            continue;
        }
        seen.insert(path.clone());
        match queue.offer(path, clock.now_ms()) {
            crate::queue::OfferOutcome::Overflowed => report.overflowed += 1,
            crate::queue::OfferOutcome::Queued | crate::queue::OfferOutcome::Coalesced => {
                report.enqueued += 1;
            }
        }
    }

    if report.is_complete() {
        report.vanished = known
            .iter()
            .filter(|path| !seen.contains(*path))
            .cloned()
            .collect();
    }
    report
}

/// Paths a sweep would consider, without touching the queue. Used by status
/// surfaces that want to show what the folder currently contains.
#[must_use]
pub fn list_candidates(config: &WatchedFolderConfig) -> Vec<PathBuf> {
    let max_depth = if config.is_recursive() {
        config.caps().max_depth
    } else {
        1
    };
    WalkDir::new(config.root())
        .min_depth(1)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| config.validate_candidate(path).is_ok())
        .take(config.caps().max_entries)
        .collect()
}

/// Whether a path still exists as a readable file.
#[must_use]
pub fn still_present(path: &Path) -> bool {
    std::fs::metadata(path).map(|meta| meta.is_file()).unwrap_or(false)
}
