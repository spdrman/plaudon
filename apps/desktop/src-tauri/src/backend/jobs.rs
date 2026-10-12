//! The job lifecycle over the core `jobs` / `job_steps` tables.
//!
//! EPIC C owns those tables and will own this logic; the [`crate::ports::JobsPort`]
//! trait is the seam, so `plaudon_jobs` can replace this file without touching
//! a command. Columns are used exactly as migration `0002_core.sql` defines
//! them: everything the wire contract needs beyond them (label, recording,
//! stage, output link) lives in `config_snapshot_json`, which is the row that
//! already carries the request.
//!
//! Spec §14.7: queue intent and final outputs persist transactionally, and the
//! UI event for a transition is emitted only after that transaction commits.

use std::collections::BTreeMap;

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorEnvelope};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::backend::store::{as_u64, rfc3339, rfc3339_opt, storage_error};
use crate::backend::LocalBackend;
use crate::dto::{JobKind, JobPage, JobState, JobSummary, OutputLink, OutputLinkKind};
use crate::error::{envelope, internal, invalid_argument};
use crate::events::{EventDraft, EventPublisher};
use crate::ports::JobsPort;
use crate::validate::{PageCursor, ValidatedListJobs};

/// Everything needed to open a job row.
#[derive(Debug, Clone)]
pub struct JobDraft {
    /// Job family.
    pub kind: JobKind,
    /// Display label.
    pub label: String,
    /// Recording the job belongs to, when it already exists.
    pub recording_id: Option<RecordingId>,
    /// Durable snapshot of the request, so a retry re-runs the same work.
    pub config_snapshot: Value,
}

/// Open a job in [`JobState::Queued`].
pub async fn create(pool: &SqlitePool, draft: &JobDraft) -> Result<JobId, AppError> {
    let id = JobId::new();
    let now = Timestamp::now().as_millis();
    let mut config = draft.config_snapshot.clone();
    if let Some(object) = config.as_object_mut() {
        object.insert("label".to_owned(), json!(draft.label));
        if let Some(recording_id) = draft.recording_id {
            object.insert("recording_id".to_owned(), json!(recording_id.to_string()));
        }
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|err| storage_error("cannot begin a job", &err))?;
    sqlx::query(
        "INSERT INTO jobs (id, job_type, state, priority, retry_count, job_key, \
         config_snapshot_json, input_hash, created_at) \
         VALUES (?1, ?2, ?3, 0, 0, NULL, ?4, NULL, ?5)",
    )
    .bind(id.to_string())
    .bind(draft.kind.as_str())
    .bind(JobState::Queued.as_str())
    .bind(config.to_string())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot create a job", &err))?;
    append_step(&mut tx, id, JobState::Queued, None, None, now).await?;
    tx.commit()
        .await
        .map_err(|err| storage_error("cannot commit a job", &err))?;
    Ok(id)
}

/// Move a job to `state`, recording the stage and progress it reports.
///
/// `progress` stays `None` whenever the stage cannot measure a ratio: the UI
/// renders an indeterminate indicator rather than an invented percentage.
pub async fn transition(
    pool: &SqlitePool,
    job_id: JobId,
    state: JobState,
    stage_label: Option<&str>,
    progress: Option<f64>,
) -> Result<(), AppError> {
    let now = Timestamp::now().as_millis();
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| storage_error("cannot begin a transition", &err))?;
    sqlx::query(
        "UPDATE jobs SET state = ?2, \
         started_at = COALESCE(started_at, CASE WHEN ?2 IN ('preflight','running_local') \
             THEN ?3 ELSE NULL END), \
         finished_at = CASE WHEN ?2 IN ('completed','failed_terminal','cancelled') \
             THEN ?3 ELSE finished_at END \
         WHERE id = ?1",
    )
    .bind(job_id.to_string())
    .bind(state.as_str())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot transition a job", &err))?;
    append_step(&mut tx, job_id, state, stage_label, progress, now).await?;
    tx.commit()
        .await
        .map_err(|err| storage_error("cannot commit a transition", &err))
}

/// Mark a job failed and persist the stable code the UI will render.
///
/// Only the code is stored: the envelope the UI receives is rebuilt from the
/// code's canonical message, so no internal detail can be persisted here.
pub async fn fail(
    pool: &SqlitePool,
    job_id: JobId,
    error: &AppError,
) -> Result<ErrorEnvelope, AppError> {
    let payload = envelope(error);
    let state = if error.is_retryable() {
        JobState::FailedRetryable
    } else {
        JobState::FailedTerminal
    };
    let now = Timestamp::now().as_millis();
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| storage_error("cannot begin a failure", &err))?;
    sqlx::query(
        "UPDATE jobs SET state = ?2, last_error_code = ?3, retry_count = retry_count, \
         finished_at = CASE WHEN ?2 = 'failed_terminal' THEN ?4 ELSE finished_at END \
         WHERE id = ?1",
    )
    .bind(job_id.to_string())
    .bind(state.as_str())
    .bind(error.code().as_str())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot record a job failure", &err))?;
    append_step(&mut tx, job_id, state, Some("failed"), None, now).await?;
    tx.commit()
        .await
        .map_err(|err| storage_error("cannot commit a job failure", &err))?;
    Ok(payload)
}

/// Complete a job inside the caller's transaction.
///
/// The job only reaches `completed` in the same transaction that commits its
/// output (spec §14.1: never show completed before the output is durable).
pub async fn complete_in_tx(
    tx: &mut SqliteConnection,
    job_id: JobId,
    link: Option<OutputLink>,
) -> Result<(), AppError> {
    let now = Timestamp::now().as_millis();
    let raw: String = sqlx::query_scalar("SELECT config_snapshot_json FROM jobs WHERE id = ?1")
        .bind(job_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|err| storage_error("cannot read a job config", &err))?
        .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such job"))?;
    let mut config: Value = serde_json::from_str(&raw)
        .map_err(|err| internal(format!("job config is not valid json: {err}")))?;
    if let (Some(object), Some(link)) = (config.as_object_mut(), link) {
        object.insert(
            "output_link".to_owned(),
            json!({
                "kind": match link.kind {
                    OutputLinkKind::Recording => "recording",
                    OutputLinkKind::Export => "export",
                },
                "id": link.id,
            }),
        );
    }

    sqlx::query(
        "UPDATE jobs SET state = 'completed', finished_at = ?2, last_error_code = NULL, \
         config_snapshot_json = ?3 WHERE id = ?1",
    )
    .bind(job_id.to_string())
    .bind(now)
    .bind(config.to_string())
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot complete a job", &err))?;
    append_step(tx, job_id, JobState::Completed, None, Some(1.0), now).await
}

async fn append_step(
    tx: &mut SqliteConnection,
    job_id: JobId,
    state: JobState,
    stage_label: Option<&str>,
    progress: Option<f64>,
    at: i64,
) -> Result<(), AppError> {
    let next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence) + 1, 0) FROM job_steps WHERE job_id = ?1",
    )
    .bind(job_id.to_string())
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot read the job step sequence", &err))?;
    let mut progress_json = serde_json::Map::new();
    if let Some(stage) = stage_label {
        progress_json.insert("stage".to_owned(), json!(stage));
    }
    if let Some(progress) = progress {
        progress_json.insert("ratio".to_owned(), json!(progress));
    }
    sqlx::query(
        "INSERT INTO job_steps (job_id, sequence, state, attempt, started_at, ended_at, \
         progress_json) VALUES (?1, ?2, ?3, 0, ?4, ?4, ?5)",
    )
    .bind(job_id.to_string())
    .bind(next)
    .bind(state.as_str())
    .bind(at)
    .bind(Value::Object(progress_json).to_string())
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot append a job step", &err))?;
    Ok(())
}

const JOB_SELECT: &str = "SELECT j.id AS id, j.job_type AS job_type, j.state AS state, \
     j.config_snapshot_json AS config_snapshot_json, j.created_at AS created_at, \
     j.started_at AS started_at, j.finished_at AS finished_at, \
     j.last_error_code AS last_error_code, \
     (SELECT js.progress_json FROM job_steps js WHERE js.job_id = j.id \
        ORDER BY js.sequence DESC LIMIT 1) AS last_progress_json, \
     (SELECT MAX(COALESCE(js.ended_at, js.started_at)) FROM job_steps js \
        WHERE js.job_id = j.id) AS last_step_at, \
     (SELECT ia.provider_request_id FROM inference_attempts ia WHERE ia.job_id = j.id \
        AND ia.provider_request_id IS NOT NULL ORDER BY ia.created_at DESC LIMIT 1) \
        AS provider_request_id \
     FROM jobs j";

/// Read one job as the UI sees it.
pub async fn load(pool: &SqlitePool, job_id: JobId) -> Result<JobSummary, AppError> {
    let sql = format!("{JOB_SELECT} WHERE j.id = ?1");
    // Audited: the statement is a constant plus `WHERE j.id = ?1`.
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(job_id.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|err| storage_error("cannot read a job", &err))?
        .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such job"))?;
    row_to_summary(&row)
}

/// Read the durable config snapshot of a job.
pub async fn config_snapshot(pool: &SqlitePool, job_id: JobId) -> Result<Value, AppError> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT config_snapshot_json FROM jobs WHERE id = ?1")
            .bind(job_id.to_string())
            .fetch_optional(pool)
            .await
            .map_err(|err| storage_error("cannot read a job config", &err))?;
    let raw = raw.ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such job"))?;
    serde_json::from_str(&raw)
        .map_err(|err| internal(format!("job config is not valid json: {err}")))
}

/// Map one row of the jobs table to its wire shape.
pub fn row_to_summary(row: &sqlx::sqlite::SqliteRow) -> Result<JobSummary, AppError> {
    let job_id: String = row.get("id");
    let job_type: String = row.get("job_type");
    let state: String = row.get("state");
    let config_raw: String = row.get("config_snapshot_json");
    let config: Value = serde_json::from_str(&config_raw)
        .map_err(|err| internal(format!("job config is not valid json: {err}")))?;
    let progress_raw: Option<String> = row.get("last_progress_json");
    let progress: Value = progress_raw
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|err| internal(format!("job progress is not valid json: {err}")))?
        .unwrap_or_else(|| json!({}));

    let state = JobState::parse(&state).ok_or_else(|| internal("stored job state is unknown"))?;
    let created_at: i64 = row.get("created_at");
    let last_step_at: Option<i64> = row.get("last_step_at");
    let recording_id = config
        .get("recording_id")
        .and_then(Value::as_str)
        .map(str::parse::<RecordingId>)
        .transpose()
        .map_err(|_| internal("stored recording id is not a uuid"))?;

    let output_link = config.get("output_link").and_then(|link| {
        let id = link.get("id").and_then(Value::as_str)?.to_owned();
        let kind = match link.get("kind").and_then(Value::as_str)? {
            "recording" => OutputLinkKind::Recording,
            "export" => OutputLinkKind::Export,
            _ => return None,
        };
        Some(OutputLink { kind, id })
    });

    Ok(JobSummary {
        job_id: job_id
            .parse()
            .map_err(|_| internal("stored job id is not a uuid"))?,
        kind: JobKind::parse(&job_type).ok_or_else(|| internal("stored job kind is unknown"))?,
        state,
        stage: progress
            .get("stage")
            .and_then(Value::as_str)
            .map(str::to_owned),
        progress: progress.get("ratio").and_then(Value::as_f64),
        created_at: rfc3339("jobs.created_at", created_at)?,
        updated_at: rfc3339("jobs.updated_at", last_step_at.unwrap_or(created_at))?,
        started_at: rfc3339_opt("jobs.started_at", row.get("started_at"))?,
        finished_at: rfc3339_opt("jobs.finished_at", row.get("finished_at"))?,
        recording_id,
        label: config
            .get("label")
            .and_then(Value::as_str)
            .map_or_else(|| job_type.clone(), str::to_owned),
        error: row
            .get::<Option<String>, _>("last_error_code")
            .as_deref()
            .and_then(envelope_for),
        // Spec §14.7: a dispatch whose outcome is unknown may already have been
        // billed, so its retry always needs an acknowledgement.
        cost_acknowledgement_required: state == JobState::CloudOutcomeUnknown
            || config
                .get("cost_acknowledgement_required")
                .and_then(Value::as_bool)
                == Some(true),
        provider_request_id: row.get("provider_request_id"),
        output_link,
    })
}

fn envelope_for(code: &str) -> Option<ErrorEnvelope> {
    let code = AppErrorCode::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.as_str() == code)?;
    Some(
        AppError::new(code, "restored from a persisted job failure")
            .to_envelope(&mut DiagnosticLog::new(), Timestamp::now()),
    )
}

/// Emit `job.updated` (or `job.failed`) for a committed transition.
pub async fn publish_update(events: &EventPublisher, summary: &JobSummary) {
    let kind = if matches!(
        summary.state,
        JobState::FailedRetryable | JobState::FailedTerminal
    ) {
        crate::dto::EventKind::JobFailed
    } else {
        crate::dto::EventKind::JobUpdated
    };
    let mut draft = EventDraft::new(kind, Timestamp::now())
        .with_job(summary.job_id)
        .with_hint("state", json!(summary.state.as_str()))
        .with_hint("kind", json!(summary.kind.as_str()));
    if let Some(recording_id) = summary.recording_id {
        draft = draft.with_recording(recording_id);
    }
    events.publish(draft).await;
}

#[async_trait]
impl JobsPort for LocalBackend {
    async fn list_jobs(&self, query: ValidatedListJobs) -> Result<JobPage, AppError> {
        let mut sql = String::from(JOB_SELECT);
        let mut conditions: Vec<String> = Vec::new();
        if !query.states.is_empty() {
            conditions.push(format!(
                "j.state IN ({})",
                quoted_list(
                    &query
                        .states
                        .iter()
                        .map(|state| state.as_str())
                        .collect::<Vec<_>>()
                )
            ));
        }
        if !query.kinds.is_empty() {
            conditions.push(format!(
                "j.job_type IN ({})",
                quoted_list(
                    &query
                        .kinds
                        .iter()
                        .map(|kind| kind.as_str())
                        .collect::<Vec<_>>()
                )
            ));
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY j.created_at DESC, j.id DESC LIMIT ?1 OFFSET ?2");

        let offset = i64::try_from(query.cursor.offset())
            .map_err(|_| invalid_argument("cursor offset is out of range"))?;
        // Audited: every interpolated fragment comes from `as_str` constants.
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(i64::from(query.limit) + 1)
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(|err| storage_error("cannot list jobs", &err))?;

        let has_more = rows.len() > query.limit as usize;
        let items = rows
            .iter()
            .take(query.limit as usize)
            .map(row_to_summary)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = has_more
            .then(|| PageCursor::at(query.cursor.offset() + u64::from(query.limit)).encode());

        // Chips count every state matching the kind filter, so selecting one
        // state does not zero the other chips.
        let mut counts_sql = String::from("SELECT state, COUNT(*) AS total FROM jobs");
        if !query.kinds.is_empty() {
            counts_sql.push_str(" WHERE job_type IN (");
            counts_sql.push_str(&quoted_list(
                &query
                    .kinds
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>(),
            ));
            counts_sql.push(')');
        }
        counts_sql.push_str(" GROUP BY state");
        // Audited: the only interpolation is the constant kind list.
        let count_rows = sqlx::query(sqlx::AssertSqlSafe(counts_sql))
            .fetch_all(self.pool())
            .await
            .map_err(|err| storage_error("cannot count jobs", &err))?;
        let mut state_counts: BTreeMap<JobState, u64> = BTreeMap::new();
        for row in count_rows {
            let state: String = row.get("state");
            let total: i64 = row.get("total");
            if let Some(state) = JobState::parse(&state) {
                state_counts.insert(state, as_u64("job count", total)?);
            }
        }

        Ok(JobPage {
            items,
            next_cursor,
            state_counts,
        })
    }

    async fn cancel_job(&self, job_id: JobId) -> Result<JobSummary, AppError> {
        let current = load(self.pool(), job_id).await?;
        if current.state.is_terminal() {
            return Err(invalid_argument(format!(
                "job is already in the terminal state {}",
                current.state.as_str()
            )));
        }
        transition(
            self.pool(),
            job_id,
            JobState::Cancelled,
            Some("cancelled"),
            None,
        )
        .await?;
        let summary = load(self.pool(), job_id).await?;
        publish_update(self.events(), &summary).await;
        Ok(summary)
    }

    async fn retry_job(
        &self,
        job_id: JobId,
        cost_acknowledged: bool,
    ) -> Result<JobSummary, AppError> {
        let current = load(self.pool(), job_id).await?;

        // Spec §14.7: a possibly-billed dispatch is never re-sent without an
        // explicit acknowledgement, and the refusal must not re-dispatch.
        if current.cost_acknowledgement_required && !cost_acknowledged {
            return Err(AppError::new(
                AppErrorCode::CostAcknowledgementRequired,
                "retry of a possibly-billed job requires cost_acknowledged",
            ));
        }

        match current.state {
            JobState::FailedRetryable | JobState::Cancelled | JobState::CloudOutcomeUnknown => {}
            other => {
                return Err(invalid_argument(format!(
                    "a job in state {} cannot be retried",
                    other.as_str()
                )))
            }
        }

        sqlx::query(
            "UPDATE jobs SET state = 'queued', last_error_code = NULL, finished_at = NULL, \
             retry_count = retry_count + 1 WHERE id = ?1",
        )
        .bind(job_id.to_string())
        .execute(self.pool())
        .await
        .map_err(|err| storage_error("cannot requeue a job", &err))?;

        match current.kind {
            JobKind::Import => crate::backend::import::rerun(self, job_id).await?,
            JobKind::Export => crate::backend::export::rerun(self, job_id).await?,
            other => {
                return Err(AppError::new(
                    AppErrorCode::CapabilityUnverified,
                    format!("this build has no executor for {} jobs", other.as_str()),
                ))
            }
        }

        let summary = load(self.pool(), job_id).await?;
        publish_update(self.events(), &summary).await;
        Ok(summary)
    }
}

/// Render a fixed vocabulary as a SQL list.
///
/// Every element comes from [`JobState::as_str`] or [`JobKind::as_str`], which
/// are compile-time constants, so no frontend string reaches the statement.
fn quoted_list(values: &[&'static str]) -> String {
    values
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::backend_fixture;

    fn draft(kind: JobKind, label: &str) -> JobDraft {
        JobDraft {
            kind,
            label: label.to_owned(),
            recording_id: None,
            config_snapshot: json!({}),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_completed_job_cannot_be_cancelled_or_retried() {
        let fixture = backend_fixture().await;
        let job_id = create(fixture.backend.pool(), &draft(JobKind::Import, "x"))
            .await
            .unwrap();
        let mut conn = fixture.backend.pool().acquire().await.unwrap();
        complete_in_tx(&mut conn, job_id, None).await.unwrap();
        drop(conn);

        let err = fixture.backend.cancel_job(job_id).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::InvalidArgument);
        let err = fixture.backend.retry_job(job_id, true).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_uncertain_cloud_outcome_refuses_to_retry_without_acknowledgement() {
        let fixture = backend_fixture().await;
        let job_id = create(
            fixture.backend.pool(),
            &draft(JobKind::Transcribe, "transcribe"),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET state = 'cloud_outcome_unknown' WHERE id = ?1")
            .bind(job_id.to_string())
            .execute(fixture.backend.pool())
            .await
            .unwrap();

        let before = load(fixture.backend.pool(), job_id).await.unwrap();
        assert!(before.cost_acknowledgement_required);

        let err = fixture.backend.retry_job(job_id, false).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::CostAcknowledgementRequired);

        let after = load(fixture.backend.pool(), job_id).await.unwrap();
        assert_eq!(
            after.state,
            JobState::CloudOutcomeUnknown,
            "the job was not re-dispatched"
        );
        assert_eq!(
            fixture.sink.events().len(),
            0,
            "no event was emitted for a refusal"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_job_is_committed_and_announced() {
        let fixture = backend_fixture().await;
        let job_id = create(fixture.backend.pool(), &draft(JobKind::Import, "x"))
            .await
            .unwrap();
        let summary = fixture.backend.cancel_job(job_id).await.unwrap();
        assert_eq!(summary.state, JobState::Cancelled);
        assert!(summary.finished_at.is_some());
        assert_eq!(fixture.sink.events().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listing_pages_and_counts_every_state() {
        let fixture = backend_fixture().await;
        for index in 0..3 {
            let job_id = create(
                fixture.backend.pool(),
                &draft(JobKind::Import, &format!("job {index}")),
            )
            .await
            .unwrap();
            if index == 0 {
                transition(
                    fixture.backend.pool(),
                    job_id,
                    JobState::RunningLocal,
                    Some("hashing"),
                    None,
                )
                .await
                .unwrap();
            }
        }

        let first = fixture
            .backend
            .list_jobs(ValidatedListJobs {
                cursor: PageCursor::START,
                limit: 2,
                states: Vec::new(),
                kinds: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(first.items.len(), 2);
        let cursor = first.next_cursor.clone().expect("a third job remains");

        let second = fixture
            .backend
            .list_jobs(ValidatedListJobs {
                cursor: PageCursor::decode(&cursor).unwrap(),
                limit: 2,
                states: Vec::new(),
                kinds: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert!(second.next_cursor.is_none());
        assert_eq!(first.state_counts.get(&JobState::Queued), Some(&2));
        assert_eq!(first.state_counts.get(&JobState::RunningLocal), Some(&1));

        let filtered = fixture
            .backend
            .list_jobs(ValidatedListJobs {
                cursor: PageCursor::START,
                limit: 10,
                states: vec![JobState::RunningLocal],
                kinds: vec![JobKind::Import],
            })
            .await
            .unwrap();
        assert_eq!(filtered.items.len(), 1);
        assert_eq!(filtered.items[0].state, JobState::RunningLocal);
        assert_eq!(filtered.items[0].stage.as_deref(), Some("hashing"));
        assert_eq!(
            filtered.items[0].progress, None,
            "an unmeasurable stage reports no ratio"
        );
        assert_eq!(filtered.items[0].label, "job 0");
    }
}
