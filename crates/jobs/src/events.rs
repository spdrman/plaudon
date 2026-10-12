//! Durable, monotonic local event log (SPEC v1.3 §14.5, §15.3).
//!
//! Every committed state change allocates a local sequence number *inside the
//! committing transaction*. A rollback therefore leaves neither an event row
//! nor a consumed sequence number: the log is strictly monotonic **and**
//! gap-free. Dispatch to in-process subscribers happens strictly after the
//! commit, reads only committed rows, and can never roll a job back.
//!
//! There is no HTTP listener, loopback REST server or webhook target anywhere
//! in this module: fan-out is in-process only (§14.5, App. A.5/A.6).

use std::sync::Arc;

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::error::JobError;
use crate::scheduler::WaitReason;
use crate::state::JobState;

/// Kinds of durable local event (§14.5, §15.3, App. A.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    #[serde(rename = "job.updated")]
    JobUpdated,
    #[serde(rename = "job.failed")]
    JobFailed,
    #[serde(rename = "recording.imported")]
    RecordingImported,
    #[serde(rename = "recording.source_updated")]
    RecordingSourceUpdated,
    #[serde(rename = "recording.deleted")]
    RecordingDeleted,
    #[serde(rename = "transcript.completed")]
    TranscriptCompleted,
    #[serde(rename = "tool.completed")]
    ToolCompleted,
    #[serde(rename = "model.availability_changed")]
    ModelAvailabilityChanged,
    #[serde(rename = "consent.required")]
    ConsentRequired,
    #[serde(rename = "library.changed")]
    LibraryChanged,
}

impl EventKind {
    /// All kinds.
    pub const ALL: &'static [EventKind] = &[
        EventKind::JobUpdated,
        EventKind::JobFailed,
        EventKind::RecordingImported,
        EventKind::RecordingSourceUpdated,
        EventKind::RecordingDeleted,
        EventKind::TranscriptCompleted,
        EventKind::ToolCompleted,
        EventKind::ModelAvailabilityChanged,
        EventKind::ConsentRequired,
        EventKind::LibraryChanged,
    ];

    /// Stable wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::JobUpdated => "job.updated",
            EventKind::JobFailed => "job.failed",
            EventKind::RecordingImported => "recording.imported",
            EventKind::RecordingSourceUpdated => "recording.source_updated",
            EventKind::RecordingDeleted => "recording.deleted",
            EventKind::TranscriptCompleted => "transcript.completed",
            EventKind::ToolCompleted => "tool.completed",
            EventKind::ModelAvailabilityChanged => "model.availability_changed",
            EventKind::ConsentRequired => "consent.required",
            EventKind::LibraryChanged => "library.changed",
        }
    }

    /// Parse a persisted name.
    pub fn parse(raw: &str) -> Option<EventKind> {
        EventKind::ALL.iter().copied().find(|k| k.as_str() == raw)
    }

    /// `true` for kinds the user sees as a completion notice (§5.5).
    pub const fn is_completion_notice(self) -> bool {
        matches!(
            self,
            EventKind::RecordingImported
                | EventKind::TranscriptCompleted
                | EventKind::ToolCompleted
                | EventKind::JobFailed
        )
    }
}

/// The sanitized payload carried by an event.
///
/// Every variant is built from typed domain values. There is no free-form
/// `String` that could carry a bearer token, a signed URL, a device secret or a
/// platform pointer (§6.3A), and no absolute filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    /// A job changed state.
    JobUpdated {
        state: JobState,
        previous_state: JobState,
        #[serde(skip_serializing_if = "Option::is_none")]
        wait_reason: Option<WaitReason>,
        /// `true` when content may already have been billed (§14.3).
        possibly_billed: bool,
    },
    /// A job failed. Carries the stable code only, never vendor text.
    JobFailed {
        state: JobState,
        code: plaudon_domain::error::AppErrorCode,
        retryable: bool,
    },
    /// A recording became part of the library with verified durable bytes.
    RecordingImported {
        media_sha256: String,
        byte_length: i64,
    },
    /// Source-side metadata for a recording changed.
    RecordingSourceUpdated { change: SourceChange },
    /// A recording was deleted by the user; a tombstone exists.
    RecordingDeleted,
    /// A transcript revision was committed.
    TranscriptCompleted {
        transcript_revision: String,
        segment_count: i64,
    },
    /// A transcript tool produced a committed output.
    ToolCompleted { tool: String, output_id: String },
    /// A model asset appeared or disappeared.
    ModelAvailabilityChanged { engine: String, available: bool },
    /// A dispatch needs explicit consent before it can proceed.
    ConsentRequired { purpose: String },
    /// Library-wide invalidation hint; consumers refetch.
    LibraryChanged,
}

/// How a source item changed relative to our record (§7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceChange {
    New,
    Unchanged,
    MetadataChanged,
    AudioChanged,
    RemoteTrashed,
    RemovedFromListing,
}

/// An event awaiting a sequence number, buffered inside a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEvent {
    pub kind: EventKind,
    pub job_id: Option<JobId>,
    pub recording_id: Option<RecordingId>,
    pub payload: EventPayload,
}

impl PendingEvent {
    /// Event scoped to a job.
    pub fn for_job(job_id: JobId, kind: EventKind, payload: EventPayload) -> Self {
        Self {
            kind,
            job_id: Some(job_id),
            recording_id: None,
            payload,
        }
    }

    /// Event scoped to a recording.
    pub fn for_recording(
        recording_id: RecordingId,
        kind: EventKind,
        payload: EventPayload,
    ) -> Self {
        Self {
            kind,
            job_id: None,
            recording_id: Some(recording_id),
            payload,
        }
    }

    /// Attach a recording to a job-scoped event.
    #[must_use]
    pub fn with_recording(mut self, recording_id: RecordingId) -> Self {
        self.recording_id = Some(recording_id);
        self
    }
}

/// A committed event row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Strictly monotonic, gap-free local sequence number.
    pub seq: i64,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<RecordingId>,
    pub occurred_at: Timestamp,
    pub payload: EventPayload,
}

/// Buffer of events produced by one transaction. Appended to the log inside
/// that same transaction, dispatched only after it commits.
#[derive(Debug, Default)]
pub struct EventBuffer {
    pending: Vec<PendingEvent>,
}

impl EventBuffer {
    /// Empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an event for this transaction.
    pub fn push(&mut self, event: PendingEvent) {
        self.pending.push(event);
    }

    /// Number of buffered events.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// `true` when nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Write every buffered event into the log, allocating sequence numbers
    /// from the transactional counter. Must be called inside the same
    /// transaction as the state change the events describe.
    pub async fn flush(
        &mut self,
        tx: &mut Transaction<'static, Sqlite>,
        at: Timestamp,
    ) -> Result<Vec<EventRecord>, JobError> {
        let mut committed = Vec::with_capacity(self.pending.len());
        for pending in self.pending.drain(..) {
            committed.push(append_event(tx, pending, at).await?);
        }
        Ok(committed)
    }
}

/// Allocate the next sequence number and write one event row.
///
/// The counter lives in `event_sequence` rather than relying on
/// `AUTOINCREMENT`, because a rolled-back `AUTOINCREMENT` insert consumes its
/// row id and would leave a permanent gap.
async fn append_event(
    tx: &mut Transaction<'static, Sqlite>,
    pending: PendingEvent,
    at: Timestamp,
) -> Result<EventRecord, JobError> {
    let seq: i64 =
        sqlx::query("UPDATE event_sequence SET next = next + 1 WHERE id = 1 RETURNING next - 1")
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| JobError::storage("allocate event sequence", e))?
            .try_get(0)
            .map_err(|e| JobError::storage("read event sequence", e))?;

    let payload_json = serde_json::to_string(&pending.payload)
        .map_err(|e| JobError::encoding("event payload", e))?;

    sqlx::query(
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(seq)
    .bind(pending.kind.as_str())
    .bind(pending.job_id.map(|id| id.to_string()))
    .bind(pending.recording_id.map(|id| id.to_string()))
    .bind(&payload_json)
    .bind(at.as_millis())
    .execute(&mut **tx)
    .await
    .map_err(|e| JobError::storage("insert event", e))?;

    Ok(EventRecord {
        seq,
        kind: pending.kind,
        job_id: pending.job_id,
        recording_id: pending.recording_id,
        occurred_at: at,
        payload: pending.payload,
    })
}

fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> Result<EventRecord, JobError> {
    let kind_raw: String = row
        .try_get("kind")
        .map_err(|e| JobError::storage("read event kind", e))?;
    let kind = EventKind::parse(&kind_raw)
        .ok_or_else(|| JobError::CorruptRecord(format!("unknown event kind `{kind_raw}`")))?;
    let payload_json: String = row
        .try_get("payload_json")
        .map_err(|e| JobError::storage("read event payload", e))?;
    let payload: EventPayload =
        serde_json::from_str(&payload_json).map_err(|e| JobError::encoding("event payload", e))?;
    let job_id: Option<String> = row
        .try_get("job_id")
        .map_err(|e| JobError::storage("read event job id", e))?;
    let recording_id: Option<String> = row
        .try_get("recording_id")
        .map_err(|e| JobError::storage("read event recording id", e))?;
    let parse_id = |raw: Option<String>, what: &str| -> Result<Option<uuid::Uuid>, JobError> {
        match raw {
            None => Ok(None),
            Some(value) => uuid::Uuid::parse_str(&value)
                .map(Some)
                .map_err(|_| JobError::CorruptRecord(format!("invalid {what} `{value}`"))),
        }
    };
    Ok(EventRecord {
        seq: row
            .try_get("seq")
            .map_err(|e| JobError::storage("read event seq", e))?,
        kind,
        job_id: parse_id(job_id, "job id")?.map(JobId::from_uuid),
        recording_id: parse_id(recording_id, "recording id")?.map(RecordingId::from_uuid),
        occurred_at: Timestamp::from_millis(
            row.try_get("occurred_at")
                .map_err(|e| JobError::storage("read event timestamp", e))?,
        ),
        payload,
    })
}

/// Read-side API over the committed event log.
#[derive(Clone, Debug)]
pub struct EventLog {
    pool: SqlitePool,
}

impl EventLog {
    /// Open a reader over `pool`.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Committed events with `seq > after`, oldest first.
    pub async fn read_after(&self, after: i64, limit: i64) -> Result<Vec<EventRecord>, JobError> {
        let rows = sqlx::query(
            "SELECT seq, kind, job_id, recording_id, payload_json, occurred_at
             FROM job_events WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
        )
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobError::storage("read events", e))?;
        rows.iter().map(row_to_event).collect()
    }

    /// Highest committed sequence number, or 0 when the log is empty.
    pub async fn latest_seq(&self) -> Result<i64, JobError> {
        let value: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM job_events")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| JobError::storage("read latest event seq", e))?;
        Ok(value.unwrap_or(0))
    }

    /// Total number of committed events.
    pub async fn count(&self) -> Result<i64, JobError> {
        sqlx::query_scalar("SELECT COUNT(*) FROM job_events")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| JobError::storage("count events", e))
    }

    /// Persisted cursor for `consumer`, or 0 when it has never consumed.
    pub async fn cursor(&self, consumer: &str) -> Result<i64, JobError> {
        let value: Option<i64> = sqlx::query_scalar(
            "SELECT last_seq FROM event_consumer_cursors WHERE consumer_id = ?1",
        )
        .bind(consumer)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobError::storage("read consumer cursor", e))?;
        Ok(value.unwrap_or(0))
    }
}

/// Failure reported by a subscriber. Never propagates into the job.
#[derive(Debug, thiserror::Error)]
#[error("event subscriber `{consumer}` failed: {reason}")]
pub struct SubscriberError {
    pub consumer: String,
    pub reason: String,
}

/// An in-process consumer of committed events.
///
/// Implementations must be cheap and non-blocking; a slow or failing
/// subscriber must never delay or roll back a committed job.
pub trait EventSubscriber: Send + Sync + std::fmt::Debug {
    /// Stable identifier used for the durable cursor.
    fn consumer_id(&self) -> &str;

    /// Handle one committed event.
    fn handle(&self, event: &EventRecord) -> Result<(), SubscriberError>;
}

/// Post-commit fan-out to in-process subscribers, with durable per-consumer
/// cursors so a restart replays exactly the events a consumer missed.
#[derive(Clone)]
pub struct EventDispatcher {
    subscribers: Arc<Vec<Arc<dyn EventSubscriber>>>,
    writer: crate::writer::WriteCoordinator,
    log: EventLog,
}

impl std::fmt::Debug for EventDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventDispatcher")
            .field("subscribers", &self.subscribers.len())
            .field("writer", &self.writer)
            .finish_non_exhaustive()
    }
}

impl EventDispatcher {
    /// Build a dispatcher over `subscribers`.
    pub fn new(
        subscribers: Vec<Arc<dyn EventSubscriber>>,
        writer: crate::writer::WriteCoordinator,
        log: EventLog,
    ) -> Self {
        Self {
            subscribers: Arc::new(subscribers),
            writer,
            log,
        }
    }

    /// Registered consumer identifiers.
    pub fn consumer_ids(&self) -> Vec<String> {
        self.subscribers
            .iter()
            .map(|s| s.consumer_id().to_string())
            .collect()
    }

    /// Deliver `events` to every subscriber and advance their durable cursors.
    ///
    /// A subscriber failure is logged and skipped: its cursor does not advance
    /// past the failed event, so the event is retried on the next pump or at
    /// the next startup. Nothing here can fail the committed job.
    pub async fn dispatch(&self, events: &[EventRecord]) {
        crate::fault::trip("event:after_commit_before_dispatch");
        for subscriber in self.subscribers.iter() {
            let consumer = subscriber.consumer_id().to_string();
            let mut delivered_through: Option<i64> = None;
            for event in events {
                match subscriber.handle(event) {
                    Ok(()) => delivered_through = Some(event.seq),
                    Err(error) => {
                        tracing::warn!(
                            consumer = %consumer,
                            seq = event.seq,
                            %error,
                            "event subscriber failed; cursor not advanced"
                        );
                        break;
                    }
                }
            }
            if let Some(seq) = delivered_through {
                if let Err(error) = self.advance_cursor(&consumer, seq).await {
                    tracing::warn!(consumer = %consumer, %error, "could not persist consumer cursor");
                }
            }
        }
    }

    /// Replay everything each consumer has not yet acknowledged.
    ///
    /// Called at startup so an event that committed but was never dispatched
    /// (process killed in the post-commit window) is delivered exactly once per
    /// consumer cursor.
    pub async fn replay_pending(&self, batch: i64) -> Result<usize, JobError> {
        let mut delivered = 0usize;
        for subscriber in self.subscribers.iter() {
            let consumer = subscriber.consumer_id().to_string();
            loop {
                let cursor = self.log.cursor(&consumer).await?;
                let events = self.log.read_after(cursor, batch).await?;
                if events.is_empty() {
                    break;
                }
                let mut advanced: Option<i64> = None;
                for event in &events {
                    match subscriber.handle(event) {
                        Ok(()) => {
                            advanced = Some(event.seq);
                            delivered += 1;
                        }
                        Err(error) => {
                            tracing::warn!(consumer = %consumer, seq = event.seq, %error, "replay stalled");
                            break;
                        }
                    }
                }
                match advanced {
                    Some(seq) => self.advance_cursor(&consumer, seq).await?,
                    None => break,
                }
            }
        }
        Ok(delivered)
    }

    async fn advance_cursor(&self, consumer: &str, seq: i64) -> Result<(), JobError> {
        let consumer = consumer.to_string();
        let now = Timestamp::now().as_millis();
        self.writer
            .submit(move |mut conn| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO event_consumer_cursors (consumer_id, last_seq, updated_at)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(consumer_id) DO UPDATE SET
                             last_seq = MAX(last_seq, excluded.last_seq),
                             updated_at = excluded.updated_at",
                    )
                    .bind(&consumer)
                    .bind(seq)
                    .bind(now)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("advance consumer cursor", e))?;
                    Ok(())
                })
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kinds_round_trip_and_match_spec_names() {
        for kind in EventKind::ALL {
            assert_eq!(EventKind::parse(kind.as_str()), Some(*kind));
            assert_eq!(
                serde_json::to_string(kind).expect("serialize"),
                format!("\"{}\"", kind.as_str())
            );
            assert!(kind.as_str().contains('.'), "{kind:?} must be namespaced");
        }
    }

    #[test]
    fn spec_15_3_event_names_are_all_present() {
        for name in [
            "job.updated",
            "recording.imported",
            "recording.source_updated",
            "transcript.completed",
            "tool.completed",
            "model.availability_changed",
            "consent.required",
            "library.changed",
        ] {
            assert!(EventKind::parse(name).is_some(), "missing {name}");
        }
    }

    #[test]
    fn completion_notices_cover_the_spec_5_5_set() {
        assert!(EventKind::RecordingImported.is_completion_notice());
        assert!(EventKind::TranscriptCompleted.is_completion_notice());
        assert!(EventKind::JobFailed.is_completion_notice());
        assert!(!EventKind::LibraryChanged.is_completion_notice());
    }

    #[test]
    fn payloads_serialize_with_a_discriminating_tag() {
        let payload = EventPayload::JobUpdated {
            state: JobState::RunningLocal,
            previous_state: JobState::Preflight,
            wait_reason: None,
            possibly_billed: false,
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["type"], "job_updated");
        assert_eq!(json["state"], "running_local");
        assert!(json.get("wait_reason").is_none());
    }

    #[test]
    fn buffer_tracks_pending_events() {
        let mut buffer = EventBuffer::new();
        assert!(buffer.is_empty());
        buffer.push(PendingEvent::for_job(
            JobId::new(),
            EventKind::JobUpdated,
            EventPayload::JobUpdated {
                state: JobState::Queued,
                previous_state: JobState::Queued,
                wait_reason: None,
                possibly_billed: false,
            },
        ));
        assert_eq!(buffer.len(), 1);
    }
}
