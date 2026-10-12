//! Transactional job store (SPEC v1.3 §13.1, §13.2, §13.3A, §14).
//!
//! Every mutation here runs inside a single SQLite transaction on the
//! single-writer connection. A transition, the rows it depends on and the
//! durable events it produces commit or roll back together; nothing the UI can
//! observe exists outside a committed transaction.

use std::collections::BTreeMap;
use std::path::Path;

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::error::JobError;
use crate::events::{EventBuffer, EventKind, EventLog, EventPayload, EventRecord, PendingEvent};
use crate::key::{JobKey, JobKeyDigest};
use crate::scheduler::{ResourceClass, WaitReason};
use crate::state::{check_transition, GuardFacts, JobState, JobTransitionError, TransitionCause};
use crate::writer::{BoxFut, Database};

/// Columns the engine requires on the core `jobs` table.
const REQUIRED_JOB_COLUMNS: &[&str] = &[
    "id",
    "job_type",
    "state",
    "priority",
    "retry_count",
    "job_key",
    "config_snapshot_json",
    "input_hash",
    "created_at",
    "started_at",
    "finished_at",
    "last_error_code",
];

/// A persisted job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: JobId,
    /// Caller-defined job type, e.g. `import`, `transcribe`.
    pub job_type: String,
    pub state: JobState,
    pub priority: i64,
    pub retry_count: i64,
    pub key_digest: JobKeyDigest,
    pub key_kind: String,
    /// Immutable snapshot of the model/policy configuration captured at
    /// creation (§6.3(4)).
    pub config_snapshot: serde_json::Value,
    pub input_hash: Option<String>,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub last_error_code: Option<String>,
    pub resource_class: ResourceClass,
    pub wait_reason: Option<WaitReason>,
    pub is_reprocess: bool,
    pub reprocess_of: Option<JobId>,
    pub recording_id: Option<RecordingId>,
    pub consent_id: Option<String>,
    /// `true` once content has left the machine for a provider.
    pub dispatched: bool,
    pub cancel_requested_at: Option<Timestamp>,
}

impl JobRecord {
    /// `true` when the job may already have incurred a provider charge.
    pub fn possibly_billed(&self) -> bool {
        self.dispatched && self.state.is_possibly_billed()
    }
}

/// What to enqueue.
#[derive(Debug, Clone)]
pub struct EnqueueRequest {
    pub job_type: String,
    pub key: JobKey,
    pub priority: i64,
    pub resource_class: ResourceClass,
    pub config_snapshot: serde_json::Value,
    pub input_hash: Option<String>,
    pub recording_id: Option<RecordingId>,
    pub consent_id: Option<String>,
    /// Set when this job is an explicit reprocessing of an earlier one.
    pub reprocess_of: Option<JobId>,
}

impl EnqueueRequest {
    /// Minimal request with sensible defaults.
    pub fn new(job_type: impl Into<String>, key: impl Into<JobKey>, class: ResourceClass) -> Self {
        Self {
            job_type: job_type.into(),
            key: key.into(),
            priority: 0,
            resource_class: class,
            config_snapshot: serde_json::Value::Object(serde_json::Map::new()),
            input_hash: None,
            recording_id: None,
            consent_id: None,
            reprocess_of: None,
        }
    }

    /// Attach the immutable configuration snapshot.
    #[must_use]
    pub fn with_config_snapshot(mut self, snapshot: serde_json::Value) -> Self {
        self.config_snapshot = snapshot;
        self
    }

    /// Attach the input content hash.
    #[must_use]
    pub fn with_input_hash(mut self, hash: impl Into<String>) -> Self {
        self.input_hash = Some(hash.into());
        self
    }

    /// Attach the recording this job belongs to.
    #[must_use]
    pub fn with_recording(mut self, recording: RecordingId) -> Self {
        self.recording_id = Some(recording);
        self
    }

    /// Attach the consent grant covering this job.
    #[must_use]
    pub fn with_consent(mut self, consent_id: impl Into<String>) -> Self {
        self.consent_id = Some(consent_id.into());
        self
    }

    /// Record that this job reprocesses an earlier one.
    #[must_use]
    pub fn reprocessing(mut self, original: JobId) -> Self {
        self.reprocess_of = Some(original);
        self
    }
}

/// Result of an enqueue (§14.2 idempotency).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EnqueueOutcome {
    /// A new job row was created.
    Created { job_id: JobId },
    /// An identical key already existed; the existing job is returned and no
    /// second job was created.
    Deduplicated { job_id: JobId },
}

impl EnqueueOutcome {
    /// The job identifier either way.
    pub fn job_id(self) -> JobId {
        match self {
            EnqueueOutcome::Created { job_id } | EnqueueOutcome::Deduplicated { job_id } => job_id,
        }
    }

    /// `true` when a new row was inserted.
    pub fn is_created(self) -> bool {
        matches!(self, EnqueueOutcome::Created { .. })
    }
}

/// A durable job output. A job may only reach `Completed` once at least one of
/// these is committed in the same transaction (§14.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutput {
    /// What the output is, e.g. `media_asset`, `transcript_revision`.
    pub kind: String,
    /// Opaque reference: an identifier or archive-relative path, never an
    /// absolute filesystem path and never a signed URL.
    pub reference: String,
    pub sha256: Option<String>,
    pub byte_length: Option<i64>,
}

impl JobOutput {
    /// Output identified by reference only.
    pub fn new(kind: impl Into<String>, reference: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            reference: reference.into(),
            sha256: None,
            byte_length: None,
        }
    }

    /// Attach the verified digest and length.
    #[must_use]
    pub fn verified(mut self, sha256: impl Into<String>, byte_length: i64) -> Self {
        self.sha256 = Some(sha256.into());
        self.byte_length = Some(byte_length);
        self
    }
}

/// A stage checkpoint row (§14.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub job_id: JobId,
    pub stage: String,
    pub stage_index: i64,
    pub input_digest: String,
    pub output_ref: Option<String>,
    pub output_digest: Option<String>,
    pub attempt: i64,
    pub prepared_at: Timestamp,
    pub committed_at: Option<Timestamp>,
}

impl Checkpoint {
    /// `true` when the stage's output is durable and may be reused.
    pub fn is_committed(&self) -> bool {
        self.committed_at.is_some()
    }
}

/// What a resumed job must do with one stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum StageAction {
    /// The stage has a commit mark; reuse its output byte-identically.
    Skip {
        stage: String,
        output_ref: String,
        output_digest: Option<String>,
    },
    /// The stage must run (never prepared, or prepared but not committed).
    Run { stage: String, attempt: i64 },
}

impl StageAction {
    /// Stage name either way.
    pub fn stage(&self) -> &str {
        match self {
            StageAction::Skip { stage, .. } | StageAction::Run { stage, .. } => stage,
        }
    }
}

/// Dispatch state of a cloud attempt row (§10.7).
///
/// The vocabulary is the shared `inference_attempts.dispatch_state` CHECK set
/// owned by `crates/persistence`; the engine does not define a second enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    /// Ledger row written; nothing has left the machine.
    NotSent,
    /// Request handed to the transport; bytes may already have left.
    Sending,
    /// The provider acknowledged receipt but no result is recorded yet.
    AcceptedByProvider,
    /// Sent with no recorded response: possibly billed, never auto-retried.
    OutcomeUnknown,
    /// A response was received and recorded.
    Completed,
    /// Refused or cancelled before any byte was sent.
    FailedBeforeSend,
}

impl DispatchState {
    /// All states.
    pub const ALL: &'static [DispatchState] = &[
        DispatchState::NotSent,
        DispatchState::Sending,
        DispatchState::AcceptedByProvider,
        DispatchState::OutcomeUnknown,
        DispatchState::Completed,
        DispatchState::FailedBeforeSend,
    ];

    /// Stable persisted name, matching the shared CHECK constraint.
    pub const fn as_str(self) -> &'static str {
        match self {
            DispatchState::NotSent => "not_sent",
            DispatchState::Sending => "sending",
            DispatchState::AcceptedByProvider => "accepted_by_provider",
            DispatchState::OutcomeUnknown => "outcome_unknown",
            DispatchState::Completed => "completed",
            DispatchState::FailedBeforeSend => "failed_before_send",
        }
    }

    /// Parse a persisted name.
    pub fn parse(raw: &str) -> Option<DispatchState> {
        DispatchState::ALL
            .iter()
            .copied()
            .find(|s| s.as_str() == raw)
    }

    /// `true` when bytes may already have reached the provider, so a retry
    /// could be billed twice (§10.4, §14.3).
    pub const fn may_have_been_billed(self) -> bool {
        matches!(
            self,
            DispatchState::Sending
                | DispatchState::AcceptedByProvider
                | DispatchState::OutcomeUnknown
                | DispatchState::Completed
        )
    }
}

/// A cloud request ledger row (§10.7). Never contains the API secret or a
/// signed URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceAttempt {
    pub id: String,
    pub job_id: JobId,
    pub content_sha256: String,
    pub request_family: String,
    pub provider: Option<String>,
    pub model: String,
    pub route: Option<String>,
    pub privacy_policy_snapshot: Option<String>,
    pub consent_id: Option<String>,
    pub status: DispatchState,
    pub provider_request_id: Option<String>,
    pub dispatched_at: Option<Timestamp>,
    pub responded_at: Option<Timestamp>,
}

/// Transactional access to job state.
#[derive(Clone)]
pub struct JobStore {
    db: Database,
    log: EventLog,
}

impl std::fmt::Debug for JobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JobStore")
    }
}

impl JobStore {
    /// Open a store over `db`, applying the job-engine schema.
    pub async fn open(db: Database) -> Result<Self, JobError> {
        let pool = db.reader().pool().clone();
        let store = Self {
            db,
            log: EventLog::new(pool),
        };
        store.verify_schema().await?;
        Ok(store)
    }

    /// Migrate and open a store at `path`.
    pub async fn open_at(path: impl AsRef<Path>) -> Result<Self, JobError> {
        Self::migrate(path.as_ref()).await?;
        Self::open(Database::open(path, 4).await?).await
    }

    /// Filesystem path of the database file.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// The underlying database.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Read-only event log.
    pub fn events(&self) -> &EventLog {
        &self.log
    }

    /// Read pool for list/history queries.
    pub fn read_pool(&self) -> &SqlitePool {
        self.db.reader().pool()
    }

    /// Create or upgrade the whole schema through the single persistence
    /// migration runner.
    ///
    /// `plaudon_persistence::migrations::MIGRATIONS` carries the job-engine
    /// file as version 100, so there is exactly one runner, one
    /// `schema_migrations` history and one per-file digest. Idempotent.
    pub async fn migrate(path: impl AsRef<Path>) -> Result<(), JobError> {
        let db = plaudon_persistence::Database::open(path.as_ref())
            .await
            .map_err(|e| JobError::Migration(e.to_string()))?;
        plaudon_persistence::migrations::MigrationRunner::new()
            .run(&db)
            .await
            .map_err(|e| JobError::Migration(e.to_string()))?;
        db.close().await;
        Ok(())
    }

    /// Verify that the core `jobs` table carries every column the engine needs.
    ///
    /// Guards against silent divergence if `crates/persistence` changes its
    /// definition of a shared table.
    pub async fn verify_schema(&self) -> Result<(), JobError> {
        let rows = sqlx::query("PRAGMA table_info(jobs)")
            .fetch_all(self.read_pool())
            .await
            .map_err(|e| JobError::storage("inspect jobs table", e))?;
        let present: Vec<String> = rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .collect();
        for required in REQUIRED_JOB_COLUMNS {
            if !present.iter().any(|c| c == required) {
                return Err(JobError::CorruptRecord(format!(
                    "the `jobs` table is missing the required column `{required}`"
                )));
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Enqueue / idempotency
    // -----------------------------------------------------------------------

    /// Enqueue a job, deduplicating on the canonical key digest.
    ///
    /// Concurrent enqueues of the same key are serialized by the single writer
    /// and collapse onto one row; the loser observes `Deduplicated`.
    pub async fn enqueue(&self, request: EnqueueRequest) -> Result<EnqueueOutcome, JobError> {
        let digest = request.key.digest();
        let canonical = request.key.canonical_text();
        let key_kind = request.key.kind_str().to_string();
        let is_reprocess = request.key.is_reprocess();
        let reprocess_token = request
            .key
            .reprocess_token()
            .map(|t| t.as_uuid().to_string());
        let config = serde_json::to_string(&request.config_snapshot)
            .map_err(|e| JobError::encoding("job config snapshot", e))?;
        let now = Timestamp::now();

        let (outcome, events) = self
            .db
            .writer()
            .transaction(move |tx| {
                Box::pin(async move {
                    let mut buffer = EventBuffer::new();
                    let existing: Option<String> =
                        sqlx::query_scalar("SELECT id FROM jobs WHERE job_key = ?1")
                            .bind(digest.as_str())
                            .fetch_optional(&mut **tx)
                            .await
                            .map_err(|e| JobError::storage("look up job key", e))?;
                    if let Some(raw) = existing {
                        let job_id = parse_job_id(&raw)?;
                        return Ok((EnqueueOutcome::Deduplicated { job_id }, Vec::new()));
                    }

                    let job_id = JobId::new();
                    sqlx::query(
                        "INSERT INTO jobs (id, job_type, state, priority, retry_count, job_key,
                                           config_snapshot_json, input_hash, created_at)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8)",
                    )
                    .bind(job_id.to_string())
                    .bind(&request.job_type)
                    .bind(JobState::Queued.as_str())
                    .bind(request.priority)
                    .bind(digest.as_str())
                    .bind(&config)
                    .bind(request.input_hash.as_deref())
                    .bind(now.as_millis())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| JobError::storage("insert job", e))?;

                    sqlx::query(
                        "INSERT INTO job_engine_state (job_id, key_kind, key_canonical,
                             is_reprocess, reprocess_token, reprocess_of, resource_class,
                             consent_id, recording_id, dispatched, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
                    )
                    .bind(job_id.to_string())
                    .bind(&key_kind)
                    .bind(&canonical)
                    .bind(i64::from(is_reprocess))
                    .bind(reprocess_token.as_deref())
                    .bind(request.reprocess_of.map(|id| id.to_string()))
                    .bind(request.resource_class.as_str())
                    .bind(request.consent_id.as_deref())
                    .bind(request.recording_id.map(|id| id.to_string()))
                    .bind(now.as_millis())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| JobError::storage("insert job engine state", e))?;

                    buffer.push(PendingEvent::for_job(
                        job_id,
                        EventKind::JobUpdated,
                        EventPayload::JobUpdated {
                            state: JobState::Queued,
                            previous_state: JobState::Queued,
                            wait_reason: None,
                            possibly_billed: false,
                        },
                    ));
                    let events = buffer.flush(tx, now).await?;
                    Ok((EnqueueOutcome::Created { job_id }, events))
                })
            })
            .await?;

        Self::after_commit(&events);
        Ok(outcome)
    }

    // -----------------------------------------------------------------------
    // Transitions
    // -----------------------------------------------------------------------

    /// Apply a state transition transactionally.
    pub async fn transition(
        &self,
        job: JobId,
        to: JobState,
        cause: TransitionCause,
    ) -> Result<JobRecord, JobError> {
        self.transition_inner(job, to, cause, None, Vec::new(), Vec::new())
            .await
    }

    /// Apply a transition into a waiting state, recording why (§14.4).
    pub async fn park(
        &self,
        job: JobId,
        reason: WaitReason,
        cause: TransitionCause,
    ) -> Result<JobRecord, JobError> {
        self.transition_inner(
            job,
            reason.wait_state(),
            cause,
            Some(reason),
            Vec::new(),
            Vec::new(),
        )
        .await
    }

    /// Commit outputs and transition to `Completed` in one transaction.
    ///
    /// The `Completed` guard reads the output rows written by this very
    /// transaction, so there is no window in which a job is complete without
    /// durable outputs.
    pub async fn complete(
        &self,
        job: JobId,
        outputs: Vec<JobOutput>,
        cause: TransitionCause,
    ) -> Result<JobRecord, JobError> {
        if outputs.is_empty() {
            return Err(JobError::Transition(
                JobTransitionError::MissingCommittedOutput,
            ));
        }
        self.transition_inner(job, JobState::Completed, cause, None, outputs, Vec::new())
            .await
    }

    /// Transition and emit extra domain events in the same transaction.
    pub async fn transition_with_events(
        &self,
        job: JobId,
        to: JobState,
        cause: TransitionCause,
        extra_events: Vec<PendingEvent>,
    ) -> Result<JobRecord, JobError> {
        self.transition_inner(job, to, cause, None, Vec::new(), extra_events)
            .await
    }

    /// Complete a job, emitting extra domain events in the same transaction.
    pub async fn complete_with_events(
        &self,
        job: JobId,
        outputs: Vec<JobOutput>,
        cause: TransitionCause,
        extra_events: Vec<PendingEvent>,
    ) -> Result<JobRecord, JobError> {
        if outputs.is_empty() {
            return Err(JobError::Transition(
                JobTransitionError::MissingCommittedOutput,
            ));
        }
        self.transition_inner(job, JobState::Completed, cause, None, outputs, extra_events)
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn transition_inner(
        &self,
        job: JobId,
        to: JobState,
        cause: TransitionCause,
        wait_reason: Option<WaitReason>,
        outputs: Vec<JobOutput>,
        extra_events: Vec<PendingEvent>,
    ) -> Result<JobRecord, JobError> {
        let now = Timestamp::now();
        let (record, events) = self
            .db
            .writer()
            .transaction(move |tx| {
                Box::pin(async move {
                    let current = load_job(tx, job).await?;

                    // Outputs are written first so the Completed guard observes
                    // them inside this transaction.
                    for output in &outputs {
                        sqlx::query(
                            "INSERT OR IGNORE INTO job_outputs
                                 (id, job_id, output_kind, output_ref, output_sha256,
                                  byte_length, committed_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        )
                        .bind(uuid::Uuid::new_v4().to_string())
                        .bind(job.to_string())
                        .bind(&output.kind)
                        .bind(&output.reference)
                        .bind(output.sha256.as_deref())
                        .bind(output.byte_length)
                        .bind(now.as_millis())
                        .execute(&mut **tx)
                        .await
                        .map_err(|e| JobError::storage("insert job output", e))?;
                    }

                    let has_committed_output: i64 =
                        sqlx::query_scalar("SELECT COUNT(*) FROM job_outputs WHERE job_id = ?1")
                            .bind(job.to_string())
                            .fetch_one(&mut **tx)
                            .await
                            .map_err(|e| JobError::storage("count job outputs", e))?;

                    check_transition(
                        current.state,
                        to,
                        &cause,
                        GuardFacts {
                            has_committed_output: has_committed_output > 0,
                        },
                    )?;

                    crate::fault::trip_dynamic(&crate::fault::transition_point(to));

                    let started_at = if current.started_at.is_none()
                        && matches!(
                            to,
                            JobState::Preflight | JobState::RunningLocal | JobState::CloudSending
                        ) {
                        Some(now.as_millis())
                    } else {
                        current.started_at.map(Timestamp::as_millis)
                    };
                    let finished_at = if to.is_terminal() {
                        Some(now.as_millis())
                    } else {
                        current.finished_at.map(Timestamp::as_millis)
                    };
                    let retry_count =
                        if to == JobState::Queued && current.state == JobState::FailedRetryable {
                            current.retry_count + 1
                        } else {
                            current.retry_count
                        };

                    sqlx::query(
                        "UPDATE jobs SET state = ?2, started_at = ?3, finished_at = ?4,
                                         retry_count = ?5
                         WHERE id = ?1 AND state = ?6",
                    )
                    .bind(job.to_string())
                    .bind(to.as_str())
                    .bind(started_at)
                    .bind(finished_at)
                    .bind(retry_count)
                    .bind(current.state.as_str())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| JobError::storage("update job state", e))?;

                    sqlx::query(
                        "UPDATE job_engine_state SET wait_reason = ?2, dispatched = ?3,
                                                     updated_at = ?4
                         WHERE job_id = ?1",
                    )
                    .bind(job.to_string())
                    .bind(wait_reason.map(WaitReason::as_str))
                    .bind(i64::from(
                        current.dispatched || to == JobState::CloudSending,
                    ))
                    .bind(now.as_millis())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| JobError::storage("update job engine state", e))?;

                    sqlx::query(
                        "INSERT INTO job_transitions
                             (job_id, from_state, to_state, actor, reason, detail, occurred_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    )
                    .bind(job.to_string())
                    .bind(current.state.as_str())
                    .bind(to.as_str())
                    .bind(format!("{:?}", cause.actor).to_lowercase())
                    .bind(&cause.reason)
                    .bind(cause.detail.as_deref())
                    .bind(now.as_millis())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| JobError::storage("insert job transition", e))?;

                    let mut buffer = EventBuffer::new();
                    buffer.push(PendingEvent::for_job(
                        job,
                        EventKind::JobUpdated,
                        EventPayload::JobUpdated {
                            state: to,
                            previous_state: current.state,
                            wait_reason,
                            possibly_billed: current.dispatched && to.is_possibly_billed(),
                        },
                    ));
                    for event in extra_events {
                        buffer.push(event);
                    }
                    let events = buffer.flush(tx, now).await?;
                    let record = load_job(tx, job).await?;
                    Ok((record, events))
                })
            })
            .await?;

        crate::fault::trip_dynamic(&crate::fault::transition_point_after(record.state));
        Self::after_commit(&events);
        Ok(record)
    }

    /// Record a failure code alongside a failure transition.
    pub async fn fail(
        &self,
        job: JobId,
        to: JobState,
        code: plaudon_domain::error::AppErrorCode,
        cause: TransitionCause,
    ) -> Result<JobRecord, JobError> {
        debug_assert!(matches!(
            to,
            JobState::FailedRetryable | JobState::FailedTerminal
        ));
        let record = self
            .transition_with_events(
                job,
                to,
                cause,
                vec![PendingEvent::for_job(
                    job,
                    EventKind::JobFailed,
                    EventPayload::JobFailed {
                        state: to,
                        code,
                        retryable: to == JobState::FailedRetryable,
                    },
                )],
            )
            .await?;
        let code_str = code.as_str().to_string();
        self.db
            .writer()
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE jobs SET last_error_code = ?2 WHERE id = ?1")
                        .bind(job.to_string())
                        .bind(&code_str)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| JobError::storage("record job error code", e))?;
                    Ok(())
                })
            })
            .await?;
        Ok(record)
    }

    /// Mark that the user requested cancellation (§15.2 acknowledgement).
    pub async fn mark_cancel_requested(&self, job: JobId) -> Result<(), JobError> {
        let now = Timestamp::now().as_millis();
        self.db
            .writer()
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE job_engine_state SET cancel_requested_at = ?2, updated_at = ?2
                         WHERE job_id = ?1",
                    )
                    .bind(job.to_string())
                    .bind(now)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("record cancel request", e))?;
                    Ok(())
                })
            })
            .await
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// Load one job.
    pub async fn get(&self, job: JobId) -> Result<JobRecord, JobError> {
        let row = sqlx::query(JOB_SELECT_SQL)
            .bind(job.to_string())
            .fetch_optional(self.read_pool())
            .await
            .map_err(|e| JobError::storage("load job", e))?
            .ok_or(JobError::JobNotFound(job))?;
        row_to_job(&row)
    }

    /// Jobs currently in any of `states`, oldest first.
    pub async fn jobs_in_states(&self, states: &[JobState]) -> Result<Vec<JobRecord>, JobError> {
        let mut out = Vec::new();
        for state in states {
            let rows = sqlx::query(JOBS_BY_STATE_SQL)
                .bind(state.as_str())
                .fetch_all(self.read_pool())
                .await
                .map_err(|e| JobError::storage("list jobs by state", e))?;
            for row in &rows {
                out.push(row_to_job(row)?);
            }
        }
        out.sort_by_key(|job| job.created_at.as_millis());
        Ok(out)
    }

    /// Outputs committed for `job`.
    pub async fn outputs(&self, job: JobId) -> Result<Vec<JobOutput>, JobError> {
        let rows = sqlx::query(
            "SELECT output_kind, output_ref, output_sha256, byte_length
             FROM job_outputs WHERE job_id = ?1 ORDER BY committed_at ASC",
        )
        .bind(job.to_string())
        .fetch_all(self.read_pool())
        .await
        .map_err(|e| JobError::storage("load job outputs", e))?;
        rows.iter()
            .map(|row| {
                Ok(JobOutput {
                    kind: row
                        .try_get("output_kind")
                        .map_err(|e| JobError::storage("read output kind", e))?,
                    reference: row
                        .try_get("output_ref")
                        .map_err(|e| JobError::storage("read output ref", e))?,
                    sha256: row
                        .try_get("output_sha256")
                        .map_err(|e| JobError::storage("read output digest", e))?,
                    byte_length: row
                        .try_get("byte_length")
                        .map_err(|e| JobError::storage("read output length", e))?,
                })
            })
            .collect()
    }

    /// Transition audit trail for `job`, oldest first.
    pub async fn transitions(&self, job: JobId) -> Result<Vec<(JobState, JobState)>, JobError> {
        let rows = sqlx::query(
            "SELECT from_state, to_state FROM job_transitions WHERE job_id = ?1 ORDER BY id ASC",
        )
        .bind(job.to_string())
        .fetch_all(self.read_pool())
        .await
        .map_err(|e| JobError::storage("load job transitions", e))?;
        rows.iter()
            .map(|row| {
                let from: String = row
                    .try_get("from_state")
                    .map_err(|e| JobError::storage("read from state", e))?;
                let to: String = row
                    .try_get("to_state")
                    .map_err(|e| JobError::storage("read to state", e))?;
                Ok((
                    JobState::parse(&from)
                        .ok_or_else(|| JobError::CorruptRecord(format!("state `{from}`")))?,
                    JobState::parse(&to)
                        .ok_or_else(|| JobError::CorruptRecord(format!("state `{to}`")))?,
                ))
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Checkpoints (§14.3)
    // -----------------------------------------------------------------------

    /// Write a prepared stage intent. Re-preparing an uncommitted stage bumps
    /// its attempt counter; re-preparing a committed stage is rejected.
    pub async fn prepare_checkpoint(
        &self,
        job: JobId,
        stage: impl Into<String>,
        stage_index: i64,
        input_digest: impl Into<String>,
    ) -> Result<Checkpoint, JobError> {
        let stage = stage.into();
        let input_digest = input_digest.into();
        let now = Timestamp::now();
        self.db
            .writer()
            .transaction(move |tx| {
                Box::pin(async move {
                    if let Some(existing) = load_checkpoint(tx, job, &stage).await? {
                        if existing.is_committed() {
                            return Err(JobError::CheckpointAlreadyCommitted { stage });
                        }
                        sqlx::query(
                            "UPDATE job_checkpoints
                             SET attempt = attempt + 1, input_digest = ?3, prepared_at = ?4
                             WHERE job_id = ?1 AND stage = ?2",
                        )
                        .bind(job.to_string())
                        .bind(&stage)
                        .bind(&input_digest)
                        .bind(now.as_millis())
                        .execute(&mut **tx)
                        .await
                        .map_err(|e| JobError::storage("re-prepare checkpoint", e))?;
                    } else {
                        sqlx::query(
                            "INSERT INTO job_checkpoints
                                 (job_id, stage, stage_index, input_digest, attempt, prepared_at)
                             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                        )
                        .bind(job.to_string())
                        .bind(&stage)
                        .bind(stage_index)
                        .bind(&input_digest)
                        .bind(now.as_millis())
                        .execute(&mut **tx)
                        .await
                        .map_err(|e| JobError::storage("prepare checkpoint", e))?;
                    }
                    crate::fault::trip("checkpoint:prepared");
                    load_checkpoint(tx, job, &stage)
                        .await?
                        .ok_or(JobError::CheckpointNotFound { job, stage })
                })
            })
            .await
    }

    /// Write the commit mark for a stage whose output is durable.
    pub async fn commit_checkpoint(
        &self,
        job: JobId,
        stage: impl Into<String>,
        output_ref: impl Into<String>,
        output_digest: Option<String>,
    ) -> Result<Checkpoint, JobError> {
        let stage = stage.into();
        let output_ref = output_ref.into();
        let now = Timestamp::now();
        self.db
            .writer()
            .transaction(move |tx| {
                Box::pin(async move {
                    let existing = load_checkpoint(tx, job, &stage).await?.ok_or(
                        JobError::CheckpointNotPrepared {
                            stage: stage.clone(),
                        },
                    )?;
                    if existing.is_committed() {
                        return Err(JobError::CheckpointAlreadyCommitted { stage });
                    }
                    sqlx::query(
                        "UPDATE job_checkpoints
                         SET output_ref = ?3, output_digest = ?4, committed_at = ?5
                         WHERE job_id = ?1 AND stage = ?2",
                    )
                    .bind(job.to_string())
                    .bind(&stage)
                    .bind(&output_ref)
                    .bind(output_digest.as_deref())
                    .bind(now.as_millis())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| JobError::storage("commit checkpoint", e))?;
                    crate::fault::trip("checkpoint:committed");
                    load_checkpoint(tx, job, &stage)
                        .await?
                        .ok_or(JobError::CheckpointNotFound { job, stage })
                })
            })
            .await
    }

    /// All checkpoints for `job`, in stage order.
    pub async fn checkpoints(&self, job: JobId) -> Result<Vec<Checkpoint>, JobError> {
        let rows = sqlx::query(
            "SELECT job_id, stage, stage_index, input_digest, output_ref, output_digest,
                    attempt, prepared_at, committed_at
             FROM job_checkpoints WHERE job_id = ?1 ORDER BY stage_index ASC, stage ASC",
        )
        .bind(job.to_string())
        .fetch_all(self.read_pool())
        .await
        .map_err(|e| JobError::storage("load checkpoints", e))?;
        rows.iter().map(row_to_checkpoint).collect()
    }

    /// Plan the continuation of `job` over `stages` (in execution order).
    ///
    /// A stage with a commit mark is skipped and its output reused; everything
    /// else runs again from its last durable point.
    pub async fn resume_plan(
        &self,
        job: JobId,
        stages: &[&str],
    ) -> Result<Vec<StageAction>, JobError> {
        let committed: BTreeMap<String, Checkpoint> = self
            .checkpoints(job)
            .await?
            .into_iter()
            .map(|cp| (cp.stage.clone(), cp))
            .collect();
        Ok(stages
            .iter()
            .map(|stage| match committed.get(*stage) {
                Some(cp) if cp.is_committed() => StageAction::Skip {
                    stage: (*stage).to_string(),
                    output_ref: cp.output_ref.clone().unwrap_or_default(),
                    output_digest: cp.output_digest.clone(),
                },
                Some(cp) => StageAction::Run {
                    stage: (*stage).to_string(),
                    attempt: cp.attempt + 1,
                },
                None => StageAction::Run {
                    stage: (*stage).to_string(),
                    attempt: 0,
                },
            })
            .collect())
    }

    /// Record a durable consent grant row so the ledger's `consent_id`
    /// foreign key resolves.
    ///
    /// The consent *model* (scope semantics, expiry, revocation UX) is owned
    /// by the security and provider epics; the job engine only guarantees that
    /// a dispatch it records points at a row that exists.
    pub async fn ensure_consent_grant(
        &self,
        consent_id: &str,
        scope: &str,
        privacy_policy_version: &str,
        engine: &str,
        model_or_provider: &str,
    ) -> Result<(), JobError> {
        let (consent_id, scope, privacy_policy_version, engine, model_or_provider) = (
            consent_id.to_string(),
            scope.to_string(),
            privacy_policy_version.to_string(),
            engine.to_string(),
            model_or_provider.to_string(),
        );
        let now = Timestamp::now().as_millis();
        self.db
            .writer()
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT OR IGNORE INTO consent_grants
                             (id, scope, privacy_policy_version, engine, model_or_provider,
                              approved_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )
                    .bind(&consent_id)
                    .bind(&scope)
                    .bind(&privacy_policy_version)
                    .bind(&engine)
                    .bind(&model_or_provider)
                    .bind(now)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("record consent grant", e))?;
                    Ok(())
                })
            })
            .await
    }

    /// Mark a consent grant revoked durably.
    pub async fn revoke_consent_grant(&self, consent_id: &str) -> Result<(), JobError> {
        let consent_id = consent_id.to_string();
        let now = Timestamp::now().as_millis();
        self.db
            .writer()
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE consent_grants SET revoked_at = ?2 WHERE id = ?1")
                        .bind(&consent_id)
                        .bind(now)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| JobError::storage("revoke consent grant", e))?;
                    Ok(())
                })
            })
            .await
    }

    // -----------------------------------------------------------------------
    // Cloud request ledger (§10.7)
    // -----------------------------------------------------------------------

    /// Write the ledger row **before** any user content is sent.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_attempt(
        &self,
        job: JobId,
        content_sha256: impl Into<String>,
        request_family: impl Into<String>,
        provider: Option<String>,
        model: impl Into<String>,
        route: Option<String>,
        privacy_policy_snapshot: Option<String>,
        consent_id: Option<String>,
    ) -> Result<String, JobError> {
        let id = uuid::Uuid::new_v4().to_string();
        let attempt_id = id.clone();
        let content_sha256 = content_sha256.into();
        let request_family = request_family.into();
        let model = model.into();
        let now = Timestamp::now().as_millis();
        crate::fault::trip("dispatch:before_record");
        self.db
            .writer()
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO inference_attempts
                             (id, job_id, content_sha256, request_family, provider, model_id,
                              route, privacy_policy_snapshot, consent_id, dispatch_state,
                              created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    )
                    .bind(&attempt_id)
                    .bind(job.to_string())
                    .bind(&content_sha256)
                    .bind(&request_family)
                    .bind(provider.as_deref().unwrap_or("unknown"))
                    .bind(&model)
                    .bind(route.as_deref())
                    .bind(privacy_policy_snapshot.as_deref())
                    .bind(consent_id.as_deref())
                    .bind(DispatchState::NotSent.as_str())
                    .bind(now)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("record inference attempt", e))?;
                    Ok(())
                })
            })
            .await?;
        crate::fault::trip("dispatch:after_record_before_send");
        Ok(id)
    }

    /// Update an attempt's status, optionally recording the vendor request ID.
    pub async fn update_attempt(
        &self,
        attempt_id: &str,
        status: DispatchState,
        provider_request_id: Option<String>,
    ) -> Result<(), JobError> {
        let attempt_id = attempt_id.to_string();
        let now = Timestamp::now().as_millis();
        self.db
            .writer()
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE inference_attempts SET dispatch_state = ?2,
                             provider_request_id = COALESCE(?3, provider_request_id),
                             dispatched_at = CASE WHEN ?2 = 'sending' THEN ?4
                                                  ELSE dispatched_at END,
                             responded_at = CASE WHEN ?2 = 'completed' THEN ?4
                                                 ELSE responded_at END,
                             settled_at = CASE WHEN ?2 IN
                                 ('completed','outcome_unknown','failed_before_send')
                                 THEN ?4 ELSE settled_at END
                         WHERE id = ?1",
                    )
                    .bind(&attempt_id)
                    .bind(status.as_str())
                    .bind(provider_request_id.as_deref())
                    .bind(now)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("update inference attempt", e))?;
                    Ok(())
                })
            })
            .await
    }

    /// Attempts for `job`, oldest first.
    pub async fn attempts(&self, job: JobId) -> Result<Vec<InferenceAttempt>, JobError> {
        let rows = sqlx::query(
            "SELECT id, job_id, content_sha256, request_family, provider, model_id, route,
                    privacy_policy_snapshot, consent_id, dispatch_state, provider_request_id,
                    dispatched_at, responded_at
             FROM inference_attempts WHERE job_id = ?1 ORDER BY created_at ASC",
        )
        .bind(job.to_string())
        .fetch_all(self.read_pool())
        .await
        .map_err(|e| JobError::storage("load inference attempts", e))?;
        rows.iter().map(row_to_attempt).collect()
    }

    /// Attempts that were dispatched but never answered, across all jobs.
    pub async fn dangling_attempts(&self) -> Result<Vec<InferenceAttempt>, JobError> {
        let rows = sqlx::query(
            "SELECT id, job_id, content_sha256, request_family, provider, model_id, route,
                    privacy_policy_snapshot, consent_id, dispatch_state, provider_request_id,
                    dispatched_at, responded_at
             FROM inference_attempts
             WHERE dispatch_state IN ('sending', 'accepted_by_provider', 'outcome_unknown')
               AND responded_at IS NULL
             ORDER BY created_at ASC",
        )
        .fetch_all(self.read_pool())
        .await
        .map_err(|e| JobError::storage("load dangling attempts", e))?;
        rows.iter().map(row_to_attempt).collect()
    }

    // -----------------------------------------------------------------------
    // Tombstones (§14.5)
    // -----------------------------------------------------------------------

    /// `true` when the user deleted this recording; jobs must never resurrect
    /// it (§14.5). Reads the shared `tombstones` table owned by
    /// `crates/persistence`.
    pub async fn is_tombstoned(&self, recording: RecordingId) -> Result<bool, JobError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tombstones
             WHERE entity_type = 'recording' AND entity_id = ?1",
        )
        .bind(recording.to_string())
        .fetch_one(self.read_pool())
        .await
        .map_err(|e| JobError::storage("check tombstone", e))?;
        Ok(count > 0)
    }

    /// Record a durable deletion tombstone. Stores no deleted content.
    pub async fn tombstone(
        &self,
        recording: RecordingId,
        reason: impl Into<String>,
    ) -> Result<(), JobError> {
        let reason = reason.into();
        let now = Timestamp::now().as_millis();
        self.db
            .writer()
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT OR REPLACE INTO tombstones
                             (id, entity_type, entity_id, reason, created_at)
                         VALUES (?1, 'recording', ?2, ?3, ?4)",
                    )
                    .bind(uuid::Uuid::new_v4().to_string())
                    .bind(recording.to_string())
                    .bind(&reason)
                    .bind(now)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("write tombstone", e))?;
                    Ok(())
                })
            })
            .await
    }

    // -----------------------------------------------------------------------

    fn after_commit(events: &[EventRecord]) {
        if events.is_empty() {
            return;
        }
        tracing::debug!(count = events.len(), "committed durable events");
    }
}

const JOBS_BY_STATE_SQL: &str =
    "SELECT j.id, j.job_type, j.state, j.priority, j.retry_count, j.job_key,
            j.config_snapshot_json, j.input_hash, j.created_at, j.started_at, j.finished_at,
            j.last_error_code, e.key_kind, e.resource_class, e.wait_reason, e.is_reprocess,
            e.reprocess_of, e.recording_id, e.consent_id, e.dispatched, e.cancel_requested_at
     FROM jobs j JOIN job_engine_state e ON e.job_id = j.id
     WHERE j.state = ?1 ORDER BY j.created_at ASC";

const JOB_SELECT_SQL: &str =
    "SELECT j.id, j.job_type, j.state, j.priority, j.retry_count, j.job_key,
            j.config_snapshot_json, j.input_hash, j.created_at, j.started_at, j.finished_at,
            j.last_error_code, e.key_kind, e.resource_class, e.wait_reason, e.is_reprocess,
            e.reprocess_of, e.recording_id, e.consent_id, e.dispatched, e.cancel_requested_at
     FROM jobs j JOIN job_engine_state e ON e.job_id = j.id WHERE j.id = ?1";

fn parse_job_id(raw: &str) -> Result<JobId, JobError> {
    uuid::Uuid::parse_str(raw)
        .map(JobId::from_uuid)
        .map_err(|_| JobError::CorruptRecord(format!("invalid job id `{raw}`")))
}

fn opt_ts(value: Option<i64>) -> Option<Timestamp> {
    value.map(Timestamp::from_millis)
}

pub(crate) fn row_to_job(row: &sqlx::sqlite::SqliteRow) -> Result<JobRecord, JobError> {
    let get_str = |name: &str| -> Result<String, JobError> {
        row.try_get(name)
            .map_err(|e| JobError::storage("read job column", e))
    };
    let get_opt_str = |name: &str| -> Result<Option<String>, JobError> {
        row.try_get(name)
            .map_err(|e| JobError::storage("read job column", e))
    };
    let get_i64 = |name: &str| -> Result<i64, JobError> {
        row.try_get(name)
            .map_err(|e| JobError::storage("read job column", e))
    };
    let get_opt_i64 = |name: &str| -> Result<Option<i64>, JobError> {
        row.try_get(name)
            .map_err(|e| JobError::storage("read job column", e))
    };

    let state_raw = get_str("state")?;
    let state = JobState::parse(&state_raw)
        .ok_or_else(|| JobError::CorruptRecord(format!("unknown job state `{state_raw}`")))?;
    let class_raw = get_str("resource_class")?;
    let resource_class = ResourceClass::parse(&class_raw)
        .ok_or_else(|| JobError::CorruptRecord(format!("unknown resource class `{class_raw}`")))?;
    let wait_reason = match get_opt_str("wait_reason")? {
        None => None,
        Some(raw) => Some(
            WaitReason::parse(&raw)
                .ok_or_else(|| JobError::CorruptRecord(format!("unknown wait reason `{raw}`")))?,
        ),
    };
    let config_raw = get_str("config_snapshot_json")?;
    let config_snapshot: serde_json::Value =
        serde_json::from_str(&config_raw).map_err(|e| JobError::encoding("job config", e))?;
    let key_digest_raw = get_opt_str("job_key")?
        .ok_or_else(|| JobError::CorruptRecord("job has no key digest".to_string()))?;
    let key_digest = JobKeyDigest::from_hex(key_digest_raw)
        .map_err(|e| JobError::CorruptRecord(e.to_string()))?;

    Ok(JobRecord {
        id: parse_job_id(&get_str("id")?)?,
        job_type: get_str("job_type")?,
        state,
        priority: get_i64("priority")?,
        retry_count: get_i64("retry_count")?,
        key_digest,
        key_kind: get_str("key_kind")?,
        config_snapshot,
        input_hash: get_opt_str("input_hash")?,
        created_at: Timestamp::from_millis(get_i64("created_at")?),
        started_at: opt_ts(get_opt_i64("started_at")?),
        finished_at: opt_ts(get_opt_i64("finished_at")?),
        last_error_code: get_opt_str("last_error_code")?,
        resource_class,
        wait_reason,
        is_reprocess: get_i64("is_reprocess")? != 0,
        reprocess_of: get_opt_str("reprocess_of")?
            .map(|raw| parse_job_id(&raw))
            .transpose()?,
        recording_id: get_opt_str("recording_id")?
            .map(|raw| {
                uuid::Uuid::parse_str(&raw)
                    .map(RecordingId::from_uuid)
                    .map_err(|_| JobError::CorruptRecord(format!("invalid recording id `{raw}`")))
            })
            .transpose()?,
        consent_id: get_opt_str("consent_id")?,
        dispatched: get_i64("dispatched")? != 0,
        cancel_requested_at: opt_ts(get_opt_i64("cancel_requested_at")?),
    })
}

async fn load_job(
    tx: &mut Transaction<'static, Sqlite>,
    job: JobId,
) -> Result<JobRecord, JobError> {
    let row = sqlx::query(JOB_SELECT_SQL)
        .bind(job.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| JobError::storage("load job in transaction", e))?
        .ok_or(JobError::JobNotFound(job))?;
    row_to_job(&row)
}

fn row_to_checkpoint(row: &sqlx::sqlite::SqliteRow) -> Result<Checkpoint, JobError> {
    let job_raw: String = row
        .try_get("job_id")
        .map_err(|e| JobError::storage("read checkpoint job", e))?;
    Ok(Checkpoint {
        job_id: parse_job_id(&job_raw)?,
        stage: row
            .try_get("stage")
            .map_err(|e| JobError::storage("read checkpoint stage", e))?,
        stage_index: row
            .try_get("stage_index")
            .map_err(|e| JobError::storage("read checkpoint index", e))?,
        input_digest: row
            .try_get("input_digest")
            .map_err(|e| JobError::storage("read checkpoint input", e))?,
        output_ref: row
            .try_get("output_ref")
            .map_err(|e| JobError::storage("read checkpoint output", e))?,
        output_digest: row
            .try_get("output_digest")
            .map_err(|e| JobError::storage("read checkpoint digest", e))?,
        attempt: row
            .try_get("attempt")
            .map_err(|e| JobError::storage("read checkpoint attempt", e))?,
        prepared_at: Timestamp::from_millis(
            row.try_get("prepared_at")
                .map_err(|e| JobError::storage("read checkpoint prepared", e))?,
        ),
        committed_at: opt_ts(
            row.try_get("committed_at")
                .map_err(|e| JobError::storage("read checkpoint committed", e))?,
        ),
    })
}

async fn load_checkpoint(
    tx: &mut Transaction<'static, Sqlite>,
    job: JobId,
    stage: &str,
) -> Result<Option<Checkpoint>, JobError> {
    let row = sqlx::query(
        "SELECT job_id, stage, stage_index, input_digest, output_ref, output_digest,
                attempt, prepared_at, committed_at
         FROM job_checkpoints WHERE job_id = ?1 AND stage = ?2",
    )
    .bind(job.to_string())
    .bind(stage)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| JobError::storage("load checkpoint", e))?;
    row.as_ref().map(row_to_checkpoint).transpose()
}

fn row_to_attempt(row: &sqlx::sqlite::SqliteRow) -> Result<InferenceAttempt, JobError> {
    let job_raw: String = row
        .try_get("job_id")
        .map_err(|e| JobError::storage("read attempt job", e))?;
    let status_raw: String = row
        .try_get("dispatch_state")
        .map_err(|e| JobError::storage("read attempt status", e))?;
    Ok(InferenceAttempt {
        id: row
            .try_get("id")
            .map_err(|e| JobError::storage("read attempt id", e))?,
        job_id: parse_job_id(&job_raw)?,
        content_sha256: row
            .try_get::<Option<String>, _>("content_sha256")
            .map(Option::unwrap_or_default)
            .map_err(|e| JobError::storage("read attempt content hash", e))?,
        request_family: row
            .try_get("request_family")
            .map_err(|e| JobError::storage("read attempt family", e))?,
        provider: row
            .try_get("provider")
            .map_err(|e| JobError::storage("read attempt provider", e))?,
        model: row
            .try_get("model_id")
            .map_err(|e| JobError::storage("read attempt model", e))?,
        route: row
            .try_get("route")
            .map_err(|e| JobError::storage("read attempt route", e))?,
        privacy_policy_snapshot: row
            .try_get("privacy_policy_snapshot")
            .map_err(|e| JobError::storage("read attempt privacy snapshot", e))?,
        consent_id: row
            .try_get("consent_id")
            .map_err(|e| JobError::storage("read attempt consent", e))?,
        status: DispatchState::parse(&status_raw).ok_or_else(|| {
            JobError::CorruptRecord(format!("unknown attempt status `{status_raw}`"))
        })?,
        provider_request_id: row
            .try_get("provider_request_id")
            .map_err(|e| JobError::storage("read attempt vendor id", e))?,
        dispatched_at: opt_ts(
            row.try_get("dispatched_at")
                .map_err(|e| JobError::storage("read attempt dispatched", e))?,
        ),
        responded_at: opt_ts(
            row.try_get("responded_at")
                .map_err(|e| JobError::storage("read attempt responded", e))?,
        ),
    })
}

/// Type alias kept for callers that build their own transactional work.
pub type TxFut<'a, T> = BoxFut<'a, Result<T, JobError>>;
