//! Per-profile calibration from the user's own decisions (VP-063, ADR-0082 §5).
//!
//! The only evidence this module consumes is what VP-063 allows to exist: a
//! score and a boolean outcome. No audio, no new embedding, no demographic
//! attribute — ADR-0082 decision 5 forbids collecting attributes to "correct"
//! bias, and §1.3 (VP-005) forbids inferring them at all.
//!
//! The policy is deliberately one-sided. A *rejected* suggestion is evidence
//! that this profile's operating point is too loose, and the offset moves up
//! far enough to put that score back in the weak band. A *confirmed* weak
//! signal is evidence that it is too tight, and the offset moves down. When
//! both kinds of evidence exist the raise wins, because VP-062 biases this
//! feature towards false rejects: a missed suggestion costs one manual rename,
//! a wrong suggestion teaches distrust.

use serde::{Deserialize, Serialize};

use super::bands::{ProfileOffset, ThresholdSet};
use super::embedding::NormalisedScore;

/// How many decisions a profile needs before its operating point moves.
///
/// One click is not calibration data; it is also the shape of an accident.
/// Three is the smallest number that can show a pattern rather than a slip.
pub const MIN_DECISIONS_FOR_OFFSET: usize = 3;

/// How far past the offending score the operating point moves, as a fraction
/// of the configured weak-band width.
///
/// Expressed as a fraction rather than an absolute number because the only
/// scale this module may assume is the one the harness published (VP-062).
pub const GUARD_FRACTION_OF_BAND: f32 = 0.1;

/// What the user did with a suggestion.
///
/// A *corrected* suggestion (VP-216: "not them — it was Ana") is two outcomes:
/// a [`Decision::Rejected`] for the suggested person and a
/// [`Decision::Confirmed`] for the corrected one. Mapping it that way is the
/// caller's job, because only the caller knows both person ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The user confirmed that the cluster is this person.
    Confirmed,
    /// The user rejected the suggestion (including by correcting it).
    Rejected,
}

/// One settled decision about one profile: a score and a boolean (VP-063).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CalibrationOutcome {
    /// The AS-Norm score that was shown.
    pub score: NormalisedScore,
    /// What the user decided.
    pub decision: Decision,
}

impl CalibrationOutcome {
    /// Record a confirmation.
    #[must_use]
    pub fn confirmed(score: NormalisedScore) -> Self {
        Self {
            score,
            decision: Decision::Confirmed,
        }
    }

    /// Record a rejection.
    #[must_use]
    pub fn rejected(score: NormalisedScore) -> Self {
        Self {
            score,
            decision: Decision::Rejected,
        }
    }
}

/// Derive a profile's threshold offset from its settled decisions.
///
/// The result is bounded by [`ThresholdSet::max_profile_offset`] and depends
/// only on the *set* of outcomes: feeding the same decisions in a different
/// order gives the same offset, so a reordered history (a re-read of the
/// database, a different sort) cannot change a profile's behaviour.
#[must_use]
pub fn calibrated_offset(outcomes: &[CalibrationOutcome], set: ThresholdSet) -> ProfileOffset {
    if outcomes.len() < MIN_DECISIONS_FOR_OFFSET {
        return ProfileOffset::ZERO;
    }

    let suggest = set.suggest().value();
    let guard = set.band_width() * GUARD_FRACTION_OF_BAND;

    // A rejection at or above the suggest threshold is a false accept the user
    // had to clean up: move the operating point above it.
    let worst_false_accept = outcomes
        .iter()
        .filter(|outcome| outcome.decision == Decision::Rejected)
        .map(|outcome| outcome.score.value())
        .filter(|score| *score >= suggest)
        .max_by(f32::total_cmp);

    if let Some(score) = worst_false_accept {
        return ProfileOffset::clamped(score - suggest + guard, set);
    }

    // A confirmation below the suggest threshold is a suggestion the user had
    // to find in the weak band: move the operating point down to catch it.
    let lowest_rescued = outcomes
        .iter()
        .filter(|outcome| outcome.decision == Decision::Confirmed)
        .map(|outcome| outcome.score.value())
        .filter(|score| *score < suggest)
        .min_by(f32::total_cmp);

    match lowest_rescued {
        Some(score) => ProfileOffset::clamped(score - suggest - guard, set),
        None => ProfileOffset::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::{calibrated_offset, CalibrationOutcome, Decision, MIN_DECISIONS_FOR_OFFSET};
    use crate::voice_profiles::matching::bands::{Band, ProfileOffset};
    use crate::voice_profiles::matching::embedding::NormalisedScore;
    use crate::voice_profiles::matching::test_support::thresholds;

    fn score(value: f32) -> NormalisedScore {
        NormalisedScore::new(value).expect("finite score")
    }

    #[test]
    fn a_profile_with_too_little_evidence_keeps_the_shipped_operating_point() {
        let set = thresholds();
        let outcomes = vec![
            CalibrationOutcome::rejected(score(3.0)),
            CalibrationOutcome::rejected(score(3.1)),
        ];
        assert!(outcomes.len() < MIN_DECISIONS_FOR_OFFSET);
        assert_eq!(calibrated_offset(&outcomes, set), ProfileOffset::ZERO);
    }

    #[test]
    fn repeated_rejections_raise_the_threshold_past_the_worst_false_accept() {
        let set = thresholds();
        let outcomes = vec![
            CalibrationOutcome::rejected(score(2.52)),
            CalibrationOutcome::rejected(score(2.55)),
            CalibrationOutcome::confirmed(score(4.0)),
        ];
        let offset = calibrated_offset(&outcomes, set);
        // Worst false accept 2.55, suggest 2.5, guard 0.1 × 1.5 = 0.15.
        assert!((offset.value() - 0.2).abs() < 1e-6, "{offset:?}");
        // The score that was wrongly suggested now lands in the weak band.
        assert_eq!(set.band(score(2.55), offset), Band::Weak);
        // And a genuinely strong score still reaches the suggest band.
        assert_eq!(set.band(score(4.0), offset), Band::Suggest);
    }

    #[test]
    fn confirmed_weak_signals_lower_the_threshold_to_catch_them() {
        let set = thresholds();
        let outcomes = vec![
            CalibrationOutcome::confirmed(score(2.45)),
            CalibrationOutcome::confirmed(score(2.48)),
            CalibrationOutcome::confirmed(score(3.0)),
        ];
        let offset = calibrated_offset(&outcomes, set);
        // Lowest rescued 2.45, suggest 2.5, guard 0.15 → −0.2.
        assert!((offset.value() + 0.2).abs() < 1e-6, "{offset:?}");
        assert_eq!(set.band(score(2.45), offset), Band::Suggest);
    }

    #[test]
    fn conflicting_evidence_is_resolved_towards_the_false_reject() {
        let set = thresholds();
        let outcomes = vec![
            CalibrationOutcome::confirmed(score(2.2)),
            CalibrationOutcome::rejected(score(2.7)),
            CalibrationOutcome::confirmed(score(3.5)),
        ];
        let offset = calibrated_offset(&outcomes, set);
        assert!(offset.value() > 0.0, "{offset:?}");
    }

    #[test]
    fn evidence_that_agrees_with_the_shipped_point_moves_nothing() {
        let set = thresholds();
        let outcomes = vec![
            CalibrationOutcome::confirmed(score(3.0)),
            CalibrationOutcome::confirmed(score(3.4)),
            CalibrationOutcome::rejected(score(1.2)),
            CalibrationOutcome::rejected(score(1.4)),
        ];
        assert_eq!(calibrated_offset(&outcomes, set), ProfileOffset::ZERO);
    }

    #[test]
    fn the_derived_offset_never_leaves_the_validated_bound() {
        let set = thresholds();
        let outcomes = vec![
            CalibrationOutcome::rejected(score(50.0)),
            CalibrationOutcome::rejected(score(49.0)),
            CalibrationOutcome::rejected(score(48.0)),
        ];
        let offset = calibrated_offset(&outcomes, set);
        assert_eq!(offset.value(), set.max_profile_offset());
    }

    #[test]
    fn an_outcome_carries_only_a_score_and_a_boolean() {
        let outcome = CalibrationOutcome::rejected(score(2.9));
        assert_eq!(outcome.decision, Decision::Rejected);
        let json = serde_json::to_value(outcome).expect("serialize");
        let object = json.as_object().expect("object");
        assert_eq!(object.len(), 2, "{json}");
        assert!(object.contains_key("score") && object.contains_key("decision"));
        assert_eq!(object["decision"], "rejected");
    }
}
