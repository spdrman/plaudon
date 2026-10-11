//! Non-inverted millisecond time ranges within a media source.

use serde::{Deserialize, Serialize};

/// A source-time offset in milliseconds from the start of the media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceTimeMs(u64);

impl SourceTimeMs {
    /// The start of the media.
    pub const ZERO: Self = Self(0);

    /// Construct an offset. Offsets are unsigned, so negative times are
    /// unrepresentable rather than silently clamped.
    #[must_use]
    pub const fn new(millis: u64) -> Self {
        Self(millis)
    }

    /// Milliseconds from the start of the media.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Error returned when a time range cannot be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeRangeError {
    /// `end` precedes `start`.
    #[error("time range is inverted: end {end_ms} precedes start {start_ms}")]
    Inverted {
        /// Proposed start offset.
        start_ms: u64,
        /// Proposed end offset.
        end_ms: u64,
    },
}

/// A closed-open millisecond range inside one media asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRangeMs {
    start: SourceTimeMs,
    end: SourceTimeMs,
}

impl TimeRangeMs {
    /// Construct a range, rejecting inverted ranges.
    pub fn new(start: SourceTimeMs, end: SourceTimeMs) -> Result<Self, TimeRangeError> {
        if end.get() < start.get() {
            return Err(TimeRangeError::Inverted {
                start_ms: start.get(),
                end_ms: end.get(),
            });
        }
        Ok(Self { start, end })
    }

    /// Start offset.
    #[must_use]
    pub const fn start(self) -> SourceTimeMs {
        self.start
    }

    /// End offset.
    #[must_use]
    pub const fn end(self) -> SourceTimeMs {
        self.end
    }

    /// Duration covered by this range.
    #[must_use]
    pub const fn duration_ms(self) -> u64 {
        self.end.get() - self.start.get()
    }

    /// Whether two ranges share at least one millisecond.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start.get() < other.end.get() && other.start.get() < self.end.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverted_ranges_are_rejected() {
        let err = TimeRangeMs::new(SourceTimeMs::new(10), SourceTimeMs::new(9)).unwrap_err();
        assert_eq!(
            err,
            TimeRangeError::Inverted {
                start_ms: 10,
                end_ms: 9
            }
        );
    }

    #[test]
    fn empty_ranges_are_allowed_and_have_zero_duration() {
        let range = TimeRangeMs::new(SourceTimeMs::new(5), SourceTimeMs::new(5)).unwrap();
        assert_eq!(range.duration_ms(), 0);
    }

    #[test]
    fn adjacent_ranges_do_not_overlap() {
        let a = TimeRangeMs::new(SourceTimeMs::ZERO, SourceTimeMs::new(100)).unwrap();
        let b = TimeRangeMs::new(SourceTimeMs::new(100), SourceTimeMs::new(200)).unwrap();
        assert!(!a.overlaps(b));
        assert!(
            a.overlaps(TimeRangeMs::new(SourceTimeMs::new(99), SourceTimeMs::new(150)).unwrap())
        );
    }
}
