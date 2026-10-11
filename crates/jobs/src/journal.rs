//! Durable import journal (SPEC v1.3 §13.3A, §14.3).
//!
//! SQLite cannot atomically commit a filesystem rename, so a durable import is
//! a six-step journalled sequence:
//!
//! 1. `staged` — write a random private staging file.
//! 2. `hashed` — compute a bounded SHA-256 over the staged bytes.
//! 3. `flushed` — fsync the media file and, where the platform allows, its
//!    directory.
//! 4. `intent_recorded` — write the prepared import-journal intent.
//! 5. `renamed` — atomically rename within the same filesystem.
//! 6. `committed` — transact asset association + job completion, then mark the
//!    journal row committed.
//!
//! Every step is individually crash-injectable, and every interruption point
//! maps to exactly one recovery decision (see [`RecoveryDecision`]).

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Position in the durable import order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalStep {
    /// (1) Bytes are being written to an app-owned staging file.
    Staged,
    /// (2) Staged bytes hashed.
    Hashed,
    /// (3) File and directory metadata flushed.
    Flushed,
    /// (4) Prepared intent durably recorded in SQLite.
    IntentRecorded,
    /// (5) Atomic same-volume rename into the archive completed.
    Renamed,
    /// (6) Association committed and journal row closed.
    Committed,
    /// The intent was deliberately abandoned; its staging file is removable.
    Abandoned,
}

impl JournalStep {
    /// All steps in order.
    pub const ALL: &'static [JournalStep] = &[
        JournalStep::Staged,
        JournalStep::Hashed,
        JournalStep::Flushed,
        JournalStep::IntentRecorded,
        JournalStep::Renamed,
        JournalStep::Committed,
        JournalStep::Abandoned,
    ];

    /// Stable persisted name.
    pub const fn as_str(self) -> &'static str {
        match self {
            JournalStep::Staged => "staged",
            JournalStep::Hashed => "hashed",
            JournalStep::Flushed => "flushed",
            JournalStep::IntentRecorded => "intent_recorded",
            JournalStep::Renamed => "renamed",
            JournalStep::Committed => "committed",
            JournalStep::Abandoned => "abandoned",
        }
    }

    /// Parse a persisted name.
    pub fn parse(raw: &str) -> Option<JournalStep> {
        JournalStep::ALL.iter().copied().find(|s| s.as_str() == raw)
    }

    /// `true` when nothing further will happen to this entry.
    pub const fn is_closed(self) -> bool {
        matches!(self, JournalStep::Committed | JournalStep::Abandoned)
    }
}

impl fmt::Display for JournalStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A journal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub id: String,
    pub job_id: plaudon_domain::ids::JobId,
    pub recording_id: Option<plaudon_domain::ids::RecordingId>,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
    pub expected_sha256: Option<String>,
    pub byte_length: Option<i64>,
    pub step: JournalStep,
    pub created_at: i64,
}

/// Which file, if any, exists on disk and whether its bytes verify.
///
/// Produced by the reconciler by hashing the actual files; never inferred from
/// mtime or size alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedFiles {
    /// Neither staging nor target file exists.
    Neither,
    /// Only the staging file exists and its hash matches `expected_sha256`.
    StagingVerified,
    /// Only the staging file exists and its hash does not match.
    StagingCorrupt,
    /// The target file exists and its hash matches.
    TargetVerified,
    /// The target file exists and its hash does not match.
    TargetCorrupt,
    /// Both exist; the target verifies (rename happened, copy left behind).
    BothTargetVerified,
}

/// What the startup reconciler must do with an interrupted journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDecision {
    /// Nothing to do; the entry is already closed.
    Nothing,
    /// Target bytes verify: finish the rename/commit half that did not run.
    CompleteCommit,
    /// Staging bytes verify but the file is not in place: redo the rename.
    RedoRename,
    /// Bytes are absent or do not verify: the transfer must run again. The
    /// job is marked retryable; nothing is completed.
    ReStage,
    /// Remove the app-owned staging file (grace period permitting) and close
    /// the entry. Only used for abandoned/cancelled intents.
    DiscardStaging,
}

/// Decide what to do with an interrupted journal entry.
///
/// Total over `(step, observed)`; this is the decision table required by issue
/// C1.3/C2.3. Key properties enforced here:
/// * a `Completed` outcome is only ever proposed when the bytes verify;
/// * a corrupt or missing file always yields `ReStage`, never a silent
///   completion;
/// * the source recording is never implicated — only app-owned staging files
///   are ever discarded.
pub fn recovery_decision(step: JournalStep, observed: ObservedFiles) -> RecoveryDecision {
    use JournalStep as J;
    use ObservedFiles as O;
    use RecoveryDecision as D;

    match step {
        J::Committed => D::Nothing,
        J::Abandoned => match observed {
            O::Neither | O::TargetVerified | O::BothTargetVerified => D::Nothing,
            _ => D::DiscardStaging,
        },
        // Before the intent is durable there is nothing authoritative to
        // complete: whatever is on disk is an orphan unless the target already
        // verifies (the rename/commit raced ahead, which cannot happen before
        // the intent, so this is defence in depth).
        J::Staged | J::Hashed | J::Flushed => match observed {
            O::TargetVerified | O::BothTargetVerified => D::CompleteCommit,
            O::StagingVerified => D::RedoRename,
            O::Neither | O::StagingCorrupt | O::TargetCorrupt => D::ReStage,
        },
        // Intent recorded: the expected digest is authoritative.
        J::IntentRecorded => match observed {
            O::TargetVerified | O::BothTargetVerified => D::CompleteCommit,
            O::StagingVerified => D::RedoRename,
            O::Neither | O::StagingCorrupt | O::TargetCorrupt => D::ReStage,
        },
        // Rename recorded: the target must verify, otherwise we re-stage.
        J::Renamed => match observed {
            O::TargetVerified | O::BothTargetVerified => D::CompleteCommit,
            O::StagingVerified => D::RedoRename,
            O::Neither | O::TargetCorrupt | O::StagingCorrupt => D::ReStage,
        },
    }
}

/// `true` when `path` is inside the app-owned staging directory and therefore
/// eligible for orphan cleanup (§13.3A "remove only app-owned orphaned staging
/// files"; §13.4 layout `temp/imports/<job-uuid>.part`).
///
/// Both paths are compared after canonicalising the *staging root* only; the
/// candidate is canonicalised lexically against that root so a non-existent
/// file can still be judged, and any `..` component disqualifies it.
pub fn is_app_owned_staging_file(staging_root: &Path, candidate: &Path) -> bool {
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let root = staging_root.canonicalize();
    let root = match root {
        Ok(root) => root,
        Err(_) => return false,
    };
    let candidate_dir = match candidate.parent() {
        Some(parent) => parent.canonicalize(),
        None => return false,
    };
    let Ok(candidate_dir) = candidate_dir else {
        return false;
    };
    if !candidate_dir.starts_with(&root) {
        return false;
    }
    candidate
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("part"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_steps_round_trip() {
        for step in JournalStep::ALL {
            assert_eq!(JournalStep::parse(step.as_str()), Some(*step));
        }
    }

    #[test]
    fn decision_table_is_total_and_never_completes_unverified_bytes() {
        let observations = [
            ObservedFiles::Neither,
            ObservedFiles::StagingVerified,
            ObservedFiles::StagingCorrupt,
            ObservedFiles::TargetVerified,
            ObservedFiles::TargetCorrupt,
            ObservedFiles::BothTargetVerified,
        ];
        for step in JournalStep::ALL.iter().copied() {
            for observed in observations {
                let decision = recovery_decision(step, observed);
                if decision == RecoveryDecision::CompleteCommit {
                    assert!(
                        matches!(
                            observed,
                            ObservedFiles::TargetVerified | ObservedFiles::BothTargetVerified
                        ),
                        "step {step} observed {observed:?} must not complete"
                    );
                }
            }
        }
    }

    #[test]
    fn corrupt_or_missing_bytes_always_restage() {
        for step in [
            JournalStep::Staged,
            JournalStep::Hashed,
            JournalStep::Flushed,
            JournalStep::IntentRecorded,
            JournalStep::Renamed,
        ] {
            for observed in [
                ObservedFiles::Neither,
                ObservedFiles::StagingCorrupt,
                ObservedFiles::TargetCorrupt,
            ] {
                assert_eq!(
                    recovery_decision(step, observed),
                    RecoveryDecision::ReStage,
                    "step {step} observed {observed:?}"
                );
            }
        }
    }

    #[test]
    fn closed_entries_are_left_alone() {
        for observed in [
            ObservedFiles::Neither,
            ObservedFiles::TargetVerified,
            ObservedFiles::StagingCorrupt,
        ] {
            assert_eq!(
                recovery_decision(JournalStep::Committed, observed),
                RecoveryDecision::Nothing
            );
        }
    }

    #[test]
    fn only_part_files_under_the_staging_root_are_app_owned() {
        let root = tempfile::tempdir().expect("tempdir");
        let staging = root.path().join("imports");
        std::fs::create_dir_all(&staging).expect("mkdir");

        let good = staging.join("1f0c.part");
        assert!(is_app_owned_staging_file(&staging, &good));

        let wrong_ext = staging.join("1f0c.wav");
        assert!(!is_app_owned_staging_file(&staging, &wrong_ext));

        let outside = root.path().join("elsewhere.part");
        assert!(!is_app_owned_staging_file(&staging, &outside));

        let traversal = staging.join("../elsewhere.part");
        assert!(!is_app_owned_staging_file(&staging, &traversal));
    }
}
