//! The job table: creation, transactional transitions and the jobs list.
//!
//! Spec §14.7: "queue intent, provider attempts and final outputs persist
//! transactionally; UI events only mirror committed state". Every transition
//! here is a single committed statement, and the caller emits its event after
//! the commit.

use std::collections::BTreeMap;

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde_json::json;
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::backend::store::{as_u64, rfc3339, rfc3339_opt, storage_error};
use crate::backend::LocalBackend;
use crate::dto::{JobKind, JobPage, JobState, JobSummary, OutputLink, OutputLinkKind};
use crate::error::{envelope, invalid_argument};
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
    pub config_snapshot: serde_json::Value,
}

/// Open a job in [`JobState::Queued`].
pub async fn create(pool: &SqlitePool, draft: &JobDraft) -> Result<JobId, AppError> {
    let id = JobId::new();
    let now = Timestamp::now().as_millis();
    sqlx::query(
        "INSERT INTO jobs (id, kind, state, stage, progress, created_at, updated_at, \
         recording_id, label, cost_acknowledgement_required, config_snapshot_json) \
         VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?4, ?5, ?6, 0, ?7)",
    )
    .bind(id.to_string())
    .bind(draft.kind.as_str())
    .bind(JobState::Queued.as_str())
    .bind(now)
    .bind(draft.recording_id.map(|id| id.to_string()))
    .bind(&draft.label)
    .bind(draft.config_snapshot.to_string())
    .execute(pool)
    .await
    .map_err(|err| storage_error("cannot create a job", &err))?;
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
    stage: Option<&str>,
    progress: Option<f64>,
) -> Result<(), AppError> {
    let now = Timestamp::now().as_millis();
    sqlx::query(
        "UPDATE jobs SET state = ?2, stage = ?3, progress = ?4, updated_at = ?5, \
         started_at = COALESCE(started_at, CASE WHEN ?2 IN ('preflight','running_local') \
             THEN ?5 ELSE NULL END), \
         finished_at = CASE WHEN ?2 IN ('completed','failed_terminal','cancelled') \
             THEN ?5 ELSE finished_at END \
         WHERE id = ?1",
    )
    .bind(job_id.to_string())
    .bind(state.as_str())
    .bind(stage)
    .bind(progress)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|err| storage_error("cannot transition a job", &err))?;
    Ok(())
}

/// Mark a job failed and persist the envelope the UI will show.
pub async fn fail(
    pool: &SqlitePool,
    job_id: JobId,
    error: &AppError,
) -> Result<ErrorEnvelope, AppError> {
    let payload = envelope(error);
    let state =
        if error.is_retryable() { JobState::FailedRetryable } else { JobState::FailedTerminal };
    let cost_ack = error.code() == AppErrorCode::OutcomeUnknown;
    let serialized = serde_json::to_string(&payload)
        .map_err(|err| crate::error::internal(format!("cannot serialize an envelope: {err}")))?;
    let now = Timestamp::now().as_millis();
    sqlx::query(
        "UPDATE jobs SET state = ?2, error_json = ?3, updated_at = ?4, \
         finished_at = CASE WHEN ?2 = 'failed_terminal' THEN ?4 ELSE finished_at END, \
         cost_acknowledgement_required = ?5 WHERE id = ?1",
    )
    .bind(job_id.to_string())
    .bind(state.as_str())
    .bind(serialized)
    .bind(now)
    .bind(i64::from(cost_ack))
    .execute(pool)
    .await
    .map_err(|err| storage_error("cannot record a job failure", &err))?;
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
    sqlx::query(
        "UPDATE jobs SET state = 'completed', stage = NULL, progress = 1.0, updated_at = ?2, \
         finished_at = ?2, error_json = NULL, output_link_kind = ?3, output_link_id = ?4 \
         WHERE id = ?1",
    )
    .bind(job_id.to_string())
    .bind(now)
    .bind(link.as_ref().map(|link| match link.kind {
        OutputLinkKind::Recording => "recording",
        OutputLinkKind::Export => "export",
    }))
    .bind(link.as_ref().map(|link| link.id.clone()))
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot complete a job", &err))?;
    Ok(())
}

/// Read one job as the UI sees it.
pub async fn load(pool: &SqlitePool, job_id: JobId) -> Result<JobSummary, AppError> {
    let row = sqlx::query(JOB_COLUMNS)
        .bind(job_id.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|err| storage_error("cannot read a job", &err))?
        .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such job"))?;
    row_to_summary(&row)
}

/// Read the durable config snapshot of a job.
pub async fn config_snapshot(
    pool: &SqlitePool,
    job_id: JobId,
) -> Result<serde_json::Value, AppError> {
    let raw: Option<String> = sqlx::query_scalar("SELECT config_snapshot_json FROM jobs WHERE id = ?1")
        .bind(job_id.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|err| storage_error("cannot read a job config", &err))?
        .flatten();
    let raw = raw.ok_or_else(|| {
        AppError::new(AppErrorCode::RecordNotFound, "the job has no stored configuration")
    })?;
    serde_json::from_str(&raw)
        .map_err(|err| crate::error::internal(format!("job config is not valid json: {err}")))
}

const JOB_COLUMNS: &str = "SELECT id, kind, state, stage, progress, created_at, updated_at, \
     started_at, finished_at, recording_id, label, error_json, \
     cost_acknowledgement_required, provider_request_id, output_link_kind, output_link_id \
     FROM jobs WHERE id = ?1";

/// Map one row of the jobs table to its wire shape.
pub fn row_to_summary(row: &sqlx::sqlite::SqliteRow) -> Result<JobSummary, AppError> {
    let job_id: String = row.get("id");
    let kind: String = row.get("kind");
    let state: String = row.get("state");
    let recording_id: Option<String> = row.get("recording_id");
    let error_json: Option<String> = row.get("error_json");
    let link_kind: Option<String> = row.get("output_link_kind");
    let link_id: Option<String> = row.get("output_link_id");

    let output_link = match (link_kind.as_deref(), link_id) {
        (Some("recording"), Some(id)) => Some(OutputLink { kind: OutputLinkKind::Recording, id }),
        (Some("export"), Some(id)) => Some(OutputLink { kind: OutputLinkKind::Export, id }),
        _ => None,
    };

    Ok(JobSummary {
        job_id: job_id
            .parse()
            .map_err(|_| crate::error::internal("stored job id is not a uuid"))?,
        kind: JobKind::parse(&kind)
            .ok_or_else(|| crate::error::internal("stored job kind is unknown"))?,
        state: JobState::parse(&state)
            .ok_or_else(|| crate::error::internal("stored job state is unknown"))?,
        stage: row.get("stage"),
        progress: row.get("progress"),
        created_at: rfc3339("jobs.created_at", row.get("created_at"))?,
        updated_at: rfc3339("jobs.updated_at", row.get("updated_at"))?,
        started_at: rfc3339_opt("jobs.started_at", row.get("started_at"))?,
        finished_at: rfc3339_opt("jobs.finished_at", row.get("finished_at"))?,
        recording_id: recording_id
            .map(|id| id.parse())
            .transpose()
            .map_err(|_| crate::error::internal("stored recording id is not a uuid"))?,
        label: row.get("label"),
        error: error_json
            .map(|raw| serde_json::from_str::<ErrorEnvelope>(&raw))
            .transpose()
            .map_err(|err| crate::error::internal(format!("stored envelope is invalid: {err}")))?,
        cost_acknowledgement_required: row.get::<i64, _>("cost_acknowledgement_required") != 0,
        provider_request_id: row.get("provider_request_id"),
        output_link,
    })
}

/// Emit `job.updated` (or `job.failed`) for a committed transition.
pub async fn publish_update(events: &EventPublisher, summary: &JobSummary) {
    let kind = if matches!(summary.state, JobState::FailedRetryable | JobState::FailedTerminal) {
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
        let mut sql = String::from(
            "SELECT id, kind, state, stage, progress, created_at, updated_at, started_at, \
             finished_at, recording_id, label, error_json, cost_acknowledgement_required, \
             provider_request_id, output_link_kind, output_link_id FROM jobs",
        );
        let mut conditions: Vec<String> = Vec::new();
        if !query.states.is_empty() {
            conditions.push(format!("state IN ({})", quoted_list(
                &query.states.iter().map(|state| state.as_str()).collect::<Vec<_>>(),
            )));
        }
        if !query.kinds.is_empty() {
            conditions.push(format!("kind IN ({})", quoted_list(
                &query.kinds.iter().map(|kind| kind.as_str()).collect::<Vec<_>>(),
            )));
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?1 OFFSET ?2");

        let limit = i64::from(query.limit);
        let offset = i64::try_from(query.cursor.offset())
            .map_err(|_| invalid_argument("cursor offset is out of range"))?;
        let rows = sqlx::query(&sql)
            .bind(limit + 1)
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
            counts_sql.push_str(&format!(
                " WHERE kind IN ({})",
                quoted_list(&query.kinds.iter().map(|kind| kind.as_str()).collect::<Vec<_>>())
            ));
        }
        counts_sql.push_str(" GROUP BY state");
        let count_rows = sqlx::query(&counts_sql)
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

        Ok(JobPage { items, next_cursor, state_counts })
    }

    async fn cancel_job(&self, job_id: JobId) -> Result<JobSummary, AppError> {
        let current = load(self.pool(), job_id).await?;
        if current.state.is_terminal() {
            return Err(invalid_argument(format!(
                "job is already in the terminal state {}",
                current.state.as_str()
            )));
        }
        let now = Timestamp::now().as_millis();
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|err| storage_error("cannot begin a cancellation", &err))?;
        sqlx::query(
            "UPDATE jobs SET state = 'cancelled', stage = NULL, updated_at = ?2, \
             finished_at = ?2 WHERE id = ?1",
        )
        .bind(job_id.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|err| storage_error("cannot cancel a job", &err))?;
        tx.commit().await.map_err(|err| storage_error("cannot commit a cancellation", &err))?;

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
        let needs_ack = current.state == JobState::CloudOutcomeUnknown
            || current.cost_acknowledgement_required;
        if needs_ack && !cost_acknowledged {
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

        transition(self.pool(), job_id, JobState::Queued, None, None).await?;
        sqlx::query("UPDATE jobs SET error_json = NULL, finished_at = NULL WHERE id = ?1")
            .bind(job_id.to_string())
            .execute(self.pool())
            .await
            .map_err(|err| storage_error("cannot clear a job error", &err))?;

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
    values.iter().map(|value| format!("'{value}'")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::backend_fixture;

    #[tokio::test]
    async fn a_completed_job_cannot_be_cancelled_or_retried() {
        let fixture = backend_fixture().await;
        let job_id = create(
            fixture.backend.pool(),
            &JobDraft {
                kind: JobKind::Import,
                label: "x".into(),
                recording_id: None,
                config_snapshot: json!({}),
            },
        )
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

    #[tokio::test]
    async fn an_uncertain_cloud_outcome_refuses_to_retry_without_acknowledgement() {
        let fixture = backend_fixture().await;
        let job_id = create(
            fixture.backend.pool(),
            &JobDraft {
                kind: JobKind::Transcribe,
                label: "transcribe".into(),
                recording_id: None,
                config_snapshot: json!({}),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE jobs SET state = 'cloud_outcome_unknown', \
             cost_acknowledgement_required = 1, provider_request_id = 'req-1' WHERE id = ?1",
        )
        .bind(job_id.to_string())
        .execute(fixture.backend.pool())
        .await
        .unwrap();

        let err = fixture.backend.retry_job(job_id, false).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::CostAcknowledgementRequired);

        let after = load(fixture.backend.pool(), job_id).await.unwrap();
        assert_eq!(after.state, JobState::CloudOutcomeUnknown, "the job was not re-dispatched");
        assert!(fixture.sink.events().is_empty(), "no event was emitted for a refusal");
    }

    #[tokio::test]
    async fn listing_pages_and_counts_every_state() {
        let fixture = backend_fixture().await;
        for index in 0..3 {
            let job_id = create(
                fixture.backend.pool(),
                &JobDraft {
                    kind: JobKind::Import,
                    label: format!("job {index}"),
                    recording_id: None,
                    config_snapshot: json!({}),
                },
            )
            .await
            .unwrap();
            if index == 0 {
                transition(fixture.backend.pool(), job_id, JobState::RunningLocal, Some("x"), None)
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
        assert_eq!(filtered.items[0].progress, None, "an unmeasurable stage reports no ratio");
    }
}
