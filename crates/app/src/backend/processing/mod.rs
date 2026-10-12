//! Real execution of queued processing jobs (spec §14).
//!
//! `plaudon-jobs` owns the durable job state machine, the bounded resource
//! scheduler, the cancellation registry and startup recovery; it deliberately
//! owns no worker loop and no executors, because a worker needs the engines
//! and the repositories, and the jobs crate is not allowed to depend on a
//! network stack (`tests/chaos` asserts that). This module is the missing
//! half: a pool of workers running on the engine's own Tokio runtime, and the
//! two stage executors — speech-to-text ([`stt`]) and
//! summarization/extraction ([`text`]).
//!
//! The order of operations per job is the contract:
//!
//! 1. claim the job by moving it `Queued → Preflight` (the store funnels every
//!    write through one task, so exactly one worker wins);
//! 2. preflight against observed disk, memory and model assets — a job that
//!    cannot run is *parked* in a visible waiting state, never failed;
//! 3. take a slot in the job's resource class and hold it for the whole run;
//! 4. run the executor with a cancellation token and a coalescing progress
//!    sink;
//! 5. commit the output, then the terminal transition, and only then publish
//!    the UI event.
//!
//! Nothing here retries by itself. A failure lands in `FailedRetryable` or
//! `FailedTerminal` and waits for the user, and a dispatch whose outcome is
//! unknown lands in `CloudOutcomeUnknown`, which the state machine only lets a
//! user leave with an explicit cost acknowledgement (§14.3, §14.7).

pub mod stt;
pub mod text;

mod progress;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use plaudon_ai_contracts::job::JobContext;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_jobs::engine::{EngineConfig as JobEngineConfig, JobEngine};
use plaudon_jobs::error::JobError;
use plaudon_jobs::key::{JobKey, ProcessingJobKey, ProcessingStage, ReprocessToken};
use plaudon_jobs::reconcile::ReconcileReport;
use plaudon_jobs::scheduler::{
    PowerSource, PreflightDecision, ResourceClass, ResourceRequest, SystemResources,
};
use plaudon_jobs::state::{JobState as EngineJobState, TransitionCause};
use plaudon_jobs::store::{EnqueueRequest, JobOutput, JobRecord};
use plaudon_jobs::writer::Database as JobDatabase;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::backend::jobs::{load, publish_update};
use crate::backend::store::DATABASE_FILE_NAME;
use crate::backend::LocalBackend;
use crate::dto::{JobKind, JobState, JobSummary, OutputLink};
use crate::error::invalid_argument;

pub use progress::ProgressReporter;

/// How long a worker sleeps before re-reading the queue when nothing woke it.
///
/// Enqueues wake the pool directly; the poll is the floor that also picks up
/// work another process (the scripting CLI) left behind.
const IDLE_POLL: Duration = Duration::from_millis(250);

/// Workers started per pool when the caller names no size.
const DEFAULT_WORKERS: usize = 2;

/// States a worker may pick work up from: freshly queued, or a cloud
/// dispatch a user explicitly re-authorized.
const CLAIMABLE: &[EngineJobState] = &[EngineJobState::Queued, EngineJobState::CloudReady];

/// What one executor produced.
#[derive(Debug)]
pub struct Completion {
    /// Durable outputs recorded against the job (§14.1).
    pub outputs: Vec<JobOutput>,
    /// What the UI should open from the finished job.
    pub link: Option<OutputLink>,
}

/// The job engine, its wake-up channel and the engines a test substituted.
///
/// Cloned with the backend; every clone shares one engine and one queue.
#[derive(Clone)]
pub struct ProcessingEngine {
    engine: JobEngine,
    wake: Arc<Notify>,
    inflight: Arc<parking_lot::Mutex<HashSet<JobId>>>,
    seam: stt::EngineSeam,
}

impl std::fmt::Debug for ProcessingEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessingEngine")
            .field("engine", &self.engine)
            .field("in_flight", &self.inflight.lock().len())
            .field("engines", &self.seam)
            .finish()
    }
}

impl ProcessingEngine {
    /// Open the job engine over the library in `root`.
    ///
    /// A second connection pool on the same file is deliberate: the jobs crate
    /// serializes every write through one writer task so a transition is
    /// atomic against the recovery sweep, and that guarantee is only worth
    /// anything if it owns the connection.
    pub(super) async fn open(
        root: &std::path::Path,
        staging_root: std::path::PathBuf,
    ) -> Result<Self, AppError> {
        let db = JobDatabase::open(root.join(DATABASE_FILE_NAME), 4)
            .await
            .map_err(job_error)?;
        let engine = JobEngine::open(db, JobEngineConfig::new(staging_root), Vec::new())
            .await
            .map_err(job_error)?;
        Ok(Self {
            engine,
            wake: Arc::new(Notify::new()),
            inflight: Arc::new(parking_lot::Mutex::new(HashSet::new())),
            seam: stt::EngineSeam::default(),
        })
    }

    /// Replace the local speech engines a worker would otherwise probe for.
    #[must_use]
    pub(super) fn with_stt_engines(
        mut self,
        engines: Vec<Arc<dyn plaudon_ai_contracts::stt::SpeechToText>>,
    ) -> Self {
        self.seam = stt::EngineSeam::substituted(engines);
        self
    }

    /// The durable job engine.
    #[must_use]
    pub fn engine(&self) -> &JobEngine {
        &self.engine
    }

    /// Startup recovery (§13.3A, §14.3, §14.7).
    ///
    /// Must run before the pool starts: the reconciler assumes no job is
    /// running, and it is what turns a process killed mid-job into an honest
    /// state — an interrupted local run becomes retryable, a dispatch with no
    /// recorded response becomes `cloud_outcome_unknown` and is never
    /// re-billed silently, and a half-cancelled job finishes cancelling.
    pub(super) async fn recover(&self) -> Result<ReconcileReport, AppError> {
        self.engine.recover().await.map_err(job_error)
    }

    /// Wake every idle worker.
    pub(super) fn wake(&self) {
        self.wake.notify_waiters();
    }

    /// Take the in-process claim on `job_id`, or `None` when a worker in this
    /// process already holds it.
    fn try_claim(&self, job_id: JobId) -> Option<ClaimGuard> {
        if !self.inflight.lock().insert(job_id) {
            return None;
        }
        Some(ClaimGuard {
            inflight: Arc::clone(&self.inflight),
            job_id,
        })
    }
}

/// Releases the in-process claim when the run ends, however it ends.
#[derive(Debug)]
struct ClaimGuard {
    inflight: Arc<parking_lot::Mutex<HashSet<JobId>>>,
    job_id: JobId,
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        self.inflight.lock().remove(&self.job_id);
    }
}

/// A running pool of job workers.
#[derive(Clone, Debug)]
pub struct ProcessingHandle {
    inner: Arc<PoolState>,
}

#[derive(Debug)]
struct PoolState {
    shutdown: CancellationToken,
    workers: parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl ProcessingHandle {
    /// Start `workers` tasks on the current Tokio runtime.
    ///
    /// Public because [`crate::AppPorts`] carries a `ProcessingHandle` as a
    /// public field and an integration test that composes its own ports — one
    /// that needs a connector factory or a recording cloud transport, which
    /// [`crate::build_ports_with`] does not take — is outside this crate and
    /// therefore cannot call a crate-private constructor. `stop` and
    /// `is_running` were already public; only the constructor was not.
    pub fn start(backend: Arc<LocalBackend>, workers: Option<usize>) -> Self {
        let shutdown = CancellationToken::new();
        let count = workers.unwrap_or(DEFAULT_WORKERS).max(1);
        let mut handles = Vec::with_capacity(count);
        for _ in 0..count {
            let backend = Arc::clone(&backend);
            let shutdown = shutdown.clone();
            handles.push(tokio::spawn(
                async move { run_worker(backend, shutdown).await },
            ));
        }
        Self {
            inner: Arc::new(PoolState {
                shutdown,
                workers: parking_lot::Mutex::new(handles),
            }),
        }
    }

    /// Stop claiming work and wait up to `grace` for the running jobs.
    ///
    /// A job still running when the grace expires is abandoned, which is safe
    /// in exactly the way a power cut is safe: nothing is reported before it
    /// is committed, and the next start reconciles the row.
    pub async fn stop(&self, grace: Duration) {
        self.inner.shutdown.cancel();
        let handles: Vec<_> = std::mem::take(&mut *self.inner.workers.lock());
        for handle in handles {
            if tokio::time::timeout(grace, handle).await.is_err() {
                tracing::warn!("a job worker did not stop within the shutdown grace");
                return;
            }
        }
    }

    /// Whether the pool is still claiming work.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.inner.shutdown.is_cancelled()
    }
}

/// One worker: claim, run, repeat until the pool stops.
async fn run_worker(backend: Arc<LocalBackend>, shutdown: CancellationToken) {
    while !shutdown.is_cancelled() {
        let claimed = match claim(&backend).await {
            Ok(claimed) => claimed,
            Err(error) => {
                tracing::warn!(
                    code = error.code().as_str(),
                    "the job queue could not be read"
                );
                None
            }
        };
        let Some(claim) = claimed else {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                () = backend.processing().wake.notified() => {}
                () = tokio::time::sleep(IDLE_POLL) => {}
            }
            continue;
        };
        execute(&backend, claim, &shutdown).await;
    }
}

/// A job this worker owns until the run ends.
struct Claimed {
    record: JobRecord,
    plan: StagePlan,
    _guard: ClaimGuard,
}

/// Claim the oldest runnable job, or `None` when there is none.
async fn claim(backend: &LocalBackend) -> Result<Option<Claimed>, AppError> {
    let processing = backend.processing();
    for state in CLAIMABLE {
        let waiting = processing
            .engine
            .store()
            .jobs_in_states(&[*state])
            .await
            .map_err(job_error)?;
        for record in waiting {
            let Some(plan) = StagePlan::of(backend, &record) else {
                continue;
            };
            let Some(guard) = processing.try_claim(record.id) else {
                continue;
            };
            if *state == EngineJobState::CloudReady {
                // Already past preflight: a user re-authorized a possibly
                // billed dispatch, and nothing about the machine has to be
                // re-examined before sending it.
                return Ok(Some(Claimed {
                    record,
                    plan,
                    _guard: guard,
                }));
            }
            let system = observed_resources(backend, &plan);
            match processing
                .engine
                .preflight(record.id, plan.request, system)
                .await
            {
                Ok(PreflightDecision::Admit) => {
                    let claimed = processing
                        .engine
                        .store()
                        .get(record.id)
                        .await
                        .map_err(job_error)?;
                    write_step(
                        backend,
                        record.id,
                        EngineJobState::Preflight.as_str(),
                        Some("preflight"),
                        None,
                    )
                    .await?;
                    backend.publish_job(record.id).await;
                    return Ok(Some(Claimed {
                        record: claimed,
                        plan,
                        _guard: guard,
                    }));
                }
                Ok(PreflightDecision::Park { state, reason }) => {
                    tracing::info!(job = %record.id, reason = reason.as_str(), "job parked");
                    write_step(
                        backend,
                        record.id,
                        state.as_str(),
                        Some(reason.as_str()),
                        None,
                    )
                    .await?;
                    backend.publish_job(record.id).await;
                }
                // Another process claimed it between the read and the write.
                Err(JobError::Transition(_) | JobError::JobNotFound(_)) => {}
                Err(other) => return Err(job_error(other)),
            }
        }
    }
    Ok(None)
}

/// Run one claimed job to a terminal state.
async fn execute(backend: &Arc<LocalBackend>, claimed: Claimed, shutdown: &CancellationToken) {
    let processing = backend.processing();
    let job = claimed.record;
    let plan = claimed.plan;
    let job_id = job.id;

    let admission = match processing.engine.admit(job_id, plan.class).await {
        Ok(admission) => admission,
        Err(JobError::Cancelled(_)) => {
            finish_cancellation(backend, job_id).await;
            return;
        }
        Err(error) => {
            settle_error(backend, job_id, &job_error(error)).await;
            return;
        }
    };

    let running = if plan.class == ResourceClass::CloudDispatch {
        EngineJobState::CloudReady
    } else {
        EngineJobState::RunningLocal
    };
    if job.state != running {
        if let Err(error) = transition(backend, job_id, running, plan.stage_label).await {
            drop(admission);
            settle_error(backend, job_id, &error).await;
            return;
        }
    }

    let cancel = processing.engine.token(job_id);
    let reporter = ProgressReporter::start(Arc::clone(backend), job_id, plan.stage_label);
    let run = RunContext {
        backend: Arc::clone(backend),
        job_id,
        config: job.config_snapshot.clone(),
        recording_id: job.recording_id,
        ctx: JobContext {
            job_id,
            cancel: cancel.clone(),
            progress: reporter.sink(),
        },
    };

    let outcome = tokio::select! {
        biased;
        // A pool shutdown abandons the attempt the way a crash does, so the
        // row is left exactly as startup recovery would leave it.
        () = shutdown.cancelled() => Err(AppError::new(
            AppErrorCode::HelperFailed,
            "the application stopped before this job finished",
        )),
        outcome = dispatch_executor(&run, plan.kind) => outcome,
    };
    reporter.stop().await;
    drop(run);
    drop(admission);

    match outcome {
        Ok(completion) => commit(backend, job_id, completion).await,
        Err(_) if cancel.is_cancelled() => finish_cancellation(backend, job_id).await,
        Err(error) => settle_error(backend, job_id, &error).await,
    }
}

/// The executor for one job kind.
async fn dispatch_executor(run: &RunContext, kind: JobKind) -> Result<Completion, AppError> {
    match kind {
        JobKind::Transcribe => stt::transcribe(run).await,
        JobKind::Summarize => text::summarize(run).await,
        other => Err(AppError::new(
            AppErrorCode::CapabilityUnverified,
            format!("this build has no executor for {} jobs", other.as_str()),
        )),
    }
}

/// Commit the outputs and complete the job.
async fn commit(backend: &LocalBackend, job_id: JobId, completion: Completion) {
    if let Some(link) = &completion.link {
        if let Err(error) = write_output_link(backend, job_id, link).await {
            tracing::warn!(
                code = error.code().as_str(),
                "the job's output link could not be recorded"
            );
        }
    }
    let result = async {
        transition(backend, job_id, EngineJobState::Verifying, "verifying").await?;
        backend
            .processing()
            .engine
            .complete(job_id, completion.outputs)
            .await
            .map_err(job_error)?;
        write_step(
            backend,
            job_id,
            EngineJobState::Completed.as_str(),
            None,
            Some(1.0),
        )
        .await
    }
    .await;
    match result {
        Ok(()) => backend.publish_job(job_id).await,
        Err(error) => settle_error(backend, job_id, &error).await,
    }
}

/// Move a job on, record the step and tell the interface, in that order.
async fn transition(
    backend: &LocalBackend,
    job_id: JobId,
    state: EngineJobState,
    stage: &str,
) -> Result<(), AppError> {
    backend
        .processing()
        .engine
        .store()
        .transition(job_id, state, TransitionCause::system(stage))
        .await
        .map_err(job_error)?;
    write_step(backend, job_id, state.as_str(), Some(stage), None).await?;
    backend.publish_job(job_id).await;
    Ok(())
}

/// Record how a run ended when it did not produce an output.
async fn settle_error(backend: &LocalBackend, job_id: JobId, error: &AppError) {
    let engine = backend.processing().engine.clone();
    let unknown = error.code() == AppErrorCode::OutcomeUnknown;
    let settled = if unknown {
        // Possibly billed: only a user may leave this state, and only with an
        // explicit cost acknowledgement.
        engine
            .store()
            .transition(
                job_id,
                EngineJobState::CloudOutcomeUnknown,
                TransitionCause::system("no_response"),
            )
            .await
            .map(|_| ())
    } else {
        engine
            .fail(
                job_id,
                !error.is_retryable(),
                error.code(),
                error.code().as_str(),
            )
            .await
            .map(|_| ())
    };
    if let Err(storage) = settled {
        tracing::warn!(
            code = job_error(storage).code().as_str(),
            "the end state of a job could not be persisted"
        );
    }
    let state = if unknown {
        EngineJobState::CloudOutcomeUnknown
    } else if error.is_retryable() {
        EngineJobState::FailedRetryable
    } else {
        EngineJobState::FailedTerminal
    };
    if let Err(step) = write_step(backend, job_id, state.as_str(), Some("failed"), None).await {
        tracing::warn!(code = step.code().as_str(), "a job step was not recorded");
    }
    if let Err(stored) = write_error_code(backend, job_id, error.code()).await {
        tracing::warn!(
            code = stored.code().as_str(),
            "a job error code was not recorded"
        );
    }
    backend.publish_job(job_id).await;
}

/// Finish a cancellation the executor observed.
async fn finish_cancellation(backend: &LocalBackend, job_id: JobId) {
    let engine = backend.processing().engine.clone();
    // Idempotent: a job already `cancelling` stays there, and a run the pool
    // stopped still has to reach a terminal state.
    if let Err(error) = engine.cancel(job_id).await {
        tracing::debug!(
            code = job_error(error).code().as_str(),
            "cancellation was already recorded"
        );
    }
    match engine.finish_cancellation(job_id, Vec::new()).await {
        Ok(_) => {
            if let Err(step) = write_step(
                backend,
                job_id,
                EngineJobState::Cancelled.as_str(),
                Some("cancelled"),
                None,
            )
            .await
            {
                tracing::warn!(code = step.code().as_str(), "a job step was not recorded");
            }
        }
        Err(error) => tracing::warn!(
            code = job_error(error).code().as_str(),
            "a cancellation did not finish"
        ),
    }
    backend.publish_job(job_id).await;
}

/* ------------------------------------------------------------------ plans */

/// How one job kind is scheduled and announced.
#[derive(Debug, Clone, Copy)]
struct StagePlan {
    kind: JobKind,
    class: ResourceClass,
    request: ResourceRequest,
    stage_label: &'static str,
    model_asset_present: bool,
}

impl StagePlan {
    /// The plan for a queued row, or `None` when this build runs that kind
    /// elsewhere (import and export are driven by their own commands).
    fn of(backend: &LocalBackend, record: &JobRecord) -> Option<Self> {
        let kind = JobKind::parse(&record.job_type)?;
        let cloud = record.resource_class == ResourceClass::CloudDispatch;
        let (stage_label, model_asset_present) = match kind {
            JobKind::Transcribe => (
                if cloud { "sending" } else { "transcribing" },
                cloud || stt::a_local_engine_is_installed(backend),
            ),
            JobKind::Summarize => (if cloud { "sending" } else { "summarizing" }, true),
            _ => return None,
        };
        Some(Self {
            kind,
            class: record.resource_class,
            request: ResourceRequest {
                required_disk_bytes: 0,
                estimated_ram_bytes: 0,
                needs_model_asset: !cloud,
                needs_source: false,
                heavy_local_compute: !cloud,
            },
            stage_label,
            model_asset_present,
        })
    }
}

/// What preflight compares the job's requirements against.
fn observed_resources(backend: &LocalBackend, plan: &StagePlan) -> SystemResources {
    SystemResources {
        available_disk_bytes: backend.storage_capability().free_bytes,
        available_ram_bytes: available_memory_bytes(),
        // No power API is wired into this build, and guessing "battery" would
        // park work that should run.
        power_source: PowerSource::Unknown,
        power_saver_enabled: false,
        model_asset_present: plan.model_asset_present,
        source_available: true,
    }
}

fn available_memory_bytes() -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.available_memory()
}

/* -------------------------------------------------------------- executors */

/// Everything an executor is given.
pub struct RunContext {
    backend: Arc<LocalBackend>,
    /// Identity of the job being run.
    pub job_id: JobId,
    /// The durable request the job was opened with.
    pub config: Value,
    /// Recording the job belongs to, when it has one.
    pub recording_id: Option<RecordingId>,
    /// Cancellation and progress, for the engine being driven.
    pub ctx: JobContext,
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("job_id", &self.job_id)
            .finish_non_exhaustive()
    }
}

impl RunContext {
    /// The backend the executor commits through.
    #[must_use]
    pub fn backend(&self) -> &LocalBackend {
        &self.backend
    }

    /// A string field of the durable request.
    #[must_use]
    pub fn text(&self, key: &str) -> Option<&str> {
        self.config.get(key).and_then(Value::as_str)
    }

    /// A boolean field of the durable request, defaulting to `false`.
    #[must_use]
    pub fn flag(&self, key: &str) -> bool {
        self.config.get(key).and_then(Value::as_bool) == Some(true)
    }

    /// The recording this job must have.
    pub fn recording(&self) -> Result<RecordingId, AppError> {
        self.recording_id.ok_or_else(|| {
            AppError::new(
                AppErrorCode::InternalError,
                "a processing job has no recording",
            )
        })
    }

    /// Announce the stage a long run has reached.
    pub async fn stage(&self, label: &str) {
        let state = match self
            .backend
            .processing()
            .engine
            .store()
            .get(self.job_id)
            .await
        {
            Ok(record) => record.state,
            Err(_) => return,
        };
        if let Err(error) = write_step(
            &self.backend,
            self.job_id,
            state.as_str(),
            Some(label),
            None,
        )
        .await
        {
            tracing::warn!(code = error.code().as_str(), "a job stage was not recorded");
        }
        self.backend.publish_job(self.job_id).await;
    }

    /// Move a cloud job from "ready to send" to "sending".
    ///
    /// The transition carries the consent flag the state machine demands, so
    /// a dispatch that never passed the consent gate cannot reach it.
    pub async fn enter_cloud_sending(&self) -> Result<(), AppError> {
        self.backend
            .processing()
            .engine
            .store()
            .transition(
                self.job_id,
                EngineJobState::CloudSending,
                TransitionCause::system("dispatch").with_consent_verified(),
            )
            .await
            .map_err(job_error)?;
        write_step(
            &self.backend,
            self.job_id,
            EngineJobState::CloudSending.as_str(),
            Some("sending"),
            None,
        )
        .await?;
        self.backend.publish_job(self.job_id).await;
        Ok(())
    }
}

/* --------------------------------------------------------------- enqueue */

/// A stage the user asked for.
#[derive(Debug, Clone)]
pub struct StageRequest {
    /// Recording the stage runs over.
    pub recording_id: RecordingId,
    /// Job family, which selects the executor.
    pub kind: JobKind,
    /// Engine the user's preference resolved to.
    pub engine: String,
    /// Model the user's preference resolved to, when the engine has one.
    pub model_id: Option<String>,
    /// Stage name the wire contract uses (`transcribe`, `summarize`, …).
    pub stage: String,
    /// Label the job list shows.
    pub label: String,
    /// Digest of the audio the stage reads, which keys the job.
    pub audio_sha256: String,
    /// Whether the work leaves the machine.
    pub cloud: bool,
    /// Whether the user acknowledged a cost that cannot be calculated.
    pub cost_acknowledged: bool,
}

impl StageRequest {
    /// The durable request an executor reads back.
    fn config(&self) -> Value {
        json!({
            "engine": self.engine,
            "model_id": self.model_id,
            "stage": self.stage,
            "label": self.label,
            "recording_id": self.recording_id.to_string(),
            "audio_sha256": self.audio_sha256,
            "cost_acknowledged": self.cost_acknowledged,
        })
    }

    /// The canonical key this stage deduplicates on (§14.2).
    fn key(&self) -> ProcessingJobKey {
        ProcessingJobKey::new(
            self.audio_sha256.clone(),
            processing_stage(self.kind),
            self.engine.clone(),
            self.model_id.clone().unwrap_or_default(),
        )
    }

    /// Rebuild a request from the durable snapshot of an earlier run.
    fn from_config(kind: JobKind, recording_id: RecordingId, config: &Value) -> Option<Self> {
        let text = |key: &str| config.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            recording_id,
            kind,
            engine: text("engine")?,
            model_id: text("model_id"),
            stage: text("stage").unwrap_or_else(|| kind.as_str().to_owned()),
            label: text("label").unwrap_or_else(|| kind.as_str().to_owned()),
            audio_sha256: text("audio_sha256")?,
            cloud: config.get("cloud").and_then(Value::as_bool) == Some(true),
            cost_acknowledged: config.get("cost_acknowledged").and_then(Value::as_bool)
                == Some(true),
        })
    }
}

/// The processing stage of one job family.
const fn processing_stage(kind: JobKind) -> ProcessingStage {
    match kind {
        JobKind::Summarize => ProcessingStage::StructuredExtraction,
        _ => ProcessingStage::Transcribe,
    }
}

impl LocalBackend {
    /// The job engine and the queue.
    #[must_use]
    pub fn processing(&self) -> &ProcessingEngine {
        &self.processing
    }

    /// The text model a test installed in place of a resolved route.
    #[must_use]
    pub(crate) fn installed_text_model(
        &self,
    ) -> Option<Arc<dyn plaudon_ai_contracts::text::TextModel>> {
        self.cloud.text_model().map(Arc::clone)
    }

    /// The built-in templates plus the user's copies.
    pub(crate) fn template_registry(
        &self,
    ) -> Result<plaudon_app_core::templates::TemplateRegistry, AppError> {
        crate::backend::templates::open_registry(&self.store().root().join("templates"))
    }

    /// Enqueue one stage, deduplicating on its canonical key.
    ///
    /// An identical stage that has not finished is *reused* rather than
    /// duplicated (§14.2); a stage whose previous run already reached a
    /// terminal state runs again under a fresh reprocess token, because
    /// asking for a finished stage again means "do it again".
    pub(crate) async fn enqueue_stage(&self, request: &StageRequest) -> Result<JobId, AppError> {
        let class = if request.cloud {
            ResourceClass::CloudDispatch
        } else {
            ResourceClass::Cpu
        };
        let mut config = request.config();
        if let Some(object) = config.as_object_mut() {
            object.insert("cloud".to_owned(), json!(request.cloud));
        }
        let enqueue = |key: ProcessingJobKey| {
            EnqueueRequest::new(request.kind.as_str(), JobKey::Processing(key), class)
                .with_config_snapshot(config.clone())
                .with_recording(request.recording_id)
                .with_input_hash(request.audio_sha256.clone())
        };

        let outcome = self
            .processing
            .engine
            .enqueue(enqueue(request.key()))
            .await
            .map_err(job_error)?;
        let mut job_id = outcome.job_id();
        if !outcome.is_created() {
            let existing = self
                .processing
                .engine
                .store()
                .get(job_id)
                .await
                .map_err(job_error)?;
            if existing.state.is_terminal() {
                let again = self
                    .processing
                    .engine
                    .enqueue(enqueue(request.key().with_reprocess(ReprocessToken::new())))
                    .await
                    .map_err(job_error)?;
                job_id = again.job_id();
            }
        }
        write_step(
            self,
            job_id,
            EngineJobState::Queued.as_str(),
            Some("queued"),
            None,
        )
        .await?;
        self.processing.wake();
        self.publish_job(job_id).await;
        Ok(job_id)
    }

    /// Whether `job_id` is owned by the job engine rather than by the
    /// command-driven import and export ladders.
    pub(crate) async fn is_engine_job(&self, job_id: JobId) -> bool {
        self.processing.engine.store().get(job_id).await.is_ok()
    }

    /// Retry a job the engine owns.
    ///
    /// Three different things, because three different things happened:
    /// a retryable failure is simply queued again; a possibly-billed dispatch
    /// is re-authorized by the user and re-sent (the caller has already
    /// checked the acknowledgement); and a cancelled stage cannot be resumed
    /// at all — it is enqueued again as an explicit reprocess, and the new
    /// job is what the caller is told about.
    pub(crate) async fn retry_engine_job(
        &self,
        job_id: JobId,
        current: &JobSummary,
    ) -> Result<JobSummary, AppError> {
        let engine = self.processing.engine.clone();
        match current.state {
            JobState::FailedRetryable => {
                engine
                    .store()
                    .transition(
                        job_id,
                        EngineJobState::Queued,
                        TransitionCause::user("explicit_retry"),
                    )
                    .await
                    .map_err(job_error)?;
            }
            JobState::CloudOutcomeUnknown => {
                engine
                    .user_retry_possibly_billed(job_id)
                    .await
                    .map_err(job_error)?;
            }
            JobState::Cancelled => {
                let record = engine.store().get(job_id).await.map_err(job_error)?;
                let recording = record.recording_id.ok_or_else(|| {
                    invalid_argument("a cancelled job with no recording cannot be run again")
                })?;
                let request =
                    StageRequest::from_config(current.kind, recording, &record.config_snapshot)
                        .ok_or_else(|| {
                            invalid_argument("this job predates the executor and cannot be re-run")
                        })?;
                let replacement = self.enqueue_stage(&request).await?;
                return load(self.pool(), replacement).await;
            }
            other => {
                return Err(invalid_argument(format!(
                    "a job in state {} cannot be retried",
                    other.as_str()
                )))
            }
        }
        write_step(
            self,
            job_id,
            EngineJobState::Queued.as_str(),
            Some("queued"),
            None,
        )
        .await?;
        self.processing.wake();
        let summary = load(self.pool(), job_id).await?;
        publish_update(self.events(), &summary).await;
        Ok(summary)
    }
}

/* ------------------------------------------------- durable UI-facing rows */

/// Append a step row for a job the engine owns.
///
/// Written through the engine's **own** writer rather than the application's
/// connection pool: two pools writing the same SQLite file race for the
/// write lock, and a busy snapshot would fail a transition that already
/// happened. One writer, one order.
pub(crate) async fn write_step(
    backend: &LocalBackend,
    job_id: JobId,
    state: &str,
    stage: Option<&str>,
    progress: Option<f64>,
) -> Result<(), AppError> {
    let mut row = serde_json::Map::new();
    if let Some(stage) = stage {
        row.insert("stage".to_owned(), json!(stage));
    }
    if let Some(progress) = progress {
        row.insert("ratio".to_owned(), json!(progress));
    }
    let progress_json = Value::Object(row).to_string();
    let state = state.to_owned();
    let at = plaudon_domain::time::Timestamp::now().as_millis();
    let id = job_id.to_string();
    backend
        .processing()
        .engine
        .store()
        .database()
        .writer()
        .submit(move |mut conn| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO job_steps (job_id, sequence, state, attempt, started_at, \
                     ended_at, progress_json) VALUES (?1, \
                     (SELECT COALESCE(MAX(sequence) + 1, 0) FROM job_steps WHERE job_id = ?1), \
                     ?2, 0, ?3, ?3, ?4)",
                )
                .bind(&id)
                .bind(&state)
                .bind(at)
                .bind(&progress_json)
                .execute(&mut *conn)
                .await
                .map_err(|err| JobError::storage("append job step", err))?;
                Ok(())
            })
        })
        .await
        .map_err(job_error)
}

/// Record what a finished job produced, for the UI's "open" affordance.
async fn write_output_link(
    backend: &LocalBackend,
    job_id: JobId,
    link: &OutputLink,
) -> Result<(), AppError> {
    let id = job_id.to_string();
    let kind = match link.kind {
        crate::dto::OutputLinkKind::Recording => "recording",
        crate::dto::OutputLinkKind::Export => "export",
    };
    let patch = json!({ "kind": kind, "id": link.id }).to_string();
    backend
        .processing()
        .engine
        .store()
        .database()
        .writer()
        .submit(move |mut conn| {
            Box::pin(async move {
                sqlx::query(
                    "UPDATE jobs SET config_snapshot_json = \
                     json_set(config_snapshot_json, '$.output_link', json(?2)) WHERE id = ?1",
                )
                .bind(&id)
                .bind(&patch)
                .execute(&mut *conn)
                .await
                .map_err(|err| JobError::storage("record job output link", err))?;
                Ok(())
            })
        })
        .await
        .map_err(job_error)
}

/// Persist the stable code a failed job renders.
async fn write_error_code(
    backend: &LocalBackend,
    job_id: JobId,
    code: AppErrorCode,
) -> Result<(), AppError> {
    let id = job_id.to_string();
    let code = code.as_str().to_owned();
    backend
        .processing()
        .engine
        .store()
        .database()
        .writer()
        .submit(move |mut conn| {
            Box::pin(async move {
                sqlx::query("UPDATE jobs SET last_error_code = ?2 WHERE id = ?1")
                    .bind(&id)
                    .bind(&code)
                    .execute(&mut *conn)
                    .await
                    .map_err(|err| JobError::storage("record job error code", err))?;
                Ok(())
            })
        })
        .await
        .map_err(job_error)
}

/// Map a job-engine failure onto the application's error envelope.
pub(crate) fn job_error(error: JobError) -> AppError {
    AppError::from(error)
}
