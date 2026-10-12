//! Stable error-envelope parts shared by every `crates/security` error type.
//!
//! Spec §15.4 fixes the wire shape of an error surfaced to the frontend:
//!
//! ```json
//! { "code": "AUTH_REQUIRED", "message": "...", "retryable": false,
//!   "action": "OPEN_PLAUD_CONNECT", "diagnostic_id": "a9f4e8..." }
//! ```
//!
//! `crates/security` deliberately does not own the application-wide error
//! taxonomy (that is `plaudon_domain::error::AppError`). It instead exposes the
//! *parts* of an envelope so the taxonomy crate can map security errors with a
//! `From` impl and no dependency cycle, and so this crate stays usable from
//! tests, fuzz targets and the audit binaries without pulling in the domain.
//!
//! Spec §15.4 also forbids signed URLs, credentials, raw request bodies and
//! transcript excerpts inside envelopes. [`ErrorEnvelopeParts::message`] is
//! therefore built from fixed vocabulary plus already-redacted labels only; see
//! [`crate::outbound::EgressDenied::destination_label`] for the pattern.

use std::fmt;

/// Machine-readable, stable error code. Values are SCREAMING_SNAKE_CASE and are
/// part of the public contract: never rename one, only add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(&'static str);

impl ErrorCode {
    /// Construct a code. Only `crates/security` constants should call this.
    pub(crate) const fn new(code: &'static str) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A UI affordance the frontend can offer in response to the error.
///
/// Mirrors the `action` field of §15.4. `None` means "no specific remedy".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorAction(&'static str);

impl ErrorAction {
    pub(crate) const fn new(action: &'static str) -> Self {
        Self(action)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ErrorAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The §15.4 envelope minus the `diagnostic_id`, which is allocated by the
/// diagnostics layer at the moment the error is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEnvelopeParts {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub action: Option<ErrorAction>,
}

impl ErrorEnvelopeParts {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            action: None,
        }
    }

    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    #[must_use]
    pub fn action(mut self, action: ErrorAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// Implemented by every public error type in this crate.
pub trait SecurityError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts;
}

// ---------------------------------------------------------------------------
// Code constants
// ---------------------------------------------------------------------------

/// Network initiation refused by the Rust outbound policy boundary (§16.2A).
pub const EGRESS_DENIED: ErrorCode = ErrorCode::new("EGRESS_DENIED");
/// The OS credential store is locked or unavailable; no plaintext fallback (§16.4).
pub const SECRET_STORE_LOCKED: ErrorCode = ErrorCode::new("SECRET_STORE_LOCKED");
/// The OS credential store reported a hard failure.
pub const SECRET_STORE_FAILED: ErrorCode = ErrorCode::new("SECRET_STORE_FAILED");
/// No secret is stored for the requested scope.
pub const SECRET_NOT_FOUND: ErrorCode = ErrorCode::new("SECRET_NOT_FOUND");
/// A cloud dispatch was attempted without a matching consent grant (§16.3).
pub const CONSENT_REQUIRED: ErrorCode = ErrorCode::new("CONSENT_REQUIRED");
/// A grant exists but does not cover the requested scope.
pub const CONSENT_SCOPE_MISMATCH: ErrorCode = ErrorCode::new("CONSENT_SCOPE_MISMATCH");
/// The grant was revoked or has expired.
pub const CONSENT_REVOKED: ErrorCode = ErrorCode::new("CONSENT_REVOKED");
/// The disclosure the user agreed to is no longer the current one (§4.4(5)).
pub const CONSENT_DISCLOSURE_CHANGED: ErrorCode = ErrorCode::new("CONSENT_DISCLOSURE_CHANGED");
/// The privacy posture of the exact endpoint/route could not be resolved (§10.8).
pub const PRIVACY_POSTURE_UNRESOLVED: ErrorCode = ErrorCode::new("PRIVACY_POSTURE_UNRESOLVED");
/// An externally supplied path failed canonicalization/containment (§16.5).
pub const PATH_REJECTED: ErrorCode = ErrorCode::new("PATH_REJECTED");
/// An archive entry was rejected before any user data was mutated (§13.5).
pub const ARCHIVE_ENTRY_REJECTED: ErrorCode = ErrorCode::new("ARCHIVE_ENTRY_REJECTED");
/// A sandboxed helper failed, was killed, or exceeded a resource cap (§6.3A).
pub const HELPER_FAILED: ErrorCode = ErrorCode::new("HELPER_FAILED");
/// A sandboxed helper exceeded its wall-clock or CPU budget.
pub const HELPER_TIMEOUT: ErrorCode = ErrorCode::new("HELPER_TIMEOUT");
/// A command was refused instead of executed (§6.3A). Inside this crate it is
/// produced by [`crate::response::ResponseLeakError`], when a result would have
/// carried protected data out to the view; `plaudon-automation` reports the same
/// code when the local automation surface is disabled or owned by another user.
pub const COMMAND_NOT_AUTHORIZED: ErrorCode = ErrorCode::new("COMMAND_NOT_AUTHORIZED");
/// A command argument failed Rust-side validation before it reached domain
/// code (§15.1).
pub const COMMAND_ARGUMENT_INVALID: ErrorCode = ErrorCode::new("COMMAND_ARGUMENT_INVALID");

// ---------------------------------------------------------------------------
// Action constants
// ---------------------------------------------------------------------------

pub const OPEN_NETWORK_SETTINGS: ErrorAction = ErrorAction::new("OPEN_NETWORK_SETTINGS");
pub const DISABLE_LOCAL_ONLY_MODE: ErrorAction = ErrorAction::new("DISABLE_LOCAL_ONLY_MODE");
pub const UNLOCK_CREDENTIAL_STORE: ErrorAction = ErrorAction::new("UNLOCK_CREDENTIAL_STORE");
pub const OPEN_CLOUD_CONSENT: ErrorAction = ErrorAction::new("OPEN_CLOUD_CONSENT");
pub const OPEN_API_KEY_SETTINGS: ErrorAction = ErrorAction::new("OPEN_API_KEY_SETTINGS");
pub const CHOOSE_DIFFERENT_FILE: ErrorAction = ErrorAction::new("CHOOSE_DIFFERENT_FILE");
pub const APPROVE_MODEL_DOWNLOAD: ErrorAction = ErrorAction::new("APPROVE_MODEL_DOWNLOAD");
pub const RETRY_JOB: ErrorAction = ErrorAction::new("RETRY_JOB");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_screaming_snake_case() {
        for code in [
            EGRESS_DENIED,
            SECRET_STORE_LOCKED,
            SECRET_STORE_FAILED,
            SECRET_NOT_FOUND,
            CONSENT_REQUIRED,
            CONSENT_SCOPE_MISMATCH,
            CONSENT_REVOKED,
            CONSENT_DISCLOSURE_CHANGED,
            PRIVACY_POSTURE_UNRESOLVED,
            PATH_REJECTED,
            ARCHIVE_ENTRY_REJECTED,
            HELPER_FAILED,
            HELPER_TIMEOUT,
            COMMAND_NOT_AUTHORIZED,
            COMMAND_ARGUMENT_INVALID,
        ] {
            let s = code.as_str();
            assert_ne!(s.len(), 0);
            assert!(
                s.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "code {s} is not SCREAMING_SNAKE_CASE"
            );
        }
    }

    #[test]
    fn builder_sets_fields() {
        let parts = ErrorEnvelopeParts::new(EGRESS_DENIED, "blocked")
            .retryable(false)
            .action(DISABLE_LOCAL_ONLY_MODE);
        assert_eq!(parts.code.as_str(), "EGRESS_DENIED");
        assert_eq!(
            parts.action.map(ErrorAction::as_str),
            Some("DISABLE_LOCAL_ONLY_MODE")
        );
        assert!(!parts.retryable);
    }
}
