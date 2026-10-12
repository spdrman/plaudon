//! Rendering of the three facts the library list is most likely to lie about.
//!
//! Spec §8.6A: a duration that was never measured is stated as unknown and
//! never drawn as `0:00`, and a timestamp that is absent or unparseable is
//! stated as unknown — "now" and the epoch are never substituted for it.
//!
//! Times render in UTC with the zone named. The shell has no locale service
//! and inventing one would make every snapshot depend on the machine that
//! produced it; saying which zone the number is in is the honest alternative
//! to silently showing the runner's.

use plaudon_domain::media::Sha256Digest;

/// What every unknown fact renders as.
pub const UNKNOWN: &str = "Unknown";

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `h:mm:ss`, `m:ss`, or [`UNKNOWN`] when the media was never measured.
#[must_use]
pub fn format_duration(duration_ms: Option<u64>) -> String {
    let Some(duration_ms) = duration_ms else {
        return UNKNOWN.to_owned();
    };
    let total_seconds = duration_ms / 1_000;
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3_600;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The same duration spoken rather than punctuated, for accessible labels.
#[must_use]
pub fn spoken_duration(duration_ms: Option<u64>) -> String {
    let Some(duration_ms) = duration_ms else {
        return "duration unknown".to_owned();
    };
    let total_seconds = duration_ms / 1_000;
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3_600;
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours} hour{}", plural(hours)));
    }
    if minutes > 0 {
        parts.push(format!("{minutes} minute{}", plural(minutes)));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds} second{}", plural(seconds)));
    }
    parts.join(" ")
}

const fn plural(value: u64) -> &'static str {
    if value == 1 {
        ""
    } else {
        "s"
    }
}

/// `12 Mar 2026, 14:03 UTC`, or [`UNKNOWN`] when there is no usable instant.
#[must_use]
pub fn format_timestamp(iso: Option<&str>) -> String {
    let Some(iso) = iso else {
        return UNKNOWN.to_owned();
    };
    let Ok(parsed) =
        time::OffsetDateTime::parse(iso, &time::format_description::well_known::Rfc3339)
    else {
        return UNKNOWN.to_owned();
    };
    let utc = parsed.to_offset(time::UtcOffset::UTC);
    let month = MONTHS[usize::from(u8::from(utc.month())) - 1];
    format!(
        "{} {month} {}, {:02}:{:02} UTC",
        utc.day(),
        utc.year(),
        utc.hour(),
        utc.minute()
    )
}

/// The first twelve hex characters of a digest, which is what the duplicate
/// and provenance copy quotes. Never the whole digest: it is a fingerprint
/// for a human to compare, not a value to retype.
#[must_use]
pub fn short_hash(digest: &Sha256Digest) -> String {
    let hex = digest.to_hex();
    format!("{}…", &hex[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmeasured_duration_is_stated_and_never_drawn_as_zero() {
        assert_eq!(format_duration(None), "Unknown");
        assert_ne!(format_duration(None), "0:00");
        assert_eq!(spoken_duration(None), "duration unknown");
    }

    #[test]
    fn a_measured_duration_renders_minutes_then_hours() {
        assert_eq!(format_duration(Some(0)), "0:00");
        assert_eq!(format_duration(Some(9_000)), "0:09");
        assert_eq!(format_duration(Some(61_000)), "1:01");
        assert_eq!(format_duration(Some(3_601_000)), "1:00:01");
        assert_eq!(format_duration(Some(45_296_000)), "12:34:56");
    }

    #[test]
    fn a_spoken_duration_names_its_units() {
        assert_eq!(spoken_duration(Some(0)), "0 seconds");
        assert_eq!(spoken_duration(Some(1_000)), "1 second");
        assert_eq!(spoken_duration(Some(61_000)), "1 minute 1 second");
        assert_eq!(spoken_duration(Some(3_600_000)), "1 hour");
    }

    #[test]
    fn a_missing_or_malformed_timestamp_is_unknown_and_never_invented() {
        assert_eq!(format_timestamp(None), "Unknown");
        assert_eq!(format_timestamp(Some("")), "Unknown");
        assert_eq!(format_timestamp(Some("yesterday")), "Unknown");
        assert_eq!(format_timestamp(Some("2026-03-12")), "Unknown");
    }

    #[test]
    fn a_timestamp_renders_in_utc_whatever_offset_it_arrived_in() {
        assert_eq!(
            format_timestamp(Some("2026-03-12T14:03:22Z")),
            "12 Mar 2026, 14:03 UTC"
        );
        assert_eq!(
            format_timestamp(Some("2026-03-12T16:03:22+02:00")),
            "12 Mar 2026, 14:03 UTC",
            "the same instant must render the same wherever it was recorded"
        );
    }

    #[test]
    fn a_digest_is_shown_as_a_twelve_character_fingerprint() {
        let digest = Sha256Digest::of(b"plaudon");
        let short = short_hash(&digest);
        assert_eq!(
            short.chars().count(),
            13,
            "twelve hex characters and an ellipsis"
        );
        assert!(digest.to_hex().starts_with(&short[..12]));
    }
}
