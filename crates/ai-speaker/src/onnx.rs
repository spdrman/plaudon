//! The real speaker-embedding engine (ADR-0081 §3, VP-024..VP-026).
//!
//! Two types live here and the split is the point:
//!
//! * [`SpeakerEmbeddingModel`] is *a checkpoint plus a function from samples
//!   to a vector*. It owns the ONNX session and the feature front end, it
//!   verifies the weights against [`crate::checkpoint`] before the session is
//!   created, and it knows nothing about profiles, thresholds or scores. It is
//!   what runs **inside the sandboxed helper process**
//!   (`src/bin/plaudon-speaker-helper.rs`), and what the evaluation harness
//!   measures directly when it is measuring the model rather than the product.
//! * [`OnnxSpeakerEmbedder`] is the [`SpeakerEmbedder`] the product's pipeline
//!   consumes. It adds exactly what the trait demands and the model cannot
//!   supply: media decoding, cancellation at the window boundary, and an
//!   operating point (thresholds plus AS-Norm cohort) that must come from the
//!   model pack because a checkpoint's thresholds are measured, never chosen.
//!   It never loads a checkpoint itself: it holds a
//!   [`SandboxedModel`][crate::sandboxed::SandboxedModel], so the model file
//!   is parsed and ONNX Runtime is run in a different process under
//!   `plaudon_security::sandbox` (ADR-0015, VP-025).
//!
//! **This engine is behind the off-by-default `onnx` feature and is not wired
//! into any composition root.** ADR-0081 §4 lets it ship only once RTF, peak
//! RSS, startup, DER and EER are recorded on *both* reference platforms; one
//! platform is measured (`docs/hardware/macos-apple-silicon.md`), Windows x64
//! is not. One further condition is open and is not pretended away here: the
//! operating point must come from a harness run whose `is_shippable_default()`
//! is true.
//!
//! Media *decoding* still happens in this process. That is the EPIC F decoder
//! boundary (open finding F-012), not this one: what moved out of process here
//! is model parsing and inference, which is what ADR-0081 §3 names.
//!
//! **No vector is ever logged.** Errors carry a reason and never a component,
//! because a log line that carries an embedding is biometric data in a log
//! (VP-002, VP-112).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ort::session::Session;
use ort::value::Tensor;
use plaudon_ai_contracts::job::JobContext;
use plaudon_app_core::voice_profiles::embedder::{
    EmbedError, EmbedWindow, EmbedderCheckpoint, SpeakerEmbedder,
};
use plaudon_app_core::voice_profiles::matching::{
    CohortMatrix, Embedding, ModelBinding, ThresholdSet, TopK,
};
use plaudon_audio::decode::{
    decode_to_canonical_pcm, CanonicalPcmProfile, PcmSampleFormat, PcmSink, ScratchPcm,
};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::media::Sha256Digest;

use crate::checkpoint::{verify_artifact, ArtifactError, ModelArtifact, MODEL_SAMPLE_RATE_HZ};
use crate::fbank::{FbankComputer, FbankError, FbankOptions};
use crate::sandboxed::SandboxedModel;

/// ONNX graph input the WeSpeaker checkpoint declares.
const FEATURE_INPUT: &str = "feats";

/// ONNX graph output the WeSpeaker checkpoint declares.
const EMBEDDING_OUTPUT: &str = "embs";

/// Why the engine could not be opened or could not produce a vector.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The weights on disk are not the reviewed weights.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),

    /// The feature front end could not be built or run.
    #[error(transparent)]
    Features(#[from] FbankError),

    /// ONNX Runtime refused the model or the run.
    #[error("the inference runtime failed: {0}")]
    Runtime(String),

    /// The model's graph is not the graph this engine knows how to drive.
    #[error("the checkpoint declares {found}, expected {expected}")]
    UnexpectedGraph {
        /// What the file declares.
        found: String,
        /// What the engine requires.
        expected: String,
    },

    /// The window carried too little audio for one analysis frame.
    #[error("the window carries {samples} sample(s), too few to embed")]
    WindowTooShort {
        /// Samples the window carried.
        samples: usize,
    },

    /// Media could not be read or decoded.
    #[error("the media could not be decoded")]
    Undecodable,
}

/// Every ONNX Runtime failure is a runtime failure here.
///
/// Generic over `ort`'s recovery parameter: a builder error carries the
/// builder back so the caller may retry, which is not a distinction this
/// engine makes — a session that would not build is a model that is not
/// available.
impl<R> From<ort::Error<R>> for EngineError {
    fn from(error: ort::Error<R>) -> Self {
        Self::Runtime(error.to_string())
    }
}

impl From<EngineError> for EmbedError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Artifact(_) | EngineError::UnexpectedGraph { .. } => Self::Unavailable,
            EngineError::Undecodable => Self::Undecodable,
            EngineError::Features(_) | EngineError::WindowTooShort { .. } => {
                Self::Malformed("the window produced no usable features".to_owned())
            }
            EngineError::Runtime(_) => Self::EngineFailure,
        }
    }
}

/// How the session is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// Threads ONNX Runtime may use inside one operator.
    ///
    /// One by default, like sherpa-onnx: a desktop application embedding a
    /// few seconds of audio per cluster gains little from a thread pool and
    /// an interactive machine notices the cores it loses.
    pub intra_op_threads: usize,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            intra_op_threads: 1,
        }
    }
}

/// Pin the session to the CPU execution provider.
///
/// `ort` 2.0.0-rc.14 sets ONNX Runtime's `MAX_EFFICIENCY` device policy on
/// every session it builds, which on this project's two reference platforms
/// means CoreML on macOS and DirectML on Windows. Three measured facts made
/// that unsuitable here, all taken on 2026-10-12 on an Apple M5 (macOS 27.0.1,
/// release build, ONNX Runtime 1.31.0, this checkpoint, one 3 s window):
///
/// 1. **CoreML cannot run in the sandbox.** Its model builder compiles the
///    graph through Apple's compiler, which needs a working directory inside
///    the *user's* temporary directory. Under the helper's `(deny default)`
///    profile that write is denied and the session never opens; granting it
///    would mean handing an inference helper write access to every other
///    process's scratch area, which is a worse trade than the one below.
/// 2. **The cost is small.** CPU: 75 ms start-up, 54.8 ms per window
///    (RTF 0.018), 92 MB resident. CoreML: 442 ms start-up cold, 16.3 ms per
///    window (RTF 0.0054), 92 MB resident. For a recording of a few windows
///    the totals are within milliseconds of each other, and both are far
///    inside the §8 budget.
/// 3. **The vectors are the same vectors.** Cosine similarity between the two
///    providers' output for the same input is **0.999974** (largest component
///    difference 1.2e-3), so the measured operating point and the committed
///    EER transfer rather than needing to be re-derived.
///
/// The fourth reason is not a measurement but a requirement: a biometric
/// threshold is a property of a checkpoint (VP-051), and a build that silently
/// ran a different numerical engine on each platform would make "the same
/// threshold" a claim nobody checked.
fn cpu_only(
    builder: ort::session::builder::SessionBuilder,
) -> Result<ort::session::builder::SessionBuilder, EngineError> {
    Ok(builder
        .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)?
        .with_execution_providers([ort::ep::CPU::default().build()])?)
}

/// A loaded, digest-verified speaker-embedding checkpoint.
pub struct SpeakerEmbeddingModel {
    artifact: &'static ModelArtifact,
    binding: ModelBinding,
    dim: usize,
    fbank: FbankComputer,
    session: Mutex<Session>,
    startup: std::time::Duration,
}

impl fmt::Debug for SpeakerEmbeddingModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpeakerEmbeddingModel")
            .field("model", &self.binding.to_string())
            .field("dim", &self.dim)
            .field("window", &self.fbank.options().window.as_str())
            .finish_non_exhaustive()
    }
}

impl SpeakerEmbeddingModel {
    /// Load a checkpoint from disk.
    ///
    /// The digest is checked **before** the session is created: a model file
    /// is parsed by native code, and parsing an unverified one is the thing
    /// ADR-0015 exists to prevent.
    ///
    /// # Errors
    ///
    /// [`EngineError::Artifact`] when the file is not the reviewed artifact,
    /// [`EngineError::UnexpectedGraph`] when the graph is not the one this
    /// engine drives, and [`EngineError::Runtime`] when ONNX Runtime refuses.
    pub fn open(
        path: &Path,
        artifact: &'static ModelArtifact,
        options: EngineOptions,
    ) -> Result<Self, EngineError> {
        let started = std::time::Instant::now();
        verify_artifact(artifact, path)?;

        let environment = ort::environment::init()
            .build()
            .map_err(|error| EngineError::Runtime(error.to_string()))?;
        let builder = Session::builder(&environment)?
            .with_intra_threads(options.intra_op_threads)?
            .with_inter_threads(1)?;
        let session = cpu_only(builder)?.commit_from_file(path)?;

        let inputs: Vec<&str> = session
            .inputs()
            .iter()
            .map(ort::value::Outlet::name)
            .collect();
        let outputs: Vec<&str> = session
            .outputs()
            .iter()
            .map(ort::value::Outlet::name)
            .collect();
        if inputs != [FEATURE_INPUT] || outputs != [EMBEDDING_OUTPUT] {
            return Err(EngineError::UnexpectedGraph {
                found: format!("inputs {inputs:?}, outputs {outputs:?}"),
                expected: format!("inputs [\"{FEATURE_INPUT}\"], outputs [\"{EMBEDDING_OUTPUT}\"]"),
            });
        }

        let digest: Sha256Digest = artifact.sha256_hex.parse().map_err(
            |error: plaudon_domain::media::Sha256ParseError| {
                EngineError::Runtime(error.to_string())
            },
        )?;
        let binding = ModelBinding::new(artifact.model_id, artifact.version, digest);
        let fbank = FbankComputer::new(FbankOptions::wespeaker())?;

        Ok(Self {
            artifact,
            binding,
            dim: crate::checkpoint::WESPEAKER_EMBEDDING_DIM,
            fbank,
            session: Mutex::new(session),
            startup: started.elapsed(),
        })
    }

    /// The artifact record this model was verified against.
    #[must_use]
    pub const fn artifact(&self) -> &'static ModelArtifact {
        self.artifact
    }

    /// Model id, version and digest, as the matching core binds them.
    #[must_use]
    pub const fn binding(&self) -> &ModelBinding {
        &self.binding
    }

    /// Dimensionality of every vector this checkpoint produces.
    #[must_use]
    pub const fn dim(&self) -> usize {
        self.dim
    }

    /// Wall time spent verifying the digest and creating the session.
    ///
    /// Reported as `startup_ms` in the ADR-0081 §4 artifact, measured here
    /// rather than in a script so the number describes the work the product
    /// actually does on first use.
    #[must_use]
    pub const fn startup(&self) -> std::time::Duration {
        self.startup
    }

    /// Embed one window of mono 16 kHz samples in `[-1, 1)`.
    ///
    /// The returned vector is the checkpoint's raw output; L2 normalisation
    /// belongs to [`Embedding`], which is where the polarity of a cosine score
    /// is enforced.
    ///
    /// # Errors
    ///
    /// [`EngineError::WindowTooShort`] for a window below one analysis frame,
    /// [`EngineError::Runtime`] when the session fails.
    pub fn embed_samples(&self, samples: &[f32]) -> Result<Vec<f32>, EngineError> {
        let mut features = match self.fbank.compute(samples) {
            Ok(features) => features,
            Err(FbankError::TooShort { samples, .. }) => {
                return Err(EngineError::WindowTooShort { samples })
            }
            Err(other) => return Err(EngineError::Features(other)),
        };
        features.subtract_mean();

        let shape = [1_i64, features.frames() as i64, features.bins() as i64];
        let tensor = Tensor::from_array((shape, features.values().to_vec()))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| EngineError::Runtime("the inference session is poisoned".to_owned()))?;
        let outputs = session.run(ort::inputs![tensor])?;
        let (shape, data) = outputs[EMBEDDING_OUTPUT].try_extract_tensor::<f32>()?;

        if shape.len() != 2 || shape[1] as usize != self.dim {
            return Err(EngineError::UnexpectedGraph {
                found: format!("an output of shape {shape:?}"),
                expected: format!("[1, {}]", self.dim),
            });
        }
        Ok(data[..self.dim].to_vec())
    }
}

/// The operating point a checkpoint is scored under (VP-051, VP-052).
///
/// Separate from the model because it is *measured*, not exported: the
/// thresholds come from a DET curve and the cohort from non-roster speakers,
/// both produced by `plaudon-eval speaker-match` and shipped in the model
/// pack beside the weights. A model without one cannot match, which is why
/// [`OnnxSpeakerEmbedder::new`] takes it rather than defaulting it.
#[derive(Debug, Clone)]
pub struct OperatingPoint {
    /// Thresholds derived from the harness, at the published false-accept rate.
    pub thresholds: ThresholdSet,
    /// Version of that operating point; it keys the match job (VP-101).
    pub version: String,
    /// Impostor cohort AS-Norm normalises against.
    pub cohort: CohortMatrix,
    /// How many cohort rows AS-Norm averages.
    pub top_k: TopK,
}

/// A decoded recording, held as canonical 16 kHz mono PCM on disk.
///
/// On disk rather than in memory on purpose: an hour of 16 kHz mono `f32` is
/// 230 MB, and §8.1 forbids making peak memory a function of total duration.
/// Windows are read by seeking, so embedding the tenth cluster of a long
/// recording costs one decode, not ten.
#[derive(Debug)]
struct DecodedMedia {
    source: PathBuf,
    pcm: PathBuf,
    frames: u64,
}

impl Drop for DecodedMedia {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pcm);
    }
}

/// The [`SpeakerEmbedder`] implemented over a sandboxed checkpoint.
///
/// Media decoding, cancellation and the operating point live here; the model
/// and ONNX Runtime live in the helper process the [`SandboxedModel`] owns.
pub struct OnnxSpeakerEmbedder {
    model: Arc<SandboxedModel>,
    checkpoint: Arc<EmbedderCheckpoint>,
    decoded: Mutex<Option<DecodedMedia>>,
}

impl fmt::Debug for OnnxSpeakerEmbedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnnxSpeakerEmbedder")
            .field("model", &self.model)
            .field(
                "threshold_set_version",
                &self.checkpoint.threshold_set_version,
            )
            .finish_non_exhaustive()
    }
}

impl OnnxSpeakerEmbedder {
    /// Bind a sandboxed model to its measured operating point.
    ///
    /// The canonical PCM derivative of whichever recording is being embedded
    /// is written into the model's granted scratch directory — the same
    /// directory the helper was given read access to, because a grant cannot
    /// be widened once the process is running — and is removed when the
    /// embedder is dropped.
    #[must_use]
    pub fn new(model: Arc<SandboxedModel>, operating_point: OperatingPoint) -> Self {
        let checkpoint = Arc::new(EmbedderCheckpoint {
            binding: model.binding().clone(),
            dim: model.dim(),
            thresholds: operating_point.thresholds,
            threshold_set_version: operating_point.version,
            cohort: operating_point.cohort,
            top_k: operating_point.top_k,
        });
        Self {
            model,
            checkpoint,
            decoded: Mutex::new(None),
        }
    }

    /// Decode `path` to canonical 16 kHz mono PCM once, and keep it.
    fn ensure_decoded(&self, path: &Path, ctx: &JobContext) -> Result<(), EngineError> {
        let mut cache = self
            .decoded
            .lock()
            .map_err(|_| EngineError::Runtime("the decode cache is poisoned".to_owned()))?;
        if cache.as_ref().is_some_and(|held| held.source == path) {
            return Ok(());
        }
        // Drop the previous derivative before writing a new one: two scratch
        // files for two recordings would double the disk cost for no gain.
        *cache = None;

        let media = validate_media(path, &ValidationRequest::manual_import())
            .map_err(|_| EngineError::Undecodable)?;
        let scratch_dir = self.model.scratch_dir();
        std::fs::create_dir_all(scratch_dir).map_err(|_| EngineError::Undecodable)?;
        let stem = format!("speaker-{}", media.sha256().to_hex());
        let mut scratch =
            ScratchPcm::create(scratch_dir, &stem).map_err(|_| EngineError::Undecodable)?;
        let profile = CanonicalPcmProfile {
            sample_rate_hz: MODEL_SAMPLE_RATE_HZ,
            channel_policy: plaudon_audio::decode::ChannelPolicy::DownmixToMono,
            sample_format: PcmSampleFormat::F32Le,
        };
        let outcome =
            decode_to_canonical_pcm(&media, &profile, &mut scratch as &mut dyn PcmSink, ctx)
                .map_err(|_| EngineError::Undecodable)?;
        let pcm = scratch.commit().map_err(|_| EngineError::Undecodable)?;
        *cache = Some(DecodedMedia {
            source: path.to_path_buf(),
            pcm,
            frames: outcome.frames,
        });
        Ok(())
    }

    /// Resolve a window to a byte range of the decoded derivative.
    ///
    /// The samples themselves are never read here: the helper reads them from
    /// the file it was granted, which is what keeps a window of attacker-
    /// chosen audio out of this process's inference path.
    fn window_range(&self, window: &EmbedWindow) -> Result<(PathBuf, u64, u64), EngineError> {
        let cache = self
            .decoded
            .lock()
            .map_err(|_| EngineError::Runtime("the decode cache is poisoned".to_owned()))?;
        let decoded = cache.as_ref().ok_or(EngineError::Undecodable)?;

        let rate = u64::from(MODEL_SAMPLE_RATE_HZ);
        let start_frame = window.start_ms.saturating_mul(rate) / 1_000;
        let end_frame = (window.end_ms.saturating_mul(rate) / 1_000).min(decoded.frames);
        if end_frame <= start_frame {
            return Err(EngineError::WindowTooShort { samples: 0 });
        }
        Ok((decoded.pcm.clone(), start_frame, end_frame - start_frame))
    }
}

#[async_trait]
impl SpeakerEmbedder for OnnxSpeakerEmbedder {
    fn checkpoint(&self) -> Arc<EmbedderCheckpoint> {
        Arc::clone(&self.checkpoint)
    }

    async fn embed_window(
        &self,
        window: &EmbedWindow,
        ctx: &JobContext,
    ) -> Result<Embedding, EmbedError> {
        // The window boundary is the cancellation point (VP-103): a user who
        // pressed Cancel waits for one window, never for a recording.
        if ctx.is_cancelled() {
            return Err(EmbedError::Cancelled);
        }
        self.ensure_decoded(&window.media_path, ctx)?;
        if ctx.is_cancelled() {
            return Err(EmbedError::Cancelled);
        }
        let (pcm, start_frame, frames) = self.window_range(window)?;
        let values: Vec<f32> = self
            .model
            .embed_window(&pcm, start_frame, frames)
            .map_err(EmbedError::from)?;
        Ok(Embedding::l2_normalise(
            self.checkpoint.binding.clone(),
            &values,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::WESPEAKER_RESNET34_LM;

    #[test]
    fn opening_a_file_that_is_not_the_reviewed_checkpoint_fails_before_any_parsing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("wespeaker_en_voxceleb_resnet34_LM.onnx");
        std::fs::write(&path, b"ONNX-shaped nothing").expect("write");
        let error =
            SpeakerEmbeddingModel::open(&path, &WESPEAKER_RESNET34_LM, EngineOptions::default())
                .expect_err("refused");
        assert!(matches!(error, EngineError::Artifact(_)));
    }

    #[test]
    fn an_artifact_failure_reaches_the_pipeline_as_unavailable_not_as_a_crash() {
        let error: EmbedError = EngineError::Artifact(ArtifactError::Size {
            artifact: "wespeaker-resnet34-lm",
            expected: 1,
            found: 2,
        })
        .into();
        assert_eq!(error, EmbedError::Unavailable);
    }

    #[test]
    fn no_error_variant_can_carry_a_vector() {
        // A compile-time-ish guard expressed as a value test: every variant is
        // built from strings and counts, so a component cannot be smuggled
        // into a log line through an error (VP-002, VP-112).
        let rendered = EngineError::WindowTooShort { samples: 12 }.to_string();
        assert_eq!(
            rendered,
            "the window carries 12 sample(s), too few to embed"
        );
    }
}
