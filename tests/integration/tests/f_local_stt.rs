#![allow(clippy::await_holding_lock)] // see `EngineGate`

//! Offline local speech-to-text, end to end (spec §8, §12, §19 Phase 3).
//!
//! These are the tests that prove EPIC F's headline claim: a recording is
//! imported, validated, canonicalised, transcribed by whisper.cpp with no
//! network access, and the result carries honest timestamps.
//!
//! **Hermeticity.** Nothing here reaches the network. The audio fixtures are
//! committed; the model weights are seeded into
//! `tests/fixtures/stt/models/` by `scripts/fetch-stt-fixtures.sh`, which the
//! developer or CI runs once, explicitly. When the weights are absent the
//! model-dependent tests report that they were skipped and why, rather than
//! passing silently — a green run with no model would be a lie about
//! coverage.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_ai_contracts::capabilities::MediaFormat;
use plaudon_ai_contracts::job::{JobContext, RecordingProgressSink};
use plaudon_ai_contracts::stt::{SpeechToText, SttError, SttInput};
use plaudon_ai_contracts::{
    validate_draft, AlignmentOrigin, ChunkingStrategy, Known, SttCapabilities, Tri,
};
use plaudon_ai_whisper::backend::{Backend, BackendAvailability, BackendProbe, BackendUnavailable};
use plaudon_ai_whisper::{WhisperConfig, WhisperEngine, WHISPER_SAMPLE_RATE_HZ};
use plaudon_audio::decode::{decode_to_canonical_pcm, CanonicalPcmProfile, MemoryPcmSink};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::ids::JobId;
use plaudon_domain::media::Sha256Digest;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn fixture(relative: &str) -> PathBuf {
    repo_root().join("tests/fixtures/stt").join(relative)
}

/// The seeded model, or `None` with an explanation.
fn tiny_model() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("PLAUDON_WHISPER_MODEL") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    let path = fixture("models/ggml-tiny.en.bin");
    path.is_file().then_some(path)
}

/// Print a skip notice and return, when the model is not seeded.
macro_rules! model_or_skip {
    () => {
        match tiny_model() {
            Some(path) => path,
            None => {
                eprintln!(
                    "SKIPPED: whisper model fixture is absent. Run \
                     `scripts/fetch-stt-fixtures.sh` to seed \
                     tests/fixtures/stt/models/, or set PLAUDON_WHISPER_MODEL."
                );
                return;
            }
        }
    };
}

/// Serialises every test that loads whisper weights.
///
/// This is not test hygiene, it is the product rule: §14.4 allows **one**
/// active local model workload by default, and ggml's backend registry is
/// initialised once per process and is not safe to bring up concurrently.
/// Running these in parallel traps inside the Metal device setup.
static ENGINE_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn engine_gate() -> std::sync::MutexGuard<'static, ()> {
    ENGINE_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Words that must appear in any correct transcription of the fixture.
///
/// The fixture is a public-domain excerpt of the 1961 US presidential
/// inaugural address; these four words are unambiguous in it.
const EXPECTED_WORDS: [&str; 4] = ["ask", "country", "fellow", "americans"];

fn normalise(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decode any committed fixture into the canonical whisper derivative.
fn canonical_pcm(relative: &str) -> (Vec<u8>, Sha256Digest, u64) {
    let path = fixture(relative);
    let media = validate_media(&path, &ValidationRequest::manual_import())
        .unwrap_or_else(|error| panic!("fixture {relative} failed validation: {error}"));
    let mut sink = MemoryPcmSink::default();
    let outcome = decode_to_canonical_pcm(
        &media,
        &CanonicalPcmProfile::whisper_mono_16k(),
        &mut sink,
        &JobContext::new(JobId::new()),
    )
    .unwrap_or_else(|error| panic!("fixture {relative} failed to decode: {error}"));
    (sink.bytes, media.sha256(), outcome.frames)
}

/// Write canonical PCM to a scratch file and build the engine input for it.
fn pcm_input(dir: &Path, relative: &str) -> SttInput {
    let (bytes, source_sha, frames) = canonical_pcm(relative);
    let path = dir.join("canonical.pcm");
    std::fs::write(&path, &bytes).expect("write canonical pcm");
    SttInput {
        media_path: path,
        media_sha256: source_sha,
        byte_length: bytes.len() as u64,
        format: MediaFormat::new("pcm", "f32le"),
        duration_ms: Some(frames * 1000 / u64::from(WHISPER_SAMPLE_RATE_HZ)),
        locale_hint: None,
        sample_rate_hz: WHISPER_SAMPLE_RATE_HZ,
        channels: 1,
        source_sample_offset: 0,
        normalization_profile: None,
        chunking_strategy: ChunkingStrategy::SingleChunk,
    }
}

fn cpu_engine(model: PathBuf) -> WhisperEngine {
    WhisperEngine::new(WhisperConfig::new(model, "ggml-tiny.en").forcing_cpu())
}

#[tokio::test]
async fn a_wav_recording_is_transcribed_offline_with_native_timestamps() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = cpu_engine(model);

    let sink = Arc::new(RecordingProgressSink::new());
    let ctx = JobContext::new(JobId::new()).with_progress(sink.clone());
    let draft = engine
        .transcribe(pcm_input(dir.path(), "audio/jfk.wav"), ctx)
        .await
        .expect("transcription should succeed");

    let text = normalise(&draft.plain_text());
    for word in EXPECTED_WORDS {
        assert!(
            text.contains(word),
            "transcript is missing `{word}`: {text}"
        );
    }

    assert_eq!(draft.alignment_origin, AlignmentOrigin::Native);
    assert_ne!(draft.segments.len(), 0);
    for segment in &draft.segments {
        let start = segment.start.expect("native alignment must carry a start");
        let end = segment.end.expect("native alignment must carry an end");
        assert_eq!(start.sample_rate_hz, WHISPER_SAMPLE_RATE_HZ);
        assert!(end.sample_offset >= start.sample_offset);
    }

    // Timestamps land inside the fixture's real duration (11 s), with a
    // second of slack for whisper's 30 s padding window.
    let (_, _, frames) = canonical_pcm("audio/jfk.wav");
    let last_end = draft
        .segments
        .last()
        .and_then(|segment| segment.end)
        .expect("a final timestamp")
        .sample_offset;
    assert!(
        last_end <= frames + u64::from(WHISPER_SAMPLE_RATE_HZ),
        "final timestamp {last_end} is past the {frames}-sample recording"
    );

    // Every §8.5 provenance field a revision needs is present.
    assert_eq!(draft.backend.as_deref(), Some("cpu"));
    assert_eq!(
        draft.engine_params.get("beam_size").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        draft.engine_params.get("threads").map(String::as_str),
        Some("4")
    );
    assert!(draft.engine_params.contains_key("whisper_version"));
    assert_eq!(
        draft.engine_params.get("backend").map(String::as_str),
        Some("cpu")
    );
    assert_eq!(draft.model.model_id, "ggml-tiny.en");

    // Progress was reported and never went backwards (§17.5).
    let updates = sink.updates();
    assert_ne!(updates.len(), 0, "no progress was reported");
    let fractions: Vec<f32> = updates
        .iter()
        .filter_map(|update| update.fraction)
        .collect();
    assert!(
        fractions.windows(2).all(|pair| pair[1] >= pair[0]),
        "progress went backwards: {fractions:?}"
    );

    let capabilities = engine.capabilities().await;
    validate_draft(&draft, Some(&capabilities)).expect("draft must satisfy the §8.6A contract");
}

#[tokio::test]
async fn an_mp3_recording_is_transcribed_through_the_audio_pipeline() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let engine = cpu_engine(model);

    // The engine accepts a container directly: it validates and canonicalises
    // through the audio pipeline before running.
    let path = fixture("audio/jfk.mp3");
    let media = validate_media(&path, &ValidationRequest::manual_import()).expect("validate mp3");
    let input = SttInput {
        media_path: path,
        media_sha256: media.sha256(),
        byte_length: media.profile().byte_length,
        format: MediaFormat::new("mp3", "mp3"),
        duration_ms: media.profile().duration_ms.get().copied(),
        locale_hint: None,
        sample_rate_hz: *media.profile().sample_rate_hz.get().expect("rate"),
        channels: *media.profile().channels.get().expect("channels"),
        source_sample_offset: 0,
        normalization_profile: None,
        chunking_strategy: ChunkingStrategy::SingleChunk,
    };

    let draft = engine
        .transcribe(input, JobContext::new(JobId::new()))
        .await
        .expect("mp3 transcription should succeed");
    let text = normalise(&draft.plain_text());
    for word in EXPECTED_WORDS {
        assert!(
            text.contains(word),
            "mp3 transcript is missing `{word}`: {text}"
        );
    }
}

#[tokio::test]
async fn repeated_runs_produce_an_identical_transcript() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = cpu_engine(model);

    let first = engine
        .transcribe(
            pcm_input(dir.path(), "audio/jfk.wav"),
            JobContext::new(JobId::new()),
        )
        .await
        .expect("first run");
    let second = engine
        .transcribe(
            pcm_input(dir.path(), "audio/jfk.wav"),
            JobContext::new(JobId::new()),
        )
        .await
        .expect("second run");

    assert_eq!(first.plain_text(), second.plain_text());
    assert_eq!(first.segments, second.segments);
    assert_eq!(first.engine_params, second.engine_params);
}

#[tokio::test]
async fn cancellation_returns_cancelled_and_no_partial_transcript() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = cpu_engine(model);

    let ctx = JobContext::new(JobId::new());
    ctx.cancel.cancel();
    let error = engine
        .transcribe(pcm_input(dir.path(), "audio/jfk.wav"), ctx)
        .await
        .expect_err("a cancelled run must not return a transcript");
    assert_eq!(error, SttError::Cancelled);
}

#[tokio::test]
async fn cancellation_mid_run_is_prompt_and_yields_nothing() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = cpu_engine(model);

    let ctx = JobContext::new(JobId::new());
    let cancel = ctx.cancel.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        cancel.cancel();
    });

    let started = std::time::Instant::now();
    let result = engine
        .transcribe(pcm_input(dir.path(), "audio/jfk.wav"), ctx)
        .await;
    canceller.await.expect("canceller");

    match result {
        Err(SttError::Cancelled) => {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "cancellation took {:?}",
                started.elapsed()
            );
        }
        // The fixture is only eleven seconds, so on a fast machine the run can
        // legitimately finish before the cancel lands. A completed transcript
        // is a valid outcome; a partial one is not.
        Ok(draft) => {
            assert_eq!(draft.alignment_origin, AlignmentOrigin::Native);
            assert_ne!(draft.segments.len(), 0);
        }
        Err(other) => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn only_one_local_workload_runs_at_a_time() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = cpu_engine(model);

    // Hold the single permit, then prove a second request is refused rather
    // than queued invisibly (§14.4).
    let lease = engine
        .arbiter()
        .try_acquire(Backend::Cpu)
        .expect("first permit");
    let error = engine
        .transcribe(
            pcm_input(dir.path(), "audio/jfk.wav"),
            JobContext::new(JobId::new()),
        )
        .await
        .expect_err("a second workload must be refused while one is active");
    assert_eq!(error, SttError::InsufficientResources);
    drop(lease);

    // Once the permit is released the same request succeeds.
    assert!(engine
        .transcribe(
            pcm_input(dir.path(), "audio/jfk.wav"),
            JobContext::new(JobId::new())
        )
        .await
        .is_ok());
}

/// A probe that reports every GPU backend as unavailable.
#[derive(Debug)]
struct NoGpuProbe;

impl BackendProbe for NoGpuProbe {
    fn probe(&self) -> Vec<BackendAvailability> {
        vec![
            BackendAvailability::unavailable(
                Backend::Metal,
                BackendUnavailable::DeviceMissing("forced unavailable by the fixture".into()),
            ),
            BackendAvailability::unavailable(
                Backend::Vulkan,
                BackendUnavailable::DeviceMissing("forced unavailable by the fixture".into()),
            ),
            BackendAvailability::available(Backend::Cpu),
        ]
    }

    fn runtime_description(&self) -> String {
        "forced-unavailable-GPU fixture".to_string()
    }
}

#[tokio::test]
async fn a_forced_unavailable_gpu_completes_on_the_cpu_with_identical_content() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let dir = tempfile::tempdir().expect("tempdir");

    let forced_cpu = cpu_engine(model.clone());
    let baseline = forced_cpu
        .transcribe(
            pcm_input(dir.path(), "audio/jfk.wav"),
            JobContext::new(JobId::new()),
        )
        .await
        .expect("cpu baseline");

    // Ask for a GPU, but let the probe refuse: the job must still complete,
    // on the CPU, with a stated reason (F2.5 acceptance).
    let fallback_engine = WhisperEngine::new(WhisperConfig::new(model, "ggml-tiny.en"))
        .with_backend_probe(Arc::new(NoGpuProbe));
    let selection = fallback_engine.resolve_backend().expect("cpu baseline");
    assert_eq!(selection.chosen, Backend::Cpu);
    assert!(selection
        .fallback_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("forced unavailable by the fixture")));

    let fallback = fallback_engine
        .transcribe(
            pcm_input(dir.path(), "audio/jfk.wav"),
            JobContext::new(JobId::new()),
        )
        .await
        .expect("gpu fallback must still complete");

    assert_eq!(
        baseline.plain_text(),
        fallback.plain_text(),
        "CPU fallback produced different content"
    );
    assert_eq!(fallback.backend.as_deref(), Some("cpu"));
    assert!(
        fallback.engine_params.contains_key("backend_fallback"),
        "the fallback reason must be recorded in revision metadata"
    );
}

#[tokio::test]
async fn silence_does_not_produce_unflagged_text() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = cpu_engine(model);

    // Ten seconds of digital silence. whisper may emit nothing, or it may
    // hallucinate; what it must never do is emit unflagged text over measured
    // non-speech (§8.6A).
    let frames = 10 * WHISPER_SAMPLE_RATE_HZ as usize;
    let bytes: Vec<u8> = std::iter::repeat_n(0.0f32, frames)
        .flat_map(f32::to_le_bytes)
        .collect();
    let path = dir.path().join("silence.pcm");
    std::fs::write(&path, &bytes).expect("write");

    let input = SttInput {
        media_path: path,
        media_sha256: Sha256Digest::of(&bytes),
        byte_length: bytes.len() as u64,
        format: MediaFormat::new("pcm", "f32le"),
        duration_ms: Some(10_000),
        locale_hint: None,
        sample_rate_hz: WHISPER_SAMPLE_RATE_HZ,
        channels: 1,
        source_sample_offset: 0,
        normalization_profile: None,
        chunking_strategy: ChunkingStrategy::SingleChunk,
    };

    let draft = engine
        .transcribe(input, JobContext::new(JobId::new()))
        .await
        .expect("silence must transcribe, not error");

    let spoken: Vec<&str> = draft
        .segments
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect();
    if !spoken.is_empty() {
        assert_ne!(
            draft.annotations.len(),
            0,
            "text was produced over measured silence without any annotation: {spoken:?}"
        );
        // The text itself must survive: flagging, never deleting.
        assert_eq!(
            draft
                .segments
                .iter()
                .filter(|s| !s.text.trim().is_empty())
                .count(),
            spoken.len()
        );
    }
}

#[tokio::test]
async fn capabilities_are_observed_from_the_model_file() {
    let model = model_or_skip!();
    let _gate = engine_gate();
    let engine = cpu_engine(model);
    let capabilities: SttCapabilities = engine.capabilities().await;

    assert_eq!(capabilities.offline, Tri::Yes);
    assert_eq!(capabilities.can_cancel, Tri::Yes);
    assert_eq!(capabilities.supports_segments, Tri::Yes);
    assert_eq!(capabilities.supports_diarization, Tri::No);
    // `ggml-tiny.en` is English-only, and that is read from the header rather
    // than guessed from the `.en` in its name.
    assert_eq!(capabilities.supports_language_detection, Tri::No);
    assert_eq!(
        capabilities.locales.get().map(Vec::len),
        Some(1),
        "an English-only model must not advertise every locale"
    );
    assert_eq!(capabilities.model_download_bytes, Known::Known(0));
    assert!(capabilities.is_selectable());

    let hardware = capabilities
        .hardware_requirements
        .get()
        .expect("hardware requirements");
    assert!(
        !hardware.requires_accelerator,
        "GPU must never be reported as a requirement"
    );
    assert!(hardware.accelerator_backends.contains(&"cpu".to_string()));
    assert!(hardware.min_ram_bytes > 0);
}

#[test]
fn the_audio_fixtures_validate_and_agree_on_content() {
    // No model needed: this is the audio pipeline on its own.
    let (wav, _, wav_frames) = canonical_pcm("audio/jfk.wav");
    let (mp3, _, mp3_frames) = canonical_pcm("audio/jfk.mp3");
    let (m4a, _, m4a_frames) = canonical_pcm("audio/jfk.m4a");

    assert!(wav_frames > 10 * u64::from(WHISPER_SAMPLE_RATE_HZ));
    // Lossy encoders pad; the derived lengths stay within a second of the WAV.
    for (label, frames) in [("mp3", mp3_frames), ("m4a", m4a_frames)] {
        assert!(
            frames.abs_diff(wav_frames) < u64::from(WHISPER_SAMPLE_RATE_HZ),
            "{label} length {frames} differs from wav {wav_frames} by more than a second"
        );
    }
    assert!(
        !wav.is_empty() && !mp3.is_empty() && !m4a.is_empty(),
        "expected wav.is_empty() && !mp3.is_empty() && !m4a to be non-empty"
    );
}

#[test]
fn canonical_pcm_is_byte_identical_across_runs() {
    let (first, first_sha, _) = canonical_pcm("audio/jfk.wav");
    let (second, second_sha, _) = canonical_pcm("audio/jfk.wav");
    assert_eq!(first, second);
    assert_eq!(first_sha, second_sha);
}
