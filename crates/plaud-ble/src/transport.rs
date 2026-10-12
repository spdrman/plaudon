//! The GATT transport contract the device actor drives (spec §7.7, App. B.4).
//!
//! # Why writes take a frame, not bytes
//!
//! [`GattTransport::write_control`] accepts a [`ControlFrame`], which can only
//! be built from a [`ReadOnlyOpcode`](plaudon_plaud_protocol::ReadOnlyOpcode).
//! There is no byte-oriented write on this trait, so neither the actor nor a
//! future transport implementation can emit a delete, clear, settings-write,
//! OTA or sync-acknowledgement command: the type system has no way to express
//! one (spec §7.9).
//!
//! # Why characteristics are discovered, not assumed
//!
//! [`Characteristics`] is produced by discovery and carries UUID-resolved
//! handles. Spec App. B.3: "We will not rely on fixed physical ATT handles;
//! characteristic UUID discovery is required."

use std::time::Duration;

use async_trait::async_trait;
use plaudon_plaud_protocol::frame::ControlFrame;
use plaudon_plaud_protocol::profile::ProtocolProfile;
use plaudon_recording_sources::device::DeviceError;
use serde::{Deserialize, Serialize};

/// Characteristics resolved by UUID discovery on one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Characteristics {
    /// The vendor control characteristic. The only writable one.
    pub control: u16,
    /// The vendor notification characteristic.
    pub notify: u16,
    /// The standard battery level characteristic, when present.
    pub battery: Option<u16>,
}

impl Characteristics {
    /// Check a discovery result against the profile it claims to implement.
    ///
    /// A peer that offers the vendor service but maps the control or
    /// notification characteristic elsewhere is refused rather than
    /// accommodated.
    pub fn validate(&self, profile: &ProtocolProfile) -> Result<(), DeviceError> {
        if self.control != profile.characteristic_control
            || self.notify != profile.characteristic_notify
        {
            return Err(DeviceError::UnsupportedFirmware);
        }
        Ok(())
    }

    /// Whether writing to `characteristic` is permitted.
    #[must_use]
    pub fn permits_write(&self, characteristic: u16) -> bool {
        characteristic == self.control
    }
}

/// What a probe established about a connected peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    /// Model the device reported, when it reports one.
    pub device_model: Option<String>,
    /// Firmware fingerprint the device reported, when it reports one.
    pub firmware: Option<String>,
    /// Characteristics resolved by discovery.
    pub characteristics: Characteristics,
    /// Battery percentage, when the standard characteristic is present.
    pub battery_percent: Option<u8>,
}

/// A link-level transport over one connected peripheral.
///
/// Implementations are single-owner: the actor holds one and never shares it.
#[async_trait]
pub trait GattTransport: Send {
    /// Resolve the vendor characteristics by UUID.
    async fn discover(&mut self) -> Result<ProbeResult, DeviceError>;

    /// Write one allowlisted control frame.
    async fn write_control(&mut self, frame: &ControlFrame) -> Result<(), DeviceError>;

    /// Await the next notification payload.
    ///
    /// Returns [`DeviceError::DeviceDisconnected`] when the link drops and
    /// [`DeviceError::TransferIncomplete`] when nothing arrives in time, so a
    /// silent device is never mistaken for a finished transfer.
    async fn next_notification(&mut self, timeout: Duration) -> Result<Vec<u8>, DeviceError>;

    /// Drop the link. Idempotent.
    async fn disconnect(&mut self) -> Result<(), DeviceError>;
}

/// Progress reported by the actor to the job engine and the interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DeviceEvent {
    /// The link is up and characteristics were resolved.
    Connected {
        /// What the probe established.
        probe: Box<ProbeResult>,
    },
    /// Authentication succeeded.
    Authenticated,
    /// A listing arrived.
    Listed {
        /// Entries reported.
        count: usize,
        /// Whether the device said the listing was truncated.
        truncated: bool,
        /// Whether this profile's listing is a delta of unsynchronized files.
        delta_only: bool,
    },
    /// Bytes arrived.
    TransferProgress(TransferProgress),
    /// The terminal marker arrived.
    TransferEnded {
        /// Bytes received in total.
        received_bytes: u64,
    },
    /// The link dropped.
    Disconnected,
}

/// A transfer progress sample.
///
/// Spec App. B.4: "Transfer progress is based on verified received bytes; ETA
/// requires a measured current transfer rate and is shown as an estimate."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferProgress {
    /// Bytes written to staging so far.
    pub received_bytes: u64,
    /// Length the device declared, when it declared one.
    pub declared_bytes: Option<u64>,
    /// Measured throughput, when enough samples exist.
    pub bytes_per_second: Option<u64>,
    /// Seconds remaining at the measured rate. Always an estimate.
    pub eta_seconds: Option<u64>,
}

impl TransferProgress {
    /// Build a sample from measured values.
    ///
    /// An ETA is produced only from a measured rate and a declared length;
    /// with either missing it is `None` rather than a guess.
    #[must_use]
    pub fn new(received_bytes: u64, declared_bytes: Option<u64>, elapsed: Duration) -> Self {
        let seconds = elapsed.as_secs_f64();
        let bytes_per_second = if seconds >= 0.25 && received_bytes > 0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some((received_bytes as f64 / seconds) as u64)
        } else {
            None
        };
        let eta_seconds = match (bytes_per_second, declared_bytes) {
            (Some(rate), Some(declared)) if rate > 0 && declared > received_bytes => {
                Some((declared - received_bytes) / rate)
            }
            _ => None,
        };
        Self {
            received_bytes,
            declared_bytes,
            bytes_per_second,
            eta_seconds,
        }
    }

    /// Fraction complete, only when a declared length exists.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        self.declared_bytes.and_then(|declared| {
            (declared > 0).then(|| (self.received_bytes as f64 / declared as f64).min(1.0))
        })
    }

    /// Whether the ETA is an estimate. It always is.
    #[must_use]
    pub const fn eta_is_estimate(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;

    use super::*;

    #[test]
    fn discovery_must_match_the_profile_it_claims() {
        let correct = Characteristics {
            control: LEGACY_NOTE_V0095.characteristic_control,
            notify: LEGACY_NOTE_V0095.characteristic_notify,
            battery: Some(LEGACY_NOTE_V0095.characteristic_battery),
        };
        correct.validate(&LEGACY_NOTE_V0095).unwrap();

        for wrong in [
            Characteristics {
                control: 0x2BB2,
                ..correct
            },
            Characteristics {
                notify: 0x2BB3,
                ..correct
            },
        ] {
            assert_eq!(
                wrong.validate(&LEGACY_NOTE_V0095),
                Err(DeviceError::UnsupportedFirmware)
            );
        }
    }

    #[test]
    fn only_the_control_characteristic_is_writable() {
        let characteristics = Characteristics {
            control: 0x2BB1,
            notify: 0x2BB0,
            battery: Some(0x2A19),
        };
        assert!(characteristics.permits_write(0x2BB1));
        for read_only in [0x2BB0, 0x2A19, 0x1910] {
            assert!(
                !characteristics.permits_write(read_only),
                "{read_only:#06x}"
            );
        }
    }

    #[test]
    fn an_eta_needs_both_a_measured_rate_and_a_declared_length() {
        let measured = TransferProgress::new(10_000, Some(50_000), Duration::from_secs(2));
        assert_eq!(measured.bytes_per_second, Some(5_000));
        assert_eq!(measured.eta_seconds, Some(8));
        assert!(measured.eta_is_estimate());

        let undeclared = TransferProgress::new(10_000, None, Duration::from_secs(2));
        assert_eq!(undeclared.bytes_per_second, Some(5_000));
        assert_eq!(undeclared.eta_seconds, None, "no length, no estimate");
        assert_eq!(undeclared.fraction(), None);

        let too_early = TransferProgress::new(100, Some(50_000), Duration::from_millis(10));
        assert_eq!(too_early.bytes_per_second, None, "no rate from one sample");
        assert_eq!(too_early.eta_seconds, None);
    }

    #[test]
    fn progress_fraction_is_bounded_and_absent_without_a_declared_length() {
        let over = TransferProgress::new(60_000, Some(50_000), Duration::from_secs(2));
        assert_eq!(over.fraction(), Some(1.0));
        assert_eq!(
            TransferProgress::new(1, Some(0), Duration::from_secs(1)).fraction(),
            None
        );
    }
}
