//! Two thresholds, three outcomes, and the per-profile offset (VP-060, VP-063).
//!
//! The band is the whole decision surface of this feature. There is no fourth
//! band above [`Band::Suggest`] and there never will be one: ADR-0082 decision
//! 1 forbids auto-assignment *at any score*, because the error of a
//! VoxCeleb-trained model is demographically uneven and an auto-assign
//! threshold is therefore a policy about whose records get silently corrupted.
//!
//! The numbers are deliberately absent from this file. VP-062 makes the two
//! operating points an output of the §17 evaluation harness (false-accept rate
//! ≤ 1% at the suggest threshold), versioned with the checkpoint, so a
//! [`ThresholdSet`] is loaded beside the model and never defaulted in code.

use serde::{Deserialize, Serialize};

use super::embedding::NormalisedScore;

/// What a (cluster, profile) score means (VP-060).
///
/// The ordering is deliberate: `None < Weak < Suggest`, so "stronger band" is
/// comparable, and `Band::Suggest` is the strongest outcome that exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// Below the floor: nothing is recorded beyond the job's run metadata.
    None,
    /// Between floor and suggest: shown only in the per-recording speaker
    /// panel's detail view as "possible" — never a notification, badge or
    /// queue entry.
    Weak,
    /// At or above the suggest threshold: a suggestion is recorded for user
    /// review. A suggestion is still only a suggestion — confirming it is a
    /// user action (ADR-0082).
    Suggest,
}

impl Band {
    /// Whether this band is written to `speaker_suggestions` (§7.2).
    ///
    /// `Band::None` records nothing, which is what makes the open-set common
    /// case cheap (VP-060).
    #[must_use]
    pub const fn is_recorded(self) -> bool {
        matches!(self, Band::Weak | Band::Suggest)
    }

    /// The `band` column value for a recorded band.
    #[must_use]
    pub const fn column_value(self) -> Option<&'static str> {
        match self {
            Band::None => None,
            Band::Weak => Some("weak"),
            Band::Suggest => Some("suggest"),
        }
    }

    /// How this band is worded where a user sees it (VP-172).
    ///
    /// `Band::None` has no wording because it is never recorded and so never
    /// reaches a surface. Going through [`plaudon_domain::speaker::MatchBand`]
    /// keeps the stored value and the sentence from drifting apart.
    #[must_use]
    pub const fn display_band(self) -> Option<plaudon_domain::speaker::MatchBand> {
        match self {
            Band::None => None,
            Band::Weak => Some(plaudon_domain::speaker::MatchBand::Possible),
            Band::Suggest => Some(plaudon_domain::speaker::MatchBand::Strong),
        }
    }
}

/// Why a threshold set was refused.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum ThresholdError {
    /// The suggest threshold is not above the floor.
    #[error("suggest threshold {suggest} must be above the floor {floor}")]
    NotOrdered {
        /// Floor that was offered.
        floor: f32,
        /// Suggest threshold that was offered.
        suggest: f32,
    },

    /// The harness-validated offset limit is negative or not finite.
    #[error("maximum per-profile offset {limit} must be finite and non-negative")]
    OffsetLimit {
        /// Limit that was offered.
        limit: f32,
    },

    /// A threshold value was not finite.
    #[error("threshold values must be finite")]
    NotFinite,

    /// A per-profile offset exceeded the harness-validated limit (VP-063).
    #[error("per-profile offset {requested} exceeds the validated limit of ±{limit}")]
    OffsetOutOfBounds {
        /// Offset that was requested.
        requested: f32,
        /// Limit from the threshold set.
        limit: f32,
    },
}

/// The operating points for one checkpoint, as produced by the §17 harness.
///
/// Serialized form (the file that ships beside the model, VP-051):
///
/// ```json
/// { "floor": 1.5, "suggest": 2.75, "max_profile_offset": 0.25 }
/// ```
///
/// Deserialization goes through the same validation as [`ThresholdSet::new`],
/// so a hand-edited or corrupt file cannot install an inverted band.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawThresholdSet", into = "RawThresholdSet")]
pub struct ThresholdSet {
    floor: NormalisedScore,
    suggest: NormalisedScore,
    max_profile_offset: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawThresholdSet {
    floor: f32,
    suggest: f32,
    max_profile_offset: f32,
}

impl TryFrom<RawThresholdSet> for ThresholdSet {
    type Error = ThresholdError;

    fn try_from(raw: RawThresholdSet) -> Result<Self, Self::Error> {
        let floor = NormalisedScore::new(raw.floor).map_err(|_| ThresholdError::NotFinite)?;
        let suggest = NormalisedScore::new(raw.suggest).map_err(|_| ThresholdError::NotFinite)?;
        Self::new(floor, suggest, raw.max_profile_offset)
    }
}

impl From<ThresholdSet> for RawThresholdSet {
    fn from(set: ThresholdSet) -> Self {
        Self {
            floor: set.floor.value(),
            suggest: set.suggest.value(),
            max_profile_offset: set.max_profile_offset,
        }
    }
}

impl ThresholdSet {
    /// Build an operating point.
    ///
    /// `max_profile_offset` is the bound VP-063 requires: per-profile
    /// calibration may slide this operating point, but only inside a range the
    /// harness validated.
    pub fn new(
        floor: NormalisedScore,
        suggest: NormalisedScore,
        max_profile_offset: f32,
    ) -> Result<Self, ThresholdError> {
        if suggest.value() <= floor.value() {
            return Err(ThresholdError::NotOrdered {
                floor: floor.value(),
                suggest: suggest.value(),
            });
        }
        if !max_profile_offset.is_finite() || max_profile_offset < 0.0 {
            return Err(ThresholdError::OffsetLimit {
                limit: max_profile_offset,
            });
        }
        Ok(Self {
            floor,
            suggest,
            max_profile_offset,
        })
    }

    /// Lower threshold: below this, nothing is recorded.
    #[must_use]
    pub fn floor(self) -> NormalisedScore {
        self.floor
    }

    /// Upper threshold: at or above this, a suggestion is recorded.
    #[must_use]
    pub fn suggest(self) -> NormalisedScore {
        self.suggest
    }

    /// Width of the weak band, used by calibration as its guard unit.
    #[must_use]
    pub fn band_width(self) -> f32 {
        self.suggest.value() - self.floor.value()
    }

    /// The harness-validated bound on a per-profile offset (VP-063).
    #[must_use]
    pub fn max_profile_offset(self) -> f32 {
        self.max_profile_offset
    }

    /// Band of `score` for a profile carrying `offset`.
    ///
    /// An offset slides the whole operating point for one profile, so the band
    /// width is invariant and the partition stays total: every finite score
    /// falls in exactly one band.
    #[must_use]
    pub fn band(self, score: NormalisedScore, offset: ProfileOffset) -> Band {
        let value = score.value();
        if value >= self.suggest.value() + offset.value() {
            Band::Suggest
        } else if value >= self.floor.value() + offset.value() {
            Band::Weak
        } else {
            Band::None
        }
    }
}

/// A per-profile threshold offset, inside the harness-validated bound.
///
/// VP-063: "every confirm/reject decision is recorded (as a boolean outcome and
/// score, not as new audio) and may adjust the *per-profile* threshold offset
/// within harness-validated bounds". The bound is carried by the
/// [`ThresholdSet`], so an offset cannot outlive the operating point it was
/// validated against: constructing one requires the set.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileOffset(f32);

impl ProfileOffset {
    /// No adjustment: the profile uses the shipped operating point.
    pub const ZERO: Self = Self(0.0);

    /// Wrap an offset, refusing anything outside the validated bound.
    pub fn new(value: f32, set: ThresholdSet) -> Result<Self, ThresholdError> {
        if !value.is_finite() {
            return Err(ThresholdError::NotFinite);
        }
        let limit = set.max_profile_offset();
        if value.abs() > limit {
            return Err(ThresholdError::OffsetOutOfBounds {
                requested: value,
                limit,
            });
        }
        Ok(Self(value))
    }

    /// Clamp an offset into the validated bound.
    ///
    /// Used by calibration, which derives a value and must never be able to
    /// push a profile outside the range the harness measured.
    #[must_use]
    pub fn clamped(value: f32, set: ThresholdSet) -> Self {
        if !value.is_finite() {
            return Self::ZERO;
        }
        let limit = set.max_profile_offset();
        Self(value.clamp(-limit, limit))
    }

    /// The offset.
    #[must_use]
    pub fn value(self) -> f32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{Band, ProfileOffset, ThresholdError, ThresholdSet};
    use crate::voice_profiles::matching::embedding::NormalisedScore;
    use crate::voice_profiles::matching::test_support::thresholds;

    fn score(value: f32) -> NormalisedScore {
        NormalisedScore::new(value).expect("finite score")
    }

    #[test]
    fn the_three_bands_partition_the_score_line() {
        let set = thresholds();
        assert_eq!(set.band(score(10.0), ProfileOffset::ZERO), Band::Suggest);
        assert_eq!(set.band(score(2.5), ProfileOffset::ZERO), Band::Suggest);
        assert_eq!(set.band(score(2.499), ProfileOffset::ZERO), Band::Weak);
        assert_eq!(set.band(score(1.0), ProfileOffset::ZERO), Band::Weak);
        assert_eq!(set.band(score(0.999), ProfileOffset::ZERO), Band::None);
        assert_eq!(set.band(score(-50.0), ProfileOffset::ZERO), Band::None);
    }

    #[test]
    fn only_the_two_upper_bands_are_recorded_and_none_implies_assignment() {
        assert!(!Band::None.is_recorded());
        assert!(Band::Weak.is_recorded());
        assert!(Band::Suggest.is_recorded());
        assert_eq!(Band::None.column_value(), None);
        assert_eq!(Band::Weak.column_value(), Some("weak"));
        assert_eq!(Band::Suggest.column_value(), Some("suggest"));
        // ADR-0082: `Suggest` is the strongest band that exists.
        assert_eq!(Band::Suggest.max(Band::Weak), Band::Suggest);
    }

    #[test]
    fn the_stored_band_value_and_the_displayed_band_agree_for_every_variant() {
        for band in [Band::None, Band::Weak, Band::Suggest] {
            let displayed = band
                .column_value()
                .and_then(plaudon_domain::speaker::MatchBand::from_column);
            assert_eq!(displayed, band.display_band(), "{band:?}");
            assert_eq!(displayed.is_some(), band.is_recorded(), "{band:?}");
        }
    }

    #[test]
    fn an_offset_slides_the_operating_point_and_keeps_the_band_width() {
        let set = thresholds();
        let raised = ProfileOffset::new(0.25, set).expect("offset");
        assert_eq!(set.band(score(2.5), raised), Band::Weak);
        assert_eq!(set.band(score(2.75), raised), Band::Suggest);
        assert_eq!(set.band(score(1.25), raised), Band::Weak);
        assert_eq!(set.band(score(1.24), raised), Band::None);

        let lowered = ProfileOffset::new(-0.25, set).expect("offset");
        assert_eq!(set.band(score(2.25), lowered), Band::Suggest);
        assert_eq!(set.band(score(0.75), lowered), Band::Weak);
        assert_eq!(set.band(score(0.74), lowered), Band::None);
    }

    #[test]
    fn an_offset_beyond_the_validated_bound_is_refused_and_a_derived_one_is_clamped() {
        let set = thresholds();
        let limit = set.max_profile_offset();
        assert_eq!(
            ProfileOffset::new(limit + 0.01, set),
            Err(ThresholdError::OffsetOutOfBounds {
                requested: limit + 0.01,
                limit
            })
        );
        assert_eq!(
            ProfileOffset::new(-limit - 0.01, set),
            Err(ThresholdError::OffsetOutOfBounds {
                requested: -limit - 0.01,
                limit
            })
        );
        assert_eq!(
            ProfileOffset::new(f32::NAN, set),
            Err(ThresholdError::NotFinite)
        );
        assert_eq!(ProfileOffset::clamped(99.0, set).value(), limit);
        assert_eq!(ProfileOffset::clamped(-99.0, set).value(), -limit);
        assert_eq!(ProfileOffset::clamped(f32::NAN, set), ProfileOffset::ZERO);
    }

    #[test]
    fn an_inverted_or_unbounded_threshold_set_is_refused() {
        assert_eq!(
            ThresholdSet::new(score(2.0), score(1.0), 0.1),
            Err(ThresholdError::NotOrdered {
                floor: 2.0,
                suggest: 1.0
            })
        );
        assert_eq!(
            ThresholdSet::new(score(1.0), score(1.0), 0.1),
            Err(ThresholdError::NotOrdered {
                floor: 1.0,
                suggest: 1.0
            })
        );
        assert_eq!(
            ThresholdSet::new(score(1.0), score(2.0), -0.1),
            Err(ThresholdError::OffsetLimit { limit: -0.1 })
        );
    }

    #[test]
    fn a_threshold_file_round_trips_and_an_invalid_one_fails_to_parse() {
        let set = thresholds();
        let json = serde_json::to_string(&set).expect("serialize");
        assert_eq!(
            json,
            r#"{"floor":1.0,"suggest":2.5,"max_profile_offset":0.25}"#
        );
        let parsed: ThresholdSet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, set);

        let inverted = r#"{"floor":2.5,"suggest":1.0,"max_profile_offset":0.25}"#;
        let error = serde_json::from_str::<ThresholdSet>(inverted).expect_err("inverted");
        assert!(
            error.to_string().contains("must be above the floor"),
            "{error}"
        );

        let unknown = r#"{"floor":1.0,"suggest":2.5,"max_profile_offset":0.25,"auto_assign":3.0}"#;
        assert!(
            serde_json::from_str::<ThresholdSet>(unknown).is_err(),
            "an auto-assign tier must not be representable"
        );
    }
}
