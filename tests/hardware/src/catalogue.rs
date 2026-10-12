//! The named checks of the hardware suite (App. B.9, §18.3A).
//!
//! Every check has a stable identifier. The identifiers listed in
//! `EvidenceRecord::REQUIRED_CHECKS` are the ones a promotion needs; the
//! remaining App. B.9 and §18.3A checks are recorded alongside them so the
//! archived evidence is complete, but a `Skipped` optional check (for example
//! the multi-hour pull on a device without the storage) does not block
//! promotion by itself.

/// How the result of a check is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derivation {
    /// Reported by the operator/run harness in the observations file.
    Observed,
    /// Computed from the emitted opcode trace; an observation cannot override it.
    FromOpcodeTrace,
    /// Computed from the per-stage disconnect checks.
    FromDisconnectStages,
    /// Computed from the verified transfer evidence.
    FromTransfer,
    /// Observed, but only accepted when a verified transfer backs it.
    ObservedNeedsVerifiedTransfer,
    /// Observed, but only accepted when the run also proved zero network requests.
    ObservedNeedsNetworkDenied,
}

/// One named check.
#[derive(Debug, Clone, Copy)]
pub struct CheckSpec {
    /// Stable identifier recorded in the evidence.
    pub id: &'static str,
    /// What the check proves, for the runner documentation and the report.
    pub description: &'static str,
    /// Where the result comes from.
    pub derivation: Derivation,
}

const fn spec(id: &'static str, derivation: Derivation, description: &'static str) -> CheckSpec {
    CheckSpec {
        id,
        description,
        derivation,
    }
}

use Derivation::{
    FromDisconnectStages, FromOpcodeTrace, FromTransfer, Observed, ObservedNeedsNetworkDenied,
    ObservedNeedsVerifiedTransfer,
};

/// Stage identifiers of "disconnect at every transfer stage" (App. B.9).
pub const DISCONNECT_STAGES: &[&str] = &[
    "disconnect_at_connecting",
    "disconnect_at_authenticating",
    "disconnect_at_listing",
    "disconnect_at_requesting",
    "disconnect_at_streaming",
    "disconnect_at_finalizing",
];

/// Every check, in the order the suite reports them.
pub const CATALOGUE: &[CheckSpec] = &[
    spec("bluetooth_permission", Observed, "OS Bluetooth permission prompt, including the macOS app-bundle usage description, is requested and honoured (§18.3A)."),
    spec("discover_only", Observed, "Scanning finds the device and connects to nothing without user selection."),
    spec("authorization_success", Observed, "A valid device authorization reaches the authenticated state."),
    spec("authorization_failure", Observed, "A wrong or revoked authorization is refused and surfaces a typed, secret-free error."),
    spec("device_name_collision", Observed, "Two peripherals advertising the same name are not conflated; selection is by stable identifier."),
    spec("missing_secret", Observed, "A bound device whose vault secret is gone fails closed and asks for re-provisioning."),
    spec("encrypted_handshake_detected", Observed, "A device offering the encrypted handshake is detected and never driven with the legacy path."),
    spec("host_ble_exclusivity_with_mobile_app", Observed, "A competing PLAUD mobile-app BLE connection is reported as a conflict, not a hang (§18.3A)."),
    spec("two_devices_in_range", Observed, "With two devices in range the app asks which one; it never auto-selects."),
    spec("listing_delta_before_after_phone_sync", Observed, "The delta listing is observed before and after a phone synchronization and the difference is reported honestly."),
    spec("transfer_pull_15s", Observed, "A ~15 s recording is pulled and verified."),
    spec("transfer_pull_5min", Observed, "A ~5 min recording is pulled and verified."),
    spec("transfer_pull_30min", Observed, "A ~30 min recording is pulled and verified."),
    spec("transfer_pull_multi_hour", Observed, "A multi-hour recording is pulled and verified where device storage permits."),
    spec("transfer_verified", FromTransfer, "One transfer has the terminal marker, a declared length equal to the received bytes and a SHA-256 digest."),
    spec("opus_decode_measurement", Observed, "The acquired Opus bitstream decodes and its duration is measured from the bitstream."),
    spec("timestamp_verified_against_trusted_source", Observed, "The device recording timestamp agrees with a trusted source within tolerance."),
    spec("disconnect_at_connecting", Observed, "Link loss while connecting leaves no partial recording."),
    spec("disconnect_at_authenticating", Observed, "Link loss while authenticating leaves no partial recording."),
    spec("disconnect_at_listing", Observed, "Link loss while listing leaves no partial recording."),
    spec("disconnect_at_requesting", Observed, "Link loss after the data request leaves no partial recording."),
    spec("disconnect_at_streaming", Observed, "Link loss mid-stream leaves a resumable partial, never a finished recording."),
    spec("disconnect_at_finalizing", Observed, "Link loss before verification completes never marks the recording imported."),
    spec("disconnect_during_transfer", FromDisconnectStages, "Disconnect at every transfer stage behaves; derived from the six stage checks."),
    spec("sleep_wake", Observed, "Host sleep and wake mid-session recovers or fails cleanly (§18.3A)."),
    spec("resume_or_restart", Observed, "An interrupted transfer resumes or restarts, and the final bytes verify."),
    spec("low_battery", Observed, "A low-battery device ends the transfer with a clear state, not corruption."),
    spec("full_disk", Observed, "A full host disk ends the transfer with a clear state and cleans staging."),
    spec("imported_bytes_verified", ObservedNeedsVerifiedTransfer, "The imported bytes equal the acquired bytes (digest and length)."),
    spec("no_device_write_opcodes", FromOpcodeTrace, "The emitted opcode trace contains only allowlisted read-only opcodes."),
    spec("usb_mount_and_eject", Observed, "USB volume arrival, read-only inspection, cancellation and eject behave (§18.3A)."),
    spec("network_denied", ObservedNeedsNetworkDenied, "With a firewall denying PLAUD and OpenRouter, transfer and local processing work with zero cloud requests (§4.2A)."),
    spec("recovery_after_process_kill", Observed, "Killing the app mid-transfer recovers on next launch with no duplicate import."),
];

/// Look a check up by id.
#[must_use]
pub fn find(id: &str) -> Option<&'static CheckSpec> {
    CATALOGUE.iter().find(|spec| spec.id == id)
}

/// Every check identifier.
#[must_use]
pub fn ids() -> Vec<&'static str> {
    CATALOGUE.iter().map(|spec| spec.id).collect()
}
