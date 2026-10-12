//! Connector diagnostics copy (spec 4.2, 7.5, 7.6, 5.4).
//!
//! Every string the connector shows is written here, from a stable error code
//! or state — never from vendor output. That is the enforcement half of spec
//! 7.6 ("we will not leak raw vendor error payloads to end users"): the
//! rendering function physically has no vendor text to render.
//!
//! Spec 7.5 also requires honest account/region guidance. An empty listing is
//! *not* an error, so it gets its own advisory that suggests checking the
//! account and region and never asks for a token, a password or a cookie.

use plaudon_domain::error::AppErrorCode;
use plaudon_recording_sources::source::SourceError;
use serde::{Deserialize, Serialize};

use crate::compatibility::CliCompatibilityTable;
use crate::state::{ConnectorState, ConnectorStatus};

/// What the user should do next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum NextAction {
    /// Nothing to do; the connector is working.
    None,
    /// Install the official CLI, with a link to vendor documentation.
    InstallCli,
    /// Install a version we have tested.
    InstallTestedVersion {
        /// Versions that would work. Empty while none has been validated.
        usable_versions: Vec<String>,
    },
    /// Start the vendor's browser login.
    SignIn,
    /// Retry the operation.
    Retry,
    /// Wait before retrying.
    WaitAndRetry,
    /// Turn off Local-only Mode, or keep it on and import manually.
    ReviewLocalOnlyMode,
    /// Import files by hand instead.
    ImportManually,
    /// Check which PLAUD account and region is signed in.
    CheckAccountAndRegion,
}

/// A complete diagnostics panel payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorDiagnostics {
    /// Lifecycle state.
    pub state: ConnectorState,
    /// One-line summary.
    pub headline: String,
    /// Explanation, in our own words.
    pub detail: String,
    /// What to do next.
    pub next_action: NextAction,
    /// Version string the tool reported, when readable.
    pub detected_version: Option<String>,
    /// Compatibility verdict in one word.
    pub compatibility: &'static str,
    /// Where the executable was found.
    pub program_origin: Option<String>,
    /// Stable code of the last error, for log correlation.
    pub last_error: Option<AppErrorCode>,
    /// Warnings to show alongside the panel.
    pub warnings: Vec<&'static str>,
}

/// Compatibility warning shown when the active parser scrapes a layout.
pub const SCRAPING_WARNING: &str = "This version of the PLAUD command line tool does not offer a \
     machine-readable listing, so we read its on-screen table. A tool update may change that \
     table and stop the listing from working; manual import is unaffected.";

/// Disclosure shown when a listing may be partial.
pub const INCOMPLETE_LISTING_WARNING: &str = "Paging has not been tested for this version of the \
     PLAUD command line tool, so this list may not include every recording in your account.";

/// Copy for the disconnect action (spec 4.2.7).
///
/// Two variants, because the truthful sentence depends on whether the vendor
/// tool actually has a logout command we validated. Claiming to have ended a
/// session we cannot end would be exactly the kind of unverifiable assertion
/// spec 1.6 forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectCopy {
    /// We can only forget our own association.
    AssociationOnly,
    /// We can also ask the vendor tool to end its session.
    WithVendorLogout,
}

impl DisconnectCopy {
    /// Pick the truthful variant for a validated release.
    #[must_use]
    pub fn for_logout_support(supported: bool) -> Self {
        if supported {
            Self::WithVendorLogout
        } else {
            Self::AssociationOnly
        }
    }

    /// Button label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AssociationOnly => "Disconnect from this app",
            Self::WithVendorLogout => "Disconnect and sign out",
        }
    }

    /// Explanatory body.
    #[must_use]
    pub const fn body(self) -> &'static str {
        match self {
            Self::AssociationOnly => {
                "This removes the link between this app and the PLAUD command line tool. It does \
                 not sign you out of PLAUD: this version of the tool has no sign-out command we \
                 have tested. To end the PLAUD session itself, use the tool or the PLAUD website \
                 directly. Recordings you already imported stay in your library."
            }
            Self::WithVendorLogout => {
                "This removes the link between this app and the PLAUD command line tool and runs \
                 the tool's sign-out command. Recordings you already imported stay in your \
                 library."
            }
        }
    }
}

/// Advisory shown when enumeration succeeds with zero items (spec 7.5).
#[must_use]
pub const fn empty_listing_advisory() -> (&'static str, &'static str) {
    (
        "No recordings found",
        "The PLAUD command line tool signed in successfully but returned no recordings. Confirm \
         you are signed in to the PLAUD account that holds them, and that it is the right \
         regional account — PLAUD keeps separate accounts per region, and a Google or Apple \
         sign-in may have created a second one. We will never ask you to paste a token, a \
         password or a browser cookie.",
    )
}

impl ConnectorDiagnostics {
    /// Render the panel for a persisted status.
    #[must_use]
    pub fn render(status: &ConnectorStatus, table: &CliCompatibilityTable) -> Self {
        let (headline, detail, next_action) = copy_for(status, table);
        let mut warnings = Vec::new();
        if status.scraping_warning {
            warnings.push(SCRAPING_WARNING);
        }
        if status.listing_possibly_incomplete {
            warnings.push(INCOMPLETE_LISTING_WARNING);
        }
        Self {
            state: status.state,
            headline,
            detail,
            next_action,
            detected_version: status.detected_version.clone(),
            compatibility: compatibility_word(status),
            program_origin: status.program_origin.clone(),
            last_error: status.last_error,
            warnings,
        }
    }

    /// Render the panel for a classified error in a given state.
    #[must_use]
    pub fn for_error(
        status: &ConnectorStatus,
        error: SourceError,
        table: &CliCompatibilityTable,
    ) -> Self {
        let mut panel = Self::render(status, table);
        let (headline, detail, next_action) = error_copy(&error, table);
        headline.clone_into(&mut panel.headline);
        detail.clone_into(&mut panel.detail);
        panel.next_action = next_action;
        panel.last_error = Some(plaudon_domain::error::AppError::from(error).code());
        panel
    }
}

const fn compatibility_word(status: &ConnectorStatus) -> &'static str {
    if status.detected_version.is_none() {
        "unknown"
    } else if status.version_validated {
        "tested"
    } else {
        "untested"
    }
}

fn copy_for(
    status: &ConnectorStatus,
    table: &CliCompatibilityTable,
) -> (String, String, NextAction) {
    match status.state {
        ConnectorState::Unconfigured => (
            "PLAUD account not connected".to_owned(),
            "We could not find the official PLAUD command line tool on this computer. Install it \
             from PLAUD's official documentation, then connect again. You can import recordings \
             by hand at any time without it."
                .to_owned(),
            NextAction::InstallCli,
        ),
        ConnectorState::IncompatibleVersion => (
            "This version of the PLAUD tool has not been tested".to_owned(),
            incompatible_detail(status, table),
            NextAction::InstallTestedVersion {
                usable_versions: table
                    .usable_versions()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
        ),
        ConnectorState::CliDetected => (
            "PLAUD tool found".to_owned(),
            "The official PLAUD command line tool is installed and its version is one we have \
             tested. Sign in to list your recordings."
                .to_owned(),
            NextAction::SignIn,
        ),
        ConnectorState::AuthRequired => (
            "Sign in to PLAUD".to_owned(),
            "The PLAUD command line tool needs you to sign in through your browser. We never see \
             your PLAUD password, and we never store a PLAUD session token."
                .to_owned(),
            NextAction::SignIn,
        ),
        ConnectorState::ExpiredAuth => (
            "Your PLAUD session expired".to_owned(),
            "The PLAUD command line tool reports that its session is no longer valid. Sign in \
             again to continue. Recordings you already imported stay in your library."
                .to_owned(),
            NextAction::SignIn,
        ),
        ConnectorState::Authenticated => (
            "Connected to PLAUD".to_owned(),
            "The PLAUD command line tool is signed in. Refresh to list the recordings in your \
             account."
                .to_owned(),
            NextAction::None,
        ),
        ConnectorState::Enumerating => (
            "Listing your PLAUD recordings".to_owned(),
            "We are asking the PLAUD command line tool for the recordings in your account."
                .to_owned(),
            NextAction::None,
        ),
        ConnectorState::Ready => (
            "Connected to PLAUD".to_owned(),
            "Your PLAUD recordings are listed. Select the ones you want to import; nothing is \
             downloaded until you ask for it."
                .to_owned(),
            NextAction::None,
        ),
        ConnectorState::Error => (
            "The PLAUD connector stopped".to_owned(),
            "The last attempt did not finish. Try again; if it keeps failing, import the files \
             by hand from a PLAUD export."
                .to_owned(),
            NextAction::Retry,
        ),
    }
}

fn incompatible_detail(status: &ConnectorStatus, table: &CliCompatibilityTable) -> String {
    let usable = table.usable_versions();
    match (status.detected_version.as_deref(), usable.is_empty()) {
        (Some(version), true) => format!(
            "The installed tool reports version {version}. No release of the official PLAUD \
             command line tool has been tested against this build yet, so we will not read its \
             output: a mis-read listing could import the wrong file. Import recordings by hand \
             from a PLAUD export in the meantime."
        ),
        (Some(version), false) => format!(
            "The installed tool reports version {version}, which we have not tested. Install one \
             of the tested versions, or import recordings by hand from a PLAUD export."
        ),
        (None, _) => "We could not read a version from the installed tool, so we will not drive \
             it. Import recordings by hand from a PLAUD export in the meantime."
            .to_owned(),
    }
}

fn error_copy(
    error: &SourceError,
    table: &CliCompatibilityTable,
) -> (&'static str, &'static str, NextAction) {
    match error {
        SourceError::CliMissing => (
            "PLAUD command line tool not found",
            "We could not find the official PLAUD command line tool on this computer. Install it \
             from PLAUD's official documentation, then connect again.",
            NextAction::InstallCli,
        ),
        SourceError::CliUnsupported => (
            "This version of the PLAUD tool has not been tested",
            "We only drive versions of the official tool whose output we have tested, so that a \
             mis-read listing cannot import the wrong file. Import recordings by hand in the \
             meantime.",
            NextAction::InstallTestedVersion {
                usable_versions: table
                    .usable_versions()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
        ),
        SourceError::AuthenticationRequired => (
            "Sign in to PLAUD",
            "The PLAUD command line tool needs you to sign in through your browser before it can \
             list recordings.",
            NextAction::SignIn,
        ),
        SourceError::AuthCanceled => (
            "Sign-in was cancelled",
            "The browser sign-in did not finish. Start it again when you are ready.",
            NextAction::SignIn,
        ),
        SourceError::SourceUnavailable => (
            "PLAUD is not reachable right now",
            "We could not complete the request. This is usually a temporary network problem. If \
             Local-only Mode is on, the PLAUD connector is switched off on purpose and no \
             request was made.",
            NextAction::ReviewLocalOnlyMode,
        ),
        SourceError::RemoteRateLimited => (
            "PLAUD asked us to slow down",
            "The PLAUD service is rate limiting requests. Wait a few minutes and try again.",
            NextAction::WaitAndRetry,
        ),
        SourceError::LinkExpired => (
            "The download link expired",
            "PLAUD download links are temporary. We asked for a new one and it expired too; try \
             the import again.",
            NextAction::Retry,
        ),
        SourceError::AssetNotFound => (
            "That recording is no longer in your PLAUD account",
            "The recording was listed earlier but is not available now. It may have been deleted \
             or moved to another account. Anything you already imported stays in your library.",
            NextAction::CheckAccountAndRegion,
        ),
        SourceError::MalformedOutput => (
            "We could not read the PLAUD tool's response",
            "The output did not match the format we tested for this version. We stopped rather \
             than guess. Import recordings by hand from a PLAUD export in the meantime.",
            NextAction::ImportManually,
        ),
        SourceError::DownloadInterrupted => (
            "The download did not finish",
            "The transfer stopped before all of the audio arrived, so nothing was added to your \
             library. Try the import again.",
            NextAction::Retry,
        ),
        SourceError::IntegrityFailed => (
            "The downloaded file failed our checks",
            "What arrived was not the audio we expected, so we discarded it rather than add it \
             to your library. Try the import again.",
            NextAction::Retry,
        ),
        SourceError::Cancelled => (
            "Import cancelled",
            "You stopped the import. Nothing was added to your library.",
            NextAction::None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ConnectorEvent;

    fn status(state: ConnectorState) -> ConnectorStatus {
        let mut status = ConnectorStatus::unconfigured(0);
        status.state = state;
        status
    }

    fn table() -> &'static CliCompatibilityTable {
        CliCompatibilityTable::committed()
    }

    const ALL_STATES: [ConnectorState; 9] = [
        ConnectorState::Unconfigured,
        ConnectorState::CliDetected,
        ConnectorState::AuthRequired,
        ConnectorState::Authenticated,
        ConnectorState::Enumerating,
        ConnectorState::Ready,
        ConnectorState::ExpiredAuth,
        ConnectorState::IncompatibleVersion,
        ConnectorState::Error,
    ];

    const ALL_ERRORS: [SourceError; 12] = [
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

    #[test]
    fn every_state_renders_non_empty_actionable_copy() {
        for state in ALL_STATES {
            let panel = ConnectorDiagnostics::render(&status(state), table());
            assert!(!panel.headline.is_empty(), "{state:?}");
            assert!(panel.detail.len() > 40, "{state:?} detail is too thin");
            assert_eq!(panel.state, state);
        }
    }

    #[test]
    fn every_spec_7_6_error_variant_renders_copy() {
        for error in ALL_ERRORS {
            let panel = ConnectorDiagnostics::for_error(
                &status(ConnectorState::Ready),
                error.clone(),
                table(),
            );
            assert!(!panel.headline.is_empty(), "{error:?}");
            assert!(panel.detail.len() > 40, "{error:?}");
            assert!(panel.last_error.is_some(), "{error:?}");
        }
    }

    #[test]
    fn no_rendered_copy_contains_a_vendor_payload_shape() {
        // Spec 7.6: no raw vendor text. Our copy is static, so the strongest
        // available assertion is that nothing resembling a payload, a URL, a
        // token or a stack frame can appear.
        let mut rendered: Vec<String> = Vec::new();
        for state in ALL_STATES {
            let panel = ConnectorDiagnostics::render(&status(state), table());
            rendered.push(panel.headline);
            rendered.push(panel.detail);
        }
        for error in ALL_ERRORS {
            let panel =
                ConnectorDiagnostics::for_error(&status(ConnectorState::Ready), error, table());
            rendered.push(panel.headline);
            rendered.push(panel.detail);
        }
        rendered.push(empty_listing_advisory().1.to_owned());
        for text in rendered {
            for leak in [
                "http://",
                "https://",
                "stderr",
                "exit code",
                "Traceback",
                "Error:",
                "sk-",
            ] {
                assert!(!text.contains(leak), "{text:?} contains {leak:?}");
            }
        }
    }

    #[test]
    fn the_empty_listing_advisory_never_asks_for_a_credential() {
        let (headline, body) = empty_listing_advisory();
        assert_eq!(headline, "No recordings found");
        let lower = body.to_ascii_lowercase();
        assert!(lower.contains("region"));
        assert!(lower.contains("account"));
        // It must say we will never ask, and must not instruct the user to
        // supply any of these.
        assert!(lower.contains("never ask you to paste"));
        for forbidden in ["enter your token", "paste your token", "copy the cookie"] {
            assert!(!lower.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn disconnect_copy_is_truthful_about_session_revocation() {
        let only = DisconnectCopy::for_logout_support(false);
        assert_eq!(only, DisconnectCopy::AssociationOnly);
        assert!(only.body().contains("does not sign you out"));
        assert!(!only.label().contains("sign out"));

        let both = DisconnectCopy::for_logout_support(true);
        assert_eq!(both, DisconnectCopy::WithVendorLogout);
        assert!(both.body().contains("sign-out command"));
    }

    #[test]
    fn an_untested_version_names_itself_and_keeps_manual_import_open() {
        let mut status = status(ConnectorState::IncompatibleVersion);
        status.detected_version = Some("9.9.9".to_owned());
        let panel = ConnectorDiagnostics::render(&status, table());
        assert!(panel.detail.contains("9.9.9"));
        assert!(panel.detail.contains("by hand"));
        assert_eq!(panel.compatibility, "untested");
        assert_eq!(
            panel.next_action,
            NextAction::InstallTestedVersion {
                usable_versions: vec!["0.3.15".to_owned()]
            }
        );
    }

    #[test]
    fn local_only_mode_is_named_when_the_source_is_unavailable() {
        let panel = ConnectorDiagnostics::for_error(
            &status(ConnectorState::Ready),
            SourceError::SourceUnavailable,
            table(),
        );
        assert!(panel.detail.contains("Local-only Mode"));
        assert_eq!(panel.next_action, NextAction::ReviewLocalOnlyMode);
    }

    #[test]
    fn warnings_follow_the_status_flags() {
        let mut status = status(ConnectorState::Ready);
        assert_eq!(
            ConnectorDiagnostics::render(&status, table()).warnings,
            Vec::<&str>::new()
        );
        status.scraping_warning = true;
        status.listing_possibly_incomplete = true;
        let panel = ConnectorDiagnostics::render(&status, table());
        assert_eq!(panel.warnings.len(), 2);
        assert!(panel.warnings.contains(&SCRAPING_WARNING));
        assert!(panel.warnings.contains(&INCOMPLETE_LISTING_WARNING));
    }

    #[test]
    fn disconnecting_clears_the_detected_version_and_warnings() {
        let mut status = ConnectorStatus::unconfigured(0);
        status.state = ConnectorState::Ready;
        status.detected_version = Some("1.4.2".to_owned());
        status.version_validated = true;
        status.scraping_warning = true;
        status.listing_possibly_incomplete = true;
        status.program_origin = Some(
            crate::discovery::DiscoveryOrigin::WellKnownLocation
                .label()
                .to_owned(),
        );

        let after = status.apply(ConnectorEvent::Disconnected, 10);
        assert_eq!(after.state, ConnectorState::Unconfigured);
        assert_eq!(after.detected_version, None);
        assert!(!after.version_validated);
        assert!(!after.scraping_warning);
        assert!(!after.listing_possibly_incomplete);
        assert_eq!(after.program_origin, None);
        assert_eq!(after.updated_at_ms, 10);
    }

    #[test]
    fn the_status_payload_version_is_checked_before_use() {
        let mut status = ConnectorStatus::unconfigured(0);
        assert!(status.is_supported_payload());
        status.payload_version = 99;
        assert!(!status.is_supported_payload());
    }
}
