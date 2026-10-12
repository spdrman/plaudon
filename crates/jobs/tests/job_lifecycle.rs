//! Durable job-engine integration tests (EPIC C, SPEC v1.3 §14).
//!
//! Every test drives a real SQLite database through the single-writer
//! coordinator. Nothing is mocked: the assertions are about committed rows.

use std::sync::Arc;

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_jobs::cancel::{InterruptionCause, CLOUD_CANCELLATION_NOTICE};
use plaudon_jobs::engine::{EngineConfig, JobEngine};
use plaudon_jobs::error::{DispatchRefusal, JobError};
use plaudon_jobs::events::{
    EventKind, EventPayload, EventRecord, EventSubscriber, SubscriberError,
};
use plaudon_jobs::history::{JobHistory, JobHistoryQuery};
use plaudon_jobs::key::{
    ImportJobKey, JobKey, ProcessingJobKey, ProcessingStage, ReprocessToken, SourceKind,
};
use plaudon_jobs::scheduler::{
    PowerSource, PreflightDecision, ResourceClass, ResourceRequest, SystemResources, WaitReason,
};
use plaudon_jobs::state::{JobState, JobTransitionError, TransitionCause};
use plaudon_jobs::store::{DispatchState, EnqueueRequest, JobOutput, JobStore, StageAction};
use plaudon_jobs::ui::{RecordingSink, UiBridge};
use plaudon_jobs::writer::Database;

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: std::path::PathBuf,
    staging: std::path::PathBuf,
    archive: std::path::PathBuf,
    store: JobStore,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("app.sqlite");
    let archive = dir.path().to_path_buf();
    let staging = dir.path().join("temp/imports");
    std::fs::create_dir_all(&staging).expect("staging dir");
    let store = JobStore::open_at(&db_path).await.expect("open store");
    Fixture {
        _dir: dir,
        db_path,
        staging,
        archive,
        store,
    }
}

fn import_request() -> EnqueueRequest {
    EnqueueRequest::new(
        "import",
        ImportJobKey::by_content(SourceKind::LocalFile, "a".repeat(64)),
        ResourceClass::LocalDisk,
    )
}

async fn enqueue_running(store: &JobStore) -> JobId {
    let job = store
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();
    store
        .transition(
            job,
            JobState::Preflight,
            TransitionCause::system("preflight"),
        )
        .await
        .expect("preflight");
    store
        .transition(
            job,
            JobState::RunningLocal,
            TransitionCause::system("start"),
        )
        .await
        .expect("run");
    job
}

// ---------------------------------------------------------------------------
// C1.1 — transactional state machine
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transitions_are_durable_across_a_reopen() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    f.store.database().close().await;

    // Re-open the same file: the committed state is what we read back.
    let reopened = JobStore::open(Database::open(&f.db_path, 2).await.expect("reopen"))
        .await
        .expect("store");
    let record = reopened.get(job).await.expect("load");
    assert_eq!(record.state, JobState::RunningLocal);
    assert_eq!(
        reopened.transitions(job).await.expect("audit"),
        vec![
            (JobState::Queued, JobState::Preflight),
            (JobState::Preflight, JobState::RunningLocal),
        ]
    );
}

#[tokio::test]
async fn an_illegal_transition_leaves_stored_state_untouched() {
    let f = fixture().await;
    let job = f
        .store
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();

    let error = f
        .store
        .transition(job, JobState::Completed, TransitionCause::system("cheat"))
        .await
        .expect_err("must reject");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::IllegalEdge { .. })
    ));

    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::Queued
    );
    assert_eq!(f.store.transitions(job).await.expect("audit").len(), 0);
}

#[tokio::test]
async fn completed_requires_outputs_committed_in_the_same_transaction() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    for state in [JobState::Verifying, JobState::Committing] {
        f.store
            .transition(job, state, TransitionCause::system("advance"))
            .await
            .expect("advance");
    }

    // No outputs yet: the guard refuses and the state stays at Committing.
    let error = f
        .store
        .transition(job, JobState::Completed, TransitionCause::system("commit"))
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

    // Completing with an output succeeds and the output is durable.
    let output = JobOutput::new("media_asset", "sha256/aa/aa.wav").verified("b".repeat(64), 10);
    f.store
        .complete(
            job,
            vec![output.clone()],
            TransitionCause::system("committed"),
        )
        .await
        .expect("complete");
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::Completed
    );
    assert_eq!(f.store.outputs(job).await.expect("outputs"), vec![output]);
}

#[tokio::test]
async fn completing_with_no_outputs_is_refused_before_any_write() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    let error = f
        .store
        .complete(job, Vec::new(), TransitionCause::system("x"))
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::MissingCommittedOutput)
    ));
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::RunningLocal
    );
}

#[tokio::test]
async fn cloud_outcome_unknown_never_re_enters_dispatch_automatically() {
    let f = fixture().await;
    let job = f
        .store
        .enqueue(import_request().with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    for (state, cause) in [
        (JobState::Preflight, TransitionCause::system("preflight")),
        (JobState::CloudReady, TransitionCause::system("ready")),
        (
            JobState::CloudSending,
            TransitionCause::system("dispatch").with_consent_verified(),
        ),
        (
            JobState::CloudOutcomeUnknown,
            TransitionCause::system("no_response"),
        ),
    ] {
        f.store.transition(job, state, cause).await.expect("step");
    }

    // The scheduler and the reconciler are both refused.
    for cause in [
        TransitionCause::system("retry"),
        TransitionCause::recovery("startup"),
    ] {
        let error = f
            .store
            .transition(job, JobState::CloudReady, cause)
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            JobError::Transition(JobTransitionError::RequiresUserAction { .. })
        ));
    }
    // A user retry without cost acknowledgement is refused too.
    let error = f
        .store
        .transition(job, JobState::CloudReady, TransitionCause::user("retry"))
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::RequiresCostAcknowledgement)
    ));
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::CloudOutcomeUnknown
    );

    // Only an explicit, cost-acknowledged user action moves it.
    f.store
        .transition(
            job,
            JobState::CloudReady,
            TransitionCause::user("retry").with_cost_acknowledged(),
        )
        .await
        .expect("user retry");
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::CloudReady
    );
}

#[tokio::test]
async fn dispatch_without_verified_consent_is_refused() {
    let f = fixture().await;
    let job = f
        .store
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();
    f.store
        .transition(job, JobState::Preflight, TransitionCause::system("p"))
        .await
        .expect("preflight");
    f.store
        .transition(job, JobState::CloudReady, TransitionCause::system("r"))
        .await
        .expect("ready");
    let error = f
        .store
        .transition(
            job,
            JobState::CloudSending,
            TransitionCause::system("dispatch"),
        )
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::RequiresVerifiedConsent)
    ));
    assert_eq!(
        f.store.get(job).await.expect("load").state,
        JobState::CloudReady
    );
}

#[tokio::test]
async fn config_snapshot_is_immutable_and_persisted_at_creation() {
    let f = fixture().await;
    let snapshot = serde_json::json!({
        "engine": "whisper_cpp",
        "model": "large-v3",
        "privacy_policy": "local_only",
    });
    let job = f
        .store
        .enqueue(import_request().with_config_snapshot(snapshot.clone()))
        .await
        .expect("enqueue")
        .job_id();
    for state in [JobState::Preflight, JobState::RunningLocal] {
        f.store
            .transition(job, state, TransitionCause::system("advance"))
            .await
            .expect("advance");
    }
    assert_eq!(
        f.store.get(job).await.expect("load").config_snapshot,
        snapshot
    );
}

// ---------------------------------------------------------------------------
// C1.2 — idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enqueuing_the_same_import_twice_yields_one_job() {
    let f = fixture().await;
    let first = f.store.enqueue(import_request()).await.expect("first");
    let second = f.store.enqueue(import_request()).await.expect("second");
    assert!(first.is_created());
    assert!(!second.is_created());
    assert_eq!(first.job_id(), second.job_id());
    assert_eq!(count_jobs(&f.store).await, 1);
}

#[tokio::test]
async fn concurrent_identical_enqueues_create_exactly_one_row() {
    let f = fixture().await;
    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = f.store.clone();
        handles.push(tokio::spawn(async move {
            store.enqueue(import_request()).await
        }));
    }
    let mut ids = std::collections::HashSet::new();
    let mut created = 0;
    for handle in handles {
        let outcome = handle.await.expect("task").expect("enqueue");
        if outcome.is_created() {
            created += 1;
        }
        ids.insert(outcome.job_id());
    }
    assert_eq!(created, 1, "exactly one enqueue may create the row");
    assert_eq!(ids.len(), 1);
    assert_eq!(count_jobs(&f.store).await, 1);
}

#[tokio::test]
async fn an_explicit_reprocess_creates_a_distinct_job() {
    let f = fixture().await;
    let original = f.store.enqueue(import_request()).await.expect("enqueue");
    let key = ImportJobKey::by_content(SourceKind::LocalFile, "a".repeat(64))
        .with_reprocess(ReprocessToken::new());
    let rerun = f
        .store
        .enqueue(
            EnqueueRequest::new("import", key, ResourceClass::LocalDisk)
                .reprocessing(original.job_id()),
        )
        .await
        .expect("reprocess");

    assert!(rerun.is_created());
    assert_ne!(original.job_id(), rerun.job_id());
    assert_eq!(count_jobs(&f.store).await, 2);

    let record = f.store.get(rerun.job_id()).await.expect("load");
    assert!(record.is_reprocess, "reprocessing is recorded distinctly");
    assert_eq!(record.reprocess_of, Some(original.job_id()));

    let first = f.store.get(original.job_id()).await.expect("load");
    assert!(!first.is_reprocess);
}

#[tokio::test]
async fn every_keyed_processing_dimension_produces_a_separate_job() {
    let f = fixture().await;
    let base = ProcessingJobKey::new(
        "c".repeat(64),
        ProcessingStage::Transcribe,
        "whisper_cpp",
        "large-v3",
    );
    let variants: Vec<ProcessingJobKey> = vec![
        base.clone(),
        {
            let mut k = base.clone();
            k.engine = "openrouter".into();
            k
        },
        {
            let mut k = base.clone();
            k.model_version = Some("1.5.5".into());
            k
        },
        {
            let mut k = base.clone();
            k.transcript_revision = Some("rev-2".into());
            k
        },
        {
            let mut k = base.clone();
            k.prompt_template_version = Some("meeting@4".into());
            k
        },
        {
            let mut k = base.clone();
            k.privacy_policy_snapshot = Some("zdr@1".into());
            k
        },
        {
            let mut k = base.clone();
            k.normalization_config
                .insert("resample_hz".into(), "48000".into());
            k
        },
    ];
    let mut ids = std::collections::HashSet::new();
    for key in &variants {
        let outcome = f
            .store
            .enqueue(EnqueueRequest::new(
                "transcribe",
                key.clone(),
                ResourceClass::Cpu,
            ))
            .await
            .expect("enqueue");
        assert!(outcome.is_created(), "each variant is a distinct job");
        ids.insert(outcome.job_id());
    }
    assert_eq!(ids.len(), variants.len());
    assert_eq!(
        count_jobs(&f.store).await,
        i64::try_from(variants.len()).expect("fits")
    );

    // Re-enqueuing every variant deduplicates onto the existing jobs.
    for key in &variants {
        let outcome = f
            .store
            .enqueue(EnqueueRequest::new(
                "transcribe",
                key.clone(),
                ResourceClass::Cpu,
            ))
            .await
            .expect("enqueue");
        assert!(!outcome.is_created());
    }
    assert_eq!(
        count_jobs(&f.store).await,
        i64::try_from(variants.len()).expect("fits")
    );
}

#[tokio::test]
async fn the_key_digest_is_stable_across_store_instances() {
    let f = fixture().await;
    let outcome = f.store.enqueue(import_request()).await.expect("enqueue");
    let digest = f
        .store
        .get(outcome.job_id())
        .await
        .expect("load")
        .key_digest;
    f.store.database().close().await;

    let reopened = JobStore::open(Database::open(&f.db_path, 2).await.expect("reopen"))
        .await
        .expect("store");
    // A fresh process computes the same digest and therefore deduplicates.
    let second = reopened.enqueue(import_request()).await.expect("enqueue");
    assert!(!second.is_created());
    assert_eq!(
        reopened
            .get(second.job_id())
            .await
            .expect("load")
            .key_digest,
        digest
    );
    assert_eq!(
        digest.as_str(),
        JobKey::Import(ImportJobKey::by_content(
            SourceKind::LocalFile,
            "a".repeat(64)
        ))
        .digest()
        .as_str()
    );
}

// ---------------------------------------------------------------------------
// C1.3 — checkpoints and the durable import journal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resuming_reruns_only_stages_without_a_commit_mark() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    let stages = ["decode", "transcribe", "summarize"];

    f.store
        .prepare_checkpoint(job, "decode", 0, "in-0")
        .await
        .expect("prepare");
    f.store
        .commit_checkpoint(job, "decode", "pcm/0.raw", Some("d".repeat(64)))
        .await
        .expect("commit");
    // `transcribe` was prepared but never committed.
    f.store
        .prepare_checkpoint(job, "transcribe", 1, "in-1")
        .await
        .expect("prepare");

    let plan = f.store.resume_plan(job, &stages).await.expect("plan");
    assert_eq!(
        plan,
        vec![
            StageAction::Skip {
                stage: "decode".into(),
                output_ref: "pcm/0.raw".into(),
                output_digest: Some("d".repeat(64)),
            },
            StageAction::Run {
                stage: "transcribe".into(),
                attempt: 1,
            },
            StageAction::Run {
                stage: "summarize".into(),
                attempt: 0,
            },
        ]
    );
}

#[tokio::test]
async fn a_committed_checkpoint_is_immutable() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    f.store
        .prepare_checkpoint(job, "decode", 0, "in-0")
        .await
        .expect("prepare");
    f.store
        .commit_checkpoint(job, "decode", "pcm/0.raw", None)
        .await
        .expect("commit");

    assert!(matches!(
        f.store
            .commit_checkpoint(job, "decode", "other", None)
            .await
            .expect_err("must refuse"),
        JobError::CheckpointAlreadyCommitted { .. }
    ));
    assert!(matches!(
        f.store
            .prepare_checkpoint(job, "decode", 0, "in-0")
            .await
            .expect_err("must refuse"),
        JobError::CheckpointAlreadyCommitted { .. }
    ));
    let checkpoint = &f.store.checkpoints(job).await.expect("read")[0];
    assert_eq!(checkpoint.output_ref.as_deref(), Some("pcm/0.raw"));
}

#[tokio::test]
async fn committing_an_unprepared_stage_is_refused() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    assert!(matches!(
        f.store
            .commit_checkpoint(job, "decode", "out", None)
            .await
            .expect_err("must refuse"),
        JobError::CheckpointNotPrepared { .. }
    ));
}

#[tokio::test]
async fn a_dispatch_without_a_response_preserves_the_vendor_request_id() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    f.store
        .ensure_consent_grant("consent-1", "cloud_inference", "v1", "openrouter", "acme")
        .await
        .expect("grant");
    let attempt = f
        .store
        .record_attempt(
            job,
            "f".repeat(64),
            "multipart_stt",
            Some("acme".into()),
            "whisper-1",
            Some("acme/eu".into()),
            Some("zdr@2026-10-01".into()),
            Some("consent-1".into()),
        )
        .await
        .expect("record");
    f.store
        .update_attempt(&attempt, DispatchState::Sending, Some("req-123".into()))
        .await
        .expect("dispatch");
    f.store
        .update_attempt(&attempt, DispatchState::OutcomeUnknown, None)
        .await
        .expect("unknown");

    let dangling = f.store.dangling_attempts().await.expect("dangling");
    assert_eq!(dangling.len(), 1);
    assert_eq!(dangling[0].provider_request_id.as_deref(), Some("req-123"));
    assert_eq!(dangling[0].status, DispatchState::OutcomeUnknown);
    assert_eq!(dangling[0].content_sha256, "f".repeat(64));
    assert!(dangling[0].status.may_have_been_billed());
}

// ---------------------------------------------------------------------------
// C1.4 — single writer and bounded scheduler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_job_load_never_hits_sqlite_busy() {
    let f = fixture().await;
    let mut handles = Vec::new();
    for index in 0..48 {
        let store = f.store.clone();
        handles.push(tokio::spawn(async move {
            let key = ImportJobKey::by_content(SourceKind::LocalFile, format!("{index:064}"));
            let job = store
                .enqueue(EnqueueRequest::new("import", key, ResourceClass::LocalDisk))
                .await?
                .job_id();
            store
                .transition(job, JobState::Preflight, TransitionCause::system("p"))
                .await?;
            store
                .transition(job, JobState::RunningLocal, TransitionCause::system("r"))
                .await?;
            store
                .transition(job, JobState::Verifying, TransitionCause::system("v"))
                .await?;
            store
                .transition(job, JobState::Committing, TransitionCause::system("c"))
                .await?;
            store
                .complete(
                    job,
                    vec![JobOutput::new("media_asset", format!("sha256/{index}.wav"))],
                    TransitionCause::system("done"),
                )
                .await
        }));
    }
    for handle in handles {
        handle.await.expect("task").expect("no SQLITE_BUSY");
    }
    assert_eq!(count_jobs(&f.store).await, 48);
    let page = JobHistory::new(f.store.read_pool().clone())
        .list_jobs(&JobHistoryQuery::all().with_limit(500))
        .await
        .expect("list");
    assert!(page.jobs.iter().all(|j| j.state == JobState::Completed));
}

#[tokio::test]
async fn preflight_parks_rather_than_failing_when_resources_are_short() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = engine
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();

    let decision = engine
        .preflight(
            job,
            ResourceRequest {
                required_disk_bytes: 10_000_000_000,
                estimated_ram_bytes: 1024,
                ..Default::default()
            },
            SystemResources {
                available_disk_bytes: 1024,
                available_ram_bytes: 8 * 1024 * 1024 * 1024,
                power_source: PowerSource::Ac,
                power_saver_enabled: false,
                model_asset_present: true,
                source_available: true,
            },
        )
        .await
        .expect("preflight");

    assert!(matches!(decision, PreflightDecision::Park { .. }));
    let record = engine.store().get(job).await.expect("load");
    assert_eq!(record.state, JobState::WaitingForUser);
    assert_eq!(record.wait_reason, Some(WaitReason::InsufficientDisk));
    assert!(!record.state.is_terminal(), "parking is not a failure");
}

#[tokio::test]
async fn a_missing_model_asset_pauses_the_job_visibly() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = engine
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();
    engine
        .preflight(
            job,
            ResourceRequest {
                needs_model_asset: true,
                ..Default::default()
            },
            SystemResources {
                available_disk_bytes: u64::MAX,
                available_ram_bytes: u64::MAX,
                power_source: PowerSource::Ac,
                power_saver_enabled: false,
                model_asset_present: false,
                source_available: true,
            },
        )
        .await
        .expect("preflight");
    let record = engine.store().get(job).await.expect("load");
    assert_eq!(record.state, JobState::WaitingForModel);
    assert_eq!(record.wait_reason, Some(WaitReason::ModelAssetMissing));
}

#[tokio::test]
async fn the_scheduler_enforces_per_class_maxima_under_load() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let mut handles = Vec::new();
    for _ in 0..24 {
        let engine = engine.clone();
        handles.push(tokio::spawn(async move {
            let admission = engine
                .admit(JobId::new(), ResourceClass::Cpu)
                .await
                .expect("admit");
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            drop(admission);
        }));
    }
    for handle in handles {
        handle.await.expect("task");
    }
    assert_eq!(engine.scheduler().high_water_mark(ResourceClass::Cpu), 1);
}

// ---------------------------------------------------------------------------
// C1.5 — cancellation, wait states, consent revocation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_acknowledges_immediately_and_settles_durably() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = engine
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();
    let token = engine.token(job);

    let derivative = f.staging.join(format!("{job}.part"));
    std::fs::write(&derivative, b"partial").expect("write derivative");

    let ack = engine.cancel(job).await.expect("cancel");
    assert!(ack.accepted);
    assert_eq!(ack.state, JobState::Cancelling);
    assert!(token.is_cancelled(), "the worker's token is signalled");

    let outcome = engine
        .finish_cancellation(job, vec![derivative.clone()])
        .await
        .expect("finish");
    assert_eq!(outcome.final_state, JobState::Cancelled);
    assert!(!outcome.possibly_billed);
    assert!(!outcome.refunded);
    assert_eq!(outcome.discarded_derivatives, vec![derivative.clone()]);
    assert!(!derivative.exists(), "incomplete derivative is discarded");
    assert_eq!(
        engine.store().get(job).await.expect("load").state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn a_cancelled_cloud_job_is_never_reported_as_refunded() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    engine
        .grant_consent("consent-1", 7, "openrouter", "acme/whisper-1")
        .await
        .expect("grant");
    let job = engine
        .enqueue(import_request().with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    engine
        .store()
        .transition(job, JobState::Preflight, TransitionCause::system("p"))
        .await
        .expect("preflight");
    engine
        .store()
        .transition(job, JobState::CloudReady, TransitionCause::system("r"))
        .await
        .expect("ready");
    engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".into()),
            "whisper-1",
            None,
            None,
            7,
        )
        .await
        .expect("dispatch");

    engine.cancel(job).await.expect("cancel");
    let outcome = engine
        .finish_cancellation(job, Vec::new())
        .await
        .expect("finish");
    assert!(outcome.possibly_billed);
    assert!(!outcome.refunded);
    assert_eq!(outcome.message, CLOUD_CANCELLATION_NOTICE);
}

#[tokio::test]
async fn interruptions_park_the_job_and_resume_cleanly() {
    let f = fixture().await;
    let engine = engine_for(&f).await;

    for (cause, expected) in [
        (InterruptionCause::OsSleep, JobState::WaitingForUser),
        (
            InterruptionCause::KeychainLocked,
            JobState::WaitingForAuthorization,
        ),
        (InterruptionCause::UserLogout, JobState::WaitingForUser),
        (InterruptionCause::BluetoothLost, JobState::WaitingForSource),
    ] {
        let key = ImportJobKey::by_content(SourceKind::LocalFile, format!("{:064x}", cause as u8));
        let job = engine
            .enqueue(EnqueueRequest::new("import", key, ResourceClass::LocalDisk))
            .await
            .expect("enqueue")
            .job_id();
        engine
            .store()
            .transition(job, JobState::Preflight, TransitionCause::system("p"))
            .await
            .expect("preflight");
        engine
            .store()
            .transition(job, JobState::RunningLocal, TransitionCause::system("r"))
            .await
            .expect("run");

        let record = engine.interrupt(job, cause).await.expect("interrupt");
        assert_eq!(record.state, expected, "{cause:?}");
        assert_ne!(record.state, JobState::Completed);
        assert_eq!(record.wait_reason, Some(cause.wait_reason()));

        let resumed = engine.resume(job).await.expect("resume");
        assert_eq!(resumed.state, JobState::Queued);
    }
}

#[tokio::test]
async fn resuming_never_redispatches_a_possibly_billed_request() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    engine
        .grant_consent("consent-1", 7, "openrouter", "acme/whisper-1")
        .await
        .expect("grant");
    let job = engine
        .enqueue(import_request().with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    engine
        .store()
        .transition(job, JobState::Preflight, TransitionCause::system("p"))
        .await
        .expect("preflight");
    engine
        .store()
        .transition(job, JobState::CloudReady, TransitionCause::system("r"))
        .await
        .expect("ready");
    let attempt = engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".into()),
            "whisper-1",
            None,
            None,
            7,
        )
        .await
        .expect("dispatch");
    engine
        .dispatch_outcome_unknown(job, &attempt, Some("req-7".into()))
        .await
        .expect("unknown");

    assert!(matches!(
        engine.resume(job).await.expect_err("must refuse"),
        JobError::DispatchRefused(DispatchRefusal::OutcomeUnknown)
    ));
    // And a fresh dispatch under the same consent is refused too.
    assert!(matches!(
        engine
            .begin_dispatch(
                job,
                "a".repeat(64),
                "multipart_stt",
                Some("acme".into()),
                "whisper-1",
                None,
                None,
                7,
            )
            .await
            .expect_err("must refuse"),
        JobError::DispatchRefused(DispatchRefusal::OutcomeUnknown)
    ));
}

#[tokio::test]
async fn consent_revocation_blocks_every_subsequent_dispatch() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    engine
        .grant_consent("consent-1", 7, "openrouter", "acme/whisper-1")
        .await
        .expect("grant");

    let mut jobs = Vec::new();
    for index in 0..4 {
        let key = ImportJobKey::by_content(SourceKind::LocalFile, format!("{index:064}"));
        let job = engine
            .enqueue(
                EnqueueRequest::new("transcribe", key, ResourceClass::CloudDispatch)
                    .with_consent("consent-1"),
            )
            .await
            .expect("enqueue")
            .job_id();
        engine
            .store()
            .transition(job, JobState::Preflight, TransitionCause::system("p"))
            .await
            .expect("preflight");
        engine
            .store()
            .transition(job, JobState::CloudReady, TransitionCause::system("r"))
            .await
            .expect("ready");
        jobs.push(job);
    }

    // One job dispatches before revocation.
    engine
        .begin_dispatch(
            jobs[0],
            "a".repeat(64),
            "multipart_stt",
            Some("acme".into()),
            "whisper-1",
            None,
            None,
            7,
        )
        .await
        .expect("dispatch");
    let authorized_before = engine.consent().authorized_count();

    let parked = engine.revoke_consent("consent-1").await.expect("revoke");
    assert_eq!(parked.len(), 3, "remaining queued jobs are parked");

    for job in &jobs[1..] {
        let record = engine.store().get(*job).await.expect("load");
        assert_eq!(record.state, JobState::WaitingForAuthorization);
        assert_eq!(record.wait_reason, Some(WaitReason::ConsentRequired));

        let error = engine
            .begin_dispatch(
                *job,
                "a".repeat(64),
                "multipart_stt",
                Some("acme".into()),
                "whisper-1",
                None,
                None,
                7,
            )
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            JobError::DispatchRefused(DispatchRefusal::ConsentRevoked)
        ));
    }
    assert_eq!(
        engine.consent().authorized_count(),
        authorized_before,
        "zero subsequent dispatch attempts were authorized"
    );
    // No further ledger rows were created for the refused jobs.
    for job in &jobs[1..] {
        assert_eq!(
            engine.store().attempts(*job).await.expect("attempts").len(),
            0
        );
    }
}

// ---------------------------------------------------------------------------
// C2.1 — durable event log
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct CountingSubscriber {
    id: String,
    seen: std::sync::Mutex<Vec<i64>>,
}

impl CountingSubscriber {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<i64> {
        self.seen.lock().expect("lock").clone()
    }
}

impl EventSubscriber for CountingSubscriber {
    fn consumer_id(&self) -> &str {
        &self.id
    }

    fn handle(&self, event: &EventRecord) -> Result<(), SubscriberError> {
        self.seen.lock().expect("lock").push(event.seq);
        Ok(())
    }
}

#[tokio::test]
async fn event_sequence_is_monotonic_and_gap_free() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    f.store
        .transition(job, JobState::Verifying, TransitionCause::system("v"))
        .await
        .expect("verify");

    let events = f.store.events().read_after(0, 100).await.expect("read");
    let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, contiguous(seqs.len()));
    assert!(events.iter().all(|e| e.kind == EventKind::JobUpdated));
}

#[tokio::test]
async fn a_rolled_back_transaction_leaves_no_event_and_no_gap() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    let before = f.store.events().latest_seq().await.expect("seq");

    // An illegal transition rolls the whole transaction back.
    f.store
        .transition(job, JobState::Completed, TransitionCause::system("cheat"))
        .await
        .expect_err("rejected");
    assert_eq!(f.store.events().latest_seq().await.expect("seq"), before);

    // The next successful transition takes the very next sequence number.
    f.store
        .transition(job, JobState::Verifying, TransitionCause::system("v"))
        .await
        .expect("verify");
    assert_eq!(
        f.store.events().latest_seq().await.expect("seq"),
        before + 1
    );
    let events = f.store.events().read_after(0, 100).await.expect("read");
    let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, contiguous(seqs.len()));
}

#[tokio::test]
async fn events_survive_restart_and_replay_from_the_consumer_cursor() {
    let f = fixture().await;
    let job = enqueue_running(&f.store).await;
    let committed = f.store.events().count().await.expect("count");
    assert!(committed >= 3);
    f.store.database().close().await;

    // A fresh process attaches a subscriber that has never consumed anything.
    let db = Database::open(&f.db_path, 2).await.expect("reopen");
    let store = JobStore::open(db.clone()).await.expect("store");
    let subscriber = Arc::new(CountingSubscriber::new("replay"));
    let dispatcher = plaudon_jobs::events::EventDispatcher::new(
        vec![subscriber.clone()],
        db.writer().clone(),
        store.events().clone(),
    );

    let delivered = dispatcher.replay_pending(64).await.expect("replay");
    assert_eq!(i64::try_from(delivered).expect("fits"), committed);
    assert_eq!(
        subscriber.seen(),
        (1..=committed).collect::<Vec<_>>(),
        "every committed event is replayed once"
    );

    // A second replay delivers nothing: the cursor is durable.
    assert_eq!(dispatcher.replay_pending(64).await.expect("replay"), 0);
    assert_eq!(
        i64::try_from(subscriber.seen().len()).expect("fits"),
        committed
    );
    assert_eq!(
        store.events().cursor("replay").await.expect("cursor"),
        committed
    );
    let _ = job;
}

#[tokio::test]
async fn a_failing_subscriber_never_blocks_or_rolls_back_a_job() {
    let f = fixture().await;
    let db = Database::open(&f.db_path, 2).await.expect("open");
    let store = JobStore::open(db.clone()).await.expect("store");
    let sink = RecordingSink::new();
    sink.fail_from(1);
    let bridge = Arc::new(UiBridge::new("ui", Arc::new(sink.clone())));
    let dispatcher = plaudon_jobs::events::EventDispatcher::new(
        vec![bridge],
        db.writer().clone(),
        store.events().clone(),
    );

    let job = store
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();
    dispatcher.replay_pending(64).await.expect("replay");

    // Delivery failed, but the job is committed and the log is intact.
    assert_eq!(sink.events().len(), 0);
    assert_eq!(store.get(job).await.expect("load").state, JobState::Queued);
    assert!(store.events().count().await.expect("count") >= 1);
    assert_eq!(store.events().cursor("ui").await.expect("cursor"), 0);

    // Once the frontend recovers, the missed events are delivered.
    sink.recover();
    let delivered = dispatcher.replay_pending(64).await.expect("replay");
    assert!(delivered >= 1);
    assert_ne!(sink.events().len(), 0);
}

// ---------------------------------------------------------------------------
// C2.3 — startup reconciler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_reconciler_classifies_interrupted_work_deterministically() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    engine
        .grant_consent("consent-1", 7, "openrouter", "acme/whisper-1")
        .await
        .expect("grant");

    // A local job interrupted while running.
    let local = engine
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();
    engine
        .store()
        .transition(local, JobState::Preflight, TransitionCause::system("p"))
        .await
        .expect("p");
    engine
        .store()
        .transition(local, JobState::RunningLocal, TransitionCause::system("r"))
        .await
        .expect("r");

    // A cloud job interrupted after dispatch.
    let cloud = engine
        .enqueue(
            EnqueueRequest::new(
                "transcribe",
                ImportJobKey::by_content(SourceKind::LocalFile, "b".repeat(64)),
                ResourceClass::CloudDispatch,
            )
            .with_consent("consent-1"),
        )
        .await
        .expect("enqueue")
        .job_id();
    engine
        .store()
        .transition(cloud, JobState::Preflight, TransitionCause::system("p"))
        .await
        .expect("p");
    engine
        .store()
        .transition(cloud, JobState::CloudReady, TransitionCause::system("r"))
        .await
        .expect("r");
    engine
        .begin_dispatch(
            cloud,
            "b".repeat(64),
            "multipart_stt",
            Some("acme".into()),
            "whisper-1",
            None,
            None,
            7,
        )
        .await
        .expect("dispatch");

    let report = engine.recover().await.expect("recover");
    assert!(report.jobs_retryable.contains(&local));
    assert!(report.jobs_outcome_unknown.contains(&cloud));

    assert_eq!(
        engine.store().get(local).await.expect("load").state,
        JobState::FailedRetryable
    );
    let cloud_record = engine.store().get(cloud).await.expect("load");
    assert_eq!(cloud_record.state, JobState::CloudOutcomeUnknown);
    assert!(cloud_record.possibly_billed());
    assert_eq!(
        engine.store().attempts(cloud).await.expect("attempts")[0].status,
        DispatchState::OutcomeUnknown
    );

    // Idempotent: a second run changes nothing.
    let second = engine.recover().await.expect("recover");
    assert!(second.is_noop(), "{second:?}");
}

#[tokio::test]
async fn the_reconciler_removes_only_app_owned_orphans() {
    let f = fixture().await;
    let engine = engine_for(&f).await;

    // An app-owned staging file with no live intent, and a user file that
    // must survive untouched.
    let orphan = f.staging.join("0199bb3b-0000-7000-8000-000000000001.part");
    std::fs::write(&orphan, b"partial").expect("write");
    let source = f.archive.join("original.wav");
    std::fs::write(&source, b"user audio").expect("write");

    let report = engine.recover().await.expect("recover");
    assert_eq!(report.orphans_removed, vec![orphan.clone()]);
    assert!(!orphan.exists());
    assert_eq!(
        std::fs::read(&source).expect("read"),
        b"user audio",
        "source files are never touched"
    );
}

#[tokio::test]
async fn an_interrupted_import_intent_is_recovered_from_its_bytes() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = enqueue_running(&f.store).await;

    let archive = plaudon_persistence::archive::MediaArchive::open(&f.archive).expect("archive");
    let journal = plaudon_persistence::journal::ImportJournal::new(
        plaudon_persistence::Database::open(&f.db_path)
            .await
            .expect("db"),
        archive.clone(),
    );
    let bytes = b"recoverable media bytes".to_vec();
    let staged = journal
        .stage(&mut bytes.as_slice(), "audio/wav")
        .expect("stage");
    let intent = journal
        .record_intent(
            &staged,
            "wav",
            plaudon_domain::media::QualityClass::UserSuppliedFile,
            None,
            Some(job),
            plaudon_domain::time::Timestamp::now(),
        )
        .await
        .expect("intent");

    // Crash equivalent: the intent is durable, the rename never happened.
    let report = engine.recover().await.expect("recover");
    assert_eq!(
        report.journal_completed,
        vec![intent.id.to_string()],
        "verified staged bytes complete the interrupted commit"
    );
    assert!(
        archive
            .exists(&intent.target_relative_path)
            .expect("exists"),
        "the bytes are in the archive"
    );
    archive
        .verify(&intent.target_relative_path, &intent.identity)
        .expect("archived bytes verify");
}

#[tokio::test]
async fn corrupt_staged_bytes_are_quarantined_and_the_job_stays_retryable() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = enqueue_running(&f.store).await;

    let archive = plaudon_persistence::archive::MediaArchive::open(&f.archive).expect("archive");
    let journal = plaudon_persistence::journal::ImportJournal::new(
        plaudon_persistence::Database::open(&f.db_path)
            .await
            .expect("db"),
        archive,
    );
    let bytes = b"bytes that will be tampered with".to_vec();
    let staged = journal
        .stage(&mut bytes.as_slice(), "audio/wav")
        .expect("stage");
    let intent = journal
        .record_intent(
            &staged,
            "wav",
            plaudon_domain::media::QualityClass::UserSuppliedFile,
            None,
            Some(job),
            plaudon_domain::time::Timestamp::now(),
        )
        .await
        .expect("intent");
    std::fs::write(&staged.path, b"tampered").expect("tamper");

    let report = engine.recover().await.expect("recover");
    assert_eq!(report.journal_restaged, vec![intent.id.to_string()]);
    assert!(report.jobs_retryable.contains(&job));
    let record = f.store.get(job).await.expect("load");
    assert_eq!(record.state, JobState::FailedRetryable);
    assert_ne!(record.state, JobState::Completed);
}

#[tokio::test]
async fn reconciliation_never_resurrects_a_user_deleted_recording() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let recording = RecordingId::new();
    engine
        .store()
        .tombstone(recording, "user_deleted")
        .await
        .expect("tombstone");

    let job = engine
        .enqueue(import_request().with_recording(recording))
        .await
        .expect("enqueue")
        .job_id();

    engine.recover().await.expect("recover");
    let record = engine.store().get(job).await.expect("load");
    assert_eq!(record.state, JobState::FailedTerminal);
    assert_eq!(
        record.last_error_code.as_deref(),
        Some(AppErrorCode::RecordNotFound.as_str())
    );
    assert!(engine
        .store()
        .is_tombstoned(recording)
        .await
        .expect("check"));
}

// ---------------------------------------------------------------------------
// C2.5 — typed events and persistent history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ui_events_carry_the_durable_sequence_and_only_committed_state() {
    let f = fixture().await;
    let db = Database::open(&f.db_path, 2).await.expect("open");
    let store = JobStore::open(db.clone()).await.expect("store");
    let sink = RecordingSink::new();
    let dispatcher = plaudon_jobs::events::EventDispatcher::new(
        vec![Arc::new(UiBridge::new(
            UiBridge::DESKTOP_CONSUMER_ID,
            Arc::new(sink.clone()),
        ))],
        db.writer().clone(),
        store.events().clone(),
    );

    let job = enqueue_running(&store).await;
    dispatcher.replay_pending(64).await.expect("replay");

    let seqs = sink.seqs();
    assert_eq!(seqs, contiguous(seqs.len()));
    let last = sink.events().last().cloned().expect("event");
    assert_eq!(last.job_id, Some(job));
    assert!(matches!(
        last.payload,
        EventPayload::JobUpdated {
            state: JobState::RunningLocal,
            ..
        }
    ));

    // No secret-shaped value anywhere in the serialized stream.
    for event in sink.events() {
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            plaudon_jobs::ui::scan_for_secret_shapes(&json).is_empty(),
            "{json}"
        );
    }
}

#[tokio::test]
async fn dropping_frontend_delivery_loses_no_state() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = engine
        .enqueue(import_request())
        .await
        .expect("enqueue")
        .job_id();
    for state in [
        JobState::Preflight,
        JobState::RunningLocal,
        JobState::Verifying,
        JobState::Committing,
    ] {
        engine
            .store()
            .transition(job, state, TransitionCause::system("advance"))
            .await
            .expect("advance");
    }
    engine
        .store()
        .complete(
            job,
            vec![JobOutput::new("media_asset", "sha256/aa/aa.wav")],
            TransitionCause::system("done"),
        )
        .await
        .expect("complete");

    // The UI never received a single event, yet a refetch reconstructs
    // everything from the database.
    let page = engine
        .history()
        .list_jobs(&JobHistoryQuery::all())
        .await
        .expect("list");
    assert_eq!(page.total, 1);
    assert_eq!(page.jobs[0].state, JobState::Completed);
    assert_eq!(page.jobs[0].output_count, 1);
    assert!(page.watermark_seq > 0);

    let activity = engine.history().activity(50).await.expect("activity");
    assert!(
        activity.iter().any(|entry| entry.job_id == Some(job)),
        "in-app activity history is present without any notification"
    );
    assert_eq!(activity[0].seq, page.watermark_seq);
}

#[tokio::test]
async fn history_filters_by_state_and_recording() {
    let f = fixture().await;
    let recording = RecordingId::new();
    let mine = f
        .store
        .enqueue(import_request().with_recording(recording))
        .await
        .expect("enqueue")
        .job_id();
    f.store
        .enqueue(EnqueueRequest::new(
            "import",
            ImportJobKey::by_content(SourceKind::LocalFile, "d".repeat(64)),
            ResourceClass::LocalDisk,
        ))
        .await
        .expect("enqueue");
    f.store
        .transition(mine, JobState::Preflight, TransitionCause::system("p"))
        .await
        .expect("p");

    let history = JobHistory::new(f.store.read_pool().clone());
    let by_recording = history
        .list_jobs(&JobHistoryQuery::all().for_recording(recording))
        .await
        .expect("list");
    assert_eq!(by_recording.total, 1);
    assert_eq!(by_recording.jobs[0].job_id, mine);

    let by_state = history
        .list_jobs(&JobHistoryQuery::all().with_states(vec![JobState::Queued]))
        .await
        .expect("list");
    assert_eq!(by_state.total, 1);
    assert_ne!(by_state.jobs[0].job_id, mine);

    let all = history
        .list_jobs(&JobHistoryQuery::all())
        .await
        .expect("list");
    assert_eq!(all.total, 2);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async fn engine_for(f: &Fixture) -> JobEngine {
    let db = Database::open(&f.db_path, 4).await.expect("open");
    let mut config = EngineConfig::new(&f.staging).with_archive_root(&f.archive);
    config.reconciler = config
        .reconciler
        .with_orphan_grace(std::time::Duration::from_secs(3600));
    JobEngine::open(db, config, Vec::new())
        .await
        .expect("engine")
}

/// `[1, 2, ..., len]`, the shape a gap-free sequence must have.
fn contiguous(len: usize) -> Vec<i64> {
    (1..=i64::try_from(len).expect("fits")).collect()
}

async fn count_jobs(store: &JobStore) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(store.read_pool())
        .await
        .expect("count")
}
