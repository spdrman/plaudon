//! Evaluation gates for model presets and derived claims (spec 17.6, 1.6).
//!
//! > We will not pick an Apple or OpenRouter model based on brand prestige.
//! > Before a model becomes `Recommended`, we will record results on a
//! > consent-cleared golden corpus with a fixed measurement protocol [...] We
//! > will evaluate `Supported` capabilities separately from `Recommended`
//! > quality.
//!
//! The separation is structural, not editorial:
//!
//! * [`Claim::Supported`] is a capability statement (spec 1.6: repeatable
//!   hardware/OS validation plus security, integrity and compatibility gates).
//!   It never consults a quality measurement and
//!   [`Claim::implies_quality`] returns `false` for it, so no caller can quote
//!   a `Supported` decision as evidence of transcription quality.
//! * [`Claim::Recommended`] requires a stored evaluation record produced on
//!   the golden corpus for *that exact engine and model*. An absent record, a
//!   record for a different model, a record measured on a different corpus
//!   revision, an empty sample or a record older than the freshness window all
//!   produce a typed [`GateError`]. There is no "probably fine" path.
//!
//! No numeric quality threshold is asserted here: spec 17.6 defers the
//! numbers. When the project freezes them they travel inside the report's
//! `frozen_thresholds` section and this gate enforces exactly those.

use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;

use crate::report::{EvaluationReport, FrozenThreshold, ThresholdOutcome};

/// Milliseconds in a day, used for the freshness window.
const MILLIS_PER_DAY: i64 = 86_400_000;

/// Default freshness window for a `Recommended` claim, in days.
///
/// This is a record-freshness policy, not a quality threshold: it bounds how
/// stale the evidence behind a published recommendation may be.
pub const DEFAULT_MAX_RECORD_AGE_DAYS: u32 = 180;

/// A claim the product may make about an engine and model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Claim {
    /// The capability runs and passes its non-quality gates (spec 1.6).
    Supported,
    /// The project recommends this model for quality reasons (spec 17.6).
    Recommended,
}

impl Claim {
    /// Whether the claim asserts anything about output quality.
    #[must_use]
    pub const fn implies_quality(self) -> bool {
        matches!(self, Self::Recommended)
    }

    /// Whether the claim requires a stored evaluation record.
    #[must_use]
    pub const fn requires_evaluation_record(self) -> bool {
        self.implies_quality()
    }

    /// The exact token used on the command line and in decisions.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Recommended => "recommended",
        }
    }

    /// Parse a command-line claim token.
    pub fn parse(raw: &str) -> Result<Self, UnknownClaim> {
        match raw {
            "supported" => Ok(Self::Supported),
            "recommended" => Ok(Self::Recommended),
            other => Err(UnknownClaim(other.to_owned())),
        }
    }
}

/// A claim token outside the spec 1.6 vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown claim `{0}`, expected `supported` or `recommended`")]
pub struct UnknownClaim(pub String);

/// The golden corpus a recommendation must have been measured on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldenCorpus {
    /// Corpus identifier.
    pub corpus_id: String,
    /// Content hash of the corpus manifest at the time of the check.
    pub manifest_sha256: Sha256Digest,
}

/// What is being claimed, for which engine and model, against which corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRequest {
    /// The claim under evaluation.
    pub claim: Claim,
    /// Engine the claim is about.
    pub engine_id: String,
    /// Model the claim is about.
    pub model_id: String,
    /// The golden corpus a recommendation must cite.
    pub golden_corpus: GoldenCorpus,
    /// The instant the gate is being evaluated at.
    pub as_of: Timestamp,
    /// Freshness window for the stored record, in days.
    pub max_record_age_days: u32,
}

impl GateRequest {
    /// A request with the default freshness window.
    #[must_use]
    pub fn new(
        claim: Claim,
        engine_id: impl Into<String>,
        model_id: impl Into<String>,
        golden_corpus: GoldenCorpus,
        as_of: Timestamp,
    ) -> Self {
        Self {
            claim,
            engine_id: engine_id.into(),
            model_id: model_id.into(),
            golden_corpus,
            as_of,
            max_record_age_days: DEFAULT_MAX_RECORD_AGE_DAYS,
        }
    }
}

/// The measurement a granted recommendation rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityEvidence {
    /// Corpus the record was measured on.
    pub corpus_id: String,
    /// Content hash of that corpus manifest.
    pub manifest_sha256: Sha256Digest,
    /// Number of measured items.
    pub sample_size: usize,
    /// When the measurement was taken.
    pub measured_on: Timestamp,
    /// Content hash of the stored report.
    pub report_sha256: Sha256Digest,
}

/// A granted claim.
#[derive(Debug, Clone, PartialEq)]
pub struct GateDecision {
    /// The claim that was granted.
    pub claim: Claim,
    /// Engine the decision covers.
    pub engine_id: String,
    /// Model the decision covers.
    pub model_id: String,
    /// The measurement behind it. Always `None` for [`Claim::Supported`],
    /// which is a capability statement and carries no quality evidence.
    pub quality_evidence: Option<QualityEvidence>,
    /// Outcome of each frozen threshold, empty while spec 17.6 defers them.
    pub threshold_outcomes: Vec<ThresholdOutcome>,
}

impl GateDecision {
    /// Sentence a release note may quote verbatim.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.quality_evidence {
            None => format!(
                "{} / {}: `{}` is a capability claim and asserts nothing about quality (spec 1.6).",
                self.engine_id,
                self.model_id,
                self.claim.as_str()
            ),
            Some(evidence) => format!(
                "{} / {}: `{}` granted on corpus {} ({}), {} items, measured {}, report {}.",
                self.engine_id,
                self.model_id,
                self.claim.as_str(),
                evidence.corpus_id,
                evidence.manifest_sha256.to_hex(),
                evidence.sample_size,
                evidence
                    .measured_on
                    .to_rfc3339()
                    .unwrap_or_else(|| format!("{}ms", evidence.measured_on.as_millis())),
                evidence.report_sha256.to_hex()
            ),
        }
    }
}

/// Why a claim was refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GateError {
    /// No stored evaluation record was supplied at all.
    #[error("`recommended` requires a stored evaluation record for engine `{engine_id}` and model `{model_id}` on the golden corpus (spec 17.6); none was supplied")]
    MissingEvaluationRecord {
        /// Engine the claim was about.
        engine_id: String,
        /// Model the claim was about.
        model_id: String,
    },
    /// The record describes a different engine.
    #[error("stored record is for engine `{found}`, not `{expected}`")]
    EngineMismatch {
        /// Engine the claim was about.
        expected: String,
        /// Engine the record describes.
        found: String,
    },
    /// The record describes a different model.
    #[error("stored record is for model `{found}`, not `{expected}`")]
    ModelMismatch {
        /// Model the claim was about.
        expected: String,
        /// Model the record describes.
        found: String,
    },
    /// The record was measured on a different corpus.
    #[error("stored record was measured on corpus `{found}`, not the golden corpus `{expected}`")]
    CorpusMismatch {
        /// Golden corpus identifier.
        expected: String,
        /// Corpus the record cites.
        found: String,
    },
    /// The record was measured on an older revision of the golden corpus.
    #[error("stored record cites corpus manifest {found}, the golden corpus is now {expected}")]
    CorpusRevisionMismatch {
        /// Current manifest hash.
        expected: String,
        /// Manifest hash the record cites.
        found: String,
    },
    /// The record is older than the freshness window.
    #[error("stored record is {age_days} days old, the freshness window is {max_age_days} days")]
    StaleRecord {
        /// Age of the record in days.
        age_days: i64,
        /// Configured window.
        max_age_days: u32,
    },
    /// The record is dated after the instant the gate is evaluated at.
    #[error("stored record is dated in the future relative to the gate evaluation")]
    RecordFromTheFuture,
    /// The record measured nothing.
    #[error("stored record measured {evaluated} items; a recommendation needs at least one")]
    EmptyEvaluation {
        /// Items the record measured.
        evaluated: usize,
    },
    /// The record excluded corpus rows that the golden corpus still declares.
    #[error("stored record excluded {excluded} corpus item(s) for consent reasons; the golden corpus must be fully cleared before a recommendation rests on it")]
    UnclearedCorpus {
        /// Rows the run refused.
        excluded: usize,
    },
    /// The record cannot say which machine or build produced it.
    #[error("stored record is missing required hardware evidence: {}", missing.join(", "))]
    IncompleteEvidence {
        /// Fields spec 17.2 requires a published measurement to carry.
        missing: Vec<String>,
    },
    /// A frozen threshold is not satisfied by the measurement.
    #[error("frozen threshold {threshold:?} is not met (measured {measured:?})")]
    ThresholdNotMet {
        /// The threshold that failed.
        threshold: FrozenThreshold,
        /// The measurement, absent when the report does not carry it.
        measured: Option<f64>,
    },
}

/// Decide whether a claim may be made.
///
/// `record` is the stored evaluation report for the engine and model. A
/// [`Claim::Supported`] request ignores it entirely; a [`Claim::Recommended`]
/// request fails without one.
pub fn evaluate_claim(
    request: &GateRequest,
    record: Option<&EvaluationReport>,
) -> Result<GateDecision, GateError> {
    if !request.claim.requires_evaluation_record() {
        return Ok(GateDecision {
            claim: request.claim,
            engine_id: request.engine_id.clone(),
            model_id: request.model_id.clone(),
            quality_evidence: None,
            threshold_outcomes: Vec::new(),
        });
    }

    let report = record.ok_or_else(|| GateError::MissingEvaluationRecord {
        engine_id: request.engine_id.clone(),
        model_id: request.model_id.clone(),
    })?;

    if report.engine.engine_id != request.engine_id {
        return Err(GateError::EngineMismatch {
            expected: request.engine_id.clone(),
            found: report.engine.engine_id.clone(),
        });
    }
    if report.engine.model_id != request.model_id {
        return Err(GateError::ModelMismatch {
            expected: request.model_id.clone(),
            found: report.engine.model_id.clone(),
        });
    }
    if report.corpus.corpus_id != request.golden_corpus.corpus_id {
        return Err(GateError::CorpusMismatch {
            expected: request.golden_corpus.corpus_id.clone(),
            found: report.corpus.corpus_id.clone(),
        });
    }
    if report.corpus.manifest_sha256 != request.golden_corpus.manifest_sha256 {
        return Err(GateError::CorpusRevisionMismatch {
            expected: request.golden_corpus.manifest_sha256.to_hex(),
            found: report.corpus.manifest_sha256.to_hex(),
        });
    }
    if report.corpus.items_evaluated == 0 {
        return Err(GateError::EmptyEvaluation {
            evaluated: report.corpus.items_evaluated,
        });
    }
    if !report.corpus.excluded.is_empty() {
        return Err(GateError::UnclearedCorpus {
            excluded: report.corpus.excluded.len(),
        });
    }

    let age_millis = request.as_of.as_millis() - report.environment.measured_on.as_millis();
    if age_millis < 0 {
        return Err(GateError::RecordFromTheFuture);
    }
    let age_days = age_millis / MILLIS_PER_DAY;
    if age_days > i64::from(request.max_record_age_days) {
        return Err(GateError::StaleRecord {
            age_days,
            max_age_days: request.max_record_age_days,
        });
    }

    let missing = report.environment.missing_evidence();
    if !missing.is_empty() {
        return Err(GateError::IncompleteEvidence {
            missing: missing.into_iter().map(ToOwned::to_owned).collect(),
        });
    }

    let threshold_outcomes = report.evaluate_thresholds();
    for outcome in &threshold_outcomes {
        if outcome.satisfied != Some(true) {
            return Err(GateError::ThresholdNotMet {
                threshold: outcome.threshold,
                measured: outcome.measured,
            });
        }
    }

    Ok(GateDecision {
        claim: request.claim,
        engine_id: request.engine_id.clone(),
        model_id: request.model_id.clone(),
        quality_evidence: Some(QualityEvidence {
            corpus_id: report.corpus.corpus_id.clone(),
            manifest_sha256: report.corpus.manifest_sha256,
            sample_size: report.corpus.items_evaluated,
            measured_on: report.environment.measured_on,
            report_sha256: report.digest(),
        }),
        threshold_outcomes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{ClearanceRejection, ItemRejection};
    use crate::report::{Comparator, FrozenThresholds, ThresholdMetric};

    fn report() -> EvaluationReport {
        crate::report::tests::sample_report()
    }

    fn request(claim: Claim) -> GateRequest {
        GateRequest::new(
            claim,
            "whisper.cpp",
            "ggml-small",
            GoldenCorpus {
                corpus_id: "unit".to_owned(),
                manifest_sha256: Sha256Digest::of(b"unit"),
            },
            Timestamp::from_millis(1_780_000_000_000 + MILLIS_PER_DAY),
        )
    }

    #[test]
    fn supported_is_granted_without_any_evaluation_record() {
        let decision = evaluate_claim(&request(Claim::Supported), None).expect("capability claim");
        assert_eq!(decision.claim, Claim::Supported);
        assert!(decision.quality_evidence.is_none());
        assert!(!Claim::Supported.implies_quality());
        assert!(!Claim::Supported.requires_evaluation_record());
        assert!(decision.summary().contains("asserts nothing about quality"));
    }

    #[test]
    fn supported_never_quotes_a_quality_measurement_even_when_one_exists() {
        let stored = report();
        let decision =
            evaluate_claim(&request(Claim::Supported), Some(&stored)).expect("capability claim");
        assert!(decision.quality_evidence.is_none());
        assert_eq!(decision.threshold_outcomes, Vec::new());
    }

    #[test]
    fn recommended_fails_without_a_stored_record() {
        let error = evaluate_claim(&request(Claim::Recommended), None).expect_err("no record");
        assert!(matches!(
            error,
            GateError::MissingEvaluationRecord { ref engine_id, .. } if engine_id == "whisper.cpp"
        ));
        assert!(Claim::Recommended.implies_quality());
    }

    #[test]
    fn recommended_passes_with_a_matching_record() {
        let stored = report();
        let decision =
            evaluate_claim(&request(Claim::Recommended), Some(&stored)).expect("granted");
        let evidence = decision.quality_evidence.expect("evidence");
        assert_eq!(evidence.corpus_id, "unit");
        assert_eq!(evidence.sample_size, 1);
        assert_eq!(evidence.report_sha256, stored.digest());
    }

    #[test]
    fn recommended_fails_for_a_different_model_of_the_same_engine() {
        let stored = report();
        let mut request = request(Claim::Recommended);
        request.model_id = "ggml-large-v3".to_owned();
        let error = evaluate_claim(&request, Some(&stored)).expect_err("model mismatch");
        assert!(matches!(
            error,
            GateError::ModelMismatch { ref found, .. } if found == "ggml-small"
        ));
    }

    #[test]
    fn recommended_fails_for_a_different_engine() {
        let stored = report();
        let mut request = request(Claim::Recommended);
        request.engine_id = "apple.speech".to_owned();
        assert!(matches!(
            evaluate_claim(&request, Some(&stored)),
            Err(GateError::EngineMismatch { .. })
        ));
    }

    #[test]
    fn recommended_fails_when_the_golden_corpus_changed() {
        let stored = report();
        let mut request = request(Claim::Recommended);
        request.golden_corpus.manifest_sha256 = Sha256Digest::of(b"revised");
        assert!(matches!(
            evaluate_claim(&request, Some(&stored)),
            Err(GateError::CorpusRevisionMismatch { .. })
        ));

        let mut other_corpus = request.clone();
        other_corpus.golden_corpus.corpus_id = "golden-v2".to_owned();
        other_corpus.golden_corpus.manifest_sha256 = stored.corpus.manifest_sha256;
        assert!(matches!(
            evaluate_claim(&other_corpus, Some(&stored)),
            Err(GateError::CorpusMismatch { .. })
        ));
    }

    #[test]
    fn recommended_fails_on_a_stale_record() {
        let stored = report();
        let mut request = request(Claim::Recommended);
        request.as_of = Timestamp::from_millis(
            stored.environment.measured_on.as_millis() + MILLIS_PER_DAY * 181,
        );
        let error = evaluate_claim(&request, Some(&stored)).expect_err("stale");
        assert!(matches!(
            error,
            GateError::StaleRecord {
                age_days: 181,
                max_age_days: 180
            }
        ));

        request.max_record_age_days = 365;
        assert!(evaluate_claim(&request, Some(&stored)).is_ok());
    }

    #[test]
    fn a_record_dated_after_the_check_is_refused() {
        let stored = report();
        let mut request = request(Claim::Recommended);
        request.as_of = Timestamp::from_millis(stored.environment.measured_on.as_millis() - 1);
        assert!(matches!(
            evaluate_claim(&request, Some(&stored)),
            Err(GateError::RecordFromTheFuture)
        ));
    }

    #[test]
    fn recommended_fails_on_an_empty_sample() {
        let mut stored = report();
        stored.corpus.items_evaluated = 0;
        assert!(matches!(
            evaluate_claim(&request(Claim::Recommended), Some(&stored)),
            Err(GateError::EmptyEvaluation { evaluated: 0 })
        ));
    }

    #[test]
    fn recommended_fails_when_the_corpus_still_contains_uncleared_rows() {
        let mut stored = report();
        stored.corpus.excluded = vec![ItemRejection {
            item_id: "unl-004".to_owned(),
            reasons: vec![ClearanceRejection::MissingLicense],
        }];
        assert!(matches!(
            evaluate_claim(&request(Claim::Recommended), Some(&stored)),
            Err(GateError::UnclearedCorpus { excluded: 1 })
        ));
    }

    #[test]
    fn recommended_fails_without_hardware_evidence() {
        let mut stored = report();
        stored.environment.cpu = None;
        let error =
            evaluate_claim(&request(Claim::Recommended), Some(&stored)).expect_err("evidence");
        assert!(matches!(
            error,
            GateError::IncompleteEvidence { ref missing } if missing == &vec!["cpu".to_owned()]
        ));
    }

    #[test]
    fn a_frozen_threshold_the_measurement_misses_refuses_the_claim() {
        let mut stored = report();
        stored.frozen_thresholds = FrozenThresholds::frozen(
            "mechanism check, not a project threshold",
            vec![FrozenThreshold {
                metric: ThresholdMetric::WordErrorRate,
                comparator: Comparator::AtMost,
                value: 0.01,
                frozen_on: None,
            }],
        );
        assert!(matches!(
            evaluate_claim(&request(Claim::Recommended), Some(&stored)),
            Err(GateError::ThresholdNotMet { .. })
        ));
    }

    #[test]
    fn a_deferred_threshold_section_grants_the_claim_on_evidence_alone() {
        let decision = evaluate_claim(&request(Claim::Recommended), Some(&report()))
            .expect("deferred thresholds assert nothing");
        assert_eq!(decision.threshold_outcomes, Vec::new());
    }

    #[test]
    fn claim_tokens_round_trip_and_reject_anything_else() {
        assert_eq!(Claim::parse("supported"), Ok(Claim::Supported));
        assert_eq!(Claim::parse("recommended"), Ok(Claim::Recommended));
        assert_eq!(Claim::Recommended.as_str(), "recommended");
        assert!(Claim::parse("Recommended").is_err());
        assert_eq!(
            Claim::parse("best").expect_err("unknown").to_string(),
            "unknown claim `best`, expected `supported` or `recommended`"
        );
    }
}
