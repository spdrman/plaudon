//! Presentation rules for jobs (spec §5.2 Jobs, §5.4, §14.1, §14.7).
//!
//! The React shell held these rules in `jobPresentation.ts`; EPIC O moves them
//! into Rust so the egui views render them and the rules stay testable without
//! a renderer. Three properties are carried over unchanged:
//!
//! * every [`JobState`] has a label, a **text glyph** and a description, so
//!   status is never communicated by colour alone (§5.3);
//! * cancelling a cloud job never claims to recall an accepted request
//!   ([`CANCEL_DISCLOSURE`], §14.7);
//! * retrying an uncertain cloud outcome requires an explicit cost
//!   acknowledgement ([`JobStatePresentation::requires_cost_ack`], §14.3).
//!
//! The match on [`JobState`] is exhaustive, so a state added to the contract
//! fails to compile here instead of rendering as nothing.

use plaudon_domain::error::{AppErrorCode, ErrorAction, ErrorEnvelope};

use crate::dto::{JobKind, JobState};
use crate::tone::Tone;

/// How one job state is rendered.
///
/// The four flags are the published contract of §14.1/§14.7 — may it be
/// cancelled, may it be retried, is it terminal, does a retry need a cost
/// acknowledgement — and a caller reads them by name. Packing them into bit
/// flags would hide exactly the distinctions this table exists to make.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobStatePresentation {
    /// Short label.
    pub label: &'static str,
    /// Monochrome text glyph duplicating the tone (§5.3).
    pub glyph: &'static str,
    /// Tone of the badge.
    pub tone: Tone,
    /// One sentence saying what the state means.
    pub description: &'static str,
    /// Whether a cancel control may be offered.
    pub can_cancel: bool,
    /// Whether a retry control may be offered.
    pub can_retry: bool,
    /// Whether no further transition is possible.
    pub terminal: bool,
    /// Whether retrying may be billed again, so an explicit acknowledgement is
    /// required before dispatch (§14.3, §14.7).
    pub requires_cost_ack: bool,
}

/// The §14.1/§14.7 presentation of a job state.
#[must_use]
pub const fn job_state_presentation(state: JobState) -> JobStatePresentation {
    match state {
        JobState::Queued => JobStatePresentation {
            label: "Queued",
            glyph: "…",
            tone: Tone::Neutral,
            description: "Waiting for a free slot.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::Preflight => JobStatePresentation {
            label: "Preflight",
            glyph: "?",
            tone: Tone::Running,
            description: "Checking inputs, space and engine availability before starting.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::WaitingForUser => JobStatePresentation {
            label: "Waiting for you",
            glyph: "!",
            tone: Tone::Waiting,
            description: "This job needs a decision from you before it can continue.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::WaitingForNetwork => JobStatePresentation {
            label: "Waiting for network",
            glyph: "~",
            tone: Tone::Waiting,
            description: "Queued until a network connection is available again.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::WaitingForModel => JobStatePresentation {
            label: "Waiting for model",
            glyph: "M",
            tone: Tone::Waiting,
            description: "Paused because the model assets are not available right now.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::WaitingForSource => JobStatePresentation {
            label: "Waiting for source",
            glyph: "S",
            tone: Tone::Waiting,
            description: "Paused because the source file or device is unavailable.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::WaitingForAuthorization => JobStatePresentation {
            label: "Waiting for authorization",
            glyph: "K",
            tone: Tone::Waiting,
            description: "Paused until the required consent or credential is granted.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::RunningLocal => JobStatePresentation {
            label: "Running locally",
            glyph: ">",
            tone: Tone::Running,
            description: "Processing on this computer; nothing leaves the device.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::CloudReady => JobStatePresentation {
            label: "Ready to send",
            glyph: "^",
            tone: Tone::Running,
            description: "Prepared for the cloud provider; nothing has been sent yet.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::CloudSending => JobStatePresentation {
            label: "Sending to provider",
            glyph: "^^",
            tone: Tone::Running,
            description: "The request is in flight to the cloud provider.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::CloudOutcomeUnknown => JobStatePresentation {
            label: "Cloud outcome unknown",
            glyph: "??",
            tone: Tone::Warning,
            description: "The provider response was lost. The request may already have been accepted and billed, so it is never retried automatically.",
            can_cancel: false,
            can_retry: true,
            terminal: false,
            requires_cost_ack: true,
        },
        JobState::Verifying => JobStatePresentation {
            label: "Verifying",
            glyph: "=",
            tone: Tone::Running,
            description: "Checking the produced output before committing it.",
            can_cancel: true,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::Committing => JobStatePresentation {
            label: "Committing",
            glyph: "+",
            tone: Tone::Running,
            description: "Writing the output durably; this step is not interrupted.",
            can_cancel: false,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::Cancelling => JobStatePresentation {
            label: "Cancelling",
            glyph: "x-",
            tone: Tone::Warning,
            description: "Stopping our remaining work and discarding incomplete output.",
            can_cancel: false,
            can_retry: false,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::Completed => JobStatePresentation {
            label: "Completed",
            glyph: "OK",
            tone: Tone::Success,
            description: "The output and its metadata are durably committed.",
            can_cancel: false,
            can_retry: false,
            terminal: true,
            requires_cost_ack: false,
        },
        JobState::FailedRetryable => JobStatePresentation {
            label: "Failed — can retry",
            glyph: "!r",
            tone: Tone::Danger,
            description: "The job stopped on an error that may succeed on another attempt.",
            can_cancel: false,
            can_retry: true,
            terminal: false,
            requires_cost_ack: false,
        },
        JobState::FailedTerminal => JobStatePresentation {
            label: "Failed — cannot retry",
            glyph: "!!",
            tone: Tone::Danger,
            description: "The job stopped for a reason retrying cannot fix.",
            can_cancel: false,
            can_retry: false,
            terminal: true,
            requires_cost_ack: false,
        },
        JobState::Cancelled => JobStatePresentation {
            label: "Cancelled",
            glyph: "x",
            tone: Tone::Neutral,
            description: "You stopped this job. Incomplete output was discarded.",
            can_cancel: false,
            can_retry: true,
            terminal: true,
            requires_cost_ack: false,
        },
    }
}

/// §14.7: cloud cancellation cannot recall an accepted request or a charge.
pub const CANCEL_DISCLOSURE: &str = "Cancelling stops our remaining work only. A cloud request that was already accepted may not be reversed, and an upstream charge may still apply.";

/// What a corrective control asks the shell to do.
///
/// Only [`ControlIntent::Retry`] and [`ControlIntent::RetryWithCostAck`] map to
/// a command; the rest are navigation the shell performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlIntent {
    /// Retry the job unchanged.
    Retry,
    /// Retry after the user acknowledged a possible new charge.
    RetryWithCostAck,
    /// Leave the job queued as it is.
    KeepQueued,
    /// Switch the stage to a local engine.
    ChooseLocalEngine,
    /// Open application settings.
    OpenSettings,
    /// Open the OpenRouter key section.
    OpenAiSettings,
    /// Open the storage section.
    OpenStorageSettings,
    /// Help the user free disk space.
    FreeSpace,
    /// Open the PLAUD Cloud connection screen.
    OpenPlaudConnect,
    /// Open the consent review sheet.
    ReviewConsent,
    /// Re-request the download link.
    RefreshSourceLink,
    /// Open the Local-only Mode explanation.
    ReviewLocalOnlyMode,
    /// Open the redacted diagnostics view.
    OpenDiagnostics,
    /// Open the operating-system permission settings.
    OpenPermissionSettings,
    /// Offer the import again.
    RetryImport,
    /// Ask the user to charge the device.
    ChargeDevice,
    /// Open device troubleshooting.
    OpenDeviceDiagnostics,
    /// Show the CLI installation instructions.
    OpenCliSetup,
    /// Install a speech model, or import one, from the engine setup
    /// (issue #305).
    InstallSpeechModel,
}

/// One corrective control: the words on it and what it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionControl {
    /// Button label.
    pub label: &'static str,
    /// What pressing it means.
    pub intent: ControlIntent,
}

/// Retry, unchanged.
pub const RETRY_CONTROL: ActionControl = ActionControl {
    label: "Retry",
    intent: ControlIntent::Retry,
};
/// Leave the job queued (§5.4 provider outage).
pub const KEEP_QUEUED_CONTROL: ActionControl = ActionControl {
    label: "Keep queued",
    intent: ControlIntent::KeepQueued,
};
/// Switch to a local engine (§5.4 provider outage).
pub const CHOOSE_LOCAL_ENGINE_CONTROL: ActionControl = ActionControl {
    label: "Choose local engine",
    intent: ControlIntent::ChooseLocalEngine,
};

/// Secondary affordance asked for by a §15.4 envelope.
///
/// An envelope may carry no action at all; it then still offers Retry when it
/// is retryable, and otherwise the message stands alone.
#[must_use]
pub fn envelope_action_control(
    action: Option<ErrorAction>,
    retryable: bool,
) -> Option<ActionControl> {
    let control = match action {
        Some(ErrorAction::WaitAndRetry) => ActionControl {
            label: "Wait and retry",
            intent: ControlIntent::Retry,
        },
        Some(ErrorAction::RetryImport) => ActionControl {
            label: "Retry import",
            intent: ControlIntent::RetryImport,
        },
        Some(ErrorAction::InstallSpeechModel) => ActionControl {
            label: "Install a model",
            intent: ControlIntent::InstallSpeechModel,
        },
        Some(ErrorAction::ChooseAnotherModel) => ActionControl {
            label: "Choose another model",
            intent: ControlIntent::ChooseLocalEngine,
        },
        Some(ErrorAction::FreeDiskSpace) => ActionControl {
            label: "Free up space",
            intent: ControlIntent::FreeSpace,
        },
        Some(ErrorAction::OpenSettings) => ActionControl {
            label: "Open settings",
            intent: ControlIntent::OpenSettings,
        },
        Some(ErrorAction::OpenPermissionSettings) => ActionControl {
            label: "Open permission settings",
            intent: ControlIntent::OpenPermissionSettings,
        },
        Some(ErrorAction::OpenPlaudConnect) => ActionControl {
            label: "Open PLAUD Cloud settings",
            intent: ControlIntent::OpenPlaudConnect,
        },
        Some(ErrorAction::OpenCliSetup) => ActionControl {
            label: "Set up the official CLI",
            intent: ControlIntent::OpenCliSetup,
        },
        Some(ErrorAction::RefreshSourceLink) => ActionControl {
            label: "Refresh source link",
            intent: ControlIntent::RefreshSourceLink,
        },
        Some(ErrorAction::ReviewConsent) => ActionControl {
            label: "Review consent",
            intent: ControlIntent::ReviewConsent,
        },
        Some(ErrorAction::ReviewLocalOnlyMode) => ActionControl {
            label: "Review Local-only Mode",
            intent: ControlIntent::ReviewLocalOnlyMode,
        },
        Some(ErrorAction::OpenDeviceDiagnostics) => ActionControl {
            label: "Open device diagnostics",
            intent: ControlIntent::OpenDeviceDiagnostics,
        },
        Some(ErrorAction::ChargeDevice) => ActionControl {
            label: "Charge the device",
            intent: ControlIntent::ChargeDevice,
        },
        Some(ErrorAction::OpenDiagnostics) => ActionControl {
            label: "Open diagnostics",
            intent: ControlIntent::OpenDiagnostics,
        },
        None => return retryable.then_some(RETRY_CONTROL),
    };
    Some(control)
}

/// The §5.4 condition a failure code puts the panel in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorGuidance {
    /// Heading of the condition.
    pub title: &'static str,
    /// Corrective controls required by §5.4, in presentation order.
    pub controls: &'static [ActionControl],
    /// True when no billable retry may be offered (§5.4 credits exhausted).
    pub suppress_retry: bool,
    /// True when the panel must compare required against available capacity.
    pub show_capacity: bool,
}

const OUTAGE_CONTROLS: &[ActionControl] = &[
    RETRY_CONTROL,
    CHOOSE_LOCAL_ENGINE_CONTROL,
    KEEP_QUEUED_CONTROL,
];
const LOCAL_ONLY_CONTROLS: &[ActionControl] = &[
    KEEP_QUEUED_CONTROL,
    RETRY_CONTROL,
    ActionControl {
        label: "Review Local-only Mode",
        intent: ControlIntent::ReviewLocalOnlyMode,
    },
];
const STORAGE_CONTROLS: &[ActionControl] = &[
    ActionControl {
        label: "Free up space",
        intent: ControlIntent::FreeSpace,
    },
    ActionControl {
        label: "Open storage settings",
        intent: ControlIntent::OpenStorageSettings,
    },
];
const BUDGET_CONTROLS: &[ActionControl] = &[ActionControl {
    label: "Open key settings",
    intent: ControlIntent::OpenAiSettings,
}];

/// §5.4 guidance for the failure code a job carries; `None` when the code has
/// no named condition and the envelope message stands on its own.
///
/// The required corrective controls come from the code, not from the
/// envelope's `action`; `action` only adds the secondary affordance.
#[must_use]
pub fn error_guidance(code: AppErrorCode) -> Option<ErrorGuidance> {
    let guidance = match code {
        AppErrorCode::SourceUnavailable => ErrorGuidance {
            title: "OpenRouter is unavailable",
            controls: OUTAGE_CONTROLS,
            suppress_retry: false,
            show_capacity: false,
        },
        AppErrorCode::LocalOnlyMode => ErrorGuidance {
            title: "You are offline",
            controls: LOCAL_ONLY_CONTROLS,
            suppress_retry: false,
            show_capacity: false,
        },
        AppErrorCode::StorageFull => ErrorGuidance {
            title: "Not enough space",
            controls: STORAGE_CONTROLS,
            suppress_retry: false,
            show_capacity: true,
        },
        AppErrorCode::BudgetExceeded => ErrorGuidance {
            title: "Provider credits exhausted",
            controls: BUDGET_CONTROLS,
            suppress_retry: true,
            show_capacity: false,
        },
        AppErrorCode::OutcomeUnknown => ErrorGuidance {
            title: "Cloud request outcome unknown",
            controls: &[],
            suppress_retry: false,
            show_capacity: false,
        },
        _ => return None,
    };
    Some(guidance)
}

/// Guidance for an envelope, when its code names a §5.4 condition.
#[must_use]
pub fn envelope_guidance(envelope: &ErrorEnvelope) -> Option<ErrorGuidance> {
    error_guidance(envelope.code)
}

/// Milliseconds between `started_at` and `finished_at`, or `now` while the job
/// runs.
///
/// `None` when the backend reported no start, when a timestamp does not parse,
/// or when the result would be negative: unknown stays unknown and is never
/// rendered as `0` (§8.6A).
#[must_use]
pub fn elapsed_ms(
    started_at: Option<&str>,
    finished_at: Option<&str>,
    now_ms: i128,
) -> Option<i64> {
    let start = parse_rfc3339_ms(started_at?)?;
    let end = match finished_at {
        Some(value) => parse_rfc3339_ms(value)?,
        None => now_ms,
    };
    let delta = end - start;
    if delta < 0 {
        return None;
    }
    i64::try_from(delta).ok()
}

fn parse_rfc3339_ms(value: &str) -> Option<i128> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|instant| instant.unix_timestamp_nanos() / 1_000_000)
}

/// `1h 02m`, `2m 05s` or `9s`; `None` stays `None`.
#[must_use]
pub fn format_elapsed(ms: Option<i64>) -> Option<String> {
    let total = ms? / 1000;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    let text = if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    };
    Some(text)
}

/// The elapsed label a job row shows, including the honest unknown case.
#[must_use]
pub fn job_elapsed_label(
    started_at: Option<&str>,
    finished_at: Option<&str>,
    now_ms: i128,
) -> String {
    match format_elapsed(elapsed_ms(started_at, finished_at, now_ms)) {
        Some(text) => format!("Elapsed {text}"),
        None => "Elapsed time unknown".to_owned(),
    }
}

/// Display name of a job family.
#[must_use]
pub const fn job_kind_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Import => "Import",
        JobKind::Transcribe => "Transcription",
        JobKind::Summarize => "Summary",
        JobKind::Export => "Export",
        JobKind::ModelInstall => "Model install",
        JobKind::SourceSync => "Source sync",
        JobKind::SpeakerMatch => "Speaker matching",
    }
}

/// Progress a job row renders.
///
/// §J3: when the backend reports no ratio the bar is indeterminate; a missing
/// fraction never becomes `0.0`, and a running job never shows completion it
/// has not reported.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobProgress {
    /// A reported fraction in `0..=1`.
    Fraction(f32),
    /// The stage cannot report a ratio.
    Indeterminate,
    /// The job is finished; there is nothing left to show in motion.
    Finished,
}

/// The progress presentation of a job summary.
#[must_use]
pub fn job_progress(state: JobState, progress: Option<f64>) -> JobProgress {
    if state.is_terminal() {
        return JobProgress::Finished;
    }
    match progress {
        Some(value) if value.is_finite() => JobProgress::Fraction(value.clamp(0.0, 1.0) as f32),
        _ => JobProgress::Indeterminate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_a_distinct_label_and_glyph() {
        let mut labels: Vec<&str> = Vec::new();
        let mut glyphs: Vec<&str> = Vec::new();
        for state in JobState::ALL {
            let presentation = job_state_presentation(*state);
            assert!(
                !presentation.label.is_empty(),
                "expected presentation.label to be non-empty"
            );
            assert!(
                !presentation.glyph.is_empty(),
                "expected presentation.glyph to be non-empty"
            );
            assert!(
                !presentation.description.is_empty(),
                "expected presentation.description to be non-empty"
            );
            labels.push(presentation.label);
            glyphs.push(presentation.glyph);
        }
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "labels must be distinct");
        let mut sorted_glyphs = glyphs.clone();
        sorted_glyphs.sort_unstable();
        sorted_glyphs.dedup();
        assert_eq!(sorted_glyphs.len(), glyphs.len(), "glyphs must be distinct");
    }

    #[test]
    fn terminal_flag_matches_the_contract() {
        for state in JobState::ALL {
            assert_eq!(
                job_state_presentation(*state).terminal,
                state.is_terminal(),
                "{state:?}"
            );
        }
    }

    #[test]
    fn only_an_unknown_cloud_outcome_demands_a_cost_acknowledgement() {
        for state in JobState::ALL {
            let presentation = job_state_presentation(*state);
            assert_eq!(
                presentation.requires_cost_ack,
                *state == JobState::CloudOutcomeUnknown,
                "{state:?}"
            );
        }
    }

    #[test]
    fn committing_and_cancelling_cannot_be_cancelled_again() {
        assert!(!job_state_presentation(JobState::Committing).can_cancel);
        assert!(!job_state_presentation(JobState::Cancelling).can_cancel);
        assert!(job_state_presentation(JobState::RunningLocal).can_cancel);
    }

    #[test]
    fn provider_outage_offers_the_three_required_controls() {
        let guidance = error_guidance(AppErrorCode::SourceUnavailable).expect("guidance");
        let intents: Vec<ControlIntent> = guidance.controls.iter().map(|c| c.intent).collect();
        assert_eq!(
            intents,
            vec![
                ControlIntent::Retry,
                ControlIntent::ChooseLocalEngine,
                ControlIntent::KeepQueued
            ]
        );
    }

    #[test]
    fn exhausted_credits_suppress_a_billable_retry() {
        let guidance = error_guidance(AppErrorCode::BudgetExceeded).expect("guidance");
        assert!(guidance.suppress_retry);
        assert!(!guidance
            .controls
            .iter()
            .any(|c| c.intent == ControlIntent::Retry));
    }

    #[test]
    fn a_full_disk_asks_for_a_capacity_comparison() {
        assert!(
            error_guidance(AppErrorCode::StorageFull)
                .expect("guidance")
                .show_capacity
        );
    }

    #[test]
    fn an_unmapped_code_has_no_named_condition() {
        assert!(error_guidance(AppErrorCode::InternalError).is_none());
    }

    #[test]
    fn a_missing_action_falls_back_to_retry_only_when_retryable() {
        assert_eq!(envelope_action_control(None, true), Some(RETRY_CONTROL));
        assert_eq!(envelope_action_control(None, false), None);
    }

    #[test]
    fn elapsed_is_unknown_rather_than_zero_without_a_start() {
        assert_eq!(elapsed_ms(None, None, 0), None);
        assert_eq!(
            job_elapsed_label(None, None, 0),
            "Elapsed time unknown".to_owned()
        );
    }

    #[test]
    fn elapsed_formats_hours_minutes_and_seconds() {
        assert_eq!(format_elapsed(Some(9_000)).as_deref(), Some("9s"));
        assert_eq!(format_elapsed(Some(125_000)).as_deref(), Some("2m 05s"));
        assert_eq!(format_elapsed(Some(3_720_000)).as_deref(), Some("1h 02m"));
        assert_eq!(format_elapsed(None), None);
    }

    #[test]
    fn elapsed_uses_the_finish_instant_when_the_job_ended() {
        let elapsed = elapsed_ms(
            Some("2026-02-01T10:00:00Z"),
            Some("2026-02-01T10:00:30Z"),
            0,
        );
        assert_eq!(elapsed, Some(30_000));
    }

    #[test]
    fn a_clock_that_went_backwards_reports_unknown() {
        let elapsed = elapsed_ms(
            Some("2026-02-01T10:00:30Z"),
            Some("2026-02-01T10:00:00Z"),
            0,
        );
        assert_eq!(elapsed, None);
    }

    #[test]
    fn progress_is_indeterminate_when_no_ratio_is_reported() {
        assert_eq!(
            job_progress(JobState::RunningLocal, None),
            JobProgress::Indeterminate
        );
        assert_eq!(
            job_progress(JobState::RunningLocal, Some(0.25)),
            JobProgress::Fraction(0.25)
        );
        assert_eq!(
            job_progress(JobState::Completed, None),
            JobProgress::Finished
        );
    }

    #[test]
    fn job_kind_labels_are_stable() {
        assert_eq!(job_kind_label(JobKind::Transcribe), "Transcription");
        assert_eq!(job_kind_label(JobKind::Summarize), "Summary");
    }
}
