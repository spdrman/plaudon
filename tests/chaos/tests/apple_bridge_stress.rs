//! Rust-side stress and interruption of the Apple bridge (task H1.5).
//!
//! `native/apple-bridge/tests/abi_stress.c` stresses the C boundary under
//! AddressSanitizer, where the failures worth catching are memory failures.
//! This suite stresses the half above it — the handle lifetimes, the
//! cancellation plumbing, the progress channel and the adapters — where the
//! failures worth catching are a leaked handle, a lost cancellation, a
//! partial transcript surviving a cancel, or a task that never finishes.
//!
//! Hermetic and deterministic: a [`FakeBridge`] answers every call, nothing
//! touches the network, a provider or an Apple framework, and the whole suite
//! runs on Windows. Spec 18.4 asks for ownership and cancellation under
//! stress; spec 6.6 asks for the bridge to be stressed before it ships.
//!
//! Every test here has a wall-clock ceiling. A stress test that can hang is
//! worse than no stress test, because CI reports it as a timeout with no
//! information.

use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use plaudon_ai_apple::bridge::{failure_document, success_document, ScriptedCall};
use plaudon_ai_apple::{
    AppleBridge, AppleSpeechToText, AppleTextModel, BridgeOp, CancelGuard, FakeBridge, ProbeCache,
};
use plaudon_ai_contracts::capabilities::MediaFormat;
use plaudon_ai_contracts::job::{JobContext, Progress, ProgressSink};
use plaudon_ai_contracts::stt::{SpeechToText, SttError, SttInput};
use plaudon_ai_contracts::text::{StructuredRequest, TextModel, TextModelError};
use plaudon_domain::ids::JobId;
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::Sha256Digest;

/// Ceiling for every scenario. Generous for the work, finite for CI.
const CEILING: Duration = Duration::from_secs(60);

fn speech_probe() -> serde_json::Value {
    serde_json::json!({
        "available": true,
        "supported_locales": ["en_US"],
        "installed_locales": ["en_US"],
        "resolved_locale": "en_US",
        "asset_status": "installed",
        "locale_installed": true,
        "supports_on_device": true,
        "is_transcriber": true,
        "max_concurrent_locales": 3,
        "reserved_locales": [],
        "accepted_formats": [{ "sample_rate_hz": 16000.0, "channels": 1 }],
        "supports_word_timestamps": true,
        "os_build": "26A434",
        "os_version": "27.0.1",
        "hardware": "Apple M4 Pro",
    })
}

fn text_probe() -> serde_json::Value {
    serde_json::json!({
        "available": true,
        "is_transcriber": false,
        "supported_languages": ["en-Latn-US"],
        "supports_structured_output": true,
        "max_context_tokens": null,
        "os_build": "26A434",
    })
}

fn transcription() -> serde_json::Value {
    serde_json::json!({
        "text": "hello world",
        "segments": [{
            "seq_index": 0,
            "start_ms": 0,
            "end_ms": 1500,
            "text": "hello world",
            "confidence": 0.8,
            "words": [{ "text": "hello", "start_ms": 0, "end_ms": 400 }],
        }],
        "alignment_origin": "native",
        "locale": "en_US",
        "duration_ms": 1500,
        "final": true,
    })
}

/// A bridge whose long calls take a little time, so cancellation has a window.
fn slow_bridge(work: Duration) -> Arc<FakeBridge> {
    Arc::new(FakeBridge::new().with_default(move |op| match op {
        BridgeOp::ProbeSpeech => success_document(op, speech_probe()),
        BridgeOp::ProbeFoundationModel => success_document(op, text_probe()),
        BridgeOp::TranscribeFile => {
            std::thread::sleep(work);
            success_document(op, transcription())
        }
        BridgeOp::GenerateText => {
            std::thread::sleep(work);
            success_document(
                op,
                serde_json::json!({ "text": "{\"summary\":\"ok\"}", "structured": true }),
            )
        }
        BridgeOp::InstallSpeechAssets => {
            failure_document(op, "asset_download_failed", Some("stress"))
        }
    }))
}

fn stt_input(path: &std::path::Path) -> SttInput {
    let mut input = SttInput::whole_file(
        path.to_path_buf(),
        Sha256Digest::from_bytes([9u8; 32]),
        1024,
        MediaFormat::new("wav", "pcm_s16le"),
        16_000,
        1,
    );
    input.locale_hint = Some(LanguageTag::parse("en-US").unwrap());
    input
}

fn schema_request() -> StructuredRequest {
    StructuredRequest {
        prompt_version: "stress@1".into(),
        prompt: "TRANSCRIPT: hello".into(),
        schema_version: "stress@1".into(),
        output_schema: serde_json::json!({
            "type": "object",
            "required": ["summary"],
            "properties": { "summary": { "type": "string" } },
        }),
        max_output_tokens: Some(64),
    }
}

/// Counts progress reports without ever blocking the engine.
#[derive(Debug, Default)]
struct CountingSink(AtomicU32);

impl ProgressSink for CountingSink {
    fn report(&self, _progress: Progress) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Run `body` under a wall-clock ceiling, failing rather than hanging.
async fn bounded<F: Future>(what: &str, body: F) -> F::Output {
    tokio::time::timeout(CEILING, body)
        .await
        .unwrap_or_else(|elapsed| panic!("{what} did not finish inside {CEILING:?}: {elapsed}"))
}

/// Thousands of allocate/cancel/release cycles across tasks.
///
/// The invariant is that the handle registry returns to exactly where it
/// started. A per-cycle leak of one handle is invisible in a unit test and
/// unmistakable after fifty thousand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handle_churn_storm_converges_to_a_released_state() {
    let bridge = Arc::new(FakeBridge::new());
    let baseline = bridge.live_token_count();

    let mut tasks = Vec::new();
    for worker in 0..8u64 {
        let handle: Arc<dyn AppleBridge> = bridge.clone();
        tasks.push(tokio::spawn(async move {
            for index in 0..6_000u64 {
                let guard = CancelGuard::new(handle.clone());
                if (worker + index) % 3 == 0 {
                    guard.cancel();
                    guard.cancel();
                }
                // Dropping is the only release path, so a missed drop shows
                // up in the final count.
                drop(guard);
            }
        }));
    }
    bounded("the handle churn storm", async {
        for task in tasks {
            task.await.expect("no worker panics");
        }
    })
    .await;

    assert_eq!(
        bridge.live_token_count(),
        baseline,
        "48000 allocate/release cycles leaked handles"
    );
}

/// Cancelling the same job from many tasks while it runs.
///
/// The call must still return exactly one answer, the handle must be
/// released, and no transcript may survive the cancellation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_storm_never_yields_a_transcript_and_never_leaks() {
    let bridge = slow_bridge(Duration::from_millis(40));
    let handle: Arc<dyn AppleBridge> = bridge.clone();
    let cache = Arc::new(ProbeCache::new(handle.clone()));
    let engine = AppleSpeechToText::new(handle, cache);
    let file = tempfile::NamedTempFile::new().unwrap();
    let baseline = bridge.live_token_count();

    for round in 0..24 {
        let ctx = JobContext::new(JobId::new());
        let mut spinners = Vec::new();
        for _ in 0..6 {
            let cancel = ctx.cancel.clone();
            spinners.push(tokio::spawn(async move {
                for _ in 0..50 {
                    cancel.cancel();
                    tokio::task::yield_now().await;
                }
            }));
        }
        let outcome = bounded(
            "a cancel-storm transcription",
            engine.transcribe(stt_input(file.path()), ctx),
        )
        .await;
        for spinner in spinners {
            spinner.await.expect("no spinner panics");
        }
        assert_eq!(
            outcome,
            Err(SttError::Cancelled),
            "round {round} produced a result despite cancellation"
        );
    }

    assert_eq!(
        bridge.live_token_count(),
        baseline,
        "the cancel storm leaked handles"
    );
}

/// Cancelling before the call starts must not reach the bridge at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_before_start_never_dispatches() {
    let bridge = slow_bridge(Duration::ZERO);
    let handle: Arc<dyn AppleBridge> = bridge.clone();
    let cache = Arc::new(ProbeCache::new(handle.clone()));
    let engine = AppleSpeechToText::new(handle, cache);
    let file = tempfile::NamedTempFile::new().unwrap();

    for _ in 0..200 {
        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();
        assert_eq!(
            bounded(
                "a pre-cancelled transcription",
                engine.transcribe(stt_input(file.path()), ctx)
            )
            .await,
            Err(SttError::Cancelled)
        );
    }
    assert_eq!(
        bridge.calls_for(BridgeOp::TranscribeFile).len(),
        0,
        "a pre-cancelled job reached the bridge"
    );
}

/// Many concurrent transcriptions against one adapter.
///
/// The platform publishes a concurrency ceiling and the adapter enforces it,
/// so the test is that every job completes rather than that they run at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_jobs_all_complete_under_the_concurrency_ceiling() {
    let bridge = slow_bridge(Duration::from_millis(5));
    let handle: Arc<dyn AppleBridge> = bridge.clone();
    let cache = Arc::new(ProbeCache::new(handle.clone()));
    let engine = Arc::new(AppleSpeechToText::new(handle, cache).with_concurrency(3));
    let file = Arc::new(tempfile::NamedTempFile::new().unwrap());

    let sink = Arc::new(CountingSink::default());
    let mut tasks = Vec::new();
    for _ in 0..40 {
        let engine = engine.clone();
        let file = file.clone();
        let sink = sink.clone();
        tasks.push(tokio::spawn(async move {
            let ctx = JobContext::new(JobId::new()).with_progress(sink);
            engine.transcribe(stt_input(file.path()), ctx).await
        }));
    }
    let results = bounded("forty concurrent transcriptions", async {
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.expect("no job panics"));
        }
        results
    })
    .await;

    assert_eq!(results.len(), 40);
    for result in &results {
        let draft = result.as_ref().expect("every job completes");
        assert_eq!(draft.segments.len(), 1);
    }
    assert_eq!(
        bridge.live_token_count(),
        0,
        "concurrent jobs leaked handles"
    );
}

/// Probe churn from many tasks against one cache.
///
/// The cache must stay coherent and must not call the bridge once per ask;
/// a library view renders a hundred rows and each one asks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_churn_is_coherent_and_does_not_storm_the_bridge() {
    let bridge = Arc::new(FakeBridge::new().with_default(|op| match op {
        BridgeOp::ProbeSpeech => success_document(op, speech_probe()),
        _ => success_document(op, text_probe()),
    }));
    let cache = Arc::new(ProbeCache::new(bridge.clone()));

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let cache = cache.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            for _ in 0..200 {
                let (availability, _) = cache.availability(Some("en-US"));
                assert!(availability.speech.is_ok());
            }
        }));
    }
    bounded("probe churn", async {
        for task in tasks {
            task.await.expect("no prober panics");
        }
    })
    .await;

    let calls = bridge.calls_for(BridgeOp::ProbeSpeech).len();
    assert!(
        calls <= 8,
        "1600 cached asks produced {calls} bridge calls; the cache is not holding"
    );
}

/// A bridge that fails at the boundary on every call.
///
/// The adapters must report a typed error and release their handles rather
/// than retrying forever or panicking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bridge_that_always_fails_degrades_without_leaking() {
    let bridge = Arc::new(FakeBridge::new().with_default(|op| match op {
        BridgeOp::ProbeSpeech => success_document(op, speech_probe()),
        BridgeOp::ProbeFoundationModel => success_document(op, text_probe()),
        _ => "}{ not a document".to_owned(),
    }));
    let handle: Arc<dyn AppleBridge> = bridge.clone();
    let cache = Arc::new(ProbeCache::new(handle.clone()));
    let stt = AppleSpeechToText::new(handle.clone(), cache.clone());
    let text = AppleTextModel::new(handle, cache);
    let file = tempfile::NamedTempFile::new().unwrap();

    for _ in 0..100 {
        assert_eq!(
            bounded(
                "a failing transcription",
                stt.transcribe(stt_input(file.path()), JobContext::new(JobId::new()))
            )
            .await,
            Err(SttError::EngineFailure)
        );
        assert_eq!(
            bounded(
                "a failing generation",
                text.generate_structured(schema_request(), JobContext::new(JobId::new()))
            )
            .await,
            Err(TextModelError::EngineFailure)
        );
    }
    assert_eq!(bridge.live_token_count(), 0, "failing calls leaked handles");
}

/// A flood of progress events must not block or lose the result.
///
/// Spec 6.5: a dropped update always beats a stalled transcription, so the
/// test is that the transcript arrives, not that every event does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_progress_flood_never_stalls_the_engine() {
    let documents: Vec<String> = (0..5_000)
        .map(|index| {
            format!(
                r#"{{"envelope_version":1,"op":"transcribe_file","phase":"transcribe","fraction":{:.4}}}"#,
                f64::from(index) / 5000.0
            )
        })
        .collect();
    let bridge = Arc::new(
        FakeBridge::new()
            .script(
                BridgeOp::TranscribeFile,
                ScriptedCall::ProgressThenReply {
                    progress: documents,
                    reply: success_document(BridgeOp::TranscribeFile, transcription()),
                },
            )
            .with_default(|op| match op {
                BridgeOp::ProbeSpeech => success_document(op, speech_probe()),
                _ => success_document(op, text_probe()),
            }),
    );
    let handle: Arc<dyn AppleBridge> = bridge.clone();
    let cache = Arc::new(ProbeCache::new(handle.clone()));
    let engine = AppleSpeechToText::new(handle, cache);
    let file = tempfile::NamedTempFile::new().unwrap();

    let sink = Arc::new(CountingSink::default());
    let ctx = JobContext::new(JobId::new()).with_progress(sink.clone());
    let draft = bounded(
        "a transcription under a progress flood",
        engine.transcribe(stt_input(file.path()), ctx),
    )
    .await
    .expect("the result survives the flood");

    assert_eq!(draft.segments.len(), 1);
    assert_eq!(bridge.live_token_count(), 0);
}

/// Interrupting at every point of a long run.
///
/// Stands in for sleep/resume and process interruption (spec 18.4): the job
/// is cancelled after a varying delay, and the only two acceptable outcomes
/// are a complete draft or a typed cancellation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupting_at_arbitrary_points_yields_either_a_draft_or_a_cancellation() {
    let bridge = slow_bridge(Duration::from_millis(20));
    let handle: Arc<dyn AppleBridge> = bridge.clone();
    let cache = Arc::new(ProbeCache::new(handle.clone()));
    let engine = Arc::new(AppleSpeechToText::new(handle, cache));
    let file = Arc::new(tempfile::NamedTempFile::new().unwrap());

    let mut cancelled = 0;
    let mut completed = 0;
    for delay_micros in (0..40_000).step_by(2_000) {
        let ctx = JobContext::new(JobId::new());
        let cancel = ctx.cancel.clone();
        let interrupter = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_micros(delay_micros)).await;
            cancel.cancel();
        });
        let outcome = bounded(
            "an interrupted transcription",
            engine.transcribe(stt_input(file.path()), ctx),
        )
        .await;
        interrupter.await.expect("the interrupter does not panic");
        match outcome {
            Ok(draft) => {
                completed += 1;
                assert_eq!(draft.segments.len(), 1, "a partial draft escaped");
            }
            Err(SttError::Cancelled) => cancelled += 1,
            Err(other) => panic!("unexpected outcome at {delay_micros}us: {other:?}"),
        }
    }

    assert!(
        cancelled > 0,
        "no interruption landed; the test proved nothing"
    );
    assert_eq!(cancelled + completed, 20);
    assert_eq!(bridge.live_token_count(), 0, "interruptions leaked handles");
}
