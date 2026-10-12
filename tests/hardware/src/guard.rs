//! Keeps credentials and recording content out of check details (spec §16,
//! §6.3 invariant on secrets; App. B.9 "no credential and no recording
//! content").
//!
//! The guard is deliberately conservative: it refuses anything that looks like
//! a secret assignment, a bearer credential or a long opaque token, and
//! anything long enough to be a transcript excerpt. A refused detail is
//! replaced, never partially redacted, and the check is recorded as failed.

/// Longest detail the suite will record.
pub const MAX_DETAIL_CHARS: usize = 240;

/// Longest run of opaque token characters tolerated.
const MAX_OPAQUE_RUN: usize = 32;

/// Replacement recorded when a detail is refused.
pub const REFUSED_DETAIL: &str = "detail refused by the credential and content guard";

/// Why a detail was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DetailRejection {
    /// The text is longer than [`MAX_DETAIL_CHARS`].
    #[error("detail is longer than {MAX_DETAIL_CHARS} characters")]
    TooLong,
    /// The text assigns a value to a secret-like name.
    #[error("detail looks like a secret assignment")]
    SecretAssignment,
    /// The text contains a bearer or authorization credential.
    #[error("detail looks like a bearer credential")]
    BearerCredential,
    /// The text contains a long opaque run (token, key or digest of content).
    #[error("detail contains a long opaque token")]
    OpaqueToken,
}

const SECRET_NAMES: &[&str] = &[
    "token",
    "secret",
    "password",
    "passphrase",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "credential",
];

/// Accept a detail or say why not.
pub fn check_detail(detail: &str) -> Result<(), DetailRejection> {
    if detail.chars().count() > MAX_DETAIL_CHARS {
        return Err(DetailRejection::TooLong);
    }
    let lowered = detail.to_ascii_lowercase();
    if lowered.contains("bearer ")
        || (lowered.contains("basic ") && lowered.contains("authorization"))
    {
        return Err(DetailRejection::BearerCredential);
    }
    for name in SECRET_NAMES {
        let mut from = 0;
        while let Some(found) = lowered[from..].find(name) {
            let after = from + found + name.len();
            let rest = lowered[after..].trim_start();
            if rest.starts_with('=') || rest.starts_with(':') {
                return Err(DetailRejection::SecretAssignment);
            }
            from = after;
        }
    }
    let mut run = 0usize;
    for ch in detail.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '_' | '-' | '=') {
            run += 1;
            if run > MAX_OPAQUE_RUN {
                return Err(DetailRejection::OpaqueToken);
            }
        } else {
            run = 0;
        }
    }
    Ok(())
}
