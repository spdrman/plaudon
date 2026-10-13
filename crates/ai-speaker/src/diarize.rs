//! A real [`DiarizationProvider`] over the two bundle-tier checkpoints
//! (ADR-0400 condition 4).
//!
//! The pipeline is the one pyannote's `SpeakerDiarization` describes, built
//! from the pieces this workspace already owns:
//!
//! 1. **Sliding-window segmentation.** [`SegmentationModel`] decides, every
//!    `window_step_frames` frames, which of up to three local speakers is
//!    active in each frame of a ten-second window. Consecutive windows overlap
//!    heavily on purpose: the aggregation step turns that redundancy into a
//!    vote rather than a union.
//! 2. **Per-track embedding.** Each local speaker of each window is embedded
//!    from *its own* speech — the frames the segmentation model gave it, and
//!    nothing else — by the WeSpeaker checkpoint. A track below
//!    [`DiarizationOptions::min_embed_ms`] is dropped, because a vector from a
//!    tenth of a second is not a voice but noise with a direction; a track
//!    below [`DiarizationOptions::min_track_ms`] is embedded and assigned, but
//!    is not allowed to shape where the cluster boundaries fall.
//! 3. **Constrained clustering.** Those vectors are clustered into
//!    session-global speakers by [`plaudon_ai_diarization::pipeline::cluster_tracks`],
//!    which may never merge two tracks of one window.
//! 4. **Aggregation.** The per-window decisions are averaged over the windows
//!    that covered each frame and binarised into turns.
//!
//! **This provider is not validated and is not wired into any composition
//! root.** It is deliberately absent from
//! [`plaudon_ai_diarization::provider::VALIDATED_PROVIDERS`]: ADR-0400 admits a
//! provider only once all five of its conditions hold, and this code exists so
//! that the fourth — DER on labelled audio including overlapping speech — is a
//! measurement rather than an open item. Running it through
//! [`plaudon_ai_diarization::provider::run_guarded`] therefore fails closed,
//! which is the behaviour a test in this module pins.
//!
//! **No vector is ever logged, returned or persisted.** Embeddings exist for
//! the length of one clustering call (VP-002, VP-112).

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{AvailabilityReason, HardwareRequirements, PrivacyClass};
use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_ai_diarization::contract::{
    DiarizationCapabilities, DiarizationError, DiarizationInput, DiarizationProvider,
    DiarizationResult, SpeakerTurn, DIARIZATION_CONTRACT_VERSION,
};
use plaudon_ai_diarization::pipeline::{
    assemble_turns, cluster_tracks, hamming_weight, AssembleOptions, ClusterOptions, LocalTrack,
    PipelineError, LOCAL_SPEAKERS,
};
use plaudon_audio::decode::{
    decode_to_canonical_pcm, CanonicalPcmProfile, ChannelPolicy, PcmSampleFormat, PcmSink,
    ScratchPcm,
};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::money::{Currency, Money, MoneyEstimate};

use crate::checkpoint::{BUNDLE_TIER, MODEL_SAMPLE_RATE_HZ};
use crate::onnx::{EngineError, SpeakerEmbeddingModel};
use crate::segmentation::{SegmentationModel, FRAME_STEP_SAMPLES, WINDOW_SAMPLES};

/// Stable identifier of this provider.
///
/// It names both checkpoints because the pipeline is both of them: the
/// segmentation model decides *when* somebody speaks and the embedding model
/// decides *who*, and a DER figure belongs to the pair.
pub const PROVIDER_ID: &str = "onnx-pyannote-wespeaker";

/// Bytes of PCM read per seek into the decoded derivative.
const PCM_CHUNK_BYTES: usize = 64 * 1024;

/// How the pipeline is run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiarizationOptions {
    /// Frames between consecutive window starts.
    ///
    /// 59 frames is 15 930 samples, 0.996 s: a tenth of the window, which is
    /// the step pyannote's own pipeline uses. A multiple of the frame step is
    /// not optional — it is what makes every window's frames land exactly on
    /// the session grid instead of being resampled onto it.
    pub window_step_frames: usize,
    /// Speech a track needs before it may shape the clustering.
    ///
    /// A shorter track is still embedded and still assigned to a speaker; it
    /// just does not get to decide where a cluster boundary lies.
    pub min_track_ms: u64,
    /// Speech a track needs before it is embedded at all.
    ///
    /// Below this the vector describes the room, not a voice, and the track's
    /// frames are dropped. The floor is low on purpose: short tracks live at
    /// the edges of turns, and dropping them there erodes the boundaries the
    /// measurement is about.
    pub min_embed_ms: u64,
    /// How tracks become session-global speakers.
    pub cluster: ClusterOptions,
    /// How the windows become turns.
    pub assemble: AssembleOptions,
}

impl Default for DiarizationOptions {
    fn default() -> Self {
        Self {
            window_step_frames: 59,
            min_track_ms: 1_000,
            min_embed_ms: 250,
            cluster: ClusterOptions::default(),
            assemble: AssembleOptions::default(),
        }
    }
}

impl DiarizationOptions {
    /// Samples between consecutive window starts.
    #[must_use]
    pub const fn step_samples(&self) -> usize {
        self.window_step_frames * FRAME_STEP_SAMPLES as usize
    }

    /// Longest recording the clustering limit admits, in milliseconds.
    ///
    /// Reported as a capability rather than discovered at the end of an hour
    /// of inference: the quadratic distance matrix is the binding constraint,
    /// and a user deserves to be told before the work starts.
    #[must_use]
    pub const fn max_duration_ms(&self) -> u64 {
        let windows = (self.cluster.max_tracks / LOCAL_SPEAKERS) as u64;
        let step_ms = self.step_samples() as u64 * 1_000 / MODEL_SAMPLE_RATE_HZ as u64;
        windows * step_ms
    }
}

/// Why a run failed.
#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// A checkpoint or its runtime failed.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// The runtime-free half of the pipeline refused the input.
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
    /// The caller cancelled the run.
    #[error("diarization was cancelled")]
    Cancelled,
}

impl From<DiarizeError> for DiarizationError {
    fn from(error: DiarizeError) -> Self {
        match error {
            DiarizeError::Cancelled => Self::Cancelled,
            DiarizeError::Engine(
                EngineError::Artifact(_) | EngineError::UnexpectedGraph { .. },
            ) => Self::Unavailable,
            DiarizeError::Pipeline(PipelineError::TooManyTracks { .. }) => Self::DurationExceeded,
            DiarizeError::Engine(_) | DiarizeError::Pipeline(_) => Self::EngineFailure,
        }
    }
}

/// Canonical 16 kHz mono samples, addressed by sample range.
///
/// A trait rather than a `&[f32]` so the product path can stream a recording
/// off disk — §8.1 forbids making peak memory a function of total duration —
/// while the measurement bridge, which already holds its synthetic mix in
/// memory, does not pay for a round trip through the file system.
pub trait SampleSource {
    /// Total samples available.
    fn total_samples(&self) -> u64;

    /// Read `len` samples starting at `start`, or fewer at the end.
    ///
    /// # Errors
    ///
    /// [`EngineError::Undecodable`] when the samples cannot be read.
    fn read(&self, start: u64, len: usize) -> Result<Vec<f32>, EngineError>;
}

/// Samples already in memory.
#[derive(Debug, Clone, Copy)]
pub struct MemorySamples<'a>(pub &'a [f32]);

impl SampleSource for MemorySamples<'_> {
    fn total_samples(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&self, start: u64, len: usize) -> Result<Vec<f32>, EngineError> {
        let start = usize::try_from(start)
            .unwrap_or(usize::MAX)
            .min(self.0.len());
        let end = start.saturating_add(len).min(self.0.len());
        Ok(self.0[start..end].to_vec())
    }
}

/// A canonical PCM derivative on disk, read by seeking.
#[derive(Debug)]
struct PcmFile {
    path: PathBuf,
    frames: u64,
}

impl Drop for PcmFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl SampleSource for PcmFile {
    fn total_samples(&self) -> u64 {
        self.frames
    }

    fn read(&self, start: u64, len: usize) -> Result<Vec<f32>, EngineError> {
        let start = start.min(self.frames);
        let wanted = usize::try_from(self.frames - start)
            .unwrap_or(usize::MAX)
            .min(len);
        let mut file = File::open(&self.path).map_err(|_| EngineError::Undecodable)?;
        file.seek(SeekFrom::Start(start * 4))
            .map_err(|_| EngineError::Undecodable)?;

        let mut samples = Vec::with_capacity(wanted);
        let mut buffer = vec![0u8; PCM_CHUNK_BYTES];
        while samples.len() < wanted {
            let remaining = (wanted - samples.len()) * 4;
            let slice = &mut buffer[..remaining.min(PCM_CHUNK_BYTES)];
            let read = file.read(slice).map_err(|_| EngineError::Undecodable)?;
            if read == 0 {
                break;
            }
            for chunk in slice[..read].as_chunks::<4>().0 {
                samples.push(f32::from_le_bytes(*chunk));
            }
        }
        Ok(samples)
    }
}

/// Where one local track ended up.
///
/// The provider's own decisions, in the form that makes a run inspectable:
/// which window saw which slot, over which seconds, and which session-global
/// cluster it joined. Carries no vector (VP-002, VP-112).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackAssignment {
    /// Window the track was decoded from.
    pub window: usize,
    /// Local speaker slot within that window.
    pub local_speaker: usize,
    /// First active frame, in milliseconds.
    pub start_ms: u64,
    /// Last active frame, in milliseconds.
    pub end_ms: u64,
    /// Active speech the track carried, in milliseconds.
    pub speech_ms: u64,
    /// Whether the track was allowed to shape the clustering.
    pub fitted: bool,
    /// Cluster the track joined.
    pub cluster: usize,
}

/// What one run produced, besides its turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStats {
    /// Windows the segmentation model ran on.
    pub windows: usize,
    /// Local speaker tracks that carried a vector.
    pub tracks: usize,
    /// Tracks dropped for carrying too little speech to embed at all.
    pub tracks_dropped: usize,
    /// Tracks embedded and assigned, but not allowed to shape the clustering.
    pub tracks_assigned: usize,
    /// Session-global clusters the tracks were grouped into.
    pub clusters: usize,
    /// Every track and the cluster it joined.
    pub assignments: Vec<TrackAssignment>,
}

/// The [`DiarizationProvider`] implemented over the two real checkpoints.
pub struct OnnxDiarizationProvider {
    segmentation: Arc<SegmentationModel>,
    embedder: Arc<SpeakerEmbeddingModel>,
    options: DiarizationOptions,
    scratch_dir: PathBuf,
}

impl fmt::Debug for OnnxDiarizationProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnnxDiarizationProvider")
            .field("provider_id", &PROVIDER_ID)
            .field("segmentation", &self.segmentation)
            .field("embedder", &self.embedder)
            .finish_non_exhaustive()
    }
}

impl OnnxDiarizationProvider {
    /// Bind the two loaded checkpoints into one provider.
    ///
    /// `scratch_dir` holds the canonical PCM derivative of whichever recording
    /// is being diarized; it is removed when the run ends.
    #[must_use]
    pub const fn new(
        segmentation: Arc<SegmentationModel>,
        embedder: Arc<SpeakerEmbeddingModel>,
        options: DiarizationOptions,
        scratch_dir: PathBuf,
    ) -> Self {
        Self {
            segmentation,
            embedder,
            options,
            scratch_dir,
        }
    }

    /// Options this provider runs with.
    #[must_use]
    pub const fn options(&self) -> &DiarizationOptions {
        &self.options
    }

    /// The version string a result and an artifact carry.
    ///
    /// Both checkpoints, by id and version: a DER measured with one
    /// segmentation model and a different embedding model is a different
    /// measurement, and the document has to be able to say so.
    #[must_use]
    pub fn provider_version(&self) -> String {
        format!(
            "{}+{}",
            self.segmentation.artifact().version,
            self.embedder.artifact().version
        )
    }

    /// Run the pipeline over a sample source.
    ///
    /// # Errors
    ///
    /// [`DiarizeError::Cancelled`] at a window boundary, and the engine or
    /// pipeline failure otherwise.
    pub fn diarize_samples(
        &self,
        source: &dyn SampleSource,
        speaker_count_hint: Option<u8>,
        ctx: &JobContext,
    ) -> Result<(Vec<SpeakerTurn>, RunStats), DiarizeError> {
        let grid = SegmentationModel::grid();
        let total = source.total_samples();
        let step = self.options.step_samples() as u64;
        let step_frames = self.options.window_step_frames as u64;
        let samples_per_ms = u64::from(MODEL_SAMPLE_RATE_HZ) / 1_000;
        let min_track_samples = (self.options.min_track_ms * samples_per_ms) as usize;
        let min_embed_samples = (self.options.min_embed_ms * samples_per_ms) as usize;

        let frames_total = grid.frames_for(total);
        let mut coverage = vec![0.0f64; usize::try_from(frames_total).unwrap_or(usize::MAX)];
        let mut tracks: Vec<LocalTrack> = Vec::new();
        let mut vectors: Vec<Vec<f32>> = Vec::new();
        let mut groups: Vec<usize> = Vec::new();
        let mut fit: Vec<bool> = Vec::new();
        let mut below_minimum = 0usize;

        // One window every `step` samples, and one more whenever the tail is
        // not yet covered: the last window is zero-padded rather than skipped,
        // because speech in the final seconds is still speech.
        let windows = if total == 0 {
            0
        } else {
            (total.saturating_sub(1) / step + 1) as usize
        };
        for window in 0..windows {
            if ctx.is_cancelled() {
                return Err(DiarizeError::Cancelled);
            }
            let offset = window as u64 * step;
            let samples = source.read(offset, WINDOW_SAMPLES)?;
            if samples.is_empty() {
                break;
            }
            let activity = self.segmentation.window_activity(&samples)?;
            // Frames whose own slice of audio is past the end of the recording
            // carry nothing but the zero padding, and must not vote.
            let valid = activity
                .len()
                .min(samples.len().div_ceil(FRAME_STEP_SAMPLES as usize));
            let base = window as u64 * step_frames;
            for frame in 0..valid {
                let global = usize::try_from(base + frame as u64).unwrap_or(usize::MAX);
                if let Some(weight) = coverage.get_mut(global) {
                    *weight += hamming_weight(frame, activity.len());
                }
            }

            for local in 0..LOCAL_SPEAKERS {
                let active: Vec<u64> = (0..valid)
                    .filter(|frame| activity[*frame][local])
                    .map(|frame| base + frame as u64)
                    .collect();
                if active.is_empty() {
                    continue;
                }
                // Embed this speaker from the frames where they are the *only*
                // speaker, as `pyannote/speaker-diarization-3.1` does with
                // `embedding_exclude_overlap`. Two people talking at once sound
                // like a third person to an embedding model, and a cluster of
                // those mixtures is a speaker who was never in the room. Where
                // a track has no exclusive speech at all, the mixture is used
                // and the track is not allowed to shape the clustering.
                let gather = |frames: &[u64]| {
                    let mut speech: Vec<f32> = Vec::with_capacity(frames.len() * step as usize);
                    for frame in frames {
                        let start = (*frame - base) as usize * FRAME_STEP_SAMPLES as usize;
                        let end = (start + FRAME_STEP_SAMPLES as usize).min(samples.len());
                        if start < end {
                            speech.extend_from_slice(&samples[start..end]);
                        }
                    }
                    speech
                };
                let exclusive: Vec<u64> = active
                    .iter()
                    .copied()
                    .filter(|frame| {
                        activity[(*frame - base) as usize]
                            .iter()
                            .filter(|speaking| **speaking)
                            .count()
                            == 1
                    })
                    .collect();
                let exclusive_speech = gather(&exclusive);
                let fits = exclusive_speech.len() >= min_track_samples;
                let speech = if exclusive_speech.len() >= min_embed_samples {
                    exclusive_speech
                } else {
                    gather(&active)
                };
                if speech.len() < min_embed_samples {
                    below_minimum += 1;
                    continue;
                }
                if ctx.is_cancelled() {
                    return Err(DiarizeError::Cancelled);
                }
                vectors.push(self.embedder.embed_samples(&speech)?);
                groups.push(window);
                fit.push(fits);
                tracks.push(LocalTrack {
                    window,
                    local_speaker: local,
                    window_base: base,
                    window_frames: activity.len(),
                    frames: active,
                });
            }
            ctx.report(Progress::fraction(
                (window + 1) as f32 / windows.max(1) as f32,
            ));
        }

        let mut options = self.options.cluster;
        if let Some(hint) = speaker_count_hint {
            options.target_clusters = Some(usize::from(hint).max(1));
        }
        let labels = cluster_tracks(&vectors, &groups, &fit, options)?;
        let clusters = labels.iter().copied().max().map_or(0, |max| max + 1);
        let turns = assemble_turns(&tracks, &labels, &coverage, &grid, self.options.assemble);
        let assignments = tracks
            .iter()
            .zip(&labels)
            .zip(&fit)
            .map(|((track, cluster), fitted)| TrackAssignment {
                window: track.window,
                local_speaker: track.local_speaker,
                start_ms: track
                    .frames
                    .first()
                    .map_or(0, |frame| grid.start_ms(*frame)),
                end_ms: track.frames.last().map_or(0, |frame| grid.end_ms(*frame)),
                speech_ms: track.frames.len() as u64 * u64::from(FRAME_STEP_SAMPLES) * 1_000
                    / u64::from(MODEL_SAMPLE_RATE_HZ),
                fitted: *fitted,
                cluster: *cluster,
            })
            .collect();
        Ok((
            turns,
            RunStats {
                windows,
                tracks: tracks.len(),
                tracks_dropped: below_minimum,
                tracks_assigned: fit.iter().filter(|fitted| !**fitted).count(),
                clusters,
                assignments,
            },
        ))
    }

    /// Decode `path` to canonical 16 kHz mono PCM in the scratch directory.
    fn decode(&self, path: &Path, ctx: &JobContext) -> Result<PcmFile, EngineError> {
        let media = validate_media(path, &ValidationRequest::manual_import())
            .map_err(|_| EngineError::Undecodable)?;
        std::fs::create_dir_all(&self.scratch_dir).map_err(|_| EngineError::Undecodable)?;
        let stem = format!("diarize-{}", media.sha256().to_hex());
        let mut scratch =
            ScratchPcm::create(&self.scratch_dir, &stem).map_err(|_| EngineError::Undecodable)?;
        let profile = CanonicalPcmProfile {
            sample_rate_hz: MODEL_SAMPLE_RATE_HZ,
            channel_policy: ChannelPolicy::DownmixToMono,
            sample_format: PcmSampleFormat::F32Le,
        };
        let outcome =
            decode_to_canonical_pcm(&media, &profile, &mut scratch as &mut dyn PcmSink, ctx)
                .map_err(|_| EngineError::Undecodable)?;
        let path = scratch.commit().map_err(|_| EngineError::Undecodable)?;
        Ok(PcmFile {
            path,
            frames: outcome.frames,
        })
    }
}

#[async_trait]
impl DiarizationProvider for OnnxDiarizationProvider {
    async fn capabilities(&self) -> DiarizationCapabilities {
        DiarizationCapabilities {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            // Experimental, never Supported: ADR-0081 §4 and ADR-0400
            // conditions 1, 3 and 5 are open, so `is_selectable` must not be
            // able to become true through this payload alone.
            status: CapabilityStatus::Experimental,
            provider_id: PROVIDER_ID.to_owned(),
            offline: Tri::Yes,
            supports_overlapping_speech: Tri::Yes,
            can_cancel: Tri::Yes,
            max_duration_ms: Known::Known(self.options.max_duration_ms()),
            privacy_class: Known::Known(PrivacyClass::OnDevice),
            added_cost: MoneyEstimate::Finalized(Money::new(0, 2, Currency::USD)),
            model_download_bytes: Known::Known(
                BUNDLE_TIER
                    .iter()
                    .map(|artifact| artifact.byte_length)
                    .sum(),
            ),
            hardware_requirements: Some(HardwareRequirements {
                // Both sessions, their weights and one window of audio, with
                // the measured peak of the §4 run as the floor.
                min_ram_bytes: 512 * 1024 * 1024,
                requires_accelerator: false,
                accelerator_backends: Vec::new(),
            }),
            availability_reason: AvailabilityReason::Available,
        }
    }

    async fn diarize(
        &self,
        input: DiarizationInput,
        ctx: JobContext,
    ) -> Result<DiarizationResult, DiarizationError> {
        if ctx.is_cancelled() {
            return Err(DiarizationError::Cancelled);
        }
        if input
            .duration_ms
            .is_some_and(|duration| duration > self.options.max_duration_ms())
        {
            return Err(DiarizationError::DurationExceeded);
        }
        let pcm = self
            .decode(&input.media_path, &ctx)
            .map_err(DiarizeError::from)?;
        let (turns, _stats) = self.diarize_samples(&pcm, input.speaker_count_hint, &ctx)?;
        Ok(DiarizationResult {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            provider_id: PROVIDER_ID.to_owned(),
            provider_version: Some(self.provider_version()),
            turns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_provider_is_not_in_the_validated_allowlist() {
        // ADR-0400 condition 4 is what this module measures; conditions 1, 3
        // and 5 are open. A provider that measured well is still refused.
        assert!(!plaudon_ai_diarization::is_validated(PROVIDER_ID));
    }

    #[test]
    fn the_capability_payload_cannot_make_the_provider_selectable_on_its_own() {
        let capabilities = DiarizationCapabilities {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            status: CapabilityStatus::Experimental,
            provider_id: PROVIDER_ID.to_owned(),
            offline: Tri::Yes,
            supports_overlapping_speech: Tri::Yes,
            can_cancel: Tri::Yes,
            max_duration_ms: Known::Known(1),
            privacy_class: Known::Known(PrivacyClass::OnDevice),
            added_cost: MoneyEstimate::Finalized(Money::new(0, 2, Currency::USD)),
            model_download_bytes: Known::Known(0),
            hardware_requirements: None,
            availability_reason: AvailabilityReason::Available,
        };
        // The payload is selectable on its face; the allowlist is what refuses
        // the run, and it is the only gate the application calls.
        assert!(capabilities.is_selectable());
        assert!(!plaudon_ai_diarization::is_validated(
            &capabilities.provider_id
        ));
    }

    #[test]
    fn the_window_step_is_a_whole_number_of_frames_so_windows_share_one_grid() {
        let options = DiarizationOptions::default();
        assert_eq!(
            options.step_samples() % FRAME_STEP_SAMPLES as usize,
            0,
            "a step that is not a multiple of the frame shift would resample every window"
        );
        assert!(options.step_samples() < WINDOW_SAMPLES);
    }

    #[test]
    fn the_advertised_duration_limit_is_the_clustering_limit_in_time() {
        let options = DiarizationOptions::default();
        // 4096 tracks over three local speakers, one window every 0.996 s.
        assert_eq!(options.max_duration_ms(), 1_365 * 995);
    }

    #[test]
    fn memory_samples_clamp_a_read_past_the_end_instead_of_panicking() {
        let samples = [0.1f32, 0.2, 0.3];
        let source = MemorySamples(&samples);
        assert_eq!(source.total_samples(), 3);
        assert_eq!(source.read(1, 10).expect("read").len(), 2);
        assert_eq!(source.read(9, 10).expect("read"), Vec::<f32>::new());
    }
}
