//! Scanning of command **return values** for secret shapes (§6.3A, §15.4,
//! §16.2 "Signed URL leak").
//!
//! §6.3A: *"The UI will never receive raw bearer tokens, signed download URLs,
//! device secrets, filesystem-wide privileges or platform-bridge pointers."*
//! The typed command surface (`plaudon_app::command`) states that intent in its
//! DTO shapes; this module is the mechanical check over an actual serialized
//! payload, so a refactor that starts emitting a credential is caught by a test
//! or by the fuzz corpus replay rather than by a user reading a support bundle.
//!
//! # Where the check applies
//!
//! The egui shell renders DTOs in-process — there is no wire format between the
//! view and the application, so this is not a runtime gate on an IPC hop. It is
//! the single place the secret patterns live for everything that *is*
//! serialized: event and DTO snapshots, exported JSON, diagnostics bundles.
//!
//! # The pattern set
//!
//! [`secret_shape_of`] is the single place the patterns live. It is a shape
//! check over text and recognises:
//!
//! | Shape | Recognised form |
//! |---|---|
//! | [`SecretShape::BearerToken`] | `Bearer ` (any case) followed by ≥16 token characters |
//! | [`SecretShape::ApiKey`] | an `sk-`-prefixed key at a word boundary with ≥16 following characters — covers `OpenRouter` `sk-or-v1-…` |
//! | [`SecretShape::SignedUrl`] | an absolute URL whose query carries a signature/credential/expiry parameter |
//! | [`SecretShape::DeviceToken`] | a maximal hexadecimal run of exactly 32 characters, the PLAUD device-token shape |
//! | [`SecretShape::RawPointer`] | a `0x`-prefixed 8–16 digit hexadecimal address, i.e. a platform-bridge pointer |
//! | [`SecretShape::VoiceEmbedding`] | any value under an embedding-bearing field name, e.g. `embedding`, `voiceprint`, `embedding_cipher` |
//!
//! [`assert_no_secret_in_response`] walks a `serde_json::Value`, applies
//! [`secret_shape_of`] to every string, and additionally rejects a non-empty
//! string or numeric value stored under a secret-bearing key name
//! ([`SecretShape::SensitiveField`]), a pointer-shaped integer
//! ([`SecretShape::RawPointer`]) and any value — including an array of floats —
//! under an embedding-bearing key name ([`SecretShape::VoiceEmbedding`]).
//!
//! # Voice embeddings
//!
//! Speaker embeddings are biometric data and are never a permissible payload
//! for anything that leaves the process (VOICE_PROFILES_SPEC VP-002, VP-110,
//! ADR-0080). The control is that the vector type of
//! `plaudon_app_core::voice_profiles::matching` implements no `Serialize` at
//! all, so a vector cannot reach a DTO; this rule is the mechanical backstop
//! for anything that reintroduces one under a recognisable name. It is a
//! name-based rule on purpose: a bare array of floats is also a waveform, a
//! duration histogram or a set of scores, and refusing those would make the
//! scanner useless.
//!
//! # Honest scope
//!
//! This is a shape check, not a proof. It reliably catches the secret formats
//! this application handles, and it cannot detect an arbitrary re-encoding
//! (for example a base64 wrapper or a split-across-fields token). It is one
//! layer; not serializing secrets in the first place is the control.
//!
//! Two deliberate calibration choices:
//!
//! * a 64-character hex run is **accepted**, because §6.3B makes
//!   `sha256(bytes)` a legitimate part of a recording payload;
//! * consequently identifiers must be emitted as dashed UUIDs — an undashed
//!   32-character hex identifier is indistinguishable from a device token and
//!   is rejected.

use std::fmt;

use serde_json::Value;

use crate::error::{ErrorEnvelopeParts, SecurityError, COMMAND_NOT_AUTHORIZED};

/// Minimum number of token characters after a `Bearer ` / `sk-` prefix before
/// we treat the run as a credential rather than prose.
const MIN_TOKEN_BODY: usize = 16;

/// Length of a PLAUD device token when rendered as hexadecimal.
const DEVICE_TOKEN_HEX_LEN: usize = 32;

/// Query parameters that mark a URL as signed/capability-bearing.
const SIGNED_URL_MARKERS: &[&str] = &[
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "x-goog-signature",
    "signature=",
    "&sig=",
    "?sig=",
    "token=",
    "access_token=",
    "key-pair-id",
    "policy=",
];

/// Key names whose value must never be serialized to the UI.
const SENSITIVE_KEY_NAMES: &[&str] = &[
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
];

/// Key names whose value is a platform-bridge pointer.
const POINTER_KEY_NAMES: &[&str] = &["ptr", "pointer", "raw_handle", "raw_pointer", "bridge_ptr"];

/// Key names whose value is (or carries) a speaker embedding.
///
/// Matched exactly or as a `_`-suffixed form, like the other key rules, so
/// `embedding`, `voice_embedding`, `speaker_embedding`, `embedding_cipher` and
/// `exemplar_vector` are all covered.
const EMBEDDING_KEY_NAMES: &[&str] = &[
    "embedding",
    "embeddings",
    "embedding_cipher",
    "embedding_nonce",
    "voiceprint",
    "voiceprints",
    "speaker_vector",
    "exemplar_vector",
    "centroid_vector",
];

/// The kind of secret a payload appears to contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretShape {
    /// An HTTP `Authorization: Bearer …` value.
    BearerToken,
    /// A provider API key, including the `OpenRouter` `sk-or-v1-…` format.
    ApiKey,
    /// A URL carrying a signature, credential or capability parameter.
    SignedUrl,
    /// A 32-character hexadecimal device token.
    DeviceToken,
    /// A platform-bridge pointer or raw native handle.
    RawPointer,
    /// A non-empty value stored under a secret-bearing field name.
    SensitiveField,
    /// A value stored under a field name that carries a speaker embedding.
    VoiceEmbedding,
}

impl SecretShape {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SecretShape::BearerToken => "bearer_token",
            SecretShape::ApiKey => "api_key",
            SecretShape::SignedUrl => "signed_url",
            SecretShape::DeviceToken => "device_token",
            SecretShape::RawPointer => "raw_pointer",
            SecretShape::SensitiveField => "sensitive_field",
            SecretShape::VoiceEmbedding => "voice_embedding",
        }
    }
}

impl fmt::Display for SecretShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A command return value that would have leaked a secret to the UI.
///
/// The error carries the JSON pointer of the offending location and the shape
/// that matched — never the value itself, because §15.4 forbids credentials and
/// signed URLs inside error envelopes.
///
/// `envelope_parts()` maps this to [`COMMAND_NOT_AUTHORIZED`]: the user-visible
/// outcome is that the operation was refused. The precise internal reason stays
/// in the variant and in `Debug` for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("response field `{pointer}` matches the {shape} secret shape")]
pub struct ResponseLeakError {
    /// JSON pointer (RFC 6901 style) to the offending value, e.g. `/items/0/token`.
    pub pointer: String,
    /// Which pattern matched.
    pub shape: SecretShape,
}

impl SecurityError for ResponseLeakError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        ErrorEnvelopeParts::new(
            COMMAND_NOT_AUTHORIZED,
            "The application refused to return this result because it contained protected data."
                .to_owned(),
        )
        .retryable(false)
    }
}

/// The single pattern implementation: does `text` look like a secret?
///
/// See the [module documentation](self) for the recognised shapes and the
/// limits of a shape check.
#[must_use]
pub fn secret_shape_of(text: &str) -> Option<SecretShape> {
    let lowered = text.to_ascii_lowercase();

    if let Some(at) = lowered.find("bearer ") {
        let body = &text[at + "bearer ".len()..];
        if token_run_len(body) >= MIN_TOKEN_BODY {
            return Some(SecretShape::BearerToken);
        }
    }

    let bytes = lowered.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = lowered[search_from..].find("sk-") {
        let at = search_from + rel;
        let boundary_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        if boundary_ok && token_run_len(&lowered[at + 3..]) >= MIN_TOKEN_BODY {
            return Some(SecretShape::ApiKey);
        }
        search_from = at + 3;
    }

    if lowered.contains("://")
        && lowered.contains('?')
        && SIGNED_URL_MARKERS.iter().any(|m| lowered.contains(m))
    {
        return Some(SecretShape::SignedUrl);
    }

    if let Some(at) = lowered.find("0x") {
        let boundary_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let run = hex_run_len(&lowered[at + 2..]);
        if boundary_ok && (8..=16).contains(&run) {
            return Some(SecretShape::RawPointer);
        }
    }

    if has_hex_run_of(&lowered, DEVICE_TOKEN_HEX_LEN) {
        return Some(SecretShape::DeviceToken);
    }

    None
}

/// Reject a command return value that carries a secret shape (§6.3A).
///
/// Call this on a serialized command result — anything built from a DTO that
/// leaves the process as bytes.
pub fn assert_no_secret_in_response(value: &Value) -> Result<(), ResponseLeakError> {
    let mut pointer = String::new();
    walk(value, &mut pointer, None)
}

fn walk(value: &Value, pointer: &mut String, key: Option<&str>) -> Result<(), ResponseLeakError> {
    match value {
        Value::String(text) => {
            if let Some(shape) = secret_shape_of(text) {
                return Err(ResponseLeakError {
                    pointer: pointer.clone(),
                    shape,
                });
            }
            if !text.is_empty() && key.is_some_and(is_sensitive_key) {
                return Err(ResponseLeakError {
                    pointer: pointer.clone(),
                    shape: SecretShape::SensitiveField,
                });
            }
            Ok(())
        }
        Value::Number(_) => {
            if key.is_some_and(is_pointer_key) {
                return Err(ResponseLeakError {
                    pointer: pointer.clone(),
                    shape: SecretShape::RawPointer,
                });
            }
            if key.is_some_and(is_sensitive_key) {
                return Err(ResponseLeakError {
                    pointer: pointer.clone(),
                    shape: SecretShape::SensitiveField,
                });
            }
            Ok(())
        }
        Value::Array(items) => {
            let base = pointer.len();
            for (index, item) in items.iter().enumerate() {
                pointer.push('/');
                pointer.push_str(&index.to_string());
                walk(item, pointer, None)?;
                pointer.truncate(base);
            }
            Ok(())
        }
        Value::Object(map) => {
            let base = pointer.len();
            for (name, item) in map {
                if let Some(shape) = secret_shape_of(name) {
                    pointer.push('/');
                    pointer.push_str(name);
                    return Err(ResponseLeakError {
                        pointer: pointer.clone(),
                        shape,
                    });
                }
                if !item.is_null() && is_embedding_key(name) {
                    pointer.push('/');
                    pointer.push_str(name);
                    return Err(ResponseLeakError {
                        pointer: pointer.clone(),
                        shape: SecretShape::VoiceEmbedding,
                    });
                }
                pointer.push('/');
                pointer.push_str(name);
                walk(item, pointer, Some(name))?;
                pointer.truncate(base);
            }
            Ok(())
        }
        Value::Bool(_) | Value::Null => Ok(()),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    SENSITIVE_KEY_NAMES
        .iter()
        .any(|name| lowered == *name || lowered.ends_with(&format!("_{name}")))
}

/// Whether `key` names a field that carries a speaker embedding.
///
/// A `null` under such a key is accepted: the absence of an embedding is not
/// an embedding. Anything else — array, number, string, object — is refused,
/// because none of them is a shape an embedding may legitimately take on the
/// way out of this process.
fn is_embedding_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    EMBEDDING_KEY_NAMES
        .iter()
        .any(|name| lowered == *name || lowered.ends_with(&format!("_{name}")))
}

fn is_pointer_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    POINTER_KEY_NAMES
        .iter()
        .any(|name| lowered == *name || lowered.ends_with(&format!("_{name}")))
}

/// Length of the leading run of credential-token characters.
fn token_run_len(text: &str) -> usize {
    text.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='))
        .count()
}

fn hex_run_len(text: &str) -> usize {
    text.chars().take_while(char::is_ascii_hexdigit).count()
}

/// `true` when `lowered` contains a maximal hexadecimal run of exactly `len`
/// characters, delimited by non-alphanumeric characters on both sides.
fn has_hex_run_of(lowered: &str, len: usize) -> bool {
    let bytes = lowered.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        let run = &bytes[start..index];
        if run.len() == len && run.iter().all(u8::is_ascii_hexdigit) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    //! These moved with the scanner when the old WebView shell and its command
    //! guard were deleted: the shape check outlived the IPC boundary it was
    //! written for, so its tests now live next to the implementation.

    use serde_json::json;

    use super::{assert_no_secret_in_response, secret_shape_of, SecretShape};
    use crate::error::{SecurityError, COMMAND_NOT_AUTHORIZED};

    #[test]
    fn the_scanner_recognises_each_documented_secret_shape() {
        assert_eq!(
            secret_shape_of("Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature"),
            Some(SecretShape::BearerToken)
        );
        assert_eq!(
            secret_shape_of("sk-or-v1-9f2a7c4e1b6d8a30f5c7e9b1d3a5f7c9"),
            Some(SecretShape::ApiKey)
        );
        assert_eq!(
            secret_shape_of(
                "https://files.plaud.example/recording.wav?X-Amz-Signature=deadbeefcafe&X-Amz-Expires=900"
            ),
            Some(SecretShape::SignedUrl)
        );
        assert_eq!(
            secret_shape_of("9f2a7c4e1b6d8a30f5c7e9b1d3a5f7c9"),
            Some(SecretShape::DeviceToken)
        );
        assert_eq!(
            secret_shape_of("0x00007fb4c0a1e2f0"),
            Some(SecretShape::RawPointer)
        );
        assert_eq!(
            secret_shape_of("Quarterly review with the design team"),
            None
        );
    }

    #[test]
    fn a_response_carrying_a_bearer_token_is_rejected() {
        let payload =
            json!({ "device": { "auth": "Bearer eyJhbGciOiJIUzI1NiJ9.abcdefghijklmnop" } });
        let error = assert_no_secret_in_response(&payload).expect_err("leak");
        assert_eq!(error.shape, SecretShape::BearerToken);
        assert_eq!(error.pointer, "/device/auth");
        assert_eq!(error.envelope_parts().code, COMMAND_NOT_AUTHORIZED);
    }

    #[test]
    fn a_response_carrying_an_openrouter_key_is_rejected() {
        let payload =
            json!({ "settings": { "value": "sk-or-v1-0a1b2c3d4e5f60718293a4b5c6d7e8f90" } });
        let error = assert_no_secret_in_response(&payload).expect_err("leak");
        assert_eq!(error.shape, SecretShape::ApiKey);
    }

    #[test]
    fn a_response_carrying_a_signed_url_is_rejected() {
        let payload = json!({
            "items": [
                { "id": "a-1" },
                { "href": "https://cdn.plaud.example/a.wav?Expires=1700&Signature=abc123&Key-Pair-Id=K1" }
            ]
        });
        let error = assert_no_secret_in_response(&payload).expect_err("leak");
        assert_eq!(error.shape, SecretShape::SignedUrl);
        assert_eq!(error.pointer, "/items/1/href");
    }

    #[test]
    fn a_response_carrying_a_device_token_is_rejected() {
        let payload = json!({ "device": { "handshake": "a1b2c3d4e5f60718293a4b5c6d7e8f90" } });
        let error = assert_no_secret_in_response(&payload).expect_err("leak");
        assert_eq!(error.shape, SecretShape::DeviceToken);
    }

    #[test]
    fn a_response_carrying_a_secret_field_name_or_pointer_is_rejected() {
        let named = json!({ "vault": { "api_key": "short" } });
        assert_eq!(
            assert_no_secret_in_response(&named)
                .expect_err("leak")
                .shape,
            SecretShape::SensitiveField
        );
        let pointer = json!({ "bridge": { "raw_pointer": 140_697_320_000_u64 } });
        assert_eq!(
            assert_no_secret_in_response(&pointer)
                .expect_err("leak")
                .shape,
            SecretShape::RawPointer
        );
    }

    #[test]
    fn a_clean_response_passes_including_a_sha256_media_identity() {
        let payload = json!({
            "recording": {
                "id": "9f1c0f2e-5f3a-4a0d-9b9b-1f0a2b3c4d5e",
                "title": "Quarterly review",
                "media_sha256": "3b1f8c2d4e5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e",
                "duration_seconds": 3600,
                "vault_reference": "plaudon/device/9f1c0f2e",
                "has_token": false
            },
            "jobs": [{ "id": "job-1", "state": "completed" }]
        });
        assert!(assert_no_secret_in_response(&payload).is_ok());
    }

    #[test]
    fn a_response_carrying_a_speaker_embedding_is_rejected() {
        let vector = json!({
            "suggestion": {
                "person_id": "9f1c0f2e-5f3a-4a0d-9b9b-1f0a2b3c4d5e",
                "embedding": [0.012_3, -0.044_1, 0.301_7]
            }
        });
        let error = assert_no_secret_in_response(&vector).expect_err("leak");
        assert_eq!(error.shape, SecretShape::VoiceEmbedding);
        assert_eq!(error.pointer, "/suggestion/embedding");

        for key in [
            "voice_embedding",
            "speaker_embedding",
            "embedding_cipher",
            "embedding_nonce",
            "voiceprint",
            "exemplar_vector",
            "centroid_vector",
        ] {
            let payload = json!({ "profile": { key: "AAECAwQFBgcICQ==" } });
            assert_eq!(
                assert_no_secret_in_response(&payload)
                    .expect_err("leak")
                    .shape,
                SecretShape::VoiceEmbedding,
                "key {key} must be refused"
            );
        }

        let nested = json!({ "profile": { "embeddings": [{ "dim": 256 }] } });
        assert_eq!(
            assert_no_secret_in_response(&nested)
                .expect_err("leak")
                .shape,
            SecretShape::VoiceEmbedding
        );
    }

    #[test]
    fn a_voice_suggestion_payload_without_a_vector_passes() {
        // The shape `plaudon_app_core::voice_profiles::matching::MatchReport`
        // actually produces: scores, bands and person ids, no vector.
        let payload = json!({
            "cluster_key": "speaker_1",
            "evaluated": 3,
            "below_floor": 2,
            "recorded": [{
                "person_id": "9f1c0f2e-5f3a-4a0d-9b9b-1f0a2b3c4d5e",
                "score": 2.731_4,
                "band": "suggest",
                "aggregation": "centroid"
            }],
            "model": {
                "model_id": "wespeaker-resnet34-lm",
                "model_version": "1.0",
                "embedding": null
            }
        });
        assert!(assert_no_secret_in_response(&payload).is_ok());
    }
}
