//! The connector state machine of spec 7.1.
//!
//! `UNCONFIGURED -> CLI_DETECTED -> AUTH_REQUIRED -> AUTHENTICATED ->
//! ENUMERATING -> READY`, plus `ERROR`, `EXPIRED_AUTH` and
//! `INCOMPATIBLE_VERSION`. Transitions are total and explicit: an event that
//! does not apply leaves the state unchanged rather than silently advancing.

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_recording_sources::source::SourceError;
use serde::{Deserialize, Serialize};

/// Connector lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorState {
    /// No CLI has been located.
    Unconfigured,
    /// An executable was located and its version read.
    CliDetected,
    /// The CLI reports that an interactive login is needed.
    AuthRequired,
    /// The CLI reports an authenticated session.
    Authenticated,
    /// An enumeration is in flight.
    Enumerating,
    /// Enumeration succeeded and a listing is available.
    Ready,
    /// A previously valid session expired.
    ExpiredAuth,
    /// The detected CLI version is outside the validated table.
    IncompatibleVersion,
    /// A failure that is not one of the specific states above.
    Error,
}

/// Something that happened to the connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorEvent {
    /// A CLI executable was located and its version is in the validated table.
    CliFound,
    /// A CLI executable was located but its version is not validated.
    CliVersionUnvalidated,
    /// No CLI executable could be located.
    CliMissing,
    /// The CLI says a login is required.
    LoginRequired,
    /// A login completed.
    LoginSucceeded,
    /// The user cancelled the login.
    LoginCanceled,
    /// A previously valid session expired.
    SessionExpired,
    /// An enumeration started.
    EnumerationStarted,
    /// An enumeration produced a listing.
    EnumerationSucceeded,
    /// An enumeration failed for a reason other than authentication.
    EnumerationFailed,
    /// The user disconnected the connector.
    Disconnected,
}

impl ConnectorState {
    /// Apply an event.
    ///
    /// Unapplicable events are ignored rather than forcing a state, which is
    /// why there is no `_ => Error` arm: a stray event must not look like a
    /// failure.
    #[must_use]
    pub const fn apply(self, event: ConnectorEvent) -> Self {
        match (self, event) {
            // Disconnecting and losing the executable both return the
            // connector to its initial state.
            (_, ConnectorEvent::Disconnected | ConnectorEvent::CliMissing) => Self::Unconfigured,
            (_, ConnectorEvent::CliVersionUnvalidated) => Self::IncompatibleVersion,

            (
                Self::Unconfigured | Self::IncompatibleVersion | Self::Error,
                ConnectorEvent::CliFound,
            ) => Self::CliDetected,

            (
                Self::CliDetected | Self::Authenticated | Self::Ready | Self::Enumerating,
                ConnectorEvent::LoginRequired,
            ) => Self::AuthRequired,
            // Spec 4.2 step 3 starts the browser login right after detection,
            // without an intervening "you must sign in" answer from the tool,
            // so `CliDetected` is a legitimate origin for both outcomes.
            (
                Self::CliDetected | Self::AuthRequired | Self::ExpiredAuth,
                ConnectorEvent::LoginSucceeded,
            ) => Self::Authenticated,
            (
                Self::CliDetected | Self::AuthRequired | Self::ExpiredAuth,
                ConnectorEvent::LoginCanceled,
            ) => Self::AuthRequired,
            (
                Self::CliDetected | Self::Authenticated | Self::Ready | Self::Enumerating,
                ConnectorEvent::SessionExpired,
            ) => Self::ExpiredAuth,

            // Enumeration may start straight after detection: running `files`
            // is how we learn whether the vendor tool still has a session, and
            // spec 4.2 does not give us a separate "am I signed in" query.
            (
                Self::CliDetected | Self::Authenticated | Self::Ready,
                ConnectorEvent::EnumerationStarted,
            ) => Self::Enumerating,
            (Self::Enumerating, ConnectorEvent::EnumerationSucceeded) => Self::Ready,
            (Self::Enumerating, ConnectorEvent::EnumerationFailed) => Self::Error,

            (state, _) => state,
        }
    }

    /// Whether enumeration may be attempted from this state.
    #[must_use]
    pub const fn may_enumerate(self) -> bool {
        matches!(self, Self::CliDetected | Self::Authenticated | Self::Ready)
    }

    /// Whether the connector needs the user before it can progress.
    #[must_use]
    pub const fn needs_user_action(self) -> bool {
        matches!(
            self,
            Self::Unconfigured | Self::AuthRequired | Self::ExpiredAuth | Self::IncompatibleVersion
        )
    }

    /// The stable source error this state reports, if any.
    #[must_use]
    pub const fn as_source_error(self) -> Option<SourceError> {
        match self {
            Self::Unconfigured => Some(SourceError::CliMissing),
            Self::IncompatibleVersion => Some(SourceError::CliUnsupported),
            Self::AuthRequired | Self::ExpiredAuth => Some(SourceError::AuthenticationRequired),
            Self::Error => Some(SourceError::SourceUnavailable),
            Self::CliDetected | Self::Authenticated | Self::Enumerating | Self::Ready => None,
        }
    }
}

/// The persisted, UI-observable connector status (spec 7.1, 4.2, 15.3).
///
/// Everything here is either our own text or a stable code. There is no field
/// that can carry vendor stdout or stderr, which is how spec 7.6's "we will
/// not leak raw vendor error payloads" is enforced for the status surface
/// rather than merely intended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorStatus {
    /// Contract version of this payload.
    pub payload_version: u32,
    /// Lifecycle state.
    pub state: ConnectorState,
    /// Version string the tool reported, when it was readable.
    pub detected_version: Option<String>,
    /// Whether the detected version is in the committed compatibility table.
    pub version_validated: bool,
    /// Where the executable was found, as a stable label.
    pub program_origin: Option<String>,
    /// Last stable error, as a code only.
    pub last_error: Option<AppErrorCode>,
    /// Whether the last listing may be a partial view of the account.
    pub listing_possibly_incomplete: bool,
    /// Whether the active parser scrapes a human-readable layout (spec 7.2).
    pub scraping_warning: bool,
    /// When this status was last written, in milliseconds since the epoch.
    pub updated_at_ms: i64,
}

impl ConnectorStatus {
    /// Contract version understood by this build.
    pub const SUPPORTED_PAYLOAD_VERSION: u32 = 1;

    /// Preference key the status is persisted under.
    pub const PREFERENCE_KEY: &'static str = "source.plaud_cloud_cli.status";

    /// The status of a connector nobody has configured yet.
    #[must_use]
    pub const fn unconfigured(at_ms: i64) -> Self {
        Self {
            payload_version: Self::SUPPORTED_PAYLOAD_VERSION,
            state: ConnectorState::Unconfigured,
            detected_version: None,
            version_validated: false,
            program_origin: None,
            last_error: None,
            listing_possibly_incomplete: false,
            scraping_warning: false,
            updated_at_ms: at_ms,
        }
    }

    /// Apply an event and record when it happened.
    #[must_use]
    pub fn apply(mut self, event: ConnectorEvent, at_ms: i64) -> Self {
        self.state = self.state.apply(event);
        self.updated_at_ms = at_ms;
        if matches!(
            event,
            ConnectorEvent::Disconnected | ConnectorEvent::CliMissing
        ) {
            self.detected_version = None;
            self.version_validated = false;
            self.program_origin = None;
            self.listing_possibly_incomplete = false;
            self.scraping_warning = false;
        }
        self.last_error = self
            .state
            .as_source_error()
            .map(|error| AppError::from(error).code());
        self
    }

    /// Record a classified failure without changing the lifecycle state.
    #[must_use]
    pub fn with_error(mut self, error: SourceError, at_ms: i64) -> Self {
        self.last_error = Some(AppError::from(error).code());
        self.updated_at_ms = at_ms;
        self
    }

    /// Whether a payload decoded from storage is usable by this build.
    #[must_use]
    pub const fn is_supported_payload(&self) -> bool {
        self.payload_version == Self::SUPPORTED_PAYLOAD_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_in_straight_after_detection_reaches_authenticated() {
        // Spec 4.2: steps 1 and 3 are adjacent. The tool does not tell us "you
        // must sign in" before we offer the button, so a login outcome has to
        // be accepted from `CliDetected` or the panel would keep asking the
        // user to do something they just did.
        assert_eq!(
            ConnectorState::CliDetected.apply(ConnectorEvent::LoginSucceeded),
            ConnectorState::Authenticated
        );
    }

    #[test]
    fn cancelling_the_first_login_asks_again_rather_than_looking_connected() {
        assert_eq!(
            ConnectorState::CliDetected.apply(ConnectorEvent::LoginCanceled),
            ConnectorState::AuthRequired
        );
        assert_eq!(
            ConnectorState::AuthRequired.apply(ConnectorEvent::LoginCanceled),
            ConnectorState::AuthRequired
        );
        assert_eq!(
            ConnectorState::ExpiredAuth.apply(ConnectorEvent::LoginCanceled),
            ConnectorState::AuthRequired
        );
    }

    #[test]
    fn a_login_outcome_never_resurrects_a_disconnected_or_incompatible_connector() {
        for state in [
            ConnectorState::Unconfigured,
            ConnectorState::IncompatibleVersion,
        ] {
            assert_eq!(state.apply(ConnectorEvent::LoginSucceeded), state);
            assert_eq!(state.apply(ConnectorEvent::LoginCanceled), state);
        }
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

    const ALL_EVENTS: [ConnectorEvent; 11] = [
        ConnectorEvent::CliFound,
        ConnectorEvent::CliVersionUnvalidated,
        ConnectorEvent::CliMissing,
        ConnectorEvent::LoginRequired,
        ConnectorEvent::LoginSucceeded,
        ConnectorEvent::LoginCanceled,
        ConnectorEvent::SessionExpired,
        ConnectorEvent::EnumerationStarted,
        ConnectorEvent::EnumerationSucceeded,
        ConnectorEvent::EnumerationFailed,
        ConnectorEvent::Disconnected,
    ];

    #[test]
    fn the_documented_happy_path_is_reachable_in_order() {
        let state = ConnectorState::Unconfigured
            .apply(ConnectorEvent::CliFound)
            .apply(ConnectorEvent::LoginRequired)
            .apply(ConnectorEvent::LoginSucceeded)
            .apply(ConnectorEvent::EnumerationStarted)
            .apply(ConnectorEvent::EnumerationSucceeded);
        assert_eq!(state, ConnectorState::Ready);
        assert!(state.may_enumerate());
        assert_eq!(state.as_source_error(), None);
    }

    #[test]
    fn an_unvalidated_version_fails_closed_from_every_state() {
        for state in ALL_STATES {
            assert_eq!(
                state.apply(ConnectorEvent::CliVersionUnvalidated),
                ConnectorState::IncompatibleVersion,
                "{state:?}"
            );
        }
        assert!(!ConnectorState::IncompatibleVersion.may_enumerate());
        assert_eq!(
            ConnectorState::IncompatibleVersion.as_source_error(),
            Some(SourceError::CliUnsupported)
        );
    }

    #[test]
    fn an_expired_session_stops_enumeration_and_asks_for_login() {
        let expired = ConnectorState::Ready.apply(ConnectorEvent::SessionExpired);
        assert_eq!(expired, ConnectorState::ExpiredAuth);
        assert!(!expired.may_enumerate());
        assert!(expired.needs_user_action());
        assert_eq!(
            expired.as_source_error(),
            Some(SourceError::AuthenticationRequired)
        );
        assert_eq!(
            expired.apply(ConnectorEvent::LoginSucceeded),
            ConnectorState::Authenticated
        );
    }

    #[test]
    fn a_cancelled_login_does_not_pretend_to_be_authenticated() {
        let state = ConnectorState::AuthRequired.apply(ConnectorEvent::LoginCanceled);
        assert_eq!(state, ConnectorState::AuthRequired);
        assert!(!state.may_enumerate());
    }

    #[test]
    fn a_missing_cli_resets_the_connector_from_any_state() {
        for state in ALL_STATES {
            assert_eq!(
                state.apply(ConnectorEvent::CliMissing),
                ConnectorState::Unconfigured
            );
        }
        assert_eq!(
            ConnectorState::Unconfigured.as_source_error(),
            Some(SourceError::CliMissing)
        );
    }

    #[test]
    fn no_event_can_reach_ready_without_enumerating_first() {
        for state in ALL_STATES {
            for event in ALL_EVENTS {
                let next = state.apply(event);
                if next == ConnectorState::Ready && state != ConnectorState::Ready {
                    assert_eq!(
                        (state, event),
                        (
                            ConnectorState::Enumerating,
                            ConnectorEvent::EnumerationSucceeded
                        ),
                        "unexpected path into Ready"
                    );
                }
            }
        }
    }

    #[test]
    fn transitions_are_total_and_never_panic() {
        for state in ALL_STATES {
            for event in ALL_EVENTS {
                let next = state.apply(event);
                assert!(ALL_STATES.contains(&next));
            }
        }
    }
}
