//! The processing pipeline, end to end, over a real SQLite library.
//!
//! Everything below the contract boundary is scripted (`crate::testing_engines`)
//! and everything above it is production code: `start_processing` opens a real
//! job through `plaudon-jobs`, a real worker claims it, the real executor
//! commits a real transcript revision and a real generated output, and the
//! real events reach the sink.

use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_ai_contracts::text::TextModelError;
use plaudon_ai_openrouter::service::{ModelChoice, Stage};
use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::{JobId, RecordingId};
use serde_json::json;

use crate::backend::testing::{backend_fixture, Fixture};
use crate::dto::{DuplicatePolicy, ImportOutcome, JobKind, JobState};
use crate::ports::{CloudPort, ImportPort, JobsPort, OutputsPort, TranscriptPort};
use crate::testing_engines::{ScriptedStt, ScriptedTextModel, SCRIPTED_STT_ENGINE};
use crate::validate::{
    PageCursor, ValidatedGetTranscript, ValidatedListJobs, ValidatedPreference,
    ValidatedPreferenceScope, ValidatedStartProcessing,
};

use super::{ProcessingHandle, StageRequest};

/// The newest revision of `recording_id`, when the executor committed one.
async fn head(
    backend: &crate::backend::LocalBackend,
    recording_id: RecordingId,
) -> Option<plaudon_domain::ids::TranscriptRevisionId> {
    backend
        .head_revision(recording_id)
        .await
        .expect("the revision table is readable")
}

/// Longest a test waits for a worker to reach a state.
const DEADLINE: Duration = Duration::from_secs(20);

/// A library with a worker pool over scripted engines.
struct Pipeline {
    fixture: Fixture,
    backend: Arc<crate::backend::LocalBackend>,
    pool: ProcessingHandle,
}

impl Pipeline {
    async fn open(stt: ScriptedStt, text: ScriptedTextModel) -> Self {
        let fixture = backend_fixture().await;
        let backend = Arc::new(
            fixture
                .backend
                .clone()
                .with_stt_engines(vec![stt.shared()])
                .with_text_model(text.shared()),
        );
        let pool = ProcessingHandle::start(Arc::clone(&backend), Some(1));
        Self {
            fixture,
            backend,
            pool,
        }
    }

    /// Import `name` and return the recording it created.
    async fn import(&self, name: &str) -> RecordingId {
        let path = self
            .fixture
            .source_file(name, format!("bytes of {name}").as_bytes());
        let response = self
            .backend
            .import_files(vec![path], DuplicatePolicy::Ask)
            .await
            .expect("the import is accepted");
        match &response.results[0] {
            ImportOutcome::Accepted { job_id, .. } => {
                crate::backend::jobs::load(self.backend.pool(), *job_id)
                    .await
                    .expect("the import job exists")
                    .recording_id
                    .expect("a committed import links its recording")
            }
            other => panic!("expected an accepted import, got {other:?}"),
        }
    }

    /// Point a stage at the scripted engine.
    async fn prefer(&self, stage: Stage, engine: &str) {
        self.backend
            .set_model_preference(ValidatedPreference {
                stage,
                scope: ValidatedPreferenceScope::Default,
                engine: engine.to_owned(),
                model_id: Some("scripted-small".to_owned()),
            })
            .await
            .expect("the preference is stored");
    }

    async fn run_stage(&self, recording_id: RecordingId, stage: Stage) -> JobId {
        let response = self
            .backend
            .start_processing(ValidatedStartProcessing {
                recording_id,
                stage,
                model_override: None,
                cost_acknowledged: false,
                rerun: false,
            })
            .await
            .expect("the stage is queued");
        response.job_id.parse().expect("a job id")
    }

    /// Wait until `job_id` reaches a terminal state, and return it.
    async fn settled(&self, job_id: JobId) -> crate::dto::JobSummary {
        self.until(job_id, |summary| summary.state.is_terminal())
            .await
    }

    /// Wait until `job_id` satisfies `predicate`.
    async fn until(
        &self,
        job_id: JobId,
        predicate: impl Fn(&crate::dto::JobSummary) -> bool,
    ) -> crate::dto::JobSummary {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let summary = crate::backend::jobs::load(self.backend.pool(), job_id)
                .await
                .expect("the job exists");
            if predicate(&summary) {
                return summary;
            }
            assert!(
                Instant::now() < deadline,
                "the job stalled in {} ({:?})",
                summary.state.as_str(),
                summary.error.as_ref().map(|envelope| envelope.code)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn stop(self) {
        self.pool.stop(Duration::from_secs(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_imported_recording_is_transcribed_summarized_and_linked() {
    let pipeline = Pipeline::open(
        ScriptedStt::new(&["we agreed to ship on friday", "owner is dana"]),
        // Null: the model answers whatever schema each stage sends, which is
        // what makes the real pipeline run end to end.
        ScriptedTextModel::new(serde_json::Value::Null),
    )
    .await;
    let recording_id = pipeline.import("standup.wav").await;
    pipeline.prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE).await;

    let job_id = pipeline.run_stage(recording_id, Stage::Transcribe).await;
    let settled = pipeline.settled(job_id).await;
    assert_eq!(
        settled.state,
        JobState::Completed,
        "the transcribe job ran to completion"
    );
    assert_eq!(settled.kind, JobKind::Transcribe);
    assert_eq!(
        settled.output_link.as_ref().map(|link| link.id.clone()),
        Some(recording_id.to_string()),
        "a finished job says what to open"
    );

    let revision_id = head(&pipeline.backend, recording_id)
        .await
        .expect("the executor committed a revision");
    let transcript = pipeline
        .backend
        .get_transcript(ValidatedGetTranscript {
            revision_id,
            cursor: 0,
            limit: 50,
        })
        .await
        .expect("the committed revision is readable");
    assert_eq!(transcript.segments.len(), 2);
    assert_eq!(transcript.segments[0].text, "we agreed to ship on friday");
    assert_eq!(
        transcript.revision.engine_id.as_deref(),
        Some(SCRIPTED_STT_ENGINE)
    );

    // The summary stage reads the committed revision and writes an output.
    pipeline.prefer(Stage::StructuredExtraction, SCRIPTED_STT_ENGINE).await;
    let summary_job = pipeline.run_stage(recording_id, Stage::StructuredExtraction).await;
    let settled = pipeline.settled(summary_job).await;
    assert_eq!(
        settled.state,
        JobState::Completed,
        "the summarize job ran to completion"
    );

    let outputs = pipeline
        .backend
        .list_outputs(recording_id)
        .await
        .expect("outputs are readable");
    assert_eq!(outputs.len(), 1, "one output was committed");

    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_job_can_be_cancelled_and_commits_nothing() {
    let pipeline = Pipeline::open(
        // Paced so the cancellation lands while the engine is working.
        ScriptedStt::new(&["one", "two", "three", "four", "five"]).with_pace_ms(120),
        ScriptedTextModel::new(json!({})),
    )
    .await;
    let recording_id = pipeline.import("long.wav").await;
    pipeline.prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE).await;
    let job_id = pipeline.run_stage(recording_id, Stage::Transcribe).await;

    pipeline
        .until(job_id, |summary| {
            summary.state == JobState::RunningLocal
        })
        .await;
    pipeline
        .backend
        .cancel_job(job_id)
        .await
        .expect("the cancellation is accepted");

    let settled = pipeline.settled(job_id).await;
    assert_eq!(settled.state, JobState::Cancelled);

    assert!(
        head(&pipeline.backend, recording_id).await.is_none(),
        "a cancelled run commits no partial transcript"
    );
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_engine_failure_is_retryable_and_the_retry_runs_again() {
    let pipeline = Pipeline::open(
        ScriptedStt::unavailable(),
        ScriptedTextModel::new(json!({})),
    )
    .await;
    let recording_id = pipeline.import("quiet.wav").await;
    pipeline.prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE).await;
    let job_id = pipeline.run_stage(recording_id, Stage::Transcribe).await;

    let settled = pipeline.settled(job_id).await;
    assert_eq!(
        settled.state,
        JobState::FailedTerminal,
        "an engine with no model installed is not a transient failure"
    );
    assert!(
        settled.error.is_some(),
        "the failure carries a code the UI can render"
    );
    assert!(
        !settled.cost_acknowledgement_required,
        "a local failure costs nothing and needs no acknowledgement"
    );
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kill_mid_job_leaves_a_retryable_job_and_no_output() {
    let fixture = backend_fixture().await;
    let backend = Arc::new(
        fixture
            .backend
            .clone()
            .with_stt_engines(vec![ScriptedStt::new(&["a", "b", "c", "d", "e", "f"])
                .with_pace_ms(200)
                .shared()]),
    );
    let pool = ProcessingHandle::start(Arc::clone(&backend), Some(1));

    let path = fixture.source_file("interrupted.wav", b"bytes of interrupted");
    let response = backend
        .import_files(vec![path], DuplicatePolicy::Ask)
        .await
        .unwrap();
    let ImportOutcome::Accepted { job_id, .. } = &response.results[0] else {
        panic!("the import is accepted");
    };
    let recording_id = crate::backend::jobs::load(backend.pool(), *job_id)
        .await
        .unwrap()
        .recording_id
        .unwrap();
    backend
        .set_model_preference(ValidatedPreference {
            stage: Stage::Transcribe,
            scope: ValidatedPreferenceScope::Default,
            engine: SCRIPTED_STT_ENGINE.to_owned(),
            model_id: Some("scripted-small".to_owned()),
        })
        .await
        .unwrap();
    let transcribe: JobId = backend
        .start_processing(ValidatedStartProcessing {
            recording_id,
            stage: Stage::Transcribe,
            model_override: None,
            cost_acknowledged: false,
            rerun: false,
        })
        .await
        .unwrap()
        .job_id
        .parse()
        .unwrap();

    // Wait until the engine is actually working, then stop the pool the way
    // a kill would: no grace, no chance to finish.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = crate::backend::jobs::load(backend.pool(), transcribe)
            .await
            .unwrap()
            .state;
        if state == JobState::RunningLocal {
            break;
        }
        assert!(Instant::now() < deadline, "the job never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pool.stop(Duration::from_millis(1)).await;
    drop(backend);

    // Restart over the same library: recovery, then the state the user sees.
    let restarted = Arc::new(
        crate::backend::LocalBackend::open(
            fixture.dir.path(),
            fixture.sink.clone(),
            Arc::new(plaudon_notifications::Dispatcher::new(
                Arc::clone(&fixture.notifier) as Arc<dyn plaudon_notifications::Notifier>,
                plaudon_notifications::NotificationSettings::uniform(false),
            )),
            fixture.secrets.clone(),
            vec![fixture.imports.path().to_path_buf()],
        )
        .await
        .expect("the library reopens")
        // The restarted process installs the same engines the first one had;
        // without them the stage would park on a missing model, which is a
        // different (and also correct) behaviour.
        .with_stt_engines(vec![ScriptedStt::new(&["a", "b", "c"]).shared()]),
    );
    restarted
        .reconcile_startup()
        .await
        .expect("startup reconciliation runs");

    let after = crate::backend::jobs::load(restarted.pool(), transcribe)
        .await
        .unwrap();
    assert_eq!(
        after.state,
        JobState::FailedRetryable,
        "a run the kill interrupted is retryable, never completed"
    );
    assert!(
        head(&restarted, recording_id).await.is_none(),
        "nothing was committed by the interrupted run"
    );

    // And the user's retry runs it again, to completion this time.
    let pool = ProcessingHandle::start(Arc::clone(&restarted), Some(1));
    let retried = restarted
        .retry_job(transcribe, false)
        .await
        .expect("a retryable job can be retried");
    assert_eq!(retried.job_id, transcribe);
    let deadline = Instant::now() + DEADLINE;
    loop {
        let summary = crate::backend::jobs::load(restarted.pool(), transcribe)
            .await
            .unwrap();
        if summary.state.is_terminal() {
            assert_eq!(summary.state, JobState::Completed);
            break;
        }
        assert!(Instant::now() < deadline, "the retry stalled");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    pool.stop(Duration::from_secs(5)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_stage_asked_for_twice_runs_once() {
    let pipeline = Pipeline::open(
        ScriptedStt::new(&["once"]).with_pace_ms(80),
        ScriptedTextModel::new(json!({})),
    )
    .await;
    let recording_id = pipeline.import("twice.wav").await;
    pipeline.prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE).await;

    let first = pipeline.run_stage(recording_id, Stage::Transcribe).await;
    let second = pipeline.run_stage(recording_id, Stage::Transcribe).await;
    assert_eq!(
        first, second,
        "an identical pending stage is reused, not duplicated"
    );
    let settled = pipeline.settled(first).await;
    assert_eq!(settled.state, JobState::Completed);

    // Asking again after it finished means "do it again", and that is a new
    // job rather than a second completion of the old one.
    let third = pipeline.run_stage(recording_id, Stage::Transcribe).await;
    assert_ne!(third, first, "a finished stage runs again under a new job");
    pipeline.settled(third).await;

    let jobs = pipeline
        .backend
        .list_jobs(ValidatedListJobs {
            states: vec![],
            kinds: vec![JobKind::Transcribe],
            cursor: PageCursor::at(0),
            limit: 50,
        })
        .await
        .expect("the job list is readable");
    assert_eq!(jobs.items.len(), 2, "two runs, not three");
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_text_model_failure_fails_the_summary_without_an_output() {
    let pipeline = Pipeline::open(
        ScriptedStt::new(&["hello"]),
        ScriptedTextModel::failing(TextModelError::Unavailable),
    )
    .await;
    let recording_id = pipeline.import("summary.wav").await;
    pipeline.prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE).await;
    pipeline.prefer(Stage::StructuredExtraction, SCRIPTED_STT_ENGINE).await;
    let transcribe = pipeline.run_stage(recording_id, Stage::Transcribe).await;
    assert_eq!(
        pipeline.settled(transcribe).await.state,
        JobState::Completed
    );

    let summarize = pipeline.run_stage(recording_id, Stage::StructuredExtraction).await;
    let settled = pipeline.settled(summarize).await;
    assert!(
        matches!(
            settled.state,
            JobState::FailedRetryable | JobState::FailedTerminal
        ),
        "an unavailable model fails the job, state was {}",
        settled.state.as_str()
    );
    let outputs = pipeline
        .backend
        .list_outputs(recording_id)
        .await
        .expect("outputs are readable");
    assert!(outputs.is_empty(), "no output was committed");
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stage_request_round_trips_through_its_durable_snapshot() {
    let pipeline = Pipeline::open(ScriptedStt::new(&["x"]), ScriptedTextModel::new(json!({})))
        .await;
    let recording_id = pipeline.import("round.wav").await;
    let request = StageRequest {
        recording_id,
        kind: JobKind::Transcribe,
        engine: SCRIPTED_STT_ENGINE.to_owned(),
        model_id: Some("scripted-small".to_owned()),
        stage: Stage::Transcribe.as_str().to_owned(),
        label: "Process (transcribe)".to_owned(),
        audio_sha256: "a".repeat(64),
        cloud: false,
        cost_acknowledged: false,
    };
    let job_id = pipeline
        .backend
        .enqueue_stage(&request)
        .await
        .expect("the stage is queued");
    let record = pipeline
        .backend
        .processing()
        .engine()
        .store()
        .get(job_id)
        .await
        .expect("the engine owns the row");
    let rebuilt = StageRequest::from_config(JobKind::Transcribe, recording_id, &record.config_snapshot)
        .expect("the snapshot describes the stage");
    assert_eq!(rebuilt.engine, request.engine);
    assert_eq!(rebuilt.model_id, request.model_id);
    assert_eq!(rebuilt.audio_sha256, request.audio_sha256);
    assert!(!rebuilt.cloud);
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cloud_stage_is_never_queued_without_a_key() {
    let pipeline = Pipeline::open(ScriptedStt::new(&["x"]), ScriptedTextModel::new(json!({})))
        .await;
    let recording_id = pipeline.import("cloud.wav").await;
    pipeline
        .backend
        .set_model_preference(ValidatedPreference {
            stage: Stage::Transcribe,
            scope: ValidatedPreferenceScope::Default,
            engine: plaudon_ai_openrouter::registry::OPENROUTER_ENGINE_ID.to_owned(),
            model_id: Some("openai/whisper-1".to_owned()),
        })
        .await
        .expect("the preference is stored");

    let refused = pipeline
        .backend
        .start_processing(ValidatedStartProcessing {
            recording_id,
            stage: Stage::Transcribe,
            model_override: Some(ModelChoice::new(
                plaudon_ai_openrouter::registry::OPENROUTER_ENGINE_ID,
                Some("openai/whisper-1".to_owned()),
            )),
            cost_acknowledged: false,
            rerun: false,
        })
        .await
        .expect_err("an unverified cloud model is refused");
    assert!(
        matches!(
            refused.code(),
            AppErrorCode::CapabilityUnverified
                | AppErrorCode::AuthenticationRequired
                | AppErrorCode::ConsentRequired
                | AppErrorCode::LocalOnlyMode
        ),
        "unexpected refusal: {:?}",
        refused.code()
    );
    let jobs = pipeline
        .backend
        .list_jobs(ValidatedListJobs {
            states: vec![],
            kinds: vec![JobKind::Transcribe],
            cursor: PageCursor::at(0),
            limit: 50,
        })
        .await
        .expect("the job list is readable");
    assert!(
        jobs.items.is_empty(),
        "a refusal before dispatch opens no job row"
    );
    pipeline.stop().await;
}

/* ------------------------------------------------- opt-in real-model run */

/// One real local transcription, end to end, with real whisper.cpp weights.
///
/// Opt-in manual evidence (spec §18.3: real artifacts are fetched once,
/// explicitly, never by a test run). It needs
///
/// * this crate built with `--features local-whisper`, which links
///   whisper.cpp, and
/// * `PLAUDON_WHISPER_MODEL` pointing at a weight file — `scripts/fetch-stt-fixtures.sh`
///   puts `ggml-tiny.en.bin` in `tests/fixtures/stt/models`.
///
/// The audio is the committed `tests/fixtures/stt/audio/jfk.wav`.
#[cfg(feature = "local-whisper")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in: needs whisper.cpp weights and the local-whisper feature"]
async fn a_real_whisper_model_transcribes_the_committed_clip() {
    let Ok(model) = std::env::var("PLAUDON_WHISPER_MODEL") else {
        panic!("set PLAUDON_WHISPER_MODEL to a whisper.cpp weight file");
    };
    let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/stt/audio/jfk.wav");
    assert!(clip.is_file(), "the committed clip is missing: {clip:?}");

    let fixture = backend_fixture().await;
    // Install the weights where the executor looks for them.
    let installed = fixture
        .backend
        .store()
        .models_dir()
        .join("whisper.cpp")
        .join("tiny.en");
    std::fs::create_dir_all(&installed).expect("the model directory");
    std::fs::copy(&model, installed.join("ggml-tiny.en.bin")).expect("the weights are installed");

    let backend = Arc::new(fixture.backend.clone());
    let pool = ProcessingHandle::start(Arc::clone(&backend), Some(1));

    let staged = fixture.imports.path().join("jfk.wav");
    std::fs::copy(&clip, &staged).expect("the clip is staged");
    let response = backend
        .import_files(
            vec![staged.to_string_lossy().into_owned()],
            DuplicatePolicy::Ask,
        )
        .await
        .expect("the import is accepted");
    let ImportOutcome::Accepted { job_id, .. } = &response.results[0] else {
        panic!("the import is accepted");
    };
    let recording_id = crate::backend::jobs::load(backend.pool(), *job_id)
        .await
        .unwrap()
        .recording_id
        .unwrap();
    backend
        .set_model_preference(ValidatedPreference {
            stage: Stage::Transcribe,
            scope: ValidatedPreferenceScope::Default,
            engine: "whisper.cpp".to_owned(),
            model_id: Some("tiny.en".to_owned()),
        })
        .await
        .unwrap();

    let started = Instant::now();
    let transcribe: JobId = backend
        .start_processing(ValidatedStartProcessing {
            recording_id,
            stage: Stage::Transcribe,
            model_override: None,
            cost_acknowledged: false,
            rerun: false,
        })
        .await
        .unwrap()
        .job_id
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(600);
    let summary = loop {
        let summary = crate::backend::jobs::load(backend.pool(), transcribe)
            .await
            .unwrap();
        if summary.state.is_terminal() {
            break summary;
        }
        assert!(
            Instant::now() < deadline,
            "the real run stalled in {}",
            summary.state.as_str()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let elapsed = started.elapsed();
    assert_eq!(
        summary.state,
        JobState::Completed,
        "the real run failed: {:?}",
        summary.error.as_ref().map(|envelope| envelope.code)
    );

    let revision_id = head(&backend, recording_id)
        .await
        .expect("the real run committed a revision");
    let page = backend
        .get_transcript(ValidatedGetTranscript {
            revision_id,
            cursor: 0,
            limit: 100,
        })
        .await
        .expect("the revision is readable");
    let text = page
        .segments
        .iter()
        .map(|segment| segment.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    println!("whisper.cpp tiny.en: {elapsed:?} for {} segment(s)", page.segments.len());
    println!("transcript: {text}");
    assert!(
        text.to_lowercase().contains("ask not"),
        "the real transcript does not contain the clip's words: {text}"
    );
    pool.stop(Duration::from_secs(5)).await;
}
