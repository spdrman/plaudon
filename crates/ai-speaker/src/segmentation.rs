//! The real speaker-segmentation engine: pyannote `segmentation-3.0` over one
//! window (ADR-0400 condition 4, ADR-0081 §3).
//!
//! The checkpoint answers one question, for ten seconds of audio at a time:
//! which of up to three people is speaking in each of 589 frames, where
//! "both of them" is a class of its own rather than two independent
//! probabilities. That is the whole of what this module does — verify the
//! weights, run the graph, decode the powerset — and everything that turns
//! those windows into a session lives in
//! [`plaudon_ai_diarization::pipeline`], which needs no inference runtime and
//! is therefore tested in the default build.
//!
//! The constants below are not guesses: they are the values the checkpoint
//! publishes in its own ONNX `metadata_props` (`num_speakers=3`,
//! `powerset_max_classes=2`, `num_classes=7`, `sample_rate=16000`,
//! `window_size=160000`, `receptive_field_size=991`,
//! `receptive_field_shift=270`), and [`SegmentationModel::open`] refuses a file
//! whose graph does not match.
//!
//! **This engine is behind the off-by-default `onnx` feature and is not wired
//! into any composition root.** ADR-0400 admits a provider to
//! `VALIDATED_PROVIDERS` only once all five of its conditions hold; this module
//! and [`crate::diarize`] exist so that the fourth — a measured DER on
//! labelled audio including overlap — can stop being an open item.

use std::fmt;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use ort::session::Session;
use ort::value::Tensor;
use plaudon_ai_diarization::pipeline::{
    decode_powerset_frame, FrameGrid, LOCAL_SPEAKERS, POWERSET_CLASSES,
};

use crate::checkpoint::{
    verify_artifact, ModelArtifact, MODEL_SAMPLE_RATE_HZ, PYANNOTE_SEGMENTATION_3_0,
};
use crate::onnx::{cpu_only, environment, EngineError, EngineOptions};

/// ONNX graph input the segmentation checkpoint declares: `[N, 1, T]`.
const WAVEFORM_INPUT: &str = "x";

/// ONNX graph output the segmentation checkpoint declares: `[N, frames, 7]`.
const ACTIVITY_OUTPUT: &str = "y";

/// Samples one inference window carries (`window_size`), ten seconds at 16 kHz.
pub const WINDOW_SAMPLES: usize = 160_000;

/// Samples between consecutive frame centres (`receptive_field_shift`).
pub const FRAME_STEP_SAMPLES: u32 = 270;

/// Samples one frame sees (`receptive_field_size`).
pub const RECEPTIVE_FIELD_SAMPLES: u32 = 991;

/// Frames one window yields, which the graph computes as
/// `floor(floor(floor(floor(T/10 - 251/10)/3 - 2/3)/3)/3 - 8/3) + 1`.
pub const FRAMES_PER_WINDOW: usize = 589;

/// A loaded, digest-verified speaker-segmentation checkpoint.
pub struct SegmentationModel {
    artifact: &'static ModelArtifact,
    session: Mutex<Session>,
    startup: Duration,
}

impl fmt::Debug for SegmentationModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SegmentationModel")
            .field("model", &self.artifact.model_id)
            .field("version", &self.artifact.version)
            .field("local_speakers", &LOCAL_SPEAKERS)
            .finish_non_exhaustive()
    }
}

impl SegmentationModel {
    /// Load the segmentation checkpoint from disk.
    ///
    /// The digest is checked **before** the session is created, for the reason
    /// [`crate::onnx::SpeakerEmbeddingModel::open`] gives: a model file is
    /// parsed by native code, and parsing an unverified one is what ADR-0015
    /// exists to prevent.
    ///
    /// # Errors
    ///
    /// [`EngineError::Artifact`] when the file is not the reviewed artifact,
    /// [`EngineError::UnexpectedGraph`] when the graph is not the one this
    /// engine drives, and [`EngineError::Runtime`] when ONNX Runtime refuses.
    pub fn open(path: &Path, options: EngineOptions) -> Result<Self, EngineError> {
        let started = std::time::Instant::now();
        let artifact = &PYANNOTE_SEGMENTATION_3_0;
        verify_artifact(artifact, path)?;

        let builder = Session::builder(environment()?)?
            .with_intra_threads(options.intra_op_threads)?
            .with_inter_threads(1)?;
        // The same CPU-only policy the embedding engine runs under: a
        // measured operating point belongs to one numerical engine.
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
        if inputs != [WAVEFORM_INPUT] || outputs != [ACTIVITY_OUTPUT] {
            return Err(EngineError::UnexpectedGraph {
                found: format!("inputs {inputs:?}, outputs {outputs:?}"),
                expected: format!("inputs [\"{WAVEFORM_INPUT}\"], outputs [\"{ACTIVITY_OUTPUT}\"]"),
            });
        }

        Ok(Self {
            artifact,
            session: Mutex::new(session),
            startup: started.elapsed(),
        })
    }

    /// The artifact record this model was verified against.
    #[must_use]
    pub const fn artifact(&self) -> &'static ModelArtifact {
        self.artifact
    }

    /// Wall time spent verifying the digest and creating the session.
    #[must_use]
    pub const fn startup(&self) -> Duration {
        self.startup
    }

    /// How this checkpoint's frames map onto time.
    #[must_use]
    pub const fn grid() -> FrameGrid {
        FrameGrid {
            sample_rate_hz: MODEL_SAMPLE_RATE_HZ,
            step_samples: FRAME_STEP_SAMPLES,
            receptive_field_samples: RECEPTIVE_FIELD_SAMPLES,
        }
    }

    /// Decode one window of mono 16 kHz samples into per-frame activity.
    ///
    /// A short window — the tail of a recording — is zero-padded to the
    /// checkpoint's fixed window rather than run at a shorter length: the
    /// receptive field is 991 samples wide and the frame grid is defined
    /// against the full window, so a ragged final inference would silently
    /// shift every frame it produced.
    ///
    /// # Errors
    ///
    /// [`EngineError::WindowTooShort`] for an empty window,
    /// [`EngineError::UnexpectedGraph`] when the output is not
    /// `[1, FRAMES_PER_WINDOW, POWERSET_CLASSES]`, and
    /// [`EngineError::Runtime`] when the session fails.
    pub fn window_activity(
        &self,
        samples: &[f32],
    ) -> Result<Vec<[bool; LOCAL_SPEAKERS]>, EngineError> {
        if samples.is_empty() {
            return Err(EngineError::WindowTooShort { samples: 0 });
        }
        let mut window = vec![0.0f32; WINDOW_SAMPLES];
        let taken = samples.len().min(WINDOW_SAMPLES);
        window[..taken].copy_from_slice(&samples[..taken]);

        let shape = [1_i64, 1, WINDOW_SAMPLES as i64];
        let tensor = Tensor::from_array((shape, window))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| EngineError::Runtime("the inference session is poisoned".to_owned()))?;
        let outputs = session.run(ort::inputs![tensor])?;
        let (shape, data) = outputs[ACTIVITY_OUTPUT].try_extract_tensor::<f32>()?;

        if shape.len() != 3
            || shape[0] != 1
            || shape[1] as usize != FRAMES_PER_WINDOW
            || shape[2] as usize != POWERSET_CLASSES
        {
            return Err(EngineError::UnexpectedGraph {
                found: format!("an output of shape {shape:?}"),
                expected: format!("[1, {FRAMES_PER_WINDOW}, {POWERSET_CLASSES}]"),
            });
        }

        Ok(data
            .as_chunks::<POWERSET_CLASSES>()
            .0
            .iter()
            .take(FRAMES_PER_WINDOW)
            .map(decode_powerset_frame)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_grid_covers_the_window_the_checkpoint_declares() {
        let grid = SegmentationModel::grid();
        // The last frame's centre must still be inside the window, and one
        // more frame would not be: that is what makes 589 the right count.
        assert!(grid.centre_samples(FRAMES_PER_WINDOW as u64 - 1) < WINDOW_SAMPLES as u64);
        assert!(
            grid.centre_samples(FRAMES_PER_WINDOW as u64) + u64::from(RECEPTIVE_FIELD_SAMPLES) / 2
                > WINDOW_SAMPLES as u64
        );
    }

    #[test]
    fn opening_a_file_that_is_not_the_reviewed_checkpoint_fails_before_any_parsing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("pyannote-segmentation-3.0.onnx");
        std::fs::write(&path, b"ONNX-shaped nothing").expect("write");
        let error = SegmentationModel::open(&path, EngineOptions::default()).expect_err("refused");
        assert!(matches!(error, EngineError::Artifact(_)));
    }
}
