//! The whisper.cpp `SpeechToText` adapter (spec §6.5, §8.4, §8.5 — F2.1/F2.5).
//!
//! whisper.cpp is the cross-platform local engine and the validated Windows
//! default. The contract this adapter has to honour is narrow and strict:
//!
//! * It reports what it **observes**, not what the model name suggests
//!   (§8.3). Language-detection support comes from the model header, segment
//!   support from the run, and availability from the file actually being
//!   there.
//! * It never falls back to a different engine. A missing model is
//!   [`SttError::ModelNotInstalled`], full stop (§8.6A).
//! * Cancellation is cooperative and prompt, and a cancelled run yields no
//!   transcript at all.
//! * Everything that changes the output — model, backend, threads, beam size,
//!   temperature — is recorded on the draft, so a rerun is reproducible
//!   (§8.5).
//!
//! Compute runs on a blocking worker, never on the async executor, which is
//! what keeps the UI responsive during a one-hour job (§17.5).

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, HardwareRequirements, MediaFormat, PricingBasis, PrivacyClass,
    RequestFamily, SttCapabilities,
};
use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_ai_contracts::stt::{
    SpeechToText, SttError, SttInput, TranscriptDraft, TranscriptSegmentDraft,
};
use plaudon_ai_contracts::transcript::{
    AnnotationKind, DraftWordAlignment, SourceTime, TimePrecision, TranscriptAnnotation,
};
use plaudon_ai_contracts::{
    detect_hallucinations, validate_draft, AlignmentOrigin, HallucinationPolicy, Known,
    SpeechActivity, Tri,
};
use plaudon_audio::decode::{CanonicalPcmProfile, MemoryPcmSink};
use plaudon_audio::silence::{measure_speech_activity, SilenceOptions};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::transcript::{Confidence, ConfidenceScale};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::arbiter::{LocalWorkloadArbiter, WorkloadLease};
use crate::backend::{
    fall_back_to_cpu, select_backend, Backend, BackendPreference, BackendProbe, BackendSelection,
    RuntimeBackendProbe,
};
use crate::model::{read_model_header, ModelProbeError, WhisperModelHeader};
use crate::preflight::{preflight, CapacityProbe, HostCapacityProbe, WorkloadEstimate};

/// Stable engine identifier used across the workspace.
pub const WHISPER_ENGINE_ID: &str = "whisper.cpp";

/// Sample rate whisper.cpp requires.
pub const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;

/// Samples per whisper timestamp tick. whisper reports centiseconds.
const SAMPLES_PER_TICK: u64 = (WHISPER_SAMPLE_RATE_HZ as u64) / 100;

/// Longest single run this adapter accepts.
///
/// whisper.cpp holds the whole mel spectrogram and `f32` PCM in memory, so
/// duration is a real memory limit rather than an API restriction. Declaring
/// it is what makes the chunk planner split a long recording instead of the
/// run dying with an allocation failure (§8.2, §17.5).
pub const MAX_RUN_DURATION_MS: u64 = 30 * 60 * 1000;

/// Byte ceiling implied by [`MAX_RUN_DURATION_MS`] at the canonical profile.
pub const MAX_RUN_BYTES: u64 = (MAX_RUN_DURATION_MS / 1000) * (WHISPER_SAMPLE_RATE_HZ as u64) * 4;

/// Decoder parameters that affect output, recorded for repeatability (§8.5).
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperParams {
    /// Language code, or `None` to let the model detect (multilingual only).
    pub language: Option<String>,
    /// Translate to English instead of transcribing.
    pub translate: bool,
    /// Decode threads. Fixed rather than derived from the core count, because
    /// whisper.cpp's reductions are order-dependent and a varying thread count
    /// makes transcripts irreproducible across machines.
    pub threads: u32,
    /// Beam width. `1` means greedy.
    pub beam_size: u32,
    /// Sampling temperature. `0.0` is deterministic.
    pub temperature: f32,
    /// Ask for token-level timestamps, which become word alignment.
    pub token_timestamps: bool,
    /// whisper's own no-speech rejection threshold.
    pub no_speech_threshold: f32,
    /// whisper's own average-log-probability rejection threshold.
    pub logprob_threshold: f32,
    /// whisper's own entropy rejection threshold.
    pub entropy_threshold: f32,
}

impl Default for WhisperParams {
    fn default() -> Self {
        Self {
            language: None,
            translate: false,
            threads: 4,
            beam_size: 1,
            temperature: 0.0,
            token_timestamps: false,
            no_speech_threshold: 0.6,
            logprob_threshold: -1.0,
            entropy_threshold: 2.4,
        }
    }
}

impl WhisperParams {
    /// Parameter map recorded on the transcript revision.
    #[must_use]
    pub fn recorded(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        map.insert(
            "language".to_string(),
            self.language.clone().unwrap_or_else(|| "auto".to_string()),
        );
        map.insert("translate".to_string(), self.translate.to_string());
        map.insert("threads".to_string(), self.threads.to_string());
        map.insert("beam_size".to_string(), self.beam_size.to_string());
        map.insert(
            "temperature".to_string(),
            format!("{:.3}", self.temperature),
        );
        map.insert(
            "token_timestamps".to_string(),
            self.token_timestamps.to_string(),
        );
        map.insert(
            "no_speech_threshold".to_string(),
            format!("{:.3}", self.no_speech_threshold),
        );
        map.insert(
            "logprob_threshold".to_string(),
            format!("{:.3}", self.logprob_threshold),
        );
        map.insert(
            "entropy_threshold".to_string(),
            format!("{:.3}", self.entropy_threshold),
        );
        map
    }
}

/// Everything the adapter needs to run.
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperConfig {
    /// Installed weight file, resolved from the model registry.
    pub model_path: PathBuf,
    /// Model identifier recorded on the revision.
    pub model_id: String,
    /// Model version recorded on the revision.
    pub model_version: Option<String>,
    /// Decoder parameters.
    pub params: WhisperParams,
    /// Which compute backend to ask for.
    pub backend_preference: BackendPreference,
    /// Whether a GPU failure *during* a run may be retried on the CPU.
    ///
    /// Either behaviour satisfies §F2.5 as long as it is explicit; the retry
    /// is the better default because it completes the job, and it is always
    /// recorded in the revision metadata and the log.
    pub cpu_retry_after_gpu_failure: bool,
}

impl WhisperConfig {
    /// A configuration for an installed model with default parameters.
    #[must_use]
    pub fn new(model_path: PathBuf, model_id: impl Into<String>) -> Self {
        Self {
            model_path,
            model_id: model_id.into(),
            model_version: None,
            params: WhisperParams::default(),
            backend_preference: BackendPreference::default(),
            cpu_retry_after_gpu_failure: true,
        }
    }

    /// Force CPU execution. Used by hermetic tests and the GPU-parity fixture.
    #[must_use]
    pub fn forcing_cpu(mut self) -> Self {
        self.backend_preference = BackendPreference::ForceCpu;
        self
    }
}

struct LoadedContext {
    backend: Backend,
    context: WhisperContext,
}

/// whisper.cpp speech-to-text engine.
pub struct WhisperEngine {
    config: WhisperConfig,
    probe: Arc<dyn BackendProbe>,
    capacity: Arc<dyn CapacityProbe>,
    arbiter: LocalWorkloadArbiter,
    hallucination_policy: HallucinationPolicy,
    loaded: Arc<Mutex<Option<LoadedContext>>>,
}

impl std::fmt::Debug for WhisperEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperEngine")
            .field("model_id", &self.config.model_id)
            .field("backend_preference", &self.config.backend_preference)
            .finish_non_exhaustive()
    }
}

impl WhisperEngine {
    /// Build an engine over an installed model.
    ///
    /// Cheap: no weights are loaded until the first transcription.
    #[must_use]
    pub fn new(config: WhisperConfig) -> Self {
        Self {
            config,
            probe: Arc::new(RuntimeBackendProbe),
            capacity: Arc::new(HostCapacityProbe),
            arbiter: LocalWorkloadArbiter::single(),
            hallucination_policy: HallucinationPolicy::default(),
            loaded: Arc::new(Mutex::new(None)),
        }
    }

    /// Share a workload arbiter with other local engines (§14.4).
    #[must_use]
    pub fn with_arbiter(mut self, arbiter: LocalWorkloadArbiter) -> Self {
        self.arbiter = arbiter;
        self
    }

    /// Override the backend probe. Used by the forced-unavailable-GPU fixture.
    #[must_use]
    pub fn with_backend_probe(mut self, probe: Arc<dyn BackendProbe>) -> Self {
        self.probe = probe;
        self
    }

    /// Override the capacity probe.
    #[must_use]
    pub fn with_capacity_probe(mut self, capacity: Arc<dyn CapacityProbe>) -> Self {
        self.capacity = capacity;
        self
    }

    /// Override the hallucination-detection thresholds.
    #[must_use]
    pub fn with_hallucination_policy(mut self, policy: HallucinationPolicy) -> Self {
        self.hallucination_policy = policy;
        self
    }

    /// Engine identity.
    #[must_use]
    pub fn engine_id() -> EngineId {
        EngineId::new(WHISPER_ENGINE_ID)
    }

    /// Model identity recorded on revisions.
    #[must_use]
    pub fn model_identity(&self) -> ModelIdentity {
        let identity = ModelIdentity::new(Self::engine_id(), self.config.model_id.clone());
        match &self.config.model_version {
            Some(version) => identity.with_version(version.clone()),
            None => identity,
        }
    }

    /// The configured workload arbiter.
    #[must_use]
    pub fn arbiter(&self) -> &LocalWorkloadArbiter {
        &self.arbiter
    }

    /// Read the model header, if the weights are installed.
    pub fn model_header(&self) -> Result<WhisperModelHeader, ModelProbeError> {
        read_model_header(&self.config.model_path)
    }

    /// Resolve the backend this engine would use right now.
    pub fn resolve_backend(&self) -> Result<BackendSelection, SttError> {
        select_backend(self.probe.as_ref(), self.config.backend_preference).map_err(|error| {
            SttError::Unavailable.tap_log(&format!("required backend unavailable: {error}"))
        })
    }

    /// Formats this adapter accepts (§8.3 `supported_formats`).
    ///
    /// Raw canonical PCM is the fast path; the container formats are decoded
    /// through the audio pipeline first, which is why they are listed honestly
    /// rather than left `unknown`.
    #[must_use]
    pub fn supported_formats() -> Vec<MediaFormat> {
        vec![
            MediaFormat::new("pcm", "f32le"),
            MediaFormat::new("wav", "pcm_s16le"),
            MediaFormat::new("wav", "pcm_f32le"),
            MediaFormat::new("mp3", "mp3"),
            MediaFormat::new("mp4", "aac"),
            MediaFormat::new("flac", "flac"),
            MediaFormat::new("ogg", "vorbis"),
        ]
    }
}

/// Small helper so an error can carry a log line without changing its type.
trait TapLog: Sized {
    fn tap_log(self, message: &str) -> Self;
}

impl TapLog for SttError {
    fn tap_log(self, message: &str) -> Self {
        tracing::warn!(target: "plaudon::ai_whisper", "{message}");
        self
    }
}

#[async_trait::async_trait]
impl SpeechToText for WhisperEngine {
    async fn capabilities(&self) -> SttCapabilities {
        let engine_id = WhisperEngine::engine_id();
        let mut capabilities = SttCapabilities::unknown(engine_id.clone(), self.model_identity());

        capabilities.offline = Tri::Yes;
        capabilities.privacy_class = Known::Known(PrivacyClass::OnDevice);
        capabilities.pricing_basis = Known::Known(PricingBasis::None);
        // A local engine implements no wire contract; it is never dispatched.
        capabilities.request_family = RequestFamily::Unsupported;
        capabilities.supports_segments = Tri::Yes;
        capabilities.supports_word_timestamps =
            Tri::from_observation(Some(self.config.params.token_timestamps));
        capabilities.supports_diarization = Tri::No;
        capabilities.can_cancel = Tri::Yes;
        capabilities.supported_formats = Known::Known(WhisperEngine::supported_formats());
        capabilities.max_duration_ms = Known::Known(MAX_RUN_DURATION_MS);
        capabilities.max_file_bytes = Known::Known(MAX_RUN_BYTES);
        // Weights are installed, so nothing further is downloaded for a run.
        capabilities.model_download_bytes = Known::Known(0);

        let header = match self.model_header() {
            Ok(header) => header,
            Err(error) => {
                capabilities.status = CapabilityStatus::Unsupported;
                capabilities.availability_reason = match error {
                    ModelProbeError::Unreadable { .. } => AvailabilityReason::ModelAssetMissing {
                        asset: self.config.model_id.clone(),
                    },
                    other => AvailabilityReason::ProbeFailed {
                        detail: other.to_string(),
                    },
                };
                return capabilities;
            }
        };

        // Observed, not inferred from the model's name.
        capabilities.supports_language_detection =
            Tri::from_observation(Some(header.is_multilingual()));
        capabilities.locales = Known::Known(if header.is_multilingual() {
            multilingual_locales()
        } else {
            vec![LanguageTag::parse("en").unwrap_or_else(|_| unreachable!("`en` is a valid tag"))]
        });

        let backend = select_backend(self.probe.as_ref(), self.config.backend_preference);
        let accelerators: Vec<String> = match &backend {
            Ok(selection) if selection.chosen.is_gpu() => vec![
                Backend::Cpu.accelerator_label().to_string(),
                selection.chosen.accelerator_label().to_string(),
            ],
            _ => vec![Backend::Cpu.accelerator_label().to_string()],
        };
        let estimate = WorkloadEstimate::for_whisper(header.byte_length, 0);
        capabilities.hardware_requirements = Known::Known(HardwareRequirements {
            min_ram_bytes: estimate.resident_bytes(),
            // GPU is an accelerator, never a requirement (§12).
            requires_accelerator: false,
            accelerator_backends: accelerators,
        });

        match backend {
            Ok(_) => {
                capabilities.status = CapabilityStatus::Supported;
                capabilities.availability_reason = AvailabilityReason::Available;
            }
            Err(error) => {
                capabilities.status = CapabilityStatus::Unsupported;
                capabilities.availability_reason = AvailabilityReason::HardwareUnsupported {
                    detail: error.to_string(),
                };
            }
        }

        capabilities
    }

    async fn transcribe(
        &self,
        input: SttInput,
        ctx: JobContext,
    ) -> Result<TranscriptDraft, SttError> {
        // 1. Is the model actually there? A missing model is actionable, never
        //    a crash and never a fallback to another engine.
        let header = self.model_header().map_err(|error| match error {
            ModelProbeError::Unreadable { .. } => SttError::ModelNotInstalled.tap_log(&format!(
                "model `{}` is not installed",
                self.config.model_id
            )),
            other => SttError::Unavailable.tap_log(&format!("model probe failed: {other}")),
        })?;

        // 2. Backend, then a permit. Taking the permit after backend selection
        //    means a refused workload never holds one.
        let mut selection = self.resolve_backend()?;
        let lease = self
            .arbiter
            .try_acquire(selection.chosen)
            .map_err(|busy| SttError::InsufficientResources.tap_log(&busy.to_string()))?;

        // 3. Audio. Raw canonical PCM is read directly; anything else goes
        //    through the audio pipeline's deterministic canonicalisation.
        let pcm = load_pcm(&input)?;
        if pcm.is_empty() {
            return Err(SttError::InputTooLarge.tap_log("input decoded to zero samples"));
        }

        // 4. Resource preflight: refuse rather than OOM (§14.4).
        let audio_bytes = (pcm.len() * std::mem::size_of::<f32>()) as u64;
        let estimate = WorkloadEstimate::for_whisper(header.byte_length, audio_bytes);
        let capacity = self
            .capacity
            .capacity(
                input
                    .media_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
            )
            .map_err(|error| {
                SttError::InsufficientResources.tap_log(&format!("capacity probe failed: {error}"))
            })?;
        if let Err(error) = preflight(estimate, capacity, selection.chosen) {
            return Err(
                SttError::InsufficientResources.tap_log(&format!("preflight refused: {error}"))
            );
        }

        if ctx.is_cancelled() {
            return Err(SttError::Cancelled);
        }

        // 5. Run. A GPU failure either becomes an explicit error or a clearly
        //    recorded CPU retry; never a silently degraded transcript.
        let run = self
            .run(&pcm, &selection, &ctx, header.is_multilingual())
            .await;
        let raw = match run {
            Ok(raw) => raw,
            Err(RunFailure::Cancelled) => return Err(SttError::Cancelled),
            Err(RunFailure::Engine(detail)) => {
                if selection.chosen.is_gpu() && self.config.cpu_retry_after_gpu_failure {
                    tracing::warn!(
                        target: "plaudon::ai_whisper",
                        backend = selection.chosen.as_str(),
                        "GPU backend failed mid-run; retrying the whole job on the CPU: {detail}"
                    );
                    selection = fall_back_to_cpu(&selection, &detail);
                    match self
                        .run(&pcm, &selection, &ctx, header.is_multilingual())
                        .await
                    {
                        Ok(raw) => raw,
                        Err(RunFailure::Cancelled) => return Err(SttError::Cancelled),
                        Err(RunFailure::Engine(detail)) => {
                            return Err(SttError::EngineFailure
                                .tap_log(&format!("cpu retry also failed: {detail}")))
                        }
                    }
                } else {
                    return Err(
                        SttError::EngineFailure.tap_log(&format!("whisper run failed: {detail}"))
                    );
                }
            }
        };

        // The lease is released here, before the (cheap) post-processing, so a
        // queued job can start as soon as the compute is done.
        drop(lease);

        if ctx.is_cancelled() {
            return Err(SttError::Cancelled);
        }

        // 6. Build and validate the draft.
        let mut draft = self.build_draft(&input, raw, &selection, header);
        let activity =
            measure_speech_activity(&pcm, WHISPER_SAMPLE_RATE_HZ, 1, &SilenceOptions::default());
        let activity = shift_activity(&activity, input.source_sample_offset);
        draft.annotations.extend(detect_hallucinations(
            &draft,
            Some(&activity),
            &self.hallucination_policy,
        ));

        let capabilities = self.capabilities().await;
        validate_draft(&draft, Some(&capabilities)).map_err(|violation| {
            SttError::TruthfulnessViolation.tap_log(&format!("draft rejected: {violation}"))
        })?;

        Ok(draft)
    }
}

#[allow(
    unsafe_code,
    clippy::multiple_unsafe_ops_per_block,
    clippy::undocumented_unsafe_blocks
)]
/// Raw ggml abort callback: reads the shared cancellation flag.
///
/// # Safety
/// `user_data` must point at a live `AtomicBool`.
unsafe extern "C" fn abort_trampoline(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    let flag = unsafe { &*(user_data as *const AtomicBool) };
    flag.load(Ordering::SeqCst)
}

/// Outcome of one whisper run.
struct RawRun {
    segments: Vec<RawSegment>,
    detected_language: Option<String>,
}

struct RawSegment {
    start_ticks: i64,
    end_ticks: i64,
    text: String,
    average_logprob: Option<f64>,
    words: Vec<RawWord>,
}

struct RawWord {
    text: String,
    start_ticks: i64,
    end_ticks: i64,
    probability: f32,
}

enum RunFailure {
    Cancelled,
    Engine(String),
}

impl WhisperEngine {
    async fn run(
        &self,
        pcm: &[f32],
        selection: &BackendSelection,
        ctx: &JobContext,
        multilingual: bool,
    ) -> Result<RawRun, RunFailure> {
        let pcm = pcm.to_vec();
        let model_path = self.config.model_path.clone();
        let params = self.config.params.clone();
        let backend = selection.chosen;
        let loaded = Arc::clone(&self.loaded);
        let cancel = ctx.cancel.clone();
        let progress_ctx = ctx.clone();
        let total_ms = (pcm.len() as u64 * 1000) / u64::from(WHISPER_SAMPLE_RATE_HZ);

        let join = tokio::task::spawn_blocking(move || {
            run_blocking(
                &loaded,
                &model_path,
                backend,
                &params,
                &pcm,
                &cancel,
                &progress_ctx,
                total_ms,
                multilingual,
            )
        })
        .await;

        match join {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Err(RunFailure::Cancelled),
            Err(error) => Err(RunFailure::Engine(format!("worker thread failed: {error}"))),
        }
    }

    fn build_draft(
        &self,
        input: &SttInput,
        raw: RawRun,
        selection: &BackendSelection,
        header: WhisperModelHeader,
    ) -> TranscriptDraft {
        let offset = input.source_sample_offset;
        let mut segments: Vec<TranscriptSegmentDraft> = Vec::with_capacity(raw.segments.len());
        let mut annotations: Vec<TranscriptAnnotation> = Vec::new();
        let mut previous_end: u64 = 0;

        for segment in raw.segments {
            let text = segment.text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            let index = u32::try_from(segments.len()).unwrap_or(u32::MAX);
            let mut start_sample = ticks_to_samples(segment.start_ticks) + offset;
            let mut end_sample = ticks_to_samples(segment.end_ticks) + offset;
            if end_sample < start_sample {
                std::mem::swap(&mut start_sample, &mut end_sample);
            }
            // whisper occasionally emits a segment that starts fractionally
            // before its predecessor ended. Clamping keeps the timeline
            // monotonic; the annotation keeps the adjustment visible.
            if start_sample < previous_end {
                annotations.push(TranscriptAnnotation {
                    kind: AnnotationKind::AmbiguousOverlap,
                    reason:
                        "engine reported a segment starting before the previous one ended; the \
                         start was clamped and the uncertainty widened"
                            .to_string(),
                    segment_index: segments.len(),
                    start: Some(source_time(previous_end)),
                    end: Some(source_time(end_sample.max(previous_end))),
                });
                start_sample = previous_end;
                end_sample = end_sample.max(previous_end);
            }
            previous_end = end_sample;

            let mut draft_segment = TranscriptSegmentDraft::timed(
                index,
                source_time(start_sample),
                source_time(end_sample),
                text,
            );
            if let Some(average) = segment.average_logprob {
                draft_segment.confidence = Some(Confidence::new(
                    WhisperEngine::engine_id(),
                    ConfidenceScale::AverageLogProbability,
                    average,
                ));
            }
            if self.config.params.token_timestamps {
                draft_segment.words = segment
                    .words
                    .into_iter()
                    .filter_map(|word| {
                        let word_start = (ticks_to_samples(word.start_ticks) + offset)
                            .clamp(start_sample, end_sample);
                        let word_end = (ticks_to_samples(word.end_ticks) + offset)
                            .clamp(word_start, end_sample);
                        let text = word.text.trim().to_string();
                        if text.is_empty() {
                            return None;
                        }
                        Some(DraftWordAlignment {
                            text,
                            start: SourceTime::new(
                                word_start,
                                WHISPER_SAMPLE_RATE_HZ,
                                TimePrecision::Word,
                            ),
                            end: SourceTime::new(
                                word_end,
                                WHISPER_SAMPLE_RATE_HZ,
                                TimePrecision::Word,
                            ),
                            confidence: Some(Confidence::new(
                                WhisperEngine::engine_id(),
                                ConfidenceScale::Probability,
                                f64::from(word.probability),
                            )),
                        })
                    })
                    .collect();
            }
            segments.push(draft_segment);
        }

        // A run that produced no timed segment is a text-free result, not a
        // timeline: say so rather than claiming `native` with nothing behind
        // it (§8.6A).
        let alignment_origin = if segments.is_empty() {
            AlignmentOrigin::None
        } else {
            AlignmentOrigin::Native
        };

        let mut engine_params = self.config.params.recorded();
        engine_params.insert("backend".to_string(), selection.chosen.as_str().to_string());
        engine_params.insert(
            "whisper_version".to_string(),
            whisper_rs::get_whisper_version().to_string(),
        );
        engine_params.insert("model_size".to_string(), header.size_label().to_string());
        engine_params.insert(
            "model_multilingual".to_string(),
            header.is_multilingual().to_string(),
        );
        if let Some(reason) = &selection.fallback_reason {
            engine_params.insert("backend_fallback".to_string(), reason.clone());
        }

        let locale = raw
            .detected_language
            .as_deref()
            .or(self.config.params.language.as_deref())
            .and_then(|tag| LanguageTag::parse(tag).ok());

        TranscriptDraft {
            engine_id: WhisperEngine::engine_id(),
            model: self.model_identity(),
            input_sha256: input.media_sha256,
            locale,
            alignment_origin,
            segments,
            duration_ms: input.duration_ms,
            engine_params,
            chunking_strategy: input.chunking_strategy.clone(),
            normalization_profile: input.normalization_profile.clone(),
            annotations,
            backend: Some(selection.chosen.as_str().to_string()),
        }
    }
}

const fn ticks_to_samples(ticks: i64) -> u64 {
    if ticks <= 0 {
        0
    } else {
        (ticks as u64) * SAMPLES_PER_TICK
    }
}

const fn source_time(sample_offset: u64) -> SourceTime {
    SourceTime::new(
        sample_offset,
        WHISPER_SAMPLE_RATE_HZ,
        TimePrecision::Segment,
    )
}

/// Move a measured activity map onto the absolute source timeline.
fn shift_activity(activity: &SpeechActivity, offset: u64) -> SpeechActivity {
    if offset == 0 {
        return activity.clone();
    }
    SpeechActivity::new(
        activity.sample_rate_hz(),
        activity.total_samples() + offset,
        activity
            .speech_intervals()
            .iter()
            .map(|(start, end)| (start + offset, end + offset))
            .collect(),
    )
}

/// The locales a multilingual whisper model advertises.
///
/// whisper.cpp exposes its own language table, so this is observed rather than
/// hard-coded.
fn multilingual_locales() -> Vec<LanguageTag> {
    let max = whisper_rs::get_lang_max_id();
    (0..=max)
        .filter_map(whisper_rs::get_lang_str)
        .filter_map(|code| LanguageTag::parse(code).ok())
        .collect()
}

/// Read the canonical PCM an engine run needs.
fn load_pcm(input: &SttInput) -> Result<Vec<f32>, SttError> {
    if input.format == MediaFormat::new("pcm", "f32le") {
        if input.sample_rate_hz != WHISPER_SAMPLE_RATE_HZ || input.channels != 1 {
            return Err(SttError::UnsupportedFormat.tap_log(&format!(
                "raw PCM must be {WHISPER_SAMPLE_RATE_HZ} Hz mono; got {} Hz / {} channels",
                input.sample_rate_hz, input.channels
            )));
        }
        let bytes = std::fs::read(&input.media_path).map_err(|source| {
            SttError::Unavailable.tap_log(&format!("cannot read canonical PCM: {source}"))
        })?;
        if bytes.len() % 4 != 0 {
            return Err(SttError::UnsupportedFormat
                .tap_log("canonical PCM length is not a whole number of f32 samples"));
        }
        return Ok(plaudon_audio::silence::f32_samples_from_pcm_bytes(&bytes));
    }

    // Anything else is validated and canonicalised through the audio pipeline,
    // which is also what guarantees the derivative is deterministic.
    let media = validate_media(&input.media_path, &ValidationRequest::manual_import()).map_err(
        |error| SttError::UnsupportedFormat.tap_log(&format!("media validation failed: {error}")),
    )?;
    let mut sink = MemoryPcmSink::default();
    let detached = JobContext::new(plaudon_domain::ids::JobId::new());
    plaudon_audio::decode::decode_to_canonical_pcm(
        &media,
        &CanonicalPcmProfile::whisper_mono_16k(),
        &mut sink,
        &detached,
    )
    .map_err(|error| SttError::UnsupportedFormat.tap_log(&format!("decode failed: {error}")))?;
    Ok(plaudon_audio::silence::f32_samples_from_pcm_bytes(
        &sink.bytes,
    ))
}

#[allow(clippy::too_many_arguments)]
fn run_blocking(
    loaded: &Mutex<Option<LoadedContext>>,
    model_path: &std::path::Path,
    backend: Backend,
    params: &WhisperParams,
    pcm: &[f32],
    cancel: &plaudon_ai_contracts::CancellationToken,
    ctx: &JobContext,
    total_ms: u64,
    multilingual: bool,
) -> Result<RawRun, RunFailure> {
    let mut guard = loaded
        .lock()
        .map_err(|_| RunFailure::Engine("whisper context mutex was poisoned".to_string()))?;

    // Reload when the backend changed, so a CPU retry really runs on the CPU.
    let needs_load = guard
        .as_ref()
        .is_none_or(|context| context.backend != backend);
    if needs_load {
        *guard = None;
        let mut context_params = WhisperContextParameters::default();
        context_params.use_gpu(backend.is_gpu());
        let context = WhisperContext::new_with_params(model_path, context_params)
            .map_err(|error| RunFailure::Engine(format!("model load failed: {error}")))?;
        *guard = Some(LoadedContext { backend, context });
    }
    let context = &guard
        .as_ref()
        .ok_or_else(|| RunFailure::Engine("context unavailable after load".to_string()))?
        .context;

    let mut state = context
        .create_state()
        .map_err(|error| RunFailure::Engine(format!("state creation failed: {error}")))?;

    let strategy = if params.beam_size > 1 {
        SamplingStrategy::BeamSearch {
            beam_size: i32::try_from(params.beam_size).unwrap_or(i32::MAX),
            patience: 0.0,
        }
    } else {
        SamplingStrategy::Greedy { best_of: 1 }
    };
    let mut full = FullParams::new(strategy);
    full.set_n_threads(i32::try_from(params.threads).unwrap_or(1).max(1));
    full.set_translate(params.translate);
    // An English-only model cannot detect a language, and asking it to try
    // makes whisper abandon the decode and return nothing. The model header
    // is the authority here, not the model's file name.
    let language = match (params.language.as_deref(), multilingual) {
        (Some(explicit), _) => Some(explicit),
        (None, true) => None,
        (None, false) => Some("en"),
    };
    full.set_language(language);
    full.set_detect_language(language.is_none());
    full.set_temperature(params.temperature);
    full.set_no_speech_thold(params.no_speech_threshold);
    full.set_logprob_thold(params.logprob_threshold);
    full.set_entropy_thold(params.entropy_threshold);
    full.set_token_timestamps(params.token_timestamps);
    // Nothing is printed to stdout: diagnostics go through `tracing`.
    full.set_print_special(false);
    full.set_print_progress(false);
    full.set_print_realtime(false);
    full.set_print_timestamps(false);

    // whisper-rs 0.16's `set_abort_callback_safe` installs a trampoline typed
    // for the closure while handing it a double-boxed trait object, which
    // aborts the process the first time ggml polls it. We install the raw
    // callback ourselves over a plain `AtomicBool`, which has no such
    // ambiguity, and keep a watcher thread bridging the cancellation token to
    // that flag so cancellation stays prompt.
    let abort_flag = Arc::new(AtomicBool::new(cancel.is_cancelled()));
    let watcher_stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let flag = Arc::clone(&abort_flag);
        let stop = Arc::clone(&watcher_stop);
        let cancel = cancel.clone();
        std::thread::Builder::new()
            .name("whisper-cancel-watch".to_string())
            .spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if cancel.is_cancelled() {
                        flag.store(true, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            })
            .map_err(|error| RunFailure::Engine(format!("cancel watcher failed: {error}")))?
    };
    // SAFETY: `abort_flag` is an `Arc` held alive for the whole `full()` call,
    // the pointer is never written through, and `abort_trampoline` only reads
    // an `AtomicBool` from it.
    #[allow(unsafe_code)]
    unsafe {
        full.set_abort_callback(Some(abort_trampoline));
        full.set_abort_callback_user_data(Arc::as_ptr(&abort_flag).cast::<c_void>().cast_mut());
    }
    {
        let ctx = ctx.clone();
        full.set_progress_callback_safe(move |percent: i32| {
            let clamped = percent.clamp(0, 100);
            #[allow(clippy::cast_precision_loss)]
            ctx.report(Progress::fraction(clamped as f32 / 100.0));
            ctx.report(Progress::processed_ms(
                total_ms * u64::try_from(clamped).unwrap_or(0) / 100,
            ));
        });
    }

    let outcome = state.full(full, pcm);
    watcher_stop.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    outcome.map_err(|error| {
        if cancel.is_cancelled() {
            RunFailure::Cancelled
        } else {
            RunFailure::Engine(format!("decode failed: {error}"))
        }
    })?;

    if cancel.is_cancelled() {
        return Err(RunFailure::Cancelled);
    }

    let count = state.full_n_segments();
    let mut segments = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for index in 0..count {
        let Some(segment) = state.get_segment(index) else {
            continue;
        };
        let text = segment.to_str_lossy().unwrap_or_default().to_string();
        let token_count = segment.n_tokens();
        let mut logprob_sum = 0.0f64;
        let mut logprob_count = 0u32;
        let mut words: Vec<RawWord> = Vec::new();
        for token_index in 0..token_count {
            let Some(token) = segment.get_token(token_index) else {
                continue;
            };
            let data = token.token_data();
            let probability = token.token_probability();
            if probability > 0.0 {
                logprob_sum += f64::from(probability).ln();
                logprob_count += 1;
            }
            if params.token_timestamps {
                let text = token.to_str_lossy().unwrap_or_default().to_string();
                // Special tokens are markup, not speech.
                if !text.starts_with("[_") && !text.starts_with("<|") {
                    words.push(RawWord {
                        text,
                        start_ticks: data.t0,
                        end_ticks: data.t1,
                        probability,
                    });
                }
            }
        }
        segments.push(RawSegment {
            start_ticks: segment.start_timestamp(),
            end_ticks: segment.end_timestamp(),
            text,
            average_logprob: (logprob_count > 0).then(|| logprob_sum / f64::from(logprob_count)),
            words,
        });
    }

    let detected_language = {
        let id = state.full_lang_id_from_state();
        whisper_rs::get_lang_str(id).map(ToString::to_string)
    };

    Ok(RawRun {
        segments,
        detected_language,
    })
}

/// Hold a lease while a closure runs. Used by the job engine to keep a permit
/// across a multi-chunk transcription.
pub fn with_lease<T>(lease: WorkloadLease, run: impl FnOnce() -> T) -> T {
    let _lease = lease;
    run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendAvailability, BackendUnavailable};
    use plaudon_domain::media::Sha256Digest;
    use std::future::Future;

    #[derive(Debug)]
    struct NoGpuProbe;

    impl BackendProbe for NoGpuProbe {
        fn probe(&self) -> Vec<BackendAvailability> {
            vec![
                BackendAvailability::unavailable(
                    Backend::Metal,
                    BackendUnavailable::DeviceMissing("forced unavailable for test".into()),
                ),
                BackendAvailability::available(Backend::Cpu),
            ]
        }

        fn runtime_description(&self) -> String {
            "no-gpu test probe".to_string()
        }
    }

    fn config(path: PathBuf) -> WhisperConfig {
        WhisperConfig::new(path, "tiny.en").forcing_cpu()
    }

    fn pcm_input(path: PathBuf, frames: u64) -> SttInput {
        SttInput {
            media_path: path,
            media_sha256: Sha256Digest::of(b"fixture"),
            byte_length: frames * 4,
            format: MediaFormat::new("pcm", "f32le"),
            duration_ms: Some(frames * 1000 / u64::from(WHISPER_SAMPLE_RATE_HZ)),
            locale_hint: None,
            sample_rate_hz: WHISPER_SAMPLE_RATE_HZ,
            channels: 1,
            source_sample_offset: 0,
            normalization_profile: None,
            chunking_strategy: plaudon_ai_contracts::ChunkingStrategy::SingleChunk,
        }
    }

    #[tokio::test]
    async fn a_missing_model_is_actionable_not_a_crash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = WhisperEngine::new(config(dir.path().join("absent.bin")));

        let capabilities = engine.capabilities().await;
        assert_eq!(
            capabilities.availability_reason,
            AvailabilityReason::ModelAssetMissing {
                asset: "tiny.en".to_string()
            }
        );
        assert!(!capabilities.is_selectable());
        // Offline and privacy facts stay true even when unavailable.
        assert_eq!(capabilities.offline, Tri::Yes);
        assert_eq!(
            capabilities.privacy_class,
            Known::Known(PrivacyClass::OnDevice)
        );

        let pcm_path = dir.path().join("audio.pcm");
        std::fs::write(&pcm_path, vec![0u8; 16_000 * 4]).expect("write");
        let error = engine
            .transcribe(
                pcm_input(pcm_path, 16_000),
                JobContext::new(plaudon_domain::ids::JobId::new()),
            )
            .await
            .expect_err("a missing model must fail");
        assert_eq!(error, SttError::ModelNotInstalled);
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_whisper_model_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-a-model.bin");
        std::fs::write(&path, vec![0x41u8; 4096]).expect("write");
        let engine = WhisperEngine::new(config(path));
        let capabilities = engine.capabilities().await;
        assert!(matches!(
            capabilities.availability_reason,
            AvailabilityReason::ProbeFailed { .. }
        ));
    }

    #[test]
    fn capabilities_never_claim_a_wire_contract() {
        // A local engine must not be dispatchable over any request family.
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = WhisperEngine::new(config(dir.path().join("absent.bin")));
        let capabilities = futures_lite_block_on(async { engine.capabilities().await });
        assert_eq!(capabilities.request_family, RequestFamily::Unsupported);
        assert!(!capabilities.request_family.has_builder());
    }

    /// Minimal block-on so the synchronous tests above do not need a runtime.
    fn futures_lite_block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn declared_limits_are_consistent_with_the_canonical_profile() {
        // 30 minutes of 16 kHz mono f32.
        assert_eq!(MAX_RUN_BYTES, 1_800 * 16_000 * 4);
        assert_eq!(MAX_RUN_DURATION_MS, 1_800_000);
    }

    #[test]
    fn recorded_parameters_cover_everything_that_changes_output() {
        let recorded = WhisperParams::default().recorded();
        for key in [
            "language",
            "translate",
            "threads",
            "beam_size",
            "temperature",
            "token_timestamps",
            "no_speech_threshold",
            "logprob_threshold",
            "entropy_threshold",
        ] {
            assert!(recorded.contains_key(key), "missing parameter `{key}`");
        }
    }

    #[test]
    fn default_parameters_are_deterministic() {
        let params = WhisperParams::default();
        assert!((params.temperature - 0.0).abs() < f32::EPSILON);
        assert_eq!(params.beam_size, 1);
        assert_eq!(
            params.threads, 4,
            "thread count must be fixed so transcripts reproduce across machines"
        );
    }

    #[test]
    fn ticks_convert_to_samples_at_the_canonical_rate() {
        assert_eq!(ticks_to_samples(0), 0);
        assert_eq!(ticks_to_samples(100), 16_000);
        assert_eq!(ticks_to_samples(-5), 0);
    }

    #[test]
    fn a_forced_unavailable_gpu_resolves_to_cpu_with_a_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = WhisperEngine::new(WhisperConfig {
            backend_preference: BackendPreference::PreferGpu,
            ..config(dir.path().join("absent.bin"))
        })
        .with_backend_probe(Arc::new(NoGpuProbe));

        let selection = engine.resolve_backend().expect("cpu baseline");
        assert_eq!(selection.chosen, Backend::Cpu);
        assert!(selection
            .fallback_reason
            .expect("reason")
            .contains("forced unavailable for test"));
    }

    #[test]
    fn supported_formats_include_the_canonical_derivative() {
        let formats = WhisperEngine::supported_formats();
        assert!(formats.contains(&MediaFormat::new("pcm", "f32le")));
        assert!(formats.contains(&MediaFormat::new("wav", "pcm_s16le")));
        assert!(formats.contains(&MediaFormat::new("mp3", "mp3")));
    }

    #[test]
    fn the_whisper_language_table_is_read_from_the_runtime() {
        let locales = multilingual_locales();
        assert!(locales.len() > 50, "got {} locales", locales.len());
        assert!(locales
            .iter()
            .any(|tag| tag.as_str().eq_ignore_ascii_case("en")));
    }

    #[test]
    fn shifting_activity_moves_every_interval() {
        let activity = SpeechActivity::new(16_000, 1_000, vec![(100, 200)]);
        let shifted = shift_activity(&activity, 5_000);
        assert_eq!(shifted.speech_intervals(), &[(5_100, 5_200)]);
        assert_eq!(shifted.total_samples(), 6_000);
        assert_eq!(shift_activity(&activity, 0), activity);
    }
}
