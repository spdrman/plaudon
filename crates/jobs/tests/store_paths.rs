//! Error, rejection and boundary paths of the transactional job store
//! (SPEC v1.3 §13.1, §14.1-§14.3).
//!
//! Every assertion is about an observable outcome: the returned error variant,
//! the committed row, or the durable event the operation did or did not write.

mod common;

use common::{advance, corrupt_write, enqueue, fixture, import_request, raw_write};

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_jobs::error::JobError;
use plaudon_jobs::events::{EventKind, EventPayload, PendingEvent};
use plaudon_jobs::key::{
    ImportJobKey, ProcessingJobKey, ProcessingStage, ReprocessToken, SourceKind,
};
use plaudon_jobs::scheduler::{ResourceClass, WaitReason};
use plaudon_jobs::state::{JobState, JobTransitionError, TransitionCause};
use plaudon_jobs::store::{
    DispatchState, EnqueueOutcome, EnqueueRequest, JobOutput, JobStore, StageAction,
};
use plaudon_jobs::writer::Database;

// ---------------------------------------------------------------------------
// Schema verification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn opening_a_store_over_an_unmigrated_database_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(dir.path().join("empty.sqlite"), 2)
        .await
        .expect("open");

    let error = JobStore::open(db).await.expect_err("must refuse");
    let JobError::CorruptRecord(detail) = &error else {
        panic!("expected a corrupt-record refusal, got {error:?}");
    };
    assert!(detail.contains("missing the required column"), "{detail}");
    assert_eq!(error.code(), AppErrorCode::IntegrityFailed);
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn a_jobs_table_missing_one_engine_column_is_named_in_the_refusal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(dir.path().join("partial.sqlite"), 2)
        .await
        .expect("open");
    db.writer()
        .submit(move |mut conn| {
            Box::pin(async move {
                // Every engine column except `last_error_code`.
                sqlx::query(
                    "CREATE TABLE jobs (id TEXT PRIMARY KEY, job_type TEXT, state TEXT,
                         priority INTEGER, retry_count INTEGER, job_key TEXT,
                         config_snapshot_json TEXT, input_hash TEXT, created_at INTEGER,
                         started_at INTEGER, finished_at INTEGER)",
                )
                .execute(&mut *conn)
                .await
                .map_err(|e| JobError::storage("create partial jobs table", e))?;
                Ok(())
            })
        })
        .await
        .expect("create table");

    let error = JobStore::open(db).await.expect_err("must refuse");
    let JobError::CorruptRecord(detail) = &error else {
        panic!("expected a corrupt-record refusal, got {error:?}");
    };
    assert!(detail.contains("last_error_code"), "{detail}");
}

#[tokio::test]
async fn migrating_into_a_directory_that_does_not_exist_reports_a_migration_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A regular file where a directory would have to be: the parent cannot be
    // created, so the migration runner never gets a database.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").expect("blocker");
    let missing = blocker.join("app.sqlite");

    let error = JobStore::migrate(&missing).await.expect_err("must fail");
    assert!(matches!(error, JobError::Migration(_)), "{error:?}");
    assert_eq!(error.code(), AppErrorCode::MigrationFailed);
    assert!(!error.is_retryable());

    // `open_at` surfaces the same failure instead of opening a half-built file.
    let error = JobStore::open_at(&missing).await.expect_err("must fail");
    assert!(matches!(error, JobError::Migration(_)), "{error:?}");
}

#[tokio::test]
async fn migration_and_schema_verification_are_idempotent() {
    let f = fixture().await;
    JobStore::migrate(&f.db_path).await.expect("second migrate");
    f.store.verify_schema().await.expect("schema still valid");
    assert_eq!(f.store.path(), f.db_path);
    assert_eq!(format!("{:?}", f.store), "JobStore");
}

// ---------------------------------------------------------------------------
// Enqueue / idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_duplicate_key_deduplicates_without_writing_a_second_event() {
    let f = fixture().await;
    let first = f.store.enqueue(import_request('a')).await.expect("first");
    let events_after_first = f.store.events().count().await.expect("count");

    let second = f.store.enqueue(import_request('a')).await.expect("second");

    assert!(first.is_created());
    assert!(!second.is_created());
    assert_eq!(first.job_id(), second.job_id());
    assert!(matches!(second, EnqueueOutcome::Deduplicated { .. }));
    assert_eq!(
        f.store.events().count().await.expect("count"),
        events_after_first,
        "a deduplicated enqueue must not append to the durable log"
    );
}

#[tokio::test]
async fn every_optional_enqueue_dimension_is_persisted_verbatim() {
    let f = fixture().await;
    let original = enqueue(&f.store, 'o').await;
    let recording = RecordingId::new();
    f.store
        .ensure_consent_grant("consent-1", "cloud_inference", "v1", "openrouter", "acme/m")
        .await
        .expect("consent row");

    let request = EnqueueRequest::new(
        "transcribe",
        ProcessingJobKey::new(
            "b".repeat(64),
            ProcessingStage::Transcribe,
            "openrouter",
            "m",
        ),
        ResourceClass::CloudDispatch,
    )
    .with_config_snapshot(serde_json::json!({ "engine": "openrouter" }))
    .with_input_hash("c".repeat(64))
    .with_recording(recording)
    .with_consent("consent-1")
    .reprocessing(original);

    let job = f.store.enqueue(request).await.expect("enqueue").job_id();
    let record = f.store.get(job).await.expect("load");

    assert_eq!(record.job_type, "transcribe");
    assert_eq!(record.key_kind, "processing");
    assert_eq!(record.resource_class, ResourceClass::CloudDispatch);
    assert_eq!(record.config_snapshot["engine"], "openrouter");
    assert_eq!(record.input_hash, Some("c".repeat(64)));
    assert_eq!(record.recording_id, Some(recording));
    assert_eq!(record.consent_id, Some("consent-1".to_string()));
    assert_eq!(record.reprocess_of, Some(original));
    assert!(!record.is_reprocess, "a reprocess_of link is not a nonce");
    assert!(!record.dispatched);
    assert!(!record.possibly_billed());
    assert_eq!(record.priority, 0);
}

#[tokio::test]
async fn an_explicit_reprocess_nonce_is_persisted_as_a_reprocess() {
    let f = fixture().await;
    let key = ImportJobKey::by_content(SourceKind::LocalFile, "d".repeat(64))
        .with_reprocess(ReprocessToken::new());
    let job = f
        .store
        .enqueue(EnqueueRequest::new("import", key, ResourceClass::LocalDisk))
        .await
        .expect("enqueue")
        .job_id();

    assert!(f.store.get(job).await.expect("load").is_reprocess);
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transitioning_a_job_that_does_not_exist_reports_it_as_missing() {
    let f = fixture().await;
    let ghost = JobId::new();

    let error = f
        .store
        .transition(ghost, JobState::Preflight, TransitionCause::system("x"))
        .await
        .expect_err("must fail");
    assert!(matches!(error, JobError::JobNotFound(id) if id == ghost));
    assert_eq!(error.code(), AppErrorCode::RecordNotFound);

    let error = f.store.get(ghost).await.expect_err("must fail");
    assert!(matches!(error, JobError::JobNotFound(id) if id == ghost));
}

#[tokio::test]
async fn a_terminal_job_refuses_every_further_transition() {
    let f = fixture().await;
    let job = enqueue(&f.store, 't').await;
    advance(
        &f.store,
        job,
        &[
            JobState::Preflight,
            JobState::Cancelling,
            JobState::Cancelled,
        ],
    )
    .await;

    let error = f
        .store
        .transition(job, JobState::Queued, TransitionCause::user("undo"))
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::AlreadyTerminal {
            state: JobState::Cancelled
        })
    ));
    assert_eq!(error.code(), AppErrorCode::Conflict);
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn a_requeue_after_a_retryable_failure_increments_the_retry_count() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'r').await;
    advance(&f.store, job, &[JobState::Preflight]).await;
    assert_eq!(f.store.get(job).await.expect("load").retry_count, 0);

    for expected in 1..=3 {
        f.store
            .fail(
                job,
                JobState::FailedRetryable,
                AppErrorCode::HelperFailed,
                TransitionCause::system("helper_died"),
            )
            .await
            .expect("fail");
        let requeued = f
            .store
            .transition(job, JobState::Queued, TransitionCause::system("retry"))
            .await
            .expect("requeue");
        assert_eq!(requeued.retry_count, expected);
        assert_eq!(
            f.store.get(job).await.expect("load").last_error_code,
            Some(AppErrorCode::HelperFailed.as_str().to_string())
        );
        advance(&f.store, job, &[JobState::Preflight]).await;
    }

    // A transition into `Queued` from anywhere else leaves the counter alone.
    f.store
        .park(
            job,
            WaitReason::NetworkUnavailable,
            TransitionCause::system("offline"),
        )
        .await
        .expect("park");
    let resumed = f
        .store
        .transition(job, JobState::Queued, TransitionCause::system("resume"))
        .await
        .expect("resume");
    assert_eq!(resumed.retry_count, 3);
    assert_eq!(resumed.wait_reason, None, "resuming clears the wait reason");
}

#[tokio::test]
async fn parking_records_the_reason_and_the_matching_wait_state() {
    let f = fixture().await;
    for (seed, reason) in [
        ('1', WaitReason::InsufficientDisk),
        ('2', WaitReason::InsufficientMemory),
        ('3', WaitReason::ModelAssetMissing),
        ('4', WaitReason::SourceUnavailable),
        ('5', WaitReason::PowerSaver),
        ('6', WaitReason::NetworkUnavailable),
        ('7', WaitReason::KeychainLocked),
        ('8', WaitReason::UserDecisionRequired),
        ('9', WaitReason::ConsentRequired),
        ('0', WaitReason::SystemSuspend),
    ] {
        let job = enqueue(&f.store, seed).await;
        let parked = f
            .store
            .park(job, reason, TransitionCause::system("parked"))
            .await
            .expect("park");
        assert_eq!(parked.state, reason.wait_state());
        assert_eq!(parked.wait_reason, Some(reason));
        assert!(parked.state.is_waiting(), "{reason:?}");
        assert_eq!(parked.finished_at, None);
    }
}

#[tokio::test]
async fn completing_with_an_empty_output_set_is_refused_by_both_entry_points() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'e').await;
    advance(
        &f.store,
        job,
        &[
            JobState::Preflight,
            JobState::RunningLocal,
            JobState::Verifying,
            JobState::Committing,
        ],
    )
    .await;
    let events_before = f.store.events().count().await.expect("count");

    let error = f
        .store
        .complete(job, Vec::new(), TransitionCause::system("commit"))
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::MissingCommittedOutput)
    ));

    let error = f
        .store
        .complete_with_events(
            job,
            Vec::new(),
            TransitionCause::system("commit"),
            Vec::new(),
        )
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::MissingCommittedOutput)
    ));

    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::Committing
    );
    assert_eq!(
        f.store.events().count().await.expect("count"),
        events_before,
        "a refused completion writes no event"
    );
}

#[tokio::test]
async fn completing_commits_outputs_and_any_extra_events_in_one_transaction() {
    let f = fixture().await;
    let recording = RecordingId::new();
    let job = f
        .store
        .enqueue(import_request('x').with_recording(recording))
        .await
        .expect("enqueue")
        .job_id();
    advance(
        &f.store,
        job,
        &[
            JobState::Preflight,
            JobState::RunningLocal,
            JobState::Verifying,
            JobState::Committing,
        ],
    )
    .await;

    let record = f
        .store
        .complete_with_events(
            job,
            vec![JobOutput::new("media_asset", "media/sha256/aa/aa.wav")
                .verified("a".repeat(64), 11)],
            TransitionCause::system("committed"),
            vec![PendingEvent::for_recording(
                recording,
                EventKind::RecordingImported,
                EventPayload::RecordingImported {
                    media_sha256: "a".repeat(64),
                    byte_length: 11,
                },
            )],
        )
        .await
        .expect("complete");

    assert_eq!(record.state, JobState::Completed);
    assert!(record.finished_at.is_some());
    assert!(record.started_at.is_some());
    let outputs = f.store.outputs(job).await.expect("outputs");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].byte_length, Some(11));
    assert_eq!(outputs[0].sha256, Some("a".repeat(64)));
    let kinds: Vec<EventKind> = f
        .store
        .events()
        .read_after(0, 100)
        .await
        .expect("events")
        .iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&EventKind::RecordingImported));
}

#[tokio::test]
async fn an_unverified_output_commits_without_a_digest_or_a_length() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'u').await;
    advance(
        &f.store,
        job,
        &[
            JobState::Preflight,
            JobState::RunningLocal,
            JobState::Verifying,
            JobState::Committing,
        ],
    )
    .await;
    f.store
        .complete(
            job,
            vec![JobOutput::new("transcript_revision", "rev-1")],
            TransitionCause::system("committed"),
        )
        .await
        .expect("complete");

    let outputs = f.store.outputs(job).await.expect("outputs");
    assert_eq!(outputs[0].sha256, None);
    assert_eq!(outputs[0].byte_length, None);
}

#[tokio::test]
async fn a_dispatched_job_is_possibly_billed_only_once_the_outcome_is_unknown() {
    let f = fixture().await;
    f.store
        .ensure_consent_grant("consent-1", "cloud_inference", "v1", "openrouter", "acme/m")
        .await
        .expect("consent");
    let job = f
        .store
        .enqueue(import_request('b').with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    advance(&f.store, job, &[JobState::Preflight, JobState::CloudReady]).await;
    f.store
        .transition(
            job,
            JobState::CloudSending,
            TransitionCause::system("dispatch").with_consent_verified(),
        )
        .await
        .expect("dispatch");

    let sending = f.store.get(job).await.expect("load");
    assert!(sending.dispatched, "the dispatch flag is durable");
    assert!(
        !sending.possibly_billed(),
        "an in-flight request is not yet reported as billed"
    );

    let unknown = f
        .store
        .transition(
            job,
            JobState::CloudOutcomeUnknown,
            TransitionCause::system("no_response"),
        )
        .await
        .expect("no response");
    assert!(unknown.possibly_billed());
}

#[tokio::test]
async fn marking_a_cancel_request_is_visible_on_the_record() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'c').await;
    assert_eq!(
        f.store.get(job).await.expect("load").cancel_requested_at,
        None
    );

    f.store.mark_cancel_requested(job).await.expect("mark");

    assert!(f
        .store
        .get(job)
        .await
        .expect("load")
        .cancel_requested_at
        .is_some());
}

// ---------------------------------------------------------------------------
// Reads and integrity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn listing_no_states_returns_nothing_and_several_states_are_merged() {
    let f = fixture().await;
    let queued = enqueue(&f.store, 'q').await;
    let parked = enqueue(&f.store, 'p').await;
    f.store
        .park(
            parked,
            WaitReason::NetworkUnavailable,
            TransitionCause::system("offline"),
        )
        .await
        .expect("park");

    let none = f.store.jobs_in_states(&[]).await.expect("none");
    assert!(none.is_empty(), "an empty state filter matches nothing");
    let mut found: Vec<JobId> = f
        .store
        .jobs_in_states(&[JobState::WaitingForNetwork, JobState::Queued])
        .await
        .expect("list")
        .iter()
        .map(|job| job.id)
        .collect();
    found.sort_unstable();
    let mut expected = vec![queued, parked];
    expected.sort_unstable();
    assert_eq!(found, expected);
    let completed = f
        .store
        .jobs_in_states(&[JobState::Completed])
        .await
        .expect("list");
    assert!(completed.is_empty(), "nothing has completed yet");
}

#[tokio::test]
async fn an_unknown_persisted_state_is_reported_as_a_corrupt_record() {
    let f = fixture().await;
    let job = enqueue(&f.store, 's').await;
    raw_write(
        &f.store,
        "UPDATE jobs SET state = 'teleporting' WHERE id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.get(job).await.expect_err("must refuse");
    let JobError::CorruptRecord(detail) = &error else {
        panic!("expected a corrupt-record failure, got {error:?}");
    };
    assert!(detail.contains("teleporting"), "{detail}");
}

#[tokio::test]
async fn an_unknown_persisted_resource_class_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'k').await;
    raw_write(
        &f.store,
        "UPDATE job_engine_state SET resource_class = 'quantum' WHERE job_id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.get(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("quantum")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unknown_persisted_wait_reason_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'w').await;
    raw_write(
        &f.store,
        "UPDATE job_engine_state SET wait_reason = 'bored' WHERE job_id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.get(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("bored")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_job_without_a_key_digest_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'm').await;
    raw_write(
        &f.store,
        "UPDATE jobs SET job_key = NULL WHERE id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.get(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("no key digest")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_job_key_that_is_not_canonical_hex_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'n').await;
    raw_write(
        &f.store,
        "UPDATE jobs SET job_key = 'NOTAHEXDIGEST' WHERE id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.get(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("NOTAHEXDIGEST")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unparsable_recording_reference_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'v').await;
    raw_write(
        &f.store,
        "UPDATE job_engine_state SET recording_id = 'not-a-uuid' WHERE job_id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.get(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("not-a-uuid")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unparsable_job_identifier_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'i').await;
    corrupt_write(
        &f.store,
        "UPDATE jobs SET id = 'not-a-job-id' WHERE id = ?1",
        vec![job.to_string()],
    )
    .await;
    corrupt_write(
        &f.store,
        "UPDATE job_engine_state SET job_id = 'not-a-job-id' WHERE job_id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f
        .store
        .jobs_in_states(&[JobState::Queued])
        .await
        .expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("not-a-job-id")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unparsable_config_snapshot_is_reported_as_an_encoding_failure() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'j').await;
    raw_write(
        &f.store,
        "UPDATE jobs SET config_snapshot_json = '{not json' WHERE id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.get(job).await.expect_err("must refuse");
    assert!(matches!(error, JobError::Encoding { .. }), "{error:?}");
    assert_eq!(error.code(), AppErrorCode::InternalError);
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn an_unknown_source_state_in_the_audit_trail_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'f').await;
    advance(&f.store, job, &[JobState::Preflight]).await;
    raw_write(
        &f.store,
        "UPDATE job_transitions SET from_state = 'nowhere' WHERE job_id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.transitions(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("nowhere")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unknown_target_state_in_the_audit_trail_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'g').await;
    advance(&f.store, job, &[JobState::Preflight]).await;
    raw_write(
        &f.store,
        "UPDATE job_transitions SET to_state = 'elsewhere' WHERE job_id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = f.store.transitions(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("elsewhere")),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Checkpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn re_preparing_an_uncommitted_stage_bumps_its_attempt_counter() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'h').await;

    let first = f
        .store
        .prepare_checkpoint(job, "decode", 0, "in-1")
        .await
        .expect("prepare");
    assert_eq!(first.attempt, 0);
    assert!(!first.is_committed());

    let second = f
        .store
        .prepare_checkpoint(job, "decode", 0, "in-2")
        .await
        .expect("re-prepare");
    assert_eq!(second.attempt, 1);
    assert_eq!(second.input_digest, "in-2");
    assert_eq!(f.store.checkpoints(job).await.expect("list").len(), 1);
}

#[tokio::test]
async fn a_committed_stage_refuses_both_a_re_prepare_and_a_second_commit() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'y').await;
    f.store
        .prepare_checkpoint(job, "decode", 0, "in")
        .await
        .expect("prepare");
    let committed = f
        .store
        .commit_checkpoint(job, "decode", "pcm/0.raw", Some("d".repeat(64)))
        .await
        .expect("commit");
    assert!(committed.is_committed());

    let error = f
        .store
        .prepare_checkpoint(job, "decode", 0, "in-2")
        .await
        .expect_err("must refuse");
    assert!(matches!(
        &error,
        JobError::CheckpointAlreadyCommitted { stage } if stage == "decode"
    ));
    assert_eq!(error.code(), AppErrorCode::Conflict);
    assert!(!error.is_retryable());

    let error = f
        .store
        .commit_checkpoint(job, "decode", "pcm/1.raw", None)
        .await
        .expect_err("must refuse");
    assert!(matches!(error, JobError::CheckpointAlreadyCommitted { .. }));

    let stored = f.store.checkpoints(job).await.expect("list");
    assert_eq!(stored[0].output_ref, Some("pcm/0.raw".to_string()));
    assert_eq!(stored[0].attempt, 0);
}

#[tokio::test]
async fn committing_a_stage_that_was_never_prepared_is_refused() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'z').await;

    let error = f
        .store
        .commit_checkpoint(job, "decode", "pcm/0.raw", None)
        .await
        .expect_err("must refuse");
    assert!(matches!(
        &error,
        JobError::CheckpointNotPrepared { stage } if stage == "decode"
    ));
    assert_eq!(error.code(), AppErrorCode::Conflict);
    let checkpoints = f.store.checkpoints(job).await.expect("list");
    assert!(
        checkpoints.is_empty(),
        "a refused commit must not create a checkpoint row"
    );
}

#[tokio::test]
async fn a_resume_plan_reruns_everything_without_a_commit_mark() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'A').await;
    f.store
        .prepare_checkpoint(job, "decode", 0, "in")
        .await
        .expect("prepare decode");
    f.store
        .commit_checkpoint(job, "decode", "pcm/0.raw", Some("e".repeat(64)))
        .await
        .expect("commit decode");
    f.store
        .prepare_checkpoint(job, "transcribe", 1, "in")
        .await
        .expect("prepare transcribe");

    let plan = f
        .store
        .resume_plan(job, &["decode", "transcribe", "summarize"])
        .await
        .expect("plan");

    assert_eq!(
        plan,
        vec![
            StageAction::Skip {
                stage: "decode".to_string(),
                output_ref: "pcm/0.raw".to_string(),
                output_digest: Some("e".repeat(64)),
            },
            StageAction::Run {
                stage: "transcribe".to_string(),
                attempt: 1,
            },
            StageAction::Run {
                stage: "summarize".to_string(),
                attempt: 0,
            },
        ]
    );
    assert_eq!(
        plan.iter().map(StageAction::stage).collect::<Vec<_>>(),
        vec!["decode", "transcribe", "summarize"]
    );
}

#[tokio::test]
async fn a_commit_mark_without_an_output_reference_still_skips_the_stage() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'B').await;
    f.store
        .prepare_checkpoint(job, "decode", 0, "in")
        .await
        .expect("prepare");
    f.store
        .commit_checkpoint(job, "decode", "pcm/0.raw", None)
        .await
        .expect("commit");
    raw_write(
        &f.store,
        "UPDATE job_checkpoints SET output_ref = NULL WHERE job_id = ?1 AND stage = 'decode'",
        vec![job.to_string()],
    )
    .await;

    let plan = f.store.resume_plan(job, &["decode"]).await.expect("plan");
    assert_eq!(
        plan,
        vec![StageAction::Skip {
            stage: "decode".to_string(),
            output_ref: String::new(),
            output_digest: None,
        }]
    );
}

// ---------------------------------------------------------------------------
// Cloud request ledger
// ---------------------------------------------------------------------------

#[test]
fn the_dispatch_state_vocabulary_round_trips_and_classifies_billing() {
    for state in DispatchState::ALL {
        assert_eq!(DispatchState::parse(state.as_str()), Some(*state));
    }
    assert_eq!(DispatchState::parse("teleported"), None);

    for state in [
        DispatchState::Sending,
        DispatchState::AcceptedByProvider,
        DispatchState::OutcomeUnknown,
        DispatchState::Completed,
    ] {
        assert!(state.may_have_been_billed(), "{state:?}");
    }
    for state in [DispatchState::NotSent, DispatchState::FailedBeforeSend] {
        assert!(!state.may_have_been_billed(), "{state:?}");
    }
}

#[tokio::test]
async fn only_unanswered_dispatches_are_reported_as_dangling() {
    let f = fixture().await;
    f.store
        .ensure_consent_grant("consent-1", "cloud_inference", "v1", "openrouter", "acme/m")
        .await
        .expect("consent");
    let job = f
        .store
        .enqueue(import_request('D').with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();

    let answered = f
        .store
        .record_attempt(
            job,
            "f".repeat(64),
            "multipart_stt",
            Some("acme".to_string()),
            "whisper-1",
            Some("eu".to_string()),
            Some("zdr@1".to_string()),
            Some("consent-1".to_string()),
        )
        .await
        .expect("record");
    let unanswered = f
        .store
        .record_attempt(
            job,
            "f".repeat(64),
            "multipart_stt",
            None,
            "whisper-1",
            None,
            None,
            Some("consent-1".to_string()),
        )
        .await
        .expect("record");

    assert!(
        f.store
            .dangling_attempts()
            .await
            .expect("dangling")
            .is_empty(),
        "a recorded-but-unsent attempt is not dangling"
    );

    f.store
        .update_attempt(&answered, DispatchState::Sending, Some("req-1".to_string()))
        .await
        .expect("sending");
    f.store
        .update_attempt(&answered, DispatchState::Completed, None)
        .await
        .expect("completed");
    f.store
        .update_attempt(&unanswered, DispatchState::Sending, None)
        .await
        .expect("sending");

    let dangling = f.store.dangling_attempts().await.expect("dangling");
    assert_eq!(dangling.len(), 1);
    assert_eq!(dangling[0].id, unanswered);
    assert_eq!(dangling[0].status, DispatchState::Sending);

    let attempts = f.store.attempts(job).await.expect("attempts");
    assert_eq!(attempts.len(), 2);
    let answered_row = attempts
        .iter()
        .find(|a| a.id == answered)
        .expect("answered attempt");
    assert_eq!(answered_row.job_id, job);
    assert_eq!(answered_row.provider, Some("acme".to_string()));
    assert_eq!(answered_row.route, Some("eu".to_string()));
    assert_eq!(
        answered_row.privacy_policy_snapshot,
        Some("zdr@1".to_string())
    );
    assert_eq!(answered_row.consent_id, Some("consent-1".to_string()));
    assert_eq!(answered_row.content_sha256, "f".repeat(64));
    assert_eq!(answered_row.model, "whisper-1");
    assert_eq!(
        answered_row.provider_request_id,
        Some("req-1".to_string()),
        "a later update must not clear the vendor request id"
    );
    assert!(answered_row.dispatched_at.is_some());
    assert!(answered_row.responded_at.is_some());

    let unanswered_row = attempts
        .iter()
        .find(|a| a.id == unanswered)
        .expect("unanswered attempt");
    assert_eq!(
        unanswered_row.provider,
        Some("unknown".to_string()),
        "an unnamed provider is recorded as `unknown`, never omitted"
    );
    assert_eq!(unanswered_row.route, None);
    assert_eq!(unanswered_row.responded_at, None);
}

#[tokio::test]
async fn an_unknown_persisted_dispatch_state_is_reported_as_corrupt() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'E').await;
    let attempt = f
        .store
        .record_attempt(
            job,
            "f".repeat(64),
            "multipart_stt",
            None,
            "m",
            None,
            None,
            None,
        )
        .await
        .expect("record");
    corrupt_write(
        &f.store,
        "UPDATE inference_attempts SET dispatch_state = 'teleported' WHERE id = ?1",
        vec![attempt],
    )
    .await;

    let error = f.store.attempts(job).await.expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("teleported")),
        "{error:?}"
    );
    assert!(
        f.store
            .dangling_attempts()
            .await
            .expect("dangling")
            .is_empty(),
        "an unrecognised status is not one of the dangling dispatch states"
    );
}

// ---------------------------------------------------------------------------
// Consent rows and tombstones
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consent_rows_are_idempotent_and_revocation_is_durable() {
    let f = fixture().await;
    f.store
        .ensure_consent_grant("consent-1", "cloud_inference", "v1", "openrouter", "acme/m")
        .await
        .expect("grant");
    f.store
        .ensure_consent_grant("consent-1", "cloud_inference", "v1", "openrouter", "acme/m")
        .await
        .expect("grant again");
    f.store
        .revoke_consent_grant("consent-1")
        .await
        .expect("revoke");
    // Revoking something that was never granted is not an error.
    f.store
        .revoke_consent_grant("consent-absent")
        .await
        .expect("revoke unknown");

    let revoked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM consent_grants WHERE id = 'consent-1' AND revoked_at IS NOT NULL",
    )
    .fetch_one(f.store.read_pool())
    .await
    .expect("count");
    assert_eq!(revoked, 1);
}

#[tokio::test]
async fn a_tombstone_is_observable_and_rewriting_it_is_idempotent() {
    let f = fixture().await;
    let recording = RecordingId::new();
    assert!(!f.store.is_tombstoned(recording).await.expect("check"));

    f.store
        .tombstone(recording, "user_deleted")
        .await
        .expect("tombstone");
    f.store
        .tombstone(recording, "user_deleted_again")
        .await
        .expect("tombstone");

    assert!(f.store.is_tombstoned(recording).await.expect("check"));
    assert!(!f
        .store
        .is_tombstoned(RecordingId::new())
        .await
        .expect("check"));
}
