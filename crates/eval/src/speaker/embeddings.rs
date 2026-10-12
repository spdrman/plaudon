//! Where the harness gets embeddings from (VP-183).
//!
//! The harness never runs a model. It asks an [`EmbeddingSource`] for the
//! vector of a (speaker, utterance, condition) triple, which gives exactly two
//! implementations and one honest distinction between them:
//!
//! * [`ScriptedEmbeddings`] — deterministic vectors from a seed. Hermetic, no
//!   audio, no model file, identical on every platform. This is the CI tier,
//!   and a report produced from it is **not** release evidence: it measures
//!   the harness, not a model.
//! * [`ModelPackEmbeddings`] — vectors a real extractor already produced,
//!   handed over as a file inside a model pack. When the pack exists the same
//!   code path produces a report that *is* about a checkpoint, pinned to its
//!   sha256 (ADR-0081 §4).
//!
//! The split exists because inference belongs out of process (ADR-0015) and
//! because a test crate that can download or load an ONNX file would make
//! "hermetic CI" a convention instead of a property.
//!
//! No vector ever leaves this module. [`super::report`] carries scores,
//! never components — VP-002 and VP-112 forbid an embedding in a log, an
//! export or any artifact, and a committed benchmark file is all three.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plaudon_app_core::voice_profiles::matching::{Embedding, EmbeddingError, ModelBinding};
use plaudon_domain::media::Sha256Digest;
use serde::{Deserialize, Serialize};

use super::fixtures::Condition;

/// Schema version of the model-pack embedding file this build reads.
pub const EMBEDDING_FILE_SCHEMA_VERSION: u32 = 1;

/// File a model pack must contain for the harness to measure it.
pub const EMBEDDING_FILE_NAME: &str = "eval-embeddings.json";

/// Which utterance of which speaker, under which condition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentKey {
    /// Speaker label from the fixture library.
    pub speaker: String,
    /// Utterance index inside that speaker's fixture material.
    pub utterance: u32,
    /// Channel condition the material was put through.
    pub condition: Condition,
}

impl SegmentKey {
    /// Build a key.
    #[must_use]
    pub fn new(speaker: impl Into<String>, utterance: u32, condition: Condition) -> Self {
        Self {
            speaker: speaker.into(),
            utterance,
            condition,
        }
    }

    /// Stable textual form, used as the model-pack file's key and in trial ids.
    #[must_use]
    pub fn token(&self) -> String {
        format!("{}/{}/{}", self.speaker, self.utterance, self.condition)
    }
}

/// How the embeddings behind a report were produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Deterministic vectors from a seed: a harness self-test, not a model
    /// measurement.
    Scripted,
    /// Vectors a real extractor produced for a pinned checkpoint.
    ModelPack,
}

impl SourceKind {
    /// Whether a report built from this source may be cited as ADR-0081 §4
    /// release evidence.
    #[must_use]
    pub const fn is_model_measurement(self) -> bool {
        matches!(self, Self::ModelPack)
    }
}

/// Why an embedding could not be produced.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EmbeddingSourceError {
    /// The source has no vector for this segment.
    #[error("no embedding for segment `{token}`")]
    MissingSegment {
        /// The segment that was asked for.
        token: String,
    },

    /// The stored vector is not a usable embedding.
    #[error("segment `{token}` has an unusable embedding: {source}")]
    Unusable {
        /// The segment that was asked for.
        token: String,
        /// Why the vector was refused.
        #[source]
        source: EmbeddingError,
    },
}

/// Why a model pack could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum ModelPackError {
    /// The pack directory has no embedding file: the real-model tier is simply
    /// not available here, which is the normal state in CI.
    #[error("model pack {} has no {EMBEDDING_FILE_NAME}", .0.display())]
    NotPresent(PathBuf),

    /// The file exists but could not be read.
    #[error("reading {}: {source}", .path.display())]
    Unreadable {
        /// File that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The file exists but is not a valid embedding document.
    #[error("{}: {source}", .path.display())]
    Malformed {
        /// File that failed.
        path: PathBuf,
        /// Parse error.
        #[source]
        source: serde_json::Error,
    },

    /// The document declares a schema version this build does not read.
    #[error("embedding file schema version {found} is not {EMBEDDING_FILE_SCHEMA_VERSION}")]
    UnsupportedSchemaVersion {
        /// Version the file declared.
        found: u32,
    },

    /// The document carries no vectors.
    #[error("embedding file carries no vectors")]
    Empty,
}

/// A supplier of embeddings for the harness.
pub trait EmbeddingSource {
    /// Checkpoint (real or scripted) every vector is bound to.
    fn binding(&self) -> &ModelBinding;

    /// Dimension of every vector.
    fn dim(&self) -> usize;

    /// How these vectors were produced.
    fn kind(&self) -> SourceKind;

    /// Raw components of one segment's vector.
    ///
    /// The only reason this exists next to [`Self::embedding`] is the AS-Norm
    /// cohort, which [`plaudon_app_core::voice_profiles::matching::CohortMatrix`]
    /// builds from component slices. Nothing else in the harness may call it,
    /// and nothing anywhere may put the result into a report.
    fn vector(&self, key: &SegmentKey) -> Result<Vec<f32>, EmbeddingSourceError>;

    /// The embedding of one segment.
    fn embedding(&self, key: &SegmentKey) -> Result<Embedding, EmbeddingSourceError> {
        let values = self.vector(key)?;
        Embedding::l2_normalise(self.binding().clone(), &values).map_err(|source| {
            EmbeddingSourceError::Unusable {
                token: key.token(),
                source,
            }
        })
    }
}

/// Deterministic vectors for the hermetic tier.
///
/// Each speaker gets a fixed direction from the seed; each utterance is that
/// direction plus seeded noise whose magnitude depends on the condition, so
/// degraded channels score worse than clean ones *by construction*. That is a
/// property of this generator, published as such: it reproduces the shape of a
/// speaker-verification problem (targets above non-targets, with overlap) so
/// the metric code can be exercised end to end, and it says nothing whatsoever
/// about any real model's accuracy.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptedEmbeddings {
    binding: ModelBinding,
    dim: usize,
    seed: u64,
    within_speaker_spread: f32,
}

/// Extra within-speaker spread each condition adds, relative to clean.
///
/// Ordered deliberately: a low-bitrate codec and a room impulse response cost
/// more than noise at a decent SNR. The numbers are generator parameters, not
/// measurements; the report says so.
#[must_use]
pub fn condition_spread_multiplier(condition: Condition) -> f32 {
    match condition {
        Condition::Clean => 1.0,
        Condition::NoiseMusan => 1.4,
        Condition::TelephoneBandpass => 1.7,
        Condition::OpusLowBitrate => 2.0,
        Condition::RoomImpulseResponse => 2.3,
    }
}

/// Model identifier a scripted run binds to.
///
/// It names itself, so no reader of a report, a database row or an error
/// message can mistake a scripted run for a checkpoint measurement.
pub const SCRIPTED_MODEL_ID: &str = "scripted-synthetic";

/// Seed the committed CI artifact is generated with.
pub const DEFAULT_SCRIPTED_SEED: u64 = 20_261_012;

/// Embedding dimension the scripted tier uses.
///
/// 192 is the dimension of the smaller candidates of spec §5.1, so the
/// scripted tier exercises the same vector sizes the real one will.
pub const DEFAULT_SCRIPTED_DIM: usize = 192;

/// Checkpoint version of the scripted generator.
///
/// Bumped when the generator's arithmetic changes, because that invalidates
/// every committed artifact and every golden expectation derived from it.
pub const SCRIPTED_MODEL_VERSION: &str = "v1";

impl ScriptedEmbeddings {
    /// Default within-speaker spread of the clean condition.
    pub const DEFAULT_SPREAD: f32 = 1.6;

    /// The binding a scripted run uses.
    ///
    /// The digest is taken over the generator's own parameters rather than
    /// over a model file, because there is no model file: it is an identity
    /// for "these exact scripted vectors", so the matching core's
    /// checkpoint-mismatch refusal (VP-075) still protects scripted runs from
    /// being compared across seeds or dimensions.
    #[must_use]
    pub fn binding_for(seed: u64, dim: usize) -> ModelBinding {
        let identity = format!(
            "plaudon-eval/{SCRIPTED_MODEL_ID}/{SCRIPTED_MODEL_VERSION}/seed={seed}/dim={dim}"
        );
        ModelBinding::new(
            SCRIPTED_MODEL_ID,
            SCRIPTED_MODEL_VERSION,
            Sha256Digest::of(identity.as_bytes()),
        )
    }

    /// Build the generator for a seed and dimension, binding included.
    #[must_use]
    pub fn seeded(seed: u64, dim: usize) -> Self {
        Self::new(Self::binding_for(seed, dim), dim, seed)
    }

    /// Build a generator.
    ///
    /// `binding` is still required: the matching core refuses to score across
    /// checkpoints (VP-075), and a scripted run must not be able to pretend to
    /// be a real one, so callers pass a binding whose model id names itself.
    #[must_use]
    pub fn new(binding: ModelBinding, dim: usize, seed: u64) -> Self {
        Self {
            binding,
            dim,
            seed,
            within_speaker_spread: Self::DEFAULT_SPREAD,
        }
    }

    /// Replace the clean-condition spread (larger = harder problem).
    #[must_use]
    pub fn with_spread(mut self, spread: f32) -> Self {
        self.within_speaker_spread = spread;
        self
    }

    /// The seeded direction of one speaker.
    fn speaker_direction(&self, speaker: &str) -> Vec<f32> {
        let mut state = super::mix(self.seed, super::hash_text(speaker));
        (0..self.dim)
            .map(|_| {
                state = super::next_u64(state);
                super::unit_gaussian(state)
            })
            .collect()
    }
}

impl EmbeddingSource for ScriptedEmbeddings {
    fn binding(&self) -> &ModelBinding {
        &self.binding
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Scripted
    }

    fn vector(&self, key: &SegmentKey) -> Result<Vec<f32>, EmbeddingSourceError> {
        let direction = self.speaker_direction(&key.speaker);
        let spread = self.within_speaker_spread * condition_spread_multiplier(key.condition);
        let mut state = super::mix(
            super::mix(
                self.seed ^ 0x5DEE_CE66_D3A1_0C21,
                super::hash_text(&key.token()),
            ),
            u64::from(key.utterance).wrapping_add(1),
        );
        Ok(direction
            .iter()
            .map(|component| {
                state = super::next_u64(state);
                component + spread * super::unit_gaussian(state)
            })
            .collect())
    }
}

/// The on-disk document a model pack supplies for the real-embedding tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingFile {
    /// Must equal [`EMBEDDING_FILE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Checkpoint that produced every vector.
    pub model: ModelBinding,
    /// Dimension of every vector.
    pub dim: usize,
    /// Vectors keyed by [`SegmentKey::token`].
    pub vectors: BTreeMap<String, Vec<f32>>,
}

/// Embeddings a real extractor produced, loaded from a model pack.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPackEmbeddings {
    file: EmbeddingFile,
}

impl ModelPackEmbeddings {
    /// Open the embedding file of a model pack directory.
    ///
    /// A missing file is [`ModelPackError::NotPresent`], not a failure: the
    /// caller falls back to the scripted tier and the report records that no
    /// model was measured.
    pub fn open(pack_dir: &Path) -> Result<Self, ModelPackError> {
        let path = pack_dir.join(EMBEDDING_FILE_NAME);
        if !path.exists() {
            return Err(ModelPackError::NotPresent(pack_dir.to_path_buf()));
        }
        let text = std::fs::read_to_string(&path).map_err(|source| ModelPackError::Unreadable {
            path: path.clone(),
            source,
        })?;
        let file: EmbeddingFile =
            serde_json::from_str(&text).map_err(|source| ModelPackError::Malformed {
                path: path.clone(),
                source,
            })?;
        if file.schema_version != EMBEDDING_FILE_SCHEMA_VERSION {
            return Err(ModelPackError::UnsupportedSchemaVersion {
                found: file.schema_version,
            });
        }
        if file.vectors.is_empty() {
            return Err(ModelPackError::Empty);
        }
        Ok(Self { file })
    }

    /// Wrap an already-parsed document (used by tests and by callers that
    /// obtained the file some other way).
    pub fn from_file(file: EmbeddingFile) -> Result<Self, ModelPackError> {
        if file.schema_version != EMBEDDING_FILE_SCHEMA_VERSION {
            return Err(ModelPackError::UnsupportedSchemaVersion {
                found: file.schema_version,
            });
        }
        if file.vectors.is_empty() {
            return Err(ModelPackError::Empty);
        }
        Ok(Self { file })
    }

    /// How many segments the pack supplies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.file.vectors.len()
    }

    /// Whether the pack supplies no segments (it cannot: construction refuses).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.file.vectors.is_empty()
    }
}

impl EmbeddingSource for ModelPackEmbeddings {
    fn binding(&self) -> &ModelBinding {
        &self.file.model
    }

    fn dim(&self) -> usize {
        self.file.dim
    }

    fn kind(&self) -> SourceKind {
        SourceKind::ModelPack
    }

    fn vector(&self, key: &SegmentKey) -> Result<Vec<f32>, EmbeddingSourceError> {
        let token = key.token();
        self.file
            .vectors
            .get(&token)
            .cloned()
            .ok_or(EmbeddingSourceError::MissingSegment { token })
    }
}
