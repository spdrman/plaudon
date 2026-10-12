//! The DET curve, EER and operating points (VP-180, VP-062).
//!
//! A trial is a (profile, test segment) pair with a boolean truth and one
//! score. Over a set of trials, a decision threshold `t` ("accept when
//! `score >= t`") produces two rates:
//!
//! ```text
//!   false-accept rate = non-target trials with score >= t / non-target trials
//!   false-reject rate =     target trials with score <  t /     target trials
//! ```
//!
//! The DET curve is those two rates at every threshold the sample can
//! distinguish, which is the set of observed scores plus one point above the
//! maximum. Three deliberate properties:
//!
//! * **Thresholds are observed scores.** Interpolating a curve through
//!   thresholds the sample never produced would invent resolution.
//! * **The EER is interpolated, the operating point is not.** The equal-error
//!   rate genuinely falls between two sample points, so it is interpolated
//!   linearly and the bracketing points are published. A shipped threshold is
//!   a number the system will compare against, so it is always an observed
//!   score with an observed false-accept rate — never an interpolation.
//! * **A rate below the sample's resolution is refused.** With 40 non-target
//!   trials the smallest non-zero false-accept rate is 2.5%, so "≤ 1%" can
//!   only mean "zero observed", which is not the same statement.
//!   [`DetCurve::resolves_rate`] says so and [`super::calibration`] refuses to
//!   publish a default from such a sample.

use serde::{Deserialize, Serialize};

use super::fixtures::Condition;

/// False-accept ceilings a report publishes a threshold for.
///
/// A committed artifact carries these points rather than every observed
/// threshold: a trial set of a few thousand pairs has a few thousand distinct
/// scores, and a benchmark file nobody can read in a review is not evidence.
/// The reported points are observed thresholds, so nothing is interpolated.
pub const REPORT_FALSE_ACCEPT_RATES: &[f64] =
    &[0.5, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005, 0.002, 0.001];

/// One scored trial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrialScore {
    /// Identifier, stable across runs: `<condition>/<profile>/<segment>`.
    pub trial_id: String,
    /// Condition the test segment was put through.
    pub condition: Condition,
    /// Whether the profile and the test segment are the same speaker.
    pub is_target: bool,
    /// The AS-Norm score the matching core produced.
    pub score: f64,
}

/// Why a curve could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DetError {
    /// No same-speaker trials, so there is no false-reject rate.
    #[error("a DET curve needs at least one target trial")]
    NoTargets,

    /// No different-speaker trials, so there is no false-accept rate.
    #[error("a DET curve needs at least one non-target trial")]
    NoNonTargets,

    /// A score was `NaN` or infinite.
    #[error("trial `{0}` has a non-finite score")]
    NonFiniteScore(&'static str),
}

/// One point of the DET curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DetPoint {
    /// Decision threshold: accept when `score >= threshold`.
    pub threshold: f64,
    /// Non-target trials accepted, as a fraction.
    pub false_accept_rate: f64,
    /// Target trials rejected, as a fraction.
    pub false_reject_rate: f64,
}

/// The equal-error point, interpolated between two sample points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqualErrorRate {
    /// The rate at which the two errors are equal.
    pub rate: f64,
    /// Threshold at which that happens.
    pub threshold: f64,
    /// Whether `rate` sits exactly on an observed point rather than between
    /// two of them.
    pub exact: bool,
}

/// A threshold choice with the errors it buys.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OperatingPoint {
    /// Threshold, always an observed score.
    pub threshold: f64,
    /// Non-target trials this threshold accepts.
    pub false_accept_rate: f64,
    /// Target trials this threshold rejects.
    pub false_reject_rate: f64,
    /// The constraint this point was selected to satisfy.
    pub max_false_accept_rate: f64,
}

/// A DET curve over one set of trials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetCurve {
    points: Vec<DetPoint>,
    targets: usize,
    nontargets: usize,
}

impl DetCurve {
    /// Build the curve from scored trials.
    pub fn from_trials(trials: &[TrialScore]) -> Result<Self, DetError> {
        let mut targets: Vec<f64> = Vec::new();
        let mut nontargets: Vec<f64> = Vec::new();
        for trial in trials {
            if !trial.score.is_finite() {
                return Err(DetError::NonFiniteScore("score"));
            }
            if trial.is_target {
                targets.push(trial.score);
            } else {
                nontargets.push(trial.score);
            }
        }
        if targets.is_empty() {
            return Err(DetError::NoTargets);
        }
        if nontargets.is_empty() {
            return Err(DetError::NoNonTargets);
        }

        let mut thresholds: Vec<f64> = targets.iter().chain(nontargets.iter()).copied().collect();
        thresholds.sort_by(f64::total_cmp);
        thresholds.dedup();
        // One threshold above every observed score: accept nothing.
        let above = thresholds
            .last()
            .copied()
            .expect("thresholds are non-empty")
            + 1.0;
        thresholds.push(above);

        let target_count = targets.len() as f64;
        let nontarget_count = nontargets.len() as f64;
        let points = thresholds
            .into_iter()
            .map(|threshold| {
                let accepted_nontargets = nontargets
                    .iter()
                    .filter(|score| **score >= threshold)
                    .count() as f64;
                let rejected_targets =
                    targets.iter().filter(|score| **score < threshold).count() as f64;
                DetPoint {
                    threshold,
                    false_accept_rate: accepted_nontargets / nontarget_count,
                    false_reject_rate: rejected_targets / target_count,
                }
            })
            .collect();

        Ok(Self {
            points,
            targets: targets.len(),
            nontargets: nontargets.len(),
        })
    }

    /// Points in increasing threshold order.
    #[must_use]
    pub fn points(&self) -> &[DetPoint] {
        &self.points
    }

    /// Target trials behind the curve.
    #[must_use]
    pub fn targets(&self) -> usize {
        self.targets
    }

    /// Non-target trials behind the curve.
    #[must_use]
    pub fn nontargets(&self) -> usize {
        self.nontargets
    }

    /// Smallest non-zero false-accept rate this sample can express.
    #[must_use]
    pub fn false_accept_resolution(&self) -> f64 {
        1.0 / self.nontargets as f64
    }

    /// Whether the sample can distinguish `rate` from zero.
    ///
    /// A false-accept constraint of 1% needs at least 100 non-target trials;
    /// below that, "≤ 1%" and "no false accept was observed" are the same
    /// observation and must not be published as the first one.
    #[must_use]
    pub fn resolves_rate(&self, rate: f64) -> bool {
        rate > 0.0 && self.false_accept_resolution() <= rate
    }

    /// The equal-error rate.
    ///
    /// Walks the curve in increasing threshold order, where the false-accept
    /// rate falls and the false-reject rate rises, and takes the crossing. If
    /// a point has both rates equal, that point is the answer and `exact` is
    /// true; otherwise the crossing is interpolated linearly between the
    /// bracketing points.
    #[must_use]
    pub fn equal_error_rate(&self) -> EqualErrorRate {
        let mut previous: Option<DetPoint> = None;
        for point in &self.points {
            let difference = point.false_accept_rate - point.false_reject_rate;
            if difference == 0.0 {
                return EqualErrorRate {
                    rate: point.false_accept_rate,
                    threshold: point.threshold,
                    exact: true,
                };
            }
            if difference < 0.0 {
                let Some(before) = previous else {
                    // The first point already has FA < FR: nothing to bracket.
                    return EqualErrorRate {
                        rate: f64::midpoint(point.false_accept_rate, point.false_reject_rate),
                        threshold: point.threshold,
                        exact: false,
                    };
                };
                let high = before.false_accept_rate - before.false_reject_rate;
                let fraction = high / (high - difference);
                let rate = before.false_accept_rate
                    + fraction * (point.false_accept_rate - before.false_accept_rate);
                let threshold = before.threshold + fraction * (point.threshold - before.threshold);
                return EqualErrorRate {
                    rate,
                    threshold,
                    exact: false,
                };
            }
            previous = Some(*point);
        }
        // FA never drops below FR, even where nothing is accepted: the last
        // point (accept nothing) has FA = 0 and FR = 1, so this is unreachable
        // for a non-empty curve, but a total function is cheaper than a panic.
        let last = *self.points.last().expect("a curve has points");
        EqualErrorRate {
            rate: f64::midpoint(last.false_accept_rate, last.false_reject_rate),
            threshold: last.threshold,
            exact: false,
        }
    }

    /// The loosest threshold whose false-accept rate is at or below `max_rate`.
    ///
    /// Loosest, because among compliant thresholds that is the one with the
    /// fewest false rejects — VP-062's trade is "as few missed suggestions as
    /// possible subject to the false-accept ceiling".
    #[must_use]
    pub fn operating_point(&self, max_rate: f64) -> Option<OperatingPoint> {
        self.points
            .iter()
            .find(|point| point.false_accept_rate <= max_rate)
            .map(|point| OperatingPoint {
                threshold: point.threshold,
                false_accept_rate: point.false_accept_rate,
                false_reject_rate: point.false_reject_rate,
                max_false_accept_rate: max_rate,
            })
    }

    /// The operating points a report publishes ([`REPORT_FALSE_ACCEPT_RATES`]).
    ///
    /// Ceilings the sample cannot resolve are skipped rather than reported as
    /// "0% false accepts": see [`Self::resolves_rate`].
    #[must_use]
    pub fn summary_points(&self) -> Vec<OperatingPoint> {
        REPORT_FALSE_ACCEPT_RATES
            .iter()
            .filter(|rate| self.resolves_rate(**rate))
            .filter_map(|rate| self.operating_point(*rate))
            .collect()
    }
}
