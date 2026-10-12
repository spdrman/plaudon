//! The spec 7.9 `DeviceSession` state machine driver.
//!
//! The state enum itself lives in `plaudon-recording-sources` so the rest of
//! the application can reason about a device session without depending on a
//! radio crate. This module owns the transitions.

use plaudon_recording_sources::device::{AuthScheme, DeviceError, DeviceSessionState};
use serde::{Deserialize, Serialize};

/// Something that happened to a device session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A candidate advertisement was seen.
    Discovered,
    /// The user asked us to probe a candidate.
    ProbeRequested,
    /// A probe established model and firmware.
    ProfileIdentified {
        /// Whether the observed firmware is in a validated compatibility cell.
        firmware_validated: bool,
        /// Authentication scheme the device offered.
        auth_scheme: AuthScheme,
    },
    /// The operating system denied radio access.
    PermissionDenied,
    /// A stored credential authenticated successfully.
    Authenticated,
    /// The device rejected our credential.
    AuthRejected,
    /// Enumeration started.
    EnumerationStarted,
    /// Enumeration produced a listing.
    EnumerationSucceeded,
    /// A transfer started.
    TransferStarted,
    /// A transfer delivered its terminal marker.
    TransferEnded,
    /// Verification of the transferred bytes succeeded.
    Verified,
    /// Verification of the transferred bytes failed.
    VerificationFailed,
    /// Durable commit succeeded.
    Committed,
    /// The link dropped.
    Disconnected,
    /// The device is busy with another host.
    Busy,
}

/// Drives a single device session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDriver {
    state: DeviceSessionState,
}

impl Default for SessionDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionDriver {
    /// A session that has not seen anything yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: DeviceSessionState::Discovered,
        }
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> DeviceSessionState {
        self.state
    }

    /// Whether a read frame may be sent right now.
    #[must_use]
    pub const fn may_read(&self) -> bool {
        self.state.permits_read()
    }

    /// Apply an event.
    ///
    /// Terminal failure states absorb everything except a fresh discovery, so
    /// an `UnsupportedProfile` device cannot be talked into a transfer by a
    /// later event.
    pub fn apply(&mut self, event: &SessionEvent) -> DeviceSessionState {
        use DeviceSessionState as S;

        if self.state.is_terminal_failure() && *event != SessionEvent::Discovered {
            return self.state;
        }

        self.state = match (self.state, event) {
            (_, SessionEvent::Discovered) => S::Discovered,
            (_, SessionEvent::PermissionDenied) => S::PermissionDenied,
            (_, SessionEvent::Disconnected) => S::Disconnected,
            (_, SessionEvent::Busy) => S::Busy,

            (S::Discovered | S::Disconnected | S::Busy, SessionEvent::ProbeRequested) => S::Probing,

            (
                S::Probing,
                SessionEvent::ProfileIdentified {
                    firmware_validated,
                    auth_scheme,
                },
            ) => {
                if !firmware_validated {
                    S::UnsupportedProfile
                } else if !auth_scheme.is_implemented() {
                    S::UnsupportedAuth
                } else if *auth_scheme == AuthScheme::None {
                    S::Authenticated
                } else {
                    S::NeedsProvisioning
                }
            }

            (S::NeedsProvisioning | S::Probing, SessionEvent::Authenticated) => S::Authenticated,
            (_, SessionEvent::AuthRejected) => S::UnsupportedAuth,

            (S::Authenticated | S::Available, SessionEvent::EnumerationStarted) => S::Enumerating,
            (S::Enumerating, SessionEvent::EnumerationSucceeded) => S::Available,

            (S::Available, SessionEvent::TransferStarted) => S::Transferring,
            (S::Transferring, SessionEvent::TransferEnded) => S::Verifying,
            (S::Verifying, SessionEvent::Verified) => S::DurableCommit,
            (S::Verifying, SessionEvent::VerificationFailed) => S::IntegrityFailed,
            (S::DurableCommit, SessionEvent::Committed) => S::Imported,

            (state, _) => state,
        };
        self.state
    }

    /// The device error this state reports, if any.
    #[must_use]
    pub const fn as_device_error(&self) -> Option<DeviceError> {
        match self.state {
            DeviceSessionState::PermissionDenied => Some(DeviceError::RadioPermissionDenied),
            DeviceSessionState::UnsupportedProfile => Some(DeviceError::UnsupportedFirmware),
            DeviceSessionState::UnsupportedAuth => Some(DeviceError::AuthRejected),
            DeviceSessionState::NeedsProvisioning => Some(DeviceError::AuthBootstrapRequired),
            DeviceSessionState::Busy => Some(DeviceError::DeviceBusy),
            DeviceSessionState::Disconnected => Some(DeviceError::DeviceDisconnected),
            DeviceSessionState::IntegrityFailed => Some(DeviceError::IntegrityFailed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_profile() -> SessionEvent {
        SessionEvent::ProfileIdentified {
            firmware_validated: true,
            auth_scheme: AuthScheme::LegacyStaticToken,
        }
    }

    #[test]
    fn the_documented_happy_path_reaches_imported() {
        let mut driver = SessionDriver::new();
        driver.apply(&SessionEvent::ProbeRequested);
        assert_eq!(
            driver.apply(&legacy_profile()),
            DeviceSessionState::NeedsProvisioning
        );
        assert_eq!(
            driver.apply(&SessionEvent::Authenticated),
            DeviceSessionState::Authenticated
        );
        driver.apply(&SessionEvent::EnumerationStarted);
        assert_eq!(
            driver.apply(&SessionEvent::EnumerationSucceeded),
            DeviceSessionState::Available
        );
        assert!(driver.may_read());
        driver.apply(&SessionEvent::TransferStarted);
        driver.apply(&SessionEvent::TransferEnded);
        driver.apply(&SessionEvent::Verified);
        assert_eq!(
            driver.apply(&SessionEvent::Committed),
            DeviceSessionState::Imported
        );
        assert_eq!(driver.as_device_error(), None);
    }

    #[test]
    fn an_unvalidated_firmware_fails_closed_and_stays_closed() {
        let mut driver = SessionDriver::new();
        driver.apply(&SessionEvent::ProbeRequested);
        let state = driver.apply(&SessionEvent::ProfileIdentified {
            firmware_validated: false,
            auth_scheme: AuthScheme::LegacyStaticToken,
        });
        assert_eq!(state, DeviceSessionState::UnsupportedProfile);
        assert!(!driver.may_read());
        assert_eq!(
            driver.as_device_error(),
            Some(DeviceError::UnsupportedFirmware)
        );

        // No later event can talk it into a transfer.
        for event in [
            SessionEvent::Authenticated,
            SessionEvent::EnumerationStarted,
            SessionEvent::EnumerationSucceeded,
            SessionEvent::TransferStarted,
        ] {
            assert_eq!(driver.apply(&event), DeviceSessionState::UnsupportedProfile);
            assert!(!driver.may_read());
        }
    }

    #[test]
    fn an_encrypted_mode_offer_is_refused_rather_than_downgraded() {
        let mut driver = SessionDriver::new();
        driver.apply(&SessionEvent::ProbeRequested);
        let state = driver.apply(&SessionEvent::ProfileIdentified {
            firmware_validated: true,
            auth_scheme: AuthScheme::EncryptedHandshake,
        });
        assert_eq!(state, DeviceSessionState::UnsupportedAuth);
        assert!(!driver.may_read());

        // Explicitly: there is no path back to the legacy scheme.
        assert_eq!(
            driver.apply(&legacy_profile()),
            DeviceSessionState::UnsupportedAuth
        );
    }

    #[test]
    fn an_unknown_auth_scheme_also_fails_closed() {
        let mut driver = SessionDriver::new();
        driver.apply(&SessionEvent::ProbeRequested);
        assert_eq!(
            driver.apply(&SessionEvent::ProfileIdentified {
                firmware_validated: true,
                auth_scheme: AuthScheme::Unknown,
            }),
            DeviceSessionState::UnsupportedAuth
        );
    }

    #[test]
    fn a_denied_radio_permission_is_terminal_until_rediscovery() {
        let mut driver = SessionDriver::new();
        assert_eq!(
            driver.apply(&SessionEvent::PermissionDenied),
            DeviceSessionState::PermissionDenied
        );
        assert_eq!(
            driver.as_device_error(),
            Some(DeviceError::RadioPermissionDenied)
        );
        assert_eq!(
            driver.apply(&SessionEvent::ProbeRequested),
            DeviceSessionState::PermissionDenied
        );
        assert_eq!(
            driver.apply(&SessionEvent::Discovered),
            DeviceSessionState::Discovered
        );
    }

    #[test]
    fn a_failed_verification_never_becomes_a_commit() {
        let mut driver = SessionDriver::new();
        driver.apply(&SessionEvent::ProbeRequested);
        driver.apply(&legacy_profile());
        driver.apply(&SessionEvent::Authenticated);
        driver.apply(&SessionEvent::EnumerationStarted);
        driver.apply(&SessionEvent::EnumerationSucceeded);
        driver.apply(&SessionEvent::TransferStarted);
        driver.apply(&SessionEvent::TransferEnded);
        assert_eq!(
            driver.apply(&SessionEvent::VerificationFailed),
            DeviceSessionState::IntegrityFailed
        );
        assert_eq!(
            driver.apply(&SessionEvent::Committed),
            DeviceSessionState::IntegrityFailed
        );
        assert_eq!(driver.as_device_error(), Some(DeviceError::IntegrityFailed));
    }

    #[test]
    fn a_disconnect_mid_transfer_stops_reads_immediately() {
        let mut driver = SessionDriver::new();
        driver.apply(&SessionEvent::ProbeRequested);
        driver.apply(&legacy_profile());
        driver.apply(&SessionEvent::Authenticated);
        driver.apply(&SessionEvent::EnumerationStarted);
        driver.apply(&SessionEvent::EnumerationSucceeded);
        driver.apply(&SessionEvent::TransferStarted);
        assert!(driver.may_read());
        assert_eq!(
            driver.apply(&SessionEvent::Disconnected),
            DeviceSessionState::Disconnected
        );
        assert!(!driver.may_read());
    }

    #[test]
    fn reads_are_impossible_before_enumeration_completes() {
        let mut driver = SessionDriver::new();
        assert!(!driver.may_read());
        driver.apply(&SessionEvent::ProbeRequested);
        assert!(!driver.may_read());
        driver.apply(&legacy_profile());
        assert!(!driver.may_read());
        driver.apply(&SessionEvent::Authenticated);
        assert!(!driver.may_read());
        driver.apply(&SessionEvent::EnumerationStarted);
        assert!(!driver.may_read());
    }
}
