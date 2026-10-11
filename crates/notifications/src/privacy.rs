//! Privacy-safe payload construction (spec 5.5, spec 22(13)).
//!
//! macOS and Windows both render notification text on the lock screen and in
//! notification history, where a third party can read it. Spec 5.5 therefore
//! requires "privacy-safe generic text on lock screens".
//!
//! The rule this module enforces is structural rather than editorial: a caller
//! cannot pass a string. [`NotificationContext`] carries counts, a reason
//! classification and a typed target id; every word that ends up in a
//! [`NotificationRequest`] comes from a `&'static str` in this file or from a
//! formatted integer. There is no code path from a recording title, a file
//! path or a transcript excerpt into a notification, because none of those
//! types can be named by the builder's input.
//!
//! The matching UI rule lives elsewhere: the in-app activity history may show
//! the real title, because it is only visible inside an unlocked app window.

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;

use crate::{NotificationCategory, NotificationRequest, NotificationTarget};

/// Why a job stopped and needs a decision (spec 5.4, 14.7).
///
/// Each variant maps to one fixed phrase. New situations need a new variant,
/// which is exactly the review point we want.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttentionReason {
    /// Cloud transfer is waiting for consent/authorization (`WaitingForAuthorization`).
    AuthorizationRequired,
    /// A local engine or model asset is unavailable (`WaitingForModel`).
    ModelUnavailable,
    /// The source (device, folder, cloud CLI) is unavailable (`WaitingForSource`).
    SourceUnavailable,
    /// Not enough disk space to continue.
    NotEnoughSpace,
    /// The job failed and can be retried.
    RetryableFailure,
    /// The job failed terminally.
    TerminalFailure,
    /// A cloud request was dispatched but its outcome is unknown and possibly
    /// billed (spec 14.3); it never auto-retries.
    CloudOutcomeUnknown,
}

impl AttentionReason {
    /// Fixed, content-free phrase used in the notification body.
    const fn phrase(self) -> &'static str {
        match self {
            Self::AuthorizationRequired => "waiting for your authorization",
            Self::ModelUnavailable => "waiting for a local model",
            Self::SourceUnavailable => "waiting for its source",
            Self::NotEnoughSpace => "stopped for lack of disk space",
            Self::RetryableFailure => "failed and can be retried",
            Self::TerminalFailure => "failed",
            Self::CloudOutcomeUnknown => "has an unknown cloud outcome",
        }
    }
}

/// Which spending window was exhausted (spec 11, 14.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetScope {
    /// The limit for the current run of billable work.
    PerRun,
    /// The daily limit.
    Daily,
    /// The monthly limit.
    Monthly,
}

impl BudgetScope {
    /// Fixed, content-free phrase used in the notification body.
    const fn phrase(self) -> &'static str {
        match self {
            Self::PerRun => "per-run",
            Self::Daily => "daily",
            Self::Monthly => "monthly",
        }
    }
}

/// Structured input for [`build`]: counts, classifications and typed ids only.
///
/// Deliberately has no `String`, `Path` or free-text member anywhere, and the
/// variant also selects the [`NotificationCategory`], so a payload cannot be
/// filed under the wrong category either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationContext {
    /// An import batch finished (spec 7.4).
    ImportFinished {
        /// Recordings that were imported successfully.
        imported: u32,
        /// Files that failed to import.
        failed: u32,
        /// The single imported recording to open, when the batch had exactly one.
        recording: Option<RecordingId>,
    },
    /// Transcription finished for one or more recordings (spec 14.7 `Completed`).
    TranscriptionFinished {
        /// Recordings whose transcript is now available.
        recordings: u32,
        /// The single recording to open, when there was exactly one.
        recording: Option<RecordingId>,
    },
    /// One or more jobs need a user decision.
    RequiresAttention {
        /// How many jobs are in this state.
        jobs: u32,
        /// Why, as a fixed classification.
        reason: AttentionReason,
        /// The single job to open, when there was exactly one.
        job: Option<JobId>,
    },
    /// Cloud spending passed its limit and cloud work is paused (spec 5.4).
    CloudBudgetExceeded {
        /// Which limit was hit.
        scope: BudgetScope,
        /// Jobs left queued because of it.
        paused_jobs: u32,
    },
}

impl NotificationContext {
    /// The spec 5.5 category this context belongs to.
    #[must_use]
    pub const fn category(&self) -> NotificationCategory {
        match self {
            Self::ImportFinished { .. } => NotificationCategory::ImportFinished,
            Self::TranscriptionFinished { .. } => NotificationCategory::TranscriptionFinished,
            Self::RequiresAttention { .. } => NotificationCategory::RequiresAttention,
            Self::CloudBudgetExceeded { .. } => NotificationCategory::CloudBudgetExceeded,
        }
    }

    /// What a click on this notification should open.
    #[must_use]
    pub const fn target(&self) -> Option<NotificationTarget> {
        match self {
            Self::ImportFinished { recording, .. }
            | Self::TranscriptionFinished { recording, .. } => match recording {
                Some(id) => Some(NotificationTarget::Recording(*id)),
                None => None,
            },
            Self::RequiresAttention { job, .. } => match job {
                Some(id) => Some(NotificationTarget::Job(*id)),
                None => None,
            },
            Self::CloudBudgetExceeded { .. } => Some(NotificationTarget::Settings),
        }
    }

    /// Fixed headline for the category.
    const fn title(&self) -> &'static str {
        match self {
            Self::ImportFinished { .. } => "Import finished",
            Self::TranscriptionFinished { .. } => "Transcription finished",
            Self::RequiresAttention { .. } => "Requires attention",
            Self::CloudBudgetExceeded { .. } => "Cloud budget exceeded",
        }
    }

    /// Generic body built from counts and fixed phrases only.
    fn body(&self) -> String {
        match *self {
            Self::ImportFinished {
                imported, failed, ..
            } => {
                let imported_part = format!(
                    "{imported} {} imported",
                    plural(imported, "recording", "recordings")
                );
                if failed == 0 {
                    format!("{imported_part}.")
                } else {
                    format!(
                        "{imported_part}, {failed} {} failed.",
                        plural(failed, "file", "files")
                    )
                }
            }
            Self::TranscriptionFinished { recordings, .. } => format!(
                "{recordings} {} ready.",
                plural(recordings, "transcript", "transcripts")
            ),
            Self::RequiresAttention { jobs, reason, .. } => {
                if jobs == 1 {
                    format!("1 job {}.", reason.phrase())
                } else {
                    format!("{jobs} jobs {}.", reason.phrase())
                }
            }
            Self::CloudBudgetExceeded { scope, paused_jobs } => {
                let limit = format!("The {} cloud spending limit was reached", scope.phrase());
                if paused_jobs == 0 {
                    format!("{limit}. Cloud work is paused.")
                } else {
                    format!(
                        "{limit}. {paused_jobs} {} paused.",
                        plural(paused_jobs, "cloud job is", "cloud jobs are")
                    )
                }
            }
        }
    }
}

fn plural(count: u32, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 {
        one
    } else {
        many
    }
}

/// Build the payload for `context`, stamped with the time the underlying event
/// happened (not the time the OS displays it, spec 14.5).
///
/// The resulting title and body contain only words defined in this module and
/// decimal counts; see the module documentation for why that is structural.
#[must_use]
pub fn build(context: NotificationContext, occurred_at: Timestamp) -> NotificationRequest {
    NotificationRequest {
        category: context.category(),
        title: context.title().to_owned(),
        body: context.body(),
        target: context.target(),
        occurred_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recording title and a transcript excerpt that must never reach the OS.
    const ADVERSARIAL_TITLE: &str = "/Users/rom/secret-merger.mp3";
    const ADVERSARIAL_TRANSCRIPT: &str =
        "the acquisition closes friday, wire the deposit to account 4815162342";

    /// Everything the payload is allowed to contain: letters, digits, spaces and
    /// a closed set of punctuation. No `/`, `\\`, `.ext`-style tokens beyond the
    /// sentence period, no quotes, no colons.
    const ALLOWLIST: &str = r"^[A-Za-z0-9][A-Za-z0-9 ,.\-]*\.$";

    fn samples() -> Vec<NotificationContext> {
        vec![
            NotificationContext::ImportFinished {
                imported: 1,
                failed: 0,
                recording: Some(RecordingId::new()),
            },
            NotificationContext::ImportFinished {
                imported: 7,
                failed: 2,
                recording: None,
            },
            NotificationContext::TranscriptionFinished {
                recordings: 1,
                recording: Some(RecordingId::new()),
            },
            NotificationContext::TranscriptionFinished {
                recordings: 4,
                recording: None,
            },
            NotificationContext::RequiresAttention {
                jobs: 1,
                reason: AttentionReason::AuthorizationRequired,
                job: Some(JobId::new()),
            },
            NotificationContext::RequiresAttention {
                jobs: 3,
                reason: AttentionReason::CloudOutcomeUnknown,
                job: None,
            },
            NotificationContext::CloudBudgetExceeded {
                scope: BudgetScope::Monthly,
                paused_jobs: 0,
            },
            NotificationContext::CloudBudgetExceeded {
                scope: BudgetScope::PerRun,
                paused_jobs: 5,
            },
        ]
    }

    #[test]
    fn redaction_payloads_match_the_allowlist_and_leak_nothing() {
        let allowlist = regex::Regex::new(ALLOWLIST).unwrap_or_else(|err| unreachable!("{err}"));
        let leaks = [
            ADVERSARIAL_TITLE,
            ADVERSARIAL_TRANSCRIPT,
            "secret-merger",
            "secret",
            "merger",
            "mp3",
            "Users",
            "/Users/rom",
            "acquisition",
            "4815162342",
            "deposit",
        ];

        for context in samples() {
            let request = build(context, Timestamp::from_millis(1_700_000_000_000));
            assert!(
                allowlist.is_match(&request.body),
                "body {:?} is outside the allowlist",
                request.body
            );
            assert!(
                request
                    .title
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ' '),
                "title {:?} is outside the allowlist",
                request.title
            );
            for leak in leaks {
                let haystack = format!("{} {}", request.title, request.body).to_lowercase();
                assert!(
                    !haystack.contains(&leak.to_lowercase()),
                    "payload {haystack:?} leaked {leak:?}"
                );
            }
            // A path separator or an extension dot would both be caught above,
            // but assert directly so the intent survives a regex edit.
            assert!(!request.body.contains('/'));
            assert!(!request.body.contains('\\'));
        }
    }

    #[test]
    fn ids_never_appear_in_the_text_only_in_the_target() {
        let recording = RecordingId::new();
        let request = build(
            NotificationContext::ImportFinished {
                imported: 1,
                failed: 0,
                recording: Some(recording),
            },
            Timestamp::EPOCH,
        );
        assert!(!request.body.contains(&recording.to_string()));
        assert!(!request.title.contains(&recording.to_string()));
        assert_eq!(
            request.target,
            Some(NotificationTarget::Recording(recording))
        );
    }

    #[test]
    fn the_four_categories_produce_distinct_payloads() {
        let built: Vec<NotificationRequest> = samples()
            .into_iter()
            .map(|context| build(context, Timestamp::EPOCH))
            .collect();

        let mut seen_categories: Vec<NotificationCategory> =
            built.iter().map(|request| request.category).collect();
        seen_categories.sort_unstable();
        seen_categories.dedup();
        assert_eq!(seen_categories.len(), 4);

        let mut texts: Vec<String> = built
            .iter()
            .map(|request| format!("{}|{}", request.title, request.body))
            .collect();
        let total = texts.len();
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), total, "two contexts produced the same text");
    }

    #[test]
    fn counts_are_pluralised() {
        let one = build(
            NotificationContext::ImportFinished {
                imported: 1,
                failed: 0,
                recording: None,
            },
            Timestamp::EPOCH,
        );
        assert_eq!(one.body, "1 recording imported.");

        let many = build(
            NotificationContext::ImportFinished {
                imported: 3,
                failed: 1,
                recording: None,
            },
            Timestamp::EPOCH,
        );
        assert_eq!(many.body, "3 recordings imported, 1 file failed.");

        let zero = build(
            NotificationContext::TranscriptionFinished {
                recordings: 0,
                recording: None,
            },
            Timestamp::EPOCH,
        );
        assert_eq!(zero.body, "0 transcripts ready.");
    }

    #[test]
    fn attention_reasons_are_distinct_fixed_phrases() {
        let reasons = [
            AttentionReason::AuthorizationRequired,
            AttentionReason::ModelUnavailable,
            AttentionReason::SourceUnavailable,
            AttentionReason::NotEnoughSpace,
            AttentionReason::RetryableFailure,
            AttentionReason::TerminalFailure,
            AttentionReason::CloudOutcomeUnknown,
        ];
        let mut phrases: Vec<&str> = reasons.iter().map(|reason| reason.phrase()).collect();
        let total = phrases.len();
        phrases.sort_unstable();
        phrases.dedup();
        assert_eq!(phrases.len(), total);
    }

    #[test]
    fn budget_notifications_point_at_settings() {
        let request = build(
            NotificationContext::CloudBudgetExceeded {
                scope: BudgetScope::Daily,
                paused_jobs: 2,
            },
            Timestamp::EPOCH,
        );
        assert_eq!(request.target, Some(NotificationTarget::Settings));
        assert_eq!(
            request.body,
            "The daily cloud spending limit was reached. 2 cloud jobs are paused."
        );
    }

    #[test]
    fn occurred_at_is_the_event_time_not_now() {
        let event = Timestamp::from_millis(42);
        let request = build(
            NotificationContext::TranscriptionFinished {
                recordings: 1,
                recording: None,
            },
            event,
        );
        assert_eq!(request.occurred_at, event);
    }
}
