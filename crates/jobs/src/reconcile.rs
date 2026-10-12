//! Startup recovery reconciler (SPEC v1.3 §6.3B, §13.3A, §14.3, §14.7).
//!
//! Runs **before** the scheduler accepts any work. Three sweeps:
//!
//! 1. **Journal sweep** — every incomplete `import_journal` intent is compared
//!    against the actual bytes on disk. Verified bytes complete the commit;
//!    anything unverified is re-stageable, never silently completed.
//! 2. **Orphan sweep** — app-owned `*.part` files older than the grace period
//!    are removed. Source recordings and committed media are never touched.
//! 3. **Job sweep** — jobs interrupted mid-flight are reclassified
//!    deterministically. A `CloudSending` job with no recorded response becomes
//!    `CloudOutcomeUnknown` and is never auto-dispatched again.
//!
//! The reconciler is idempotent: a second run over the same store changes
//! nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use plaudon_domain::error::AppErrorCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::JobError;
use crate::state::{JobState, TransitionCause};
use crate::store::{DispatchState, JobStore};
use plaudon_persistence::journal::{ImportJournal, ReconcileOutcome};

/// Reconciler configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilerConfig {
    /// Root of the app-owned staging directory (`temp/imports`, §13.4).
    pub staging_root: PathBuf,
    /// Minimum age before an orphaned staging file may be removed.
    pub orphan_grace: Duration,
    /// Maximum bytes hashed per file during verification. Zero means no bound.
    pub max_hash_bytes: u64,
}

impl ReconcilerConfig {
    /// Default policy: one hour of grace, no hashing bound.
    pub fn new(staging_root: impl Into<PathBuf>) -> Self {
        Self {
            staging_root: staging_root.into(),
            orphan_grace: Duration::from_secs(3600),
            max_hash_bytes: 0,
        }
    }

    /// Override the orphan grace period.
    #[must_use]
    pub fn with_orphan_grace(mut self, grace: Duration) -> Self {
        self.orphan_grace = grace;
        self
    }
}

/// What the reconciler did, for logs, tests and the diagnostics view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// Journal intents completed because their bytes verified.
    pub journal_completed: Vec<String>,
    /// Journal intents whose transfer must run again.
    pub journal_restaged: Vec<String>,
    /// Orphaned staging files removed.
    pub orphans_removed: Vec<PathBuf>,
    /// Intents that could not be recovered at all, for example because the
    /// archive target is occupied or the volume is unwritable. Their owning
    /// jobs are marked retryable; nothing is completed.
    pub journal_unrecoverable: Vec<String>,
    /// Jobs reclassified to `FailedRetryable`.
    pub jobs_retryable: Vec<plaudon_domain::ids::JobId>,
    /// Jobs moved to `CloudOutcomeUnknown` (possibly billed, never retried).
    pub jobs_outcome_unknown: Vec<plaudon_domain::ids::JobId>,
    /// Jobs finished cancelling.
    pub jobs_cancelled: Vec<plaudon_domain::ids::JobId>,
    /// `Completed` jobs whose outputs could not be resolved.
    pub completed_without_bytes: Vec<plaudon_domain::ids::JobId>,
}

impl ReconcileReport {
    /// `true` when the reconciler changed nothing.
    pub fn is_noop(&self) -> bool {
        self.journal_completed.is_empty()
            && self.journal_restaged.is_empty()
            && self.journal_unrecoverable.is_empty()
            && self.orphans_removed.is_empty()
            && self.jobs_retryable.is_empty()
            && self.jobs_outcome_unknown.is_empty()
            && self.jobs_cancelled.is_empty()
    }
}

/// Resolves an output reference to bytes on disk so the reconciler can verify
/// that every `Completed` job points at a real, matching asset (§6.3B).
pub trait AssetResolver: Send + Sync + std::fmt::Debug {
    /// Absolute path for a committed output reference, or `None` when the
    /// reference does not denote a file (an identifier, for instance).
    fn resolve(&self, output_kind: &str, reference: &str) -> Option<PathBuf>;
}

/// Resolver backed by the real content-addressed archive (§13.4).
///
/// Delegates path validation to `MediaArchive::absolute`, so traversal,
/// absolute paths and drive-relative prefixes are rejected by the component
/// that owns the archive rather than by a second implementation here.
#[derive(Debug, Clone)]
pub struct ArchiveAssetResolver {
    archive: plaudon_persistence::archive::MediaArchive,
}

impl ArchiveAssetResolver {
    /// Resolve `media_asset` references through `archive`.
    pub fn new(archive: plaudon_persistence::archive::MediaArchive) -> Self {
        Self { archive }
    }
}

impl AssetResolver for ArchiveAssetResolver {
    fn resolve(&self, output_kind: &str, reference: &str) -> Option<PathBuf> {
        if output_kind != "media_asset" {
            return None;
        }
        self.archive.absolute(reference).ok()
    }
}

/// Startup recovery.
#[derive(Debug)]
pub struct Reconciler {
    store: JobStore,
    config: ReconcilerConfig,
    resolver: Option<std::sync::Arc<dyn AssetResolver>>,
    journal: Option<ImportJournal>,
}

impl Reconciler {
    /// Build a reconciler over `store`.
    pub fn new(store: JobStore, config: ReconcilerConfig) -> Self {
        Self {
            store,
            config,
            resolver: None,
            journal: None,
        }
    }

    /// Attach the import journal so interrupted intents are recovered and the
    /// jobs that own them are reclassified.
    #[must_use]
    pub fn with_import_journal(mut self, journal: ImportJournal) -> Self {
        self.journal = Some(journal);
        self
    }

    /// Attach an asset resolver so `Completed` jobs are verified against bytes.
    #[must_use]
    pub fn with_asset_resolver(mut self, resolver: std::sync::Arc<dyn AssetResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Run all three sweeps. Safe to call repeatedly.
    pub async fn run(&self) -> Result<ReconcileReport, JobError> {
        let mut report = ReconcileReport::default();
        self.sweep_journal(&mut report).await?;
        self.sweep_jobs(&mut report).await?;
        self.verify_completed(&mut report).await?;
        Ok(report)
    }

    // -----------------------------------------------------------------------

    /// Delegate intent and orphan recovery to the owner of the import journal.
    ///
    /// `plaudon_persistence::journal::ImportJournal::reconcile` re-hashes what
    /// is actually on disk, completes intents whose bytes verify, quarantines
    /// bytes that do not, abandons intents with no usable bytes, and removes
    /// only app-owned orphan staging files. The job engine's part is to
    /// reclassify the jobs that owned those intents.
    async fn sweep_journal(&self, report: &mut ReconcileReport) -> Result<(), JobError> {
        let Some(journal) = &self.journal else {
            return Ok(());
        };
        let at = plaudon_domain::time::Timestamp::now();
        let incomplete = journal
            .incomplete()
            .await
            .map_err(|e| JobError::Journal(e.to_string()))?;
        let owners: std::collections::HashMap<_, _> = incomplete
            .iter()
            .filter_map(|intent| intent.job_id.map(|job| (intent.id, job)))
            .collect();

        // A single unrecoverable intent must not abort startup: the engine
        // classifies its job as retryable and carries on, so the rest of the
        // library stays usable (§18.6A "fail closed with a retryable
        // classification, not a partial commit").
        let outcomes = match journal.reconcile(at).await {
            Ok(outcomes) => outcomes,
            Err(error) => {
                tracing::warn!(%error, "import journal reconciliation could not finish");
                for intent in &incomplete {
                    report.journal_unrecoverable.push(intent.id.to_string());
                    if let Some(job) = intent.job_id {
                        self.mark_retryable(job, report).await?;
                    }
                }
                return Ok(());
            }
        };

        for outcome in outcomes {
            match outcome {
                ReconcileOutcome::Committed(intent) => {
                    report.journal_completed.push(intent.to_string());
                }
                ReconcileOutcome::Abandoned(intent) | ReconcileOutcome::Quarantined(intent) => {
                    report.journal_restaged.push(intent.to_string());
                    if let Some(job) = owners.get(&intent) {
                        self.mark_retryable(*job, report).await?;
                    }
                }
                ReconcileOutcome::OrphanRemoved(path) => report.orphans_removed.push(path),
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn sweep_jobs(&self, report: &mut ReconcileReport) -> Result<(), JobError> {
        // Any job that was mid-flight when the process died.
        let interrupted = self
            .store
            .jobs_in_states(&[
                JobState::Preflight,
                JobState::RunningLocal,
                JobState::Verifying,
                JobState::Committing,
                JobState::CloudReady,
                JobState::CloudSending,
                JobState::Cancelling,
            ])
            .await?;

        for job in interrupted {
            match job.state {
                // A dispatch with no recorded response is possibly billed. It
                // becomes `CloudOutcomeUnknown` and is never auto-dispatched
                // again; only an explicit, cost-acknowledged user action can
                // move it on (§14.3, §14.7).
                JobState::CloudSending => {
                    let attempts = self.store.attempts(job.id).await?;
                    let answered = attempts
                        .iter()
                        .any(|attempt| attempt.status == DispatchState::Completed);
                    if answered {
                        self.store
                            .transition(
                                job.id,
                                JobState::Verifying,
                                TransitionCause::recovery("response_already_recorded"),
                            )
                            .await?;
                    } else {
                        for attempt in attempts
                            .iter()
                            .filter(|a| a.status == DispatchState::Sending)
                        {
                            self.store
                                .update_attempt(&attempt.id, DispatchState::OutcomeUnknown, None)
                                .await?;
                        }
                        self.store
                            .transition(
                                job.id,
                                JobState::CloudOutcomeUnknown,
                                TransitionCause::recovery("dispatch_without_response"),
                            )
                            .await?;
                        report.jobs_outcome_unknown.push(job.id);
                    }
                }
                // Cancellation was in progress: finish it.
                JobState::Cancelling => {
                    self.store
                        .transition(
                            job.id,
                            JobState::Cancelled,
                            TransitionCause::recovery("cancellation_interrupted"),
                        )
                        .await?;
                    report.jobs_cancelled.push(job.id);
                }
                // Everything else is local work that produced nothing durable.
                // It is retryable; committed checkpoints make the retry cheap.
                _ => {
                    self.store
                        .fail(
                            job.id,
                            JobState::FailedRetryable,
                            // A killed local helper is retryable work, never
                            // an internal error and never a completion.
                            AppErrorCode::HelperFailed,
                            TransitionCause::recovery("interrupted_by_shutdown"),
                        )
                        .await?;
                    report.jobs_retryable.push(job.id);
                }
            }
        }

        // A job parked on a tombstoned recording must never resurrect it.
        for job in self
            .store
            .jobs_in_states(&[JobState::Queued, JobState::FailedRetryable])
            .await?
        {
            if let Some(recording) = job.recording_id {
                if self.store.is_tombstoned(recording).await? {
                    self.store
                        .fail(
                            job.id,
                            JobState::FailedTerminal,
                            AppErrorCode::RecordNotFound,
                            TransitionCause::recovery("recording_deleted_by_user"),
                        )
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Assert the §6.3B invariant: every `Completed` import points at an
    /// existing asset whose hash matches the recorded digest.
    async fn verify_completed(&self, report: &mut ReconcileReport) -> Result<(), JobError> {
        let Some(resolver) = &self.resolver else {
            return Ok(());
        };
        for job in self.store.jobs_in_states(&[JobState::Completed]).await? {
            for output in self.store.outputs(job.id).await? {
                let Some(path) = resolver.resolve(&output.kind, &output.reference) else {
                    continue;
                };
                let actual = hash_if_present(&path, self.config.max_hash_bytes)?;
                let ok = match (&output.sha256, &actual) {
                    (Some(expected), Some(actual)) => expected == actual,
                    (None, Some(_)) => true,
                    _ => false,
                };
                if !ok {
                    report.completed_without_bytes.push(job.id);
                    tracing::error!(
                        job = %job.id,
                        kind = %output.kind,
                        "completed job references missing or mismatched bytes"
                    );
                }
            }
        }
        Ok(())
    }

    async fn mark_retryable(
        &self,
        job: plaudon_domain::ids::JobId,
        report: &mut ReconcileReport,
    ) -> Result<(), JobError> {
        let record = self.store.get(job).await?;
        if record.state.is_terminal() || record.state == JobState::FailedRetryable {
            return Ok(());
        }
        self.store
            .fail(
                job,
                JobState::FailedRetryable,
                AppErrorCode::DownloadInterrupted,
                TransitionCause::recovery("import_not_durable"),
            )
            .await?;
        if !report.jobs_retryable.contains(&job) {
            report.jobs_retryable.push(job);
        }
        Ok(())
    }
}

/// SHA-256 of `path`, or `None` when the file does not exist.
///
/// `max_bytes` bounds how much is read (§13.3A "bounded hash"); zero means the
/// whole file.
fn hash_if_present(path: &Path, max_bytes: u64) -> Result<Option<String>, JobError> {
    use std::io::Read;

    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(JobError::io("open file for verification", error)),
    };
    let mut reader: Box<dyn Read> = if max_bytes == 0 {
        Box::new(file)
    } else {
        Box::new(file.take(max_bytes))
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| JobError::io("read file for verification", e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hex::encode(hasher.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_resolver_rejects_traversal_and_absolute_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive =
            plaudon_persistence::archive::MediaArchive::open(dir.path()).expect("archive");
        let resolver = ArchiveAssetResolver::new(archive);
        assert!(resolver
            .resolve("media_asset", "media/sha256/ab/abcd.wav")
            .is_some());
        assert!(resolver
            .resolve("media_asset", "../../etc/passwd")
            .is_none());
        assert!(resolver.resolve("media_asset", "/etc/passwd").is_none());
        assert!(resolver.resolve("transcript_revision", "rev-1").is_none());
    }

    #[test]
    fn hashing_a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            hash_if_present(&dir.path().join("nope"), 0).expect("hash"),
            None
        );
    }

    #[test]
    fn hashing_is_byte_exact_and_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"hello world").expect("write");
        let full = hash_if_present(&path, 0).expect("hash").expect("some");
        assert_eq!(
            full,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        let bounded = hash_if_present(&path, 5).expect("hash").expect("some");
        assert_ne!(bounded, full);
    }

    #[test]
    fn an_empty_report_is_a_noop() {
        assert!(ReconcileReport::default().is_noop());
    }
}
