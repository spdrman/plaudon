//! Notification payload redaction (spec §18.6, §5.5).
//!
//! # The rule
//!
//! **A notification carries counts, a lifecycle state and opaque recording
//! identifiers. It never carries content.**
//!
//! OS notification centres persist, mirror to other devices, render on a lock
//! screen and are readable by other local software. Anything we hand them has
//! left our trust boundary, so filtering is not enough: a filter can only
//! remove what it recognises, and a transcript excerpt is indistinguishable
//! from a status sentence.
//!
//! [`redact_notification`] therefore **reconstructs** the notification from an
//! allowlisted vocabulary instead of passing the caller's strings through:
//!
//! * secrets are scrubbed first (defence in depth, in case a caller formatted a
//!   URL or key into a title);
//! * a subject word and a lifecycle state are looked up in a fixed vocabulary;
//! * counts (`<number> <known unit>`) and opaque identifiers (`rec_…`,
//!   `job_…`, UUIDs) are extracted;
//! * everything else is discarded.
//!
//! A transcript excerpt contributes no vocabulary tokens, so it cannot survive.

use std::sync::LazyLock;

use regex::Regex;

use crate::redact::Redactor;

/// Subject words a notification may name.
const SUBJECTS: &[&str] = &[
    "transcription",
    "transcript",
    "summary",
    "import",
    "export",
    "backup",
    "restore",
    "recording",
    "device",
    "sync",
    "job",
    "download",
    "update",
];

/// Lifecycle states a notification may report.
const STATES: &[&str] = &[
    "complete",
    "completed",
    "finished",
    "ready",
    "failed",
    "cancelled",
    "canceled",
    "queued",
    "started",
    "paused",
    "connected",
    "disconnected",
    "available",
    "unavailable",
];

/// Fallback body when nothing safe could be extracted.
pub const NOTIFICATION_FALLBACK_BODY: &str = "Open Plaudon for details.";

/// Fallback title.
pub const NOTIFICATION_FALLBACK_TITLE: &str = "Plaudon";

/// Maximum number of extracted counts or identifiers echoed into the body.
const MAX_BODY_TOKENS: usize = 4;

static COUNT_RE: LazyLock<Regex> = LazyLock::new(|| {
    regex_or_empty(
        r"(?i)\b(\d{1,7})\s+(recordings?|files?|jobs?|items?|segments?|errors?|minutes?|seconds?|hours?|transcripts?|devices?|summaries|summary|actions?)\b",
    )
});

static IDENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    regex_or_empty(
        r"(?i)\b((?:rec|job|dev|med|src|exp|imp)_[a-z0-9]{4,32}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\b",
    )
});

static WORD_RE: LazyLock<Regex> = LazyLock::new(|| regex_or_empty(r"[A-Za-z]+"));

fn regex_or_empty(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(re) => re,
        Err(err) => unreachable!("built-in notification pattern failed to compile: {err}"),
    }
}

/// Reduce a notification to a non-sensitive summary.
///
/// Returns `(title, body)`. Neither output contains any substring of the input
/// other than allowlisted vocabulary words, extracted counts and opaque
/// identifiers.
#[must_use]
pub fn redact_notification(title: &str, body: &str) -> (String, String) {
    let redactor = Redactor::shared();
    let safe_title = redactor.redact(title).into_owned();
    let safe_body = redactor.redact(body).into_owned();
    let combined = format!("{safe_title}\n{safe_body}");

    let mut subject = None;
    let mut state = None;
    for word in WORD_RE.find_iter(&combined) {
        let lowered = word.as_str().to_ascii_lowercase();
        if subject.is_none() {
            if let Some(found) = SUBJECTS.iter().find(|s| **s == lowered) {
                subject = Some(*found);
                continue;
            }
        }
        if state.is_none() {
            if let Some(found) = STATES.iter().find(|s| **s == lowered) {
                state = Some(*found);
            }
        }
    }

    let title_out = match (subject, state) {
        (Some(subject), Some(state)) => {
            format!(
                "{NOTIFICATION_FALLBACK_TITLE}: {} {state}",
                capitalize(subject)
            )
        }
        (Some(subject), None) => {
            format!("{NOTIFICATION_FALLBACK_TITLE}: {}", capitalize(subject))
        }
        (None, Some(state)) => format!("{NOTIFICATION_FALLBACK_TITLE}: {}", capitalize(state)),
        (None, None) => NOTIFICATION_FALLBACK_TITLE.to_owned(),
    };

    let mut tokens: Vec<String> = Vec::new();
    for caps in COUNT_RE.captures_iter(&combined) {
        let (Some(number), Some(unit)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let token = format!("{} {}", number.as_str(), unit.as_str().to_ascii_lowercase());
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    for caps in IDENT_RE.captures_iter(&combined) {
        let Some(ident) = caps.get(1) else { continue };
        let token = ident.as_str().to_owned();
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens.truncate(MAX_BODY_TOKENS);

    let body_out = if tokens.is_empty() {
        NOTIFICATION_FALLBACK_BODY.to_owned()
    } else {
        format!("{} · {NOTIFICATION_FALLBACK_BODY}", tokens.join(" · "))
    };

    (title_out, body_out)
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_notification_keeps_subject_state_and_counts() {
        let (title, body) = redact_notification(
            "Transcription complete",
            "3 recordings ready · rec_7hd92kfa",
        );
        assert_eq!(title, "Plaudon: Transcription complete");
        assert!(body.contains("3 recordings"));
        assert!(body.contains("rec_7hd92kfa"));
    }

    #[test]
    fn unknown_vocabulary_collapses_to_fallback() {
        let (title, body) = redact_notification("", "");
        assert_eq!(title, NOTIFICATION_FALLBACK_TITLE);
        assert_eq!(body, NOTIFICATION_FALLBACK_BODY);
    }
}
