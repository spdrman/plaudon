//! Startup recovery sweeps that the happy-path suite does not reach
//! (SPEC v1.3 §6.3B, §14.3, §14.7).

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{advance, enqueue, fixture, import_request, Fixture};

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::QualityClass;
use plaudon_domain::time::Timestamp;
use plaudon_jobs::error::JobError;
use plaudon_jobs::reconcile::{AssetResolver, ReconcileReport, Reconciler, ReconcilerConfig};
use plaudon_jobs::state::{JobState, TransitionCause};
use plaudon_jobs::store::{DispatchState, JobOutput, JobStore};
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::journal::{ImportJournal, PreparedIntent, StagedFile};

/// Resolves `media_asset` references underneath a fixed root, with no
/// validation, so a test can point one at any path it likes.
#[derive(Debug)]
struct FixedResolver {
    root: PathBuf,
}

impl AssetResolver for FixedResolver {
    fn resolve(&self, output_kind: &str, reference: &str) -> Option<PathBuf> {
        if output_kind == "media_asset" {
            Some(self.root.join(reference))
        } else {
            None
        }
    }
}

fn reconciler(f: &Fixture) -> Reconciler {
    Reconciler::new(
        f.store.clone(),
        ReconcilerConfig::new(&f.staging).with_orphan_grace(Duration::from_secs(0)),
    )
}

/// An import journal over the fixture's archive and database.
async fn journal_for(f: &Fixture) -> ImportJournal {
    let archive = MediaArchive::open(&f.archive).expect("archive");
    ImportJournal::new(
        plaudon_persistence::Database::open(&f.db_path)
            .await
            .expect("journal database"),
        archive,
    )
}

/// Stage `bytes` and record an interrupted import intent owned by `job`,
/// returning the staged file alongside the durable intent.
async fn interrupted_intent(
    journal: &ImportJournal,
    job: plaudon_domain::ids::JobId,
    bytes: &[u8],
) -> (StagedFile, PreparedIntent) {
    let mut source = bytes;
    let staged = journal
        .stage(&mut source, "audio/wav")
        .expect("stage bytes");
    let intent = journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            Some(job),
            Timestamp::now(),
        )
        .await
        .expect("record intent");
    (staged, intent)
}

async fn complete_with_output(
    store: &JobStore,
    seed: char,
    output: JobOutput,
) -> plaudon_domain::ids::JobId {
    let job = enqueue(store, seed).await;
    advance(
        store,
        job,
        &[
            JobState::Preflight,
            JobState::RunningLocal,
            JobState::Verifying,
            JobState::Committing,
        ],
    )
    .await;
    store
        .complete(job, vec![output], TransitionCause::system("committed"))
        .await
        .expect("complete");
    job
}

#[test]
fn the_orphan_grace_period_is_configurable_and_a_fresh_report_is_a_noop() {
    let config = ReconcilerConfig::new("/tmp/staging");
    assert_eq!(config.orphan_grace, Duration::from_secs(3600));
    assert_eq!(config.max_hash_bytes, 0);

    let tightened = config.clone().with_orphan_grace(Duration::from_secs(5));
    assert_eq!(tightened.orphan_grace, Duration::from_secs(5));
    assert_ne!(tightened, config);

    assert!(ReconcileReport::default().is_noop());
}

#[tokio::test]
async fn a_report_stops_being_a_noop_once_a_job_was_reclassified() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'a').await;
    advance(
        &f.store,
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;

    let report = reconciler(&f).run().await.expect("recover");

    assert!(!report.is_noop());
    assert_eq!(report.jobs_retryable, vec![job]);
    let record = f.store.get(job).await.expect("load");
    assert_eq!(record.state, JobState::FailedRetryable);
    assert_eq!(
        record.last_error_code,
        Some(AppErrorCode::HelperFailed.as_str().to_string())
    );

    // A second run has nothing left to reclassify.
    let again = reconciler(&f).run().await.expect("recover");
    assert!(again.is_noop(), "{again:?}");
}

#[tokio::test]
async fn an_interrupted_cancellation_is_finished_by_recovery() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'b').await;
    advance(&f.store, job, &[JobState::Preflight, JobState::Cancelling]).await;

    let report = reconciler(&f).run().await.expect("recover");

    assert_eq!(report.jobs_cancelled, vec![job]);
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn a_dispatch_with_a_recorded_response_resumes_at_verification() {
    let f = fixture().await;
    f.store
        .ensure_consent_grant("consent-1", "cloud_inference", "v1", "openrouter", "acme/m")
        .await
        .expect("consent");
    let job = f
        .store
        .enqueue(import_request('c').with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    advance(&f.store, job, &[JobState::Preflight, JobState::CloudReady]).await;
    let attempt = f
        .store
        .record_attempt(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".to_string()),
            "whisper-1",
            None,
            None,
            Some("consent-1".to_string()),
        )
        .await
        .expect("record");
    f.store
        .transition(
            job,
            JobState::CloudSending,
            TransitionCause::system("dispatch").with_consent_verified(),
        )
        .await
        .expect("dispatch");
    f.store
        .update_attempt(
            &attempt,
            DispatchState::Completed,
            Some("req-1".to_string()),
        )
        .await
        .expect("response");

    let report = reconciler(&f).run().await.expect("recover");

    assert!(
        report.jobs_outcome_unknown.is_empty(),
        "an answered dispatch is never possibly billed"
    );
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::Verifying,
        "a dispatch whose response is already on disk is never marked unknown"
    );
}

#[tokio::test]
async fn a_tombstoned_recording_terminates_its_retryable_job() {
    let f = fixture().await;
    let recording = RecordingId::new();
    let job = f
        .store
        .enqueue(import_request('d').with_recording(recording))
        .await
        .expect("enqueue")
        .job_id();
    f.store
        .tombstone(recording, "user_deleted")
        .await
        .expect("tombstone");

    reconciler(&f).run().await.expect("recover");

    let record = f.store.get(job).await.expect("load");
    assert_eq!(record.state, JobState::FailedTerminal);
    assert_eq!(
        record.last_error_code,
        Some(AppErrorCode::RecordNotFound.as_str().to_string())
    );
}

#[tokio::test]
async fn completed_outputs_are_verified_against_the_bytes_they_name() {
    let f = fixture().await;
    let resolver = Arc::new(FixedResolver {
        root: f.archive.clone(),
    });

    let missing = complete_with_output(
        &f.store,
        'e',
        JobOutput::new("media_asset", "missing.wav").verified("a".repeat(64), 7),
    )
    .await;

    let mismatched = complete_with_output(
        &f.store,
        'f',
        JobOutput::new("media_asset", "mismatched.wav").verified("b".repeat(64), 7),
    )
    .await;
    std::fs::write(f.archive.join("mismatched.wav"), b"other bytes").expect("write");

    let undigested =
        complete_with_output(&f.store, 'g', JobOutput::new("media_asset", "plain.wav")).await;
    std::fs::write(f.archive.join("plain.wav"), b"present").expect("write");

    let identifier = complete_with_output(
        &f.store,
        'h',
        JobOutput::new("transcript_revision", "rev-1").verified("c".repeat(64), 7),
    )
    .await;

    let report = reconciler(&f)
        .with_asset_resolver(resolver)
        .run()
        .await
        .expect("recover");

    assert!(report.completed_without_bytes.contains(&missing));
    assert!(report.completed_without_bytes.contains(&mismatched));
    assert!(!report.completed_without_bytes.contains(&undigested));
    assert!(!report.completed_without_bytes.contains(&identifier));
}

#[tokio::test]
async fn an_output_whose_bytes_cannot_be_read_is_a_filesystem_failure() {
    let f = fixture().await;
    // A directory where a file is expected: it opens, but it cannot be read.
    std::fs::create_dir_all(f.archive.join("a-directory.wav")).expect("dir");
    complete_with_output(
        &f.store,
        'i',
        JobOutput::new("media_asset", "a-directory.wav").verified("a".repeat(64), 7),
    )
    .await;

    let error = reconciler(&f)
        .with_asset_resolver(Arc::new(FixedResolver {
            root: f.archive.clone(),
        }))
        .run()
        .await
        .expect_err("must fail");

    assert!(matches!(error, JobError::Io { .. }), "{error:?}");
    assert_eq!(error.code(), AppErrorCode::StorageUnavailable);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn a_reconciler_without_a_journal_or_a_resolver_sweeps_only_jobs() {
    let f = fixture().await;
    enqueue(&f.store, 'j').await;

    let report = reconciler(&f).run().await.expect("recover");

    assert!(
        report.journal_completed.is_empty(),
        "no journal is attached"
    );
    assert!(report.journal_restaged.is_empty(), "no journal is attached");
    assert!(
        report.journal_unrecoverable.is_empty(),
        "no journal is attached"
    );
    assert!(report.orphans_removed.is_empty(), "no journal is attached");
    assert!(
        report.completed_without_bytes.is_empty(),
        "no resolver is attached"
    );
    assert!(report.is_noop());
    assert!(format!("{:?}", reconciler(&f)).contains("Reconciler"));
}

#[tokio::test]
async fn a_quarantined_intent_never_reopens_a_job_that_already_settled() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'k').await;
    advance(
        &f.store,
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;
    let journal = journal_for(&f).await;
    let (staged, intent) = interrupted_intent(&journal, job, b"bytes to be tampered with").await;
    std::fs::write(&staged.path, b"tampered").expect("tamper");
    f.store
        .fail(
            job,
            JobState::FailedTerminal,
            AppErrorCode::IntegrityFailed,
            TransitionCause::system("corrupt"),
        )
        .await
        .expect("fail");

    let report = reconciler(&f)
        .with_import_journal(journal)
        .run()
        .await
        .expect("recover");

    assert_eq!(report.journal_restaged, vec![intent.id.to_string()]);
    assert!(
        report.jobs_retryable.is_empty(),
        "a settled job is never reopened by recovery"
    );
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::FailedTerminal
    );
}

#[tokio::test]
async fn a_job_that_is_already_retryable_is_not_failed_a_second_time() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'l').await;
    advance(
        &f.store,
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;
    let journal = journal_for(&f).await;
    let (staged, intent) = interrupted_intent(&journal, job, b"bytes to be tampered with").await;
    std::fs::write(&staged.path, b"tampered").expect("tamper");
    f.store
        .fail(
            job,
            JobState::FailedRetryable,
            AppErrorCode::DownloadInterrupted,
            TransitionCause::system("interrupted"),
        )
        .await
        .expect("fail");
    let transitions_before = f.store.transitions(job).await.expect("audit").len();

    let report = reconciler(&f)
        .with_import_journal(journal)
        .run()
        .await
        .expect("recover");

    assert_eq!(report.journal_restaged, vec![intent.id.to_string()]);
    assert!(
        report.jobs_retryable.is_empty(),
        "an already retryable job needs no second classification"
    );
    assert_eq!(
        f.store.transitions(job).await.expect("audit").len(),
        transitions_before,
        "recovery wrote no redundant transition"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_intent_the_archive_cannot_accept_is_unrecoverable_and_its_job_retryable() {
    use std::os::unix::fs::PermissionsExt;

    let f = fixture().await;
    let job = enqueue(&f.store, 'm').await;
    advance(
        &f.store,
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;
    let journal = journal_for(&f).await;
    let (_staged, intent) =
        interrupted_intent(&journal, job, b"bytes the volume will refuse").await;

    // The directory the bytes must land in exists but cannot be written to,
    // which is what an unwritable volume looks like to the journal.
    let target = f.archive.join(&intent.target_relative_path);
    let parent = target.parent().expect("target parent").to_path_buf();
    std::fs::create_dir_all(&parent).expect("target dir");
    let original = std::fs::metadata(&parent).expect("metadata").permissions();
    let mut locked = original.clone();
    locked.set_mode(0o500);
    std::fs::set_permissions(&parent, locked).expect("lock dir");

    let report = reconciler(&f)
        .with_import_journal(journal)
        .run()
        .await
        .expect("recover");

    std::fs::set_permissions(&parent, original).expect("unlock dir");

    assert!(
        report
            .journal_unrecoverable
            .contains(&intent.id.to_string()),
        "{report:?}"
    );
    assert!(report.jobs_retryable.contains(&job), "{report:?}");
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::FailedRetryable,
        "an unrecoverable intent fails its job retryably, never completes it"
    );
    assert!(!report.is_noop());
}
