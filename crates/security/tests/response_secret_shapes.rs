//! Secret-shape scanning of serialized command results: the near-miss and
//! negative side (§6.3A, §15.4, §16.2 "Signed URL leak").
//!
//! The inline tests in `src/response.rs` prove each documented shape is caught.
//! This suite proves the scanner stops where it says it stops — a short token
//! run, a `sk-` inside a word, a hex run of the wrong length, a pointer outside
//! the address-width window, an unsigned URL — because a scanner that fires on
//! prose would be turned off, and one that is never exercised on prose is not
//! known to be quiet.

use plaudon_security::error::SecurityError;
use plaudon_security::response::{assert_no_secret_in_response, secret_shape_of, SecretShape};
use serde_json::json;

/// Every shape has a distinct stable label and renders as that label.
#[test]
fn every_secret_shape_has_a_distinct_label() {
    let all = [
        SecretShape::BearerToken,
        SecretShape::ApiKey,
        SecretShape::SignedUrl,
        SecretShape::DeviceToken,
        SecretShape::RawPointer,
        SecretShape::SensitiveField,
        SecretShape::VoiceEmbedding,
    ];
    let mut labels = std::collections::BTreeSet::new();
    for shape in all {
        assert!(labels.insert(shape.label()), "duplicate {}", shape.label());
        assert_eq!(format!("{shape}"), shape.label());
        assert_eq!(
            serde_json::to_string(&shape).unwrap(),
            format!("\"{}\"", shape.label())
        );
    }
    assert_eq!(labels.len(), all.len());
}

/// The `Bearer ` rule needs a long enough token run; a short one is prose.
#[test]
fn a_bearer_prefix_with_too_short_a_body_is_not_a_credential() {
    // Exactly one character below the 16-character minimum body length.
    assert_eq!(secret_shape_of("Bearer abcdef012345678"), None);
    // Exactly at the minimum.
    assert_eq!(
        secret_shape_of("Bearer abcdef0123456789"),
        Some(SecretShape::BearerToken)
    );
    // Case-insensitive, and a trailing separator ends the run.
    assert_eq!(
        secret_shape_of("authorization: BEARER AAAABBBBCCCCDDDD!"),
        Some(SecretShape::BearerToken)
    );
    assert_eq!(secret_shape_of("Bearer of bad news"), None);
    assert_eq!(secret_shape_of("Bearer "), None);
}

/// The `sk-` rule only fires on a word boundary, and the scanner keeps looking
/// after a non-boundary hit rather than giving up on the first one.
#[test]
fn an_sk_dash_run_must_start_on_a_word_boundary() {
    // Preceded by an alphanumeric: part of a word, not a key.
    assert_eq!(secret_shape_of("task-0123456789abcdef0"), None);
    assert_eq!(secret_shape_of("risk-management-strategy-notes"), None);

    // A non-boundary occurrence first, then a real one: the scan must continue.
    assert_eq!(
        secret_shape_of("task-x and sk-or-v1-0123456789abcdef"),
        Some(SecretShape::ApiKey)
    );
    // Boundary at offset zero.
    assert_eq!(
        secret_shape_of("sk-0123456789abcdef"),
        Some(SecretShape::ApiKey)
    );
    // Boundary, but the body is one character too short.
    assert_eq!(secret_shape_of("key sk-0123456789abcde"), None);
    assert_eq!(secret_shape_of("sk-"), None);
}

/// A URL is only "signed" when it has a query *and* a capability marker.
#[test]
fn a_plain_url_is_not_a_signed_url() {
    assert_eq!(secret_shape_of("https://plaud.ai/support"), None);
    assert_eq!(secret_shape_of("https://plaud.ai/support?page=2"), None);
    assert_eq!(
        secret_shape_of("a signature= with no scheme and no query"),
        None
    );
    for marker in [
        "X-Amz-Signature=a",
        "X-Amz-Credential=a",
        "X-Amz-Security-Token=a",
        "X-Goog-Signature=a",
        "signature=a",
        "&sig=a",
        "?sig=a",
        "token=a",
        "access_token=a",
        "Key-Pair-Id=a",
        "policy=a",
    ] {
        let url = format!("https://cdn.plaud.example/a.wav?x=1&{marker}");
        assert_eq!(
            secret_shape_of(&url),
            Some(SecretShape::SignedUrl),
            "marker {marker} must be recognised"
        );
    }
}

/// The pointer window is 8..=16 hexadecimal digits after `0x`, on a word
/// boundary; both edges and both sides of the boundary rule are checked.
#[test]
fn the_raw_pointer_window_is_bounded_on_both_edges() {
    assert_eq!(secret_shape_of("0x0000000"), None, "7 digits is too short");
    assert_eq!(
        secret_shape_of("0x00000000"),
        Some(SecretShape::RawPointer),
        "8 digits is the lower edge"
    );
    assert_eq!(
        secret_shape_of("0x0123456789abcdef"),
        Some(SecretShape::RawPointer),
        "16 digits is the upper edge"
    );
    // 17 digits is not an address; it also is not a 32-char device token.
    assert_eq!(secret_shape_of("0x0123456789abcdef0"), None);
    // `0x` glued to the end of a word is not a pointer literal.
    assert_eq!(secret_shape_of("a0x00000000"), None);
    assert_eq!(secret_shape_of("0x"), None);
}

/// The device-token rule wants a *maximal* hexadecimal run of exactly 32
/// characters: 31, 33 and the 64-character media digest are all accepted.
#[test]
fn only_a_maximal_32_character_hex_run_is_a_device_token() {
    let thirty_two = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
    assert_eq!(thirty_two.len(), 32);
    assert_eq!(secret_shape_of(thirty_two), Some(SecretShape::DeviceToken));
    assert_eq!(
        secret_shape_of(&thirty_two[..31]),
        None,
        "31 is not a token"
    );
    assert_eq!(
        secret_shape_of(&format!("{thirty_two}a")),
        None,
        "33 is not a token"
    );
    assert_eq!(
        secret_shape_of(&format!("{thirty_two}{thirty_two}")),
        None,
        "a 64-character sha256 is a legitimate media identity (§6.3B)"
    );
    // Delimited on both sides, so a sentence containing one is still caught.
    assert_eq!(
        secret_shape_of(&format!("handshake {thirty_two} received")),
        Some(SecretShape::DeviceToken)
    );
    // A 32-character run that is not hexadecimal is just a word.
    assert_eq!(secret_shape_of("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"), None);
    // A dashed UUID has no 32-character run.
    assert_eq!(
        secret_shape_of("9f1c0f2e-5f3a-4a0d-9b9b-1f0a2b3c4d5e"),
        None
    );
}

/// Ordinary product text never trips the scanner.
#[test]
fn ordinary_response_text_is_not_a_secret() {
    for text in [
        "",
        "Quarterly review with the design team",
        "sk",
        "0",
        "bearer",
        "https://plaud.ai",
        "Meeting notes — 2024-06-01 — follow up on the sk invoice",
    ] {
        assert_eq!(secret_shape_of(text), None, "{text:?} must be quiet");
    }
}

/// A secret-bearing *key name* refuses a non-empty string and a number alike,
/// including the `_`-suffixed forms, while an empty string under the same key
/// is accepted because there is nothing to leak.
#[test]
fn a_sensitive_key_name_refuses_a_string_or_a_number_but_not_an_empty_value() {
    for key in [
        "token",
        "secret",
        "password",
        "passphrase",
        "api_key",
        "apikey",
        "access_key",
        "secret_key",
        "private_key",
        "credential",
        "credentials",
        "signed_url",
        "download_url",
        "authorization",
        "bearer",
        "session_key",
        "user_token",
        "VENDOR_API_KEY",
        "refresh_credential",
    ] {
        let as_text = json!({ "settings": { key: "x" } });
        let error = assert_no_secret_in_response(&as_text)
            .expect_err("a non-empty value under a secret-bearing key must be refused");
        assert_eq!(error.shape, SecretShape::SensitiveField, "{key}");
        assert_eq!(error.pointer, format!("/settings/{key}"));

        let as_number = json!({ "settings": { key: 1 } });
        assert_eq!(
            assert_no_secret_in_response(&as_number)
                .expect_err("numbers under a secret key leak too")
                .shape,
            SecretShape::SensitiveField,
            "{key}"
        );
    }

    // An empty string carries nothing, so it is not a leak.
    assert!(assert_no_secret_in_response(&json!({ "token": "" })).is_ok());
    // A boolean presence flag is the §15.2 "status only" shape.
    assert!(assert_no_secret_in_response(&json!({ "has_token": true })).is_ok());
    assert!(assert_no_secret_in_response(&json!({ "token": null })).is_ok());
}

/// A pointer-bearing key name refuses a number; the pointer rule is checked
/// before the generic sensitive-field rule so the reported shape is specific.
#[test]
fn a_pointer_key_name_refuses_a_numeric_handle() {
    for key in ["ptr", "pointer", "raw_handle", "raw_pointer", "bridge_ptr"] {
        let payload = json!({ "bridge": { key: 140_697_320_000_u64 } });
        let error = assert_no_secret_in_response(&payload).expect_err("handle must be refused");
        assert_eq!(error.shape, SecretShape::RawPointer, "{key}");
        assert_eq!(error.pointer, format!("/bridge/{key}"));
    }
    // A plain count is not a handle.
    assert!(assert_no_secret_in_response(&json!({ "bridge": { "count": 3 } })).is_ok());
}

/// A key *name* that is itself secret-shaped is refused before its value is
/// even walked, so a map keyed by credential cannot smuggle one out.
#[test]
fn a_secret_shaped_key_name_is_refused_before_its_value_is_walked() {
    let payload = json!({
        "sessions": { "Bearer eyJhbGciOiJIUzI1NiJ9.abcdefghijklmnop": true }
    });
    let error = assert_no_secret_in_response(&payload).expect_err("key name leak");
    assert_eq!(error.shape, SecretShape::BearerToken);
    assert!(error.pointer.starts_with("/sessions/Bearer "));

    let hex_key = json!({ "devices": { "a1b2c3d4e5f60718293a4b5c6d7e8f90": { "name": "A" } } });
    assert_eq!(
        assert_no_secret_in_response(&hex_key)
            .expect_err("hex key name leak")
            .shape,
        SecretShape::DeviceToken
    );
}

/// An embedding key refuses every shape except `null`, and the JSON pointer
/// names the offending location rather than the value.
#[test]
fn an_embedding_key_refuses_every_shape_except_null() {
    for value in [
        json!([0.1, 0.2]),
        json!("AAECAwQ="),
        json!(0.5),
        json!({ "dim": 256 }),
        json!([]),
        json!(false),
    ] {
        let payload = json!({ "profile": { "embedding": value } });
        let error = assert_no_secret_in_response(&payload).expect_err("embedding must be refused");
        assert_eq!(error.shape, SecretShape::VoiceEmbedding);
        assert_eq!(error.pointer, "/profile/embedding");
    }

    // The absence of an embedding is not an embedding.
    assert!(assert_no_secret_in_response(&json!({ "profile": { "embedding": null } })).is_ok());

    for key in [
        "embedding",
        "embeddings",
        "embedding_cipher",
        "embedding_nonce",
        "voiceprint",
        "voiceprints",
        "speaker_vector",
        "exemplar_vector",
        "centroid_vector",
        "voice_embedding",
        "cluster_centroid_vector",
    ] {
        let payload = json!({ "p": { key: [0.0] } });
        assert_eq!(
            assert_no_secret_in_response(&payload)
                .expect_err("must be refused")
                .shape,
            SecretShape::VoiceEmbedding,
            "{key}"
        );
    }

    // A similarly-named field that is not an embedding carrier passes.
    for key in ["embedded_at", "vector_count", "voice_name"] {
        let payload = json!({ "p": { key: 1 } });
        assert!(
            assert_no_secret_in_response(&payload).is_ok(),
            "{key} must not be treated as an embedding"
        );
    }
}

/// The pointer is an RFC 6901-style path through arrays and nested objects, and
/// the walk restores its prefix between siblings rather than accumulating it.
#[test]
fn the_reported_pointer_walks_arrays_and_nested_objects_precisely() {
    let payload = json!({
        "pages": [
            { "items": [{ "id": "a" }, { "id": "b" }] },
            { "items": [{ "id": "c" }, { "auth": "Bearer AAAABBBBCCCCDDDDEEEE" }] }
        ]
    });
    let error = assert_no_secret_in_response(&payload).expect_err("leak");
    assert_eq!(error.pointer, "/pages/1/items/1/auth");
    assert_eq!(error.shape, SecretShape::BearerToken);
    assert_eq!(
        error.to_string(),
        "response field `/pages/1/items/1/auth` matches the bearer_token secret shape"
    );

    // Top-level scalars and arrays walk without a key.
    assert!(assert_no_secret_in_response(&json!("hello")).is_ok());
    assert!(assert_no_secret_in_response(&json!(42)).is_ok());
    assert!(assert_no_secret_in_response(&json!(null)).is_ok());
    assert!(assert_no_secret_in_response(&json!([1, "two", true, null])).is_ok());

    let in_array = json!(["ok", "sk-or-v1-0123456789abcdef"]);
    let error = assert_no_secret_in_response(&in_array).expect_err("leak");
    assert_eq!(error.pointer, "/1");
    assert_eq!(error.shape, SecretShape::ApiKey);
}

/// Whatever the shape, the user-visible outcome is one refusal code and the
/// value itself never reaches the envelope.
#[test]
fn every_leak_maps_to_one_refusal_code_and_never_echoes_the_value() {
    let payloads = [
        json!({ "a": "Bearer eyJhbGciOiJIUzI1NiJ9.abcdefghijklmnop" }),
        json!({ "a": "sk-or-v1-0123456789abcdef0" }),
        json!({ "a": "https://cdn.x.example/a?X-Amz-Signature=deadbeef" }),
        json!({ "a": "a1b2c3d4e5f60718293a4b5c6d7e8f90" }),
        json!({ "a": "0x00007fb4c0a1e2f0" }),
        json!({ "api_key": "shhh" }),
        json!({ "embedding": [1.0] }),
    ];
    for payload in payloads {
        let error = assert_no_secret_in_response(&payload).expect_err("leak");
        let parts = error.envelope_parts();
        assert_eq!(parts.code.as_str(), "COMMAND_NOT_AUTHORIZED");
        assert!(!parts.retryable);
        assert_eq!(parts.action, None);
        assert!(
            !parts.message.contains("sk-or-v1"),
            "envelope echoed the value: {}",
            parts.message
        );
        assert!(!parts.message.contains("deadbeef"));
        assert!(!parts.message.contains("shhh"));
    }
}

/// A realistic clean response — identifiers, digests, counts, timestamps — is
/// accepted end to end, which is what makes the refusals above meaningful.
#[test]
fn a_realistic_clean_response_passes_the_whole_walk() {
    let payload = json!({
        "recordings": [{
            "id": "9f1c0f2e-5f3a-4a0d-9b9b-1f0a2b3c4d5e",
            "title": "Quarterly review",
            "media_sha256": "3b1f8c2d4e5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e",
            "duration_seconds": 3600,
            "imported_at_unix": 1_700_000_000_i64,
            "has_transcript": true,
            "vault_reference": "plaudon/device/9f1c0f2e",
            "deleted_at": null
        }],
        "page": { "cursor": "cursor-0002", "total": 1 },
        "capabilities": { "local_only": true, "cloud_cli_launchable": false }
    });
    assert!(assert_no_secret_in_response(&payload).is_ok());
}
