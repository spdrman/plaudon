//! Persistent job and activity history (SPEC v1.3 §5.5, §15.2 `list_jobs`).
//!
//! The database is authoritative. Events are hints; the UI refetches through
//! these queries whenever it restarts or notices a sequence gap, and the
//! activity history stays visible even when OS notifications are denied
//! (§22.13).

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::error::{retry_class, JobError, RetryClass};
use crate::events::{EventKind, EventRecord};
use crate::scheduler::WaitReason;
use crate::state::{JobPhase, JobState};
use crate::store::row_to_job;

/// Filter for [`JobHistory::list_jobs`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHistoryQuery {
    /// Restrict to these states; empty means all.
    #[serde(default)]
    pub states: Vec<JobState>,
    /// Restrict to one recording.
    #[serde(default)]
    pub recording_id: Option<RecordingId>,
    /// Restrict to one job type.
    #[serde(default)]
    pub job_type: Option<String>,
    /// Only jobs created at or after this instant.
    #[serde(default)]
    pub since: Option<Timestamp>,
    /// Page size; clamped to 500.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Rows to skip.
    #[serde(default)]
    pub offset: Option<i64>,
}

impl JobHistoryQuery {
    /// Everything, newest first.
    pub fn all() -> Self {
        Self::default()
    }

    /// Restrict to the given states.
    #[must_use]
    pub fn with_states(mut self, states: Vec<JobState>) -> Self {
        self.states = states;
        self
    }

    /// Restrict to one recording.
    #[must_use]
    pub fn for_recording(mut self, recording: RecordingId) -> Self {
        self.recording_id = Some(recording);
        self
    }

    /// Page size.
    #[must_use]
    pub fn with_limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit);
        self
    }

    fn effective_limit(&self) -> i64 {
        self.limit.unwrap_or(100).clamp(1, 500)
    }
}

/// A row in the Jobs view (§5.2, §15.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSummary {
    pub job_id: JobId,
    pub job_type: String,
    pub state: JobState,
    pub phase: JobPhase,
    pub retry_class: RetryClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_reason: Option<WaitReason>,
    pub retry_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<RecordingId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    pub created_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// `true` when the job may already have incurred a provider charge.
    pub possibly_billed: bool,
    /// `true` when this run is an explicit reprocessing, not a duplicate.
    pub is_reprocess: bool,
    /// Number of committed outputs.
    pub output_count: i64,
}

/// One page of job summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHistoryPage {
    pub jobs: Vec<JobSummary>,
    /// Total matching rows, ignoring paging.
    pub total: i64,
    /// Highest durable event sequence at the time of the query. The UI stores
    /// this and refetches when it sees a gap.
    pub watermark_seq: i64,
}

/// An entry in the in-app activity history (§5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub seq: i64,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<RecordingId>,
    pub occurred_at: Timestamp,
    /// `true` when this entry corresponds to a completion notice (§5.5).
    pub completion_notice: bool,
}

impl From<&EventRecord> for ActivityEntry {
    fn from(event: &EventRecord) -> Self {
        Self {
            seq: event.seq,
            kind: event.kind,
            job_id: event.job_id,
            recording_id: event.recording_id,
            occurred_at: event.occurred_at,
            completion_notice: event.kind.is_completion_notice(),
        }
    }
}

/// Shared predicate for the job list and its count. Every filter is optional
/// and expressed with a bind parameter, so the SQL text is constant.
const LIST_JOBS_SQL: &str = "SELECT j.id, j.job_type, j.state, j.priority, j.retry_count,
            j.job_key, j.config_snapshot_json, j.input_hash, j.created_at, j.started_at,
            j.finished_at, j.last_error_code, e.key_kind, e.resource_class, e.wait_reason,
            e.is_reprocess, e.reprocess_of, e.recording_id, e.consent_id, e.dispatched,
            e.cancel_requested_at
     FROM jobs j JOIN job_engine_state e ON e.job_id = j.id
     WHERE (?1 = 0 OR j.state IN (SELECT value FROM json_each(?2)))
       AND (?3 IS NULL OR e.recording_id = ?3)
       AND (?4 IS NULL OR j.job_type = ?4)
       AND (?5 IS NULL OR j.created_at >= ?5)
     ORDER BY j.created_at DESC, j.id DESC
     LIMIT ?6 OFFSET ?7";

const COUNT_JOBS_SQL: &str = "SELECT COUNT(*)
     FROM jobs j JOIN job_engine_state e ON e.job_id = j.id
     WHERE (?1 = 0 OR j.state IN (SELECT value FROM json_each(?2)))
       AND (?3 IS NULL OR e.recording_id = ?3)
       AND (?4 IS NULL OR j.job_type = ?4)
       AND (?5 IS NULL OR j.created_at >= ?5)";

/// Read-only history queries over committed state.
#[derive(Clone)]
pub struct JobHistory {
    pool: SqlitePool,
}

impl std::fmt::Debug for JobHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JobHistory")
    }
}

impl JobHistory {
    /// Query history through `pool`.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Jobs matching `query`, newest first.
    pub async fn list_jobs(&self, query: &JobHistoryQuery) -> Result<JobHistoryPage, JobError> {
        // Static SQL with nullable binds: no dynamic string is ever handed to
        // the driver. `?1` toggles the state filter, `?2` carries the state
        // list as a JSON array consumed by `json_each`.
        let states_json = serde_json::to_string(
            &query
                .states
                .iter()
                .map(|state| state.as_str())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| JobError::encoding("job state filter", e))?;
        let filter_states = i64::from(!query.states.is_empty());
        let recording = query.recording_id.map(|id| id.to_string());
        let job_type = query.job_type.clone();
        let since = query.since.map(plaudon_domain::time::Timestamp::as_millis);

        let rows = sqlx::query(LIST_JOBS_SQL)
            .bind(filter_states)
            .bind(&states_json)
            .bind(recording.as_deref())
            .bind(job_type.as_deref())
            .bind(since)
            .bind(query.effective_limit())
            .bind(query.offset.unwrap_or(0).max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| JobError::storage("list jobs", e))?;
        let total: i64 = sqlx::query_scalar(COUNT_JOBS_SQL)
            .bind(filter_states)
            .bind(&states_json)
            .bind(recording.as_deref())
            .bind(job_type.as_deref())
            .bind(since)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| JobError::storage("count jobs", e))?;
        let watermark_seq: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM job_events")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| JobError::storage("read event watermark", e))?;

        let mut jobs = Vec::with_capacity(rows.len());
        for row in &rows {
            let record = row_to_job(row)?;
            let output_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM job_outputs WHERE job_id = ?1")
                    .bind(record.id.to_string())
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| JobError::storage("count job outputs", e))?;
            jobs.push(JobSummary {
                job_id: record.id,
                job_type: record.job_type.clone(),
                state: record.state,
                phase: record.state.phase(),
                retry_class: retry_class(record.state),
                wait_reason: record.wait_reason,
                retry_count: record.retry_count,
                recording_id: record.recording_id,
                last_error_code: record.last_error_code.clone(),
                created_at: record.created_at,
                started_at: record.started_at,
                finished_at: record.finished_at,
                possibly_billed: record.possibly_billed(),
                is_reprocess: record.is_reprocess,
                output_count,
            });
        }

        Ok(JobHistoryPage {
            jobs,
            total,
            watermark_seq: watermark_seq.unwrap_or(0),
        })
    }

    /// In-app activity history, newest first.
    ///
    /// Backed by the durable event log, so entries survive restarts and are
    /// present regardless of OS notification permission.
    pub async fn activity(&self, limit: i64) -> Result<Vec<ActivityEntry>, JobError> {
        let rows = sqlx::query(
            "SELECT seq, kind, job_id, recording_id, occurred_at
             FROM job_events ORDER BY seq DESC LIMIT ?1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobError::storage("read activity history", e))?;

        rows.iter()
            .map(|row| {
                let kind_raw: String = row
                    .try_get("kind")
                    .map_err(|e| JobError::storage("read activity kind", e))?;
                let kind = EventKind::parse(&kind_raw).ok_or_else(|| {
                    JobError::CorruptRecord(format!("unknown event kind `{kind_raw}`"))
                })?;
                let job_id: Option<String> = row
                    .try_get("job_id")
                    .map_err(|e| JobError::storage("read activity job", e))?;
                let recording_id: Option<String> = row
                    .try_get("recording_id")
                    .map_err(|e| JobError::storage("read activity recording", e))?;
                Ok(ActivityEntry {
                    seq: row
                        .try_get("seq")
                        .map_err(|e| JobError::storage("read activity seq", e))?,
                    kind,
                    job_id: job_id
                        .map(|raw| {
                            uuid::Uuid::parse_str(&raw)
                                .map(JobId::from_uuid)
                                .map_err(|_| {
                                    JobError::CorruptRecord(format!("invalid job id `{raw}`"))
                                })
                        })
                        .transpose()?,
                    recording_id: recording_id
                        .map(|raw| {
                            uuid::Uuid::parse_str(&raw)
                                .map(RecordingId::from_uuid)
                                .map_err(|_| {
                                    JobError::CorruptRecord(format!("invalid recording id `{raw}`"))
                                })
                        })
                        .transpose()?,
                    occurred_at: Timestamp::from_millis(
                        row.try_get("occurred_at")
                            .map_err(|e| JobError::storage("read activity time", e))?,
                    ),
                    completion_notice: kind.is_completion_notice(),
                })
            })
            .collect()
    }
}
