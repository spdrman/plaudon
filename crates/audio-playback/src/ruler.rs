//! Ruler tick selection and time labelling.
//!
//! The step ladder and the "first candidate wide enough" rule are the ones
//! filmcraft's timeline ruler uses, expressed over integer microseconds
//! instead of video frames so that a tick always lands on an exact time.

use crate::time::{Micros, TimeSpan, MICROS_PER_MILLI, MICROS_PER_SECOND};
use crate::timeline::Timeline;

/// Candidate tick spacings, ascending. Every entry divides cleanly into the
/// next human unit so labels read as round numbers at every zoom.
const STEPS_MICROS: &[i64] = &[
    MICROS_PER_MILLI,
    2 * MICROS_PER_MILLI,
    5 * MICROS_PER_MILLI,
    10 * MICROS_PER_MILLI,
    20 * MICROS_PER_MILLI,
    50 * MICROS_PER_MILLI,
    100 * MICROS_PER_MILLI,
    200 * MICROS_PER_MILLI,
    500 * MICROS_PER_MILLI,
    MICROS_PER_SECOND,
    2 * MICROS_PER_SECOND,
    5 * MICROS_PER_SECOND,
    10 * MICROS_PER_SECOND,
    15 * MICROS_PER_SECOND,
    30 * MICROS_PER_SECOND,
    60 * MICROS_PER_SECOND,
    120 * MICROS_PER_SECOND,
    300 * MICROS_PER_SECOND,
    600 * MICROS_PER_SECOND,
    900 * MICROS_PER_SECOND,
    1_800 * MICROS_PER_SECOND,
    3_600 * MICROS_PER_SECOND,
    7_200 * MICROS_PER_SECOND,
    10_800 * MICROS_PER_SECOND,
    21_600 * MICROS_PER_SECOND,
    43_200 * MICROS_PER_SECOND,
    86_400 * MICROS_PER_SECOND,
];

/// Minor and major tick spacing chosen for one zoom level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickSpacing {
    /// Spacing of unlabelled ticks.
    pub minor: Micros,
    /// Spacing of labelled ticks. Always a multiple of `minor`.
    pub major: Micros,
}

/// One tick to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    /// Exact time of the tick.
    pub time: Micros,
    /// Whether the tick carries a label.
    pub major: bool,
}

/// Pick the finest spacings whose on-screen width clears the given minimums.
///
/// `min_minor_points` guards against a wall of ticks; `min_major_points`
/// against overlapping labels.
#[must_use]
pub fn choose_spacing(
    timeline: Timeline,
    min_minor_points: f32,
    min_major_points: f32,
) -> TickSpacing {
    let pick = |minimum: f32| -> i64 {
        let minimum = minimum.max(1.0);
        for &step in STEPS_MICROS {
            if timeline.scale().points_for_micros(Micros::new(step)) >= minimum {
                return step;
            }
        }
        *STEPS_MICROS.last().unwrap_or(&MICROS_PER_SECOND)
    };
    let minor = pick(min_minor_points);
    let mut major = pick(min_major_points.max(min_minor_points));
    if major % minor != 0 {
        // Keep labels on minor ticks: walk up until the two divide.
        major = STEPS_MICROS
            .iter()
            .copied()
            .find(|candidate| *candidate >= major && candidate % minor == 0)
            .unwrap_or(minor * 2);
    }
    TickSpacing {
        minor: Micros::new(minor),
        major: Micros::new(major),
    }
}

/// Every tick inside `span`, ascending. The count is bounded by the span
/// width in points, never by the length of the recording.
#[must_use]
pub fn ticks(span: TimeSpan, spacing: TickSpacing) -> Vec<Tick> {
    let minor = spacing.minor.get().max(1);
    let major = spacing.major.get().max(minor);
    let first = span.start().get().div_euclid(minor) * minor;
    let mut out = Vec::new();
    let mut t = first;
    while t < span.end().get() {
        if t >= span.start().get() {
            out.push(Tick {
                time: Micros::new(t),
                major: t % major == 0,
            });
        }
        t = match t.checked_add(minor) {
            Some(next) => next,
            None => break,
        };
    }
    out
}

/// `h:mm:ss` above an hour, `m:ss` below it — the format the React player used.
#[must_use]
pub fn format_clock(time: Micros) -> String {
    let total = time.get().max(0) / MICROS_PER_SECOND;
    let (hours, minutes, seconds) = (total / 3600, (total / 60) % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// A tick label at the given spacing: sub-second spacings gain decimals.
#[must_use]
pub fn format_tick(time: Micros, spacing: Micros) -> String {
    if spacing.get() >= MICROS_PER_SECOND {
        return format_clock(time);
    }
    let total = time.get().max(0);
    let whole = Micros::new(total - total % MICROS_PER_SECOND);
    let millis = (total % MICROS_PER_SECOND) / MICROS_PER_MILLI;
    if spacing.get() >= 100 * MICROS_PER_MILLI {
        format!("{}.{}", format_clock(whole), millis / 100)
    } else {
        format!("{}.{millis:03}", format_clock(whole))
    }
}

/// Position and duration as the transport bar renders them, mirroring the
/// React player's "unknown duration" behaviour.
#[must_use]
pub fn format_position(position: Micros, duration: Option<Micros>) -> String {
    match duration {
        Some(total) => format!("{} / {}", format_clock(position), format_clock(total)),
        None => format!("{} / duration unknown", format_clock(position)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_hour_in_a_thousand_points_labels_minutes_or_coarser() {
        let timeline = Timeline::new(Micros::from_secs(3600), 1000);
        let spacing = choose_spacing(timeline, 12.0, 110.0);
        assert!(spacing.major >= Micros::from_secs(300), "{spacing:?}");
        assert_eq!(spacing.major.get() % spacing.minor.get(), 0);
    }

    #[test]
    fn deep_zoom_labels_fractions_of_a_second() {
        let mut timeline = Timeline::new(Micros::from_secs(3600), 1000);
        for _ in 0..60 {
            timeline.zoom_about(crate::timeline::Ratio::ZOOM_IN_STEP, 0.0);
        }
        let spacing = choose_spacing(timeline, 12.0, 110.0);
        assert!(spacing.minor < Micros::from_secs(1), "{spacing:?}");
        let label = format_tick(Micros::new(1_234_500), spacing.major);
        assert!(label.starts_with("0:01"), "{label}");
    }

    #[test]
    fn tick_count_tracks_the_viewport_not_the_recording() {
        let short = Timeline::new(Micros::from_secs(60), 1000);
        let long = Timeline::new(Micros::from_secs(3 * 3600), 1000);
        let short_ticks = ticks(short.visible(), choose_spacing(short, 12.0, 110.0)).len();
        let long_ticks = ticks(long.visible(), choose_spacing(long, 12.0, 110.0)).len();
        assert!(short_ticks <= 120, "{short_ticks}");
        assert!(long_ticks <= 120, "{long_ticks}");
    }

    #[test]
    fn ticks_land_on_exact_multiples_and_stay_inside_the_span() {
        let span = TimeSpan::new(Micros::new(1_234_567), Micros::new(9_876_543));
        let spacing = TickSpacing {
            minor: Micros::from_millis(500),
            major: Micros::from_secs(1),
        };
        let found = ticks(span, spacing);
        assert_ne!(found.len(), 0);
        for tick in &found {
            assert_eq!(tick.time.get() % 500_000, 0);
            assert!(span.contains(tick.time));
            assert_eq!(tick.major, tick.time.get() % 1_000_000 == 0);
        }
    }

    #[test]
    fn clock_formatting_matches_the_react_player() {
        assert_eq!(format_clock(Micros::ZERO), "0:00");
        assert_eq!(format_clock(Micros::from_secs(61)), "1:01");
        assert_eq!(format_clock(Micros::from_secs(3600)), "1:00:00");
        assert_eq!(format_clock(Micros::from_secs(-5)), "0:00");
        assert_eq!(
            format_position(Micros::from_secs(5), None),
            "0:05 / duration unknown"
        );
    }
}
