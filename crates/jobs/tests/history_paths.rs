//! Filter, paging and integrity paths of the read-only job history
//! (SPEC v1.3 §5.5, §15.2).

mod common;

use common::{advance, enqueue, fixture, import_request, raw_write};

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use plaudon_jobs::error::{JobError, RetryClass};
use plaudon_jobs::events::{EventKind, EventPayload, EventRecord};
use plaudon_jobs::history::{ActivityEntry, JobHistory, JobHistoryQuery};
use plaudon_jobs::scheduler::WaitReason;
use plaudon_jobs::state::{JobPhase, JobState, TransitionCause};
use plaudon_jobs::store::JobOutput;

fn history(f: &common::Fixture) -> JobHistory {
    JobHistory::new(f.store.read_pool().clone())
}

#[tokio::test]
async fn an_empty_library_pages_to_nothing_with_a_zero_watermark() {
    let f = fixture().await;
    let history = history(&f);
    assert!(format!("{history:?}").contains("JobHistory"));

    let page = history
        .list_jobs(&JobHistoryQuery::all())
        .await
        .expect("list");

    assert!(page.jobs.is_empty(), "there is nothing to list");
    assert_eq!(page.total, 0);
    assert_eq!(page.watermark_seq, 0);
    let activity = history.activity(10).await.expect("activity");
    assert!(activity.is_empty(), "there is nothing to show");
}

#[tokio::test]
async fn the_page_size_is_clamped_to_at_least_one_and_at_most_five_hundred() {
    let f = fixture().await;
    for seed in "abcde".chars() {
        enqueue(&f.store, seed).await;
    }
    let history = history(&f);

    let clamped_low = history
        .list_jobs(&JobHistoryQuery::all().with_limit(0))
        .await
        .expect("list");
    assert_eq!(clamped_low.jobs.len(), 1);
    assert_eq!(clamped_low.total, 5, "the total ignores paging");

    let negative = history
        .list_jobs(&JobHistoryQuery::all().with_limit(-7))
        .await
        .expect("list");
    assert_eq!(negative.jobs.len(), 1);

    let clamped_high = history
        .list_jobs(&JobHistoryQuery::all().with_limit(10_000))
        .await
        .expect("list");
    assert_eq!(clamped_high.jobs.len(), 5);

    let defaulted = history
        .list_jobs(&JobHistoryQuery::all())
        .await
        .expect("list");
    assert_eq!(defaulted.jobs.len(), 5);
    assert!(defaulted.watermark_seq > 0);
}

#[tokio::test]
async fn paging_walks_the_whole_result_set_without_overlap() {
    let f = fixture().await;
    for seed in "fghij".chars() {
        enqueue(&f.store, seed).await;
    }
    let history = history(&f);

    let mut seen: Vec<JobId> = Vec::new();
    for offset in [0, 2, 4] {
        let mut query = JobHistoryQuery::all().with_limit(2);
        query.offset = Some(offset);
        let page = history.list_jobs(&query).await.expect("list");
        assert_eq!(page.total, 5);
        seen.extend(page.jobs.iter().map(|job| job.job_id));
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 5, "every row appears exactly once");

    // A negative offset is treated as the first page, never as an SQL error.
    let mut query = JobHistoryQuery::all().with_limit(2);
    query.offset = Some(-10);
    let page = history.list_jobs(&query).await.expect("list");
    assert_eq!(page.jobs.len(), 2);
}

#[tokio::test]
async fn every_filter_dimension_narrows_the_result_set() {
    let f = fixture().await;
    let recording = RecordingId::new();
    let matching = f
        .store
        .enqueue(import_request('k').with_recording(recording))
        .await
        .expect("enqueue")
        .job_id();
    advance(&f.store, matching, &[JobState::Preflight]).await;
    let other_recording = f
        .store
        .enqueue(import_request('l').with_recording(RecordingId::new()))
        .await
        .expect("enqueue")
        .job_id();
    let other_type = f
        .store
        .enqueue(
            plaudon_jobs::store::EnqueueRequest::new(
                "transcribe",
                plaudon_jobs::key::ProcessingJobKey::new(
                    "m".repeat(64),
                    plaudon_jobs::key::ProcessingStage::Transcribe,
                    "whisper_cpp",
                    "base",
                ),
                plaudon_jobs::scheduler::ResourceClass::Cpu,
            )
            .with_recording(recording),
        )
        .await
        .expect("enqueue")
        .job_id();
    let history = history(&f);

    let by_state = history
        .list_jobs(&JobHistoryQuery::all().with_states(vec![JobState::Preflight]))
        .await
        .expect("list");
    assert_eq!(by_state.total, 1);
    assert_eq!(by_state.jobs[0].job_id, matching);
    assert_eq!(by_state.jobs[0].phase, JobPhase::Pending);
    assert_eq!(by_state.jobs[0].retry_class, RetryClass::AutomaticRetry);

    let by_recording = history
        .list_jobs(&JobHistoryQuery::all().for_recording(recording))
        .await
        .expect("list");
    assert_eq!(by_recording.total, 2);
    assert!(by_recording
        .jobs
        .iter()
        .all(|job| job.recording_id == Some(recording)));

    let mut by_type = JobHistoryQuery::all();
    by_type.job_type = Some("transcribe".to_string());
    let by_type = history.list_jobs(&by_type).await.expect("list");
    assert_eq!(by_type.total, 1);
    assert_eq!(by_type.jobs[0].job_id, other_type);

    let mut combined = JobHistoryQuery::all().for_recording(recording);
    combined.job_type = Some("import".to_string());
    combined.since = Some(Timestamp::from_millis(0));
    let combined = history.list_jobs(&combined).await.expect("list");
    assert_eq!(combined.total, 1);
    assert_eq!(combined.jobs[0].job_id, matching);

    let mut future = JobHistoryQuery::all();
    future.since = Some(Timestamp::from_millis(
        Timestamp::now().as_millis() + 86_400_000,
    ));
    assert_eq!(history.list_jobs(&future).await.expect("list").total, 0);

    let mut nothing = JobHistoryQuery::all();
    nothing.job_type = Some("no-such-type".to_string());
    assert_eq!(history.list_jobs(&nothing).await.expect("list").total, 0);
    assert_ne!(other_recording, matching);
}

#[tokio::test]
async fn a_summary_reports_outputs_waiting_and_billing_state() {
    let f = fixture().await;
    let completed = enqueue(&f.store, 'n').await;
    advance(
        &f.store,
        completed,
        &[
            JobState::Preflight,
            JobState::RunningLocal,
            JobState::Verifying,
            JobState::Committing,
        ],
    )
    .await;
    f.store
        .complete(
            completed,
            vec![
                JobOutput::new("media_asset", "media/sha256/aa/aa.wav"),
                JobOutput::new("transcript_revision", "rev-1"),
            ],
            TransitionCause::system("committed"),
        )
        .await
        .expect("complete");

    let parked = enqueue(&f.store, 'o').await;
    f.store
        .park(
            parked,
            WaitReason::NetworkUnavailable,
            TransitionCause::system("offline"),
        )
        .await
        .expect("park");

    let page = history(&f)
        .list_jobs(&JobHistoryQuery::all())
        .await
        .expect("list");

    let completed_row = page
        .jobs
        .iter()
        .find(|job| job.job_id == completed)
        .expect("completed row");
    assert_eq!(completed_row.output_count, 2);
    assert_eq!(completed_row.phase, JobPhase::Terminal);
    assert_eq!(completed_row.retry_class, RetryClass::Terminal);
    assert!(completed_row.started_at.is_some());
    assert!(completed_row.finished_at.is_some());
    assert!(!completed_row.possibly_billed);
    assert!(!completed_row.is_reprocess);
    assert_eq!(completed_row.wait_reason, None);

    let parked_row = page
        .jobs
        .iter()
        .find(|job| job.job_id == parked)
        .expect("parked row");
    assert_eq!(parked_row.output_count, 0);
    assert_eq!(parked_row.phase, JobPhase::Waiting);
    assert_eq!(parked_row.retry_class, RetryClass::ResumeWhenAvailable);
    assert_eq!(parked_row.wait_reason, Some(WaitReason::NetworkUnavailable));
    assert_eq!(parked_row.retry_count, 0);
    assert_eq!(parked_row.last_error_code, None);
}

#[tokio::test]
async fn a_corrupt_job_row_fails_the_listing_instead_of_being_skipped() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'p').await;
    raw_write(
        &f.store,
        "UPDATE jobs SET state = 'levitating' WHERE id = ?1",
        vec![job.to_string()],
    )
    .await;

    let error = history(&f)
        .list_jobs(&JobHistoryQuery::all())
        .await
        .expect_err("must refuse");

    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("levitating")),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Activity history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn activity_is_newest_first_and_its_limit_is_clamped() {
    let f = fixture().await;
    let job = enqueue(&f.store, 'q').await;
    advance(
        &f.store,
        job,
        &[JobState::Preflight, JobState::RunningLocal],
    )
    .await;
    let history = history(&f);

    let all = history.activity(1_000_000).await.expect("activity");
    assert_eq!(all.len(), 3, "enqueue plus two transitions");
    assert!(
        all.windows(2).all(|pair| pair[0].seq > pair[1].seq),
        "entries are newest first"
    );
    assert!(all.iter().all(|entry| entry.job_id == Some(job)));
    assert!(all.iter().all(|entry| entry.recording_id.is_none()));
    assert!(all.iter().all(|entry| !entry.completion_notice));
    assert!(all.iter().all(|entry| entry.kind == EventKind::JobUpdated));
    assert!(all[0].occurred_at.as_millis() > 0);

    let clamped = history.activity(0).await.expect("activity");
    assert_eq!(clamped.len(), 1);
    assert_eq!(clamped[0].seq, all[0].seq);
}

#[tokio::test]
async fn a_failure_is_recorded_in_the_activity_history_as_a_completion_notice() {
    let f = fixture().await;
    let recording = RecordingId::new();
    let job = f
        .store
        .enqueue(import_request('r').with_recording(recording))
        .await
        .expect("enqueue")
        .job_id();
    f.store
        .fail(
            job,
            JobState::FailedTerminal,
            plaudon_domain::error::AppErrorCode::IntegrityFailed,
            TransitionCause::system("corrupt"),
        )
        .await
        .expect("fail");

    let activity = history(&f).activity(10).await.expect("activity");

    assert!(activity
        .iter()
        .any(|entry| entry.kind == EventKind::JobFailed && entry.completion_notice));
}

#[tokio::test]
async fn an_unknown_event_kind_in_the_log_is_reported_as_corrupt() {
    let f = fixture().await;
    enqueue(&f.store, 's').await;
    raw_write(
        &f.store,
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (9001, 'job.teleported', NULL, NULL, '{\"type\":\"library_changed\"}', 1)",
        Vec::new(),
    )
    .await;

    let error = history(&f).activity(10).await.expect_err("must refuse");

    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("job.teleported")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unparsable_job_reference_in_the_log_is_reported_as_corrupt() {
    let f = fixture().await;
    raw_write(
        &f.store,
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (9002, 'job.updated', 'not-a-job', NULL, '{\"type\":\"library_changed\"}', 1)",
        Vec::new(),
    )
    .await;

    let error = history(&f).activity(10).await.expect_err("must refuse");

    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("not-a-job")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unparsable_recording_reference_in_the_log_is_reported_as_corrupt() {
    let f = fixture().await;
    raw_write(
        &f.store,
        "INSERT INTO job_events (seq, kind, job_id, recording_id, payload_json, occurred_at)
         VALUES (9003, 'recording.imported', NULL, 'not-a-recording',
                 '{\"type\":\"library_changed\"}', 1)",
        Vec::new(),
    )
    .await;

    let error = history(&f).activity(10).await.expect_err("must refuse");

    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d.contains("not-a-recording")),
        "{error:?}"
    );
}

#[test]
fn an_activity_entry_derives_its_completion_notice_from_the_event_kind() {
    let recording = RecordingId::new();
    let record = EventRecord {
        seq: 42,
        kind: EventKind::RecordingImported,
        job_id: None,
        recording_id: Some(recording),
        occurred_at: Timestamp::from_millis(1_700_000_000_000),
        payload: EventPayload::RecordingImported {
            media_sha256: "a".repeat(64),
            byte_length: 11,
        },
    };

    let entry = ActivityEntry::from(&record);

    assert_eq!(entry.seq, 42);
    assert_eq!(entry.kind, EventKind::RecordingImported);
    assert_eq!(entry.job_id, None);
    assert_eq!(entry.recording_id, Some(recording));
    assert!(entry.completion_notice);

    let quiet = EventRecord {
        kind: EventKind::LibraryChanged,
        payload: EventPayload::LibraryChanged,
        ..record
    };
    assert!(!ActivityEntry::from(&quiet).completion_notice);
}

#[test]
fn a_history_query_round_trips_through_serde_with_every_filter_set() {
    let mut query = JobHistoryQuery::all()
        .with_states(vec![JobState::Queued, JobState::Completed])
        .for_recording(RecordingId::new())
        .with_limit(25);
    query.job_type = Some("import".to_string());
    query.since = Some(Timestamp::from_millis(1_700_000_000_000));
    query.offset = Some(50);

    let json = serde_json::to_string(&query).expect("serialize");
    let parsed: JobHistoryQuery = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(parsed, query);
    assert_eq!(JobHistoryQuery::default(), JobHistoryQuery::all());
}
