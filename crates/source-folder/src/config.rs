//! Configuration of one watched folder (§4.3).

use std::path::{Path, PathBuf};

use plaudon_security::fsguard::{canonicalize_and_validate, PathPolicy, ValidatedPath};

use crate::error::{FolderError, FolderResult};

/// The three per-folder rules of §4.3(7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FolderRule {
    /// Import the audio and stop there.
    #[default]
    ImportOnly,
    /// Import and queue local transcription. Local work only; no egress.
    ImportAndQueueLocalStt,
    /// Import, and refuse every background cloud dispatch that is not covered
    /// by an explicit, valid consent grant.
    AskBeforeCloud,
}

impl FolderRule {
    /// Column value stored in `watched_folders.rule`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImportOnly => "import_only",
            Self::ImportAndQueueLocalStt => "import_and_queue_local_stt",
            Self::AskBeforeCloud => "ask_before_cloud",
        }
    }

    /// Parse a stored value; unknown values fall back to the safest rule.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "import_and_queue_local_stt" => Self::ImportAndQueueLocalStt,
            "ask_before_cloud" => Self::AskBeforeCloud,
            _ => Self::ImportOnly,
        }
    }

    /// Whether this rule may queue local transcription without asking.
    #[must_use]
    pub const fn queues_local_stt(self) -> bool {
        matches!(self, Self::ImportAndQueueLocalStt)
    }
}

/// Bounds on how much work one folder may cause (§4.3(2), §17.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderCaps {
    /// Maximum directory depth an inventory sweep descends.
    pub max_depth: usize,
    /// Maximum entries one sweep inspects.
    pub max_entries: usize,
    /// Wall-clock budget for one sweep.
    pub inventory_time_budget_ms: u64,
    /// Largest file the folder will import.
    pub max_file_bytes: u64,
    /// Free space required beyond the file's own size before staging.
    pub free_space_headroom_bytes: u64,
    /// Maximum paths the coalescing queue holds before it reports overflow.
    pub max_pending: usize,
}

impl Default for FolderCaps {
    fn default() -> Self {
        Self {
            max_depth: 8,
            max_entries: 20_000,
            inventory_time_budget_ms: 5_000,
            max_file_bytes: 8 * 1024 * 1024 * 1024,
            free_space_headroom_bytes: 256 * 1024 * 1024,
            max_pending: 4_096,
        }
    }
}

/// One configured folder.
///
/// The root is canonicalised once, through
/// [`plaudon_security::fsguard::canonicalize_and_validate`], and every
/// candidate is re-validated against it. That is what makes a symlink loop, a
/// Windows device name, an over-long path or a traversal attempt a rejected
/// candidate instead of an exciting filesystem operation (§16.5, App. A.4).
#[derive(Debug, Clone)]
pub struct WatchedFolderConfig {
    root: PathBuf,
    recursive: bool,
    rule: FolderRule,
    quiescence_ms: u64,
    required_stable_probes: u32,
    caps: FolderCaps,
}

impl WatchedFolderConfig {
    /// Validate a user-selected folder.
    ///
    /// # Errors
    ///
    /// Fails when the path is not a usable directory, or when canonicalising
    /// it is refused by the path policy.
    pub fn new(root: &Path, rule: FolderRule) -> FolderResult<Self> {
        let policy = PathPolicy::for_roots([root.to_path_buf()]);
        let validated = canonicalize_and_validate(&policy, root)?;
        let root = validated.into_path_buf();
        if !root.is_dir() {
            return Err(FolderError::UnusableRoot(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        Ok(Self {
            root,
            recursive: false,
            rule,
            quiescence_ms: 2_000,
            required_stable_probes: 2,
            caps: FolderCaps::default(),
        })
    }

    /// Include subdirectories.
    #[must_use]
    pub fn recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }

    /// Set the quiescence window.
    #[must_use]
    pub fn with_quiescence_ms(mut self, quiescence_ms: u64) -> Self {
        self.quiescence_ms = quiescence_ms;
        self
    }

    /// Set how many consecutive unchanged probes the barrier requires.
    #[must_use]
    pub fn with_required_stable_probes(mut self, probes: u32) -> Self {
        self.required_stable_probes = probes.max(1);
        self
    }

    /// Replace the caps.
    #[must_use]
    pub fn with_caps(mut self, caps: FolderCaps) -> Self {
        self.caps = caps;
        self
    }

    /// The canonical root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether subdirectories are included.
    #[must_use]
    pub const fn is_recursive(&self) -> bool {
        self.recursive
    }

    /// The per-folder rule.
    #[must_use]
    pub const fn rule(&self) -> FolderRule {
        self.rule
    }

    /// The quiescence window in milliseconds.
    #[must_use]
    pub const fn quiescence_ms(&self) -> u64 {
        self.quiescence_ms
    }

    /// Consecutive unchanged probes required.
    #[must_use]
    pub const fn required_stable_probes(&self) -> u32 {
        self.required_stable_probes
    }

    /// The caps.
    #[must_use]
    pub const fn caps(&self) -> &FolderCaps {
        &self.caps
    }

    /// Validate a candidate path and prove it resolves inside this root.
    ///
    /// A symlink whose target leaves the root is rejected and never opened; a
    /// symlink loop surfaces as a rejection rather than an infinite walk.
    ///
    /// # Errors
    ///
    /// Fails when the path is rejected by the policy, resolves outside the
    /// root, or is not a candidate this folder may see because the folder is
    /// non-recursive and the path is in a subdirectory.
    pub fn validate_candidate(&self, candidate: &Path) -> FolderResult<ValidatedPath> {
        let policy = PathPolicy::for_roots([self.root.clone()]);
        let validated = canonicalize_and_validate(&policy, candidate)?;
        if !self.recursive {
            let in_root = validated.as_path().parent() == Some(self.root.as_path());
            if !in_root {
                return Err(FolderError::UnusableRoot(format!(
                    "{} is below a subdirectory of a non-recursive folder",
                    validated.as_path().display()
                )));
            }
        }
        Ok(validated)
    }
}
