//! Stage 3 of the §4 pipeline: the speaker-embedding seam
//! (`VOICE_PROFILES_SPEC` VP-022..VP-026, VP-040, VP-051, VP-103; ADR-0081).
//!
//! An embedder is a *checkpoint plus a function from a window of audio to a
//! vector*. Everything that makes a score comparable — the model identity, its
//! digest, the operating point and the AS-Norm cohort — is a property of that
//! checkpoint and ships with it in the model pack (VP-040, VP-051, VP-052), so
//! it is read from the embedder rather than configured beside it. That is what
//! makes "thresholds are per-checkpoint, never per-architecture" structurally
//! true instead of a convention somebody has to remember.
//!
//! The trait takes a *path and a time window*, not samples, for the same
//! reason [`plaudon_ai_contracts::stt::SpeechToText`] does: decoding belongs to
//! the engine that knows what its model wants, and keeping it there means no
//! decoded PCM of a whole recording has to exist in this crate's memory. The
//! window boundary is also the cancellation point (VP-103): the embedding loop
//! is the expensive one, and a user who pressed Cancel should not wait for a
//! whole recording.
//!
//! No real model lives here. [`ScriptedEmbedder`] is the deterministic double
//! that makes the whole pipeline testable without weights (§17.5, VP-183), and
//! it is a *double*, not a mock: it implements the entire contract, honours
//! cancellation, refuses to answer after a configured failure and produces the
//! same vectors on every platform. The real ONNX embedder (EPIC P phase 2)
//! implements the same trait and nothing above it changes.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_ai_contracts::job::{CancellationToken, JobContext};
use plaudon_domain::media::Sha256Digest;

use super::matching::{
    CohortMatrix, Embedding, EmbeddingError, ModelBinding, NormalisedScore, ThresholdSet, TopK,
};

/// One window of single-speaker audio to embed.
///
/// The window is always inside a span the reconciliation attributed to exactly
/// one cluster (VP-022): overlapping speech never reaches an embedder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedWindow {
    /// Media file the window is cut from.
    pub media_path: PathBuf,
    /// Cluster the window belongs to.
    pub cluster_key: String,
    /// Inclusive start offset in the recording, in milliseconds.
    pub start_ms: u64,
    /// Exclusive end offset in the recording, in milliseconds.
    pub end_ms: u64,
}

impl EmbedWindow {
    /// Window length in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// Why an embedding could not be produced.
///
/// No variant carries a vector component or a file's contents: an error is a
/// log line, and a log line may never contain biometric data (VP-002, VP-112).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmbedError {
    /// The run was cancelled at a window boundary (VP-103).
    #[error("speaker embedding was cancelled")]
    Cancelled,
    /// The checkpoint is not installed in this build or library.
    #[error("the speaker-embedding model is not available")]
    Unavailable,
    /// The media could not be read or decoded.
    #[error("the window could not be decoded from the media")]
    Undecodable,
    /// The model returned something that is not an embedding.
    #[error("the embedder returned a malformed vector: {0}")]
    Malformed(String),
    /// The engine failed in a way this build does not model.
    #[error("speaker embedding failed")]
    EngineFailure,
}

impl From<EmbeddingError> for EmbedError {
    fn from(error: EmbeddingError) -> Self {
        Self::Malformed(error.to_string())
    }
}

/// Everything about an embedder that is a property of its checkpoint.
///
/// Carried as one value because these fields only make sense together: a
/// threshold set belongs to a checkpoint (VP-051) and an AS-Norm cohort is
/// computed under one (VP-052), so handing them out separately would invite
/// exactly the mix-and-match the spec forbids.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedderCheckpoint {
    /// Model id, version and digest of the weights (VP-026).
    pub binding: ModelBinding,
    /// Dimensionality of every vector this checkpoint produces.
    pub dim: usize,
    /// Operating point shipped with this checkpoint (VP-051, VP-062).
    pub thresholds: ThresholdSet,
    /// Version of that operating point, which keys the job (VP-101).
    pub threshold_set_version: String,
    /// Impostor cohort shipped with the model pack (VP-052).
    pub cohort: CohortMatrix,
    /// Cohort selection size AS-Norm averages over.
    pub top_k: TopK,
}

/// A speaker-embedding model.
#[async_trait]
pub trait SpeakerEmbedder: Send + Sync + fmt::Debug {
    /// The checkpoint this embedder is, with its operating point and cohort.
    ///
    /// Shared rather than cloned: the cohort is a few hundred kilobytes and is
    /// read once per match run.
    fn checkpoint(&self) -> Arc<EmbedderCheckpoint>;

    /// Embed one window of single-speaker audio.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Cancelled`] when the context was cancelled,
    /// [`EmbedError::Unavailable`] when the checkpoint is not installed, and
    /// [`EmbedError::Malformed`] when the model's output is not a usable
    /// vector — never a low-quality vector in its place (VP-023).
    async fn embed_window(
        &self,
        window: &EmbedWindow,
        ctx: &JobContext,
    ) -> Result<Embedding, EmbedError>;
}

/// Provider id of the deterministic scripted embedder.
pub const SCRIPTED_EMBEDDER_ID: &str = "scripted-deterministic";

/// Version of the scripted checkpoint's fixture operating point.
pub const SCRIPTED_THRESHOLD_SET_VERSION: &str = "fixture-1";

/// Rows in the scripted cohort. Small on purpose: it exists to exercise
/// AS-Norm, not to model a corpus.
const SCRIPTED_COHORT_ROWS: usize = 16;

/// A deterministic embedder with no model, no corpus and no file access.
///
/// Each cluster key is a "voice": the same key always produces the same
/// direction, different keys produce near-orthogonal ones, and a window's
/// position inside the recording perturbs the vector slightly, so aggregating
/// several windows is a real aggregation rather than a repeat of one value.
///
/// The operating point below is a **fixture**, not a measurement: no threshold
/// in this file was derived from an evaluation harness, and none of them may be
/// quoted as one (VP-062, §17.5).
pub struct ScriptedEmbedder {
    checkpoint: Arc<EmbedderCheckpoint>,
    voices: BTreeMap<String, Vec<f32>>,
    failure: Option<EmbedError>,
    cancel_after: Option<(usize, CancellationToken)>,
    calls: AtomicUsize,
    windows: Mutex<Vec<EmbedWindow>>,
}

impl fmt::Debug for ScriptedEmbedder {
    /// Prints the checkpoint, the script and the call count — never a voice
    /// vector, because `Debug` output is a log line (VP-002, VP-112).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedEmbedder")
            .field("binding", &self.checkpoint.binding.to_string())
            .field("dim", &self.checkpoint.dim)
            .field("voices", &self.voices.len())
            .field("failure", &self.failure)
            .field("cancel_after", &self.cancel_after.as_ref().map(|(n, _)| *n))
            .field("calls", &self.call_count())
            .field(
                "windows",
                &self.windows.lock().map_or(0, |windows| windows.len()),
            )
            .finish()
    }
}

impl ScriptedEmbedder {
    /// A scripted checkpoint of `dim` dimensions.
    ///
    /// # Panics
    ///
    /// Panics if `dim` is outside the embedding bounds, which only a test can
    /// arrange.
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self::versioned(dim, "1.0")
    }

    /// A scripted checkpoint of `dim` dimensions under a named version.
    ///
    /// Two versions are two checkpoints: their vectors never score against
    /// each other (VP-026, VP-075), which is what a model-upgrade test needs.
    ///
    /// # Panics
    ///
    /// Panics if `dim` is outside the embedding bounds.
    #[must_use]
    pub fn versioned(dim: usize, version: &str) -> Self {
        let binding = ModelBinding::new(
            SCRIPTED_EMBEDDER_ID,
            version,
            Sha256Digest::of(format!("{SCRIPTED_EMBEDDER_ID}:{version}:{dim}").as_bytes()),
        );
        let cohort = CohortMatrix::from_vectors(
            binding.clone(),
            &pseudo_random_rows(SCRIPTED_COHORT_ROWS, dim, 0x5EED_1234),
        )
        .expect("the scripted cohort is well formed");
        let thresholds = ThresholdSet::new(
            NormalisedScore::new(1.0).expect("finite"),
            NormalisedScore::new(2.5).expect("finite"),
            0.1,
        )
        .expect("the fixture operating point is ordered");
        Self {
            checkpoint: Arc::new(EmbedderCheckpoint {
                binding,
                dim,
                thresholds,
                threshold_set_version: SCRIPTED_THRESHOLD_SET_VERSION.to_owned(),
                cohort,
                top_k: TopK::new(4).expect("the scripted cohort has at least four rows"),
            }),
            voices: BTreeMap::new(),
            failure: None,
            cancel_after: None,
            calls: AtomicUsize::new(0),
            windows: Mutex::new(Vec::new()),
        }
    }

    /// Give one cluster key an explicit voice direction.
    ///
    /// Used to script "this cluster is the same person as that exemplar" and
    /// its opposite without relying on the derived directions.
    ///
    /// # Panics
    ///
    /// Panics if `vector` is not the checkpoint's dimension.
    #[must_use]
    pub fn with_voice(mut self, cluster_key: &str, vector: Vec<f32>) -> Self {
        assert_eq!(
            vector.len(),
            self.checkpoint.dim,
            "a scripted voice has the checkpoint's dimension"
        );
        self.voices.insert(cluster_key.to_owned(), vector);
        self
    }

    /// Cancel `token` once `calls` windows have been embedded.
    ///
    /// The deterministic stand-in for a user pressing Cancel in the middle of
    /// the embedding loop, and for a process killed between two stages: the
    /// window count a fixture produces is known, so the stop lands exactly
    /// where the test says it does.
    #[must_use]
    pub fn cancelling_after(mut self, calls: usize, token: CancellationToken) -> Self {
        self.cancel_after = Some((calls, token));
        self
    }

    /// Fail every window with `error` instead of embedding it.
    #[must_use]
    pub fn failing(mut self, error: EmbedError) -> Self {
        self.failure = Some(error);
        self
    }

    /// How many windows were actually embedded.
    ///
    /// The number a resume test watches: resuming after the embed stage
    /// committed must not embed anything again.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Every window this embedder was asked for, in order.
    #[must_use]
    pub fn windows(&self) -> Vec<EmbedWindow> {
        self.windows
            .lock()
            .map(|windows| windows.clone())
            .unwrap_or_default()
    }

    /// The deterministic voice direction of a cluster key.
    ///
    /// Exposed so a test can enrol the same direction it will later match
    /// against, without hard-coding vectors.
    #[must_use]
    pub fn voice_of(&self, cluster_key: &str) -> Vec<f32> {
        self.voices.get(cluster_key).cloned().unwrap_or_else(|| {
            pseudo_random_rows(1, self.checkpoint.dim, seed_of(cluster_key))
                .pop()
                .expect("one row")
        })
    }

    /// Share this embedder with the pipeline.
    #[must_use]
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[async_trait]
impl SpeakerEmbedder for ScriptedEmbedder {
    fn checkpoint(&self) -> Arc<EmbedderCheckpoint> {
        Arc::clone(&self.checkpoint)
    }

    async fn embed_window(
        &self,
        window: &EmbedWindow,
        ctx: &JobContext,
    ) -> Result<Embedding, EmbedError> {
        if ctx.is_cancelled() {
            return Err(EmbedError::Cancelled);
        }
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut windows) = self.windows.lock() {
            windows.push(window.clone());
        }
        if let Some((after, token)) = &self.cancel_after {
            if calls >= *after {
                token.cancel();
            }
        }

        // The voice, perturbed by where the window sits: windows of one
        // cluster stay close to each other and far from another cluster's.
        let mut values = self.voice_of(&window.cluster_key);
        let jitter = (window.start_ms % 1_000) as f32 / 40_000.0;
        for (index, value) in values.iter_mut().enumerate() {
            if index % 4 == 0 {
                *value += jitter;
            }
        }
        Ok(Embedding::l2_normalise(
            self.checkpoint.binding.clone(),
            &values,
        )?)
    }
}

/// Stable seed for a cluster key, so a voice is reproducible across processes.
fn seed_of(key: &str) -> u64 {
    key.bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
        .max(1)
}

/// `rows × dim` deterministic vectors in `[-1, 1)` from a plain LCG.
///
/// A dependency-free generator on purpose: the scripted checkpoint must be
/// byte-identical on every platform and must not drift when a crate upgrades.
fn pseudo_random_rows(rows: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit_interval = (state >> 33) as f64 / f64::from(1u32 << 31);
        (unit_interval * 2.0 - 1.0) as f32
    };
    (0..rows)
        .map(|_| (0..dim).map(|_| next()).collect())
        .collect()
}
