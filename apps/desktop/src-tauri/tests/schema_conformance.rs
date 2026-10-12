//! Every command DTO is validated against its published JSON Schema.
//!
//! `schemas/commands/v1/*.schema.json` is the versioned wire contract. Each
//! schema sets `additionalProperties: false` and lists every field in
//! `required`, so adding, removing or renaming a field in Rust fails this test
//! until the schema is updated too.

use std::collections::BTreeMap;
use std::path::PathBuf;

use plaudon_desktop::dto::{
    ActivityEntry, ActivityPage, ActivityTarget, ActivityTargetKind, AnnotationKind, Appearance,
    CancelJobRequest, Capabilities, CompleteOnboardingRequest, Contrast, DuplicatePolicy,
    EmptyRequest, EngineInventoryEntry, EngineStage, EventKind, ExportFormat,
    ExportRecordingRequest, ExportRecordingResponse, GetRecordingRequest, GetTranscriptRequest,
    HardwareInfo, ImportFilesRequest, ImportFilesResponse, ImportOutcome, ImportStatus, JobKind,
    JobPage, JobState, JobSummary, ListActivityRequest, ListJobsRequest, ListRecordingsRequest,
    LocalOnlyModeResponse, NotificationCategory, NotificationSettings,
    NotificationSettingsResponse, OsInfo, OsNotificationPermission, OutputKind, OutputLink,
    OutputLinkKind, PermissionState, Platform, Preferences, RecordingDetail, RecordingFilter,
    RecordingMediaInfo, RecordingOutput, RecordingPage, RecordingSort, RecordingSortField,
    RecordingSourceRef, RecordingSummary, RetryJobRequest, SaveTranscriptRevisionRequest,
    SaveTranscriptRevisionResponse, SegmentConfidence, SegmentEdit, SetLocalOnlyModeRequest,
    SetNotificationSettingsRequest, SortDirection, SourceKind, StatusResponse, StorageCapability,
    TranscriptAnnotation, TranscriptPage, TranscriptRevisionRef, TranscriptSegmentDto,
    TranscriptionStatus, UiEvent,
};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorEnvelope};
use plaudon_domain::ids::{JobId, OutputId, RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use serde_json::{json, Value};

fn schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../schemas")
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
        kind: plaudon_desktop::dto::RevisionKind::Machine,
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
    let parsed: plaudon_desktop::dto::SetOpenRouterKeyRequest =
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
#[test]
fn every_published_command_has_both_schemas() {
    for command in plaudon_desktop::commands::COMMAND_ALLOWLIST {
        for suffix in ["request", "response"] {
            let path = schema_dir()
                .join("commands/v1")
                .join(format!("{command}.{suffix}.schema.json"));
            assert!(path.is_file(), "missing schema {}", path.display());
        }
    }
}
