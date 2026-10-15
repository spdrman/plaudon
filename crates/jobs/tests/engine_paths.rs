//! Refusal, park and recovery paths of the job engine facade
//! (SPEC v1.3 §14.3, §14.4, §14.7, §16.3).

mod common;

use std::sync::Arc;

use common::{
    advance, engine_for, engine_with_archive, engine_with_limits, engine_with_subscribers, enqueue,
    fixture, import_request, Fixture,
};

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::JobId;
use plaudon_jobs::cancel::InterruptionCause;
use plaudon_jobs::engine::{EngineConfig, JobEngine};
use plaudon_jobs::error::{DispatchRefusal, JobError};
use plaudon_jobs::events::{EventSubscriber, SubscriberError};
use plaudon_jobs::scheduler::{
    PowerSource, PreflightDecision, ResourceClass, ResourceRequest, SchedulerLimits,
    SystemResources, WaitReason,
};
use plaudon_jobs::state::{JobState, JobTransitionError, TransitionCause};
use plaudon_jobs::store::{DispatchState, JobOutput};
use plaudon_jobs::ui::{RecordingSink, UiBridge};

const DESTINATION: u64 = 7;

fn roomy() -> SystemResources {
    SystemResources {
        available_disk_bytes: 64 * 1024 * 1024 * 1024,
        available_ram_bytes: 16 * 1024 * 1024 * 1024,
        power_source: PowerSource::Ac,
        power_saver_enabled: false,
        model_asset_present: true,
        source_available: true,
    }
}

/// A cloud job sitting in `CloudReady` with a live consent grant.
async fn cloud_ready(engine: &JobEngine, seed: char) -> JobId {
    engine
        .grant_consent("consent-1", DESTINATION, "openrouter", "acme/whisper-1")
        .await
        .expect("grant");
    let job = engine
        .enqueue(import_request(seed).with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    advance(
        engine.store(),
        job,
        &[JobState::Preflight, JobState::CloudReady],
    )
    .await;
    job
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_engine_exposes_every_component_it_wires_together() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = engine
        .enqueue(import_request('a'))
        .await
        .expect("enqueue")
        .job_id();

    assert_eq!(engine.store().get(job).await.expect("load").id, job);
    assert_eq!(
        engine
            .history()
            .list_jobs(&plaudon_jobs::history::JobHistoryQuery::all())
            .await
            .expect("list")
            .total,
        1
    );
    assert_eq!(engine.scheduler().limits().get(ResourceClass::Cpu), 1);
    assert!(engine.cancellation().get(job).is_some());
    assert_eq!(engine.consent().refused_count(), 0);
    assert!(
        engine.dispatcher().consumer_ids().is_empty(),
        "no subscribers were registered"
    );
    assert!(!engine.token(job).is_cancelled());
    assert!(format!("{engine:?}").contains("limits"));

    let config = EngineConfig::new(&f.staging).with_archive_root(&f.archive);
    assert_eq!(config.archive_root.as_deref(), Some(f.archive.as_path()));
    assert_eq!(config.reconciler.staging_root, f.staging);
}

// ---------------------------------------------------------------------------
// Cloud dispatch refusals (§16.3 fail-closed)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_job_without_a_consent_grant_never_reaches_the_ledger() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = enqueue(engine.store(), 'b').await;
    advance(
        engine.store(),
        job,
        &[JobState::Preflight, JobState::CloudReady],
    )
    .await;

    let error = engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".to_string()),
            "whisper-1",
            None,
            None,
            DESTINATION,
        )
        .await
        .expect_err("must refuse");

    assert!(matches!(
        error,
        JobError::DispatchRefused(DispatchRefusal::NoConsentGrant)
    ));
    assert_eq!(error.code(), AppErrorCode::ConsentRequired);
    let ledger = engine.store().attempts(job).await.expect("ledger");
    assert!(ledger.is_empty(), "a refusal writes no ledger row");
    assert_eq!(
        engine.store().get(job).await.expect("load").state,
        JobState::CloudReady
    );
    assert!(!engine.store().get(job).await.expect("load").dispatched);
}

#[tokio::test]
async fn a_destination_that_changed_since_consent_is_refused() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = cloud_ready(&engine, 'c').await;

    let error = engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".to_string()),
            "whisper-1",
            None,
            None,
            DESTINATION + 1,
        )
        .await
        .expect_err("must refuse");

    assert!(matches!(
        error,
        JobError::DispatchRefused(DispatchRefusal::DestinationChanged)
    ));
    assert_eq!(engine.consent().refused_count(), 1);
    let ledger = engine.store().attempts(job).await.expect("ledger");
    assert!(ledger.is_empty(), "a refusal writes no ledger row");
}

#[tokio::test]
async fn an_expired_consent_grant_is_refused() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = cloud_ready(&engine, 'd').await;
    engine.consent().expire("consent-1");

    let error = engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            None,
            "whisper-1",
            None,
            None,
            DESTINATION,
        )
        .await
        .expect_err("must refuse");

    assert!(matches!(
        error,
        JobError::DispatchRefused(DispatchRefusal::ConsentExpired)
    ));
    let ledger = engine.store().attempts(job).await.expect("ledger");
    assert!(ledger.is_empty(), "a refusal writes no ledger row");
}

#[tokio::test]
async fn a_recorded_response_moves_the_job_on_and_settles_the_ledger_row() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = cloud_ready(&engine, 'e').await;

    let attempt = engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".to_string()),
            "whisper-1",
            Some("eu".to_string()),
            Some("zdr@1".to_string()),
            DESTINATION,
        )
        .await
        .expect("dispatch");
    assert_eq!(
        engine.store().get(job).await.expect("load").state,
        JobState::CloudSending
    );

    let record = engine
        .finish_dispatch(job, &attempt, Some("req-1".to_string()))
        .await
        .expect("finish");

    assert_eq!(record.state, JobState::Verifying);
    let ledger = engine.store().attempts(job).await.expect("ledger");
    assert_eq!(ledger[0].status, DispatchState::Completed);
    assert_eq!(ledger[0].provider_request_id, Some("req-1".to_string()));
    let dangling = engine.store().dangling_attempts().await.expect("dangling");
    assert!(
        dangling.is_empty(),
        "an answered dispatch is never dangling"
    );
}

#[tokio::test]
async fn a_missing_response_parks_the_job_as_possibly_billed_until_the_user_acts() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = cloud_ready(&engine, 'f').await;
    let attempt = engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".to_string()),
            "whisper-1",
            None,
            None,
            DESTINATION,
        )
        .await
        .expect("dispatch");

    let record = engine
        .dispatch_outcome_unknown(job, &attempt, None)
        .await
        .expect("no response");
    assert_eq!(record.state, JobState::CloudOutcomeUnknown);
    assert!(record.possibly_billed());
    assert_eq!(
        engine.store().attempts(job).await.expect("ledger")[0].status,
        DispatchState::OutcomeUnknown
    );

    // Neither the scheduler nor an unacknowledged user action may leave it.
    let error = engine
        .store()
        .transition(job, JobState::CloudReady, TransitionCause::system("retry"))
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::RequiresUserAction { .. })
    ));
    assert_eq!(error.code(), AppErrorCode::ConfirmationRequired);

    let error = engine
        .store()
        .transition(job, JobState::CloudReady, TransitionCause::user("retry"))
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::Transition(JobTransitionError::RequiresCostAcknowledgement)
    ));
    assert_eq!(error.code(), AppErrorCode::CostAcknowledgementRequired);

    // Resuming is refused outright while the outcome is unknown.
    let error = engine.resume(job).await.expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::DispatchRefused(DispatchRefusal::OutcomeUnknown)
    ));
    assert_eq!(error.code(), AppErrorCode::OutcomeUnknown);

    // The gate refuses a dispatch for as long as the job sits in
    // `CloudOutcomeUnknown`, even with a live grant for this destination.
    let error = engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            None,
            "whisper-1",
            None,
            None,
            DESTINATION,
        )
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::DispatchRefused(DispatchRefusal::OutcomeUnknown)
    ));

    // Only an explicit, cost-acknowledged action moves it.
    let retried = engine
        .user_retry_possibly_billed(job)
        .await
        .expect("explicit retry");
    assert_eq!(retried.state, JobState::CloudReady);
}

// ---------------------------------------------------------------------------
// Preflight parking (§14.4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preflight_admits_a_job_the_machine_has_room_for() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = enqueue(engine.store(), 'g').await;

    let decision = engine
        .preflight(job, ResourceRequest::default(), roomy())
        .await
        .expect("preflight");

    assert_eq!(decision, PreflightDecision::Admit);
    assert_eq!(
        engine.store().get(job).await.expect("load").state,
        JobState::Preflight
    );
}

#[tokio::test]
async fn every_preflight_shortage_parks_the_job_durably_instead_of_failing_it() {
    let f = fixture().await;
    let engine = engine_for(&f).await;

    let shortages: [(char, ResourceRequest, SystemResources, WaitReason); 5] = [
        (
            '1',
            ResourceRequest {
                needs_model_asset: true,
                ..ResourceRequest::default()
            },
            SystemResources {
                model_asset_present: false,
                ..roomy()
            },
            WaitReason::ModelAssetMissing,
        ),
        (
            '2',
            ResourceRequest {
                needs_source: true,
                ..ResourceRequest::default()
            },
            SystemResources {
                source_available: false,
                ..roomy()
            },
            WaitReason::SourceUnavailable,
        ),
        (
            '3',
            ResourceRequest {
                required_disk_bytes: 1,
                ..ResourceRequest::default()
            },
            SystemResources {
                available_disk_bytes: 0,
                ..roomy()
            },
            WaitReason::InsufficientDisk,
        ),
        (
            '4',
            ResourceRequest {
                estimated_ram_bytes: 1,
                ..ResourceRequest::default()
            },
            SystemResources {
                available_ram_bytes: 0,
                ..roomy()
            },
            WaitReason::InsufficientMemory,
        ),
        (
            '5',
            ResourceRequest {
                heavy_local_compute: true,
                ..ResourceRequest::default()
            },
            SystemResources {
                power_saver_enabled: true,
                power_source: PowerSource::Battery,
                ..roomy()
            },
            WaitReason::PowerSaver,
        ),
    ];

    for (seed, request, system, expected) in shortages {
        let job = enqueue(engine.store(), seed).await;
        let decision = engine
            .preflight(job, request, system)
            .await
            .expect("preflight");
        assert_eq!(
            decision,
            PreflightDecision::Park {
                state: expected.wait_state(),
                reason: expected,
            }
        );
        let record = engine.store().get(job).await.expect("load");
        assert_eq!(record.state, expected.wait_state());
        assert_eq!(record.wait_reason, Some(expected));
        assert!(!record.state.is_terminal(), "parking is never a failure");
    }
}

// ---------------------------------------------------------------------------
// Scheduling and cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_class_configured_with_no_capacity_parks_rather_than_overrunning() {
    let f = fixture().await;
    let engine =
        engine_with_limits(&f, SchedulerLimits::default().with(ResourceClass::Gpu, 0)).await;
    let job = enqueue(engine.store(), 'h').await;

    let error = engine
        .admit(job, ResourceClass::Gpu)
        .await
        .expect_err("must refuse");
    assert!(matches!(
        error,
        JobError::InsufficientResources {
            resource: "configured concurrency",
            needed: 1,
            available: 0,
        }
    ));
    assert_eq!(error.code(), AppErrorCode::StorageFull);
    assert!(error.is_retryable());

    // A class with capacity admits and releases its slot.
    let admission = engine
        .admit(job, ResourceClass::LocalDisk)
        .await
        .expect("admit");
    assert_eq!(admission.class(), ResourceClass::LocalDisk);
    assert_eq!(engine.scheduler().in_flight(ResourceClass::LocalDisk), 1);
    drop(admission);
    assert_eq!(engine.scheduler().in_flight(ResourceClass::LocalDisk), 0);
}

#[tokio::test]
async fn a_cancelled_job_is_never_admitted() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = enqueue(engine.store(), 'i').await;
    engine.token(job).cancel();

    let error = engine
        .admit(job, ResourceClass::LocalDisk)
        .await
        .expect_err("must refuse");
    assert!(matches!(error, JobError::Cancelled(id) if id == job));
    assert_eq!(error.code(), AppErrorCode::Canceled);
}

#[tokio::test]
async fn cancelling_a_terminal_job_is_acknowledged_without_being_accepted() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = enqueue(engine.store(), 'j').await;
    advance(
        engine.store(),
        job,
        &[
            JobState::Preflight,
            JobState::Cancelling,
            JobState::Cancelled,
        ],
    )
    .await;

    let ack = engine.cancel(job).await.expect("ack");

    assert_eq!(ack.job_id, job);
    assert!(!ack.accepted, "a settled job cannot be cancelled again");
    assert_eq!(ack.state, JobState::Cancelled);
    assert_eq!(
        engine
            .store()
            .get(job)
            .await
            .expect("load")
            .cancel_requested_at,
        None,
        "no cancellation request is recorded for a terminal job"
    );
}

#[tokio::test]
async fn cancelling_a_dispatched_job_reports_it_as_possibly_billed_and_never_refunded() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = cloud_ready(&engine, 'k').await;
    engine
        .begin_dispatch(
            job,
            "a".repeat(64),
            "multipart_stt",
            Some("acme".to_string()),
            "whisper-1",
            None,
            None,
            DESTINATION,
        )
        .await
        .expect("dispatch");

    let ack = engine.cancel(job).await.expect("ack");
    assert!(ack.accepted);
    assert_eq!(ack.state, JobState::Cancelling);
    assert!(engine.cancellation().is_cancelled(job));

    let outcome = engine
        .finish_cancellation(job, Vec::new())
        .await
        .expect("finish");

    assert!(outcome.possibly_billed);
    assert!(!outcome.refunded);
    assert_eq!(
        outcome.message,
        plaudon_jobs::cancel::CLOUD_CANCELLATION_NOTICE
    );
    assert_eq!(outcome.final_state, JobState::Cancelled);
    assert!(engine.cancellation().get(job).is_none());
}

#[tokio::test]
async fn cancellation_discards_only_app_owned_staging_files() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = enqueue(engine.store(), 'l').await;
    advance(
        engine.store(),
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;

    let derivative = f.staging.join(format!("{job}.part"));
    std::fs::write(&derivative, b"half written").expect("write derivative");
    let source = f.dir.path().join("user-recording.wav");
    std::fs::write(&source, b"the user's own file").expect("write source");
    let absent = f.staging.join("already-gone.part");

    engine.cancel(job).await.expect("cancel");
    let outcome = engine
        .finish_cancellation(job, vec![derivative.clone(), source.clone(), absent])
        .await
        .expect("finish");

    assert_eq!(outcome.discarded_derivatives, vec![derivative.clone()]);
    assert!(!outcome.possibly_billed);
    assert!(!derivative.exists());
    assert!(source.exists(), "a user file is never discarded");
    assert!(outcome.message.contains("Incomplete intermediate files"));
}

#[tokio::test]
async fn a_terminal_failure_forgets_the_token_while_a_retryable_one_keeps_it() {
    let f = fixture().await;
    let engine = engine_for(&f).await;

    let retryable = engine
        .enqueue(import_request('m'))
        .await
        .expect("enqueue")
        .job_id();
    advance(engine.store(), retryable, &[JobState::Preflight]).await;
    let record = engine
        .fail(retryable, false, AppErrorCode::HelperFailed, "helper_died")
        .await
        .expect("fail");
    assert_eq!(record.state, JobState::FailedRetryable);
    assert!(engine.cancellation().get(retryable).is_some());

    let terminal = engine
        .enqueue(import_request('n'))
        .await
        .expect("enqueue")
        .job_id();
    let record = engine
        .fail(terminal, true, AppErrorCode::IntegrityFailed, "corrupt")
        .await
        .expect("fail");
    assert_eq!(record.state, JobState::FailedTerminal);
    assert!(engine.cancellation().get(terminal).is_none());
}

#[tokio::test]
async fn every_interruption_parks_a_running_job_and_the_resume_requeues_it() {
    let f = fixture().await;
    let engine = engine_for(&f).await;

    for (seed, cause) in "123456789".chars().zip(InterruptionCause::ALL.iter()) {
        let job = enqueue(engine.store(), seed).await;
        advance(
            engine.store(),
            job,
            &[JobState::Preflight, JobState::RunningLocal],
        )
        .await;

        let parked = engine.interrupt(job, *cause).await.expect("interrupt");
        assert_eq!(parked.state, cause.wait_state());
        assert_eq!(parked.wait_reason, Some(cause.wait_reason()));
        assert_ne!(parked.state, JobState::Completed);

        let resumed = engine.resume(job).await.expect("resume");
        assert_eq!(resumed.state, JobState::Queued);
        assert_eq!(resumed.wait_reason, None);
    }
}

#[tokio::test]
async fn revoking_consent_parks_only_the_jobs_that_depend_on_it() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    engine
        .grant_consent("consent-1", DESTINATION, "openrouter", "acme/whisper-1")
        .await
        .expect("grant");
    engine
        .grant_consent("consent-2", DESTINATION, "openrouter", "acme/whisper-1")
        .await
        .expect("grant");

    let affected = engine
        .enqueue(import_request('o').with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    let other_grant = engine
        .enqueue(import_request('p').with_consent("consent-2"))
        .await
        .expect("enqueue")
        .job_id();
    let no_grant = enqueue(engine.store(), 'q').await;
    let running = engine
        .enqueue(import_request('r').with_consent("consent-1"))
        .await
        .expect("enqueue")
        .job_id();
    advance(
        engine.store(),
        running,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;

    let parked = engine.revoke_consent("consent-1").await.expect("revoke");

    assert_eq!(parked, vec![affected]);
    assert_eq!(
        engine.store().get(affected).await.expect("load").state,
        JobState::WaitingForAuthorization
    );
    assert_eq!(
        engine.store().get(other_grant).await.expect("load").state,
        JobState::Queued
    );
    assert_eq!(
        engine.store().get(no_grant).await.expect("load").state,
        JobState::Queued
    );
    assert_eq!(
        engine.store().get(running).await.expect("load").state,
        JobState::RunningLocal,
        "a job already running is not swept by the revocation pass"
    );
}

// ---------------------------------------------------------------------------
// Startup recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_flags_completed_jobs_whose_bytes_are_missing_or_mismatched() {
    let f = fixture().await;
    let engine = engine_with_archive(&f).await;

    let missing = complete_with(&engine, 's', "media_asset", "media/sha256/aa/aa.wav", true).await;
    let mismatched =
        complete_with(&engine, 't', "media_asset", "media/sha256/bb/bb.wav", true).await;
    write_asset(&f, "media/sha256/bb/bb.wav", b"different bytes");
    let undigested =
        complete_with(&engine, 'u', "media_asset", "media/sha256/cc/cc.wav", false).await;
    write_asset(&f, "media/sha256/cc/cc.wav", b"present");
    let identifier = complete_with(&engine, 'v', "transcript_revision", "rev-1", false).await;

    let report = engine.recover().await.expect("recover");

    assert!(report.completed_without_bytes.contains(&missing));
    assert!(report.completed_without_bytes.contains(&mismatched));
    assert!(
        !report.completed_without_bytes.contains(&undigested),
        "an output with no recorded digest only has to exist"
    );
    assert!(
        !report.completed_without_bytes.contains(&identifier),
        "a non-file reference is not verified against the archive"
    );
}

#[tokio::test]
async fn recovery_without_an_archive_root_still_reclassifies_interrupted_work() {
    let f = fixture().await;
    let engine = engine_for(&f).await;
    let job = engine
        .enqueue(import_request('y'))
        .await
        .expect("enqueue")
        .job_id();
    advance(
        engine.store(),
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;

    let report = engine.recover().await.expect("recover");

    assert_eq!(report.jobs_retryable, vec![job]);
    assert!(
        report.completed_without_bytes.is_empty(),
        "with no archive root nothing is verified against bytes"
    );
    assert_eq!(
        engine.store().get(job).await.expect("load").state,
        JobState::FailedRetryable
    );
    // A second pass over the same store is a no-op.
    assert!(engine.recover().await.expect("recover").is_noop());
}

#[tokio::test]
async fn recovery_refuses_an_archive_root_that_is_not_a_directory() {
    let f = fixture().await;
    let blocker = f.dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"regular file").expect("blocker");
    let db = plaudon_jobs::writer::Database::open(&f.db_path, 2)
        .await
        .expect("open");
    let engine = JobEngine::open(
        db,
        EngineConfig::new(&f.staging).with_archive_root(&blocker),
        Vec::new(),
    )
    .await
    .expect("engine");

    let error = engine.recover().await.expect_err("must refuse");

    assert!(matches!(error, JobError::Journal(_)), "{error:?}");
    assert_eq!(error.code(), AppErrorCode::StorageUnavailable);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn the_event_pump_survives_a_database_that_went_away() {
    let f = fixture().await;
    let sink = RecordingSink::new();
    let bridge: Arc<dyn EventSubscriber> = Arc::new(UiBridge::new(
        UiBridge::DESKTOP_CONSUMER_ID,
        Arc::new(sink.clone()),
    ));
    let engine = engine_with_subscribers(&f, vec![bridge]).await;
    engine
        .enqueue(import_request('w'))
        .await
        .expect("enqueue through the engine so the pump runs");
    assert!(!sink.seqs().is_empty(), "the first pump delivered");

    engine.store().database().close().await;

    // A dispatch failure is logged, never propagated: the job already
    // committed and the events stay in the durable log.
    engine.pump().await;
}

/// A subscriber that refuses every event it is handed.
#[derive(Debug)]
struct AlwaysFails {
    id: String,
}

impl EventSubscriber for AlwaysFails {
    fn consumer_id(&self) -> &str {
        &self.id
    }

    fn handle(&self, _event: &plaudon_jobs::events::EventRecord) -> Result<(), SubscriberError> {
        Err(SubscriberError {
            consumer: "always-fails".to_string(),
            reason: "frontend unavailable".to_string(),
        })
    }
}

#[tokio::test]
async fn a_subscriber_that_always_fails_never_advances_its_cursor_or_blocks_a_job() {
    let f = fixture().await;
    let engine = engine_with_subscribers(
        &f,
        vec![Arc::new(AlwaysFails {
            id: "always-fails".to_string(),
        })],
    )
    .await;

    let job = engine
        .enqueue(import_request('x'))
        .await
        .expect("enqueue")
        .job_id();

    assert_eq!(
        engine.store().get(job).await.expect("load").state,
        JobState::Queued
    );
    assert_eq!(
        engine
            .store()
            .events()
            .cursor("always-fails")
            .await
            .expect("cursor"),
        0,
        "the cursor never moves past an event the subscriber refused"
    );
    assert!(engine.store().events().latest_seq().await.expect("seq") > 0);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Drive a fresh job all the way to `Completed` with one output.
async fn complete_with(
    engine: &JobEngine,
    seed: char,
    kind: &str,
    reference: &str,
    digested: bool,
) -> JobId {
    let job = enqueue(engine.store(), seed).await;
    advance(
        engine.store(),
        job,
        &[
            JobState::Preflight,
            JobState::RunningLocal,
            JobState::Verifying,
        ],
    )
    .await;
    let output = if digested {
        JobOutput::new(kind, reference).verified("a".repeat(64), 7)
    } else {
        JobOutput::new(kind, reference)
    };
    engine.complete(job, vec![output]).await.expect("complete");
    job
}

fn write_asset(f: &Fixture, reference: &str, bytes: &[u8]) {
    let path = f.archive.join(reference);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("asset dir");
    std::fs::write(path, bytes).expect("asset bytes");
}
