//! Presentation tables for the Devices destination (spec §5.2, App. B.8).
//!
//! Only *structure* lives here: section order and names, glyphs, number
//! formatting and which states can still be cancelled. Every compatibility
//! statement — the status label, the known limitations, the radio disclosures,
//! the media label, the completeness qualifier and the transfer-state label —
//! arrives in the DTO, which `plaudon-app` derives from the compatibility
//! register and from the `*_ui_label()` helpers. Nothing in this file is
//! allowed to describe what a BLE source yields or how trustworthy a link is.

use plaudon_app::dto::{
    CloudRoute, DeviceCapability, DeviceSection, DeviceSupportStatus as SupportStatus,
    DeviceTransferState as TransferState, ListingCompleteness, RadioState,
};

/// The five App. B.8 sections, in display order.
pub const SECTION_ORDER: [DeviceSection; 5] = [
    DeviceSection::Nearby,
    DeviceSection::Connected,
    DeviceSection::NeedsAuthorization,
    DeviceSection::UnsupportedFirmware,
    DeviceSection::PreviouslyLinked,
];

/// Heading of one section.
#[must_use]
pub const fn section_label(section: DeviceSection) -> &'static str {
    match section {
        DeviceSection::Nearby => "Nearby",
        DeviceSection::Connected => "Connected",
        DeviceSection::NeedsAuthorization => "Needs authorization",
        DeviceSection::UnsupportedFirmware => "Unsupported firmware",
        DeviceSection::PreviouslyLinked => "Previously linked",
    }
}

/// What a section says when it holds no rows *after a scan that ran*.
#[must_use]
pub const fn section_empty(section: DeviceSection) -> &'static str {
    match section {
        DeviceSection::Nearby => "No PLAUD devices were heard in this scan.",
        DeviceSection::Connected => "No device is connected.",
        DeviceSection::NeedsAuthorization => "No scanned device is waiting for authorization.",
        DeviceSection::UnsupportedFirmware => "No scanned device reported unsupported firmware.",
        DeviceSection::PreviouslyLinked => "No device has been linked on this computer.",
    }
}

/// What a section says when the radio could not be used, so "empty" would be a
/// claim nothing supports.
pub const SECTION_NOT_CHECKED: &str = "Not checked: the radio could not be used for this scan.";

/// Whether a section's contents depend on the radio having been consulted.
#[must_use]
pub const fn section_depends_on_radio(section: DeviceSection) -> bool {
    !matches!(section, DeviceSection::PreviouslyLinked)
}

/// A status rendered as a glyph *and* a word, so no state is told apart by
/// colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Badge {
    /// Non-colour marker, repeated in every theme.
    pub glyph: &'static str,
    /// Severity, used for the text colour only — never as the sole signal.
    pub tone: Tone,
}

/// Severity of a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// No judgement.
    Neutral,
    /// Work is in progress.
    Running,
    /// Waiting on something outside the application.
    Waiting,
    /// Finished as intended.
    Success,
    /// Needs attention but is not a failure.
    Warning,
    /// Failed, refused or unsupported.
    Danger,
}

/// Glyph and tone for a register status. The *word* is the register's own
/// `status_label`, never one written here.
#[must_use]
pub const fn support_badge(status: SupportStatus) -> Badge {
    match status {
        SupportStatus::Supported => Badge {
            glyph: "✓",
            tone: Tone::Success,
        },
        SupportStatus::Experimental => Badge {
            glyph: "!",
            tone: Tone::Warning,
        },
        SupportStatus::NotEvaluated => Badge {
            glyph: "?",
            tone: Tone::Neutral,
        },
        SupportStatus::KnownUnsupported => Badge {
            glyph: "×",
            tone: Tone::Danger,
        },
    }
}

/// Glyph and tone for a transfer state (§4.2A.8). `On device`,
/// `Transferred locally` and `Archived` differ in all three of glyph, tone and
/// label, so none of them is distinguishable by colour alone.
#[must_use]
pub const fn transfer_badge(state: TransferState) -> Badge {
    match state {
        TransferState::Discovered => Badge {
            glyph: "○",
            tone: Tone::Neutral,
        },
        TransferState::Queued | TransferState::WaitingForDevice => Badge {
            glyph: "…",
            tone: Tone::Waiting,
        },
        TransferState::Connecting | TransferState::Authenticating | TransferState::Requesting => {
            Badge {
                glyph: "↻",
                tone: Tone::Running,
            }
        }
        TransferState::Receiving => Badge {
            glyph: "↓",
            tone: Tone::Running,
        },
        TransferState::Verifying => Badge {
            glyph: "≟",
            tone: Tone::Running,
        },
        TransferState::Committing => Badge {
            glyph: "◐",
            tone: Tone::Waiting,
        },
        TransferState::Imported => Badge {
            glyph: "●",
            tone: Tone::Success,
        },
        TransferState::Interrupted => Badge {
            glyph: "!",
            tone: Tone::Warning,
        },
        TransferState::Paused => Badge {
            glyph: "‖",
            tone: Tone::Waiting,
        },
        TransferState::Cancelled => Badge {
            glyph: "×",
            tone: Tone::Neutral,
        },
        TransferState::Unsupported => Badge {
            glyph: "×",
            tone: Tone::Danger,
        },
    }
}

/// Whether a transfer in this state can still be stopped.
#[must_use]
pub const fn is_cancellable(state: TransferState) -> bool {
    matches!(
        state,
        TransferState::Queued
            | TransferState::Connecting
            | TransferState::Authenticating
            | TransferState::Requesting
            | TransferState::Receiving
            | TransferState::Verifying
            | TransferState::Committing
            | TransferState::WaitingForDevice
            | TransferState::Paused
    )
}

/// Whether a transfer stopped in a way the user can ask us to try again.
///
/// The ledger keeps the staged bytes of an interrupted transfer, so asking for
/// the same recording again is the recovery path; a cancelled or unsupported
/// transfer is terminal and `Unsupported` is never retried, because the profile
/// did not change.
#[must_use]
pub const fn is_retryable(state: TransferState) -> bool {
    matches!(state, TransferState::Interrupted | TransferState::Cancelled)
}

/// Whether a transfer is in flight, which is what blocks a second import.
#[must_use]
pub const fn is_in_flight(state: TransferState) -> bool {
    matches!(
        state,
        TransferState::Queued
            | TransferState::Connecting
            | TransferState::Authenticating
            | TransferState::Requesting
            | TransferState::Receiving
            | TransferState::Verifying
            | TransferState::Committing
    )
}

/// Name of a capability. What it *may do* is the DTO's `available` and
/// `reason`.
#[must_use]
pub const fn capability_label(capability: DeviceCapability) -> &'static str {
    match capability {
        DeviceCapability::ReadOnlyListing => "Read-only listing of recordings",
        DeviceCapability::ReadOnlyTransfer => "Read-only transfer",
        DeviceCapability::AutomaticSync => "Automatic transfer when nearby",
    }
}

/// Human name of a transport identifier.
#[must_use]
pub fn transport_label(transport: &str) -> &str {
    match transport {
        "ble" => "Bluetooth LE (BLE)",
        other => other,
    }
}

/// Whether a cloud route was involved. The caller must pass the DTO's own
/// `cloud_route`; this is the only place either sentence exists, so no screen
/// can print the reassuring one unconditionally.
#[must_use]
pub const fn cloud_label(route: CloudRoute) -> &'static str {
    match route {
        CloudRoute::NotUsed => "Cloud: not used",
        CloudRoute::Used => "Cloud: used",
    }
}

/// Heading for a radio that could not be consulted.
#[must_use]
pub const fn radio_heading(state: RadioState) -> Option<&'static str> {
    match state {
        RadioState::Scanned => None,
        RadioState::PermissionDenied => Some("Bluetooth access is not permitted"),
        RadioState::UnavailableInBuild => Some("This build cannot scan for Bluetooth devices"),
    }
}

/// Qualifier of a listing, as a short badge word next to the DTO's own
/// `completeness_label` sentence.
#[must_use]
pub const fn completeness_badge(completeness: ListingCompleteness) -> Badge {
    match completeness {
        ListingCompleteness::UnsyncedDelta => Badge {
            glyph: "!",
            tone: Tone::Warning,
        },
        ListingCompleteness::UnsyncedDeltaTruncated => Badge {
            glyph: "!",
            tone: Tone::Warning,
        },
        ListingCompleteness::Unknown => Badge {
            glyph: "?",
            tone: Tone::Neutral,
        },
    }
}

/// §5.4 corrective actions for a scan that heard nothing.
pub const NOT_FOUND_ACTIONS: [&str; 4] = [
    "Check that Bluetooth permission is allowed for Plaudon in your system settings.",
    "Check that the device is powered on and charged.",
    "Close the PLAUD phone app: a phone connected to the device can keep other hosts out.",
    "Check that the model is one this application can check. Unsupported models are listed with their status once they are found.",
];

/// An unknown stays unknown: never `0 dBm`, never a guess.
#[must_use]
pub fn signal_label(rssi_dbm: Option<i16>) -> String {
    rssi_dbm.map_or_else(|| "Unknown".to_owned(), |dbm| format!("{dbm} dBm"))
}

/// Battery, or `Unknown`. Device storage capacity is not reported at all, so
/// no screen shows one.
#[must_use]
pub fn battery_label(percent: Option<u8>) -> String {
    percent.map_or_else(|| "Unknown".to_owned(), |value| format!("{value}%"))
}

/// Observed bandwidth. Never invented: before anything is measurable it says
/// so.
#[must_use]
pub fn bandwidth_label(bytes_per_second: Option<u64>) -> String {
    bytes_per_second.map_or_else(
        || "Not measured yet".to_owned(),
        |rate| format!("{}/s", bytes_label(rate)),
    )
}

/// A rounded estimate of the time left, or a statement that there is none.
/// The caller labels it an estimate; this never returns a bare number.
#[must_use]
pub fn eta_label(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return "Cannot be estimated yet".to_owned();
    };
    if seconds < 60 {
        return format!("about {} s", seconds.max(1));
    }
    if seconds < 3600 {
        return format!("about {} min", (seconds as f64 / 60.0).round() as u64);
    }
    let hours = (seconds as f64 / 360.0).round() / 10.0;
    format!("about {hours} h")
}

/// Binary byte sizes, as the settings screen formats them.
#[must_use]
pub fn bytes_label(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Display name of a device that may not advertise one.
#[must_use]
pub fn device_name(advertised: Option<&str>) -> &str {
    match advertised {
        Some(name) if !name.trim().is_empty() => name,
        _ => "Unnamed device",
    }
}

/// Fraction of the declared length that has arrived, when both numbers exist.
///
/// The declared length is the device's claim, not a measurement, which is why
/// the caller always labels the bar with what it is a fraction *of*.
#[must_use]
pub fn progress_fraction(bytes_received: Option<u64>, declared_byte_length: u64) -> Option<f32> {
    match bytes_received {
        Some(received) if declared_byte_length > 0 => {
            Some((received as f32 / declared_byte_length as f32).clamp(0.0, 1.0))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_five_sections_are_in_appendix_order() {
        assert_eq!(
            SECTION_ORDER.map(section_label),
            [
                "Nearby",
                "Connected",
                "Needs authorization",
                "Unsupported firmware",
                "Previously linked"
            ]
        );
    }

    #[test]
    fn every_transfer_state_has_a_distinct_glyph_within_its_tone() {
        for state in TransferState::ALL {
            let badge = transfer_badge(*state);
            assert!(!badge.glyph.is_empty(), "{state:?} has no glyph");
        }
        let discovered = transfer_badge(TransferState::Discovered);
        let committing = transfer_badge(TransferState::Committing);
        let imported = transfer_badge(TransferState::Imported);
        assert_ne!(discovered.glyph, committing.glyph);
        assert_ne!(committing.glyph, imported.glyph);
        assert_ne!(discovered.tone, imported.tone);
    }

    #[test]
    fn unknown_facts_are_unknown_and_never_zero() {
        assert_eq!(signal_label(None), "Unknown");
        assert_eq!(signal_label(Some(-64)), "-64 dBm");
        assert_eq!(battery_label(None), "Unknown");
        assert_eq!(battery_label(Some(0)), "0%");
        assert_eq!(bandwidth_label(None), "Not measured yet");
        assert_eq!(eta_label(None), "Cannot be estimated yet");
    }

    #[test]
    fn an_estimate_is_rounded_and_never_a_bare_number() {
        assert_eq!(eta_label(Some(0)), "about 1 s");
        assert_eq!(eta_label(Some(45)), "about 45 s");
        assert_eq!(eta_label(Some(90)), "about 2 min");
        assert_eq!(eta_label(Some(7200)), "about 2 h");
    }

    #[test]
    fn progress_needs_both_numbers() {
        assert_eq!(progress_fraction(None, 100), None);
        assert_eq!(progress_fraction(Some(50), 0), None);
        assert!((progress_fraction(Some(50), 100).unwrap() - 0.5).abs() < f32::EPSILON);
        assert!((progress_fraction(Some(500), 100).unwrap() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn only_a_live_transfer_can_be_cancelled_and_only_a_stopped_one_retried() {
        assert!(is_cancellable(TransferState::Receiving));
        assert!(!is_cancellable(TransferState::Imported));
        assert!(!is_cancellable(TransferState::Discovered));
        assert!(is_retryable(TransferState::Interrupted));
        assert!(is_retryable(TransferState::Cancelled));
        assert!(!is_retryable(TransferState::Unsupported));
        assert!(!is_retryable(TransferState::Imported));
    }

    #[test]
    fn bytes_are_binary_and_small_sizes_stay_exact() {
        assert_eq!(bytes_label(512), "512 B");
        assert_eq!(bytes_label(1024), "1.0 KiB");
        assert_eq!(bytes_label(1_572_864), "1.5 MiB");
    }

    #[test]
    fn the_cloud_sentence_is_derived_from_the_dto() {
        assert_eq!(cloud_label(CloudRoute::NotUsed), "Cloud: not used");
        assert_eq!(cloud_label(CloudRoute::Used), "Cloud: used");
    }

    #[test]
    fn a_radio_that_could_not_be_used_is_not_an_empty_room() {
        assert_eq!(radio_heading(RadioState::Scanned), None);
        assert!(radio_heading(RadioState::PermissionDenied).is_some());
        assert!(radio_heading(RadioState::UnavailableInBuild).is_some());
        assert_ne!(
            radio_heading(RadioState::PermissionDenied),
            radio_heading(RadioState::UnavailableInBuild)
        );
    }
}
