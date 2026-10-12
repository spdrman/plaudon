//! Contract tests replaying the captured A1.2 fixtures (spec 18.3).
//!
//! The suite is fixture-only. A real-provider run is opt-in through
//! `scripts/probe-openrouter.sh` and never happens in CI.

use std::path::{Path, PathBuf};

use plaudon_ai_contracts::capabilities::{MediaFormat, RequestFamily};
use plaudon_ai_openrouter::catalog::{
    CatalogEntry, ModelProbeRecord, ProbeEvidence, TimestampSupport,
};
use plaudon_ai_openrouter::error::{classify_http_status, OpenRouterError, Retriability};
use plaudon_ai_openrouter::request::MultipartSttRequest;
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::transcript::AlignmentOrigin;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .join("tests/fixtures/openrouter")
}

fn load(name: &str) -> serde_json::Value {
    let path = fixtures().join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn status_of(fixture: &serde_json::Value) -> u16 {
    u16::try_from(fixture["status"].as_u64().expect("status")).expect("status fits")
}

#[test]
fn the_captured_catalog_declares_audio_input_but_never_a_request_family() {
    let snapshot = load("catalog-snapshot.json");
    let entries: Vec<CatalogEntry> =
        serde_json::from_value(snapshot["data"].clone()).expect("catalog entries parse");

    assert!(
        entries.len() >= 40,
        "the snapshot should carry the full audio shortlist"
    );
    assert_eq!(
        usize::try_from(
            snapshot["_models_declaring_audio_input"]
                .as_u64()
                .expect("count")
        )
        .expect("fits"),
        entries.len()
    );

    for entry in &entries {
        assert!(
            entry.declares_audio_input(),
            "{} lost its audio modality",
            entry.id
        );
        assert_eq!(
            entry.implied_request_family(),
            RequestFamily::Unknown,
            "{} must not be promoted by the catalog alone",
            entry.id
        );
        let record = ModelProbeRecord::from_catalog(entry, "2026-10-11");
        assert!(
            !record.is_offerable(),
            "{} must not be offerable from catalog data",
            entry.id
        );
        assert_eq!(record.capability_status(), CapabilityStatus::Unknown);
    }
}

#[test]
fn no_catalog_entry_exposes_an_endpoint_family_or_a_retention_control() {
    // The finding that justifies `RequestFamily::Unknown` as the default: the
    // catalog has no field that could answer either question.
    let snapshot = load("catalog-snapshot.json");
    let raw = snapshot["data"].as_array().expect("array");
    for entry in raw {
        let object = entry.as_object().expect("object");
        for forbidden in [
            "endpoints",
            "supported_endpoints",
            "request_family",
            "transcription",
            "retention",
            "zdr",
            "data_policy",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "catalog unexpectedly exposes {forbidden}; the probe can be simplified"
            );
        }
    }

    let endpoints = load("model-endpoints-gpt-audio.json");
    let keys: Vec<&str> = endpoints["_observed_endpoint_keys"]
        .as_array()
        .expect("keys")
        .iter()
        .map(|k| k.as_str().expect("string"))
        .collect();
    assert!(
        keys.contains(&"provider_name"),
        "the endpoints resource does name the route"
    );
    for forbidden in ["supported_endpoints", "retention", "zdr", "data_policy"] {
        assert!(
            !keys.contains(&forbidden),
            "the endpoints resource unexpectedly exposes {forbidden}"
        );
    }
}

#[test]
fn a_successful_response_with_segments_yields_native_alignment() {
    let fixture = load("transcription-success-segments.json");
    assert_eq!(status_of(&fixture), 200);
    let segments = fixture["body"]["segments"].as_array().expect("segments");
    assert_ne!(segments.len(), 0);
    assert_eq!(
        TimestampSupport::ObservedSegments.alignment_origin(),
        AlignmentOrigin::Native
    );
}

#[test]
fn a_text_only_response_yields_no_alignment_and_no_invented_times() {
    let fixture = load("transcription-success-text-only.json");
    assert_eq!(status_of(&fixture), 200);
    assert!(fixture["body"].get("segments").is_none());
    assert_eq!(
        TimestampSupport::ObservedNone.alignment_origin(),
        AlignmentOrigin::None
    );
}

#[test]
fn every_error_fixture_classifies_into_the_expected_typed_failure() {
    let cases: [(&str, OpenRouterError); 6] = [
        ("error-401-unauthorized.json", OpenRouterError::Unauthorized),
        (
            "error-402-insufficient-credit.json",
            OpenRouterError::InsufficientCredit,
        ),
        (
            "error-404-unknown-model.json",
            OpenRouterError::UnknownModel,
        ),
        (
            "error-413-payload-too-large.json",
            OpenRouterError::PayloadTooLarge,
        ),
        (
            "error-429-rate-limited.json",
            OpenRouterError::RateLimited {
                retry_after_seconds: Some(30),
            },
        ),
        (
            "error-502-provider-unavailable.json",
            OpenRouterError::ProviderUnavailable,
        ),
    ];
    for (name, expected) in cases {
        let fixture = load(name);
        let retry_after = fixture
            .get("retry_after_seconds")
            .and_then(serde_json::Value::as_u64)
            .map(|v| u32::try_from(v).expect("fits"));
        assert_eq!(
            classify_http_status(status_of(&fixture), retry_after, true),
            expected,
            "{name}"
        );
    }
}

#[test]
fn a_truncated_response_is_an_unknown_outcome_and_never_auto_retries() {
    let fixture = load("truncated-response.json");
    assert_eq!(fixture["truncated"], serde_json::json!(true));
    let error = OpenRouterError::TruncatedResponse;
    assert_ne!(error.retriability(), Retriability::AutomaticRetry);
    assert!(!error.dispatch_state().auto_retry_safe());
}

#[test]
fn a_malformed_body_is_reported_rather_than_coerced() {
    let fixture = load("malformed-json.json");
    assert_eq!(status_of(&fixture), 200);
    assert!(fixture["body"].get("text").is_none());
    // A 200 with the wrong shape is the caller's cue to raise
    // MalformedResponse; it must never be read as an empty transcript.
    assert_eq!(
        OpenRouterError::MalformedResponse.code(),
        plaudon_domain::error::AppErrorCode::MalformedOutput
    );
}

#[test]
fn a_redirect_is_not_followed_blindly_into_a_different_host() {
    let fixture = load("redirect-308.json");
    let location = fixture["location"].as_str().expect("location");
    assert!(
        location.starts_with("https://openrouter.ai/"),
        "redirect target must stay on-host"
    );
    assert_eq!(status_of(&fixture), 308);
}

#[test]
fn no_fixture_contains_a_credential_or_a_request_body() {
    for entry in std::fs::read_dir(fixtures()).expect("fixtures dir") {
        let path = entry.expect("entry").path();
        let text = std::fs::read_to_string(&path).expect("read fixture");
        let lower = text.to_ascii_lowercase();
        for forbidden in [
            "authorization",
            "bearer",
            "sk-or-",
            "api_key",
            "x-api-key",
            "cookie",
        ] {
            assert!(
                !lower.contains(forbidden),
                "{} contains {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn an_unclassifiable_model_can_never_produce_a_request() {
    let wav = MediaFormat::new("wav", "pcm_s16le");
    let snapshot = load("catalog-snapshot.json");
    let entries: Vec<CatalogEntry> =
        serde_json::from_value(snapshot["data"].clone()).expect("entries");

    for entry in entries.iter().take(5) {
        let record = ModelProbeRecord::from_catalog(entry, "2026-10-11");
        assert_eq!(record.evidence, ProbeEvidence::CatalogOnly);
        let built = MultipartSttRequest::build(
            record.request_family,
            &record.model_id,
            &wav,
            std::slice::from_ref(&wav),
            None,
            vec![1, 2, 3],
            None,
        );
        assert!(built.is_err(), "{} must have no builder", record.model_id);
    }
}
