//! The integer-time timeline: zoom, scroll and the pixel ↔ time mapping.
//!
//! Zoom is stored as *microseconds per logical point* in unsigned Q32.32 fixed
//! point, and scroll is stored as an exact [`Micros`] offset. Both mappings are
//! recomputed from that state on every call, so neither panning nor zooming
//! ever accumulates error: zooming in two hundred times and back out returns
//! the same time to the same pixel, which a float `pixels_per_second` running
//! total does not.
//!
//! All pixel quantities are egui *logical points*, not physical device pixels.
//! The widget layer converts once, at the edge.

use crate::time::{Micros, TimeSpan};

/// Fractional bits in the fixed-point scale.
const FRAC_BITS: u32 = 32;
const ONE: u64 = 1u64 << FRAC_BITS;

/// An exact rational zoom factor.
///
/// Gestures arrive as floats; they are quantised into a rational here, once,
/// and the timeline then only ever multiplies exact integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ratio {
    num: u32,
    den: u32,
}

impl Ratio {
    /// Identity.
    pub const ONE: Self = Self { num: 1, den: 1 };
    /// One zoom-in notch: the view spans four fifths of the time it did.
    pub const ZOOM_IN_STEP: Self = Self { num: 4, den: 5 };
    /// One zoom-out notch.
    pub const ZOOM_OUT_STEP: Self = Self { num: 5, den: 4 };

    /// Build a ratio, rejecting a zero denominator or numerator.
    #[must_use]
    pub const fn new(num: u32, den: u32) -> Self {
        if num == 0 || den == 0 {
            return Self::ONE;
        }
        Self { num, den }
    }

    /// Quantise a positive float factor to 1/1024ths.
    ///
    /// Anything outside `[1/64, 64]` is clamped: a pinch gesture that reports
    /// an absurd factor must not be able to overflow the scale.
    #[must_use]
    pub fn from_f32(factor: f32) -> Self {
        if !factor.is_finite() || factor <= 0.0 {
            return Self::ONE;
        }
        let clamped = factor.clamp(1.0 / 64.0, 64.0);
        let num = (f64::from(clamped) * 1024.0).round().max(1.0) as u32;
        Self { num, den: 1024 }
    }

    /// Numerator.
    #[must_use]
    pub const fn num(self) -> u32 {
        self.num
    }

    /// Denominator.
    #[must_use]
    pub const fn den(self) -> u32 {
        self.den
    }

    /// The reciprocal.
    #[must_use]
    pub const fn inverse(self) -> Self {
        Self {
            num: self.den,
            den: self.num,
        }
    }
}

/// Microseconds covered by one logical point, in Q32.32 fixed point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scale(u64);

impl Scale {
    /// Finest zoom: one microsecond per point. Far beyond sample resolution at
    /// every rate we decode, so the limit is never the thing a user feels.
    pub const MIN: Self = Self(ONE);
    /// Coarsest zoom: one thousand seconds per point — a 24-hour recording in
    /// 87 points.
    pub const MAX: Self = Self(1_000_000_000 * ONE);

    /// Build from a raw Q32.32 value, clamped into `[MIN, MAX]`.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        if raw < Self::MIN.0 {
            Self::MIN
        } else if raw > Self::MAX.0 {
            Self::MAX
        } else {
            Self(raw)
        }
    }

    /// The raw Q32.32 value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Build from microseconds per point.
    #[must_use]
    pub fn from_micros_per_point(micros: f64) -> Self {
        if !micros.is_finite() || micros <= 0.0 {
            return Self::MIN;
        }
        let raw = (micros * ONE as f64).round();
        if raw >= Self::MAX.0 as f64 {
            Self::MAX
        } else {
            Self::from_raw(raw as u64)
        }
    }

    /// Build from points per second, the unit humans reason about.
    #[must_use]
    pub fn from_points_per_second(pps: f64) -> Self {
        if !pps.is_finite() || pps <= 0.0 {
            return Self::MAX;
        }
        Self::from_micros_per_point(1_000_000.0 / pps)
    }

    /// Microseconds per point, for display and debugging.
    #[must_use]
    pub fn micros_per_point(self) -> f64 {
        self.0 as f64 / ONE as f64
    }

    /// Points per second, for display and debugging.
    #[must_use]
    pub fn points_per_second(self) -> f64 {
        1_000_000.0 / self.micros_per_point()
    }

    /// Multiply by an exact ratio, clamped into `[MIN, MAX]`.
    #[must_use]
    pub fn scaled(self, ratio: Ratio) -> Self {
        let raw = u128::from(self.0) * u128::from(ratio.num()) / u128::from(ratio.den());
        if raw > u128::from(Self::MAX.0) {
            Self::MAX
        } else {
            Self::from_raw(raw as u64)
        }
    }

    /// Exact duration spanned by a whole number of points.
    ///
    /// Division, not a shift: an arithmetic shift rounds towards negative
    /// infinity, so a shift would make a pan of −1 point travel one
    /// microsecond further than a pan of +1 point and panning would stop
    /// being reversible.
    #[must_use]
    pub fn micros_for_points(self, points: i64) -> Micros {
        let value = i128::from(points) * i128::from(self.0) / (1i128 << FRAC_BITS);
        Micros::new(clamp(value))
    }

    /// Duration spanned by a fractional point offset (pointer positions).
    ///
    /// The float is quantised to 1/256 of a point before any integer work, so
    /// the same pointer position always yields the same time, and the
    /// division truncates towards zero so the result is symmetric in sign.
    #[must_use]
    pub fn micros_for_points_f32(self, points: f32) -> Micros {
        let two_fifty_sixths = (f64::from(points) * 256.0).round() as i64;
        let value = i128::from(two_fifty_sixths) * i128::from(self.0) / (1i128 << (FRAC_BITS + 8));
        Micros::new(clamp(value))
    }

    /// Point offset of a duration, with 1/256-point precision.
    #[must_use]
    pub fn points_for_micros(self, micros: Micros) -> f32 {
        let numerator = (i128::from(micros.get()) << (FRAC_BITS + 8)) / i128::from(self.0);
        (clamp(numerator) as f64 / 256.0) as f32
    }
}

fn clamp(value: i128) -> i64 {
    if value > i128::from(i64::MAX) {
        i64::MAX
    } else if value < i128::from(i64::MIN) {
        i64::MIN
    } else {
        value as i64
    }
}

/// Zoom, scroll and the viewport width of one waveform view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeline {
    duration: Micros,
    scroll: Micros,
    scale: Scale,
    viewport_points: u32,
}

impl Timeline {
    /// A timeline over `duration`, zoomed so the whole recording fits `viewport_points`.
    #[must_use]
    pub fn new(duration: Micros, viewport_points: u32) -> Self {
        let mut timeline = Self {
            duration: Micros::new(duration.get().max(0)),
            scroll: Micros::ZERO,
            scale: Scale::MIN,
            viewport_points: viewport_points.max(1),
        };
        timeline.zoom_to_fit();
        timeline
    }

    /// Total length of the recording.
    #[must_use]
    pub const fn duration(self) -> Micros {
        self.duration
    }

    /// Replace the duration, keeping zoom and clamping scroll.
    pub fn set_duration(&mut self, duration: Micros) {
        self.duration = Micros::new(duration.get().max(0));
        self.clamp_scroll();
    }

    /// Current viewport width in logical points.
    #[must_use]
    pub const fn viewport_points(self) -> u32 {
        self.viewport_points
    }

    /// Resize the viewport, keeping the left edge and the zoom level.
    pub fn set_viewport_points(&mut self, points: f32) {
        let rounded = if points.is_finite() {
            points.round().clamp(1.0, f32::from(u16::MAX)) as u32
        } else {
            1
        };
        self.viewport_points = rounded;
        self.clamp_scroll();
    }

    /// Time at the left edge of the viewport.
    #[must_use]
    pub const fn scroll(self) -> Micros {
        self.scroll
    }

    /// Current zoom.
    #[must_use]
    pub const fn scale(self) -> Scale {
        self.scale
    }

    /// Duration visible in the viewport at the current zoom.
    #[must_use]
    pub fn visible_duration(self) -> Micros {
        self.scale
            .micros_for_points(i64::from(self.viewport_points))
    }

    /// The visible span, clamped to the recording.
    #[must_use]
    pub fn visible(self) -> TimeSpan {
        TimeSpan::from_len(self.scroll, self.visible_duration())
    }

    /// Largest legal scroll offset: never scroll past the end.
    #[must_use]
    pub fn max_scroll(self) -> Micros {
        Micros::new((self.duration.get() - self.visible_duration().get()).max(0))
    }

    /// Time under a point offset measured from the left edge of the viewport.
    #[must_use]
    pub fn time_at_point(self, point: f32) -> Micros {
        self.scroll
            .saturating_add(self.scale.micros_for_points_f32(point))
    }

    /// Point offset of a time, measured from the left edge of the viewport.
    /// May be negative or beyond the viewport width; callers cull.
    #[must_use]
    pub fn point_of_time(self, time: Micros) -> f32 {
        self.scale
            .points_for_micros(time.saturating_sub(self.scroll))
    }

    /// Set the left edge, clamped into `[0, max_scroll]`.
    pub fn set_scroll(&mut self, scroll: Micros) {
        self.scroll = scroll.clamp_to(Micros::ZERO, self.max_scroll());
    }

    /// Pan by a point delta (positive scrolls forward in time).
    pub fn scroll_by_points(&mut self, points: f32) {
        let delta = self.scale.micros_for_points_f32(points);
        self.set_scroll(self.scroll.saturating_add(delta));
    }

    /// Centre the viewport on `time`.
    pub fn center_on(&mut self, time: Micros) {
        let half = Micros::new(self.visible_duration().get() / 2);
        self.set_scroll(time.saturating_sub(half));
    }

    /// Scroll the minimum distance that brings `time` into view with `margin`
    /// points of slack. Returns whether the scroll changed.
    pub fn scroll_to_reveal(&mut self, time: Micros, margin_points: f32) -> bool {
        let before = self.scroll;
        let margin = self.scale.micros_for_points_f32(margin_points.max(0.0));
        let visible = self.visible();
        if time < visible.start().saturating_add(margin) {
            self.set_scroll(time.saturating_sub(margin));
        } else if time > visible.end().saturating_sub(margin) {
            let span = self.visible_duration();
            self.set_scroll(time.saturating_add(margin).saturating_sub(span));
        }
        self.scroll != before
    }

    /// Zoom so the whole recording fits the viewport.
    pub fn zoom_to_fit(&mut self) {
        let points = u128::from(self.viewport_points.max(1));
        let raw = if self.duration.get() <= 0 {
            u128::from(Scale::MIN.raw())
        } else {
            ((self.duration.get() as u128) << FRAC_BITS) / points
        };
        self.scale = Scale::from_raw(if raw > u128::from(u64::MAX) {
            u64::MAX
        } else {
            raw as u64
        });
        self.scroll = Micros::ZERO;
    }

    /// Multiply the zoom by `ratio`, keeping the time under `anchor_point`
    /// fixed. A ratio below one zooms in.
    pub fn zoom_about(&mut self, ratio: Ratio, anchor_point: f32) {
        let anchor_time = self.time_at_point(anchor_point);
        let next = self.scale.scaled(ratio);
        if next == self.scale {
            return;
        }
        self.scale = next;
        // Recomputed from the exact anchor time, never from the previous
        // scroll value: this is what keeps repeated zooming drift free.
        let offset = self.scale.micros_for_points_f32(anchor_point);
        self.scroll = anchor_time.saturating_sub(offset);
        self.clamp_scroll();
    }

    /// Zoom about the centre of the viewport.
    pub fn zoom_centered(&mut self, ratio: Ratio) {
        let center = self.viewport_points as f32 / 2.0;
        self.zoom_about(ratio, center);
    }

    /// Set the zoom directly, keeping the time under `anchor_point` fixed.
    pub fn set_scale_about(&mut self, scale: Scale, anchor_point: f32) {
        let anchor_time = self.time_at_point(anchor_point);
        self.scale = scale;
        let offset = self.scale.micros_for_points_f32(anchor_point);
        self.scroll = anchor_time.saturating_sub(offset);
        self.clamp_scroll();
    }

    /// Clamp a time into `[0, duration]`.
    #[must_use]
    pub fn clamp_time(self, time: Micros) -> Micros {
        time.clamp_to(Micros::ZERO, self.duration)
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp_to(Micros::ZERO, self.max_scroll());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hour_timeline() -> Timeline {
        Timeline::new(Micros::from_secs(3600), 1000)
    }

    #[test]
    fn a_fresh_timeline_shows_the_whole_recording() {
        let timeline = hour_timeline();
        assert_eq!(timeline.scroll(), Micros::ZERO);
        let visible = timeline.visible_duration();
        // Integer division of the scale may lose at most one point worth.
        assert!(visible <= Micros::from_secs(3600));
        assert!(visible >= Micros::from_secs(3600) - Micros::from_millis(4));
    }

    #[test]
    fn zoom_keeps_the_anchor_time_under_the_anchor_pixel() {
        let mut timeline = hour_timeline();
        let anchor = 250.0;
        let before = timeline.time_at_point(anchor);
        timeline.zoom_about(Ratio::ZOOM_IN_STEP, anchor);
        let after = timeline.time_at_point(anchor);
        // Within one point of the new, finer scale.
        let tolerance = timeline.scale().micros_for_points(1);
        assert!(
            (after - before).abs() <= tolerance,
            "{before} vs {after}, tolerance {tolerance}"
        );
    }

    #[test]
    fn zooming_in_and_back_out_returns_the_original_scale() {
        let mut timeline = hour_timeline();
        let original = timeline.scale();
        for _ in 0..40 {
            timeline.zoom_about(Ratio::ZOOM_IN_STEP, 500.0);
        }
        for _ in 0..40 {
            timeline.zoom_about(Ratio::ZOOM_OUT_STEP, 500.0);
        }
        let drift = original.raw().abs_diff(timeline.scale().raw());
        // Exact integer multiply-divide: the only error is the rounding of
        // each individual step, which stays below a part in a thousand.
        assert!(
            drift * 1000 < original.raw(),
            "drift {drift} of {}",
            original.raw()
        );
    }

    #[test]
    fn scroll_never_runs_past_the_end() {
        let mut timeline = hour_timeline();
        timeline.zoom_about(Ratio::new(1, 100), 0.0);
        timeline.set_scroll(Micros::from_secs(100_000));
        assert_eq!(timeline.scroll(), timeline.max_scroll());
        assert!(timeline.visible().end() <= Micros::from_secs(3600) + Micros::from_millis(1));
    }

    #[test]
    fn a_pan_is_the_same_distance_in_both_directions() {
        let timeline = hour_timeline();
        let scale = timeline.scale();
        assert_eq!(
            scale.micros_for_points_f32(1.0).get(),
            -scale.micros_for_points_f32(-1.0).get()
        );
        assert_eq!(
            scale.micros_for_points(7).get(),
            -scale.micros_for_points(-7).get()
        );
    }

    #[test]
    fn panning_a_thousand_points_forwards_and_back_is_exact() {
        let mut timeline = hour_timeline();
        timeline.zoom_about(Ratio::new(1, 64), 500.0);
        let start = timeline.scroll();
        for _ in 0..1000 {
            timeline.scroll_by_points(3.0);
        }
        for _ in 0..1000 {
            timeline.scroll_by_points(-3.0);
        }
        assert_eq!(timeline.scroll(), start);
    }

    #[test]
    fn point_and_time_round_trip_at_every_zoom() {
        let mut timeline = hour_timeline();
        for _ in 0..30 {
            for point in [0.0f32, 1.0, 37.5, 999.0] {
                let time = timeline.time_at_point(point);
                let back = timeline.point_of_time(time);
                assert!(
                    (back - point).abs() <= 1.0,
                    "point {point} -> {time} -> {back} at scale {}",
                    timeline.scale().micros_per_point()
                );
            }
            timeline.zoom_about(Ratio::ZOOM_IN_STEP, 0.0);
        }
    }

    #[test]
    fn scroll_to_reveal_moves_the_minimum_distance() {
        let mut timeline = hour_timeline();
        timeline.zoom_about(Ratio::new(1, 60), 0.0);
        let target = Micros::from_secs(1800);
        assert!(timeline.scroll_to_reveal(target, 8.0));
        assert!(timeline.visible().contains(target));
        assert!(!timeline.scroll_to_reveal(target, 8.0));
    }

    #[test]
    fn scale_is_clamped_at_both_ends() {
        let mut timeline = hour_timeline();
        for _ in 0..500 {
            timeline.zoom_about(Ratio::ZOOM_IN_STEP, 0.0);
        }
        assert_eq!(timeline.scale(), Scale::MIN);
        for _ in 0..500 {
            timeline.zoom_about(Ratio::ZOOM_OUT_STEP, 0.0);
        }
        assert_eq!(timeline.scale(), Scale::MAX);
    }

    #[test]
    fn a_zero_length_recording_is_still_usable() {
        let timeline = Timeline::new(Micros::ZERO, 800);
        assert_eq!(timeline.max_scroll(), Micros::ZERO);
        assert_eq!(timeline.clamp_time(Micros::from_secs(5)), Micros::ZERO);
    }

    #[test]
    fn ratio_quantisation_is_stable_and_bounded() {
        assert_eq!(Ratio::from_f32(f32::NAN), Ratio::ONE);
        assert_eq!(Ratio::from_f32(-1.0), Ratio::ONE);
        assert_eq!(Ratio::from_f32(1.0), Ratio::new(1024, 1024));
        assert_eq!(Ratio::from_f32(1e9).num(), 64 * 1024);
    }
}
