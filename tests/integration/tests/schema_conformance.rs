//! Serialized domain payloads must satisfy the committed JSON Schemas in
//! `schemas/`, and the schemas must stay in step with the Rust types
//! (spec 6.6, 15.4, 18.2).

use std::path::{Path, PathBuf};

use plaudon_domain::error::{
    AppError, AppErrorCode, DiagnosticLog, ErrorAction, Redactor, SafeHint,
};
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};

fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).expect("root").join("schemas")
}

fn load(name: &str) -> serde_json::Value {
    let path = schemas_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

fn validator(name: &str) -> jsonschema::Validator {
    let schema = load(name);
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("schema compiles")
}

fn assert_valid(validator: &jsonschema::Validator, instance: &serde_json::Value) {
    let errors: Vec<String> = validator.iter_errors(instance).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "instance failed schema: {errors:?}\n{instance:#}");
}

#[test]
fn the_envelope_schema_lists_exactly_the_rust_code_set() {
    let schema = load("error-envelope.v1.schema.json");
    let listed: Vec<&str> = schema["properties"]["code"]["enum"]
        .as_array()
        .expect("code enum")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    let actual: Vec<&str> = AppErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(listed, actual, "schemas/error-envelope.v1.schema.json drifted from AppErrorCode");
}

#[test]
fn the_envelope_schema_lists_exactly_the_rust_action_set() {
    let schema = load("error-envelope.v1.schema.json");
    let listed: Vec<String> = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum")
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect();
    let actual: Vec<String> = AppErrorCode::ALL
        .iter()
        .filter_map(|code| code.action())
        .map(|action| {
            serde_json::to_value(action).expect("action serializes").as_str().expect("string").to_owned()
        })
        .collect();
    for action in &actual {
        assert!(listed.contains(action), "schema is missing action {action}");
    }
    for action in &listed {
        let parsed: Result<ErrorAction, _> = serde_json::from_value(serde_json::json!(action));
        assert!(parsed.is_ok(), "schema lists unknown action {action}");
    }
}

#[test]
fn every_error_code_produces_a_schema_valid_envelope() {
    let validator = validator("error-envelope.v1.schema.json");
    let mut log = DiagnosticLog::new();
    for code in AppErrorCode::ALL {
        let error = AppError::new(*code, "internal detail with https://example.com/signed?sig=x")
            .with_hint(SafeHint::RetryAfterSeconds(5));
        let envelope = error.to_envelope(&mut log, Timestamp::EPOCH);
        let json = serde_json::to_value(&envelope).expect("envelope serializes");
        assert_valid(&validator, &json);
        assert!(!json.to_string().contains("https://"));
    }
    assert_eq!(log.len(), AppErrorCode::ALL.len());
}

#[test]
fn an_envelope_with_an_extra_field_is_rejected_by_the_schema() {
    let validator = validator("error-envelope.v1.schema.json");
    let mut log = DiagnosticLog::new();
    let envelope =
        AppError::new(AppErrorCode::Conflict, "x").to_envelope(&mut log, Timestamp::EPOCH);
    let mut json = serde_json::to_value(&envelope).expect("serializes");
    json["signed_url"] = serde_json::json!("https://example.com/x?sig=1");
    assert!(
        validator.iter_errors(&json).next().is_some(),
        "the envelope schema must forbid additional properties"
    );
}

fn revision(origin: AlignmentOrigin, segments: Vec<TranscriptSegment>) -> TranscriptRevision {
    TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: RecordingId::new(),
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"audio"),
        engine: EngineId::new("whisper-cpp"),
        model: ModelIdentity::new(EngineId::new("whisper-cpp"), "base.en"),
        locale: None,
        alignment_origin: origin,
        status: RevisionStatus::Complete,
        normalization_profile: "nfc-basic".into(),
        chunking_strategy: "single-pass".into(),
        duration_ms: Some(1_000),
        created_at: Timestamp::EPOCH,
        segments,
    })
    .expect("valid revision")
}

#[test]
fn a_timed_revision_satisfies_the_transcript_schema() {
    let validator = validator("transcript-revision.v1.schema.json");
    let value = serde_json::to_value(revision(
        AlignmentOrigin::Native,
        vec![
            TranscriptSegment::timed(0, 0, 500, "hello"),
            TranscriptSegment::timed(1, 500, 1_000, "world"),
        ],
    ))
    .expect("serializes");
    assert_valid(&validator, &value);
}

#[test]
fn a_text_only_revision_satisfies_the_transcript_schema_with_null_times() {
    let validator = validator("transcript-revision.v1.schema.json");
    let value = serde_json::to_value(revision(
        AlignmentOrigin::None,
        vec![TranscriptSegment::text_only(0, "hello")],
    ))
    .expect("serializes");
    assert_eq!(value["segments"][0]["start"], serde_json::Value::Null);
    assert_valid(&validator, &value);
}

#[test]
fn the_schema_rejects_a_text_only_revision_that_carries_times() {
    let validator = validator("transcript-revision.v1.schema.json");
    let mut value = serde_json::to_value(revision(
        AlignmentOrigin::None,
        vec![TranscriptSegment::text_only(0, "hello")],
    ))
    .expect("serializes");
    value["segments"][0]["start"] = serde_json::json!(0);
    value["segments"][0]["end"] = serde_json::json!(500);
    assert!(
        validator.iter_errors(&value).next().is_some(),
        "alignment_origin none must forbid segment times at the schema level too"
    );
}

#[test]
fn the_schema_rejects_an_unknown_alignment_origin() {
    let validator = validator("transcript-revision.v1.schema.json");
    let mut value = serde_json::to_value(revision(
        AlignmentOrigin::Native,
        vec![TranscriptSegment::timed(0, 0, 10, "x")],
    ))
    .expect("serializes");
    value["alignment_origin"] = serde_json::json!("psychic");
    assert!(validator.iter_errors(&value).next().is_some());
}

#[test]
fn the_redactor_is_applied_to_everything_the_diagnostics_log_stores() {
    let mut log = DiagnosticLog::new();
    let error = AppError::new(
        AppErrorCode::InternalError,
        "Authorization: Bearer sk-or-v1-abcdefghijklmnopqrstuvwxyz0123 https://host/x?sig=y",
    );
    let envelope = error.to_envelope(&mut log, Timestamp::EPOCH);
    let record = log.get(envelope.diagnostic_id).expect("entry");
    let stored = serde_json::to_string(record).expect("serializes");
    for forbidden in ["sk-or-v1", "https://", "sig=y"] {
        assert!(!stored.contains(forbidden), "diagnostics stored {forbidden}");
    }
    assert!(stored.contains(Redactor::PLACEHOLDER));
}
