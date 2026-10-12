//! End-to-end drive of the real ports over a temporary library.
//!
//! No Tauri runtime, no network, no OS notification: the commands' own logic
//! is exercised through [`plaudon_desktop::AppPorts`] exactly as the IPC layer
//! calls it.

use std::sync::Arc;
use std::time::Duration;

use plaudon_notifications::testing::RecordingNotifier;
use plaudon_notifications::{
    Dispatcher, NotificationSettings as CrateSettings, Notifier, OsPermission,
};

use plaudon_desktop::commands::{import, jobs, library, settings, transcript};
use plaudon_desktop::dto::{
    CancelJobRequest, DuplicatePolicy, EventKind, ExportFormat, ExportRecordingRequest,
    GetRecordingRequest, GetTranscriptRequest, ImportFilesRequest, ImportOutcome, JobKind,
    JobState, ListActivityRequest, ListJobsRequest, ListRecordingsRequest, NotificationCategory,
    RecordingFilter, RecordingSort, RecordingSortField, SaveTranscriptRevisionRequest, SegmentEdit,
    SetLocalOnlyModeRequest, SortDirection,
};
use plaudon_desktop::events::{EventSink, RecordingEventSink};
use plaudon_desktop::ports::{MachineRevisionSpec, MachineSegment};
use plaudon_desktop::secrets::{InMemorySecretStore, SecretStore};
use plaudon_desktop::{build_ports, AppPorts};
use plaudon_domain::error::AppErrorCode;
use plaudon_domain::transcript::AlignmentOrigin;

struct Harness {
    ports: AppPorts,
    sink: Arc<RecordingEventSink>,
    notifier: Arc<RecordingNotifier>,
    _library: tempfile::TempDir,
    sources: tempfile::TempDir,
    exports: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        Self::with_notifier(RecordingNotifier::granted()).await
    }

    async fn with_notifier(notifier: Arc<RecordingNotifier>) -> Self {
        let library = tempfile::tempdir().expect("tempdir");
        let sources = tempfile::tempdir().expect("tempdir");
        let exports = tempfile::tempdir().expect("tempdir");
        let sink = Arc::new(RecordingEventSink::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let dispatcher = Arc::new(
            Dispatcher::new(
                Arc::clone(&notifier) as Arc<dyn Notifier>,
                CrateSettings::uniform(false),
            )
            .with_timeout(Duration::from_millis(100)),
        );
        let ports = build_ports(
            library.path(),
            Arc::clone(&sink) as Arc<dyn EventSink>,
            dispatcher,
            Arc::clone(&secrets) as Arc<dyn SecretStore>,
            vec![sources.path().to_path_buf()],
        )
        .await
        .expect("the library opens");
        Self {
            ports,
            sink,
            notifier,
            _library: library,
            sources,
            exports,
        }
    }

    fn source_file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.sources.path().join(name);
        std::fs::write(&path, bytes).expect("write a source file");
        path.to_string_lossy().into_owned()
    }

    fn list_request(limit: u32, cursor: Option<String>) -> ListRecordingsRequest {
        ListRecordingsRequest {
            cursor,
            limit,
            filter: RecordingFilter {
                query: None,
                import_status: Vec::new(),
                transcription_status: Vec::new(),
                sources: Vec::new(),
                tags: Vec::new(),
                date_from: None,
                date_to: None,
            },
            sort: RecordingSort {
                field: RecordingSortField::Title,
                direction: SortDirection::Asc,
            },
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)]
async fn the_shell_drives_import_review_export_and_activity() {
    let harness = Harness::new().await;
    let ports = &harness.ports;

    // ---------------------------------------------------------- import two
    let first = harness.source_file("alpha.wav", b"RIFF alpha payload");
    let second = harness.source_file("beta.wav", b"RIFF beta payload");
    let duplicate = harness.source_file("alpha-copy.wav", b"RIFF alpha payload");

    let imported = import::import_files(
        ports,
        ImportFilesRequest {
            paths: vec![first, second],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the import request is accepted");
    assert_eq!(imported.results.len(), 2);
    assert!(imported
        .results
        .iter()
        .all(|r| matches!(r, ImportOutcome::Accepted { .. })));

    let duplicate_result = import::import_files(
        ports,
        ImportFilesRequest {
            paths: vec![duplicate],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the duplicate request is accepted");
    let ImportOutcome::Duplicate {
        existing_recording_id,
        existing_title,
        ..
    } = &duplicate_result.results[0]
    else {
        panic!(
            "identical bytes must be reported as a duplicate: {:?}",
            duplicate_result.results[0]
        )
    };
    assert_eq!(existing_title, "alpha");

    // ------------------------------------------------------------ paginate
    let page_one = library::list_recordings(ports, Harness::list_request(1, None))
        .await
        .expect("the first page");
    assert_eq!(page_one.items.len(), 1);
    assert_eq!(
        page_one.total, 2,
        "the duplicate created no second recording"
    );
    assert_eq!(page_one.items[0].title, "alpha");
    let cursor = page_one.next_cursor.clone().expect("a second page exists");

    let page_two = library::list_recordings(ports, Harness::list_request(1, Some(cursor)))
        .await
        .expect("the second page");
    assert_eq!(page_two.items[0].title, "beta");
    assert!(page_two.next_cursor.is_none());

    let recording_id = page_one.items[0].recording_id;
    assert_eq!(recording_id, *existing_recording_id);

    // ------------------------------------------------------------- detail
    let detail = library::get_recording(
        ports,
        GetRecordingRequest {
            recording_id: recording_id.to_string(),
        },
    )
    .await
    .expect("the recording detail");
    assert_eq!(detail.media.original_file_name, "alpha.wav");
    assert_eq!(
        detail.summary.duration_ms, None,
        "nothing measured a duration"
    );
    assert_eq!(detail.current_revision_id, None);
    assert_eq!(detail.revisions.len(), 0);

    // --------------------------------------------------------- transcript
    let machine = ports
        .transcript
        .create_machine_revision(MachineRevisionSpec {
            recording_id,
            segments: vec![
                MachineSegment::timed("welcome everyone", 0, 1_500),
                MachineSegment::timed("lets begin", 1_500, 3_000),
            ],
            engine_id: "whisper.cpp".into(),
            model_id: "small".into(),
            alignment_origin: AlignmentOrigin::Native,
        })
        .await
        .expect("the machine revision is stored");

    let page = transcript::get_transcript(
        ports,
        GetTranscriptRequest {
            revision_id: machine.revision_id.to_string(),
            cursor: None,
            limit: 10,
        },
    )
    .await
    .expect("the transcript page");
    assert_eq!(page.total_segments, 2);
    assert_eq!(page.segments[0].text, "welcome everyone");
    assert_eq!(page.segments[1].start_ms, Some(1_500));

    // ------------------------------------------------------------- edit
    let saved = transcript::save_transcript_revision(
        ports,
        SaveTranscriptRevisionRequest {
            recording_id: recording_id.to_string(),
            base_revision_id: machine.revision_id.to_string(),
            edits: vec![SegmentEdit {
                index: 1,
                text: "let us begin".into(),
            }],
        },
    )
    .await
    .expect("the edit is saved as a new revision");
    assert_ne!(saved.revision_id, machine.revision_id);

    let conflict = transcript::save_transcript_revision(
        ports,
        SaveTranscriptRevisionRequest {
            recording_id: recording_id.to_string(),
            base_revision_id: machine.revision_id.to_string(),
            edits: vec![SegmentEdit {
                index: 0,
                text: "overwrite".into(),
            }],
        },
    )
    .await
    .expect_err("a stale base must conflict");
    assert_eq!(conflict.code(), AppErrorCode::Conflict);

    // The superseded revision is still readable, unchanged.
    let old = transcript::get_transcript(
        ports,
        GetTranscriptRequest {
            revision_id: machine.revision_id.to_string(),
            cursor: None,
            limit: 10,
        },
    )
    .await
    .expect("the old revision is still readable");
    assert_eq!(old.segments[1].text, "lets begin");
    assert!(old.revision.superseded);

    let new = transcript::get_transcript(
        ports,
        GetTranscriptRequest {
            revision_id: saved.revision_id.to_string(),
            cursor: None,
            limit: 10,
        },
    )
    .await
    .expect("the new revision");
    assert_eq!(new.segments[1].text, "let us begin");

    // ------------------------------------------------------------ export
    let destination = harness.exports.path().join("alpha.md");
    let export = jobs::export_recording(
        ports,
        ExportRecordingRequest {
            recording_id: recording_id.to_string(),
            revision_id: saved.revision_id.to_string(),
            format: ExportFormat::Markdown,
            destination: destination.to_string_lossy().into_owned(),
        },
    )
    .await
    .expect("the export job runs");
    let written = std::fs::read_to_string(&destination).expect("the export file exists");
    assert!(written.contains("let us begin"), "{written}");
    assert!(written.starts_with("# alpha"), "{written}");

    // -------------------------------------------------------------- jobs
    let job_page = jobs::list_jobs(
        ports,
        ListJobsRequest {
            cursor: None,
            limit: 50,
            states: Vec::new(),
            kinds: Vec::new(),
        },
    )
    .await
    .expect("the job list");
    let completed: Vec<_> = job_page
        .items
        .iter()
        .filter(|job| job.state == JobState::Completed)
        .collect();
    assert_eq!(completed.len(), 3, "two imports and one export committed");
    assert!(job_page.items.iter().any(|job| job.job_id == export.job_id
        && job.kind == JobKind::Export
        && job.state == JobState::Completed));
    assert_eq!(job_page.state_counts.get(&JobState::Completed), Some(&3));

    // The duplicate decision is a real job waiting for the user, and it can be
    // cancelled.
    let waiting = job_page
        .items
        .iter()
        .find(|job| job.state == JobState::WaitingForUser)
        .expect("the duplicate decision is a visible job");
    let cancelled = jobs::cancel_job(
        ports,
        CancelJobRequest {
            job_id: waiting.job_id.to_string(),
        },
    )
    .await
    .expect("the waiting job can be cancelled");
    assert_eq!(cancelled.state, JobState::Cancelled);

    // ---------------------------------------------------------- activity
    let activity = settings::list_activity(
        ports,
        ListActivityRequest {
            cursor: None,
            limit: 50,
        },
    )
    .await
    .expect("the activity page");
    assert_eq!(activity.items.len(), 2, "one entry per committed import");
    assert!(activity
        .items
        .iter()
        .all(|entry| entry.category == NotificationCategory::ImportFinished));
    assert!(
        activity
            .items
            .iter()
            .all(|entry| !entry.notification_delivered),
        "no notification was delivered by this backend"
    );
    assert_eq!(activity.unread_count, 2);

    // ------------------------------------------------------------ events
    let sequences = harness.sink.sequences();
    assert_ne!(sequences.len(), 0);
    for pair in sequences.windows(2) {
        assert!(
            pair[1] > pair[0],
            "event sequence must increase strictly: {sequences:?}"
        );
    }
    assert_eq!(
        sequences,
        (1..=sequences.len() as u64).collect::<Vec<_>>(),
        "the sequence has no gaps"
    );
    let kinds = harness.sink.kinds();
    assert!(kinds.contains(&EventKind::RecordingImported));
    assert!(kinds.contains(&EventKind::LibraryChanged));
    assert!(kinds.contains(&EventKind::ActivityAdded));
    assert!(kinds.contains(&EventKind::TranscriptCompleted));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_commands_refuse_to_act_without_their_confirmation() {
    let harness = Harness::new().await;
    let error = settings::set_local_only_mode(
        &harness.ports,
        SetLocalOnlyModeRequest {
            enabled: true,
            confirmed: false,
        },
    )
    .await
    .expect_err("an unconfirmed policy change is refused");
    assert_eq!(error.code(), AppErrorCode::ConfirmationRequired);
    assert!(
        !harness
            .ports
            .settings
            .preferences()
            .await
            .expect("preferences")
            .local_only_mode,
        "nothing changed"
    );

    let applied = settings::set_local_only_mode(
        &harness.ports,
        SetLocalOnlyModeRequest {
            enabled: true,
            confirmed: true,
        },
    )
    .await
    .expect("a confirmed change applies");
    assert!(applied.enabled);
    assert_eq!(applied.allowed_hosts.len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_openrouter_key_never_appears_in_any_response() {
    const KEY: &str = "sk-or-v1-00112233445566778899aabbccddeeff";
    let harness = Harness::new().await;

    let status = settings::set_openrouter_key(
        &harness.ports,
        plaudon_desktop::dto::SetOpenRouterKeyRequest {
            key: KEY.to_owned(),
        },
    )
    .await
    .expect("the key is stored");
    assert!(serde_json::to_string(&status)
        .expect("serializes")
        .contains("true"));

    let capabilities = settings::get_capabilities(&harness.ports)
        .await
        .expect("capabilities are reported");
    assert!(capabilities.openrouter_key_present);
    let rendered = serde_json::to_string(&capabilities).expect("capabilities serialize");
    assert!(
        !rendered.contains(KEY),
        "the key leaked into get_capabilities"
    );
    assert!(
        !rendered.contains("sk-or"),
        "a key prefix leaked into get_capabilities"
    );

    settings::remove_openrouter_key(&harness.ports)
        .await
        .expect("the key is removed");
    let capabilities = settings::get_capabilities(&harness.ports)
        .await
        .expect("capabilities are reported");
    assert!(!capabilities.openrouter_key_present);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hostile_path_is_rejected_without_touching_the_library() {
    let harness = Harness::new().await;
    let outside = tempfile::tempdir().expect("tempdir");
    let escaping = outside.path().join("elsewhere.wav");
    std::fs::write(&escaping, b"RIFF").expect("write");

    let response = import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![
                escaping.to_string_lossy().into_owned(),
                harness.source_file("notes.txt", b"not audio"),
            ],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the request itself is valid");

    let codes: Vec<_> = response
        .results
        .iter()
        .map(|result| match result {
            ImportOutcome::Rejected { error, .. } => error.code,
            other => panic!("expected a rejection, got {other:?}"),
        })
        .collect();
    assert_eq!(
        codes,
        vec![
            AppErrorCode::ArchivePathRejected,
            AppErrorCode::UnsupportedMedia
        ]
    );

    let page = library::list_recordings(&harness.ports, Harness::list_request(10, None))
        .await
        .expect("the library is still readable");
    assert_eq!(page.total, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_media_scheme_serves_ranges_and_refuses_anything_but_a_recording_id() {
    let harness = Harness::new().await;
    let payload: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let path = harness.source_file("audio.wav", &payload);
    import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![path],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the import is accepted");
    let recording_id = library::list_recordings(&harness.ports, Harness::list_request(10, None))
        .await
        .expect("the library lists the import")
        .items[0]
        .recording_id;

    let full =
        plaudon_desktop::media::serve(&harness.ports.library, &format!("/{recording_id}"), None)
            .await;
    assert_eq!(full.status, 200);
    assert_eq!(full.content_type, "audio/wav");
    assert_eq!(full.body.len(), payload.len());

    let partial = plaudon_desktop::media::serve(
        &harness.ports.library,
        &format!("/{recording_id}"),
        Some("bytes=100-199"),
    )
    .await;
    assert_eq!(partial.status, 206);
    assert_eq!(partial.content_range.as_deref(), Some("bytes 100-199/4096"));
    assert_eq!(partial.body, payload[100..=199]);

    for hostile in [
        "/..%2F..%2Fetc%2Fpasswd".to_owned(),
        "/../../etc/passwd".to_owned(),
        format!("/{recording_id}/../other"),
        format!("/{}", plaudon_domain::ids::RecordingId::new()),
        "/media/ab/cd/blob.wav".to_owned(),
    ] {
        let response = plaudon_desktop::media::serve(&harness.ports.library, &hostile, None).await;
        assert_eq!(response.status, 404, "{hostile} must not resolve");
        assert_eq!(response.body.len(), 0);
    }
}
