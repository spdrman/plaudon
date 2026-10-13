//! The speaker-matching harness itself (VP-180).
//!
//! One pass of the harness is:
//!
//! ```text
//!   fixture library ─► segments ─► EmbeddingSource ─► matching core (#242)
//!                                                       │
//!            trials (target / non-target) ◄──────────────┘
//!                     │
//!                     ├─► DET curve per condition and pooled ─► EER
//!                     ├─► calibration: T_suggest at FA ≤ 1%, T_floor
//!                     ├─► second pass with the derived thresholds ─► bands
//!                     └─► golden suggestions for the pinned fixture
//!   synthetic session ─► scripted hypothesis ─► diarization_error_rate ─► DER
//! ```
//!
//! Three things are worth saying out loud about the design.
//!
//! **The scores come from the product's own matcher.** The harness builds
//! [`VoiceProfile`]s and calls [`Matcher::match_cluster`], so cosine, AS-Norm,
//! multi-exemplar aggregation and the band rule are measured as shipped rather
//! than re-implemented here (re-implementing them is how a harness ends up
//! certifying code that does not exist).
//!
//! **Scoring and thresholding are two passes.** The first pass runs with an
//! open operating point so every (cluster, profile) score is observed — a DET
//! curve cannot be drawn from scores a floor already discarded. The thresholds
//! derived from that curve are then installed and the trials are re-banded, so
//! the published band counts are produced by [`ThresholdSet::band`] itself.
//!
//! **Enrolment and test material never overlap.** Exemplars use the first
//! `enrol_utterances` utterances of a speaker, trials the ones after them.
//! Scoring a profile against its own enrolment audio measures nothing.

use std::collections::BTreeMap;

use plaudon_ai_diarization::contract::SpeakerTurn;
use plaudon_ai_diarization::der::{diarization_error_rate, DerOptions};
use plaudon_app_core::voice_profiles::matching::{
    ClusterEmbedding, CohortMatrix, Embedding, Exemplar, MatchError, Matcher, NormalisedScore,
    ProfileError, ProfileOffset, ThresholdSet, TopK, VoiceProfile,
};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};

use super::calibration::{calibrate, CalibrationProvenance};
use super::det::{DetCurve, DetError, TrialScore};
use super::embeddings::{EmbeddingSource, EmbeddingSourceError, SegmentKey};
use super::fixtures::{Condition, FixtureError, FixtureManifest, SessionPlan, SyntheticSession};
use super::report::{
    condition_token, BandCounts, ConditionReport, DiarizationSection, FixtureSummary,
    GoldenSuggestion, ModelRecord, ReportParts, RuntimeRecord, SpeakerMatchReport,
    OVERALL_CONDITION,
};

/// How the scripted diarization hypothesis is produced, for the report.
pub const SCRIPTED_HYPOTHESIS_SOURCE: &str = "scripted-perturbation/1";

/// Why a harness pass could not finish.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The fixture library was refused.
    #[error(transparent)]
    Fixtures(#[from] FixtureError),

    /// An embedding was unavailable.
    #[error(transparent)]
    Embeddings(#[from] EmbeddingSourceError),

    /// A profile could not be built from the fixtures.
    #[error("enrolling `{person}`: {source}")]
    Profile {
        /// Person whose profile failed.
        person: String,
        /// Why.
        #[source]
        source: ProfileError,
    },

    /// The matching core refused to score.
    #[error(transparent)]
    Matching(#[from] MatchError),

    /// The trials did not form a curve.
    #[error(transparent)]
    Det(#[from] DetError),

    /// The plan is internally inconsistent.
    #[error("harness plan is unusable: {0}")]
    Plan(String),
}

/// A complete, reproducible description of one harness pass.
///
/// Serializable because it is committed: `tests/fixtures/speaker-eval/` holds
/// the plan the CI tier runs, so "what produced this artifact" is a file, not
/// a command line somebody remembers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessPlan {
    /// The licence-checked fixture library.
    pub library: FixtureManifest,
    /// Session layout used for the diarization half.
    pub session: SessionPlan,
    /// Conditions to evaluate (VP-184).
    pub conditions: Vec<Condition>,
    /// Speakers that are enrolled and tested.
    pub roster: Vec<String>,
    /// Speakers forming the AS-Norm cohort. Disjoint from the roster.
    pub cohort_speakers: Vec<String>,
    /// Utterances per speaker used for enrolment.
    pub enrol_utterances: u32,
    /// Utterances per speaker used as trials.
    pub test_utterances: u32,
    /// Clean single-speaker speech behind one utterance, in milliseconds.
    pub utterance_ms: u32,
    /// Cohort rows averaged by AS-Norm.
    pub cohort_top_k: usize,
    /// Condition whose suggestions are pinned as golden (VP-183).
    pub golden_condition: Condition,
    /// How many clusters of that condition are pinned.
    pub golden_clusters: usize,
}

impl HarnessPlan {
    /// Parse a committed plan, licence-checking the library it carries.
    pub fn parse(document: &str) -> Result<Self, HarnessError> {
        let plan: Self = serde_json::from_str(document)
            .map_err(|source| HarnessError::Fixtures(FixtureError::Malformed(source)))?;
        let rejections = plan.library.rejections();
        if !rejections.is_empty() {
            return Err(HarnessError::Fixtures(FixtureError::Refused(rejections)));
        }
        plan.check()?;
        Ok(plan)
    }

    /// Reject a plan that cannot produce a meaningful measurement.
    fn check(&self) -> Result<(), HarnessError> {
        if self.roster.len() < 2 {
            return Err(HarnessError::Plan(
                "at least two roster speakers are needed for non-target trials".to_owned(),
            ));
        }
        if self.conditions.is_empty() {
            return Err(HarnessError::Plan("no conditions to evaluate".to_owned()));
        }
        if self.enrol_utterances == 0 || self.test_utterances == 0 {
            return Err(HarnessError::Plan(
                "enrolment and trial material must both be non-empty".to_owned(),
            ));
        }
        if !self.conditions.contains(&self.golden_condition) {
            return Err(HarnessError::Plan(
                "the golden condition is not among the evaluated conditions".to_owned(),
            ));
        }
        for speaker in &self.roster {
            if self.cohort_speakers.contains(speaker) {
                return Err(HarnessError::Plan(format!(
                    "`{speaker}` is both enrolled and in the AS-Norm cohort"
                )));
            }
        }
        Ok(())
    }

    /// Enrolment segments of one speaker (clean condition: VP-067 measures the
    /// mismatch between a clean profile and a degraded test).
    fn enrolment_segments(&self, speaker: &str) -> Vec<SegmentKey> {
        (0..self.enrol_utterances)
            .map(|utterance| SegmentKey::new(speaker, utterance, Condition::Clean))
            .collect()
    }

    /// Trial segments of one speaker under one condition.
    fn trial_segments(&self, speaker: &str, condition: Condition) -> Vec<SegmentKey> {
        (self.enrol_utterances..self.enrol_utterances + self.test_utterances)
            .map(|utterance| SegmentKey::new(speaker, utterance, condition))
            .collect()
    }

    /// How many trials [`Self::trial_list`] enumerates.
    ///
    /// The harness checks its scored trials against this count: the first pass
    /// runs with an open operating point precisely so nothing is discarded,
    /// and a band rule that quietly dropped a score would otherwise shrink the
    /// denominator of every rate in the report.
    #[must_use]
    pub fn expected_trials(&self) -> usize {
        self.conditions.len()
            * self.roster.len()
            * self.test_utterances as usize
            * self.roster.len()
    }

    /// The trial list this plan defines (VP-180).
    ///
    /// Every (profile, test segment) pair the run will score, enumerated
    /// before anything is scored, so the list is reviewable on its own and a
    /// run that produced a different number of scores is detectable. The pairs
    /// are the full cross product: an open-set harness has to see each test
    /// segment against every enrolled profile, which is what makes the
    /// non-target side large enough to resolve a 1% false-accept rate.
    #[must_use]
    pub fn trial_list(&self) -> Vec<TrialSpec> {
        let mut trials = Vec::new();
        for condition in &self.conditions {
            for speaker in &self.roster {
                for segment in self.trial_segments(speaker, *condition) {
                    for person in &self.roster {
                        trials.push(TrialSpec {
                            trial_id: format!("{person}/{}", segment.token()),
                            person_id: person.clone(),
                            segment: segment.clone(),
                            is_target: person == speaker,
                        });
                    }
                }
            }
        }
        trials
    }
}

/// One (profile, test segment) pair the harness will score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialSpec {
    /// Identifier the scored trial carries: `<person>/<segment token>`.
    pub trial_id: String,
    /// Enrolled person the test segment is scored against.
    pub person_id: String,
    /// The test segment.
    pub segment: SegmentKey,
    /// Whether the pair is the same speaker.
    pub is_target: bool,
}

/// Everything a caller may record about the run that the harness cannot know.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunMetadata {
    /// When the run happened. `None` for a deterministic scripted run.
    pub measured_on: Option<Timestamp>,
    /// Commit the run was made at.
    pub commit: Option<String>,
    /// Measured runtime footprint (ADR-0081 §4).
    pub runtime: Option<RuntimeRecord>,
}

/// A threshold set that records everything: the first pass.
///
/// AS-Norm scores are z-scores, so they are unbounded in principle and sit
/// within a few units in practice. A floor far below any achievable score
/// makes the first pass record every (cluster, profile) outcome, which is what
/// a DET curve needs.
fn open_thresholds() -> ThresholdSet {
    let floor = NormalisedScore::new(-1.0e6).expect("a finite floor");
    let suggest = NormalisedScore::new(-1.0e6 + 1.0).expect("a finite suggestion threshold");
    ThresholdSet::new(floor, suggest, 0.0).expect("an ordered open operating point")
}

/// Score a plan with one embedding source and build the artifact.
pub fn evaluate(
    plan: &HarnessPlan,
    source: &dyn EmbeddingSource,
    metadata: &RunMetadata,
) -> Result<SpeakerMatchReport, HarnessError> {
    plan.check()?;

    let cohort = build_cohort(plan, source)?;
    let top_k = TopK::new(plan.cohort_top_k).map_err(MatchError::Cohort)?;
    let profiles = enrol(plan, source)?;

    let open = Matcher::new(
        source.binding().clone(),
        open_thresholds(),
        cohort.clone(),
        top_k,
    )?;
    let prepared = open.prepare_all(&profiles)?;

    // Pass one: every score, so the curve has every point the sample supports.
    let mut trials: Vec<TrialScore> = Vec::new();
    for condition in &plan.conditions {
        for speaker in &plan.roster {
            for segment in plan.trial_segments(speaker, *condition) {
                let cluster = cluster_embedding(plan, source, &segment)?;
                let report = open.match_cluster(&cluster, &prepared)?;
                for scored in &report.recorded {
                    trials.push(TrialScore {
                        trial_id: format!("{}/{}", scored.person_id, segment.token()),
                        condition: *condition,
                        is_target: scored.person_id == *speaker,
                        score: f64::from(scored.score.value()),
                    });
                }
            }
        }
    }

    if trials.len() != plan.expected_trials() {
        return Err(HarnessError::Plan(format!(
            "scored {} trial(s), the plan enumerates {}",
            trials.len(),
            plan.expected_trials()
        )));
    }

    let pooled = DetCurve::from_trials(&trials)?;
    let provenance = CalibrationProvenance {
        embedding_source: source.kind(),
        model_id: source.binding().model_id.clone(),
        model_version: source.binding().model_version.clone(),
        model_sha256: source.binding().model_sha256.to_hex(),
        library_id: plan.library.library_id.clone(),
        manifest_sha256: plan.library.digest().to_hex(),
        targets: pooled.targets(),
        nontargets: pooled.nontargets(),
        false_accept_resolution: pooled.false_accept_resolution(),
    };
    let calibration = calibrate(&pooled, provenance);

    // Pass two: band the same trials with the derived operating point, using
    // the product's own band rule.
    let derived = calibration.threshold_set();
    let overall_bands = derived.map(|set| band_counts(&trials, set, None));
    let mut conditions = Vec::with_capacity(plan.conditions.len());
    for condition in &plan.conditions {
        let subset: Vec<TrialScore> = trials
            .iter()
            .filter(|trial| trial.condition == *condition)
            .cloned()
            .collect();
        let curve = DetCurve::from_trials(&subset)?;
        conditions.push(ConditionReport {
            condition: condition_token(*condition),
            targets: curve.targets(),
            nontargets: curve.nontargets(),
            false_accept_resolution: curve.false_accept_resolution(),
            equal_error_rate: curve.equal_error_rate(),
            operating_points: curve.summary_points(),
            bands: derived.map(|set| band_counts(&trials, set, Some(*condition))),
        });
    }

    let overall = ConditionReport {
        condition: OVERALL_CONDITION.to_owned(),
        targets: pooled.targets(),
        nontargets: pooled.nontargets(),
        false_accept_resolution: pooled.false_accept_resolution(),
        equal_error_rate: pooled.equal_error_rate(),
        operating_points: pooled.summary_points(),
        bands: overall_bands,
    };

    let golden = match derived {
        Some(set) => golden_suggestions(plan, source, &cohort, top_k, &profiles, set)?,
        None => Vec::new(),
    };

    let session = plan.session.synthesize();
    let diarization = Some(diarization_section(&session));

    let mut extra_caveats = Vec::new();
    if !source.kind().is_model_measurement() {
        extra_caveats.push(
            "Embeddings in this run were generated from a seed, not extracted from audio by a \
             model. The error rates below describe that generator and the harness around it; they \
             are not a measurement of any checkpoint (VP-183)."
                .to_owned(),
        );
    }
    extra_caveats.push(format!(
        "The diarization section scores a {SCRIPTED_HYPOTHESIS_SOURCE} hypothesis against exact \
         synthetic ground truth; it measures the DER implementation, not a diarization provider."
    ));

    Ok(SpeakerMatchReport::new(ReportParts {
        measured_on: metadata.measured_on,
        commit: metadata.commit.clone(),
        model: ModelRecord {
            model_id: source.binding().model_id.clone(),
            model_version: source.binding().model_version.clone(),
            model_sha256: source.binding().model_sha256.to_hex(),
            dim: source.dim(),
            embedding_source: source.kind(),
        },
        runtime: metadata.runtime.clone(),
        fixtures: FixtureSummary {
            library_id: plan.library.library_id.clone(),
            manifest_sha256: plan.library.digest().to_hex(),
            fixtures: plan.library.fixtures.len(),
            speakers: plan.library.speakers().len(),
            attributions: plan.library.attributions(),
        },
        overall,
        conditions,
        calibration,
        diarization,
        golden_suggestions: golden,
        extra_caveats,
    }))
}

/// Build the AS-Norm cohort from the cohort speakers (VP-052).
fn build_cohort(
    plan: &HarnessPlan,
    source: &dyn EmbeddingSource,
) -> Result<CohortMatrix, HarnessError> {
    let mut vectors = Vec::with_capacity(plan.cohort_speakers.len());
    for speaker in &plan.cohort_speakers {
        let key = SegmentKey::new(speaker, 0, Condition::Clean);
        vectors.push(source.vector(&key)?);
    }
    CohortMatrix::from_vectors(source.binding().clone(), &vectors)
        .map_err(|error| HarnessError::Matching(MatchError::Cohort(error)))
}

/// Enrol every roster speaker from their enrolment segments.
fn enrol(
    plan: &HarnessPlan,
    source: &dyn EmbeddingSource,
) -> Result<Vec<VoiceProfile>, HarnessError> {
    let mut profiles = Vec::with_capacity(plan.roster.len());
    for speaker in &plan.roster {
        let mut exemplars = Vec::new();
        for key in plan.enrolment_segments(speaker) {
            let embedding: Embedding = source.embedding(&key)?;
            let exemplar = Exemplar::new(embedding, plan.utterance_ms).map_err(|source| {
                HarnessError::Profile {
                    person: speaker.clone(),
                    source: ProfileError::NotEnoughSpeech(source),
                }
            })?;
            exemplars.push(exemplar);
        }
        let neutral = ProfileOffset::clamped(0.0, open_thresholds());
        let profile = VoiceProfile::new(speaker.clone(), exemplars, neutral).map_err(|source| {
            HarnessError::Profile {
                person: speaker.clone(),
                source,
            }
        })?;
        profiles.push(profile);
    }
    Ok(profiles)
}

/// One trial cluster: a test segment presented as a diarization cluster.
fn cluster_embedding(
    plan: &HarnessPlan,
    source: &dyn EmbeddingSource,
    key: &SegmentKey,
) -> Result<ClusterEmbedding, HarnessError> {
    let embedding = source.embedding(key)?;
    ClusterEmbedding::new(key.token(), embedding, plan.utterance_ms)
        .map_err(|source| HarnessError::Matching(MatchError::NotEnoughSpeech(source)))
}

/// Partition trials into bands with the product's own band rule.
fn band_counts(
    trials: &[TrialScore],
    thresholds: ThresholdSet,
    condition: Option<Condition>,
) -> BandCounts {
    use plaudon_app_core::voice_profiles::matching::Band;

    let mut counts = BandCounts {
        target_suggest: 0,
        target_weak: 0,
        target_none: 0,
        nontarget_suggest: 0,
        nontarget_weak: 0,
        nontarget_none: 0,
    };
    for trial in trials {
        if condition.is_some_and(|wanted| trial.condition != wanted) {
            continue;
        }
        let Ok(score) = NormalisedScore::new(trial.score as f32) else {
            continue;
        };
        let band = thresholds.band(score, ProfileOffset::clamped(0.0, thresholds));
        match (trial.is_target, band) {
            (true, Band::Suggest) => counts.target_suggest += 1,
            (true, Band::Weak) => counts.target_weak += 1,
            (true, Band::None) => counts.target_none += 1,
            (false, Band::Suggest) => counts.nontarget_suggest += 1,
            (false, Band::Weak) => counts.nontarget_weak += 1,
            (false, Band::None) => counts.nontarget_none += 1,
        }
    }
    counts
}

/// Suggestions the product would show for the pinned fixture (VP-183).
fn golden_suggestions(
    plan: &HarnessPlan,
    source: &dyn EmbeddingSource,
    cohort: &CohortMatrix,
    top_k: TopK,
    profiles: &[VoiceProfile],
    thresholds: ThresholdSet,
) -> Result<Vec<GoldenSuggestion>, HarnessError> {
    let matcher = Matcher::new(source.binding().clone(), thresholds, cohort.clone(), top_k)?;
    let prepared = matcher.prepare_all(profiles)?;

    let mut out = Vec::new();
    for speaker in plan.roster.iter().take(plan.golden_clusters) {
        let Some(segment) = plan
            .trial_segments(speaker, plan.golden_condition)
            .into_iter()
            .next()
        else {
            continue;
        };
        let cluster = cluster_embedding(plan, source, &segment)?;
        let report = matcher.match_cluster(&cluster, &prepared)?;
        for scored in &report.recorded {
            out.push(GoldenSuggestion {
                cluster_key: report.cluster_key.clone(),
                person_id: scored.person_id.clone(),
                band: scored.band,
                score: f64::from(scored.score.value()),
            });
        }
    }
    Ok(out)
}

/// Score the synthetic session's scripted hypothesis.
fn diarization_section(session: &SyntheticSession) -> DiarizationSection {
    let reference = session.reference_turns();
    let hypothesis = scripted_hypothesis(session);
    let options = DerOptions {
        collar_ms: 0,
        skip_overlap_regions: false,
    };
    let report = diarization_error_rate(&reference, &hypothesis, options);
    DiarizationSection {
        session_id: session.session_id.clone(),
        hypothesis_source: SCRIPTED_HYPOTHESIS_SOURCE.to_owned(),
        der: report.der,
        missed_ms: report.missed_ms,
        false_alarm_ms: report.false_alarm_ms,
        confusion_ms: report.confusion_ms,
        reference_speech_ms: session.speech_ms(),
        overlap_ms: session.overlap_ms(),
        collar_ms: options.collar_ms,
        skip_overlap_regions: options.skip_overlap_regions,
    }
}

/// A deterministic, imperfect diarization hypothesis for the session.
///
/// Three failure modes are scripted in, because a hypothesis that equals the
/// reference exercises nothing: boundaries are pulled in by 40 ms (missed
/// speech), every seventh turn is dropped entirely (more missed speech), and
/// every fifth turn is attributed to the previous speaker's cluster
/// (confusion). The cluster names are deliberately not the speaker names: the
/// scorer has to find the mapping, as it does for a real provider.
#[must_use]
pub fn scripted_hypothesis(session: &SyntheticSession) -> Vec<SpeakerTurn> {
    let mut clusters: BTreeMap<&str, String> = BTreeMap::new();
    for turn in &session.turns {
        let next = clusters.len();
        clusters
            .entry(turn.speaker.as_str())
            .or_insert_with(|| format!("cluster-{next}"));
    }

    let mut out = Vec::with_capacity(session.turns.len());
    let mut previous_cluster: Option<String> = None;
    for (index, turn) in session.turns.iter().enumerate() {
        if index % 7 == 6 {
            continue;
        }
        let own = clusters
            .get(turn.speaker.as_str())
            .expect("every speaker has a cluster")
            .clone();
        let cluster = if index % 5 == 4 {
            previous_cluster.clone().unwrap_or_else(|| own.clone())
        } else {
            own.clone()
        };
        previous_cluster = Some(own);
        let start = turn.start_ms + 40;
        let end = turn.end_ms.saturating_sub(40).max(start);
        out.push(SpeakerTurn::new(cluster, start, end));
    }
    out
}
