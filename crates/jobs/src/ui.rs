//! Typed UI event bridge (SPEC v1.3 §15.3, §5.5, §6.3A).
//!
//! The frontend never receives a bearer token, a signed URL, a device secret,
//! a platform pointer or an absolute filesystem path. Everything that crosses
//! the boundary is built from the typed, sanitized [`crate::events::EventPayload`]
//! variants and carries the durable sequence number so a subscriber can detect
//! a gap and refetch authoritative state.
//!
//! Event delivery to the frontend is explicitly *not* durable persistence: the
//! durable record is the event log, and [`crate::history::JobHistory`] is the
//! authoritative read path.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::events::{EventKind, EventRecord, EventSubscriber, SubscriberError};

/// Version of the UI event contract. Bumped when a payload shape changes.
pub const UI_EVENT_CONTRACT_VERSION: u32 = 1;

/// The JSON Schema describing [`UiEvent`], checked into `schemas/events/`.
pub const UI_EVENT_SCHEMA: &str = include_str!("../../../schemas/events/job-events.v1.schema.json");

/// One sanitized event delivered to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiEvent {
    /// Contract version so the frontend can reject an unknown shape.
    pub version: u32,
    /// Durable, strictly monotonic sequence number.
    pub seq: i64,
    /// Event name, e.g. `job.updated`.
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<plaudon_domain::ids::JobId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<plaudon_domain::ids::RecordingId>,
    /// Milliseconds since the Unix epoch.
    pub occurred_at: i64,
    pub payload: crate::events::EventPayload,
}

impl From<&EventRecord> for UiEvent {
    fn from(record: &EventRecord) -> Self {
        Self {
            version: UI_EVENT_CONTRACT_VERSION,
            seq: record.seq,
            kind: record.kind,
            job_id: record.job_id,
            recording_id: record.recording_id,
            occurred_at: record.occurred_at.as_millis(),
            payload: record.payload.clone(),
        }
    }
}

/// Failure emitting to the frontend. Never propagates into a job.
#[derive(Debug, thiserror::Error)]
#[error("could not emit UI event: {0}")]
pub struct SinkError(pub String);

/// Transport that delivers a [`UiEvent`] to the user interface.
///
/// `plaudon_app::event::ChannelEventSink` implements this by pushing onto the
/// bounded event channel the shell drains, then waking the render loop.
/// Implementations must not block and must not return an error that the caller
/// is expected to act on; a failed emit is logged and the committed state
/// stands.
pub trait EventSink: Send + Sync + std::fmt::Debug {
    /// Deliver one event.
    fn emit(&self, event: &UiEvent) -> Result<(), SinkError>;
}

/// Adapts an [`EventSink`] to the durable-log subscriber interface.
#[derive(Debug)]
pub struct UiBridge {
    consumer_id: String,
    sink: Arc<dyn EventSink>,
}

impl UiBridge {
    /// Bridge committed events to `sink` under the given consumer id.
    pub fn new(consumer_id: impl Into<String>, sink: Arc<dyn EventSink>) -> Self {
        Self {
            consumer_id: consumer_id.into(),
            sink,
        }
    }

    /// Default consumer id used by the desktop frontend.
    pub const DESKTOP_CONSUMER_ID: &'static str = "desktop-ui";
}

impl EventSubscriber for UiBridge {
    fn consumer_id(&self) -> &str {
        &self.consumer_id
    }

    fn handle(&self, event: &EventRecord) -> Result<(), SubscriberError> {
        let ui_event = UiEvent::from(event);
        self.sink.emit(&ui_event).map_err(|error| SubscriberError {
            consumer: self.consumer_id.clone(),
            reason: error.to_string(),
        })
    }
}

/// An in-memory sink used by tests and by the headless CLI.
#[derive(Debug, Default, Clone)]
pub struct RecordingSink {
    events: Arc<Mutex<Vec<UiEvent>>>,
    fail_from_seq: Arc<Mutex<Option<i64>>>,
}

impl RecordingSink {
    /// Empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything emitted so far.
    pub fn events(&self) -> Vec<UiEvent> {
        self.events.lock().expect("sink poisoned").clone()
    }

    /// Sequence numbers emitted so far.
    pub fn seqs(&self) -> Vec<i64> {
        self.events().into_iter().map(|e| e.seq).collect()
    }

    /// Make the sink fail for every event with `seq >= from`, simulating a
    /// dropped frontend connection.
    pub fn fail_from(&self, from: i64) {
        *self.fail_from_seq.lock().expect("sink poisoned") = Some(from);
    }

    /// Stop failing.
    pub fn recover(&self) {
        *self.fail_from_seq.lock().expect("sink poisoned") = None;
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: &UiEvent) -> Result<(), SinkError> {
        if let Some(from) = *self.fail_from_seq.lock().expect("sink poisoned") {
            if event.seq >= from {
                return Err(SinkError(format!(
                    "frontend unavailable at seq {}",
                    event.seq
                )));
            }
        }
        self.events
            .lock()
            .expect("sink poisoned")
            .push(event.clone());
        Ok(())
    }
}

/// Patterns that must never appear in a serialized UI payload (§18.5).
///
/// Used by the security test and available to consumers that want the same
/// assertion in their own suites.
pub const FORBIDDEN_PAYLOAD_PATTERNS: &[&str] = &[
    "sk-",
    "Bearer ",
    "bearer ",
    "X-Api-Key",
    "x-api-key",
    "api_key",
    "apiKey",
    "access_token",
    "refresh_token",
    "Authorization",
    "authorization",
    "X-Signature",
    "signed_url",
    "SignedUrl",
    "AWSAccessKeyId",
    "X-Amz-Signature",
    "?token=",
    "&token=",
    "keychain",
    "Keychain",
    "0x",
];

/// Scan a serialized payload for secret-shaped values.
///
/// Returns every pattern found. An empty result means the payload is clean.
pub fn scan_for_secret_shapes(serialized: &str) -> Vec<&'static str> {
    FORBIDDEN_PAYLOAD_PATTERNS
        .iter()
        .copied()
        .filter(|pattern| serialized.contains(pattern))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventPayload;
    use crate::state::JobState;
    use plaudon_domain::ids::JobId;
    use plaudon_domain::time::Timestamp;

    fn record(seq: i64) -> EventRecord {
        EventRecord {
            seq,
            kind: EventKind::JobUpdated,
            job_id: Some(JobId::new()),
            recording_id: None,
            occurred_at: Timestamp::from_millis(1_700_000_000_000),
            payload: EventPayload::JobUpdated {
                state: JobState::CloudOutcomeUnknown,
                previous_state: JobState::CloudSending,
                wait_reason: None,
                possibly_billed: true,
            },
        }
    }

    #[test]
    fn ui_event_carries_the_durable_sequence_and_version() {
        let event = UiEvent::from(&record(7));
        assert_eq!(event.seq, 7);
        assert_eq!(event.version, UI_EVENT_CONTRACT_VERSION);
    }

    #[test]
    fn serialized_payloads_contain_no_secret_shapes() {
        let event = UiEvent::from(&record(1));
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            scan_for_secret_shapes(&json).is_empty(),
            "secret-shaped value in {json}"
        );
        assert!(!json.contains('/'), "no filesystem path may appear: {json}");
    }

    #[test]
    fn every_payload_variant_serializes_without_secret_shapes() {
        let payloads = vec![
            EventPayload::JobUpdated {
                state: JobState::RunningLocal,
                previous_state: JobState::Preflight,
                wait_reason: Some(crate::scheduler::WaitReason::ModelAssetMissing),
                possibly_billed: false,
            },
            EventPayload::JobFailed {
                state: JobState::FailedTerminal,
                code: plaudon_domain::error::AppErrorCode::IntegrityFailed,
                retryable: false,
            },
            EventPayload::RecordingImported {
                media_sha256: "a".repeat(64),
                byte_length: 1024,
            },
            EventPayload::RecordingSourceUpdated {
                change: crate::events::SourceChange::AudioChanged,
            },
            EventPayload::RecordingDeleted,
            EventPayload::TranscriptCompleted {
                transcript_revision: "rev-1".into(),
                segment_count: 12,
            },
            EventPayload::ToolCompleted {
                tool: "meeting_summary".into(),
                output_id: "out-1".into(),
            },
            EventPayload::ModelAvailabilityChanged {
                engine: "whisper_cpp".into(),
                available: true,
            },
            EventPayload::ConsentRequired {
                purpose: "cloud_transcription".into(),
            },
            EventPayload::LibraryChanged,
        ];
        for payload in payloads {
            let json = serde_json::to_string(&payload).expect("serialize");
            assert!(
                scan_for_secret_shapes(&json).is_empty(),
                "secret-shaped value in {json}"
            );
        }
    }

    #[test]
    fn secret_scanner_detects_planted_values() {
        assert_eq!(
            scan_for_secret_shapes("{\"url\":\"https://x?token=abc\"}").len(),
            1
        );
        assert_eq!(scan_for_secret_shapes("Bearer sk-live-123").len(), 2);
        assert_eq!(scan_for_secret_shapes("{\"state\":\"completed\"}").len(), 0);
    }

    #[test]
    fn a_failing_sink_reports_but_does_not_panic() {
        let sink = RecordingSink::new();
        sink.fail_from(3);
        assert!(sink.emit(&UiEvent::from(&record(2))).is_ok());
        assert!(sink.emit(&UiEvent::from(&record(3))).is_err());
        assert_eq!(sink.seqs(), vec![2]);
        sink.recover();
        assert!(sink.emit(&UiEvent::from(&record(3))).is_ok());
    }

    #[test]
    fn schema_is_valid_json_and_describes_every_event_kind() {
        let schema: serde_json::Value =
            serde_json::from_str(UI_EVENT_SCHEMA).expect("schema must be valid JSON");
        let kinds = schema["properties"]["kind"]["enum"]
            .as_array()
            .expect("kind enum");
        let names: Vec<&str> = kinds.iter().filter_map(serde_json::Value::as_str).collect();
        for kind in EventKind::ALL {
            assert!(
                names.contains(&kind.as_str()),
                "schema is missing {}",
                kind.as_str()
            );
        }
        assert_eq!(names.len(), EventKind::ALL.len());
    }
}
