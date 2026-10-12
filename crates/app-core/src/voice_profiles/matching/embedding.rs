//! Vectors, checkpoint binding and the two score types (VP-050, VP-051, VP-075).
//!
//! # Polarity lives in the type system
//!
//! VP-050 exists because the ecosystem publishes both conventions: pyannote's
//! 3.1 clustering config states a cosine **distance** threshold
//! (`0.7045654963945799`) while sherpa-onnx's speaker manager compares cosine
//! **similarity**. Copying a number across that boundary inverts the decision
//! and still type-checks if both are `f32`. Here they do not: a
//! [`CosineDistance`] cannot be compared with a [`CosineSimilarity`], and the
//! only way between them is [`CosineSimilarity::from_distance`] /
//! [`CosineSimilarity::to_distance`], which is a conversion a reviewer can see.
//!
//! A third type, [`NormalisedScore`], is what the thresholds of
//! [`super::bands`] actually compare against: AS-Norm output is a normalised
//! score, not a cosine, and is not confined to `[-1, 1]`. Keeping it distinct
//! stops a raw-cosine threshold from being applied to a normalised score.

use std::fmt;

use plaudon_domain::media::Sha256Digest;
use serde::{Deserialize, Serialize};

/// Smallest embedding dimension this module accepts.
///
/// One dimension is not a direction in any useful sense (every pair of unit
/// vectors is `+1` or `-1` apart), so it is refused rather than scored.
pub const MIN_EMBEDDING_DIM: usize = 2;

/// Largest embedding dimension this module accepts.
///
/// Deliberately the exemplar store's own ceiling
/// ([`crate::voice_profiles::store::MAX_EMBEDDING_DIM`]): a vector the store
/// will seal must be a vector the matcher will score, so the two bounds cannot
/// drift apart and strand stored exemplars. The candidates of spec §5.1 are
/// 192–512 dimensional; the ceiling exists so a corrupt or hostile stored
/// dimension cannot become an allocation (§17.3's `fuzz_embedding_blob`).
pub use crate::voice_profiles::store::MAX_EMBEDDING_DIM;

/// Shortest vector norm that still counts as a direction.
const MIN_VECTOR_NORM: f64 = 1e-6;

/// How far a norm may sit from 1.0 and still count as already normalised.
///
/// An `f32` vector of a few hundred components accumulates roughly this much
/// error in its own norm, so anything tighter would re-divide (and re-round)
/// vectors that are already unit length.
const UNIT_NORM_TOLERANCE: f64 = 1e-6;

/// The exact checkpoint a vector was produced by (VP-075).
///
/// Thresholds are per-checkpoint, never per-architecture (VP-051): the same
/// CAM++ architecture ships with vendor thresholds 0.39 and 0.31. Every
/// embedding therefore carries the identity of the checkpoint that produced
/// it, and [`super::Matcher`] refuses to score across a mismatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelBinding {
    /// Model identifier, e.g. `wespeaker-resnet34-lm`.
    pub model_id: String,
    /// Checkpoint version string.
    pub model_version: String,
    /// Digest of the checkpoint file.
    pub model_sha256: Sha256Digest,
}

impl ModelBinding {
    /// Bind to a checkpoint.
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        model_version: impl Into<String>,
        model_sha256: Sha256Digest,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            model_version: model_version.into(),
            model_sha256,
        }
    }
}

impl fmt::Display for ModelBinding {
    /// Renders as `id@version+sha12`.
    ///
    /// The digest is abbreviated to twelve hex characters: enough to tell two
    /// checkpoints apart in an error message, short enough that the result is
    /// not mistaken for a device token by the response scanner's 32-hex rule.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.model_sha256.to_hex();
        write!(f, "{}@{}+{}", self.model_id, self.model_version, &hex[..12])
    }
}

/// Why a vector could not become an [`Embedding`].
///
/// No variant carries a component value: an error message is a log line, and
/// a log line may not contain biometric data (VP-002, VP-112).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EmbeddingError {
    /// The dimension is outside [`MIN_EMBEDDING_DIM`]..=[`MAX_EMBEDDING_DIM`].
    #[error("embedding dimension {dim} is outside {MIN_EMBEDDING_DIM}..={MAX_EMBEDDING_DIM}")]
    Dimension {
        /// Dimension that was offered.
        dim: usize,
    },

    /// A component was `NaN` or infinite.
    #[error("embedding component {index} is not finite")]
    NonFinite {
        /// Position of the first offending component.
        index: usize,
    },

    /// The vector has (effectively) no direction, so it has no cosine.
    #[error("embedding has no direction: its norm is below {MIN_VECTOR_NORM:e}")]
    ZeroNorm,

    /// Two vectors of different dimensions were compared.
    #[error("cannot compare a {left}-dimensional embedding with a {right}-dimensional one")]
    DimensionMismatch {
        /// Dimension of the left operand.
        left: usize,
        /// Dimension of the right operand.
        right: usize,
    },
}

/// An L2-normalised embedding vector bound to the checkpoint that produced it.
///
/// Construction normalises (VP-050: "cosine over L2-normalised embeddings"), so
/// a cosine is a dot product and every comparison is scale-free.
///
/// # Not serializable, on purpose
///
/// This type implements neither `Serialize` nor `Deserialize`, and its `Debug`
/// shows the checkpoint and the dimension only. A vector therefore cannot
/// reach a DTO, an event, a log line, a diagnostics bundle or an export by
/// accident — only by someone adding an impl, which is a reviewable change.
///
/// ```compile_fail
/// # use plaudon_app_core::voice_profiles::matching::Embedding;
/// fn requires_serialize<T: serde::Serialize>() {}
/// requires_serialize::<Embedding>();
/// ```
///
/// ```compile_fail
/// # use plaudon_app_core::voice_profiles::matching::Embedding;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<Embedding>();
/// ```
#[derive(Clone, PartialEq)]
pub struct Embedding {
    binding: ModelBinding,
    unit: Vec<f32>,
}

impl Embedding {
    /// L2-normalise `values` and bind the result to `binding`.
    ///
    /// A vector that is already of unit length is copied rather than divided,
    /// which makes normalisation exactly idempotent: re-reading a stored unit
    /// vector (or decoding the cohort file) reproduces the same bits instead of
    /// drifting by a least-significant bit every round trip.
    pub fn l2_normalise(binding: ModelBinding, values: &[f32]) -> Result<Self, EmbeddingError> {
        if !(MIN_EMBEDDING_DIM..=MAX_EMBEDDING_DIM).contains(&values.len()) {
            return Err(EmbeddingError::Dimension { dim: values.len() });
        }
        let mut sum_squares = 0.0f64;
        for (index, value) in values.iter().enumerate() {
            if !value.is_finite() {
                return Err(EmbeddingError::NonFinite { index });
            }
            sum_squares += f64::from(*value) * f64::from(*value);
        }
        let norm = sum_squares.sqrt();
        if !norm.is_finite() || norm < MIN_VECTOR_NORM {
            return Err(EmbeddingError::ZeroNorm);
        }
        let unit = if (norm - 1.0).abs() <= UNIT_NORM_TOLERANCE {
            values.to_vec()
        } else {
            values
                .iter()
                .map(|value| (f64::from(*value) / norm) as f32)
                .collect()
        };
        Ok(Self { binding, unit })
    }

    /// Checkpoint this vector was produced by.
    #[must_use]
    pub fn binding(&self) -> &ModelBinding {
        &self.binding
    }

    /// Number of components.
    #[must_use]
    pub fn dim(&self) -> usize {
        self.unit.len()
    }

    /// The normalised components.
    ///
    /// Crate-visible: the cohort maths needs them, nothing outside does.
    pub(crate) fn unit(&self) -> &[f32] {
        &self.unit
    }

    /// Cosine similarity against another embedding (VP-050).
    ///
    /// Checkpoint equality is *not* checked here — that refusal belongs to
    /// [`super::Matcher`], which can say which side mismatched.
    pub fn cosine(&self, other: &Self) -> Result<CosineSimilarity, EmbeddingError> {
        if self.unit.len() != other.unit.len() {
            return Err(EmbeddingError::DimensionMismatch {
                left: self.unit.len(),
                right: other.unit.len(),
            });
        }
        Ok(CosineSimilarity::clamped(dot(&self.unit, &other.unit)))
    }

    /// Cosine similarity against an embedding already known to have the same
    /// dimension.
    ///
    /// The matcher establishes that invariant by routing both vectors through
    /// the cohort, which refuses a vector of the wrong dimension; this skips a
    /// per-pair check inside the scoring loop.
    pub(crate) fn cosine_same_dim(&self, other: &Self) -> CosineSimilarity {
        debug_assert_eq!(self.unit.len(), other.unit.len());
        CosineSimilarity::clamped(dot(&self.unit, &other.unit))
    }
}

impl fmt::Debug for Embedding {
    /// Checkpoint and dimension only — never components (VP-112).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Embedding")
            .field("model", &self.binding.to_string())
            .field("dim", &self.unit.len())
            .finish_non_exhaustive()
    }
}

/// Dot product of two equally long slices, accumulated in `f64`.
pub(crate) fn dot(left: &[f32], right: &[f32]) -> f64 {
    debug_assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum()
}

/// Cosine **similarity**: higher means more similar, range `[-1, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CosineSimilarity(f32);

impl CosineSimilarity {
    /// Identical directions.
    pub const MAX: Self = Self(1.0);
    /// Opposite directions.
    pub const MIN: Self = Self(-1.0);

    /// Wrap a similarity, refusing non-finite values and clamping the
    /// floating-point overshoot of an otherwise valid cosine.
    pub fn new(value: f32) -> Result<Self, ScoreError> {
        if !value.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(-1.001..=1.001).contains(&value) {
            return Err(ScoreError::OutOfRange { value });
        }
        Ok(Self::clamped(f64::from(value)))
    }

    /// Clamp a computed inner product into range.
    pub(crate) fn clamped(value: f64) -> Self {
        Self(value.clamp(-1.0, 1.0) as f32)
    }

    /// Convert a published cosine *distance* threshold (pyannote convention).
    #[must_use]
    pub fn from_distance(distance: CosineDistance) -> Self {
        Self::clamped(1.0 - f64::from(distance.value()))
    }

    /// Express this similarity as a cosine distance.
    #[must_use]
    pub fn to_distance(self) -> CosineDistance {
        CosineDistance(1.0 - self.0)
    }

    /// The underlying value.
    #[must_use]
    pub fn value(self) -> f32 {
        self.0
    }
}

impl fmt::Display for CosineSimilarity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4}", self.0)
    }
}

/// Cosine **distance**: lower means more similar, range `[0, 2]`.
///
/// Exists so a vendor threshold published in distance form (pyannote) can be
/// carried verbatim and converted explicitly, never silently compared.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CosineDistance(f32);

impl CosineDistance {
    /// Wrap a distance, refusing non-finite and out-of-range values.
    pub fn new(value: f32) -> Result<Self, ScoreError> {
        if !value.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(-0.001..=2.001).contains(&value) {
            return Err(ScoreError::OutOfRange { value });
        }
        Ok(Self(value))
    }

    /// The underlying value.
    #[must_use]
    pub fn value(self) -> f32 {
        self.0
    }
}

/// Score produced by AS-Norm (VP-052), and the space the thresholds live in.
///
/// Normalisation subtracts a cohort mean and divides by a cohort standard
/// deviation, so the result is not a cosine and is not confined to `[-1, 1]`.
/// It is finite by construction.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct NormalisedScore(f32);

impl NormalisedScore {
    /// Wrap a normalised score, refusing non-finite values.
    pub fn new(value: f32) -> Result<Self, ScoreError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(ScoreError::NotFinite)
        }
    }

    /// Wrap a value already known to be finite.
    pub(crate) fn finite(value: f64) -> Self {
        debug_assert!(value.is_finite());
        Self(value as f32)
    }

    /// The underlying value.
    #[must_use]
    pub fn value(self) -> f32 {
        self.0
    }

    /// Total order over finite scores, for sorting and selection.
    #[must_use]
    pub fn total_cmp(self, other: Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl TryFrom<f32> for NormalisedScore {
    type Error = ScoreError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<NormalisedScore> for f32 {
    fn from(score: NormalisedScore) -> Self {
        score.0
    }
}

impl fmt::Display for NormalisedScore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4}", self.0)
    }
}

/// Why a score value was refused.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum ScoreError {
    /// `NaN` or an infinity reached a score constructor.
    #[error("score is not finite")]
    NotFinite,

    /// A cosine or distance outside its definition range.
    #[error("score {value} is outside the range of its kind")]
    OutOfRange {
        /// The offending value.
        value: f32,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        CosineDistance, CosineSimilarity, Embedding, EmbeddingError, ModelBinding, NormalisedScore,
        ScoreError, MAX_EMBEDDING_DIM,
    };
    use crate::voice_profiles::matching::test_support::binding;

    #[test]
    fn normalisation_makes_cosine_scale_free() {
        let small = Embedding::l2_normalise(binding(), &[1.0, 2.0, 2.0]).expect("small");
        let large = Embedding::l2_normalise(binding(), &[100.0, 200.0, 200.0]).expect("large");
        let cosine = small.cosine(&large).expect("cosine");
        assert!((cosine.value() - 1.0).abs() < 1e-6, "{cosine}");
    }

    #[test]
    fn normalising_an_already_normalised_vector_changes_nothing() {
        let once = Embedding::l2_normalise(binding(), &[0.3, -0.7, 0.2, 0.9]).expect("once");
        let twice = Embedding::l2_normalise(binding(), once.unit()).expect("twice");
        assert_eq!(
            once.unit(),
            twice.unit(),
            "normalisation must be idempotent"
        );
    }

    #[test]
    fn orthogonal_vectors_score_zero_and_opposite_vectors_score_minus_one() {
        let east = Embedding::l2_normalise(binding(), &[1.0, 0.0]).expect("east");
        let north = Embedding::l2_normalise(binding(), &[0.0, 3.0]).expect("north");
        let west = Embedding::l2_normalise(binding(), &[-2.0, 0.0]).expect("west");
        assert!(east.cosine(&north).expect("orthogonal").value().abs() < 1e-7);
        assert!((east.cosine(&west).expect("opposite").value() + 1.0).abs() < 1e-7);
    }

    #[test]
    fn a_vector_with_no_direction_or_a_hostile_component_is_refused() {
        assert_eq!(
            Embedding::l2_normalise(binding(), &[0.0, 0.0]),
            Err(EmbeddingError::ZeroNorm)
        );
        assert_eq!(
            Embedding::l2_normalise(binding(), &[1.0, f32::NAN]),
            Err(EmbeddingError::NonFinite { index: 1 })
        );
        assert_eq!(
            Embedding::l2_normalise(binding(), &[f32::INFINITY, 1.0]),
            Err(EmbeddingError::NonFinite { index: 0 })
        );
        assert_eq!(
            Embedding::l2_normalise(binding(), &[1.0]),
            Err(EmbeddingError::Dimension { dim: 1 })
        );
        let oversized = vec![1.0f32; MAX_EMBEDDING_DIM + 1];
        assert_eq!(
            Embedding::l2_normalise(binding(), &oversized),
            Err(EmbeddingError::Dimension {
                dim: MAX_EMBEDDING_DIM + 1
            })
        );
    }

    #[test]
    fn embeddings_of_different_dimensions_cannot_be_compared() {
        let two = Embedding::l2_normalise(binding(), &[1.0, 0.0]).expect("two");
        let three = Embedding::l2_normalise(binding(), &[1.0, 0.0, 0.0]).expect("three");
        assert_eq!(
            two.cosine(&three),
            Err(EmbeddingError::DimensionMismatch { left: 2, right: 3 })
        );
    }

    #[test]
    fn embedding_debug_shows_the_checkpoint_and_dimension_but_no_component() {
        let embedding =
            Embedding::l2_normalise(binding(), &[0.123_456_7, 0.987_654_3]).expect("embedding");
        let rendered = format!("{embedding:?}");
        assert!(rendered.contains("dim: 2"), "{rendered}");
        assert!(rendered.contains("test-embedder"), "{rendered}");
        assert!(!rendered.contains("123456"), "{rendered}");
        assert!(!rendered.contains("98765"), "{rendered}");
    }

    #[test]
    fn a_distance_threshold_only_becomes_a_similarity_through_an_explicit_conversion() {
        // pyannote 3.1's published clustering threshold, in distance form.
        let published = CosineDistance::new(0.704_565_5).expect("distance");
        let similarity = CosineSimilarity::from_distance(published);
        assert!(
            (similarity.value() - 0.295_434_5).abs() < 1e-6,
            "{similarity}"
        );
        assert!(
            (similarity.to_distance().value() - published.value()).abs() < 1e-6,
            "round trip"
        );
    }

    #[test]
    fn score_constructors_refuse_non_finite_and_out_of_range_values() {
        assert_eq!(CosineSimilarity::new(f32::NAN), Err(ScoreError::NotFinite));
        assert_eq!(
            CosineSimilarity::new(1.5),
            Err(ScoreError::OutOfRange { value: 1.5 })
        );
        assert_eq!(
            CosineDistance::new(f32::INFINITY),
            Err(ScoreError::NotFinite)
        );
        assert_eq!(
            CosineDistance::new(-1.0),
            Err(ScoreError::OutOfRange { value: -1.0 })
        );
        assert_eq!(NormalisedScore::new(f32::NAN), Err(ScoreError::NotFinite));
        assert!(NormalisedScore::new(12.5).is_ok(), "AS-Norm is unbounded");
    }

    #[test]
    fn a_normalised_score_round_trips_through_json_but_a_non_finite_one_cannot_be_parsed() {
        let score = NormalisedScore::new(1.25).expect("score");
        let json = serde_json::to_string(&score).expect("serialize");
        assert_eq!(json, "1.25");
        let parsed: NormalisedScore = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, score);
        assert!(serde_json::from_str::<NormalisedScore>("1e400").is_err());
    }

    #[test]
    fn a_model_binding_renders_as_checkpoint_identity_with_a_short_digest() {
        let rendered = ModelBinding::new(
            "wespeaker-resnet34-lm",
            "1.0",
            "3b1f8c2d4e5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e"
                .parse()
                .expect("digest"),
        )
        .to_string();
        assert_eq!(rendered, "wespeaker-resnet34-lm@1.0+3b1f8c2d4e5a");
    }
}
