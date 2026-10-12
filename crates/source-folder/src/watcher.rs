//! The platform watcher (§4.3(2)).
//!
//! Everything this module produces is a **hint**: a path that may be worth
//! probing. Hints are never evidence that a file is complete, which is why the
//! only thing a watcher can do is push into the coalescing queue. The
//! completion barrier decides the rest.
//!
//! Two failure modes are first-class. A platform watcher can tell us it lost
//! events (`notify` reports this as a rescan request), and it can fail
//! outright. Both are recorded as missed changes so the bounded inventory
//! sweep picks the work back up; neither is treated as "nothing happened".

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::clock::Clock;
use crate::config::WatchedFolderConfig;
use crate::error::{FolderError, FolderResult};
use crate::queue::CoalescingQueue;

/// One hint from a filesystem event source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsHint {
    /// A path may have changed.
    Touched(PathBuf),
    /// The source lost events and the folder must be re-enumerated.
    Rescan,
}

/// Anything that can produce hints.
///
/// Implemented by [`NotifyWatcher`] in the application and by a scripted
/// source in tests, so an event sequence can be replayed deterministically
/// without a real filesystem race.
pub trait FsEventSource: std::fmt::Debug + Send {
    /// Take every hint available right now, without blocking.
    fn drain_hints(&mut self) -> Vec<FsHint>;
}

/// A `notify` watcher over one folder.
#[derive(Debug)]
pub struct NotifyWatcher {
    // Dropping the watcher stops the platform subscription, so it is held
    // for its lifetime even though nothing reads it.
    _watcher: RecommendedWatcher,
    hints: Receiver<FsHint>,
}

impl NotifyWatcher {
    /// Start watching the configured root.
    ///
    /// # Errors
    ///
    /// Fails when the platform watcher cannot be created or cannot subscribe
    /// to the folder, for example because the path became unreachable.
    pub fn start(config: &WatchedFolderConfig) -> FolderResult<Self> {
        let (sender, hints) = channel();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                match event {
                    Ok(event) => {
                        if event.need_rescan() {
                            let _ = sender.send(FsHint::Rescan);
                            return;
                        }
                        for path in event.paths {
                            let _ = sender.send(FsHint::Touched(path));
                        }
                    }
                    // A watcher error is a lost-event condition, not a silence.
                    Err(_) => {
                        let _ = sender.send(FsHint::Rescan);
                    }
                }
            })
            .map_err(|error| FolderError::Watcher(error.to_string()))?;

        let mode = if config.is_recursive() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher
            .watch(config.root(), mode)
            .map_err(|error| FolderError::Watcher(error.to_string()))?;

        Ok(Self {
            _watcher: watcher,
            hints,
        })
    }
}

impl FsEventSource for NotifyWatcher {
    fn drain_hints(&mut self) -> Vec<FsHint> {
        let mut hints = Vec::new();
        while let Ok(hint) = self.hints.try_recv() {
            hints.push(hint);
        }
        hints
    }
}

/// A scripted event source for deterministic fixtures.
#[derive(Debug, Default)]
pub struct ScriptedEventSource {
    queued: Vec<FsHint>,
}

impl ScriptedEventSource {
    /// An empty source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one hint.
    pub fn push(&mut self, hint: FsHint) {
        self.queued.push(hint);
    }

    /// Queue a touch for a path.
    pub fn touch(&mut self, path: impl Into<PathBuf>) {
        self.queued.push(FsHint::Touched(path.into()));
    }
}

impl FsEventSource for ScriptedEventSource {
    fn drain_hints(&mut self) -> Vec<FsHint> {
        std::mem::take(&mut self.queued)
    }
}

/// How many hints were absorbed, and whether a rescan is owed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PumpReport {
    /// Hints offered to the queue.
    pub hints: usize,
    /// Hints the queue could not hold.
    pub overflowed: usize,
    /// Hints rejected because the path is not a valid candidate for this
    /// folder: outside the root, a device name, an over-long path.
    pub rejected: usize,
    /// The source asked for a full re-enumeration.
    pub rescan_requested: bool,
}

/// Move every available hint from a source into the queue.
///
/// Paths are validated against the folder root before they are queued, so a
/// hostile or stale event cannot introduce a path the folder does not own.
pub fn pump(
    source: &mut dyn FsEventSource,
    config: &WatchedFolderConfig,
    queue: &CoalescingQueue,
    clock: &dyn Clock,
) -> PumpReport {
    let mut report = PumpReport::default();
    for hint in source.drain_hints() {
        match hint {
            FsHint::Rescan => {
                report.rescan_requested = true;
                queue.record_watcher_overflow(1);
            }
            FsHint::Touched(path) => {
                let Ok(validated) = config.validate_candidate(&path) else {
                    report.rejected += 1;
                    continue;
                };
                if !is_regular_file(validated.as_path()) {
                    report.rejected += 1;
                    continue;
                }
                report.hints += 1;
                if queue.offer(validated.into_path_buf(), clock.now_ms())
                    == crate::queue::OfferOutcome::Overflowed
                {
                    report.overflowed += 1;
                }
            }
        }
    }
    report
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}
