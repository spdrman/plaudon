//! The header, the schema and the Rust side must agree (task H1.1).
//!
//! The C ABI is a contract between three files that no compiler checks
//! against each other: `native/apple-bridge/include/plaudon_apple_bridge.h`,
//! `schemas/apple-bridge-envelope.v1.schema.json` and
//! `crates/ai-apple/src/envelope.rs`. Drift between them is silent and
//! expensive, so it is a test failure instead.
//!
//! Hermetic: it reads text files and never links or calls the library.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use plaudon_ai_apple::envelope::{
    BridgeErrorCode, BridgeOp, ENVELOPE_VERSION, EXPECTED_ABI_VERSION,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/ai-apple sits two levels below the workspace root")
        .to_path_buf()
}

fn header() -> String {
    std::fs::read_to_string(repo_root().join("native/apple-bridge/include/plaudon_apple_bridge.h"))
        .expect("the bridge header is committed")
}

fn schema() -> serde_json::Value {
    let text =
        std::fs::read_to_string(repo_root().join("schemas/apple-bridge-envelope.v1.schema.json"))
            .expect("the envelope schema is committed");
    serde_json::from_str(&text).expect("the envelope schema is valid JSON")
}

fn swift_sources() -> String {
    let dir = repo_root().join("native/apple-bridge/Sources/PlaudonAppleBridge");
    let mut combined = String::new();
    for entry in std::fs::read_dir(dir).expect("the Swift sources are committed") {
        let path = entry.expect("readable directory entry").path();
        if path.extension().is_some_and(|ext| ext == "swift") {
            combined.push_str(&std::fs::read_to_string(&path).expect("readable Swift source"));
            combined.push('\n');
        }
    }
    combined
}

/// Entry points the header must declare and the Swift side must export.
const ENTRY_POINTS: [&str; 12] = [
    "plaudon_apple_bridge_abi_version",
    "plaudon_apple_string_free",
    "plaudon_apple_probe_speech",
    "plaudon_apple_probe_foundation_model",
    "plaudon_apple_install_speech_assets",
    "plaudon_apple_transcribe_file",
    "plaudon_apple_generate_text",
    "plaudon_apple_new_cancel_token",
    "plaudon_apple_cancel",
    "plaudon_apple_release_cancel_token",
    "plaudon_apple_cancel_token_is_cancelled",
    "plaudon_apple_live_cancel_token_count",
];

#[test]
fn the_header_declares_exactly_the_entry_points_the_swift_side_exports() {
    let header = header();
    let swift = swift_sources();
    for symbol in ENTRY_POINTS {
        assert!(
            header.contains(symbol),
            "the header does not declare {symbol}"
        );
        assert!(
            swift.contains(&format!("@_cdecl(\"{symbol}\")")),
            "the Swift side does not export {symbol}"
        );
    }

    // No extra `@_cdecl` sneaks in: every exported symbol is one the header
    // documents and the Rust side knows about.
    let declared: BTreeSet<&str> = ENTRY_POINTS.into_iter().collect();
    let mut exported = BTreeSet::new();
    for (index, _) in swift.match_indices("@_cdecl(\"") {
        let rest = &swift[index + "@_cdecl(\"".len()..];
        let end = rest.find('"').expect("a closing quote");
        exported.insert(&rest[..end]);
    }
    assert_eq!(
        exported, declared,
        "the Swift exports and the documented entry points differ"
    );
}

#[test]
fn the_abi_and_envelope_versions_agree_across_the_three_files() {
    let header = header();
    assert!(
        header.contains(&format!(
            "#define PLAUDON_APPLE_BRIDGE_ABI_VERSION {EXPECTED_ABI_VERSION}"
        )),
        "the header's ABI version does not match the Rust constant"
    );
    assert!(
        header.contains(&format!(
            "#define PLAUDON_APPLE_BRIDGE_ENVELOPE_VERSION {ENVELOPE_VERSION}"
        )),
        "the header's envelope version does not match the Rust constant"
    );

    let swift = swift_sources();
    assert!(
        swift.contains(&format!(
            "let bridgeABIVersion: Int32 = {EXPECTED_ABI_VERSION}"
        )),
        "the Swift ABI version does not match the Rust constant"
    );
    assert!(
        swift.contains(&format!(
            "let bridgeEnvelopeVersion: Int = {ENVELOPE_VERSION}"
        )),
        "the Swift envelope version does not match the Rust constant"
    );

    let schema = schema();
    assert_eq!(
        schema["$defs"]["envelopeVersion"]["const"],
        serde_json::json!(ENVELOPE_VERSION)
    );
    assert_eq!(
        schema["$defs"]["abiVersion"]["const"],
        serde_json::json!(EXPECTED_ABI_VERSION)
    );
}

#[test]
fn the_error_vocabulary_is_identical_in_all_three_files() {
    let rust: BTreeSet<String> = BridgeErrorCode::KNOWN
        .iter()
        .map(|code| code.as_str().to_owned())
        .collect();

    let schema = schema();
    let json: BTreeSet<String> = schema["$defs"]["errorCode"]["enum"]
        .as_array()
        .expect("the schema lists the error codes")
        .iter()
        .map(|value| value.as_str().expect("codes are strings").to_owned())
        .collect();
    assert_eq!(rust, json, "the Rust and schema error vocabularies differ");

    // The header documents each code in its section 5 table, and the Swift
    // enum defines each one. Both are checked by substring because neither is
    // machine-readable, which is exactly why the check exists.
    let header = header();
    let swift = swift_sources();
    for code in &rust {
        assert!(
            header.contains(code.as_str()),
            "the header does not document the {code} error code"
        );
        assert!(
            swift.contains(&format!("= \"{code}\"")),
            "the Swift error enum does not define {code}"
        );
    }

    // And nothing the Swift side can emit is unknown to Rust.
    let mut swift_codes = BTreeSet::new();
    for (index, _) in swift.match_indices("    case ") {
        let rest = &swift[index..];
        if let Some(equals) = rest.find(" = \"") {
            if let Some(end) = rest[equals + 4..].find('"') {
                let value = &rest[equals + 4..equals + 4 + end];
                if rest[..equals].matches('\n').count() == 0 {
                    swift_codes.insert(value.to_owned());
                }
            }
        }
    }
    let unknown: Vec<&String> = swift_codes.difference(&rust).collect();
    assert!(
        unknown.is_empty(),
        "the Swift side can emit codes Rust does not know: {unknown:?}"
    );
}

#[test]
fn every_operation_is_named_in_the_header_and_the_schema() {
    let header = header();
    let schema = schema();
    let listed: BTreeSet<String> = schema["$defs"]["operation"]["enum"]
        .as_array()
        .expect("the schema lists the operations")
        .iter()
        .map(|value| value.as_str().expect("operations are strings").to_owned())
        .collect();
    let known: BTreeSet<String> = BridgeOp::ALL
        .iter()
        .map(|op| op.as_str().to_owned())
        .collect();
    assert_eq!(known, listed);
    for op in BridgeOp::ALL {
        assert!(
            header.contains(op.as_str()),
            "the header does not mention the {} operation",
            op.as_str()
        );
    }
}

#[test]
fn the_header_states_every_ownership_rule_the_rust_side_relies_on() {
    let header = header();
    for claim in [
        "plaudon_apple_string_free",
        "MUST NOT call `free`",
        "is borrowed for the duration of the",
        "No Swift or Objective-C object pointer ever crosses",
        "never NULL",
        "No progress callback is delivered after the call that registered it has",
        "No progress callback is delivered after `plaudon_apple_cancel` has",
        "Cancellation is cooperative and idempotent",
        "never returns a partial transcript",
        "Handles are never reused",
        "timeout_ms",
    ] {
        assert!(
            header.contains(claim),
            "the header no longer states: {claim}"
        );
    }
}

#[test]
fn the_normative_protocol_document_exists_and_points_at_the_header() {
    let document = std::fs::read_to_string(repo_root().join("docs/protocol/apple-bridge.md"))
        .expect("the bridge protocol document is committed");
    assert!(document.contains("plaudon_apple_bridge.h"));
    assert!(document.contains("apple-bridge-envelope.v1.schema.json"));
    for op in BridgeOp::ALL {
        assert!(
            document.contains(op.as_str()),
            "the protocol document does not describe {}",
            op.as_str()
        );
    }
}

#[test]
fn the_schema_accepts_a_real_response_and_rejects_an_invented_code() {
    let schema = schema();
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");

    let good = serde_json::json!({
        "envelope_version": ENVELOPE_VERSION,
        "abi_version": EXPECTED_ABI_VERSION,
        "op": "probe_foundation_model",
        "ok": false,
        "error": { "code": "disabled_by_user", "detail": null, "retriable": false },
    });
    assert!(validator.is_valid(&good), "a real refusal must validate");

    let invented = serde_json::json!({
        "envelope_version": ENVELOPE_VERSION,
        "abi_version": EXPECTED_ABI_VERSION,
        "op": "probe_foundation_model",
        "ok": false,
        "error": { "code": "vibes_unavailable", "detail": null, "retriable": false },
    });
    assert!(
        !validator.is_valid(&invented),
        "the schema must reject a code outside the closed vocabulary"
    );

    let request = serde_json::json!({
        "envelope_version": ENVELOPE_VERSION,
        "op": "transcribe_file",
        "path": "/tmp/a.wav",
        "cancel_token": 0,
        "timeout_ms": 0,
        "allow_asset_download": false,
        "want_word_timestamps": true,
    });
    assert!(validator.is_valid(&request), "a real request must validate");

    let progress = serde_json::json!({
        "envelope_version": ENVELOPE_VERSION,
        "op": "transcribe_file",
        "phase": "transcribe",
        "fraction": 0.5,
        "processed_ms": 750,
        "completed_units": null,
        "total_units": null,
    });
    assert!(
        validator.is_valid(&progress),
        "a real progress document must validate"
    );
}

#[test]
fn a_transcription_result_declaring_a_non_final_output_is_schema_invalid() {
    // The wire contract forbids it; a volatile result is the `output_not_final`
    // error instead. Encoding that in the schema means a future bridge change
    // cannot quietly start returning partial transcripts.
    let validator = jsonschema::validator_for(&schema()).expect("the schema compiles");
    let document = serde_json::json!({
        "envelope_version": ENVELOPE_VERSION,
        "abi_version": EXPECTED_ABI_VERSION,
        "op": "transcribe_file",
        "ok": true,
        "result": {
            "text": "partial",
            "segments": [],
            "alignment_origin": "none",
            "locale": "en_US",
            "final": false,
        },
    });
    assert!(!validator.is_valid(&document));
}
