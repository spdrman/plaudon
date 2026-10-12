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
    /// `None` when nothing drains the queue, so a queued job stays queued.
    pool: Option<ProcessingHandle>,
}

impl Pipeline {
    async fn open(stt: ScriptedStt, text: ScriptedTextModel) -> Self {
        Self::with_workers(stt, text, Some(1)).await
    }

    /// A pipeline whose queue nothing claims.
    async fn idle(stt: ScriptedStt, text: ScriptedTextModel) -> Self {
        Self::with_workers(stt, text, None).await
    }

    async fn with_workers(
        stt: ScriptedStt,
        text: ScriptedTextModel,
        workers: Option<usize>,
    ) -> Self {
        let fixture = backend_fixture().await;
        let backend = Arc::new(
            fixture
                .backend
                .clone()
                .with_stt_engines(vec![stt.shared()])
                .with_text_model(text.shared()),
        );
        let pool = workers.map(|count| ProcessingHandle::start(&backend, Some(count)));
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
        if let Some(pool) = self.pool {
            pool.stop(Duration::from_secs(5)).await;
        }
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
    pipeline
        .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
        .await;

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
    pipeline
        .prefer(Stage::StructuredExtraction, SCRIPTED_STT_ENGINE)
        .await;
    let summary_job = pipeline
        .run_stage(recording_id, Stage::StructuredExtraction)
        .await;
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
    pipeline
        .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
        .await;
    let job_id = pipeline.run_stage(recording_id, Stage::Transcribe).await;

    pipeline
        .until(job_id, |summary| summary.state == JobState::RunningLocal)
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
    pipeline
        .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
        .await;
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
    let backend = Arc::new(fixture.backend.clone().with_stt_engines(
        vec![ScriptedStt::new(&["a", "b", "c", "d", "e", "f"])
                .with_pace_ms(200)
                .shared()],
    ));
    let pool = ProcessingHandle::start(&backend, Some(1));

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
    let pool = ProcessingHandle::start(&restarted, Some(1));
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
    pipeline
        .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
        .await;

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
    pipeline
        .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
        .await;
    pipeline
        .prefer(Stage::StructuredExtraction, SCRIPTED_STT_ENGINE)
        .await;
    let transcribe = pipeline.run_stage(recording_id, Stage::Transcribe).await;
    assert_eq!(
        pipeline.settled(transcribe).await.state,
        JobState::Completed
    );

    let summarize = pipeline
        .run_stage(recording_id, Stage::StructuredExtraction)
        .await;
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
    let pipeline =
        Pipeline::open(ScriptedStt::new(&["x"]), ScriptedTextModel::new(json!({}))).await;
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
        transcript_revision: None,
        model_version: None,
        normalization: std::collections::BTreeMap::new(),
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
    let rebuilt =
        StageRequest::from_config(JobKind::Transcribe, recording_id, &record.config_snapshot)
            .expect("the snapshot describes the stage");
    assert_eq!(rebuilt.engine, request.engine);
    assert_eq!(rebuilt.model_id, request.model_id);
    assert_eq!(rebuilt.audio_sha256, request.audio_sha256);
    assert!(!rebuilt.cloud);
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cloud_stage_is_never_queued_without_a_key() {
    let pipeline =
        Pipeline::open(ScriptedStt::new(&["x"]), ScriptedTextModel::new(json!({}))).await;
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

/* ------------------------------------------------------ writer contention */

/// Ask for 200 stages concurrently while workers claim, run and commit.
///
/// Two connection pools write this one SQLite file: the job engine's single
/// writer and the application's pool, which commits transcripts, steps and
/// events for the very jobs the engine is opening. Queueing a stage must
/// never fail because the other side happened to commit first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_stage_requests_survive_writer_contention() {
    /// Requests fired at once per round.
    const CONCURRENCY: usize = 8;
    /// Rounds; `CONCURRENCY * ROUNDS` is the number of requests.
    const ROUNDS: usize = 25;

    let pipeline = Pipeline::with_workers(
        ScriptedStt::new(&["contended"]),
        ScriptedTextModel::new(json!({})),
        Some(2),
    )
    .await;
    let mut recordings = Vec::new();
    for index in 0..4 {
        recordings.push(pipeline.import(&format!("stress-{index}.wav")).await);
    }
    pipeline
        .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
        .await;

    let mut accepted = 0_usize;
    for round in 0..ROUNDS {
        let mut calls = Vec::with_capacity(CONCURRENCY);
        for slot in 0..CONCURRENCY {
            let backend = Arc::clone(&pipeline.backend);
            let recording_id = recordings[(round + slot) % recordings.len()];
            calls.push(tokio::spawn(async move {
                backend
                    .start_processing(ValidatedStartProcessing {
                        recording_id,
                        stage: Stage::Transcribe,
                        model_override: None,
                        cost_acknowledged: false,
                        rerun: false,
                    })
                    .await
            }));
        }
        for call in calls {
            match call.await.expect("the request task ran to completion") {
                Ok(response) => {
                    response.job_id.parse::<JobId>().expect("a job id");
                    accepted += 1;
                }
                Err(error) => panic!(
                    "a stage request failed under contention: {:?} ({})",
                    error.code(),
                    error.detail()
                ),
            }
        }
    }
    assert_eq!(
        accepted,
        ROUNDS * CONCURRENCY,
        "every stage request was accepted"
    );
    pipeline.stop().await;
}

/// Identical requests that race open one job, never two.
///
/// Nothing claims the queue here, so every request sees the same pending
/// stage: the keyed insert is what deduplicates them, and it must do so
/// without a retry that could open a second, separately chargeable job.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_identical_requests_open_exactly_one_job() {
    /// Requests fired at once.
    const CONCURRENCY: usize = 32;

    let pipeline = Pipeline::idle(
        ScriptedStt::new(&["once"]),
        ScriptedTextModel::new(json!({})),
    )
    .await;
    let recording_id = pipeline.import("race.wav").await;
    pipeline
        .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
        .await;

    let mut calls = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let backend = Arc::clone(&pipeline.backend);
        calls.push(tokio::spawn(async move {
            backend
                .start_processing(ValidatedStartProcessing {
                    recording_id,
                    stage: Stage::Transcribe,
                    model_override: None,
                    cost_acknowledged: false,
                    rerun: false,
                })
                .await
        }));
    }
    let mut job_ids = std::collections::BTreeSet::new();
    for call in calls {
        let response = call
            .await
            .expect("the request task ran to completion")
            .expect("the stage is queued");
        job_ids.insert(response.job_id.parse::<JobId>().expect("a job id"));
    }
    assert_eq!(
        job_ids.len(),
        1,
        "racing identical requests collapsed onto one job"
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
    assert_eq!(jobs.items.len(), 1, "one job row, not thirty-two");
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
    let pool = ProcessingHandle::start(&backend, Some(1));

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
    println!(
        "whisper.cpp tiny.en: {elapsed:?} for {} segment(s)",
        page.segments.len()
    );
    println!("transcript: {text}");
    assert!(
        text.to_lowercase().contains("ask not"),
        "the real transcript does not contain the clip's words: {text}"
    );
    pool.stop(Duration::from_secs(5)).await;
}

/* ------------------------------------------------- speaker matching (P1.3) */

/// A pipeline whose worker can also run the speaker-matching executor, over a
/// deterministic diarization provider and a scripted embedder
/// (`VOICE_PROFILES_SPEC` VP-100..VP-105).
struct VoicePipeline {
    inner: Pipeline,
    embedder: Arc<plaudon_app_core::voice_profiles::embedder::ScriptedEmbedder>,
    secrets: Arc<plaudon_security::secrets::InMemorySecretStore>,
}

/// Dimension of the scripted checkpoint; the arithmetic lives in app-core.
const VOICE_DIM: usize = 8;

/// Four six-second segments, so each cluster clears the three-second
/// identification minimum of VP-065.
const VOICE_SEGMENT_MS: u64 = 6_000;

impl VoicePipeline {
    async fn open() -> Self {
        Self::open_over(None).await
    }

    /// The same pipeline with `watch` in place of the HTTPS transport, so a
    /// test can say what the whole flow attempted to send.
    async fn watched(watch: &EgressWatch) -> Self {
        Self::open_over(Some(watch.transport())).await
    }

    async fn open_over(
        transport: Option<Arc<dyn plaudon_ai_openrouter::transport::AsyncHttpTransport>>,
    ) -> Self {
        use plaudon_ai_diarization::SpeakerTurn;
        use plaudon_app_core::voice_profiles::embedder::ScriptedEmbedder;

        let fixture = crate::backend::testing::backend_fixture().await;
        let embedder = Arc::new(ScriptedEmbedder::new(VOICE_DIM));
        let secrets = Arc::new(plaudon_security::secrets::InMemorySecretStore::new());
        let diarizer = Arc::new(plaudon_ai_diarization::FakeDiarizationProvider::new(vec![
            SpeakerTurn::new("S1", 0, 12_000),
            SpeakerTurn::new("S2", 12_000, 24_000),
        ]));
        let mut backend = fixture
            .backend
            .clone()
            .with_stt_engines(vec![ScriptedStt::new(&["one", "two", "three", "four"])
                .with_segment_ms(VOICE_SEGMENT_MS)
                .shared()])
            .with_text_model(ScriptedTextModel::new(json!({})).shared())
            .with_voice_engines(
                diarizer as Arc<dyn plaudon_ai_diarization::contract::DiarizationProvider>,
                Arc::clone(&embedder)
                    as Arc<dyn plaudon_app_core::voice_profiles::embedder::SpeakerEmbedder>,
            )
            .with_voice_secrets(
                Arc::clone(&secrets) as Arc<dyn plaudon_security::secrets::SecretStore>
            );
        if let Some(transport) = transport {
            backend = backend.with_cloud_transport(transport);
        }
        let backend = Arc::new(backend);
        let pool = ProcessingHandle::start(&backend, Some(1));
        Self {
            inner: Pipeline {
                fixture,
                backend,
                pool: Some(pool),
            },
            embedder,
            secrets,
        }
    }

    /// Import a recording and transcribe it, so there is a timed revision to
    /// reconcile speaker turns against.
    async fn transcribed(&self, name: &str) -> RecordingId {
        let recording_id = self.inner.import(name).await;
        self.inner
            .prefer(Stage::Transcribe, SCRIPTED_STT_ENGINE)
            .await;
        let job = self.inner.run_stage(recording_id, Stage::Transcribe).await;
        let summary = self.inner.settled(job).await;
        assert_eq!(summary.state, JobState::Completed, "{summary:?}");
        recording_id
    }

    /// Turn the runtime gate on, as the §12.2 activation command does.
    async fn activate(&self) {
        plaudon_persistence::voice_profiles::VoiceProfileGateRepository::new(
            self.inner.backend.store().db().clone(),
        )
        .activate(
            plaudon_app_core::voice_profiles::pipeline::CONSENT_DISCLOSURE_VERSION,
            plaudon_domain::time::Timestamp::from_millis(0),
        )
        .await
        .expect("the gate is stored");
    }

    /// Enrol a person whose voice is the scripted voice of `cluster`.
    async fn enrol(&self, cluster: &str) -> plaudon_domain::ids::PersonId {
        use plaudon_app_core::voice_profiles::store::{EmbeddingCipher, ExemplarBinding};
        use plaudon_persistence::voice_profiles::{
            EnrollmentDraft, ExemplarInsert, VoiceEnrollmentRepository,
        };

        let person = plaudon_domain::ids::PersonId::new();
        sqlx::query(
            "INSERT INTO people (id, display_name, created_at, updated_at, status)
             VALUES (?1, 'Priya', 0, 0, 'active')",
        )
        .bind(person.to_string())
        .execute(self.inner.backend.pool())
        .await
        .expect("person");

        let voices = VoiceEnrollmentRepository::new(self.inner.backend.store().db().clone());
        voices
            .enroll(
                person,
                &EnrollmentDraft {
                    consent_version:
                        plaudon_app_core::voice_profiles::pipeline::CONSENT_DISCLOSURE_VERSION
                            .to_owned(),
                    consent_note: None,
                    retention_months: 12,
                },
                plaudon_domain::time::Timestamp::from_millis(0),
            )
            .await
            .expect("enrolment");

        let cipher = EmbeddingCipher::new(
            Arc::clone(&self.secrets) as Arc<dyn plaudon_security::secrets::SecretStore>
        );
        let checkpoint = plaudon_app_core::voice_profiles::embedder::SpeakerEmbedder::checkpoint(
            self.embedder.as_ref(),
        );
        let vector = self.embedder.voice_of(cluster);
        for index in 0..2u8 {
            let binding = ExemplarBinding {
                person_id: person,
                model_id: checkpoint.binding.model_id.clone(),
                model_version: checkpoint.binding.model_version.clone(),
                embedding_dim: u32::try_from(VOICE_DIM).unwrap(),
            };
            let sealed = cipher.seal(&vector, &binding).expect("seal");
            voices
                .add_exemplar(
                    &ExemplarInsert {
                        person_id: person,
                        recording_id: None,
                        diarization_run_id: None,
                        cluster_key: None,
                        source_start_ms: None,
                        source_end_ms: None,
                        speech_ms: 6_000,
                        model_id: checkpoint.binding.model_id.clone(),
                        model_version: checkpoint.binding.model_version.clone(),
                        model_sha256: checkpoint.binding.model_sha256.to_hex(),
                        embedding_dim: sealed.embedding_dim,
                        embedding_cipher: sealed.cipher,
                        embedding_nonce: sealed.nonce,
                    },
                    plaudon_domain::time::Timestamp::from_millis(i64::from(index)),
                )
                .await
                .expect("exemplar");
        }
        person
    }

    async fn suggestions(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM speaker_suggestions")
            .fetch_one(self.inner.backend.pool())
            .await
            .expect("count")
    }

    async fn stop(self) {
        self.inner.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_speaker_match_job_runs_and_records_a_suggestion() {
    let pipeline = VoicePipeline::open().await;
    let recording_id = pipeline.transcribed("voices.wav").await;
    pipeline.activate().await;
    let person = pipeline.enrol("S1").await;

    let job_id = pipeline
        .inner
        .backend
        .enqueue_speaker_match(recording_id)
        .await
        .expect("the job is queued");
    let summary = pipeline.inner.settled(job_id).await;
    assert_eq!(summary.kind, JobKind::SpeakerMatch);
    assert_eq!(summary.state, JobState::Completed, "{summary:?}");

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT cluster_key, person_id, state FROM speaker_suggestions ORDER BY cluster_key",
    )
    .fetch_all(pipeline.inner.backend.pool())
    .await
    .expect("suggestions");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1, person.to_string());
    assert_eq!(rows[0].2, "pending", "nothing is assigned by a job");
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asking_for_the_same_match_twice_deduplicates_until_a_profile_changes() {
    let pipeline = VoicePipeline::open().await;
    let recording_id = pipeline.transcribed("dedup.wav").await;
    pipeline.activate().await;
    pipeline.enrol("S1").await;

    let digest_before = pipeline
        .inner
        .backend
        .enrolled_profile_digest()
        .await
        .expect("digest");
    let first = pipeline
        .inner
        .backend
        .enqueue_speaker_match(recording_id)
        .await
        .expect("queued");
    let again = pipeline
        .inner
        .backend
        .enqueue_speaker_match(recording_id)
        .await
        .expect("queued again");
    assert!(
        first == again || pipeline.inner.settled(first).await.state.is_terminal(),
        "an identical request neither duplicated nor reused the job"
    );

    // Enrolling a second person changes the profile-set digest, which is a
    // keyed dimension: the next request is different work (VP-101).
    pipeline.enrol("S2").await;
    let digest_after = pipeline
        .inner
        .backend
        .enrolled_profile_digest()
        .await
        .expect("digest");
    assert_ne!(digest_before, digest_after);
    let third = pipeline
        .inner
        .backend
        .enqueue_speaker_match(recording_id)
        .await
        .expect("queued under the new key");
    assert_ne!(third, first, "a new profile set makes a fresh job");
    pipeline.inner.settled(third).await;
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matching_cannot_be_queued_while_the_gate_is_closed() {
    let pipeline = VoicePipeline::open().await;
    let recording_id = pipeline.transcribed("closed.wav").await;

    // Off by default: no activation, nobody enrolled (VP-003).
    let refused = pipeline
        .inner
        .backend
        .enqueue_speaker_match(recording_id)
        .await
        .expect_err("a closed gate queues nothing");
    assert_eq!(refused.code(), AppErrorCode::CapabilityUnverified);

    // Activated, but still nobody enrolled: still nothing to match (VP-024).
    pipeline.activate().await;
    let refused = pipeline
        .inner
        .backend
        .enqueue_speaker_match(recording_id)
        .await
        .expect_err("nobody is enrolled");
    assert_eq!(refused.code(), AppErrorCode::CapabilityUnverified);

    assert_eq!(pipeline.embedder.call_count(), 0, "nothing was embedded");
    assert_eq!(pipeline.suggestions().await, 0);
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turning_matching_off_after_queueing_leaves_the_job_without_biometric_stages() {
    let pipeline = VoicePipeline::open().await;
    let recording_id = pipeline.transcribed("flip.wav").await;
    pipeline.activate().await;
    pipeline.enrol("S1").await;

    // Queue the job, then withdraw the toggle before the worker gets to the
    // biometric stages: the run must plan none of them (VP-003, VP-024).
    plaudon_persistence::voice_profiles::VoiceProfileGateRepository::new(
        pipeline.inner.backend.store().db().clone(),
    )
    .disable(plaudon_domain::time::Timestamp::from_millis(1))
    .await
    .expect("the gate is stored");
    let request = StageRequest {
        recording_id,
        kind: JobKind::SpeakerMatch,
        engine: plaudon_ai_diarization::FAKE_PROVIDER_ID.to_owned(),
        model_id: Some(plaudon_app_core::voice_profiles::embedder::SCRIPTED_EMBEDDER_ID.to_owned()),
        model_version: Some("1.0".to_owned()),
        stage: "speaker_match".to_owned(),
        label: "Match speakers".to_owned(),
        audio_sha256: "b".repeat(64),
        cloud: false,
        cost_acknowledged: false,
        transcript_revision: None,
        normalization: std::collections::BTreeMap::new(),
    };
    let job_id = pipeline
        .inner
        .backend
        .enqueue_stage(&request)
        .await
        .expect("queued directly");
    let summary = pipeline.inner.settled(job_id).await;
    assert_eq!(summary.state, JobState::Completed, "{summary:?}");
    assert_eq!(
        pipeline.embedder.call_count(),
        0,
        "the gate closed between enqueue and run, so nothing was embedded"
    );
    assert_eq!(pipeline.suggestions().await, 0);
    // Diarization still ran: it carries no biometrics.
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM diarization_runs")
        .fetch_one(pipeline.inner.backend.pool())
        .await
        .expect("count");
    assert_eq!(runs, 1);
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_build_without_an_embedder_parks_the_job_instead_of_failing_it() {
    // No `with_voice_engines`: the host has no speaker-embedding model, which
    // preflight must treat as a waiting state, not a failure (§14.4).
    let pipeline =
        Pipeline::open(ScriptedStt::new(&["x"]), ScriptedTextModel::new(json!({}))).await;
    let recording_id = pipeline.import("noembedder.wav").await;
    let request = StageRequest {
        recording_id,
        kind: JobKind::SpeakerMatch,
        engine: plaudon_ai_diarization::FAKE_PROVIDER_ID.to_owned(),
        model_id: Some("absent".to_owned()),
        model_version: None,
        stage: "speaker_match".to_owned(),
        label: "Match speakers".to_owned(),
        audio_sha256: "c".repeat(64),
        cloud: false,
        cost_acknowledged: false,
        transcript_revision: None,
        normalization: std::collections::BTreeMap::new(),
    };
    let job_id = pipeline
        .backend
        .enqueue_stage(&request)
        .await
        .expect("queued");
    let summary = pipeline
        .until(job_id, |summary| {
            summary.state == JobState::WaitingForModel || summary.state.is_terminal()
        })
        .await;
    assert_eq!(summary.state, JobState::WaitingForModel, "{summary:?}");
    pipeline.stop().await;
}

/* ------------------------------- voice-profile commands (P1.5, §8, §9A) */

/// An embedder that panics if it is ever called.
///
/// The structural claim of VP-011/VP-215 is that no embed call is *reachable*
/// while the feature is off or the consent gate is unpassed. A double that
/// cannot be called without failing the test is how that is proved at runtime;
/// the compile-time half is
/// [`plaudon_app_core::voice_profiles::ConsentProof`], which the roster path
/// never obtains.
#[derive(Debug)]
struct PanickingEmbedder {
    checkpoint: Arc<plaudon_app_core::voice_profiles::EmbedderCheckpoint>,
}

impl PanickingEmbedder {
    fn new() -> Self {
        Self {
            checkpoint: plaudon_app_core::voice_profiles::SpeakerEmbedder::checkpoint(
                &plaudon_app_core::voice_profiles::embedder::ScriptedEmbedder::new(VOICE_DIM),
            ),
        }
    }
}

#[async_trait::async_trait]
impl plaudon_app_core::voice_profiles::SpeakerEmbedder for PanickingEmbedder {
    fn checkpoint(&self) -> Arc<plaudon_app_core::voice_profiles::EmbedderCheckpoint> {
        Arc::clone(&self.checkpoint)
    }

    async fn embed_window(
        &self,
        _window: &plaudon_app_core::voice_profiles::EmbedWindow,
        _ctx: &plaudon_ai_contracts::job::JobContext,
    ) -> Result<
        plaudon_app_core::voice_profiles::matching::Embedding,
        plaudon_app_core::voice_profiles::EmbedError,
    > {
        panic!("the consent gate did not precede embedding (VP-011, VP-215)");
    }
}

impl VoicePipeline {
    /// A pipeline whose embedder fails the test if anything calls it.
    async fn open_with_panicking_embedder() -> Self {
        use plaudon_ai_diarization::SpeakerTurn;
        use plaudon_app_core::voice_profiles::embedder::ScriptedEmbedder;

        let fixture = crate::backend::testing::backend_fixture().await;
        let embedder = Arc::new(ScriptedEmbedder::new(VOICE_DIM));
        let secrets = Arc::new(plaudon_security::secrets::InMemorySecretStore::new());
        let diarizer = Arc::new(plaudon_ai_diarization::FakeDiarizationProvider::new(vec![
            SpeakerTurn::new("S1", 0, 12_000),
            SpeakerTurn::new("S2", 12_000, 24_000),
        ]));
        let backend = Arc::new(
            fixture
                .backend
                .clone()
                .with_stt_engines(vec![ScriptedStt::new(&["one", "two", "three", "four"])
                    .with_segment_ms(VOICE_SEGMENT_MS)
                    .shared()])
                .with_text_model(ScriptedTextModel::new(json!({})).shared())
                .with_voice_engines(
                    diarizer as Arc<dyn plaudon_ai_diarization::contract::DiarizationProvider>,
                    Arc::new(PanickingEmbedder::new())
                        as Arc<dyn plaudon_app_core::voice_profiles::SpeakerEmbedder>,
                )
                .with_voice_secrets(
                    Arc::clone(&secrets) as Arc<dyn plaudon_security::secrets::SecretStore>
                ),
        );
        let pool = ProcessingHandle::start(&backend, Some(1));
        Self {
            inner: Pipeline {
                fixture,
                backend,
                pool: Some(pool),
            },
            embedder,
            secrets,
        }
    }

    /// The speaker row of one cluster.
    async fn cluster_speaker(
        &self,
        recording_id: RecordingId,
        cluster: &str,
    ) -> plaudon_persistence::speakers::StoredSpeaker {
        self.inner
            .backend
            .speaker_row_for_cluster(recording_id, cluster)
            .await
            .expect("the speakers table is readable")
            .unwrap_or_else(|| panic!("no speaker row for {cluster}"))
    }

    /// `(person_id, name)` of a speaker row, read straight from SQLite.
    async fn stored_speaker(
        &self,
        speaker_id: plaudon_domain::ids::SpeakerId,
    ) -> (Option<String>, Option<String>, bool) {
        let row: (Option<String>, Option<String>, i64) =
            sqlx::query_as("SELECT person_id, name, user_confirmed FROM speakers WHERE id = ?1")
                .bind(speaker_id.to_string())
                .fetch_one(self.inner.backend.pool())
                .await
                .expect("the speaker row is readable");
        (row.0, row.1, row.2 != 0)
    }

    /// How many of a revision's segments point at `speaker_id`.
    async fn segments_for(&self, speaker_id: plaudon_domain::ids::SpeakerId) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM transcript_segments WHERE speaker_id = ?1")
            .bind(speaker_id.to_string())
            .fetch_one(self.inner.backend.pool())
            .await
            .expect("the segments table is readable")
    }

    /// Run the pipeline's non-biometric half, which is what an enrolment
    /// needs: a diarization run and its reconciliation.
    ///
    /// Queued directly rather than through
    /// [`LocalBackend::enqueue_speaker_match`], because that refuses while the
    /// gate is closed (VP-003) — and before anybody is enrolled it always is.
    /// `stage_intent` keeps this job's key distinct from a later real match of
    /// the same recording, so the two never deduplicate onto each other.
    async fn diarize_only(&self, recording_id: RecordingId) -> JobState {
        let mut normalization = std::collections::BTreeMap::new();
        normalization.insert("stage_intent".to_owned(), "diarize_only".to_owned());
        let request = StageRequest {
            recording_id,
            kind: JobKind::SpeakerMatch,
            engine: plaudon_ai_diarization::FAKE_PROVIDER_ID.to_owned(),
            model_id: Some(
                plaudon_app_core::voice_profiles::embedder::SCRIPTED_EMBEDDER_ID.to_owned(),
            ),
            model_version: Some("1.0".to_owned()),
            stage: "speaker_match".to_owned(),
            label: "Match speakers".to_owned(),
            audio_sha256: "c".repeat(64),
            cloud: false,
            cost_acknowledged: false,
            transcript_revision: None,
            normalization,
        };
        let job = self
            .inner
            .backend
            .enqueue_stage(&request)
            .await
            .expect("the job is queued directly");
        self.inner.settled(job).await.state
    }

    /// Run a real diarization-and-match job to a terminal state.
    async fn match_now(&self, recording_id: RecordingId) -> JobState {
        let job = self
            .inner
            .backend
            .enqueue_speaker_match(recording_id)
            .await
            .expect("the job is queued");
        self.inner.settled(job).await.state
    }
}

/// The attestation a gated-on enrolment carries (VP-120 level 3).
fn consent_dto() -> crate::dto::EnrolmentConsentDto {
    crate::dto::EnrolmentConsentDto {
        disclosure_version: plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE
            .version
            .to_owned(),
        person_attested: true,
        retention_months: None,
        consent_note: Some("verbal agreement, stand-up of 2026-03-02".to_owned()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_roster_round_trips_with_matching_off_and_never_reaches_the_embedder() {
    // VP-011/VP-215: the people list, assignment and every roster edit work
    // with biometric matching off, and nothing in that path can call an
    // embedder — the double here panics if it is.
    use crate::ports::{PeoplePort, VoiceProfilesPort};

    let pipeline = VoicePipeline::open_with_panicking_embedder().await;
    let recording_id = pipeline.transcribed("manual.wav").await;
    let backend = Arc::clone(&pipeline.inner.backend);

    let settings = backend
        .voice_profile_settings()
        .await
        .expect("settings are readable with the feature off");
    assert!(!settings.enabled, "biometric matching is off by default");
    assert_eq!(settings.consent.state, "off");
    assert_eq!(settings.enrolled_people, 0);

    let priya = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Priya"))
        .await
        .expect("a roster entry is created with matching off");
    let edited = backend
        .update_person(
            priya.person_id,
            &plaudon_persistence::people::PersonDraft {
                display_name: "Priya Raman".to_owned(),
                aliases: vec!["Priya".to_owned()],
                email: Some("priya@example.test".to_owned()),
                organisation: Some("Acme".to_owned()),
            },
        )
        .await
        .expect("a roster entry is edited");
    assert_eq!(edited.display_name, "Priya Raman");
    assert!(
        edited.voice.is_none(),
        "a name-only entry has no voiceprint"
    );

    // The cluster assignment is one action: the speaker row's name and link,
    // plus every segment the reconciliation gave that cluster (VP-032).
    pipeline.diarize_only(recording_id).await;
    let speaker = pipeline.cluster_speaker(recording_id, "S1").await;
    let assigned = backend
        .assign_cluster_person(&crate::ports::ClusterAssignment {
            recording_id,
            speaker_id: speaker.id,
            person_id: Some(priya.person_id),
            overwrite_segment_exceptions: false,
        })
        .await
        .expect("a cluster is assigned with matching off");
    assert!(assigned.applied);
    assert!(assigned.segments_assigned > 0, "{assigned:?}");
    let (person_id, name, confirmed) = pipeline.stored_speaker(speaker.id).await;
    assert_eq!(
        person_id.as_deref(),
        Some(priya.person_id.to_string()).as_deref()
    );
    assert_eq!(name.as_deref(), Some("Priya Raman"));
    assert!(confirmed, "a user's pick is a confirmed identity");

    let listed = backend
        .list_people(Some("raman"))
        .await
        .expect("the roster is searchable by alias and name");
    assert_eq!(listed.people.len(), 1);
    assert_eq!(
        listed.people[0].recordings, 1,
        "the blast radius is counted"
    );

    // Unknown/none clears the link and returns the neutral label (VP-210).
    let cleared = backend
        .assign_cluster_person(&crate::ports::ClusterAssignment {
            recording_id,
            speaker_id: speaker.id,
            person_id: None,
            overwrite_segment_exceptions: false,
        })
        .await
        .expect("an assignment is cleared");
    assert!(cleared.person.is_none());
    let (person_id, name, _) = pipeline.stored_speaker(speaker.id).await;
    assert_eq!(person_id, None);
    assert_eq!(name, None);

    // Merge and delete are roster actions too, and with nobody enrolled they
    // never touch a key.
    let duplicate = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Priya R."))
        .await
        .expect("a duplicate entry");
    let disclosure = backend
        .merge_people(duplicate.person_id, priya.person_id, false)
        .await
        .expect("a merge discloses before it moves");
    assert!(!disclosure.applied);
    let merged = backend
        .merge_people(duplicate.person_id, priya.person_id, true)
        .await
        .expect("a merge applies");
    assert!(merged.applied);
    assert_eq!(merged.exemplars, 0);
    let deleted = backend
        .delete_person(priya.person_id, true)
        .await
        .expect("a roster entry is deleted");
    assert!(deleted.applied);
    assert_eq!(deleted.exemplars, 0);

    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_embedding_is_reachable_while_ungated_or_while_matching_is_off() {
    // The §17.1 structural test: the scripted embedder panics on call, and the
    // enrolment and matching commands are driven through the whole engine in
    // both closed states (VP-011, VP-215, VP-003).
    use crate::ports::{PeoplePort, VoiceProfilesPort};

    let pipeline = VoicePipeline::open_with_panicking_embedder().await;
    let recording_id = pipeline.transcribed("gated.wav").await;
    let backend = Arc::clone(&pipeline.inner.backend);
    let person = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Sam"))
        .await
        .expect("a roster entry");

    let request = crate::ports::EnrolmentRequest {
        person_id: person.person_id,
        recording_id,
        cluster_key: "S1".to_owned(),
        attestation: Some(plaudon_app_core::voice_profiles::EnrolmentAttestation::attested()),
    };

    // State one: the toggle is off, which is its default.
    let refused = backend
        .enroll_person_voice(&request)
        .await
        .expect_err("an ungated enrolment computes nothing");
    assert_eq!(refused.code(), AppErrorCode::CapabilityUnverified);
    let refused = backend
        .add_voice_exemplar(&request)
        .await
        .expect_err("an ungated exemplar computes nothing");
    assert_eq!(refused.code(), AppErrorCode::CapabilityUnverified);
    let queued = backend
        .start_speaker_matching(&[recording_id])
        .await
        .expect("a bulk request answers rather than failing");
    assert!(queued.jobs.is_empty(), "{queued:?}");
    assert_eq!(queued.refusals.len(), 1);

    // State two: the toggle is on but the disclosure is stale, which is the
    // same closed gate from the embedder's point of view (VP-121).
    plaudon_persistence::voice_profiles::VoiceProfileGateRepository::new(
        backend.store().db().clone(),
    )
    .activate(
        "voice-matching-v0",
        plaudon_domain::time::Timestamp::from_millis(0),
    )
    .await
    .expect("a stale acceptance is storable");
    let refused = backend
        .enroll_person_voice(&request)
        .await
        .expect_err("a stale disclosure computes nothing");
    assert_eq!(refused.code(), AppErrorCode::CapabilityUnverified);

    // And the whole job runs without a single embed call, because the plan has
    // no biometric stages at all (VP-024).
    assert_eq!(
        pipeline.diarize_only(recording_id).await,
        JobState::Completed
    );

    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turning_the_feature_on_requires_accepting_the_current_disclosure() {
    use crate::ports::VoiceProfilesPort;

    let pipeline = VoicePipeline::open_with_panicking_embedder().await;
    let backend = Arc::clone(&pipeline.inner.backend);

    let refused = backend
        .set_voice_profiles_enabled(true, None)
        .await
        .expect_err("an enable without an acceptance is refused");
    assert_eq!(refused.code(), AppErrorCode::InvalidArgument);
    let refused = backend
        .set_voice_profiles_enabled(true, Some("voice-matching-v0"))
        .await
        .expect_err("an enable quoting another disclosure is refused");
    assert_eq!(refused.code(), AppErrorCode::InvalidArgument);

    let settings = backend
        .set_voice_profiles_enabled(
            true,
            Some(plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE.version),
        )
        .await
        .expect("accepting the current disclosure arms the feature");
    assert!(settings.enabled);
    assert_eq!(settings.consent.state, "active");
    assert!(settings
        .disclosure
        .two_tier_sentence
        .contains("needs no special consent"));

    // The acceptance is journalled, which is what VP-122's exportable consent
    // record starts from.
    let events = plaudon_persistence::voice_profiles::VoiceProfileGateRepository::new(
        backend.store().db().clone(),
    )
    .library_events()
    .await
    .expect("the journal is readable");
    assert!(
        events.iter().any(|event| event.kind == "feature_activated"),
        "{events:?}"
    );

    let off = backend
        .set_voice_profiles_enabled(false, None)
        .await
        .expect("the feature switches off");
    assert!(!off.enabled);
    assert_eq!(off.consent.state, "off");
    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolling_then_confirming_a_suggestion_names_the_cluster_and_its_segments() {
    use crate::ports::{PeoplePort, VoiceProfilesPort};

    let pipeline = VoicePipeline::open().await;
    let first = pipeline.transcribed("week-one.wav").await;
    let backend = Arc::clone(&pipeline.inner.backend);
    backend
        .set_voice_profiles_enabled(
            true,
            Some(plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE.version),
        )
        .await
        .expect("the gate opens");

    // The enrolment needs a diarization run to cut clean speech from. Before
    // anybody is enrolled the gate refuses a match job, so the run comes from
    // the pipeline's non-biometric half (VP-024).
    pipeline.diarize_only(first).await;
    let priya = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Priya"))
        .await
        .expect("a roster entry");
    let enrolled = backend
        .enroll_person_voice(&crate::ports::EnrolmentRequest {
            person_id: priya.person_id,
            recording_id: first,
            cluster_key: "S1".to_owned(),
            attestation: Some(
                crate::commands::voice_profiles::attestation(&consent_dto())
                    .expect("a valid attestation"),
            ),
        })
        .await
        .expect("an enrolment from a cluster with 12 s of clean speech");
    assert_eq!(enrolled.exemplars, 1);
    assert!(enrolled.speech_ms >= plaudon_app_core::voice_profiles::ENROL_SPEECH_MS);
    assert_eq!(enrolled.retention_months, 12);

    // A later recording's matching job produces a suggestion, and nothing is
    // renamed until the user confirms (VP-060, ADR-0082).
    let second = pipeline.transcribed("week-two.wav").await;
    assert_eq!(pipeline.match_now(second).await, JobState::Completed);
    let queue = backend
        .list_speaker_suggestions(None, 50)
        .await
        .expect("the review queue reads");
    assert!(!queue.suggestions.is_empty(), "{queue:?}");
    let suggestion = queue
        .suggestions
        .iter()
        .find(|row| row.recording_id == second && row.person.person_id == priya.person_id)
        .expect("the second recording has a suggestion for Priya")
        .clone();
    let speaker = pipeline
        .cluster_speaker(second, &suggestion.cluster_key)
        .await;
    let (before, _, _) = pipeline.stored_speaker(speaker.id).await;
    assert_eq!(before, None, "a suggestion assigns nothing by itself");

    let decision = backend
        .confirm_speaker_suggestion(suggestion.suggestion_id, false)
        .await
        .expect("a confirmation applies the person");
    assert!(decision.applied);
    assert_eq!(decision.state, "confirmed");
    assert!(decision.segments_assigned > 0, "{decision:?}");
    let (person_id, name, confirmed) = pipeline.stored_speaker(speaker.id).await;
    assert_eq!(
        person_id.as_deref(),
        Some(priya.person_id.to_string()).as_deref()
    );
    assert_eq!(name.as_deref(), Some("Priya"));
    assert!(confirmed);
    assert!(pipeline.segments_for(speaker.id).await > 0);

    // Renaming the person propagates by reference to every recording that
    // links to them, and changes no assignment (VP-213a).
    backend
        .update_person(
            priya.person_id,
            &plaudon_persistence::people::PersonDraft::named("Priya Raman"),
        )
        .await
        .expect("a rename");
    let listed = backend.list_people(None).await.expect("the roster");
    let row = listed
        .people
        .iter()
        .find(|row| row.person_id == priya.person_id)
        .expect("the entry");
    assert_eq!(row.display_name, "Priya Raman");
    assert!(row.recordings >= 1);
    assert!(row.voice.is_some(), "the enrolment badge is present");

    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejecting_never_renames_and_reject_then_pick_records_the_correction() {
    use crate::ports::{PeoplePort, VoiceProfilesPort};

    let pipeline = VoicePipeline::open().await;
    let first = pipeline.transcribed("mistake-one.wav").await;
    let backend = Arc::clone(&pipeline.inner.backend);
    backend
        .set_voice_profiles_enabled(
            true,
            Some(plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE.version),
        )
        .await
        .expect("the gate opens");
    pipeline.diarize_only(first).await;

    let sam = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Sam"))
        .await
        .expect("a roster entry");
    backend
        .enroll_person_voice(&crate::ports::EnrolmentRequest {
            person_id: sam.person_id,
            recording_id: first,
            cluster_key: "S1".to_owned(),
            attestation: Some(
                crate::commands::voice_profiles::attestation(&consent_dto())
                    .expect("a valid attestation"),
            ),
        })
        .await
        .expect("Sam is enrolled");

    let second = pipeline.transcribed("mistake-two.wav").await;
    pipeline.match_now(second).await;
    let suggestion = backend
        .list_speaker_suggestions(Some(second), 50)
        .await
        .expect("the panel reads")
        .suggestions
        .first()
        .cloned()
        .expect("a suggestion for the second recording");
    let speaker = pipeline
        .cluster_speaker(second, &suggestion.cluster_key)
        .await;

    // Not them, with nobody named: nothing is renamed, and the score stays as
    // negative evidence (VP-216).
    let decision = backend
        .reject_speaker_suggestion(suggestion.suggestion_id, None, false)
        .await
        .expect("a rejection settles");
    assert_eq!(decision.state, "rejected");
    assert!(!decision.applied, "a rejection never renames");
    assert_eq!(decision.segments_assigned, 0);
    let (person_id, name, _) = pipeline.stored_speaker(speaker.id).await;
    assert_eq!(person_id, None);
    assert_eq!(name, None);
    let evidence = plaudon_persistence::voice_profiles::VoiceEnrollmentRepository::new(
        backend.store().db().clone(),
    )
    .decisions(sam.person_id)
    .await
    .expect("the decisions read");
    assert_eq!(evidence.len(), 1);
    assert!(!evidence[0].confirmed, "a rejection is negative evidence");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM voice_exemplars WHERE person_id = ?1")
            .bind(sam.person_id.to_string())
            .fetch_one(backend.pool())
            .await
            .expect("count"),
        1,
        "the rejected cluster's embedding is never stored against anybody"
    );

    // Reject-then-pick on a second cluster: the corrected person is assigned
    // and the suggestion is marked `corrected` (VP-216, VP-217).
    let third = pipeline.transcribed("mistake-three.wav").await;
    pipeline.match_now(third).await;
    let suggestion = backend
        .list_speaker_suggestions(Some(third), 50)
        .await
        .expect("the panel reads")
        .suggestions
        .first()
        .cloned()
        .expect("a suggestion for the third recording");
    let priya = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Priya"))
        .await
        .expect("a roster entry");
    let same_person = backend
        .reject_speaker_suggestion(suggestion.suggestion_id, Some(sam.person_id), false)
        .await
        .expect_err("a correction cannot name the suggested person");
    assert_eq!(same_person.code(), AppErrorCode::InvalidArgument);
    let decision = backend
        .reject_speaker_suggestion(suggestion.suggestion_id, Some(priya.person_id), false)
        .await
        .expect("reject-then-pick settles");
    assert_eq!(decision.state, "corrected");
    assert!(decision.applied);
    let speaker = pipeline
        .cluster_speaker(third, &suggestion.cluster_key)
        .await;
    let (person_id, name, _) = pipeline.stored_speaker(speaker.id).await;
    assert_eq!(
        person_id.as_deref(),
        Some(priya.person_id.to_string()).as_deref()
    );
    assert_eq!(name.as_deref(), Some("Priya"));

    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn splitting_an_enrolment_re_seals_the_exemplars_it_moves() {
    use crate::ports::{PeoplePort, VoiceProfilesPort};

    let pipeline = VoicePipeline::open().await;
    let first = pipeline.transcribed("split-one.wav").await;
    let second = pipeline.transcribed("split-two.wav").await;
    let backend = Arc::clone(&pipeline.inner.backend);
    backend
        .set_voice_profiles_enabled(
            true,
            Some(plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE.version),
        )
        .await
        .expect("the gate opens");
    pipeline.diarize_only(first).await;
    pipeline.diarize_only(second).await;

    let priya = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Priya"))
        .await
        .expect("a roster entry");
    let attestation =
        crate::commands::voice_profiles::attestation(&consent_dto()).expect("a valid attestation");
    backend
        .enroll_person_voice(&crate::ports::EnrolmentRequest {
            person_id: priya.person_id,
            recording_id: first,
            cluster_key: "S1".to_owned(),
            attestation: Some(attestation.clone()),
        })
        .await
        .expect("enrolled from the first recording");
    let extended = backend
        .add_voice_exemplar(&crate::ports::EnrolmentRequest {
            person_id: priya.person_id,
            recording_id: second,
            cluster_key: "S2".to_owned(),
            attestation: None,
        })
        .await
        .expect("extended from the second recording");
    assert_eq!(extended.exemplars, 2);
    assert_eq!(extended.source_recordings, 2, "device diversity is counted");

    let voices = plaudon_persistence::voice_profiles::VoiceEnrollmentRepository::new(
        backend.store().db().clone(),
    );
    let moving = voices
        .exemplars(priya.person_id)
        .await
        .expect("exemplars")
        .into_iter()
        .find(|row| row.recording_id == Some(second))
        .expect("the second recording's exemplar");

    let split = backend
        .split_voice_enrollment(&crate::ports::SplitRequest {
            person_id: priya.person_id,
            exemplar_ids: vec![moving.id],
            target: crate::ports::SplitTarget::New("Sam".to_owned()),
            attestation,
        })
        .await
        .expect("a split by provenance");
    assert_eq!(split.exemplars_moved, 1);
    assert_eq!(split.exemplars_remaining, 1);
    assert!(split.target.voice.is_some());

    // The moved ciphertext authenticates under the *target's* binding, which
    // is the whole point of re-sealing: a bare column rewrite would have left
    // an exemplar nothing can decrypt.
    let cipher = plaudon_app_core::voice_profiles::EmbeddingCipher::new(Arc::clone(
        &pipeline.secrets,
    )
        as Arc<dyn plaudon_security::secrets::SecretStore>);
    let moved = voices
        .exemplars(split.target_person_id)
        .await
        .expect("the target's exemplars")
        .into_iter()
        .next()
        .expect("one exemplar moved");
    cipher
        .open(
            &plaudon_app_core::voice_profiles::SealedEmbedding {
                cipher: moved.embedding_cipher.clone(),
                nonce: moved.embedding_nonce.clone(),
                embedding_dim: moved.embedding_dim,
            },
            &plaudon_app_core::voice_profiles::ExemplarBinding {
                person_id: split.target_person_id,
                model_id: moved.model_id.clone(),
                model_version: moved.model_version.clone(),
                embedding_dim: moved.embedding_dim,
            },
        )
        .expect("the moved exemplar decrypts under its new owner");

    pipeline.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forgetting_everyone_keeps_the_roster_and_turns_the_feature_off() {
    use crate::ports::{PeoplePort, VoiceProfilesPort};

    let pipeline = VoicePipeline::open().await;
    let recording_id = pipeline.transcribed("forget.wav").await;
    let backend = Arc::clone(&pipeline.inner.backend);
    backend
        .set_voice_profiles_enabled(
            true,
            Some(plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE.version),
        )
        .await
        .expect("the gate opens");
    pipeline.diarize_only(recording_id).await;
    let priya = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Priya"))
        .await
        .expect("a roster entry");
    backend
        .enroll_person_voice(&crate::ports::EnrolmentRequest {
            person_id: priya.person_id,
            recording_id,
            cluster_key: "S1".to_owned(),
            attestation: Some(
                crate::commands::voice_profiles::attestation(&consent_dto())
                    .expect("a valid attestation"),
            ),
        })
        .await
        .expect("enrolled");

    // Deleting one person's voiceprint keeps them on the roster (VP-080).
    let disclosure = backend
        .delete_voice_enrollment(priya.person_id, false, None)
        .await
        .expect("the deletion discloses first");
    assert!(!disclosure.applied);
    assert_eq!(disclosure.exemplars, 1);
    let deleted = backend
        .delete_voice_enrollment(priya.person_id, true, Some("withdrew consent"))
        .await
        .expect("the voiceprint goes");
    assert!(deleted.applied);
    assert!(deleted.person_kept);
    assert!(deleted.person.voice.is_none());

    // And "forget everyone" leaves the roster standing with the feature off.
    backend
        .enroll_person_voice(&crate::ports::EnrolmentRequest {
            person_id: priya.person_id,
            recording_id,
            cluster_key: "S1".to_owned(),
            attestation: Some(
                crate::commands::voice_profiles::attestation(&consent_dto())
                    .expect("a valid attestation"),
            ),
        })
        .await
        .expect("re-enrolled");
    let preview = backend
        .forget_all_voice_profiles(false)
        .await
        .expect("the disclosure");
    assert!(!preview.applied);
    assert_eq!(preview.enrollments, 1);
    assert!(!preview.limits.is_empty(), "the honest limits are shown");
    let receipt = backend
        .forget_all_voice_profiles(true)
        .await
        .expect("everything goes");
    assert!(receipt.applied);
    assert!(receipt.key_destroyed);
    assert!(receipt.feature_disabled);
    assert_eq!(receipt.people_kept, 1, "names are not voiceprints");
    let settings = backend.voice_profile_settings().await.expect("settings");
    assert!(!settings.enabled);
    assert_eq!(settings.enrolled_people, 0);
    assert_eq!(settings.roster_people, 1);
    assert_eq!(
        backend
            .list_people(None)
            .await
            .expect("the roster")
            .people
            .len(),
        1
    );

    pipeline.stop().await;
}

/* ------------- the biometric tier reaches no network (VP-110, VP-111) */

/// One attempted outbound request, reduced to what a test log may keep.
///
/// §15.4 forbids a credential or user content in a log line, so the URL is cut
/// down to its path — a signed query parameter is a credential — and no header
/// and no body is read, not even to count bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OutboundAttempt {
    /// `GET` or `POST`, from the egress gate's own vocabulary.
    method: &'static str,
    /// Request path, without the query string.
    path: String,
}

/// The transport the backend is given instead of the HTTPS one.
///
/// Every attempt is recorded and then refused with
/// [`TransportFailure::ConnectFailed`], the one failure whose
/// `bytes_transmitted` is false: the double claims nothing left the machine,
/// because nothing did.
#[derive(Debug, Clone, Default)]
struct EgressWatch {
    attempts: Arc<std::sync::Mutex<Vec<OutboundAttempt>>>,
}

impl EgressWatch {
    fn new() -> Self {
        Self::default()
    }

    /// The same watch, as the backend wants it.
    fn transport(&self) -> Arc<dyn plaudon_ai_openrouter::transport::AsyncHttpTransport> {
        Arc::new(self.clone())
    }

    fn attempts(&self) -> Vec<OutboundAttempt> {
        self.attempts.lock().expect("the egress log").clone()
    }
}

#[async_trait::async_trait]
impl plaudon_ai_openrouter::transport::AsyncHttpTransport for EgressWatch {
    async fn send(
        &self,
        request: plaudon_security::outbound::http::GatedRequest,
    ) -> Result<
        plaudon_security::outbound::http::HttpResponse,
        plaudon_ai_openrouter::transport::TransportFailure,
    > {
        self.attempts
            .lock()
            .expect("the egress log")
            .push(OutboundAttempt {
                method: request.method().as_str(),
                path: path_of(request.url()),
            });
        Err(
            plaudon_ai_openrouter::transport::TransportFailure::ConnectFailed {
                detail: "the egress watch never completes a request".to_owned(),
            },
        )
    }
}

/// The path of `url`, with the query string and fragment dropped.
fn path_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.find('/').map_or("/", |at| &after_scheme[at..]);
    path.split(['?', '#']).next().unwrap_or("/").to_owned()
}

/// Enrol a voice and match a speaker for real, and hand back everything the
/// watch saw.
///
/// Every stage below is the production one: the scripted STT commits a real
/// revision, the fake diarization provider's turns are reconciled against it,
/// the enrolment cuts clean speech and seals a real embedding through
/// [`plaudon_app_core::voice_profiles::EmbeddingCipher`], and the match job the
/// job engine claims runs the §10 pipeline and writes a suggestion row. The
/// embedder's call count is asserted, so the zero at the end is a zero for work
/// that actually computed a voiceprint.
///
/// `local_only` is the §4.3 mode, set through the settings port before anything
/// runs. VP-111 is a claim about both settings of it:
/// `PayloadClass::VoiceBiometric` is refused by
/// [`plaudon_security::outbound`] in every network mode and no consent grant
/// can name it, so turning the mode off must not buy a voiceprint a way out.
async fn enrol_and_match_under_the_watch(local_only: bool) -> Vec<OutboundAttempt> {
    use crate::ports::{PeoplePort, SettingsPort, VoiceProfilesPort};

    let watch = EgressWatch::new();
    let pipeline = VoicePipeline::watched(&watch).await;
    let backend = Arc::clone(&pipeline.inner.backend);

    let mode = backend
        .set_local_only_mode(local_only)
        .await
        .expect("the mode is stored");
    assert_eq!(
        mode.enabled, local_only,
        "the mode the command reports back is the mode"
    );

    // The gate, then the diarization run an enrolment cuts its speech from:
    // before anybody is enrolled a match job is refused, so the run comes from
    // the pipeline's non-biometric half (VP-024).
    let first = pipeline.transcribed("enrol.wav").await;
    backend
        .set_voice_profiles_enabled(
            true,
            Some(plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE.version),
        )
        .await
        .expect("the gate opens");
    assert_eq!(pipeline.diarize_only(first).await, JobState::Completed);

    let priya = backend
        .create_person(&plaudon_persistence::people::PersonDraft::named("Priya"))
        .await
        .expect("a roster entry");
    let enrolled = backend
        .enroll_person_voice(&crate::ports::EnrolmentRequest {
            person_id: priya.person_id,
            recording_id: first,
            cluster_key: "S1".to_owned(),
            attestation: Some(
                crate::commands::voice_profiles::attestation(&consent_dto())
                    .expect("a valid attestation"),
            ),
        })
        .await
        .expect("an enrolment from a cluster with 12 s of clean speech");
    assert_eq!(enrolled.exemplars, 1);
    assert!(enrolled.speech_ms >= plaudon_app_core::voice_profiles::ENROL_SPEECH_MS);
    let embeds_after_enrolment = pipeline.embedder.call_count();
    assert!(
        embeds_after_enrolment > 0,
        "the enrolment stored an exemplar without embedding any audio"
    );

    // A second recording, matched for real: the job engine claims it, the
    // executor runs the §10 pipeline and the suggestion is written.
    let second = pipeline.transcribed("match.wav").await;
    assert_eq!(pipeline.match_now(second).await, JobState::Completed);
    assert!(
        pipeline.embedder.call_count() > embeds_after_enrolment,
        "the match job produced no embedding, so nothing was really matched"
    );
    let queue = backend
        .list_speaker_suggestions(Some(second), 50)
        .await
        .expect("the panel reads");
    let suggestion = queue
        .suggestions
        .iter()
        .find(|row| row.person.person_id == priya.person_id)
        .unwrap_or_else(|| {
            panic!("the match wrote no suggestion for the enrolled person: {queue:?}")
        });
    assert_eq!(suggestion.state, "pending", "a job assigns nothing");
    assert!(
        suggestion.band == "suggest" || suggestion.band == "weak",
        "unexpected band {:?}",
        suggestion.band
    );

    // What the library now holds is biometric data, which is the thing VP-111
    // is about: the zero below is a zero with a voiceprint on disk.
    let settings = backend
        .voice_profile_settings()
        .await
        .expect("the settings block reads");
    assert_eq!(settings.enrolled_people, 1);
    assert_eq!(settings.exemplars, 1);
    assert!(settings.pending_suggestions >= 1);

    pipeline.stop().await;
    watch.attempts()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_enrol_and_match_authorizes_no_outbound_request_with_local_only_mode_on() {
    let attempts = enrol_and_match_under_the_watch(true).await;
    assert!(
        attempts.is_empty(),
        "the biometric tier authorized {} outbound request(s) under local-only mode: {attempts:?}",
        attempts.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_enrol_and_match_authorizes_no_outbound_request_with_local_only_mode_off() {
    // The same flow, the same watch, the same zero — and that is the claim.
    // Local-only Mode is the user's network switch; a voiceprint's refusal is
    // not conditional on it (`crates/security/src/outbound.rs`:
    // `PayloadClass::never_egressable`).
    let attempts = enrol_and_match_under_the_watch(false).await;
    assert!(
        attempts.is_empty(),
        "the biometric tier authorized {} outbound request(s) with local-only mode off: \
         {attempts:?}",
        attempts.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_watch_catches_the_catalogue_refresh_once_a_key_is_stored() {
    // The negative control: a watch that cannot fail proves nothing. The one
    // flow in this build that reaches the transport without a verified model is
    // the catalogue refresh `list_models` performs once Local-only Mode is off
    // *and* a key is stored. Without this test the two zeros above could just
    // as easily mean the transport was never installed.
    use crate::ports::{CloudPort, SettingsPort};
    use crate::secrets::SecretStore;

    let watch = EgressWatch::new();
    let pipeline = VoicePipeline::watched(&watch).await;
    let backend = Arc::clone(&pipeline.inner.backend);
    backend
        .set_local_only_mode(false)
        .await
        .expect("the mode is stored");

    let query = crate::validate::ValidatedListModels {
        stage: Stage::Transcribe,
        engine: None,
    };
    let before = backend
        .list_models(query.clone())
        .await
        .expect("the picker answers without a key");
    assert_eq!(before.degraded_reason.as_deref(), Some("no_key"));
    assert!(
        watch.attempts().is_empty(),
        "nothing should be attempted before a key exists: {:?}",
        watch.attempts()
    );

    pipeline
        .inner
        .fixture
        .secrets
        .set_openrouter_key(
            &crate::secrets::SecretValue::new("sk-or-v1-0000000000000000000000000000".to_owned())
                .expect("a key-shaped value"),
        )
        .expect("the in-memory store takes the key");

    let after = backend
        .list_models(query)
        .await
        .expect("the picker answers with a key");
    let attempts = watch.attempts();
    assert_eq!(
        attempts.len(),
        1,
        "the catalogue refresh must reach the transport exactly once: {attempts:?}"
    );
    assert_eq!(attempts[0].method, "GET");
    assert_eq!(attempts[0].path, "/api/v1/models");
    // The watch refused the connection, so the catalogue degrades rather than
    // inventing entries, and it says which way it degraded.
    assert_eq!(after.degraded_reason.as_deref(), Some("offline"));
    assert_eq!(after.catalog_source, "pinned_allowlist");

    pipeline.stop().await;
}
