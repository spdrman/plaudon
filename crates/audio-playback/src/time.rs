//! Integer time. Every position, duration and range in the player is an exact
//! microsecond count; no part of the clock, the timeline or the peak index is
//! allowed to accumulate a float.
//!
//! Microseconds were chosen because they divide every sample rate we decode
//! finely enough for display (one 48 kHz frame is 20.83 µs), they survive a
//! three-hour recording in an `i64` with twelve orders of magnitude to spare,
//! and they convert to the millisecond vocabulary `plaudon_domain` already
//! speaks without rounding surprises.

use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// Microseconds in one second.
pub const MICROS_PER_SECOND: i64 = 1_000_000;
/// Microseconds in one millisecond.
pub const MICROS_PER_MILLI: i64 = 1_000;

/// An exact microsecond offset. May be negative so that differences are
/// expressible; player positions are clamped non-negative by their owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Micros(i64);

impl Micros {
    /// Zero offset.
    pub const ZERO: Self = Self(0);
    /// Largest representable offset.
    pub const MAX: Self = Self(i64::MAX);

    /// Wrap a raw microsecond count.
    #[must_use]
    pub const fn new(micros: i64) -> Self {
        Self(micros)
    }

    /// The raw microsecond count.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Build from whole milliseconds.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis.saturating_mul(MICROS_PER_MILLI))
    }

    /// Build from whole seconds.
    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(MICROS_PER_SECOND))
    }

    /// Floor-divided whole milliseconds, matching `plaudon_domain::SourceTimeMs`.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0.div_euclid(MICROS_PER_MILLI)
    }

    /// Floor-divided whole seconds.
    #[must_use]
    pub const fn as_secs(self) -> i64 {
        self.0.div_euclid(MICROS_PER_SECOND)
    }

    /// Seconds as a float. Display only — never feed this back into a position.
    #[must_use]
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / MICROS_PER_SECOND as f64
    }

    /// Offset of sample frame `frames` at `rate` Hz, rounded away from zero.
    ///
    /// Not every frame lands on a whole microsecond — one 16 kHz frame is
    /// 62.5 µs — so a direction has to be chosen. Rounding away from zero is
    /// the one that makes [`Micros::to_frames`] the exact inverse, which is
    /// what stops a seek-to-frame-to-seek cycle from creeping backwards a
    /// frame at a time.
    ///
    /// Returns [`Micros::ZERO`] when `rate` is zero: a rate-less source has no
    /// time axis, and a panic in the mixer would be worse than a still clock.
    #[must_use]
    pub fn from_frames(frames: i64, rate: u32) -> Self {
        if rate == 0 {
            return Self::ZERO;
        }
        let numerator = i128::from(frames) * i128::from(MICROS_PER_SECOND);
        let denominator = i128::from(rate);
        let micros = if numerator >= 0 {
            (numerator + denominator - 1) / denominator
        } else {
            -((-numerator + denominator - 1) / denominator)
        };
        Self(clamp_i128(micros))
    }

    /// Sample frame containing this offset at `rate` Hz, rounded down.
    #[must_use]
    pub fn to_frames(self, rate: u32) -> i64 {
        if rate == 0 {
            return 0;
        }
        let frames = i128::from(self.0) * i128::from(rate) / i128::from(MICROS_PER_SECOND);
        clamp_i128(frames)
    }

    /// Saturating addition.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Saturating subtraction.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// Clamp into `[low, high]`; an inverted bound collapses to `low`.
    #[must_use]
    pub const fn clamp_to(self, low: Self, high: Self) -> Self {
        if high.0 < low.0 {
            return low;
        }
        if self.0 < low.0 {
            low
        } else if self.0 > high.0 {
            high
        } else {
            self
        }
    }

    /// Absolute value.
    #[must_use]
    pub const fn abs(self) -> Self {
        Self(self.0.saturating_abs())
    }

    /// Scale by the exact ratio `num / den`, rounding towards zero.
    ///
    /// `den == 0` is treated as a no-op rather than a panic for the same
    /// reason as [`Micros::from_frames`].
    #[must_use]
    pub fn scaled(self, num: u32, den: u32) -> Self {
        if den == 0 {
            return self;
        }
        let value = i128::from(self.0) * i128::from(num) / i128::from(den);
        Self(clamp_i128(value))
    }
}

fn clamp_i128(value: i128) -> i64 {
    if value > i128::from(i64::MAX) {
        i64::MAX
    } else if value < i128::from(i64::MIN) {
        i64::MIN
    } else {
        value as i64
    }
}

impl Add for Micros {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        self.saturating_add(rhs)
    }
}

impl AddAssign for Micros {
    fn add_assign(&mut self, rhs: Self) {
        *self = self.saturating_add(rhs);
    }
}

impl Sub for Micros {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.saturating_sub(rhs)
    }
}

impl SubAssign for Micros {
    fn sub_assign(&mut self, rhs: Self) {
        *self = self.saturating_sub(rhs);
    }
}

impl Neg for Micros {
    type Output = Self;
    fn neg(self) -> Self {
        Self(self.0.saturating_neg())
    }
}

impl fmt::Display for Micros {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}µs", self.0)
    }
}

/// A half-open `[start, end)` span on the timeline, never inverted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TimeSpan {
    start: Micros,
    end: Micros,
}

impl TimeSpan {
    /// Build a span, swapping the bounds if they arrive inverted.
    #[must_use]
    pub fn new(a: Micros, b: Micros) -> Self {
        if b < a {
            Self { start: b, end: a }
        } else {
            Self { start: a, end: b }
        }
    }

    /// Build from a start and a non-negative length.
    #[must_use]
    pub fn from_len(start: Micros, len: Micros) -> Self {
        Self::new(start, start.saturating_add(Micros(len.0.max(0))))
    }

    /// Start offset.
    #[must_use]
    pub const fn start(self) -> Micros {
        self.start
    }

    /// End offset, exclusive.
    #[must_use]
    pub const fn end(self) -> Micros {
        self.end
    }

    /// Length of the span.
    #[must_use]
    pub const fn duration(self) -> Micros {
        Micros(self.end.0.saturating_sub(self.start.0))
    }

    /// Whether the span covers no time at all.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end.0 <= self.start.0
    }

    /// Whether `t` lies in `[start, end)`.
    #[must_use]
    pub const fn contains(self, t: Micros) -> bool {
        t.0 >= self.start.0 && t.0 < self.end.0
    }

    /// Whether the two spans share at least one microsecond.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start.0 < other.end.0 && other.start.0 < self.end.0
    }

    /// The shared part of two spans, if any.
    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (end > start).then_some(Self { start, end })
    }

    /// Shift both bounds by `delta`.
    #[must_use]
    pub fn shifted(self, delta: Micros) -> Self {
        Self {
            start: self.start.saturating_add(delta),
            end: self.end.saturating_add(delta),
        }
    }

    /// Clamp both bounds into `[low, high]`.
    #[must_use]
    pub fn clamped(self, low: Micros, high: Micros) -> Self {
        Self {
            start: self.start.clamp_to(low, high),
            end: self.end.clamp_to(low, high),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_conversion_round_trips_on_frame_boundaries() {
        for rate in [8_000u32, 16_000, 44_100, 48_000] {
            for frame in [0i64, 1, 7, 1_000, 999_983, 10_000_000] {
                let t = Micros::from_frames(frame, rate);
                assert_eq!(t.to_frames(rate), frame, "rate {rate} frame {frame}");
            }
        }
    }

    #[test]
    fn three_hours_of_microseconds_is_nowhere_near_the_i64_ceiling() {
        let three_hours = Micros::from_secs(3 * 3600);
        assert_eq!(three_hours.get(), 10_800_000_000);
        assert!(three_hours.get() < i64::MAX / 1_000_000);
    }

    #[test]
    fn millisecond_truncation_floors_towards_negative_infinity() {
        assert_eq!(Micros::new(1_999).as_millis(), 1);
        assert_eq!(Micros::new(-1).as_millis(), -1);
    }

    #[test]
    fn zero_rate_never_panics() {
        assert_eq!(Micros::from_frames(100, 0), Micros::ZERO);
        assert_eq!(Micros::from_secs(5).to_frames(0), 0);
    }

    #[test]
    fn spans_normalise_inverted_bounds() {
        let span = TimeSpan::new(Micros::from_secs(5), Micros::from_secs(1));
        assert_eq!(span.start(), Micros::from_secs(1));
        assert_eq!(span.duration(), Micros::from_secs(4));
    }

    #[test]
    fn intersect_is_none_when_disjoint() {
        let a = TimeSpan::new(Micros::ZERO, Micros::from_secs(1));
        let b = TimeSpan::new(Micros::from_secs(2), Micros::from_secs(3));
        assert!(a.intersect(b).is_none());
        assert!(!a.overlaps(b));
    }

    #[test]
    fn exact_ratio_scaling_does_not_drift() {
        // 1.25x applied four times is exactly 2.44140625x, not an approximation.
        let mut t = Micros::from_secs(1);
        for _ in 0..4 {
            t = t.scaled(5, 4);
        }
        assert_eq!(t.get(), 2_441_406);
    }
}
