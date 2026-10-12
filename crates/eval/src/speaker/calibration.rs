//! Deriving the two shipped thresholds from a DET curve (VP-062, VP-180).
//!
//! > the two thresholds are set from the §17 evaluation harness's DET curve
//! > such that at `T_suggest` the false-accept rate on the harness is ≤ 1%
//!
//! That sentence is this module. `T_suggest` is the loosest observed threshold
//! whose false-accept rate is at or below [`SUGGEST_MAX_FALSE_ACCEPT`];
//! `T_floor` is the same construction at the looser
//! [`FLOOR_MAX_FALSE_ACCEPT`], because the weak band is a detail view, not a
//! notification, and its job is to be a *hint* rather than a second claim.
//!
//! Four refusals, each of which produces a [`CalibrationStatus`] instead of a
//! number:
//!
//! * the sample cannot resolve a 1% rate (fewer than 100 non-target trials);
//! * no observed threshold meets the ceiling;
//! * the two thresholds collapse onto the same score, so there would be no
//!   weak band at all;
//! * the embeddings were scripted, which is not a measurement of a model.
//!
//! The last one does not block the arithmetic — a scripted run still produces
//! thresholds, which is how the harness is tested — but it is recorded in the
//! provenance and makes [`ThresholdCalibration::is_shippable_default`] false.
//! Shipping a default from scripted vectors would be inventing a measurement.

use plaudon_app_core::voice_profiles::matching::{NormalisedScore, ThresholdError, ThresholdSet};
use serde::{Deserialize, Serialize};

use super::det::{DetCurve, EqualErrorRate, OperatingPoint};
use super::embeddings::SourceKind;

/// False-accept ceiling for the suggestion threshold (VP-062).
pub const SUGGEST_MAX_FALSE_ACCEPT: f64 = 0.01;

/// False-accept ceiling for the weak-signal floor.
///
/// Ten times looser than the suggestion ceiling: the weak band never produces
/// a notification, a badge or a queue entry (VP-060), so its cost of being
/// wrong is a line in a detail view. One order of magnitude is a choice, made
/// here once and published with the artifact rather than tuned per release.
pub const FLOOR_MAX_FALSE_ACCEPT: f64 = 0.10;

/// Non-target trials needed before a 1% ceiling means anything.
pub const MIN_NONTARGETS_FOR_SUGGEST: usize = 100;

/// Per-profile offset bound, as a fraction of the weak band's width.
///
/// VP-063 lets per-profile calibration slide the operating point "within
/// harness-validated bounds". A quarter of the band keeps a calibrated profile
/// inside the region the harness actually measured.
pub const PROFILE_OFFSET_FRACTION_OF_BAND: f32 = 0.25;

/// Outcome of a calibration attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CalibrationStatus {
    /// Both thresholds were derived from observed points.
    Derived,

    /// The trial set is too small to resolve the false-accept ceiling.
    InsufficientTrials {
        /// Non-target trials the ceiling needs.
        required_nontargets: usize,
        /// Non-target trials the run had.
        observed_nontargets: usize,
    },

    /// No observed threshold meets the ceiling.
    CeilingUnreachable {
        /// Ceiling that could not be met.
        max_false_accept_rate: f64,
        /// Lowest false-accept rate any observed threshold achieved.
        best_false_accept_rate: f64,
    },

    /// The two thresholds landed on the same score, leaving no weak band.
    NoWeakBand {
        /// The score both landed on.
        threshold: f64,
    },

    /// The thresholds were rejected by the matching core's own invariants.
    Rejected {
        /// Why `ThresholdSet::new` refused them.
        reason: String,
    },
}

impl CalibrationStatus {
    /// Whether thresholds exist at all.
    #[must_use]
    pub const fn is_derived(&self) -> bool {
        matches!(self, Self::Derived)
    }
}

/// What the calibration was computed from (ADR-0081 §4 provenance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationProvenance {
    /// How the embeddings were produced.
    pub embedding_source: SourceKind,
    /// Model identifier the scores are bound to.
    pub model_id: String,
    /// Checkpoint version.
    pub model_version: String,
    /// Checkpoint digest, so a silent model change invalidates the thresholds.
    pub model_sha256: String,
    /// Fixture library the trials came from.
    pub library_id: String,
    /// Digest of that library's manifest.
    pub manifest_sha256: String,
    /// Target trials behind the curve.
    pub targets: usize,
    /// Non-target trials behind the curve.
    pub nontargets: usize,
    /// Smallest non-zero false-accept rate the sample can express.
    pub false_accept_resolution: f64,
}

/// The derived operating points, with everything needed to audit them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThresholdCalibration {
    /// Whether thresholds were derived, and why not when they were not.
    pub status: CalibrationStatus,
    /// The suggestion operating point, when derived.
    pub suggest: Option<OperatingPoint>,
    /// The weak-signal floor, when derived.
    pub floor: Option<OperatingPoint>,
    /// Bound on per-profile calibration offsets (VP-063), when derived.
    pub max_profile_offset: Option<f32>,
    /// The equal-error point of the same curve, for context.
    pub equal_error_rate: EqualErrorRate,
    /// What produced these numbers.
    pub provenance: CalibrationProvenance,
}

impl ThresholdCalibration {
    /// The thresholds as the matching core's own type, when derived.
    #[must_use]
    pub fn threshold_set(&self) -> Option<ThresholdSet> {
        let (suggest, floor, offset) = (self.suggest?, self.floor?, self.max_profile_offset?);
        let floor_score = NormalisedScore::new(floor.threshold as f32).ok()?;
        let suggest_score = NormalisedScore::new(suggest.threshold as f32).ok()?;
        ThresholdSet::new(floor_score, suggest_score, offset).ok()
    }

    /// Whether these thresholds may ship as the product default.
    ///
    /// Requires both that they were derived and that they came from real
    /// model embeddings: a scripted run measures the harness (VP-183), and
    /// VP-062 says the shipped values are an output of the harness measuring a
    /// pipeline, not of its self-test.
    #[must_use]
    pub fn is_shippable_default(&self) -> bool {
        self.status.is_derived() && self.provenance.embedding_source.is_model_measurement()
    }
}

/// Derive the two thresholds from a curve.
#[must_use]
pub fn calibrate(curve: &DetCurve, provenance: CalibrationProvenance) -> ThresholdCalibration {
    let equal_error_rate = curve.equal_error_rate();
    let refuse = |status: CalibrationStatus| ThresholdCalibration {
        status,
        suggest: None,
        floor: None,
        max_profile_offset: None,
        equal_error_rate,
        provenance: provenance.clone(),
    };

    if !curve.resolves_rate(SUGGEST_MAX_FALSE_ACCEPT) {
        return refuse(CalibrationStatus::InsufficientTrials {
            required_nontargets: MIN_NONTARGETS_FOR_SUGGEST,
            observed_nontargets: curve.nontargets(),
        });
    }

    let best_false_accept_rate = curve
        .points()
        .iter()
        .map(|point| point.false_accept_rate)
        .fold(f64::INFINITY, f64::min);

    let Some(suggest) = curve.operating_point(SUGGEST_MAX_FALSE_ACCEPT) else {
        return refuse(CalibrationStatus::CeilingUnreachable {
            max_false_accept_rate: SUGGEST_MAX_FALSE_ACCEPT,
            best_false_accept_rate,
        });
    };
    let Some(floor) = curve.operating_point(FLOOR_MAX_FALSE_ACCEPT) else {
        return refuse(CalibrationStatus::CeilingUnreachable {
            max_false_accept_rate: FLOOR_MAX_FALSE_ACCEPT,
            best_false_accept_rate,
        });
    };
    if floor.threshold >= suggest.threshold {
        return refuse(CalibrationStatus::NoWeakBand {
            threshold: suggest.threshold,
        });
    }

    let band_width = (suggest.threshold - floor.threshold) as f32;
    let max_profile_offset = band_width * PROFILE_OFFSET_FRACTION_OF_BAND;
    match build_set(floor.threshold, suggest.threshold, max_profile_offset) {
        Ok(_) => ThresholdCalibration {
            status: CalibrationStatus::Derived,
            suggest: Some(suggest),
            floor: Some(floor),
            max_profile_offset: Some(max_profile_offset),
            equal_error_rate,
            provenance,
        },
        Err(reason) => refuse(CalibrationStatus::Rejected { reason }),
    }
}

/// Build the matching core's threshold type, rendering its refusal as text.
fn build_set(floor: f64, suggest: f64, offset: f32) -> Result<ThresholdSet, String> {
    let floor = NormalisedScore::new(floor as f32).map_err(|error| error.to_string())?;
    let suggest = NormalisedScore::new(suggest as f32).map_err(|error| error.to_string())?;
    ThresholdSet::new(floor, suggest, offset).map_err(|error: ThresholdError| error.to_string())
}
