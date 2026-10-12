//! Cloud-drive placeholder detection (§4.3, §7.10, App. A.2).
//!
//! A cloud sync client can leave an entry that looks like a complete file and
//! is not: iCloud Drive evicts content and leaves a `.<name>.icloud` brick,
//! OneDrive and Dropbox mark reparse-point stubs with
//! `FILE_ATTRIBUTE_RECALL_ON_OPEN` or `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`.
//! §7.10 is explicit that such entries are never treated as finished
//! recordings, so detection happens *before* the barrier reads anything —
//! reading a dataless file is what triggers an unwanted multi-gigabyte
//! download in the first place.
//!
//! Detection is a trait because neither platform can be simulated honestly by
//! creating a file: tests install [`ScriptedPlaceholderProbe`] and the
//! application installs [`OsPlaceholderProbe`].

use std::collections::HashMap;
use std::fmt;
use std::fs::Metadata;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;

/// Why a path looks like a placeholder rather than bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaceholderReason {
    /// macOS: an iCloud Drive brick file (`.name.ext.icloud`).
    DatalessName,
    /// The file claims a length but occupies no blocks on disk.
    NoAllocatedBlocks,
    /// Windows: an offline or recall-on-access reparse point.
    OfflineAttribute,
    /// The bytes could not be read even though the entry exists.
    ContentUnavailable,
}

impl PlaceholderReason {
    /// Stable, non-secret label for status surfaces and metrics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DatalessName => "dataless_name",
            Self::NoAllocatedBlocks => "no_allocated_blocks",
            Self::OfflineAttribute => "offline_attribute",
            Self::ContentUnavailable => "content_unavailable",
        }
    }
}

/// Whether the bytes behind a path are actually on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderState {
    /// The bytes are local.
    Local,
    /// The entry is a placeholder; importing it would import nothing.
    Placeholder(PlaceholderReason),
}

impl PlaceholderState {
    /// Whether this state permits reading the file as a complete recording.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }
}

/// Decides whether a path is backed by local bytes.
pub trait PlaceholderProbe: fmt::Debug + Send + Sync {
    /// Classify one entry. `metadata` is the stat the barrier already took,
    /// so a probe never needs a second syscall for the common case.
    fn probe(&self, path: &Path, metadata: &Metadata) -> PlaceholderState;
}

/// The platform probe.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsPlaceholderProbe;

impl PlaceholderProbe for OsPlaceholderProbe {
    fn probe(&self, path: &Path, metadata: &Metadata) -> PlaceholderState {
        if is_icloud_brick(path) {
            return PlaceholderState::Placeholder(PlaceholderReason::DatalessName);
        }
        platform_probe(metadata)
    }
}

/// macOS evicts content by replacing `Meeting.m4a` with `.Meeting.m4a.icloud`.
/// The brick is tiny, is not the recording, and its presence means the real
/// bytes are in iCloud.
fn is_icloud_brick(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with('.') && name.ends_with(".icloud")
}

#[cfg(unix)]
fn platform_probe(metadata: &Metadata) -> PlaceholderState {
    use std::os::unix::fs::MetadataExt;

    // A dataless file reports its logical length but owns no blocks. This is
    // the `kMDItemIsDownloaded == false` case observable without Spotlight.
    if metadata.len() > 0 && metadata.blocks() == 0 {
        return PlaceholderState::Placeholder(PlaceholderReason::NoAllocatedBlocks);
    }
    PlaceholderState::Local
}

#[cfg(windows)]
fn platform_probe(metadata: &Metadata) -> PlaceholderState {
    use std::os::windows::fs::MetadataExt;

    /// `FILE_ATTRIBUTE_OFFLINE`
    const OFFLINE: u32 = 0x0000_1000;
    /// `FILE_ATTRIBUTE_RECALL_ON_OPEN`
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    /// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    let attributes = metadata.file_attributes();
    if attributes & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0 {
        return PlaceholderState::Placeholder(PlaceholderReason::OfflineAttribute);
    }
    PlaceholderState::Local
}

#[cfg(not(any(unix, windows)))]
fn platform_probe(_metadata: &Metadata) -> PlaceholderState {
    PlaceholderState::Local
}

/// A probe whose answers are scripted per path.
///
/// Each path may be given a queue of answers, so a test can model "placeholder
/// on the first two probes, hydrated on the third" and assert the retry
/// behaviour §4.3 requires. When a path's queue is empty the default applies.
#[derive(Debug)]
pub struct ScriptedPlaceholderProbe {
    answers: Mutex<HashMap<PathBuf, Vec<PlaceholderState>>>,
    default: PlaceholderState,
}

impl Default for ScriptedPlaceholderProbe {
    fn default() -> Self {
        Self::always_local()
    }
}

impl ScriptedPlaceholderProbe {
    /// A probe that reports every path as local.
    #[must_use]
    pub fn always_local() -> Self {
        Self {
            answers: Mutex::new(HashMap::new()),
            default: PlaceholderState::Local,
        }
    }

    /// A probe whose default answer is a placeholder.
    #[must_use]
    pub fn always_placeholder(reason: PlaceholderReason) -> Self {
        Self {
            answers: Mutex::new(HashMap::new()),
            default: PlaceholderState::Placeholder(reason),
        }
    }

    /// Script the next answers for one path, oldest first.
    pub fn script(&self, path: impl Into<PathBuf>, answers: Vec<PlaceholderState>) {
        let mut queued = answers;
        queued.reverse();
        self.answers.lock().insert(path.into(), queued);
    }
}

impl PlaceholderProbe for ScriptedPlaceholderProbe {
    fn probe(&self, path: &Path, _metadata: &Metadata) -> PlaceholderState {
        let mut answers = self.answers.lock();
        match answers.get_mut(path).and_then(Vec::pop) {
            Some(state) => state,
            None => self.default,
        }
    }
}
