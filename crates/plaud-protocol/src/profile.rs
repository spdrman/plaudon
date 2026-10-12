//! Named protocol profiles (spec App. B.3, §7.0, §7.9).
//!
//! Every constant the legacy profile uses — GATT identifiers, chunk size,
//! terminal sentinel, token shape, bounds — lives in a [`ProtocolProfile`]
//! value with a name, a vendor, a model and an explicit list of firmware
//! fingerprints. Nothing in this crate reads a bare constant at a call site,
//! so a constant cannot leak onto hardware it was never observed on.
//!
//! [`profile_for`] matches an **exact** `(model, firmware)` pair. A device
//! whose firmware is not listed resolves to `None`, which is the fail-closed
//! outcome spec §2.5B requires: "We will fail closed if a firmware or OS
//! combination is unknown."

use serde::{Deserialize, Serialize};

use crate::gatt;

/// How a profile authenticates a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAuthMode {
    /// A static per-device token presented over an unencrypted link.
    LegacyStaticToken,
    /// An encrypted pre-handshake this build does not implement.
    EncryptedHandshake,
}

/// Everything the wire codec needs to talk to one named device profile.
///
/// The struct is deliberately `'static` and constructed only in this module:
/// a profile is evidence, and evidence is not assembled at runtime from
/// whatever a device advertised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolProfile {
    /// Stable identifier used in fixtures, evidence records and the registry.
    pub name: &'static str,
    /// Vendor this profile was observed for.
    pub vendor: &'static str,
    /// Device model this profile was observed for.
    pub device_model: &'static str,
    /// Firmware fingerprints this profile covers. Exact strings, no wildcards.
    pub firmware_fingerprints: &'static [&'static str],
    /// Vendor GATT service.
    pub service_vendor: u16,
    /// Characteristic the host writes control frames to.
    pub characteristic_control: u16,
    /// Characteristic the device notifies on.
    pub characteristic_notify: u16,
    /// Standard battery level characteristic.
    pub characteristic_battery: u16,
    /// Application chunk payload observed for this profile.
    pub chunk_payload_bytes: u16,
    /// Authentication mode this profile offers.
    pub auth_mode: ProfileAuthMode,
    /// Accepted lengths, in bytes, of a decoded device token.
    pub auth_token_byte_lengths: &'static [usize],
    /// Hard ceiling on one transferred recording.
    pub max_transfer_bytes: u64,
    /// Hard ceiling on the number of entries one listing may report.
    pub max_listed_recordings: usize,
    /// Whether repeated offset reads were demonstrated on real hardware.
    ///
    /// Spec §7.7 permits range resume "only after hardware demonstrates
    /// correct repeated reads and offset semantics", so this is `false` until
    /// an archived hardware record says otherwise.
    pub offset_reads_demonstrated: bool,
    /// Whether the listing is a delta of unsynchronized files rather than a
    /// complete on-device history (spec §2.5A, App. B.3).
    pub listing_is_delta_only: bool,
}

impl ProtocolProfile {
    /// Whether this profile covers `firmware`.
    #[must_use]
    pub fn covers_firmware(&self, firmware: &str) -> bool {
        let firmware = firmware.trim();
        !firmware.is_empty()
            && self
                .firmware_fingerprints
                .iter()
                .any(|known| known.eq_ignore_ascii_case(firmware))
    }

    /// Whether this profile covers `model`.
    #[must_use]
    pub fn covers_model(&self, model: &str) -> bool {
        self.device_model.eq_ignore_ascii_case(model.trim())
    }

    /// Whether a decoded token has a length this profile accepts.
    #[must_use]
    pub fn accepts_token_length(&self, bytes: usize) -> bool {
        self.auth_token_byte_lengths.contains(&bytes)
    }

    /// Whether this build can complete the profile's authentication mode.
    ///
    /// Spec §7.9 and App. B.5: an encrypted mode is refused, never downgraded.
    #[must_use]
    pub const fn auth_mode_is_implemented(&self) -> bool {
        matches!(self.auth_mode, ProfileAuthMode::LegacyStaticToken)
    }
}

/// Original PLAUD Note, legacy `V0095`-like firmware (spec App. B.3).
///
/// Sourced from the specification's record of the MIT `plaude-cli` notes. No
/// PLAUD hardware has been attached to this build, so the matching
/// `compatibility.yaml` cells stay `not_evaluated`.
pub const LEGACY_NOTE_V0095: ProtocolProfile = ProtocolProfile {
    name: "plaud-note-legacy-v0095",
    vendor: "PLAUD",
    device_model: "Note",
    firmware_fingerprints: &["V0095"],
    service_vendor: gatt::SERVICE_VENDOR,
    characteristic_control: gatt::CHARACTERISTIC_CONTROL,
    characteristic_notify: gatt::CHARACTERISTIC_NOTIFY,
    characteristic_battery: gatt::CHARACTERISTIC_BATTERY,
    chunk_payload_bytes: 80,
    auth_mode: ProfileAuthMode::LegacyStaticToken,
    // Upstream notes describe a 16 or 32 byte token, hex-encoded for storage.
    auth_token_byte_lengths: &[16, 32],
    // A multi-hour `.ASR` sidecar is tens of megabytes; the ceiling exists so a
    // forged length cannot turn into unbounded disk use.
    max_transfer_bytes: 512 * 1024 * 1024,
    max_listed_recordings: 512,
    offset_reads_demonstrated: false,
    listing_is_delta_only: true,
};

/// Every profile this build implements.
pub const PROFILES: &[&ProtocolProfile] = &[&LEGACY_NOTE_V0095];

/// Resolve the profile for an observed `(model, firmware)` pair.
///
/// Exact match on both components. There is no closest match and no
/// name-prefix match: spec §7.7 refuses to apply the legacy profile to a
/// newly advertised model, and §2.5B fails closed on an unknown firmware.
#[must_use]
pub fn profile_for(device_model: &str, firmware: &str) -> Option<&'static ProtocolProfile> {
    PROFILES
        .iter()
        .copied()
        .find(|profile| profile.covers_model(device_model) && profile.covers_firmware(firmware))
}

/// Look a profile up by its stable name, for fixtures and evidence records.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<&'static ProtocolProfile> {
    PROFILES
        .iter()
        .copied()
        .find(|profile| profile.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legacy_profile_matches_only_its_recorded_model_and_firmware() {
        assert_eq!(
            profile_for("Note", "V0095").map(|p| p.name),
            Some("plaud-note-legacy-v0095")
        );
        assert_eq!(
            profile_for(" note ", " v0095 ").map(|p| p.name),
            Some("plaud-note-legacy-v0095")
        );

        for (model, firmware) in [
            ("Note", "V0096"),
            ("Note", "unknown"),
            ("Note", ""),
            ("Note Pro", "V0095"),
            ("NotePin S", "V0095"),
            ("Note2", "V0095"),
            ("", "V0095"),
        ] {
            assert!(
                profile_for(model, firmware).is_none(),
                "({model:?}, {firmware:?}) must not resolve to a named profile"
            );
        }
    }

    #[test]
    fn a_profile_carries_its_own_constants_rather_than_sharing_globals() {
        let profile = LEGACY_NOTE_V0095;
        assert_eq!(profile.chunk_payload_bytes, 80);
        assert_eq!(profile.service_vendor, 0x1910);
        assert_eq!(profile.characteristic_control, 0x2BB1);
        assert_eq!(profile.characteristic_notify, 0x2BB0);
        assert_eq!(profile.characteristic_battery, 0x2A19);
        assert!(profile.listing_is_delta_only);
        assert!(
            !profile.offset_reads_demonstrated,
            "resume stays off until hardware demonstrates offset semantics"
        );
    }

    #[test]
    fn the_legacy_auth_mode_is_implemented_and_an_encrypted_one_is_not() {
        assert!(LEGACY_NOTE_V0095.auth_mode_is_implemented());

        let encrypted = ProtocolProfile {
            auth_mode: ProfileAuthMode::EncryptedHandshake,
            ..LEGACY_NOTE_V0095
        };
        assert!(!encrypted.auth_mode_is_implemented());
    }

    #[test]
    fn token_lengths_outside_the_profile_are_refused() {
        assert!(LEGACY_NOTE_V0095.accepts_token_length(16));
        assert!(LEGACY_NOTE_V0095.accepts_token_length(32));
        for wrong in [0, 1, 15, 17, 31, 33, 1024] {
            assert!(!LEGACY_NOTE_V0095.accepts_token_length(wrong), "{wrong}");
        }
    }

    #[test]
    fn profile_names_are_unique_and_resolvable() {
        for profile in PROFILES {
            assert_eq!(
                profile_by_name(profile.name).map(|p| p.name),
                Some(profile.name)
            );
        }
        assert!(profile_by_name("plaud-note-encrypted").is_none());
    }
}
