//! Presentation tables for the PLAUD Cloud connector and watched folders.
//!
//! One word per state, and the mapping from a `next_action` onto a control.
//! Everything that *explains* a state — the headline, the detail, the warnings
//! and the disconnect wording — is written in Rust by
//! `plaudon_source_plaud_cli::diagnostics` and arrives in the DTO; this screen
//! renders those strings verbatim and composes no sentence of its own.

use plaudon_app::dto::{
    PlaudCompatibility, PlaudConnectorState, PlaudImportOutcome, PlaudListingCompleteness,
    PlaudNextAction, WatchedFolderRule,
};

use crate::screens::devices::presentation::{Badge, Tone};

/// PLAUD's own documentation for the official command-line tool (§4.2.2). The
/// application links to it and never installs anything itself.
pub const PLAUD_CLI_DOCS_URL: &str =
    "https://support.plaud.ai/hc/en-us/articles/57751026815257-Plaud-CLI";

/// One word for a connector state. A label, never a sentence.
#[must_use]
pub const fn state_label(state: PlaudConnectorState) -> &'static str {
    match state {
        PlaudConnectorState::Unconfigured => "Not connected",
        PlaudConnectorState::CliDetected => "Tool found",
        PlaudConnectorState::AuthRequired => "Sign-in needed",
        PlaudConnectorState::Authenticated => "Signed in",
        PlaudConnectorState::Enumerating => "Listing",
        PlaudConnectorState::Ready => "Ready",
        PlaudConnectorState::ExpiredAuth => "Session expired",
        PlaudConnectorState::IncompatibleVersion => "Untested version",
        PlaudConnectorState::Error => "Stopped",
        PlaudConnectorState::LocalOnlyBlocked => "Switched off by Local-only Mode",
        PlaudConnectorState::RuntimeMissing => "Node.js missing",
    }
}

/// Glyph and tone for a connector state.
#[must_use]
pub const fn state_badge(state: PlaudConnectorState) -> Badge {
    match state {
        PlaudConnectorState::Unconfigured | PlaudConnectorState::LocalOnlyBlocked => Badge {
            glyph: "•",
            tone: Tone::Neutral,
        },
        PlaudConnectorState::CliDetected => Badge {
            glyph: "•",
            tone: Tone::Waiting,
        },
        PlaudConnectorState::Enumerating => Badge {
            glyph: "↻",
            tone: Tone::Running,
        },
        PlaudConnectorState::Authenticated | PlaudConnectorState::Ready => Badge {
            glyph: "✓",
            tone: Tone::Success,
        },
        PlaudConnectorState::AuthRequired
        | PlaudConnectorState::ExpiredAuth
        | PlaudConnectorState::IncompatibleVersion
        | PlaudConnectorState::RuntimeMissing => Badge {
            glyph: "!",
            tone: Tone::Warning,
        },
        PlaudConnectorState::Error => Badge {
            glyph: "×",
            tone: Tone::Danger,
        },
    }
}

/// The compatibility verdict word (§7.1).
#[must_use]
pub const fn compatibility_label(compatibility: PlaudCompatibility) -> &'static str {
    match compatibility {
        PlaudCompatibility::Unknown => "Unknown",
        PlaudCompatibility::Tested => "Tested",
        PlaudCompatibility::Untested => "Untested",
    }
}

/// What a control triggers. The screen decides how to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRun {
    /// `plaud_connect`.
    Connect,
    /// `plaud_probe` again.
    Retry,
    /// Open the privacy section, where Local-only Mode lives.
    ReviewLocalOnlyMode,
    /// Open the manual file import.
    ImportManually,
}

/// One concrete control for one `next_action` (§5.4, §4.2).
///
/// The label names the control; the sentence that says *why* is the
/// diagnostics detail, rendered verbatim next to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextActionControl {
    /// Open vendor documentation in the browser.
    Link {
        /// Control label.
        label: &'static str,
        /// Target.
        href: &'static str,
    },
    /// Run something in the application.
    Action {
        /// Control label.
        label: &'static str,
        /// What it runs.
        run: ControlRun,
    },
}

/// Map a `next_action` onto its control, or nothing when there is nothing to
/// do.
#[must_use]
pub fn next_action_control(action: &PlaudNextAction) -> Option<NextActionControl> {
    match action {
        PlaudNextAction::None => None,
        PlaudNextAction::InstallCli | PlaudNextAction::InstallTestedVersion { .. } => {
            Some(NextActionControl::Link {
                label: "Open PLAUD's installation guide",
                href: PLAUD_CLI_DOCS_URL,
            })
        }
        PlaudNextAction::SignIn => Some(NextActionControl::Action {
            label: "Sign in to PLAUD",
            run: ControlRun::Connect,
        }),
        PlaudNextAction::Retry | PlaudNextAction::WaitAndRetry => Some(NextActionControl::Action {
            label: "Try again",
            run: ControlRun::Retry,
        }),
        PlaudNextAction::ReviewLocalOnlyMode => Some(NextActionControl::Action {
            label: "Review Local-only Mode",
            run: ControlRun::ReviewLocalOnlyMode,
        }),
        PlaudNextAction::ImportManually => Some(NextActionControl::Action {
            label: "Import audio files instead",
            run: ControlRun::ImportManually,
        }),
        PlaudNextAction::CheckAccountAndRegion => Some(NextActionControl::Action {
            label: "Sign in again",
            run: ControlRun::Connect,
        }),
    }
}

/// The control for `action`, worded for the state it is shown in.
///
/// In [`PlaudConnectorState::LocalOnlyBlocked`] the review control is the only
/// way the connector comes on, so it says so (#304). It still only opens
/// Privacy settings: the choice to leave Local-only Mode is the user's.
#[must_use]
pub fn next_action_control_in(
    state: PlaudConnectorState,
    action: &PlaudNextAction,
) -> Option<NextActionControl> {
    match (state, next_action_control(action)) {
        (
            PlaudConnectorState::LocalOnlyBlocked,
            Some(NextActionControl::Action {
                run: ControlRun::ReviewLocalOnlyMode,
                ..
            }),
        ) => Some(NextActionControl::Action {
            label: "Turn on the PLAUD connector\u{2026}",
            run: ControlRun::ReviewLocalOnlyMode,
        }),
        (_, control) => control,
    }
}

/// Whether the panel offers "Check again", which re-runs discovery and the
/// version probe. Only for states a change on this computer can fix.
#[must_use]
pub const fn offers_check_again(state: PlaudConnectorState) -> bool {
    matches!(
        state,
        PlaudConnectorState::Unconfigured
            | PlaudConnectorState::IncompatibleVersion
            | PlaudConnectorState::LocalOnlyBlocked
            | PlaudConnectorState::RuntimeMissing
    )
}

/// Whether there is anything to disconnect. A connector that never got past
/// finding the tool has no session and no association to drop.
#[must_use]
pub const fn offers_disconnect(state: PlaudConnectorState) -> bool {
    !matches!(
        state,
        PlaudConnectorState::Unconfigured
            | PlaudConnectorState::LocalOnlyBlocked
            | PlaudConnectorState::RuntimeMissing
    )
}

/// Versions the diagnostics says would work, if it names any.
#[must_use]
pub fn usable_versions(action: &PlaudNextAction) -> &[String] {
    match action {
        PlaudNextAction::InstallTestedVersion { usable_versions } => usable_versions,
        _ => &[],
    }
}

/// Whether the connector can list recordings from this state.
#[must_use]
pub const fn can_list(state: PlaudConnectorState) -> bool {
    matches!(
        state,
        PlaudConnectorState::Authenticated
            | PlaudConnectorState::Ready
            | PlaudConnectorState::Enumerating
    )
}

/// Badge for a listing's completeness disclosure (§7.2).
#[must_use]
pub const fn listing_badge(completeness: PlaudListingCompleteness) -> (Badge, &'static str) {
    match completeness {
        PlaudListingCompleteness::Complete => (
            Badge {
                glyph: "✓",
                tone: Tone::Success,
            },
            "Complete",
        ),
        PlaudListingCompleteness::PossiblyIncomplete => (
            Badge {
                glyph: "!",
                tone: Tone::Warning,
            },
            "May be incomplete",
        ),
    }
}

/// Name of a watched-folder rule (§4.3(7)).
#[must_use]
pub const fn rule_label(rule: WatchedFolderRule) -> &'static str {
    match rule {
        WatchedFolderRule::ImportOnly => "Import the audio only",
        WatchedFolderRule::ImportAndQueueLocalStt => {
            "Import and queue local transcription (nothing leaves this computer)"
        }
        WatchedFolderRule::AskBeforeCloud => {
            "Import, and ask before any cloud step: no background dispatch without consent"
        }
    }
}

/// The three rules, in the order the specification lists them.
pub const RULES: [WatchedFolderRule; 3] = [
    WatchedFolderRule::ImportOnly,
    WatchedFolderRule::ImportAndQueueLocalStt,
    WatchedFolderRule::AskBeforeCloud,
];

/// `m:ss` or `h:mm:ss`, or nothing when the source reported no duration.
///
/// An unknown duration is never rendered as `0:00`.
#[must_use]
pub fn duration_label(duration_ms: Option<u64>) -> Option<String> {
    let total = duration_ms? / 1000;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    Some(if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    })
}

/// The badge and word for one import outcome.
#[must_use]
pub fn outcome_badge(outcome: &PlaudImportOutcome) -> (Badge, &'static str) {
    match outcome {
        PlaudImportOutcome::Imported { .. } => (
            Badge {
                glyph: "✓",
                tone: Tone::Success,
            },
            "Imported",
        ),
        PlaudImportOutcome::AlreadyImported { .. } => (
            Badge {
                glyph: "=",
                tone: Tone::Neutral,
            },
            "Already in library",
        ),
        PlaudImportOutcome::Failed { .. } => (
            Badge {
                glyph: "✕",
                tone: Tone::Danger,
            },
            "Failed",
        ),
    }
}

/// The external identifier an outcome is about.
#[must_use]
pub fn outcome_id(outcome: &PlaudImportOutcome) -> &str {
    match outcome {
        PlaudImportOutcome::Imported { external_id, .. }
        | PlaudImportOutcome::AlreadyImported { external_id, .. }
        | PlaudImportOutcome::Failed { external_id, .. } => external_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_its_own_word() {
        let states = [
            PlaudConnectorState::Unconfigured,
            PlaudConnectorState::CliDetected,
            PlaudConnectorState::AuthRequired,
            PlaudConnectorState::Authenticated,
            PlaudConnectorState::Enumerating,
            PlaudConnectorState::Ready,
            PlaudConnectorState::ExpiredAuth,
            PlaudConnectorState::IncompatibleVersion,
            PlaudConnectorState::Error,
            PlaudConnectorState::LocalOnlyBlocked,
            PlaudConnectorState::RuntimeMissing,
        ];
        let mut labels: Vec<&str> = states.iter().copied().map(state_label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), states.len(), "two states share a word");
    }

    #[test]
    fn only_a_usable_session_may_list() {
        assert!(can_list(PlaudConnectorState::Ready));
        assert!(can_list(PlaudConnectorState::Authenticated));
        assert!(!can_list(PlaudConnectorState::ExpiredAuth));
        assert!(!can_list(PlaudConnectorState::IncompatibleVersion));
        assert!(!can_list(PlaudConnectorState::Error));
        assert!(!can_list(PlaudConnectorState::Unconfigured));
    }

    #[test]
    fn every_next_action_becomes_one_control() {
        let actions = [
            PlaudNextAction::None,
            PlaudNextAction::InstallCli,
            PlaudNextAction::InstallTestedVersion {
                usable_versions: vec!["1.2.3".to_owned()],
            },
            PlaudNextAction::SignIn,
            PlaudNextAction::Retry,
            PlaudNextAction::WaitAndRetry,
            PlaudNextAction::ReviewLocalOnlyMode,
            PlaudNextAction::ImportManually,
            PlaudNextAction::CheckAccountAndRegion,
        ];
        for action in &actions {
            let control = next_action_control(action);
            assert_eq!(
                control.is_none(),
                matches!(action, PlaudNextAction::None),
                "{action:?} must map to exactly one control"
            );
        }
        assert_eq!(
            usable_versions(&PlaudNextAction::InstallTestedVersion {
                usable_versions: vec!["1.2.3".to_owned()]
            }),
            ["1.2.3".to_owned()]
        );
        assert_eq!(usable_versions(&PlaudNextAction::SignIn), [] as [String; 0]);
    }

    #[test]
    fn an_unknown_duration_is_not_rendered_as_zero() {
        assert_eq!(duration_label(None), None);
        assert_eq!(duration_label(Some(0)), Some("0:00".to_owned()));
        assert_eq!(duration_label(Some(65_000)), Some("1:05".to_owned()));
        assert_eq!(duration_label(Some(3_605_000)), Some("1:00:05".to_owned()));
    }
}
