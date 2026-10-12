//! Open-set speaker matching: cosine + AS-Norm, bands, multi-exemplar scoring
//! (VOICE_PROFILES_SPEC §6, VP-050–VP-067; ADR-0082).
//!
//! This module is stage 4 of the §4 pipeline and nothing else. It takes
//! embeddings that someone else computed and produces *scores and bands*. It
//! has no model, no I/O, no clock and no global state, which is what makes the
//! whole of §6 testable without a single model file (VP-183).
//!
//! # What a match is
//!
//! ```text
//!   cluster embedding ─┐
//!                      ├─ cosine ─ AS-Norm ─┬─ vs centroid        ─┐
//!   profile exemplars ─┘                    └─ vs each exemplar ─┐ ├─ max → band
//!                                              top-2 mean ───────┘ ┘
//! ```
//!
//! and the band is one of three: suggest, weak, none (VP-060). There is no
//! fourth band, no auto-assignment and no path from this module to a transcript
//! label — ADR-0082 decision 1. [`MatchReport`] is a list of scores; acting on
//! one is a user action, taken elsewhere.
//!
//! # Open set first
//!
//! "Matches nobody enrolled" is the expected outcome, so it is the cheap one:
//! with no profiles [`Matcher::match_cluster`] returns
//! [`MatchReport::matches_nobody`] without touching the cohort, and scores
//! below the floor are counted but never recorded (VP-060, VP-061).
//!
//! # Checkpoints
//!
//! Thresholds are properties of a checkpoint, not of an architecture (VP-051),
//! so every embedding carries its [`ModelBinding`] and the matcher refuses —
//! loudly, with [`MatchError::ModelMismatch`] — to score across a mismatch
//! rather than silently comparing incomparable vectors (VP-026, VP-075).

pub mod bands;
pub mod calibration;
pub mod cohort;
pub mod embedding;
pub mod gates;

#[cfg(test)]
pub(crate) mod test_support;

use serde::{Deserialize, Serialize};

pub use bands::{Band, ProfileOffset, ThresholdError, ThresholdSet};
pub use calibration::{
    calibrated_offset, CalibrationOutcome, Decision, GUARD_FRACTION_OF_BAND,
    MIN_DECISIONS_FOR_OFFSET,
};
pub use cohort::{
    as_norm, CohortError, CohortMatrix, CohortStats, TopK, COHORT_FORMAT_VERSION,
    COHORT_HEADER_BYTES, COHORT_MAGIC, MAX_COHORT_ROWS, MIN_COHORT_ROWS,
};
pub use embedding::{
    CosineDistance, CosineSimilarity, Embedding, EmbeddingError, ModelBinding, NormalisedScore,
    ScoreError, MAX_EMBEDDING_DIM, MIN_EMBEDDING_DIM,
};
pub use gates::{
    InsufficientSpeech, SpeechGate, MIN_EMBED_SPEECH_MS, MIN_ENROL_SPEECH_MS,
    MIN_IDENTIFY_SPEECH_MS,
};

/// Value of the `speaker_suggestions.score_kind` column (§7.2).
///
/// Every score this module produces is an AS-Normalised cosine similarity, and
/// the column's `CHECK` constraint admits exactly this string: a persisted
/// score can never be ambiguous about its polarity or its normalisation.
pub const SCORE_KIND: &str = "cosine_similarity_asnorm";

/// How many exemplar similarities the multi-exemplar aggregate averages
/// (VP-064: "the top-2 mean of per-exemplar similarities").
pub const EXEMPLAR_AGGREGATE_SIZE: usize = 2;

/// [`aggregate`] tracks the best two scores in two variables rather than
/// sorting, so changing the size above means rewriting it.
const _: () = assert!(EXEMPLAR_AGGREGATE_SIZE == 2);

/// Why a profile could not be built.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProfileError {
    /// A profile must belong to a person.
    #[error("a voice profile needs a person identifier")]
    MissingPerson,

    /// A profile with no exemplars cannot be matched.
    #[error("a voice profile needs at least one exemplar")]
    NoExemplars,

    /// Exemplars from different checkpoints cannot share a profile (VP-075).
    #[error("exemplar {index} was produced by {found}, the profile is {expected}")]
    MixedCheckpoints {
        /// Index of the offending exemplar.
        index: usize,
        /// Checkpoint the profile is bound to.
        expected: String,
        /// Checkpoint the exemplar carries.
        found: String,
    },

    /// Exemplars disagreed about the dimension.
    #[error("exemplar {index} is {found}-dimensional, the profile is {expected}-dimensional")]
    MixedDimensions {
        /// Index of the offending exemplar.
        index: usize,
        /// Dimension of the profile.
        expected: usize,
        /// Dimension of the exemplar.
        found: usize,
    },

    /// The enrolment minimum was not met (VP-065).
    #[error(transparent)]
    NotEnoughSpeech(#[from] InsufficientSpeech),

    /// The exemplars cancel out, so there is no centroid to match against.
    #[error("the exemplars have no common direction, so the profile has no centroid")]
    DegenerateCentroid,
}

/// Why a match could not be produced.
///
/// Every variant is a refusal to score. None of them is a low-confidence score,
/// because VP-023 and VP-066 require an explicit outcome instead.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MatchError {
    /// Something was produced by a different checkpoint (VP-026, VP-075).
    #[error("refusing to score: the {what} was produced by {found}, this matcher is {expected}")]
    ModelMismatch {
        /// Which input mismatched.
        what: &'static str,
        /// Checkpoint the matcher is bound to.
        expected: String,
        /// Checkpoint the input carries.
        found: String,
    },

    /// The cluster did not meet the identification minimum (VP-065).
    #[error(transparent)]
    NotEnoughSpeech(#[from] InsufficientSpeech),

    /// The cohort could not be used.
    #[error(transparent)]
    Cohort(#[from] CohortError),
}

/// One stored embedding of an enrolled person, with the speech behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Exemplar {
    embedding: Embedding,
    clean_speech_ms: u32,
}

impl Exemplar {
    /// Wrap an embedding with the clean single-speaker speech it came from.
    ///
    /// Refuses anything below [`MIN_EMBED_SPEECH_MS`]: VP-065 allows an
    /// embedding to exist only over at least one second of clean speech, so an
    /// exemplar that claims less is a bug upstream, not a weak exemplar.
    pub fn new(embedding: Embedding, clean_speech_ms: u32) -> Result<Self, InsufficientSpeech> {
        SpeechGate::Embed.check(clean_speech_ms)?;
        Ok(Self {
            embedding,
            clean_speech_ms,
        })
    }

    /// The embedding.
    #[must_use]
    pub fn embedding(&self) -> &Embedding {
        &self.embedding
    }

    /// Clean single-speaker speech behind this exemplar.
    #[must_use]
    pub fn clean_speech_ms(&self) -> u32 {
        self.clean_speech_ms
    }
}

/// An enrolled person's voice profile: exemplars plus their centroid (VP-064).
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceProfile {
    person_id: String,
    exemplars: Vec<Exemplar>,
    centroid: Embedding,
    offset: ProfileOffset,
}

impl VoiceProfile {
    /// Assemble a profile and compute its centroid.
    ///
    /// The enrolment minimum (VP-065: ≥ 10 s accumulated clean speech) is a
    /// profile invariant, not a one-off check at enrolment time: a profile
    /// whose exemplars were pruned below the minimum stops matching rather
    /// than quietly matching on less evidence than the spec allows.
    pub fn new(
        person_id: impl Into<String>,
        exemplars: Vec<Exemplar>,
        offset: ProfileOffset,
    ) -> Result<Self, ProfileError> {
        let person_id = person_id.into();
        if person_id.trim().is_empty() {
            return Err(ProfileError::MissingPerson);
        }
        let Some(first) = exemplars.first() else {
            return Err(ProfileError::NoExemplars);
        };
        let binding = first.embedding.binding().clone();
        let dim = first.embedding.dim();

        let mut total_speech_ms: u32 = 0;
        for (index, exemplar) in exemplars.iter().enumerate() {
            if exemplar.embedding.binding() != &binding {
                return Err(ProfileError::MixedCheckpoints {
                    index,
                    expected: binding.to_string(),
                    found: exemplar.embedding.binding().to_string(),
                });
            }
            if exemplar.embedding.dim() != dim {
                return Err(ProfileError::MixedDimensions {
                    index,
                    expected: dim,
                    found: exemplar.embedding.dim(),
                });
            }
            total_speech_ms = total_speech_ms.saturating_add(exemplar.clean_speech_ms);
        }
        SpeechGate::Enrol.check(total_speech_ms)?;

        let mut sum = vec![0.0f32; dim];
        for exemplar in &exemplars {
            for (slot, component) in sum.iter_mut().zip(exemplar.embedding.unit()) {
                *slot += *component;
            }
        }
        let centroid =
            Embedding::l2_normalise(binding, &sum).map_err(|_| ProfileError::DegenerateCentroid)?;

        Ok(Self {
            person_id,
            exemplars,
            centroid,
            offset,
        })
    }

    /// The roster person this profile belongs to (`people.id`).
    #[must_use]
    pub fn person_id(&self) -> &str {
        &self.person_id
    }

    /// Checkpoint every exemplar of this profile was produced by.
    #[must_use]
    pub fn binding(&self) -> &ModelBinding {
        self.centroid.binding()
    }

    /// The exemplars.
    #[must_use]
    pub fn exemplars(&self) -> &[Exemplar] {
        &self.exemplars
    }

    /// The L2-normalised mean of the exemplars.
    #[must_use]
    pub fn centroid(&self) -> &Embedding {
        &self.centroid
    }

    /// This profile's calibration offset (VP-063).
    #[must_use]
    pub fn offset(&self) -> ProfileOffset {
        self.offset
    }

    /// Total clean speech behind the profile.
    #[must_use]
    pub fn clean_speech_ms(&self) -> u32 {
        self.exemplars.iter().fold(0u32, |total, exemplar| {
            total.saturating_add(exemplar.clean_speech_ms)
        })
    }

    /// Replace the calibration offset (VP-063).
    #[must_use]
    pub fn with_offset(mut self, offset: ProfileOffset) -> Self {
        self.offset = offset;
        self
    }
}

/// The embedding of one diarization cluster, waiting to be matched.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterEmbedding {
    cluster_key: String,
    embedding: Embedding,
    clean_speech_ms: u32,
}

impl ClusterEmbedding {
    /// Wrap a cluster embedding with its clean single-speaker speech total.
    pub fn new(
        cluster_key: impl Into<String>,
        embedding: Embedding,
        clean_speech_ms: u32,
    ) -> Result<Self, InsufficientSpeech> {
        SpeechGate::Embed.check(clean_speech_ms)?;
        Ok(Self {
            cluster_key: cluster_key.into(),
            embedding,
            clean_speech_ms,
        })
    }

    /// Diarization cluster key (`speaker_suggestions.cluster_key`).
    #[must_use]
    pub fn cluster_key(&self) -> &str {
        &self.cluster_key
    }

    /// The embedding.
    #[must_use]
    pub fn embedding(&self) -> &Embedding {
        &self.embedding
    }

    /// Clean single-speaker speech accumulated for this cluster.
    #[must_use]
    pub fn clean_speech_ms(&self) -> u32 {
        self.clean_speech_ms
    }
}

/// Which half of the multi-exemplar aggregate produced the score (VP-064).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    /// Similarity to the profile centroid.
    Centroid,
    /// Mean of the best [`EXEMPLAR_AGGREGATE_SIZE`] per-exemplar similarities.
    TopExemplarMean,
}

/// One (cluster, profile) outcome that is worth recording (VP-060).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredProfile {
    /// Roster person the score belongs to.
    pub person_id: String,
    /// The AS-Norm score.
    pub score: NormalisedScore,
    /// Band the score fell in — never anything stronger than `suggest`.
    pub band: Band,
    /// Which aggregate won (VP-064), kept for the §17 harness and support.
    pub aggregation: Aggregation,
}

/// The result of matching one cluster against the enrolled profiles.
///
/// Carries scores, bands and person ids — never an embedding, which is why it
/// is safe to serialize into a DTO, an event or a job's run metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchReport {
    /// Cluster this report is about.
    pub cluster_key: String,
    /// Scores at or above the floor, strongest first.
    pub recorded: Vec<ScoredProfile>,
    /// How many profiles were scored.
    pub evaluated: usize,
    /// How many scored below the floor, i.e. recorded nothing (VP-060).
    pub below_floor: usize,
}

impl MatchReport {
    /// Whether this cluster matched nobody enrolled — the expected case.
    #[must_use]
    pub fn matches_nobody(&self) -> bool {
        self.recorded.is_empty()
    }

    /// The suggestion-band outcomes, strongest first.
    pub fn suggestions(&self) -> impl Iterator<Item = &ScoredProfile> {
        self.recorded
            .iter()
            .filter(|scored| scored.band == Band::Suggest)
    }

    /// The weak-band outcomes, strongest first.
    pub fn weak_signals(&self) -> impl Iterator<Item = &ScoredProfile> {
        self.recorded
            .iter()
            .filter(|scored| scored.band == Band::Weak)
    }

    /// The `score_kind` every row of this report is persisted with (§7.2).
    #[must_use]
    pub fn score_kind(&self) -> &'static str {
        SCORE_KIND
    }
}

/// A profile with its cohort statistics already computed.
///
/// Cohort statistics depend only on the profile's own vectors, so they are
/// computed once per match run instead of once per cluster: a 12-cluster
/// recording against 20 profiles would otherwise repeat the same few hundred
/// thousand dot products twelve times.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedProfile<'a> {
    profile: &'a VoiceProfile,
    centroid_stats: CohortStats,
    exemplar_stats: Vec<CohortStats>,
}

impl PreparedProfile<'_> {
    /// The profile this preparation belongs to.
    #[must_use]
    pub fn profile(&self) -> &VoiceProfile {
        self.profile
    }
}

/// Stage 4 of the §4 pipeline: scores clusters against enrolled profiles.
#[derive(Debug, Clone, PartialEq)]
pub struct Matcher {
    binding: ModelBinding,
    thresholds: ThresholdSet,
    cohort: CohortMatrix,
    top_k: TopK,
}

impl Matcher {
    /// Build a matcher for one checkpoint.
    ///
    /// The cohort ships with the model pack and is bound to the same checkpoint
    /// (VP-052); a cohort from another checkpoint is refused here rather than
    /// quietly normalising scores against the wrong distribution.
    pub fn new(
        binding: ModelBinding,
        thresholds: ThresholdSet,
        cohort: CohortMatrix,
        top_k: TopK,
    ) -> Result<Self, MatchError> {
        if cohort.binding() != &binding {
            return Err(MatchError::ModelMismatch {
                what: "AS-Norm cohort",
                expected: binding.to_string(),
                found: cohort.binding().to_string(),
            });
        }
        if top_k.get() > cohort.len() {
            return Err(MatchError::Cohort(CohortError::TopKTooLarge {
                k: top_k.get(),
                rows: cohort.len(),
            }));
        }
        Ok(Self {
            binding,
            thresholds,
            cohort,
            top_k,
        })
    }

    /// Checkpoint this matcher scores for.
    #[must_use]
    pub fn binding(&self) -> &ModelBinding {
        &self.binding
    }

    /// Operating points in use.
    #[must_use]
    pub fn thresholds(&self) -> ThresholdSet {
        self.thresholds
    }

    /// Compute a profile's cohort statistics once, for reuse across clusters.
    pub fn prepare<'a>(
        &self,
        profile: &'a VoiceProfile,
    ) -> Result<PreparedProfile<'a>, MatchError> {
        if profile.binding() != &self.binding {
            return Err(MatchError::ModelMismatch {
                what: "profile",
                expected: self.binding.to_string(),
                found: profile.binding().to_string(),
            });
        }
        let centroid_stats = self.cohort.statistics(profile.centroid(), self.top_k)?;
        let exemplar_stats = profile
            .exemplars()
            .iter()
            .map(|exemplar| self.cohort.statistics(exemplar.embedding(), self.top_k))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedProfile {
            profile,
            centroid_stats,
            exemplar_stats,
        })
    }

    /// Prepare every profile of a set.
    pub fn prepare_all<'a>(
        &self,
        profiles: &'a [VoiceProfile],
    ) -> Result<Vec<PreparedProfile<'a>>, MatchError> {
        profiles.iter().map(|p| self.prepare(p)).collect()
    }

    /// Score one cluster against the enrolled profiles (VP-060, VP-064).
    ///
    /// Returns every outcome at or above the floor, strongest first, and
    /// counts the rest. With no profiles the answer is "nobody", produced
    /// without computing a single similarity.
    pub fn match_cluster(
        &self,
        cluster: &ClusterEmbedding,
        profiles: &[PreparedProfile<'_>],
    ) -> Result<MatchReport, MatchError> {
        SpeechGate::Identify.check(cluster.clean_speech_ms())?;
        if cluster.embedding().binding() != &self.binding {
            return Err(MatchError::ModelMismatch {
                what: "cluster embedding",
                expected: self.binding.to_string(),
                found: cluster.embedding().binding().to_string(),
            });
        }

        // The open-set common case: nobody is enrolled, so nothing is scored.
        if profiles.is_empty() {
            return Ok(MatchReport {
                cluster_key: cluster.cluster_key().to_owned(),
                recorded: Vec::new(),
                evaluated: 0,
                below_floor: 0,
            });
        }

        let test_stats = self.cohort.statistics(cluster.embedding(), self.top_k)?;
        let mut recorded = Vec::new();
        let mut below_floor = 0usize;
        for prepared in profiles {
            let (score, aggregation) = score(cluster.embedding(), test_stats, prepared);
            let band = self.thresholds.band(score, prepared.profile.offset());
            if band.is_recorded() {
                recorded.push(ScoredProfile {
                    person_id: prepared.profile.person_id().to_owned(),
                    score,
                    band,
                    aggregation,
                });
            } else {
                below_floor += 1;
            }
        }
        recorded.sort_by(|left, right| {
            right
                .score
                .total_cmp(left.score)
                .then_with(|| left.person_id.cmp(&right.person_id))
        });

        Ok(MatchReport {
            cluster_key: cluster.cluster_key().to_owned(),
            recorded,
            evaluated: profiles.len(),
            below_floor,
        })
    }
}

/// Multi-exemplar score for one profile (VP-064).
///
/// Dimensions are already known to agree: [`Matcher::prepare`] and
/// [`Matcher::match_cluster`] both go through the cohort, which refuses a
/// vector of the wrong dimension, so every vector here is cohort-shaped.
fn score(
    cluster: &Embedding,
    test_stats: CohortStats,
    prepared: &PreparedProfile<'_>,
) -> (NormalisedScore, Aggregation) {
    let centroid_score = as_norm(
        cluster.cosine_same_dim(prepared.profile.centroid()),
        prepared.centroid_stats,
        test_stats,
    );
    let exemplar_scores = prepared
        .profile
        .exemplars()
        .iter()
        .zip(&prepared.exemplar_stats)
        .map(|(exemplar, stats)| {
            as_norm(
                cluster.cosine_same_dim(exemplar.embedding()),
                *stats,
                test_stats,
            )
        });
    aggregate(centroid_score, exemplar_scores)
}

/// Combine a centroid score and per-exemplar scores into one score (VP-064).
///
/// The result is the larger of the centroid score and the mean of the best
/// [`EXEMPLAR_AGGREGATE_SIZE`] exemplar scores. Centroid-only matching degrades
/// when exemplars span devices and rooms — the average of a laptop microphone
/// and a speakerphone resembles neither — while exemplar-only matching throws
/// away the stability of an average. Taking the maximum keeps both.
///
/// Ties go to the centroid, and the result depends only on the *set* of
/// exemplar scores: it is invariant under their order (a `max` and a mean of
/// the two largest values are both symmetric).
#[must_use]
pub fn aggregate<I>(centroid: NormalisedScore, exemplars: I) -> (NormalisedScore, Aggregation)
where
    I: IntoIterator<Item = NormalisedScore>,
{
    let mut best = f64::NEG_INFINITY;
    let mut second = f64::NEG_INFINITY;
    let mut count = 0usize;
    for score in exemplars {
        let value = f64::from(score.value());
        count += 1;
        if value > best {
            second = best;
            best = value;
        } else if value > second {
            second = value;
        }
    }

    let exemplar_mean = match count {
        0 => return (centroid, Aggregation::Centroid),
        1 => best,
        _ => f64::midpoint(best, second),
    };
    if exemplar_mean > f64::from(centroid.value()) {
        (
            NormalisedScore::finite(exemplar_mean),
            Aggregation::TopExemplarMean,
        )
    } else {
        (centroid, Aggregation::Centroid)
    }
}

#[cfg(test)]
mod tests;
