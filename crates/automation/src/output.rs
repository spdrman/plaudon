//! The CLI's output envelope (spec 15.4, 16.6, App. A.5).
//!
//! Everything the CLI prints to stdout is one JSON document. That is the
//! M2.1 acceptance criterion, and it is also what makes the no-secrets rule
//! checkable: there is exactly one place where output is produced, and it
//! serializes typed values rather than formatting arbitrary strings.
//!
//! Failures use the same `code` / `message` / `retryable` shape as the
//! frontend error envelope (spec 15.4), so a script and the GUI describe the
//! same failure the same way.

use serde::{Deserialize, Serialize};

/// Contract version of the CLI output envelope.
pub const OUTPUT_CONTRACT_VERSION: u32 = 1;

/// One CLI result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Envelope {
    /// The command succeeded.
    Ok {
        /// Envelope contract version.
        contract_version: u32,
        /// Command that ran, e.g. `list`.
        command: String,
        /// Command-specific payload.
        data: serde_json::Value,
    },
    /// The command failed.
    Error {
        /// Envelope contract version.
        contract_version: u32,
        /// Command that ran.
        command: String,
        /// Stable error code, from the application taxonomy where one applies.
        code: String,
        /// Vendor-neutral message. Never a provider payload or a transcript
        /// excerpt (spec 15.4).
        message: String,
        /// Whether retrying the same invocation unchanged is safe.
        retryable: bool,
    },
}

impl Envelope {
    /// A success envelope.
    #[must_use]
    pub fn ok(command: &str, data: serde_json::Value) -> Self {
        Self::Ok {
            contract_version: OUTPUT_CONTRACT_VERSION,
            command: command.to_string(),
            data,
        }
    }

    /// A failure envelope.
    #[must_use]
    pub fn error(command: &str, code: &str, message: &str, retryable: bool) -> Self {
        Self::Error {
            contract_version: OUTPUT_CONTRACT_VERSION,
            command: command.to_string(),
            code: code.to_string(),
            message: message.to_string(),
            retryable,
        }
    }

    /// Whether this envelope reports success.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Process exit code: zero for success, two for a refusal or failure.
    ///
    /// Two rather than one, so a shell can distinguish "the CLI refused" from
    /// "the CLI could not start at all".
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Ok { .. } => 0,
            Self::Error { .. } => 2,
        }
    }

    /// Render as a single JSON line.
    ///
    /// # Panics
    ///
    /// Never: every field is a plain serializable type.
    #[must_use]
    pub fn render(&self) -> String {
        serde_json::to_string(self).expect("an envelope is always serializable")
    }

    /// Render as indented JSON.
    ///
    /// # Panics
    ///
    /// Never: every field is a plain serializable type.
    #[must_use]
    pub fn render_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("an envelope is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_success_envelope_carries_the_command_and_version() {
        let envelope = Envelope::ok("list", serde_json::json!({"recordings": []}));
        assert!(envelope.is_ok());
        assert_eq!(envelope.exit_code(), 0);
        let value: serde_json::Value = serde_json::from_str(&envelope.render()).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["command"], "list");
        assert_eq!(value["contract_version"], OUTPUT_CONTRACT_VERSION);
    }

    #[test]
    fn a_failure_envelope_exits_non_zero_with_a_stable_code() {
        let envelope = Envelope::error(
            "process",
            "CONSENT_REQUIRED",
            "cloud processing needs consent that has not been granted",
            false,
        );
        assert!(!envelope.is_ok());
        assert_eq!(envelope.exit_code(), 2);
        let value: serde_json::Value = serde_json::from_str(&envelope.render()).unwrap();
        assert_eq!(value["code"], "CONSENT_REQUIRED");
        assert_eq!(value["retryable"], false);
    }

    #[test]
    fn every_envelope_is_a_single_line_of_valid_json() {
        for envelope in [
            Envelope::ok("list", serde_json::json!({"a": 1})),
            Envelope::error("export", "RECORD_NOT_FOUND", "no such recording", false),
        ] {
            let rendered = envelope.render();
            assert!(!rendered.contains('\n'));
            assert!(serde_json::from_str::<serde_json::Value>(&rendered).is_ok());
        }
    }

    #[test]
    fn envelopes_round_trip() {
        let envelope = Envelope::ok("jobs", serde_json::json!({"jobs": [{"id": "x"}]}));
        let back: Envelope = serde_json::from_str(&envelope.render()).unwrap();
        assert_eq!(back, envelope);
    }
}
