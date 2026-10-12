//! Typed UI events (spec §15.3) with a durable, strictly monotonic sequence.
//!
//! Two rules make the stream trustworthy:
//!
//! 1. A sequence number is drawn from the `event_log` table **after** the
//!    database transaction that caused the event has committed, so the UI can
//!    never observe an event for state that is not readable yet (spec §14.5:
//!    "the Rust job service will dispatch internal events after DB commit").
//! 2. Emission is best-effort. A sink failure is logged and swallowed; it never
//!    rolls back committed work, because frontend delivery is not durable
//!    storage.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde_json::{Map, Value};
use sqlx::SqlitePool;

use crate::dto::{EventKind, UiEvent};

/// Everything needed to publish one event, before a sequence number exists.
#[derive(Debug, Clone)]
pub struct EventDraft {
    /// Event name.
    pub kind: EventKind,
    /// When the committed change happened.
    pub occurred_at: Timestamp,
    /// Job the event belongs to.
    pub job_id: Option<JobId>,
    /// Recording the event belongs to.
    pub recording_id: Option<RecordingId>,
    /// Opaque hint payload.
    pub payload: Map<String, Value>,
}

impl EventDraft {
    /// A draft with an empty payload.
    #[must_use]
    pub fn new(kind: EventKind, occurred_at: Timestamp) -> Self {
        Self {
            kind,
            occurred_at,
            job_id: None,
            recording_id: None,
            payload: Map::new(),
        }
    }

    /// Attach a job.
    #[must_use]
    pub fn with_job(mut self, job_id: JobId) -> Self {
        self.job_id = Some(job_id);
        self
    }

    /// Attach a recording.
    #[must_use]
    pub fn with_recording(mut self, recording_id: RecordingId) -> Self {
        self.recording_id = Some(recording_id);
        self
    }

    /// Attach one hint field.
    #[must_use]
    pub fn with_hint(mut self, key: &str, value: Value) -> Self {
        self.payload.insert(key.to_owned(), value);
        self
    }
}

/// Where published events go.
#[async_trait]
pub trait EventSink: Send + Sync + std::fmt::Debug {
    /// Deliver one event. Implementations must not fail the caller.
    async fn emit(&self, event: UiEvent);
}

/// Sink used by headless runs and by the integration tests that do not assert
/// on events.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullEventSink;

#[async_trait]
impl EventSink for NullEventSink {
    async fn emit(&self, _event: UiEvent) {}
}

/// Sink that keeps every event for assertions.
#[derive(Debug, Default)]
pub struct RecordingEventSink {
    events: Mutex<Vec<UiEvent>>,
}

impl RecordingEventSink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of everything emitted so far.
    #[must_use]
    pub fn events(&self) -> Vec<UiEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }

    /// Sequence numbers emitted so far.
    #[must_use]
    pub fn sequences(&self) -> Vec<u64> {
        self.events().iter().map(|event| event.seq).collect()
    }

    /// Event names emitted so far.
    #[must_use]
    pub fn kinds(&self) -> Vec<EventKind> {
        self.events().iter().map(|event| event.kind).collect()
    }
}

#[async_trait]
impl EventSink for RecordingEventSink {
    async fn emit(&self, event: UiEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

/// Allocates sequence numbers from `event_log` and hands events to a sink.
#[derive(Debug, Clone)]
pub struct EventPublisher {
    pool: SqlitePool,
    sink: Arc<dyn EventSink>,
}

impl EventPublisher {
    /// Publisher over `pool`, delivering to `sink`.
    #[must_use]
    pub fn new(pool: SqlitePool, sink: Arc<dyn EventSink>) -> Self {
        Self { pool, sink }
    }

    /// The sink, for callers that need to inspect delivery in tests.
    #[must_use]
    pub fn sink(&self) -> &Arc<dyn EventSink> {
        &self.sink
    }

    /// Record and emit one event.
    ///
    /// MUST be called after the transaction that produced the change has
    /// committed. Both the insert and the delivery are best-effort: a failure
    /// is logged, and the caller's committed work stands.
    pub async fn publish(&self, draft: EventDraft) {
        let seq = match self.allocate(&draft).await {
            Ok(seq) => seq,
            Err(err) => {
                tracing::warn!(
                    kind = draft.kind.as_str(),
                    error = %err,
                    "could not record a ui event; committed state is unaffected"
                );
                return;
            }
        };
        let Some(occurred_at) = draft.occurred_at.to_rfc3339() else {
            tracing::warn!(
                kind = draft.kind.as_str(),
                "event timestamp is out of range"
            );
            return;
        };
        let event = UiEvent {
            seq,
            kind: draft.kind,
            occurred_at,
            job_id: draft.job_id,
            recording_id: draft.recording_id,
            payload: draft.payload,
        };
        self.sink.emit(event).await;
    }

    async fn allocate(&self, draft: &EventDraft) -> Result<u64, sqlx::Error> {
        let payload = Value::Object(draft.payload.clone()).to_string();
        let result = sqlx::query(
            "INSERT INTO event_log (kind, occurred_at, job_id, recording_id, payload_json) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(draft.kind.as_str())
        .bind(draft.occurred_at.as_millis())
        .bind(draft.job_id.map(|id| id.to_string()))
        .bind(draft.recording_id.map(|id| id.to_string()))
        .bind(payload)
        .execute(&self.pool)
        .await?;
        let seq = result.last_insert_rowid();
        u64::try_from(seq).map_err(|_| sqlx::Error::Protocol("negative event sequence".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::store::Store;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sequences_are_strictly_increasing_without_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let sink = Arc::new(RecordingEventSink::new());
        let publisher = EventPublisher::new(store.pool().clone(), sink.clone());

        for _ in 0..5 {
            publisher
                .publish(EventDraft::new(EventKind::LibraryChanged, Timestamp::now()))
                .await;
        }

        assert_eq!(sink.sequences(), vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_payload_round_trips_and_ids_are_rendered_as_strings() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let sink = Arc::new(RecordingEventSink::new());
        let publisher = EventPublisher::new(store.pool().clone(), sink.clone());
        let recording_id = RecordingId::new();

        publisher
            .publish(
                EventDraft::new(EventKind::RecordingImported, Timestamp::EPOCH)
                    .with_recording(recording_id)
                    .with_hint("title", Value::String("Board meeting".into())),
            )
            .await;

        let events = sink.events();
        assert_eq!(events.len(), 1);
        let json = serde_json::to_value(&events[0]).unwrap();
        assert_eq!(json["kind"], "recording.imported");
        assert_eq!(json["recording_id"], recording_id.to_string());
        assert_eq!(json["job_id"], Value::Null);
        assert_eq!(json["occurred_at"], "1970-01-01T00:00:00Z");
        assert_eq!(json["payload"]["title"], "Board meeting");
        assert_eq!(json["seq"], 1);
    }
}
