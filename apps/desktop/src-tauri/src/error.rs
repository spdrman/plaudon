//! The single boundary where an [`AppError`] becomes something the UI can see.
//!
//! Spec §15.4: the envelope never carries signed URLs, credentials, raw request
//! bodies, filesystem paths or transcript excerpts. This module is the only
//! place that builds one, and it builds it exclusively from the code's own
//! canonical message. The internal detail goes to `tracing` (after the domain
//! redactor has run over it) and nowhere else.

use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorEnvelope};
use plaudon_domain::time::Timestamp;

/// Result type of every command: the frontend receives an envelope, never an
/// internal error type.
pub type CommandResult<T> = Result<T, ErrorEnvelope>;

/// Build the frontend envelope for `err` and log its redacted detail.
///
/// A fresh [`plaudon_domain::ids::DiagnosticId`] is minted for every call so a
/// user-reported id maps to exactly one log line.
#[must_use]
pub fn envelope(err: &AppError) -> ErrorEnvelope {
    let mut log = DiagnosticLog::new();
    let envelope = err.to_envelope(&mut log, Timestamp::now());
    let redacted = log
        .get(envelope.diagnostic_id)
        .map(|record| record.redacted_detail.clone());
    tracing::warn!(
        code = envelope.code.as_str(),
        diagnostic_id = %envelope.diagnostic_id,
        detail = redacted.as_deref().unwrap_or("[unavailable]"),
        "command boundary rejected an operation"
    );
    envelope
}

/// Convenience for the common `Result<T, AppError>` to `CommandResult<T>` hop.
pub fn to_command_result<T>(result: Result<T, AppError>) -> CommandResult<T> {
    result.map_err(|err| envelope(&err))
}

/// Shorthand for an internal failure whose detail must not reach the UI.
pub fn internal(detail: impl Into<String>) -> AppError {
    AppError::new(AppErrorCode::InternalError, detail)
}

/// Shorthand for a rejected command argument.
pub fn invalid_argument(detail: impl Into<String>) -> AppError {
    AppError::new(AppErrorCode::InvalidArgument, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEARER: &str = "Bearer sk-or-v1-9f3c2b7d8e1a4c6f0b2d";
    const SIGNED_URL: &str =
        "https://media.example.com/a/b.wav?X-Amz-Signature=2b1f9c7e4d6a8b0c2e4f6a8b0c2e4f6a";
    const PATH: &str = "/Users/someone/Documents/Board meeting.wav";
    const TRANSCRIPT: &str = "we agreed to terminate the Ashford contract on Friday";

    fn leaky_error() -> AppError {
        AppError::new(
            AppErrorCode::ProviderRejected,
            format!("authorization={BEARER} url={SIGNED_URL} file={PATH} text={TRANSCRIPT}"),
        )
    }

    #[test]
    fn the_envelope_leaks_no_secret_no_url_no_path_and_no_transcript() {
        let envelope = envelope(&leaky_error());
        let json = serde_json::to_string(&envelope).unwrap();
        for needle in [
            BEARER,
            SIGNED_URL,
            PATH,
            TRANSCRIPT,
            "sk-or-v1-9f3c2b7d8e1a4c6f0b2d",
            "X-Amz-Signature",
            "Ashford",
            "Documents",
        ] {
            assert!(
                !json.contains(needle),
                "envelope JSON leaked {needle}: {json}"
            );
        }
    }

    #[test]
    fn the_envelope_message_is_exactly_the_canonical_message() {
        let err = leaky_error();
        let envelope = envelope(&err);
        assert_eq!(
            envelope.message,
            AppErrorCode::ProviderRejected.canonical_message()
        );
        assert_eq!(envelope.code, AppErrorCode::ProviderRejected);
        assert_eq!(
            envelope.retryable,
            AppErrorCode::ProviderRejected.is_retryable()
        );
    }

    #[test]
    fn every_call_mints_a_distinct_diagnostic_id() {
        let err = leaky_error();
        assert_ne!(envelope(&err).diagnostic_id, envelope(&err).diagnostic_id);
    }

    #[test]
    fn no_error_code_renders_a_message_that_contains_a_path_or_url() {
        for code in AppErrorCode::ALL {
            let json = serde_json::to_string(&envelope(&AppError::new(*code, PATH))).unwrap();
            assert!(!json.contains(PATH), "{code} leaked the detail");
            assert!(!json.contains("http"), "{code} rendered a URL");
        }
    }
}
