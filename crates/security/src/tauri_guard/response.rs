//! Rust-side scanning of command **return values** (§6.3A, §15.4, §16.2
//! "Signed URL leak").
//!
//! §6.3A: *"The UI will never receive raw bearer tokens, signed download URLs,
//! device secrets, filesystem-wide privileges or platform-bridge pointers."*
//! The manifest records that intent (`returns_secret: false` for every command);
//! this module enforces it on the actual payload, so a refactor that starts
//! serializing a secret is caught at the boundary instead of in a user's
//! devtools pane.
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
//!
//! [`assert_no_secret_in_response`] walks a `serde_json::Value`, applies
//! [`secret_shape_of`] to every string, and additionally rejects a non-empty
//! string or numeric value stored under a secret-bearing key name
//! ([`SecretShape::SensitiveField`]) and a pointer-shaped integer
//! ([`SecretShape::RawPointer`]).
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
/// Call this on every serialized command result before it crosses the IPC
/// boundary.
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
