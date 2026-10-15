//! Durable event log and post-commit dispatch paths (SPEC v1.3 §14.5, §15.3).

mod common;

use std::sync::{Arc, Mutex};

use common::{advance, enqueue, fixture, raw_write, Fixture};

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_jobs::error::JobError;
use plaudon_jobs::events::{
    EventDispatcher, EventKind, EventPayload, EventRecord, EventSubscriber, PendingEvent,
    SourceChange, SubscriberError,
};
use plaudon_jobs::state::JobState;

/// A subscriber that records what it saw and can be made to refuse everything
/// from a chosen sequence number on.
#[derive(Debug)]
struct Recorder {
    id: String,
    seen: Mutex<Vec<i64>>,
    refuse_from: Mutex<Option<i64>>,
}

impl Recorder {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_string(),
            seen: Mutex::new(Vec::new()),
            refuse_from: Mutex::new(None),
        })
    }

    fn refuse_from(&self, seq: i64) {
        *self.refuse_from.lock().expect("lock") = Some(seq);
    }

    fn accept_everything(&self) {
        *self.refuse_from.lock().expect("lock") = None;
    }

    fn seen(&self) -> Vec<i64> {
        self.seen.lock().expect("lock").clone()
    }
}

impl EventSubscriber for Recorder {
    fn consumer_id(&self) -> &str {
        &self.id
    }

    fn handle(&self, event: &EventRecord) -> Result<(), SubscriberError> {
        if let Some(from) = *self.refuse_from.lock().expect("lock") {
            if event.seq >= from {
                return Err(SubscriberError {
                    consumer: self.id.clone(),
                    reason: format!("refusing seq {}", event.seq),
                });
            }
        }
        self.seen.lock().expect("lock").push(event.seq);
        Ok(())
    }
}

fn dispatcher_for(f: &Fixture, subscribers: Vec<Arc<dyn EventSubscriber>>) -> EventDispatcher {
    EventDispatcher::new(
        subscribers,
        f.store.database().writer().clone(),
        f.store.events().clone(),
    )
}

/// Commit three events for one job.
async fn three_events(f: &Fixture, seed: char) -> JobId {
    let job = enqueue(&f.store, seed).await;
    advance(
        &f.store,
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;
    job
}

#[tokio::test]
async fn an_empty_log_reports_zero_for_every_counter() {
    let f = fixture().await;
    let log = f.store.events();

    assert_eq!(log.latest_seq().await.expect("seq"), 0);
    assert_eq!(log.count().await.expect("count"), 0);
    assert_eq!(log.cursor("never-seen").await.expect("cursor"), 0);
    let events = log.read_after(0, 10).await.expect("read");
    assert!(events.is_empty(), "nothing has been committed");
}

#[tokio::test]
async fn dispatch_advances_every_consumer_cursor_independently() {
    let f = fixture().await;
    let fast = Recorder::new("fast");
    let slow = Recorder::new("slow");
    let dispatcher = dispatcher_for(
        &f,
        vec![
            fast.clone() as Arc<dyn EventSubscriber>,
            slow.clone() as Arc<dyn EventSubscriber>,
        ],
    );
    assert_eq!(dispatcher.consumer_ids(), vec!["fast", "slow"]);
    assert!(format!("{dispatcher:?}").contains("subscribers"));

    three_events(&f, 'a').await;
    let events = f.store.events().read_after(0, 100).await.expect("read");
    assert_eq!(events.len(), 3);
    slow.refuse_from(events[1].seq);

    dispatcher.dispatch(&events).await;

    assert_eq!(
        fast.seen(),
        vec![events[0].seq, events[1].seq, events[2].seq]
    );
    assert_eq!(slow.seen(), vec![events[0].seq]);
    assert_eq!(
        f.store.events().cursor("fast").await.expect("cursor"),
        events[2].seq
    );
    assert_eq!(
        f.store.events().cursor("slow").await.expect("cursor"),
        events[0].seq,
        "a refused event holds the cursor back"
    );
}

#[tokio::test]
async fn dispatching_nothing_leaves_every_cursor_alone() {
    let f = fixture().await;
    let recorder = Recorder::new("quiet");
    let dispatcher = dispatcher_for(&f, vec![recorder.clone() as Arc<dyn EventSubscriber>]);

    dispatcher.dispatch(&[]).await;

    assert!(
        recorder.seen().is_empty(),
        "an empty dispatch delivers nothing"
    );
    assert_eq!(f.store.events().cursor("quiet").await.expect("cursor"), 0);
}

#[tokio::test]
async fn a_subscriber_that_refuses_the_very_first_event_stalls_the_replay() {
    let f = fixture().await;
    let recorder = Recorder::new("stalled");
    let dispatcher = dispatcher_for(&f, vec![recorder.clone() as Arc<dyn EventSubscriber>]);
    three_events(&f, 'b').await;
    recorder.refuse_from(1);

    let delivered = dispatcher.replay_pending(256).await.expect("replay");

    assert_eq!(delivered, 0);
    assert_eq!(f.store.events().cursor("stalled").await.expect("cursor"), 0);

    // Once the consumer recovers, the replay delivers exactly what it missed.
    recorder.accept_everything();
    let delivered = dispatcher.replay_pending(256).await.expect("replay");
    assert_eq!(delivered, 3);
    assert_eq!(recorder.seen().len(), 3);

    // A third replay has nothing left to do.
    assert_eq!(dispatcher.replay_pending(256).await.expect("replay"), 0);
}

#[tokio::test]
async fn a_replay_batch_smaller_than_the_log_still_drains_it() {
    let f = fixture().await;
    let recorder = Recorder::new("batched");
    let dispatcher = dispatcher_for(&f, vec![recorder.clone() as Arc<dyn EventSubscriber>]);
    three_events(&f, 'c').await;

    assert_eq!(dispatcher.replay_pending(1).await.expect("replay"), 3);
    assert_eq!(recorder.seen().len(), 3);
}

#[tokio::test]
async fn a_cursor_that_cannot_be_persisted_never_fails_the_committed_job() {
    let f = fixture().await;
    let recorder = Recorder::new("doomed");
    let dispatcher = dispatcher_for(&f, vec![recorder.clone() as Arc<dyn EventSubscriber>]);
    three_events(&f, 'd').await;
    let events = f.store.events().read_after(0, 100).await.expect("read");

    f.store.database().close().await;
    // The write of the cursor fails; the dispatch itself still happened and
    // nothing is propagated to the caller.
    dispatcher.dispatch(&events).await;

    assert_eq!(recorder.seen().len(), 3);
}

#[tokio::test]
async fn an_unknown_event_kind_in_the_log_fails_the_read() {
    let f = fixture().await;
    raw_write(
        &f.store,
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (9101, 'job.teleported', NULL, NULL, '{\"type\":\"library_changed\"}', 1)",
        Vec::new(),
    )
    .await;

    let error = f
        .store
        .events()
        .read_after(0, 100)
        .await
        .expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("job.teleported")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unparsable_event_payload_fails_the_read() {
    let f = fixture().await;
    raw_write(
        &f.store,
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (9102, 'library.changed', NULL, NULL, 'definitely not json', 1)",
        Vec::new(),
    )
    .await;

    let error = f
        .store
        .events()
        .read_after(0, 100)
        .await
        .expect_err("must refuse");
    assert!(matches!(error, JobError::Encoding { .. }), "{error:?}");
}

#[tokio::test]
async fn an_unparsable_job_reference_in_an_event_row_fails_the_read() {
    let f = fixture().await;
    raw_write(
        &f.store,
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (9103, 'library.changed', 'not-a-job', NULL, '{\"type\":\"library_changed\"}', 1)",
        Vec::new(),
    )
    .await;

    let error = f
        .store
        .events()
        .read_after(0, 100)
        .await
        .expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("not-a-job")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unparsable_recording_reference_in_an_event_row_fails_the_read() {
    let f = fixture().await;
    raw_write(
        &f.store,
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (9104, 'library.changed', NULL, 'not-a-recording',
                 '{\"type\":\"library_changed\"}', 1)",
        Vec::new(),
    )
    .await;

    let error = f
        .store
        .events()
        .read_after(0, 100)
        .await
        .expect_err("must refuse");
    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("not-a-recording")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_recording_scoped_event_round_trips_through_the_log() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'e').await;
    let recording = RecordingId::new();
    advance(&f.store, job, &[JobState::Preflight]).await;

    f.store
        .transition_with_events(
            job,
            JobState::RunningLocal,
            plaudon_jobs::state::TransitionCause::system("start"),
            vec![
                PendingEvent::for_recording(
                    recording,
                    EventKind::RecordingSourceUpdated,
                    EventPayload::RecordingSourceUpdated {
                        change: SourceChange::AudioChanged,
                    },
                ),
                PendingEvent::for_job(
                    job,
                    EventKind::ConsentRequired,
                    EventPayload::ConsentRequired {
                        purpose: "cloud_transcription".to_string(),
                    },
                )
                .with_recording(recording),
            ],
        )
        .await
        .expect("transition");

    let events = f.store.events().read_after(0, 100).await.expect("read");
    let source_updated = events
        .iter()
        .find(|e| e.kind == EventKind::RecordingSourceUpdated)
        .expect("source event");
    assert_eq!(source_updated.recording_id, Some(recording));
    assert_eq!(source_updated.job_id, None);
    assert_eq!(
        source_updated.payload,
        EventPayload::RecordingSourceUpdated {
            change: SourceChange::AudioChanged
        }
    );

    let consent = events
        .iter()
        .find(|e| e.kind == EventKind::ConsentRequired)
        .expect("consent event");
    assert_eq!(consent.job_id, Some(job));
    assert_eq!(consent.recording_id, Some(recording));

    let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(
        seqs,
        (1..=i64::try_from(seqs.len()).expect("fits")).collect::<Vec<_>>()
    );
    assert_eq!(
        f.store.events().count().await.expect("count"),
        i64::try_from(events.len()).expect("fits")
    );
}

#[test]
fn a_subscriber_error_names_its_consumer_and_its_reason() {
    let error = SubscriberError {
        consumer: "desktop-ui".to_string(),
        reason: "channel closed".to_string(),
    };

    let rendered = error.to_string();
    assert!(rendered.contains("desktop-ui"), "{rendered}");
    assert!(rendered.contains("channel closed"), "{rendered}");
}
