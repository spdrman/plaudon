//! The connector state machine of spec 7.1.
//!
//! `UNCONFIGURED -> CLI_DETECTED -> AUTH_REQUIRED -> AUTHENTICATED ->
//! ENUMERATING -> READY`, plus `ERROR`, `EXPIRED_AUTH` and
//! `INCOMPATIBLE_VERSION`. Transitions are total and explicit: an event that
//! does not apply leaves the state unchanged rather than silently advancing.

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
            (Self::AuthRequired | Self::ExpiredAuth, ConnectorEvent::LoginSucceeded) => {
                Self::Authenticated
            }
            (Self::AuthRequired | Self::ExpiredAuth, ConnectorEvent::LoginCanceled) => {
                Self::AuthRequired
            }
            (
                Self::Authenticated | Self::Ready | Self::Enumerating,
                ConnectorEvent::SessionExpired,
            ) => Self::ExpiredAuth,

            (Self::Authenticated | Self::Ready, ConnectorEvent::EnumerationStarted) => {
                Self::Enumerating
            }
            (Self::Enumerating, ConnectorEvent::EnumerationSucceeded) => Self::Ready,
            (Self::Enumerating, ConnectorEvent::EnumerationFailed) => Self::Error,

            (state, _) => state,
        }
    }

    /// Whether enumeration may be attempted from this state.
    #[must_use]
    pub const fn may_enumerate(self) -> bool {
        matches!(self, Self::Authenticated | Self::Ready)
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

#[cfg(test)]
mod tests {
    use super::*;

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
