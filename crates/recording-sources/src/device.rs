//! The direct-device contract (spec 6.5, 7.7, 7.9).
//!
//! A device session owns a radio handle that cannot safely be shared across
//! arbitrary workers, so the trait is implemented by a thread-safe *handle* in
//! front of a single-owner actor. Nothing here requires the session state
//! itself to be `Sync`.

use async_trait::async_trait;
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::DeviceRecordingId;
use plaudon_domain::media::{MediaIdentity, QualityClass};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::sink::AsyncWriteSink;

/// Physical transport a device profile uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Bluetooth Low Energy.
    Ble,
    /// A mounted USB mass-storage volume.
    UsbMassStorage,
    /// Vendor Wi-Fi fast transfer.
    Wifi,
    /// A transport this build does not model.
    #[serde(other)]
    Unknown,
}

/// How a device authenticates a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    /// No authentication is required to list or read.
    None,
    /// A static device token presented over an unencrypted link.
    LegacyStaticToken,
    /// An encrypted handshake whose provisioning we do not implement.
    EncryptedHandshake,
    /// A scheme this build does not model.
    #[serde(other)]
    Unknown,
}

impl AuthScheme {
    /// Whether this build can complete the scheme.
    ///
    /// Spec 7.9 fails closed on newer auth modes rather than downgrading.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        matches!(self, Self::None | Self::LegacyStaticToken)
    }
}

/// Whether an interrupted transfer may be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeSemantics {
    /// Offset reads were demonstrated to be correct and repeatable.
    VerifiedOffsets,
    /// Resume is not available; an interrupted transfer restarts from zero.
    RestartOnly,
    /// Resume behaviour has not been established.
    #[serde(other)]
    Unknown,
}

impl ResumeSemantics {
    /// Whether resume may actually be attempted.
    #[must_use]
    pub const fn may_resume(self) -> bool {
        matches!(self, Self::VerifiedOffsets)
    }
}

/// Evidence-backed snapshot of one device profile (spec 6.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    /// Contract version of this payload.
    pub payload_version: u32,
    /// Validation status of this exact compatibility cell.
    pub status: CapabilityStatus,
    /// Vendor name as observed.
    pub vendor: String,
    /// Device model as observed.
    pub model: String,
    /// Hardware generation, when the device reports one.
    pub hardware_generation: Known<String>,
    /// Firmware fingerprint as observed.
    pub observed_firmware: Known<String>,
    /// Transport in use.
    pub transport: Transport,
    /// Authentication scheme the device offered.
    pub auth_scheme: AuthScheme,
    /// Whether read-only listing was demonstrated.
    pub supports_read_only_listing: Tri,
    /// Native media format the device produces.
    pub source_media_format: Known<String>,
    /// Fidelity class of the bytes this transport yields.
    pub quality_class: QualityClass,
    /// Resume behaviour.
    pub resume_semantics: ResumeSemantics,
    /// Host operating system the evidence was gathered on.
    pub host_os: String,
    /// When the evidence was recorded.
    pub validated_at: Known<Timestamp>,
    /// Identifier of the archived evidence run.
    pub test_run_id: Known<String>,
}

impl DeviceCapabilities {
    /// Contract version understood by this build.
    pub const SUPPORTED_PAYLOAD_VERSION: u32 = 1;

    /// A capability snapshot that claims nothing.
    #[must_use]
    pub fn unknown(vendor: impl Into<String>, model: impl Into<String>, host_os: impl Into<String>) -> Self {
        Self {
            payload_version: Self::SUPPORTED_PAYLOAD_VERSION,
            status: CapabilityStatus::Unknown,
            vendor: vendor.into(),
            model: model.into(),
            hardware_generation: Known::Unknown,
            observed_firmware: Known::Unknown,
            transport: Transport::Unknown,
            auth_scheme: AuthScheme::Unknown,
            supports_read_only_listing: Tri::Unknown,
            source_media_format: Known::Unknown,
            quality_class: QualityClass::Unknown,
            resume_semantics: ResumeSemantics::Unknown,
            host_os: host_os.into(),
            validated_at: Known::Unknown,
            test_run_id: Known::Unknown,
        }
    }

    /// Effective status after contract-version negotiation.
    #[must_use]
    pub fn effective_status(&self) -> CapabilityStatus {
        if self.payload_version == Self::SUPPORTED_PAYLOAD_VERSION {
            self.status
        } else {
            CapabilityStatus::Unknown
        }
    }

    /// Whether listing and transfer may be offered for this cell.
    ///
    /// Spec 7.9 requires an archived evidence record before a cell counts as
    /// supported, so a cell without `test_run_id` and `validated_at` can never
    /// be selectable regardless of the status it claims.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        self.effective_status().is_selectable()
            && self.auth_scheme.is_implemented()
            && self.supports_read_only_listing.satisfies()
            && self.observed_firmware.is_known()
            && self.validated_at.is_known()
            && self.test_run_id.is_known()
    }
}

/// One recording as listed by a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecording {
    /// Device-scoped identity.
    pub id: DeviceRecordingId,
    /// Name the device reports. Advisory metadata only.
    pub display_name: String,
    /// Byte length the device reports. Untrusted until verified.
    pub declared_byte_length: Option<u64>,
    /// Capture time the device reports, when available.
    pub captured_at: Option<Timestamp>,
    /// Codec the device reports.
    pub declared_codec: Option<String>,
}

/// A transfer that satisfied every completeness check (spec 7.9).
///
/// Our own digest is not sufficient evidence of completeness: the profile must
/// also supply a terminal marker and, where available, a device-reported
/// length, otherwise the transfer stays unverified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedTransfer {
    /// Device-scoped identity of the transferred recording.
    pub device_recording_id: DeviceRecordingId,
    /// Identity of the bytes we received.
    pub identity: MediaIdentity,
    /// Fidelity class of the received bytes.
    pub quality_class: QualityClass,
    /// Duration decoded from the received bytes, when decodable.
    pub duration_ms: Option<u64>,
    /// Whether the protocol's terminal marker was observed.
    pub terminal_marker_observed: bool,
    /// Byte length the device declared, when the profile provides one.
    pub device_declared_byte_length: Option<u64>,
}

impl VerifiedTransfer {
    /// Whether the transfer may be committed as a complete asset.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.terminal_marker_observed
            && self
                .device_declared_byte_length
                .is_none_or(|declared| declared == self.identity.byte_length)
    }
}

/// Direct-device transfer errors (spec 7.7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    /// The operating system denied radio access.
    #[error("radio permission denied")]
    RadioPermissionDenied,
    /// The device is busy with another host or operation.
    #[error("the device is busy")]
    DeviceBusy,
    /// The link dropped.
    #[error("the device disconnected")]
    DeviceDisconnected,
    /// The observed firmware is outside every validated cell.
    #[error("the device firmware is not supported")]
    UnsupportedFirmware,
    /// The device requires provisioning we do not perform.
    #[error("the device requires an unimplemented provisioning step")]
    AuthBootstrapRequired,
    /// The device rejected our credentials.
    #[error("the device rejected authentication")]
    AuthRejected,
    /// The transfer ended without a verified terminal marker.
    #[error("the transfer did not complete")]
    TransferIncomplete,
    /// The received bytes use a codec we cannot decode.
    #[error("the device media codec is not supported")]
    UnsupportedCodec,
    /// The requested recording is not listed by the device.
    #[error("the recording is not present on the device")]
    NotPresentOnDevice,
    /// The device battery is too low to transfer.
    #[error("the device battery is too low")]
    DevicePowerLow,
    /// The transfer was cancelled.
    #[error("the transfer was cancelled")]
    Cancelled,
    /// Received bytes failed verification.
    #[error("the received bytes failed verification")]
    IntegrityFailed,
}

impl From<DeviceError> for AppError {
    fn from(error: DeviceError) -> Self {
        let code = match error {
            DeviceError::RadioPermissionDenied => AppErrorCode::RadioPermissionDenied,
            DeviceError::DeviceBusy => AppErrorCode::DeviceBusy,
            DeviceError::DeviceDisconnected => AppErrorCode::DeviceDisconnected,
            DeviceError::UnsupportedFirmware => AppErrorCode::UnsupportedFirmware,
            DeviceError::AuthBootstrapRequired => AppErrorCode::AuthBootstrapRequired,
            DeviceError::AuthRejected => AppErrorCode::AuthRejected,
            DeviceError::TransferIncomplete => AppErrorCode::TransferIncomplete,
            DeviceError::UnsupportedCodec => AppErrorCode::UnsupportedCodec,
            DeviceError::NotPresentOnDevice => AppErrorCode::NotPresentOnDevice,
            DeviceError::DevicePowerLow => AppErrorCode::DevicePowerLow,
            DeviceError::Cancelled => AppErrorCode::Canceled,
            DeviceError::IntegrityFailed => AppErrorCode::IntegrityFailed,
        };
        Self::new(code, error.to_string())
    }
}

/// Device session lifecycle (spec 7.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceSessionState {
    /// Seen in an advertisement.
    Discovered,
    /// Identity and firmware are being established.
    Probing,
    /// The device needs provisioning we do not perform.
    NeedsProvisioning,
    /// The profile is outside every validated cell.
    UnsupportedProfile,
    /// The authentication mode is not implemented.
    UnsupportedAuth,
    /// The operating system denied access.
    PermissionDenied,
    /// Authenticated and idle.
    Authenticated,
    /// Listing recordings.
    Enumerating,
    /// Listing complete; ready to transfer.
    Available,
    /// Busy with another host.
    Busy,
    /// Link lost.
    Disconnected,
    /// Device not reachable.
    Unavailable,
    /// Bytes in flight.
    Transferring,
    /// Transfer finished; checking completeness.
    Verifying,
    /// Transfer interrupted.
    Interrupted,
    /// Resume was offered but its semantics are unverified.
    ResumeUnverified,
    /// Verification failed.
    IntegrityFailed,
    /// Durable commit in progress.
    DurableCommit,
    /// Committed to the library.
    Imported,
}

impl DeviceSessionState {
    /// Whether a read may be issued in this state.
    #[must_use]
    pub const fn permits_read(self) -> bool {
        matches!(self, Self::Available | Self::Transferring)
    }

    /// Whether this is a terminal failure state.
    #[must_use]
    pub const fn is_terminal_failure(self) -> bool {
        matches!(
            self,
            Self::UnsupportedProfile
                | Self::UnsupportedAuth
                | Self::PermissionDenied
                | Self::IntegrityFailed
        )
    }
}

/// A read-only direct device (spec 6.5, 7.9).
///
/// Every method is read-only by construction: the trait exposes no mutation,
/// so an acquisition cannot delete, clear, reconfigure or update a device.
#[async_trait]
pub trait DirectDevice: Send + Sync {
    /// Evidence-backed capability snapshot for this device and host.
    async fn capabilities(&self) -> Result<DeviceCapabilities, DeviceError>;

    /// List recordings the device exposes.
    async fn list_available(&self) -> Result<Vec<DeviceRecording>, DeviceError>;

    /// Stream one recording into `sink`.
    ///
    /// A cancelled transfer returns [`DeviceError::Cancelled`] and never a
    /// [`VerifiedTransfer`], even when bytes reached the sink.
    async fn read_to_staging(
        &self,
        id: &DeviceRecordingId,
        sink: &mut (dyn AsyncWriteSink + Send),
        cancel: &CancellationToken,
    ) -> Result<VerifiedTransfer, DeviceError>;
}

#[cfg(test)]
mod tests {
    use plaudon_domain::media::Sha256Digest;

    use super::*;

    fn validated_cell() -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::unknown("PLAUD", "Note", "macOS 26.0");
        caps.status = CapabilityStatus::Supported;
        caps.auth_scheme = AuthScheme::LegacyStaticToken;
        caps.supports_read_only_listing = Tri::Yes;
        caps.observed_firmware = Known::Known("V0095".into());
        caps.validated_at = Known::Known(Timestamp::from_millis(1));
        caps.test_run_id = Known::Known("run-1".into());
        caps.transport = Transport::Ble;
        caps
    }

    #[test]
    fn a_cell_without_archived_evidence_is_never_selectable() {
        let mut caps = validated_cell();
        assert!(caps.is_selectable());

        caps.test_run_id = Known::Unknown;
        assert!(!caps.is_selectable(), "a Supported claim needs an archived test run");

        let mut caps = validated_cell();
        caps.validated_at = Known::Unknown;
        assert!(!caps.is_selectable());

        let mut caps = validated_cell();
        caps.observed_firmware = Known::Unknown;
        assert!(!caps.is_selectable(), "unknown firmware must fail closed");
    }

    #[test]
    fn an_unimplemented_auth_scheme_fails_closed() {
        let mut caps = validated_cell();
        caps.auth_scheme = AuthScheme::EncryptedHandshake;
        assert!(!caps.auth_scheme.is_implemented());
        assert!(!caps.is_selectable());

        caps.auth_scheme = AuthScheme::Unknown;
        assert!(!caps.is_selectable());
    }

    #[test]
    fn resume_is_only_permitted_with_verified_offsets() {
        assert!(ResumeSemantics::VerifiedOffsets.may_resume());
        assert!(!ResumeSemantics::RestartOnly.may_resume());
        assert!(!ResumeSemantics::Unknown.may_resume());
    }

    #[test]
    fn a_transfer_without_a_terminal_marker_is_not_complete() {
        let mut transfer = VerifiedTransfer {
            device_recording_id: DeviceRecordingId::new(),
            identity: MediaIdentity::new(Sha256Digest::of(b"bytes"), 5, "opus"),
            quality_class: QualityClass::CompressedDeviceTransfer,
            duration_ms: Some(1_000),
            terminal_marker_observed: true,
            device_declared_byte_length: Some(5),
        };
        assert!(transfer.is_complete());

        transfer.terminal_marker_observed = false;
        assert!(!transfer.is_complete(), "our own digest alone does not prove completeness");

        transfer.terminal_marker_observed = true;
        transfer.device_declared_byte_length = Some(9);
        assert!(!transfer.is_complete(), "a declared length mismatch is not complete");
    }

    #[test]
    fn reads_are_only_permitted_in_ready_states() {
        assert!(DeviceSessionState::Available.permits_read());
        assert!(DeviceSessionState::Transferring.permits_read());
        for state in [
            DeviceSessionState::Discovered,
            DeviceSessionState::Probing,
            DeviceSessionState::NeedsProvisioning,
            DeviceSessionState::UnsupportedProfile,
            DeviceSessionState::UnsupportedAuth,
            DeviceSessionState::PermissionDenied,
            DeviceSessionState::Authenticated,
            DeviceSessionState::Busy,
            DeviceSessionState::Disconnected,
        ] {
            assert!(!state.permits_read(), "{state:?} must not permit a read");
        }
    }

    #[test]
    fn every_device_error_maps_to_its_spec_7_7_code() {
        let pairs = [
            (DeviceError::RadioPermissionDenied, "RADIO_PERMISSION_DENIED"),
            (DeviceError::DeviceBusy, "DEVICE_BUSY"),
            (DeviceError::DeviceDisconnected, "DEVICE_DISCONNECTED"),
            (DeviceError::UnsupportedFirmware, "UNSUPPORTED_FIRMWARE"),
            (DeviceError::AuthBootstrapRequired, "AUTH_BOOTSTRAP_REQUIRED"),
            (DeviceError::AuthRejected, "AUTH_REJECTED"),
            (DeviceError::TransferIncomplete, "TRANSFER_INCOMPLETE"),
            (DeviceError::UnsupportedCodec, "UNSUPPORTED_CODEC"),
            (DeviceError::NotPresentOnDevice, "NOT_PRESENT_ON_DEVICE"),
            (DeviceError::DevicePowerLow, "DEVICE_POWER_LOW"),
        ];
        for (error, expected) in pairs {
            assert_eq!(AppError::from(error.clone()).code().as_str(), expected, "{error:?}");
        }
    }
}
