//! Every command DTO is validated against its published JSON Schema.
//!
//! `schemas/commands/v1/*.schema.json` is the versioned wire contract. Each
//! schema sets `additionalProperties: false` and lists every field in
//! `required`, so adding, removing or renaming a field in Rust fails this test
//! until the schema is updated too.
//!
//! The DTOs are defined in `plaudon_app::dto`, so this suite moved here from
//! the Tauri shell crate with EPIC O task O2.5. The schemas remain a published
//! contract after the shell's deletion: they are what the DTOs are reviewed
//! against, and `schemas/events/ui-event.v1.schema.json` still describes the
//! event payloads the interface receives, so dropping this suite would have
//! left the wire contract unenforced.

use std::collections::BTreeMap;
use std::path::PathBuf;

use plaudon_app::dto::{
    ActivityEntry, ActivityPage, ActivityTarget, ActivityTargetKind, AnnotationKind, Appearance,
    CancelJobRequest, Capabilities, CloudBudget, CompleteOnboardingRequest, ConsentScopeDto,
    ConsentSummary, Contrast, CostConfidence, CostValue, DuplicatePolicy, EmptyRequest,
    EngineInventoryEntry, EngineStage, EventKind, ExportDefaults, ExportFormat,
    ExportRecordingRequest, ExportRecordingResponse, GetRecordingRequest, GetTranscriptRequest,
    GetUsageRequest, GetUsageResponse, GrantCloudConsentRequest, GrantCloudConsentResponse,
    HardwareInfo, ImportFilesRequest, ImportFilesResponse, ImportOutcome, ImportStatus, JobKind,
    JobPage, JobState, JobSummary, ListActivityRequest, ListJobsRequest, ListModelsRequest,
    ListModelsResponse, ListRecordingsRequest, LocalOnlyModeResponse, ModelCard, ModelFeatures,
    ModelOverride, ModelPreferenceScope, ModelPricing, ModelPrivacy, MoneyAmount,
    NotificationCategory, NotificationSettings, NotificationSettingsResponse, OsInfo,
    OsNotificationPermission, OutputKind, OutputLink, OutputLinkKind, PermissionState, Platform,
    Preferences, RecordingDetail, RecordingFilter, RecordingMediaInfo, RecordingOutput,
    RecordingPage, RecordingSort, RecordingSortField, RecordingSourceRef, RecordingSummary,
    RetryJobRequest, RevokeCloudConsentRequest, SaveTranscriptRevisionRequest,
    SaveTranscriptRevisionResponse, SegmentConfidence, SegmentEdit, SetCloudBudgetRequest,
    SetLocalOnlyModeRequest, SetModelPreferenceRequest, SetModelPreferenceResponse,
    SetNotificationSettingsRequest, SetSegmentSpeakerRequest, SetSegmentSpeakerResponse,
    SortDirection, SourceKind, StartProcessingRequest, StartProcessingResponse, StatusResponse,
    StorageCapability, TranscriptAnnotation, TranscriptPage, TranscriptRevisionRef,
    TranscriptSegmentDto, TranscriptionStatus, TriState, UiEvent, UsageAttempt, UsageByModel,
    UsageTotals, VerifyModelRequest, VerifyModelResponse,
};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorEnvelope};
use plaudon_domain::ids::{JobId, OutputId, RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use serde_json::{json, Value};

fn schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
}

fn validate(schema_name: &str, instance: &Value) {
    let path = schema_dir()
        .join("commands/v1")
        .join(format!("{schema_name}.schema.json"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    let schema: Value = serde_json::from_str(&raw).expect("the schema is valid json");
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect();
    assert!(
        errors.is_empty(),
        "{schema_name} does not match its schema:\n{}\ninstance: {}",
        errors.join("\n"),
        serde_json::to_string_pretty(instance).unwrap_or_default()
    );
}

fn envelope() -> ErrorEnvelope {
    AppError::new(AppErrorCode::StorageFull, "internal detail")
        .to_envelope(&mut DiagnosticLog::new(), Timestamp::EPOCH)
}

fn notification_settings() -> NotificationSettings {
    let mut categories = BTreeMap::new();
    for category in NotificationCategory::ALL {
        categories.insert(*category, true);
    }
    NotificationSettings {
        enabled: true,
        categories,
    }
}

fn preferences() -> Preferences {
    Preferences {
        onboarding_completed: true,
        local_only_mode: false,
        default_stt_engine_id: Some("whisper.cpp".into()),
        default_text_engine_id: None,
        appearance: Appearance::System,
        contrast: Contrast::Standard,
        notifications: notification_settings(),
        // Populated rather than `ExportDefaults::default()`, including the
        // optional directory, so the closed object is checked with every key
        // present and none of them left to the serde default.
        export_defaults: ExportDefaults {
            format: ExportFormat::Markdown,
            destination_directory: Some("/Users/example/Documents".into()),
            include_speaker_labels: true,
            include_timestamps: false,
        },
    }
}

fn summary() -> RecordingSummary {
    RecordingSummary {
        recording_id: RecordingId::new(),
        title: "Board meeting".into(),
        recorded_at: None,
        imported_at: "1970-01-01T00:00:00Z".into(),
        duration_ms: None,
        source: RecordingSourceRef {
            source_kind: SourceKind::ManualImport,
            label: "Local file".into(),
        },
        import_status: ImportStatus::Imported,
        transcription_status: TranscriptionStatus::Completed,
        processing_engine: None,
        last_error: Some(envelope()),
        tags: vec!["weekly".into()],
        byte_length: 1024,
        sha256: Sha256Digest::of(b"bytes"),
    }
}

fn revision_ref() -> TranscriptRevisionRef {
    TranscriptRevisionRef {
        revision_id: TranscriptRevisionId::new(),
        kind: plaudon_app::dto::RevisionKind::Machine,
        created_at: "1970-01-01T00:00:00Z".into(),
        engine_id: Some("whisper.cpp".into()),
        model_id: None,
        alignment_origin: AlignmentOrigin::Native,
        segment_count: 2,
        superseded: false,
    }
}

fn job_summary() -> JobSummary {
    JobSummary {
        job_id: JobId::new(),
        kind: JobKind::Import,
        state: JobState::Completed,
        stage: Some("committing".into()),
        progress: None,
        created_at: "1970-01-01T00:00:00Z".into(),
        updated_at: "1970-01-01T00:00:01Z".into(),
        started_at: None,
        finished_at: Some("1970-01-01T00:00:02Z".into()),
        recording_id: Some(RecordingId::new()),
        label: "meeting.wav".into(),
        error: Some(envelope()),
        cost_acknowledgement_required: false,
        provider_request_id: None,
        output_link: Some(OutputLink {
            kind: OutputLinkKind::Recording,
            id: RecordingId::new().to_string(),
        }),
        cost: CostValue::unknown(),
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("DTOs serialize")
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_request_dto_matches_its_schema() {
    validate("get_capabilities.request", &to_value(&EmptyRequest {}));
    validate("remove_openrouter_key.request", &to_value(&EmptyRequest {}));

    validate(
        "list_recordings.request",
        &to_value(&ListRecordingsRequest {
            cursor: None,
            limit: 50,
            filter: RecordingFilter {
                query: Some("board".into()),
                import_status: vec![ImportStatus::Imported],
                transcription_status: vec![TranscriptionStatus::None],
                sources: vec!["manual_import".into()],
                tags: vec!["weekly".into()],
                date_from: None,
                date_to: None,
            },
            sort: RecordingSort {
                field: RecordingSortField::ImportedAt,
                direction: SortDirection::Desc,
            },
        }),
    );
    validate(
        "import_files.request",
        &to_value(&ImportFilesRequest {
            paths: vec!["/tmp/a.wav".into()],
            duplicate_policy: DuplicatePolicy::Ask,
        }),
    );
    validate(
        "get_recording.request",
        &to_value(&GetRecordingRequest {
            recording_id: RecordingId::new().to_string(),
        }),
    );
    validate(
        "get_transcript.request",
        &to_value(&GetTranscriptRequest {
            revision_id: TranscriptRevisionId::new().to_string(),
            cursor: Some(0),
            limit: 100,
        }),
    );
    validate(
        "save_transcript_revision.request",
        &to_value(&SaveTranscriptRevisionRequest {
            recording_id: RecordingId::new().to_string(),
            base_revision_id: TranscriptRevisionId::new().to_string(),
            edits: vec![SegmentEdit {
                index: 0,
                text: "fixed".into(),
            }],
        }),
    );
    validate(
        "list_jobs.request",
        &to_value(&ListJobsRequest {
            cursor: None,
            limit: 25,
            states: vec![JobState::Queued],
            kinds: vec![JobKind::Import],
        }),
    );
    validate(
        "cancel_job.request",
        &to_value(&CancelJobRequest {
            job_id: JobId::new().to_string(),
        }),
    );
    validate(
        "retry_job.request",
        &to_value(&RetryJobRequest {
            job_id: JobId::new().to_string(),
            cost_acknowledged: true,
        }),
    );
    validate(
        "export_recording.request",
        &to_value(&ExportRecordingRequest {
            recording_id: RecordingId::new().to_string(),
            revision_id: TranscriptRevisionId::new().to_string(),
            format: ExportFormat::Markdown,
            destination: "/tmp/export.md".into(),
        }),
    );
    validate(
        "set_notification_settings.request",
        &to_value(&SetNotificationSettingsRequest {
            settings: notification_settings(),
        }),
    );
    validate(
        "set_local_only_mode.request",
        &to_value(&SetLocalOnlyModeRequest {
            enabled: true,
            confirmed: true,
        }),
    );
    validate(
        "list_activity.request",
        &to_value(&ListActivityRequest {
            cursor: None,
            limit: 20,
        }),
    );
    validate(
        "complete_onboarding.request",
        &to_value(&CompleteOnboardingRequest {
            default_stt_engine_id: None,
            default_text_engine_id: None,
            local_only_mode: true,
            notifications: notification_settings(),
        }),
    );

    // The key request deliberately has no `Serialize`: a secret must not be
    // serializable at all. The wire shape is asserted by deserializing the
    // schema-valid instance into it.
    let key_request = json!({ "key": "sk-or-v1-0123456789abcdef" });
    validate("set_openrouter_key.request", &key_request);
    let parsed: plaudon_app::dto::SetOpenRouterKeyRequest =
        serde_json::from_value(key_request).expect("the request deserializes");
    assert_ne!(parsed.key, "");
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_response_dto_matches_its_schema() {
    let capabilities = Capabilities {
        os: OsInfo {
            platform: Platform::Macos,
            version: "15.0".into(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".into(),
            logical_cores: 10,
            total_memory_bytes: 17_179_869_184,
        },
        engines: vec![EngineInventoryEntry {
            engine_id: "whisper.cpp".into(),
            display_name: "whisper.cpp (local)".into(),
            stage: EngineStage::Stt,
            status: CapabilityStatus::Unknown,
            unavailable_reason: Some("not validated here".into()),
            installed_models: Vec::new(),
            download_bytes_required: None,
            requires_network: false,
        }],
        storage: StorageCapability {
            library_path: "/tmp/library".into(),
            free_bytes: 1,
            total_bytes: 2,
        },
        integrations: BTreeMap::from([("openrouter".to_owned(), CapabilityStatus::Unknown)]),
        permissions: BTreeMap::from([("notifications".to_owned(), PermissionState::NotRequested)]),
        preferences: preferences(),
        openrouter_key_present: false,
        cloud_consents: vec![consent_summary()],
    };
    validate("get_capabilities.response", &to_value(&capabilities));

    validate(
        "list_recordings.response",
        &to_value(&RecordingPage {
            items: vec![summary()],
            next_cursor: None,
            total: 1,
        }),
    );

    validate(
        "get_recording.response",
        &to_value(&RecordingDetail {
            summary: summary(),
            media: RecordingMediaInfo {
                observed_codec: "unknown".into(),
                sample_rate_hz: None,
                channels: None,
                original_file_name: "meeting.wav".into(),
            },
            revisions: vec![revision_ref()],
            current_revision_id: Some(TranscriptRevisionId::new()),
            outputs: vec![RecordingOutput {
                output_id: OutputId::new(),
                kind: OutputKind::Summary,
                source_revision_id: TranscriptRevisionId::new(),
                created_at: "1970-01-01T00:00:00Z".into(),
                stale: false,
            }],
        }),
    );

    validate(
        "get_transcript.response",
        &to_value(&TranscriptPage {
            revision: revision_ref(),
            segments: vec![TranscriptSegmentDto {
                index: 0,
                text: "hello".into(),
                start_ms: Some(0),
                end_ms: Some(1000),
                alignment_origin: AlignmentOrigin::Native,
                confidence: Some(SegmentConfidence {
                    engine_id: "whisper.cpp".into(),
                    value: -0.2,
                }),
                speaker_label: Some("Speaker 1".into()),
                speaker_id: Some(SpeakerId::new().to_string()),
                annotations: vec![TranscriptAnnotation {
                    kind: AnnotationKind::LowConfidence,
                    reason: "engine reported low confidence".into(),
                    segment_index: 0,
                    start_ms: Some(0),
                    end_ms: Some(1000),
                }],
            }],
            next_cursor: None,
            total_segments: 1,
        }),
    );

    let recording_id = RecordingId::new();
    let revision_id = TranscriptRevisionId::new();
    let speaker_id = SpeakerId::new();
    validate(
        "set_segment_speaker.request",
        &to_value(&SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: revision_id.to_string(),
            seq_index: 3,
            speaker_id: Some(speaker_id.to_string()),
        }),
    );
    // Clearing an attribution is a published shape too: `null` is how a
    // segment says nobody could be told apart.
    validate(
        "set_segment_speaker.request",
        &to_value(&SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: revision_id.to_string(),
            seq_index: 3,
            speaker_id: None,
        }),
    );
    validate(
        "set_segment_speaker.response",
        &to_value(&SetSegmentSpeakerResponse {
            recording_id,
            transcript_revision_id: revision_id,
            seq_index: 3,
            speaker_id: Some(speaker_id),
            speaker_label: Some("Speaker 1".into()),
        }),
    );

    validate(
        "save_transcript_revision.response",
        &to_value(&SaveTranscriptRevisionResponse {
            revision_id: TranscriptRevisionId::new(),
            stale_output_ids: vec![OutputId::new()],
        }),
    );

    validate(
        "list_jobs.response",
        &to_value(&JobPage {
            items: vec![job_summary()],
            next_cursor: Some("6f66663a3530".into()),
            state_counts: BTreeMap::from([(JobState::Completed, 1)]),
        }),
    );
    validate("cancel_job.response", &to_value(&job_summary()));
    validate("retry_job.response", &to_value(&job_summary()));

    validate(
        "import_files.response",
        &to_value(&ImportFilesResponse {
            results: vec![
                ImportOutcome::Accepted {
                    path: "/tmp/a.wav".into(),
                    job_id: JobId::new(),
                },
                ImportOutcome::Duplicate {
                    path: "/tmp/b.wav".into(),
                    sha256: Sha256Digest::of(b"b"),
                    existing_recording_id: RecordingId::new(),
                    existing_title: "b".into(),
                },
                ImportOutcome::Skipped {
                    path: "/tmp/c.wav".into(),
                    sha256: Sha256Digest::of(b"c"),
                    existing_recording_id: RecordingId::new(),
                },
                ImportOutcome::Rejected {
                    path: "/tmp/d.txt".into(),
                    error: envelope(),
                },
            ],
        }),
    );

    validate(
        "export_recording.response",
        &to_value(&ExportRecordingResponse {
            job_id: JobId::new(),
        }),
    );
    validate(
        "set_notification_settings.response",
        &to_value(&NotificationSettingsResponse {
            settings: notification_settings(),
            os_permission: OsNotificationPermission::NotRequested,
        }),
    );
    validate(
        "set_local_only_mode.response",
        &to_value(&LocalOnlyModeResponse {
            enabled: true,
            description: "enforced".into(),
            allowed_hosts: Vec::new(),
        }),
    );
    validate(
        "set_openrouter_key.response",
        &to_value(&StatusResponse::ok()),
    );
    validate(
        "remove_openrouter_key.response",
        &to_value(&StatusResponse::ok()),
    );
    validate(
        "list_activity.response",
        &to_value(&ActivityPage {
            items: vec![ActivityEntry {
                activity_id: uuid::Uuid::now_v7().to_string(),
                category: NotificationCategory::ImportFinished,
                occurred_at: "1970-01-01T00:00:00Z".into(),
                title: "Import finished".into(),
                body: "meeting was added to your library.".into(),
                notification_delivered: false,
                notification_suppressed_reason: Some("delivery_not_attempted".into()),
                target: Some(ActivityTarget {
                    kind: ActivityTargetKind::Recording,
                    id: RecordingId::new().to_string(),
                }),
                read: false,
            }],
            next_cursor: None,
            unread_count: 1,
        }),
    );
    validate("complete_onboarding.response", &to_value(&preferences()));
}

#[test]
fn the_ui_event_matches_its_schema() {
    let path = schema_dir().join("events/ui-event.v1.schema.json");
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the schema exists"))
            .expect("the schema is valid json");
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");

    for kind in EventKind::ALL {
        let event = UiEvent {
            seq: 1,
            kind: *kind,
            occurred_at: "1970-01-01T00:00:00Z".into(),
            job_id: Some(JobId::new()),
            recording_id: None,
            payload: serde_json::Map::new(),
        };
        let value = to_value(&event);
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|error| error.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "{} does not match: {}",
            kind.as_str(),
            errors.join("\n")
        );
    }
}

#[test]
fn a_renamed_or_added_field_fails_validation() {
    // The guard that makes this suite meaningful: the schemas are closed, so
    // drift in either direction is caught.
    let mut value = to_value(&StatusResponse::ok());
    value
        .as_object_mut()
        .expect("object")
        .insert("extra".into(), json!(1));
    let path = schema_dir().join("commands/v1/remove_openrouter_key.response.schema.json");
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the schema exists"))
            .expect("valid json");
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    assert!(
        validator.validate(&value).is_err(),
        "an added field must fail validation"
    );
}

/// Compile-time guard: the import of every DTO above keeps this list honest.
///
/// In the Tauri shell this iterated `commands::COMMAND_ALLOWLIST`, the list
/// handed to `generate_handler!`. That allowlist died with the shell; the
/// closed surface is now `command::COMMAND_NAMES`, which is generated from the
/// same table as the `AppCommand`/`AppResponse` enums and the dispatch arms.
/// It is a strict superset of the old allowlist (78 names against 54, each
/// listed once rather than with the six archive commands duplicated), so this
/// test now covers more ground than it did, not less.
#[test]
fn every_published_command_has_both_schemas() {
    for command in plaudon_app::command::COMMAND_NAMES {
        for suffix in ["request", "response"] {
            let path = schema_dir()
                .join("commands/v1")
                .join(format!("{command}.{suffix}.schema.json"));
            assert!(path.is_file(), "missing schema {}", path.display());
        }
    }
}

fn money(amount: &str) -> MoneyAmount {
    MoneyAmount {
        amount: amount.into(),
        currency: "USD".into(),
    }
}

fn consent_summary() -> ConsentSummary {
    ConsentSummary {
        consent_id: "consent-1".into(),
        purpose: "transcription".into(),
        content_type: "audio_bytes".into(),
        scope_kind: "recording".into(),
        recording_id: Some(RecordingId::new().to_string()),
        model_id: "openai/whisper-1".into(),
        downstream_provider: None,
        required_retention: Some("zero_retention".into()),
        disclosure_version: "2026-02-01".into(),
        granted_at: "1970-01-01T00:00:00Z".into(),
        expires_at: None,
        revoked_at: Some("1970-01-01T00:00:01Z".into()),
    }
}

fn model_card(verified: bool) -> ModelCard {
    ModelCard {
        model_id: "openai/whisper-1".into(),
        display_name: "Whisper".into(),
        engine: "openrouter".into(),
        request_family: if verified { "multipart_stt" } else { "unknown" }.into(),
        verification: if verified { "verified" } else { "not_verified" }.into(),
        verification_reason: (!verified).then(|| "no_observed_exchange".to_owned()),
        fixture_id: verified.then(|| "capability-check".to_owned()),
        validated_at: verified.then(|| "2026-10-11T00:00:00Z".to_owned()),
        features: ModelFeatures {
            segments: TriState::Yes,
            word_timestamps: TriState::Unknown,
            diarization: TriState::No,
            language_detection: TriState::Unknown,
        },
        locales: None,
        max_input_bytes: Some(25_000_000),
        max_duration_ms: None,
        pricing: ModelPricing {
            basis: "per_token".into(),
            unit_label: "USD per audio input token".into(),
            amount_per_unit: verified.then(|| money("0.000002000000")),
        },
        privacy: ModelPrivacy {
            relay: "openrouter".into(),
            downstream_provider: None,
            retention: "unknown".into(),
            retention_days: None,
            zero_retention_available: false,
        },
        selectable: verified,
    }
}

fn budget() -> CloudBudget {
    CloudBudget {
        per_job_limit: Some(money("1.000000000000")),
        monthly_limit: None,
        month_to_date: CostValue {
            confidence: CostConfidence::Estimated,
            amount: Some(money("0.250000000000")),
        },
        blocks_dispatch: true,
        enforcement_note: "device only".into(),
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_cloud_request_dto_matches_its_schema() {
    validate(
        "list_models.request",
        &to_value(&ListModelsRequest {
            stage: "transcribe".into(),
            engine: None,
        }),
    );
    validate(
        "get_usage.request",
        &to_value(&GetUsageRequest {
            from: "2026-10-01T00:00:00Z".into(),
            to: "2026-11-01T00:00:00Z".into(),
        }),
    );
    validate(
        "set_cloud_budget.request",
        &to_value(&SetCloudBudgetRequest {
            per_job_limit: Some("1.5".into()),
            monthly_limit: None,
            currency: "USD".into(),
        }),
    );
    for kind in ["ask_every_time", "recording", "model_policy"] {
        validate(
            "grant_cloud_consent.request",
            &to_value(&GrantCloudConsentRequest {
                purpose: "transcription".into(),
                content_type: "audio_bytes".into(),
                scope: ConsentScopeDto {
                    kind: kind.into(),
                    recording_id: (kind == "recording").then(|| RecordingId::new().to_string()),
                },
                model_id: "openai/whisper-1".into(),
                downstream_provider: Some("openai".into()),
                require_zero_retention: false,
                disclosure_version: "2026-02-01".into(),
            }),
        );
    }
    validate(
        "revoke_cloud_consent.request",
        &to_value(&RevokeCloudConsentRequest {
            consent_id: "consent-1".into(),
        }),
    );
    validate(
        "set_model_preference.request",
        &to_value(&SetModelPreferenceRequest {
            stage: "transcribe".into(),
            scope: ModelPreferenceScope {
                kind: "recording".into(),
                recording_id: Some(RecordingId::new().to_string()),
            },
            engine: "openrouter".into(),
            model_id: None,
        }),
    );
    validate(
        "start_processing.request",
        &to_value(&StartProcessingRequest {
            recording_id: RecordingId::new().to_string(),
            stage: "transcribe".into(),
            model_override: Some(ModelOverride {
                engine: "openrouter".into(),
                model_id: "openai/whisper-1".into(),
            }),
            cost_acknowledged: false,
            rerun: false,
        }),
    );
    validate(
        "verify_model.request",
        &to_value(&VerifyModelRequest {
            model_id: "openai/whisper-1".into(),
            task: "transcription".into(),
            cost_acknowledged: false,
        }),
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_cloud_response_dto_matches_its_schema() {
    for verified in [true, false] {
        validate(
            "list_models.response",
            &to_value(&ListModelsResponse {
                models: vec![model_card(verified)],
                catalog_source: "pinned_allowlist".into(),
                catalog_fetched_at: None,
                degraded_reason: Some("no_key".into()),
                default_model_id: Some("openai/whisper-1".into()),
            }),
        );
        validate(
            "set_model_preference.response",
            &to_value(&SetModelPreferenceResponse {
                stage: "transcribe".into(),
                scope_kind: "default".into(),
                recording_id: None,
                engine: "openrouter".into(),
                model_id: Some("openai/whisper-1".into()),
                effective_model: verified.then(|| model_card(true)),
            }),
        );
        validate(
            "verify_model.response",
            &to_value(&VerifyModelResponse {
                model: model_card(verified),
                cost: CostValue::unknown(),
                provider_request_id: Some("req-1".into()),
            }),
        );
    }
    validate(
        "get_usage.response",
        &to_value(&GetUsageResponse {
            from: "2026-10-01T00:00:00Z".into(),
            to: "2026-11-01T00:00:00Z".into(),
            totals: UsageTotals {
                finalized: Some(money("0.100000000000")),
                estimated: None,
                unknown_count: 2,
            },
            by_model: vec![UsageByModel {
                model_id: "openai/whisper-1".into(),
                attempts: 3,
                finalized: Some(money("0.100000000000")),
                estimated: None,
                unknown_count: 2,
            }],
            attempts: vec![UsageAttempt {
                attempt_id: "attempt-1".into(),
                job_id: JobId::new().to_string(),
                model_id: "openai/whisper-1".into(),
                request_family: "multipart_stt".into(),
                dispatch_state: "outcome_unknown".into(),
                cost: CostValue::unknown(),
                provider_request_id: None,
                created_at: "1970-01-01T00:00:00Z".into(),
                responded_at: None,
                possibly_billed: true,
            }],
            budget: budget(),
        }),
    );
    validate("set_cloud_budget.response", &to_value(&budget()));
    validate(
        "grant_cloud_consent.response",
        &to_value(&GrantCloudConsentResponse {
            consent_id: "consent-1".into(),
            scope_kind: "model_policy".into(),
            granted_at: "1970-01-01T00:00:00Z".into(),
            expires_at: None,
        }),
    );
    validate(
        "revoke_cloud_consent.response",
        &to_value(&StatusResponse::ok()),
    );
    for dispatch in ["queued", "consent_required", "blocked"] {
        validate(
            "start_processing.response",
            &to_value(&StartProcessingResponse {
                job_id: JobId::new().to_string(),
                estimated_cost: CostValue {
                    confidence: CostConfidence::Estimated,
                    amount: Some(money("0.010000000000")),
                },
                dispatch: dispatch.into(),
                blocked_reason: (dispatch != "queued").then(|| "reason".to_owned()),
            }),
        );
    }
    let mut job = job_summary();
    job.cost = CostValue {
        confidence: CostConfidence::Finalized,
        amount: Some(money("0.010000000000")),
    };
    validate("retry_job.response", &to_value(&job));
    validate("cancel_job.response", &to_value(&job));
    validate(
        "list_jobs.response",
        &to_value(&JobPage {
            items: vec![job],
            next_cursor: None,
            state_counts: BTreeMap::new(),
        }),
    );
}

/* -------------------------- voice profiles and the roster (P1.5, §8, §9A) */

fn person_dto() -> plaudon_app::dto::PersonDto {
    plaudon_app::dto::PersonDto {
        person_id: plaudon_domain::ids::PersonId::new(),
        display_name: "Priya Raman".into(),
        aliases: vec!["Priya".into()],
        email: Some("priya@example.test".into()),
        organisation: None,
        status: "active".into(),
        created_at: Timestamp::EPOCH,
        updated_at: Timestamp::EPOCH,
        recordings: 14,
        pending_suggestions: 1,
        voice: Some(plaudon_app::dto::PersonVoiceDto {
            status: "active".into(),
            exemplars: 2,
            source_recordings: 2,
            total_speech_ms: 24_000,
            retention_months: 12,
            consent_version: "voice-matching-v1".into(),
            enrolled_at: Timestamp::EPOCH,
            last_matched_at: None,
            expires_at: Some(Timestamp::EPOCH),
        }),
    }
}

fn speaker_dto() -> plaudon_app::dto::SpeakerDto {
    plaudon_app::dto::SpeakerDto {
        speaker_id: SpeakerId::new(),
        recording_id: RecordingId::new(),
        cluster_key: Some("S1".into()),
        person_id: Some(plaudon_domain::ids::PersonId::new()),
        label: "Priya Raman".into(),
        name: Some("Priya Raman".into()),
        confirmed: true,
        original_label: "Speaker 1".into(),
        renamed_at: None,
        confirmed_at: Some(Timestamp::EPOCH),
    }
}

#[test]
fn the_roster_commands_match_their_schemas() {
    use plaudon_app::dto::{
        AssignClusterPersonRequest, AssignClusterPersonResponse, CreatePersonRequest,
        DeletePersonRequest, DeletePersonResponse, ListPeopleRequest, ListPeopleResponse,
        MergeCandidateGroupDto, MergePeopleRequest, MergePeopleResponse, PersonRefDto,
        PersonResponse, UpdatePersonRequest,
    };

    let person = person_dto();
    let reference = PersonRefDto {
        person_id: person.person_id,
        display_name: person.display_name.clone(),
    };
    validate(
        "list_people.request",
        &to_value(&ListPeopleRequest {
            query: Some("pri".into()),
        }),
    );
    validate(
        "list_people.response",
        &to_value(&ListPeopleResponse {
            people: vec![person.clone()],
            merge_candidates: vec![MergeCandidateGroupDto {
                normalised_name: "priya raman".into(),
                people: vec![reference.clone(), reference.clone()],
            }],
        }),
    );
    validate(
        "create_person.request",
        &to_value(&CreatePersonRequest {
            display_name: "Priya".into(),
            aliases: Vec::new(),
            email: None,
            organisation: None,
        }),
    );
    validate(
        "create_person.response",
        &to_value(&PersonResponse {
            person: person.clone(),
        }),
    );
    validate(
        "update_person.request",
        &to_value(&UpdatePersonRequest {
            person_id: person.person_id.to_string(),
            display_name: "Priya Raman".into(),
            aliases: vec!["Priya".into()],
            email: None,
            organisation: Some("Acme".into()),
        }),
    );
    validate(
        "update_person.response",
        &to_value(&PersonResponse {
            person: person.clone(),
        }),
    );
    validate(
        "merge_people.request",
        &to_value(&MergePeopleRequest {
            loser_person_id: plaudon_domain::ids::PersonId::new().to_string(),
            survivor_person_id: person.person_id.to_string(),
            apply: false,
        }),
    );
    validate(
        "merge_people.response",
        &to_value(&MergePeopleResponse {
            applied: true,
            speaker_links: 3,
            recordings: 2,
            exemplars: 1,
            pending_suggestions: 0,
            enrollment_moved: true,
            survivor: Some(person.clone()),
        }),
    );
    validate(
        "delete_person.request",
        &to_value(&DeletePersonRequest {
            person_id: person.person_id.to_string(),
            apply: true,
        }),
    );
    validate(
        "delete_person.response",
        &to_value(&DeletePersonResponse {
            applied: true,
            severed_speaker_links: 3,
            recordings: 2,
            exemplars: 1,
            suggestions: 4,
        }),
    );
    validate(
        "assign_cluster_person.request",
        &to_value(&AssignClusterPersonRequest {
            recording_id: RecordingId::new().to_string(),
            speaker_id: SpeakerId::new().to_string(),
            person_id: Some(person.person_id.to_string()),
            overwrite_segment_exceptions: true,
        }),
    );
    validate(
        "assign_cluster_person.response",
        &to_value(&AssignClusterPersonResponse {
            applied: true,
            speaker: speaker_dto(),
            person: Some(reference),
            assigned_segments: vec![0, 1, 4],
            segment_exceptions: 3,
        }),
    );
}

#[test]
fn the_voice_profile_commands_match_their_schemas() {
    use plaudon_app::dto::{
        ActivationDisclosureDto, AddVoiceExemplarRequest, ClusterSpeechDto,
        ConfirmSpeakerSuggestionRequest, ConsentGateDto, DeleteVoiceEnrollmentRequest,
        DeleteVoiceEnrollmentResponse, EmptyRequest, EnrollPersonVoiceRequest, EnrolmentConsentDto,
        ForgetAllVoiceProfilesRequest, ForgetAllVoiceProfilesResponse, GetClusterSpeechRequest,
        ListSpeakerSuggestionsRequest, ListSpeakerSuggestionsResponse, ListVoiceExemplarsRequest,
        ListVoiceExemplarsResponse, PersonRefDto, RejectSpeakerSuggestionRequest,
        SetVoiceProfilesEnabledRequest, SpeakerMatchJobDto, SpeakerMatchRefusalDto,
        SpeakerSuggestionDecisionDto, SpeakerSuggestionDto, SplitVoiceEnrollmentRequest,
        SplitVoiceEnrollmentResponse, StartSpeakerMatchingRequest, StartSpeakerMatchingResponse,
        VoiceEnrollmentDto, VoiceExemplarDto, VoiceProfileSettingsDto,
    };

    let person = person_dto();
    let reference = PersonRefDto {
        person_id: person.person_id,
        display_name: person.display_name.clone(),
    };
    let settings = VoiceProfileSettingsDto {
        enabled: true,
        consent: ConsentGateDto {
            state: "active".into(),
            reason: None,
            accepted_version: Some("voice-matching-v1".into()),
            accepted_at: Some(Timestamp::EPOCH),
            required_version: "voice-matching-v1".into(),
        },
        disclosure: ActivationDisclosureDto {
            version: "voice-matching-v1".into(),
            title: "Turn on voice matching?".into(),
            body: vec!["A voiceprint is biometric data.".into()],
            two_tier_sentence: "Keeping a list of names needs no special consent.".into(),
            responsibility_sentence: "People you enrol must agree first.".into(),
            retention_default_months: 12,
        },
        enrolled_people: 2,
        exemplars: 5,
        pending_suggestions: 3,
        roster_people: 9,
        key_available: true,
        embedder_available: false,
        enrol_speech_ms: 10_000,
        exemplar_speech_ms: 3_000,
    };
    let enrolment = VoiceEnrollmentDto {
        person_id: person.person_id,
        exemplar_id: plaudon_domain::ids::VoiceExemplarId::new(),
        exemplars: 2,
        speech_ms: 12_000,
        total_speech_ms: 24_000,
        source_recordings: 2,
        model_id: "scripted-deterministic".into(),
        model_version: "1.0".into(),
        retention_months: 12,
        expires_at: Some(Timestamp::EPOCH),
    };
    let consent = EnrolmentConsentDto {
        disclosure_version: "voice-matching-v1".into(),
        person_attested: true,
        retention_months: Some(6),
        consent_note: Some("signed release".into()),
    };
    let decision = SpeakerSuggestionDecisionDto {
        suggestion_id: plaudon_domain::ids::SpeakerSuggestionId::new(),
        state: "confirmed".into(),
        applied: true,
        speaker: Some(speaker_dto()),
        person: Some(reference.clone()),
        assigned_segments: vec![2, 3],
        segment_exceptions: 0,
    };

    validate(
        "get_voice_profile_settings.request",
        &to_value(&EmptyRequest {}),
    );
    validate("get_voice_profile_settings.response", &to_value(&settings));
    validate(
        "set_voice_profiles_enabled.request",
        &to_value(&SetVoiceProfilesEnabledRequest {
            enabled: true,
            accept_disclosure_version: Some("voice-matching-v1".into()),
        }),
    );
    validate("set_voice_profiles_enabled.response", &to_value(&settings));
    validate(
        "enroll_person_voice.request",
        &to_value(&EnrollPersonVoiceRequest {
            person_id: person.person_id.to_string(),
            recording_id: RecordingId::new().to_string(),
            cluster_key: "S1".into(),
            consent: consent.clone(),
        }),
    );
    validate("enroll_person_voice.response", &to_value(&enrolment));
    validate(
        "add_voice_exemplar.request",
        &to_value(&AddVoiceExemplarRequest {
            person_id: person.person_id.to_string(),
            recording_id: RecordingId::new().to_string(),
            cluster_key: "S2".into(),
        }),
    );
    validate("add_voice_exemplar.response", &to_value(&enrolment));
    validate(
        "split_voice_enrollment.request",
        &to_value(&SplitVoiceEnrollmentRequest {
            person_id: person.person_id.to_string(),
            exemplar_ids: vec![plaudon_domain::ids::VoiceExemplarId::new().to_string()],
            target_person_id: None,
            new_person_display_name: Some("Sam".into()),
            consent,
        }),
    );
    validate(
        "split_voice_enrollment.response",
        &to_value(&SplitVoiceEnrollmentResponse {
            source_person_id: person.person_id,
            target_person_id: plaudon_domain::ids::PersonId::new(),
            exemplars_moved: 1,
            exemplars_remaining: 1,
            target: person.clone(),
        }),
    );
    validate(
        "list_voice_exemplars.request",
        &to_value(&ListVoiceExemplarsRequest {
            person_id: person.person_id.to_string(),
        }),
    );
    let exemplars = ListVoiceExemplarsResponse {
        person_id: person.person_id,
        exemplars: vec![
            VoiceExemplarDto {
                exemplar_id: plaudon_domain::ids::VoiceExemplarId::new(),
                recording_id: Some(RecordingId::new()),
                recording_title: Some("Monday stand-up".into()),
                cluster_key: Some("S1".into()),
                source_start_ms: Some(12_000),
                source_end_ms: Some(96_000),
                speech_ms: 24_000,
                model_id: "ecapa".into(),
                model_version: "1".into(),
                created_at: Timestamp::from_millis(1_770_000_000_000),
            },
            // A deleted recording nulls the provenance and keeps the
            // exemplar (VP-080), so the schema has to accept that shape too.
            VoiceExemplarDto {
                exemplar_id: plaudon_domain::ids::VoiceExemplarId::new(),
                recording_id: None,
                recording_title: None,
                cluster_key: None,
                source_start_ms: None,
                source_end_ms: None,
                speech_ms: 18_000,
                model_id: "ecapa".into(),
                model_version: "1".into(),
                created_at: Timestamp::from_millis(1_770_000_000_000),
            },
        ],
        total_speech_ms: 42_000,
        source_recordings: 1,
    };
    assert!(
        !to_value(&exemplars).to_string().contains("embedding"),
        "an exemplar listing must never carry a vector (VP-085)"
    );
    validate("list_voice_exemplars.response", &to_value(&exemplars));
    validate(
        "get_cluster_speech.request",
        &to_value(&GetClusterSpeechRequest {
            recording_id: RecordingId::new().to_string(),
            cluster_key: "S1".into(),
        }),
    );
    validate(
        "get_cluster_speech.response",
        &to_value(&ClusterSpeechDto {
            recording_id: RecordingId::new(),
            cluster_key: "S1".into(),
            clean_speech_ms: 12_000,
            spans: 3,
            first_span_start_ms: Some(12_000),
            last_span_end_ms: Some(96_000),
            enrol_speech_ms: 10_000,
            exemplar_speech_ms: 3_000,
            meets_enrol_minimum: true,
            meets_exemplar_minimum: true,
        }),
    );
    validate(
        "delete_voice_enrollment.request",
        &to_value(&DeleteVoiceEnrollmentRequest {
            person_id: person.person_id.to_string(),
            apply: true,
            reason: Some("withdrew consent".into()),
        }),
    );
    validate(
        "delete_voice_enrollment.response",
        &to_value(&DeleteVoiceEnrollmentResponse {
            applied: true,
            exemplars: 2,
            suggestions: 1,
            person_kept: true,
            person: person.clone(),
        }),
    );
    validate(
        "forget_all_voice_profiles.request",
        &to_value(&ForgetAllVoiceProfilesRequest { apply: false }),
    );
    validate(
        "forget_all_voice_profiles.response",
        &to_value(&ForgetAllVoiceProfilesResponse {
            applied: false,
            enrollments: 2,
            exemplars: 5,
            suggestions: 3,
            people_kept: 9,
            key_present: true,
            key_destroyed: false,
            feature_disabled: false,
            limits: vec!["Names are not voiceprints.".into()],
        }),
    );
    validate(
        "list_speaker_suggestions.request",
        &to_value(&ListSpeakerSuggestionsRequest {
            recording_id: None,
            limit: Some(50),
        }),
    );
    validate(
        "list_speaker_suggestions.response",
        &to_value(&ListSpeakerSuggestionsResponse {
            suggestions: vec![SpeakerSuggestionDto {
                suggestion_id: plaudon_domain::ids::SpeakerSuggestionId::new(),
                recording_id: RecordingId::new(),
                recording_title: Some("Stand-up".into()),
                diarization_run_id: uuid::Uuid::now_v7().to_string(),
                cluster_key: "S1".into(),
                speaker_id: Some(SpeakerId::new()),
                speaker_label: Some("Speaker 1".into()),
                person: reference,
                score: 0.82,
                score_kind: "cosine_similarity_asnorm".into(),
                band: "suggest".into(),
                state: "pending".into(),
                created_at: Timestamp::EPOCH,
            }],
            pending_count: 1,
        }),
    );
    validate(
        "confirm_speaker_suggestion.request",
        &to_value(&ConfirmSpeakerSuggestionRequest {
            suggestion_id: plaudon_domain::ids::SpeakerSuggestionId::new().to_string(),
            overwrite_segment_exceptions: false,
        }),
    );
    validate("confirm_speaker_suggestion.response", &to_value(&decision));
    validate(
        "reject_speaker_suggestion.request",
        &to_value(&RejectSpeakerSuggestionRequest {
            suggestion_id: plaudon_domain::ids::SpeakerSuggestionId::new().to_string(),
            corrected_person_id: Some(person.person_id.to_string()),
            overwrite_segment_exceptions: false,
        }),
    );
    validate("reject_speaker_suggestion.response", &to_value(&decision));
    validate(
        "start_speaker_matching.request",
        &to_value(&StartSpeakerMatchingRequest {
            recording_ids: vec![RecordingId::new().to_string()],
        }),
    );
    validate(
        "start_speaker_matching.response",
        &to_value(&StartSpeakerMatchingResponse {
            jobs: vec![SpeakerMatchJobDto {
                recording_id: RecordingId::new(),
                job_id: JobId::new(),
            }],
            refusals: vec![SpeakerMatchRefusalDto {
                recording_id: RecordingId::new(),
                reason: "This recording has no transcript yet.".into(),
            }],
        }),
    );
}
