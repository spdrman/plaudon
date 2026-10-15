//! Boundary and rejection behaviour of the engine's pure value types:
//! job keys, the error taxonomy, scheduler vocabulary and cancellation helpers.

use std::collections::BTreeMap;

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::JobId;
use plaudon_jobs::cancel::{
    discard_incomplete_derivatives, is_app_owned_staging_file, CancellationOutcome,
    CancellationRegistry, ConsentGate, InterruptionCause, CLOUD_CANCELLATION_NOTICE,
    CONSENT_REVOKED_NOTICE,
};
use plaudon_jobs::error::{DispatchRefusal, JobError};
use plaudon_jobs::key::{
    ImportJobKey, JobKey, JobKeyDigest, ProcessingJobKey, ProcessingStage, ReprocessToken,
    SourceKind,
};
use plaudon_jobs::scheduler::{ResourceClass, ResourceScheduler, SchedulerLimits, WaitReason};
use plaudon_jobs::state::{is_legal_edge, successors, JobState, JobTransitionError};

// ---------------------------------------------------------------------------
// Job keys (§14.2)
// ---------------------------------------------------------------------------

#[test]
fn only_sixty_four_lowercase_hex_characters_are_a_canonical_digest() {
    let valid = "a".repeat(64);
    let digest = JobKeyDigest::from_hex(valid.clone()).expect("valid");
    assert_eq!(digest.as_str(), valid);
    assert_eq!(digest.to_string(), valid);

    for rejected in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "z".repeat(64),
        format!("{}Z", "a".repeat(63)),
    ] {
        let error = JobKeyDigest::from_hex(rejected.clone()).expect_err("must refuse");
        assert_eq!(error.value, rejected);
        assert!(
            error.to_string().contains("canonical job key digest"),
            "{error}"
        );
    }
}

#[test]
fn an_import_key_identifies_by_content_or_by_source_item_but_never_by_nothing() {
    let by_content = ImportJobKey::by_content(SourceKind::LocalFile, "a".repeat(64));
    assert!(by_content.is_identifying());

    let by_item = ImportJobKey::by_source_item(SourceKind::PlaudCloudCli, "acct-1", "item-1");
    assert!(by_item.is_identifying());

    let anonymous = ImportJobKey {
        source_kind: SourceKind::WatchedFolder,
        source_id: Some("folder-1".to_string()),
        external_id: None,
        content_sha256: None,
        reprocess: None,
    };
    assert!(
        !anonymous.is_identifying(),
        "a source without an item identity or a hash identifies nothing"
    );

    let nothing = ImportJobKey {
        source_kind: SourceKind::PlaudDeviceBle,
        source_id: None,
        external_id: Some("item-1".to_string()),
        content_sha256: None,
        reprocess: None,
    };
    assert!(!nothing.is_identifying());
}

#[test]
fn a_reprocess_token_is_part_of_the_key_and_visible_on_it() {
    let token = ReprocessToken::new();
    assert_eq!(ReprocessToken::from_uuid(token.as_uuid()), token);
    assert_ne!(ReprocessToken::default(), token);

    let plain: JobKey = ImportJobKey::by_content(SourceKind::LocalFile, "a".repeat(64)).into();
    let reprocessed: JobKey = ImportJobKey::by_content(SourceKind::LocalFile, "a".repeat(64))
        .with_reprocess(token.clone())
        .into();

    assert_eq!(plain.kind_str(), "import");
    assert!(!plain.is_reprocess());
    assert_eq!(plain.reprocess_nonce(), None);
    assert!(reprocessed.is_reprocess());
    assert_eq!(reprocessed.reprocess_nonce(), Some(&token));
    assert_ne!(plain.digest(), reprocessed.digest());

    let processing: JobKey =
        ProcessingJobKey::new("a".repeat(64), ProcessingStage::Diarize, "whisper_cpp", "m")
            .with_reprocess(token.clone())
            .into();
    assert_eq!(processing.kind_str(), "processing");
    assert!(processing.is_reprocess());
    assert_eq!(processing.reprocess_nonce(), Some(&token));
}

#[test]
fn an_absent_optional_field_never_encodes_like_an_empty_one() {
    let absent = ProcessingJobKey::new("a".repeat(64), ProcessingStage::ToolRun, "e", "m");
    let mut empty = absent.clone();
    empty.transcript_revision = Some(String::new());
    let mut configured = absent.clone();
    configured
        .normalization_config
        .insert("loudness".to_string(), "ebu-r128".to_string());

    let digests = [
        JobKey::Processing(absent).digest(),
        JobKey::Processing(empty).digest(),
        JobKey::Processing(configured).digest(),
    ];
    let mut unique = digests.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 3, "every keyed dimension must be injective");
}

#[test]
fn the_canonical_text_is_printable_and_covers_every_keyed_dimension() {
    let mut normalization = BTreeMap::new();
    normalization.insert("resample_hz".to_string(), "16000".to_string());
    let key = JobKey::Processing(ProcessingJobKey {
        audio_sha256: "a".repeat(64),
        transcript_revision: Some("rev-1".to_string()),
        stage: ProcessingStage::SpeakerMatch,
        engine: "whisper_cpp".to_string(),
        model: "large-v3".to_string(),
        model_version: Some("1.5.4".to_string()),
        normalization_config: normalization,
        prompt_template_version: Some("meeting@3".to_string()),
        privacy_policy_snapshot: Some("zdr@2".to_string()),
        reprocess: Some(ReprocessToken::new()),
    });

    let text = key.canonical_text();
    assert!(!text.contains('\u{1f}'), "separators are escaped");
    assert!(!text.contains('\u{1e}'), "separators are escaped");
    assert!(text.contains("speaker_match"), "{text}");
    assert!(text.contains("zdr@2"), "{text}");
    assert_eq!(text.len(), key.canonical_bytes().len());
}

#[test]
fn every_source_kind_and_processing_stage_has_a_distinct_stable_name() {
    let source_names = [
        SourceKind::PlaudDeviceBle,
        SourceKind::PlaudDeviceUsb,
        SourceKind::PlaudCloudCli,
        SourceKind::LocalFile,
        SourceKind::WatchedFolder,
    ]
    .map(SourceKind::as_str);
    let mut unique = source_names.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), source_names.len());

    let stage_names = [
        ProcessingStage::Transcribe,
        ProcessingStage::Diarize,
        ProcessingStage::SpeakerEmbed,
        ProcessingStage::SpeakerMatch,
        ProcessingStage::Normalize,
        ProcessingStage::SectionSummary,
        ProcessingStage::ConsolidatedSummary,
        ProcessingStage::StructuredExtraction,
        ProcessingStage::CitationVerification,
        ProcessingStage::ToolRun,
    ]
    .map(ProcessingStage::as_str);
    let mut unique = stage_names.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), stage_names.len());
}

// ---------------------------------------------------------------------------
// Error taxonomy (§15.4)
// ---------------------------------------------------------------------------

#[test]
fn every_engine_error_maps_onto_the_shared_taxonomy_with_a_stable_retry_rule() {
    let job = JobId::new();
    let cases: Vec<(JobError, AppErrorCode, bool)> = vec![
        (
            JobError::Transition(JobTransitionError::IllegalEdge {
                from: JobState::Queued,
                to: JobState::Completed,
            }),
            AppErrorCode::InternalError,
            false,
        ),
        (
            JobError::Transition(JobTransitionError::MissingCommittedOutput),
            AppErrorCode::InternalError,
            false,
        ),
        (
            JobError::Transition(JobTransitionError::AlreadyTerminal {
                state: JobState::Cancelled,
            }),
            AppErrorCode::Conflict,
            false,
        ),
        (
            JobError::Transition(JobTransitionError::StateConflict {
                expected: JobState::Queued,
                actual: JobState::Preflight,
            }),
            AppErrorCode::Conflict,
            false,
        ),
        (
            JobError::Transition(JobTransitionError::RequiresVerifiedConsent),
            AppErrorCode::ConsentRequired,
            false,
        ),
        (
            JobError::Transition(JobTransitionError::RequiresCostAcknowledgement),
            AppErrorCode::CostAcknowledgementRequired,
            false,
        ),
        (
            JobError::Transition(JobTransitionError::RequiresUserAction {
                from: JobState::CloudOutcomeUnknown,
                to: JobState::CloudReady,
                actor: plaudon_jobs::state::Actor::Recovery,
            }),
            AppErrorCode::ConfirmationRequired,
            false,
        ),
        (
            JobError::JobNotFound(job),
            AppErrorCode::RecordNotFound,
            false,
        ),
        (
            JobError::CheckpointNotFound {
                job,
                stage: "decode".to_string(),
            },
            AppErrorCode::RecordNotFound,
            true,
        ),
        (
            JobError::CheckpointAlreadyCommitted {
                stage: "decode".to_string(),
            },
            AppErrorCode::Conflict,
            false,
        ),
        (
            JobError::CheckpointNotPrepared {
                stage: "decode".to_string(),
            },
            AppErrorCode::Conflict,
            false,
        ),
        (
            JobError::DigestMismatch {
                what: "media".to_string(),
                expected: "a".repeat(64),
                actual: "b".repeat(64),
            },
            AppErrorCode::IntegrityFailed,
            true,
        ),
        (
            JobError::MissingAsset("whisper-large".to_string()),
            AppErrorCode::ModelUnavailable,
            true,
        ),
        (
            JobError::UnknownResourceClass {
                class: "quantum".to_string(),
            },
            AppErrorCode::InvalidArgument,
            false,
        ),
        (
            JobError::InsufficientResources {
                resource: "disk",
                needed: 10,
                available: 1,
            },
            AppErrorCode::StorageFull,
            true,
        ),
        (JobError::Cancelled(job), AppErrorCode::Canceled, false),
        (
            JobError::DispatchRefused(DispatchRefusal::PrivacyRequirementUnmet),
            AppErrorCode::PolicyUnresolved,
            false,
        ),
        (
            JobError::WriterClosed,
            AppErrorCode::StorageUnavailable,
            true,
        ),
        (
            JobError::CorruptRecord("bad row".to_string()),
            AppErrorCode::IntegrityFailed,
            false,
        ),
        (
            JobError::Migration("boom".to_string()),
            AppErrorCode::MigrationFailed,
            false,
        ),
        (
            JobError::Journal("boom".to_string()),
            AppErrorCode::StorageUnavailable,
            true,
        ),
        (
            JobError::io(
                "write staging file",
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            ),
            AppErrorCode::StorageUnavailable,
            true,
        ),
        (
            JobError::encoding(
                "job config snapshot",
                serde_json::from_str::<serde_json::Value>("{").expect_err("invalid json"),
            ),
            AppErrorCode::InternalError,
            false,
        ),
        (
            JobError::storage("load job", sqlx::Error::RowNotFound),
            AppErrorCode::StorageUnavailable,
            true,
        ),
    ];

    for (error, code, retryable) in cases {
        assert_eq!(error.code(), code, "{error:?}");
        assert_eq!(error.is_retryable(), retryable, "{error:?}");
        assert_eq!(error.action(), code.action(), "{error:?}");
        let rendered = error.to_string();
        assert!(!rendered.is_empty(), "{error:?} must render");
        let app: plaudon_domain::error::AppError = error.into();
        assert_eq!(app.code(), code);
    }
}

#[test]
fn a_dispatch_refusal_always_asks_the_user_for_something() {
    for refusal in [
        DispatchRefusal::NoConsentGrant,
        DispatchRefusal::ConsentRevoked,
        DispatchRefusal::ConsentExpired,
        DispatchRefusal::DestinationChanged,
        DispatchRefusal::PrivacyRequirementUnmet,
        DispatchRefusal::OutcomeUnknown,
    ] {
        let error = JobError::DispatchRefused(refusal);
        assert!(!error.is_retryable(), "{refusal:?}");
        assert_ne!(refusal.to_string(), "");
        assert_eq!(error.action(), error.code().action(), "{refusal:?}");
    }
}

// ---------------------------------------------------------------------------
// Scheduler vocabulary (§14.4, §14.7)
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_persisted_scheduler_name_parses_to_nothing() {
    assert_eq!(ResourceClass::parse("quantum"), None);
    assert_eq!(ResourceClass::parse(""), None);
    assert_eq!(WaitReason::parse("bored"), None);
    assert_eq!(WaitReason::parse(""), None);
}

#[test]
fn only_local_inference_classes_count_as_heavy() {
    for class in ResourceClass::ALL.iter().copied() {
        let heavy = matches!(class, ResourceClass::Cpu | ResourceClass::Gpu);
        assert_eq!(class.is_heavy_local(), heavy, "{class:?}");
    }

    let limits = SchedulerLimits::default()
        .with(ResourceClass::Cpu, 2)
        .with(ResourceClass::Gpu, 3)
        .with(ResourceClass::Ble, 9);
    assert_eq!(limits.heavy_local_total(), 5);
    assert_eq!(limits.get(ResourceClass::Ble), 9);
}

#[tokio::test]
async fn an_admission_reports_its_class_and_releases_its_slot_on_drop() {
    let scheduler = ResourceScheduler::new(SchedulerLimits::default().with(ResourceClass::Ble, 1));

    let admission = scheduler.try_admit(ResourceClass::Ble).expect("slot");
    assert_eq!(admission.class(), ResourceClass::Ble);
    assert_eq!(scheduler.in_flight(ResourceClass::Ble), 1);
    assert_eq!(scheduler.high_water_mark(ResourceClass::Ble), 1);
    assert!(scheduler.try_admit(ResourceClass::Ble).is_none());

    drop(admission);
    assert_eq!(scheduler.in_flight(ResourceClass::Ble), 0);
    assert_eq!(
        scheduler.high_water_mark(ResourceClass::Ble),
        1,
        "the high-water mark is a record, not a gauge"
    );
    assert!(scheduler.try_admit(ResourceClass::Ble).is_some());
}

// ---------------------------------------------------------------------------
// Cancellation helpers (§13.3A, §14.7)
// ---------------------------------------------------------------------------

#[test]
fn only_app_owned_staging_files_are_eligible_for_discard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let staging = dir.path().join("temp/imports");
    std::fs::create_dir_all(&staging).expect("staging");

    assert!(is_app_owned_staging_file(
        &staging,
        &staging.join("job.part")
    ));
    assert!(
        is_app_owned_staging_file(&staging, &staging.join("job.PART")),
        "the extension check is case insensitive"
    );
    assert!(
        !is_app_owned_staging_file(&staging, &staging.join("job.wav")),
        "only `.part` files are app-owned intermediates"
    );
    assert!(
        !is_app_owned_staging_file(&staging, &staging.join("..").join("escape.part")),
        "a parent-dir component disqualifies a candidate"
    );
    assert!(
        !is_app_owned_staging_file(&staging, &dir.path().join("elsewhere.part")),
        "a file outside the staging root is never app-owned"
    );
    assert!(
        !is_app_owned_staging_file(&staging, &staging.join("missing-dir/job.part")),
        "a candidate whose directory does not exist cannot be judged app-owned"
    );
    assert!(
        !is_app_owned_staging_file(&dir.path().join("no-such-root"), &staging.join("job.part")),
        "an unresolvable staging root disqualifies everything"
    );
    assert!(
        !is_app_owned_staging_file(&staging, std::path::Path::new("/")),
        "a path with no parent is never app-owned"
    );
}

#[test]
fn discarding_reports_what_it_removed_and_surfaces_a_real_io_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let staging = dir.path().join("temp/imports");
    std::fs::create_dir_all(&staging).expect("staging");

    let removable = staging.join("a.part");
    std::fs::write(&removable, b"half").expect("write");
    let outside = dir.path().join("user.wav");
    std::fs::write(&outside, b"user data").expect("write");
    let absent = staging.join("gone.part");

    let removed =
        discard_incomplete_derivatives(&staging, &[removable.clone(), outside.clone(), absent])
            .expect("discard");

    assert_eq!(removed, vec![removable]);
    assert!(outside.exists(), "a user file is never discarded");

    // A `.part` entry that is a directory cannot be unlinked with `remove_file`;
    // the failure is surfaced rather than silently swallowed.
    let blocked = staging.join("b.part");
    std::fs::create_dir(&blocked).expect("dir");
    let error =
        discard_incomplete_derivatives(&staging, &[blocked]).expect_err("must surface the failure");
    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn the_registry_signals_every_live_job_and_forgets_finished_ones() {
    let registry = CancellationRegistry::new();
    assert!(registry.is_empty(), "a fresh registry holds no tokens");

    let a = JobId::new();
    let b = JobId::new();
    let token_a = registry.register(a);
    let token_b = registry.register(b);
    assert_eq!(registry.len(), 2);
    assert!(!registry.is_empty(), "two jobs are registered");
    assert!(!registry.is_cancelled(a));
    assert!(registry.get(JobId::new()).is_none());

    registry.cancel_all();
    assert!(token_a.is_cancelled());
    assert!(token_b.is_cancelled());
    assert!(registry.is_cancelled(b));

    registry.forget(a);
    assert_eq!(registry.len(), 1);
    assert!(!registry.is_cancelled(a), "a forgotten job has no token");
}

#[test]
fn an_unknown_consent_id_can_be_revoked_or_expired_without_creating_a_grant() {
    let gate = ConsentGate::new();
    gate.revoke("never-granted");
    gate.expire("never-granted");

    assert_eq!(
        gate.authorize("never-granted", 7, false),
        Err(DispatchRefusal::NoConsentGrant)
    );
    assert_eq!(gate.refused_count(), 1);
    assert_eq!(gate.authorized_count(), 0);

    gate.grant("granted", 7);
    assert_eq!(gate.authorize("granted", 7, false), Ok(()));
    assert_eq!(gate.authorized_count(), 1);
    assert_eq!(
        gate.authorize("granted", 7, true),
        Err(DispatchRefusal::OutcomeUnknown)
    );
    assert_eq!(
        gate.authorize("granted", 8, false),
        Err(DispatchRefusal::DestinationChanged)
    );
    gate.expire("granted");
    assert_eq!(
        gate.authorize("granted", 7, false),
        Err(DispatchRefusal::ConsentExpired)
    );
    gate.revoke("granted");
    assert_eq!(
        gate.authorize("granted", 7, false),
        Err(DispatchRefusal::ConsentRevoked),
        "revocation takes precedence over every other reason"
    );
    assert_eq!(gate.refused_count(), 5);
}

#[test]
fn a_cancellation_outcome_never_claims_a_refund() {
    let job = JobId::new();
    let local = CancellationOutcome::local(job, vec!["a.part".into()]);
    assert!(!local.possibly_billed);
    assert!(!local.refunded);
    assert_eq!(local.final_state, JobState::Cancelled);
    assert_eq!(local.discarded_derivatives.len(), 1);

    let cloud = CancellationOutcome::cloud_possibly_billed(job, Vec::new());
    assert!(cloud.possibly_billed);
    assert!(!cloud.refunded);
    assert_eq!(cloud.message, CLOUD_CANCELLATION_NOTICE);
    assert!(CONSENT_REVOKED_NOTICE.contains("cannot be recalled"));
}

#[test]
fn every_interruption_names_itself_and_declares_how_it_resumes() {
    let automatic = [
        InterruptionCause::OsSleep,
        InterruptionCause::BluetoothLost,
        InterruptionCause::SourceDisconnected,
        InterruptionCause::NetworkLost,
        InterruptionCause::ModelAssetRemoved,
        InterruptionCause::GpuBusy,
    ];
    let mut names = Vec::new();
    for cause in InterruptionCause::ALL.iter().copied() {
        assert_eq!(cause.resumes_automatically(), automatic.contains(&cause));
        assert!(!cause.wait_state().is_terminal());
        names.push(cause.as_str());
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), InterruptionCause::ALL.len());
}

// ---------------------------------------------------------------------------
// State machine edges reachable only from outside the crate
// ---------------------------------------------------------------------------

#[test]
fn a_terminal_state_has_no_successors_and_no_legal_edge_into_anything() {
    for state in JobState::ALL.iter().copied().filter(|s| s.is_terminal()) {
        assert!(successors(state).is_empty(), "{state}");
        for target in JobState::ALL {
            assert!(!is_legal_edge(state, *target), "{state} -> {target}");
        }
    }
    assert!(is_legal_edge(JobState::Queued, JobState::Preflight));
    assert!(!is_legal_edge(JobState::Queued, JobState::Completed));
    assert_eq!(JobState::parse("not_a_state"), None);
    assert_eq!(JobState::Queued.to_string(), "queued");
    assert!(JobState::FailedRetryable.is_retryable());
    assert!(!JobState::Queued.is_retryable());
    assert!(JobState::CloudOutcomeUnknown.requires_user_action());
    assert!(!JobState::RunningLocal.requires_user_action());
}
