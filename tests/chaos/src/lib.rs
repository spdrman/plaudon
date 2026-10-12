//! Crash-injection and interruption suites for durable state transitions.
//!
//! The import suite drives the real [`plaudon_persistence::journal`]
//! implementation in a child process, kills that process with `abort` at a
//! chosen step of the spec 13.3A durable order, then runs the ordinary
//! startup reconciler in the parent. Nothing in the production code path
//! knows the tests exist: the steps are separate public methods and the test
//! simply stops calling them.

pub mod job_engine;

/// The crash points of the export, backup, restore and deletion paths
/// (spec 18.6A "Archive" and "Portable backup" clusters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExportStep {
    /// Half of a transcript export has been written to its temporary file.
    ExportPartialWrite,
    /// Half of an archive has been written to its temporary file.
    BackupPartialWrite,
    /// A restore extracted and verified every entry into private staging.
    RestoreAfterStaging,
    /// A restore committed the media blobs but not the database rows.
    RestoreAfterMediaCommit,
    /// A deletion committed its journal row and nothing else.
    DeleteAfterJournal,
    /// A deletion removed its rows but has not unlinked the blobs.
    DeleteAfterRows,
}

impl ExportStep {
    /// Every step, in order.
    pub const ALL: [Self; 6] = [
        Self::ExportPartialWrite,
        Self::BackupPartialWrite,
        Self::RestoreAfterStaging,
        Self::RestoreAfterMediaCommit,
        Self::DeleteAfterJournal,
        Self::DeleteAfterRows,
    ];

    /// Parse the command-line form used by the child process.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "export-partial-write" => Self::ExportPartialWrite,
            "backup-partial-write" => Self::BackupPartialWrite,
            "restore-after-staging" => Self::RestoreAfterStaging,
            "restore-after-media-commit" => Self::RestoreAfterMediaCommit,
            "delete-after-journal" => Self::DeleteAfterJournal,
            "delete-after-rows" => Self::DeleteAfterRows,
            _ => return None,
        })
    }

    /// The command-line form used by the child process.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExportPartialWrite => "export-partial-write",
            Self::BackupPartialWrite => "backup-partial-write",
            Self::RestoreAfterStaging => "restore-after-staging",
            Self::RestoreAfterMediaCommit => "restore-after-media-commit",
            Self::DeleteAfterJournal => "delete-after-journal",
            Self::DeleteAfterRows => "delete-after-rows",
        }
    }
}

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-chaos-tests";

/// The six durable import steps of spec 13.3A.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportStep {
    /// Nothing has happened yet.
    BeforeStaging,
    /// The staging file exists and has been hashed and flushed.
    AfterStaging,
    /// The prepared intent is durable.
    AfterIntent,
    /// The staged file has been renamed into the archive.
    AfterRename,
    /// The asset association is committed.
    AfterCommit,
}

impl ImportStep {
    /// Every step, in order.
    pub const ALL: [Self; 5] = [
        Self::BeforeStaging,
        Self::AfterStaging,
        Self::AfterIntent,
        Self::AfterRename,
        Self::AfterCommit,
    ];

    /// Parse the command-line form used by the child process.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "before-staging" => Self::BeforeStaging,
            "after-staging" => Self::AfterStaging,
            "after-intent" => Self::AfterIntent,
            "after-rename" => Self::AfterRename,
            "after-commit" => Self::AfterCommit,
            _ => return None,
        })
    }

    /// The command-line form used by the child process.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeStaging => "before-staging",
            Self::AfterStaging => "after-staging",
            Self::AfterIntent => "after-intent",
            Self::AfterRename => "after-rename",
            Self::AfterCommit => "after-commit",
        }
    }
}
