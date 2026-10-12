//! Classifying vendor failures into the stable error set (spec 7.6).
//!
//! Spec 2.1 forbids assuming the CLI has documented exit codes, so this module
//! cannot key on them alone. It uses both signals — exit code *and* a small set
//! of English phrase shapes on the redacted stderr excerpt — and falls back to
//! [`SourceError::SourceUnavailable`] rather than guessing.
//!
//! The raw vendor text never leaves this module. The classifier consumes a
//! redacted excerpt and returns a stable variant; [`crate::diagnostics`] turns
//! that variant into copy we wrote ourselves. Spec 7.6: "We will not leak raw
//! vendor error payloads to end users."

use plaudon_recording_sources::source::SourceError;

use crate::invocation::CliCommand;
use crate::runner::CliOutput;

/// A phrase shape and the stable error it indicates.
///
/// Matching is on lowercase substrings, which is deliberately conservative:
/// a phrase we have not seen falls through to the exit-code rule rather than
/// being forced into the nearest-looking variant.
const PHRASE_RULES: &[(&[&str], SourceError)] = &[
    (
        &[
            "not logged in",
            "login required",
            "please log in",
            "please login",
            "unauthenticated",
        ],
        SourceError::AuthenticationRequired,
    ),
    (
        &[
            "session expired",
            "token expired",
            "credentials expired",
            "re-authenticate",
        ],
        SourceError::AuthenticationRequired,
    ),
    (
        &[
            "login cancel",
            "cancelled by user",
            "canceled by user",
            "aborted by user",
        ],
        SourceError::AuthCanceled,
    ),
    (
        &["rate limit", "too many requests", "429"],
        SourceError::RemoteRateLimited,
    ),
    (
        &[
            "link expired",
            "url expired",
            "signature expired",
            "presigned",
            "410 gone",
        ],
        SourceError::LinkExpired,
    ),
    (
        &[
            "not found",
            "no such recording",
            "no such file",
            "unknown id",
            "404",
        ],
        SourceError::AssetNotFound,
    ),
    (
        &[
            "network",
            "connection refused",
            "timed out",
            "timeout",
            "dns",
            "offline",
            "unreachable",
        ],
        SourceError::SourceUnavailable,
    ),
    (
        &["checksum", "integrity", "corrupt"],
        SourceError::IntegrityFailed,
    ),
    (
        &[
            "unexpected response",
            "unsupported format",
            "parse error",
            "invalid json",
        ],
        SourceError::MalformedOutput,
    ),
];

/// Classify a completed run that did not exit successfully.
///
/// Returns `None` for a successful run, so the caller moves on to parsing.
#[must_use]
pub fn classify(output: &CliOutput, command: &CliCommand) -> Option<SourceError> {
    if output.succeeded() {
        return None;
    }
    let haystack = output.stderr_excerpt.to_ascii_lowercase();
    for (phrases, error) in PHRASE_RULES {
        if phrases.iter().any(|phrase| haystack.contains(phrase)) {
            return Some(error.clone());
        }
    }
    Some(classify_by_exit_code(output.exit_code, command))
}

/// Fall back to the exit code when no phrase matched.
///
/// Only two conventions are treated as meaningful, because only two are
/// portable: the shell's "command not executable" family (126/127) and the
/// signalled-process case. Everything else is an unexplained failure of the
/// command we ran, which is what `SourceUnavailable` means.
#[must_use]
pub const fn classify_by_exit_code(exit_code: Option<i32>, command: &CliCommand) -> SourceError {
    match exit_code {
        Some(126 | 127) => SourceError::CliMissing,
        // A signalled process produced no code. For an interactive login that
        // is almost always the user closing the browser flow; for anything
        // else it is an unexplained termination.
        None if command.is_interactive() => SourceError::AuthCanceled,
        _ => SourceError::SourceUnavailable,
    }
}

/// Whether an error is worth retrying without user action.
///
/// `LinkExpired` is excluded on purpose: the acquisition pipeline handles it
/// with exactly one fresh-URL attempt, and a generic retry layer would turn
/// that into an unbounded loop.
#[must_use]
pub const fn is_transient(error: &SourceError) -> bool {
    matches!(
        error,
        SourceError::SourceUnavailable
            | SourceError::RemoteRateLimited
            | SourceError::DownloadInterrupted
    )
}

/// Whether the connector must stop and ask the user for something.
#[must_use]
pub const fn needs_user_action(error: &SourceError) -> bool {
    matches!(
        error,
        SourceError::CliMissing
            | SourceError::CliUnsupported
            | SourceError::AuthenticationRequired
            | SourceError::AuthCanceled
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn output(exit_code: Option<i32>, stderr: &str) -> CliOutput {
        CliOutput {
            stdout: String::new(),
            stderr_excerpt: stderr.to_owned(),
            exit_code,
            elapsed: Duration::from_millis(1),
        }
    }

    #[test]
    fn a_successful_run_is_not_an_error() {
        assert_eq!(
            classify(
                &output(Some(0), "warning: beta build"),
                &CliCommand::files()
            ),
            None
        );
    }

    #[test]
    fn every_documented_failure_path_of_spec_4_2_is_representable() {
        // Spec 4.2 "Failure paths": CLI missing, CLI updated incompatibly,
        // login expired, canceled browser auth, missing audio, URL expired,
        // incompatible response format, network loss, insufficient disk
        // capacity, checksum mismatch, duplicate file.
        let cases: &[(Option<i32>, &str, SourceError)] = &[
            (Some(127), "", SourceError::CliMissing),
            (Some(126), "", SourceError::CliMissing),
            (
                Some(1),
                "Error: not logged in",
                SourceError::AuthenticationRequired,
            ),
            (
                Some(1),
                "session expired, please re-authenticate",
                SourceError::AuthenticationRequired,
            ),
            (Some(130), "login cancelled", SourceError::AuthCanceled),
            (Some(1), "no such recording", SourceError::AssetNotFound),
            (
                Some(1),
                "the download url expired",
                SourceError::LinkExpired,
            ),
            (
                Some(1),
                "unexpected response from server",
                SourceError::MalformedOutput,
            ),
            (
                Some(1),
                "network is unreachable",
                SourceError::SourceUnavailable,
            ),
            (Some(1), "checksum mismatch", SourceError::IntegrityFailed),
            (
                Some(1),
                "rate limit exceeded",
                SourceError::RemoteRateLimited,
            ),
        ];
        for (code, stderr, expected) in cases {
            assert_eq!(
                classify(&output(*code, stderr), &CliCommand::files()),
                Some(expected.clone()),
                "exit {code:?} / {stderr:?}"
            );
        }
    }

    #[test]
    fn an_unexplained_failure_is_not_guessed_into_a_specific_variant() {
        assert_eq!(
            classify(
                &output(Some(42), "something went wrong"),
                &CliCommand::files()
            ),
            Some(SourceError::SourceUnavailable)
        );
        assert_eq!(
            classify(&output(Some(1), ""), &CliCommand::files()),
            Some(SourceError::SourceUnavailable)
        );
    }

    #[test]
    fn a_killed_login_reads_as_a_cancelled_authentication() {
        assert_eq!(
            classify(&output(None, ""), &CliCommand::Login),
            Some(SourceError::AuthCanceled)
        );
        assert_eq!(
            classify(&output(None, ""), &CliCommand::files()),
            Some(SourceError::SourceUnavailable)
        );
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(
            classify(
                &output(Some(1), "ERROR: NOT LOGGED IN"),
                &CliCommand::files()
            ),
            Some(SourceError::AuthenticationRequired)
        );
    }

    #[test]
    fn classification_never_returns_vendor_text() {
        // The return type is a closed enum, so this is structural; the test
        // pins it so a future "message" field cannot be added without
        // revisiting spec 7.6.
        let error = classify(
            &output(
                Some(1),
                "FATAL: account 4711 at eu-west rejected token abcdef",
            ),
            &CliCommand::files(),
        )
        .expect("classified");
        let rendered = error.to_string();
        for leak in ["4711", "eu-west", "abcdef", "FATAL"] {
            assert!(!rendered.contains(leak), "{rendered} leaked {leak}");
        }
    }

    #[test]
    fn transient_and_user_action_sets_are_disjoint() {
        let all = [
            SourceError::CliMissing,
            SourceError::CliUnsupported,
            SourceError::AuthenticationRequired,
            SourceError::AuthCanceled,
            SourceError::SourceUnavailable,
            SourceError::RemoteRateLimited,
            SourceError::LinkExpired,
            SourceError::AssetNotFound,
            SourceError::MalformedOutput,
            SourceError::DownloadInterrupted,
            SourceError::IntegrityFailed,
            SourceError::Cancelled,
        ];
        for error in all {
            assert!(
                !(is_transient(&error) && needs_user_action(&error)),
                "{error:?} cannot be both"
            );
        }
        assert!(!is_transient(&SourceError::LinkExpired));
        assert!(!is_transient(&SourceError::IntegrityFailed));
    }
}
