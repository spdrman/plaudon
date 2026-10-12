//! Every source command DTO is validated against its published JSON Schema
//! (the EPIC K half of `schema_conformance.rs`).
//!
//! The schemas are closed (`additionalProperties: false`, every field
//! required), so adding, removing or renaming a Rust field fails here until
//! the schema is updated too.

use std::path::PathBuf;

use plaudon_desktop::dto::{
    FolderSyncCounts, JobState, ListSourceArtifactsRequest, ListSourceArtifactsResponse,
    PlaudCompatibility, PlaudConnectRequest, PlaudConnectorSnapshot, PlaudConnectorState,
    PlaudConnectorStatus, PlaudDiagnostics, PlaudDisconnectCopy, PlaudDisconnectResponse,
    PlaudDisconnectVariant, PlaudImportOutcome, PlaudImportRequest, PlaudImportResponse,
    PlaudItemSummary, PlaudListRequest, PlaudListResponse, PlaudListingCompleteness,
    PlaudNextAction, ReconcileDecisionSummary, SourceAddFolderRequest, SourceAddFolderResponse,
    SourceArtifactSummary, SourceReconcileRequest, SourceReconcileResponse, SourceSyncRequest,
    SourceSyncResponse, SyncTrigger, SyncedSourceKind, WatchedFolderRule,
};
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde_json::Value;

fn validate(schema_name: &str, instance: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../schemas/commands/v1")
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
        "{schema_name} does not match its schema:\n{}\ninstance: {instance:#}",
        errors.join("\n")
    );
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("serializes")
}

fn snapshot(next_action: PlaudNextAction) -> PlaudConnectorSnapshot {
    PlaudConnectorSnapshot {
        status: PlaudConnectorStatus {
            state: PlaudConnectorState::IncompatibleVersion,
            detected_version: Some("9.9.9".into()),
            version_validated: false,
            program_origin: Some("standard installation directory".into()),
            last_error: Some(AppErrorCode::CliUnsupported),
            listing_possibly_incomplete: true,
            scraping_warning: false,
            updated_at_ms: 1_760_000_000_000,
        },
        diagnostics: PlaudDiagnostics {
            state: PlaudConnectorState::IncompatibleVersion,
            headline: "headline".into(),
            detail: "detail".into(),
            next_action,
            detected_version: Some("9.9.9".into()),
            compatibility: PlaudCompatibility::Untested,
            program_origin: None,
            last_error: Some(AppErrorCode::CliUnsupported),
            warnings: vec!["warning".into()],
        },
        disconnect: PlaudDisconnectCopy {
            variant: PlaudDisconnectVariant::AssociationOnly,
            label: "Disconnect from this app".into(),
            body: "body".into(),
        },
    }
}

#[test]
fn the_connector_snapshot_matches_its_schemas_for_every_next_action() {
    let actions = [
        PlaudNextAction::None,
        PlaudNextAction::InstallCli,
        PlaudNextAction::InstallTestedVersion {
            usable_versions: vec!["1.4.2".into()],
        },
        PlaudNextAction::SignIn,
        PlaudNextAction::Retry,
        PlaudNextAction::WaitAndRetry,
        PlaudNextAction::ReviewLocalOnlyMode,
        PlaudNextAction::ImportManually,
        PlaudNextAction::CheckAccountAndRegion,
    ];
    for action in actions {
        let value = to_value(&snapshot(action));
        validate("plaud_probe.response", &value);
        validate("plaud_connect.response", &value);
    }
    validate("plaud_probe.request", &to_value(&serde_json::json!({})));
    validate(
        "plaud_disconnect.response",
        &to_value(&PlaudDisconnectResponse {
            disconnect: snapshot(PlaudNextAction::None).disconnect,
            snapshot: snapshot(PlaudNextAction::None),
        }),
    );
    validate(
        "plaud_disconnect.request",
        &to_value(&serde_json::json!({})),
    );
}

#[test]
fn the_connector_requests_and_listing_match_their_schemas() {
    validate(
        "plaud_connect.request",
        &to_value(&PlaudConnectRequest {
            user_intent: "connect_plaud_account".into(),
        }),
    );
    validate(
        "plaud_list.request",
        &to_value(&PlaudListRequest { cursor: None }),
    );
    validate(
        "plaud_list.request",
        &to_value(&PlaudListRequest {
            cursor: Some("eyJvZmZzZXQiOjJ9".into()),
        }),
    );
    validate(
        "plaud_list.response",
        &to_value(&PlaudListResponse {
            source_id: "0198a0a0-0000-7000-8000-000000000000".into(),
            items: vec![PlaudItemSummary {
                external_id: "rec_1".into(),
                display_name: "Board sync".into(),
                captured_at: Some("2026-10-11T00:00:00Z".into()),
                duration_ms: Some(1000),
                declared_byte_length: None,
                imported_recording_id: Some(RecordingId::new()),
            }],
            next_cursor: None,
            completeness: PlaudListingCompleteness::PossiblyIncomplete,
            empty_account_advisory: None,
        }),
    );
    validate(
        "plaud_import.request",
        &to_value(&PlaudImportRequest {
            external_ids: vec!["rec_1".into()],
        }),
    );
}

#[test]
fn every_import_outcome_matches_its_schema() {
    let mut log = DiagnosticLog::new();
    let error = AppError::new(AppErrorCode::IntegrityFailed, "detail")
        .to_envelope(&mut log, Timestamp::now());
    validate(
        "plaud_import.response",
        &to_value(&PlaudImportResponse {
            results: vec![
                PlaudImportOutcome::Imported {
                    external_id: "a".into(),
                    job_id: JobId::new(),
                    recording_id: RecordingId::new(),
                },
                PlaudImportOutcome::AlreadyImported {
                    external_id: "b".into(),
                    recording_id: RecordingId::new(),
                },
                PlaudImportOutcome::Failed {
                    external_id: "c".into(),
                    job_id: JobId::new(),
                    error,
                },
            ],
        }),
    );
}

#[test]
fn the_folder_and_artifact_dtos_match_their_schemas() {
    validate(
        "source_add_folder.request",
        &to_value(&SourceAddFolderRequest {
            path: "/Users/someone/Recordings".into(),
            recursive: true,
            rule: WatchedFolderRule::AskBeforeCloud,
        }),
    );
    validate(
        "source_add_folder.response",
        &to_value(&SourceAddFolderResponse {
            source_id: "0198a0a0-0000-7000-8000-000000000000".into(),
            rule: WatchedFolderRule::ImportOnly,
            recursive: false,
        }),
    );
    for trigger in [SyncTrigger::Manual, SyncTrigger::Background] {
        validate(
            "source_sync.request",
            &to_value(&SourceSyncRequest {
                source_id: "0198a0a0-0000-7000-8000-000000000000".into(),
                trigger,
            }),
        );
    }
    let counts = FolderSyncCounts {
        scanned: 3,
        inventory_complete: true,
        imported: 1,
        already_imported: 1,
        deferred: 1,
        refused_tombstoned: 0,
        rejected: 0,
        vanished: 0,
    };
    validate(
        "source_sync.response",
        &to_value(&SourceSyncResponse {
            job_id: JobId::new(),
            source_id: "s".into(),
            source_kind: SyncedSourceKind::WatchedFolder,
            state: JobState::Completed,
            folder: Some(counts.clone()),
            listed: None,
            imported_recording_ids: vec![RecordingId::new()],
        }),
    );
    validate(
        "source_sync.response",
        &to_value(&SourceSyncResponse {
            job_id: JobId::new(),
            source_id: "s".into(),
            source_kind: SyncedSourceKind::PlaudCloud,
            state: JobState::Completed,
            folder: None,
            listed: Some(3),
            imported_recording_ids: Vec::new(),
        }),
    );
    validate(
        "source_reconcile.request",
        &to_value(&SourceReconcileRequest {
            source_id: "s".into(),
        }),
    );
    validate(
        "source_reconcile.response",
        &to_value(&SourceReconcileResponse {
            job_id: JobId::new(),
            source_id: "s".into(),
            source_kind: SyncedSourceKind::PlaudCloud,
            decisions: vec![ReconcileDecisionSummary {
                external_id: Some("rec_1".into()),
                event: "new_external_id".into(),
                actions: vec!["queue_acquisition".into()],
            }],
            listing_complete: Some(true),
            folder: None,
        }),
    );
    validate(
        "source_reconcile.response",
        &to_value(&SourceReconcileResponse {
            job_id: JobId::new(),
            source_id: "s".into(),
            source_kind: SyncedSourceKind::WatchedFolder,
            decisions: Vec::new(),
            listing_complete: None,
            folder: Some(counts),
        }),
    );
    validate(
        "list_source_artifacts.request",
        &to_value(&ListSourceArtifactsRequest {
            recording_id: RecordingId::new().to_string(),
        }),
    );
    validate(
        "list_source_artifacts.response",
        &to_value(&ListSourceArtifactsResponse {
            artifacts: vec![SourceArtifactSummary {
                artifact_id: "a".into(),
                kind: "transcript".into(),
                source_vendor: "plaud".into(),
                content_sha256: "0".repeat(64),
                byte_length: 10,
                original_filename: None,
                imported_at: None,
                import_mode: "skip_vendor_content".into(),
                recorded_at: "2026-10-11T00:00:00Z".into(),
            }],
        }),
    );
}
