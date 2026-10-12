//! End-to-end drive of the real ports over a temporary library.
//!
//! No user interface, no network, no OS notification: the commands' own logic
//! is exercised through [`plaudon_app::AppPorts`] exactly as
//! [`plaudon_app::command::Dispatcher`] calls it, which is the single path the
//! egui screens take.
//!
//! This suite was written against the Tauri shell crate, where the same ports
//! lived behind the IPC handlers; it moved here with EPIC O task O2.5 when that
//! shell was deleted. Nothing in it ever referred to Tauri, so the move is a
//! change of address only. The six top-level cases cover the whole import →
//! review → export → activity walk, the policy commands refusing to act
//! without their confirmation, the OpenRouter key never surfacing in a
//! response, a hostile import path, the media reader's byte ranges and the
//! notification permission prompt; the `cloud` module below adds seven more
//! that drive the cloud commands over a transport that never touches a network.

use std::sync::Arc;
use std::time::Duration;

use plaudon_notifications::testing::RecordingNotifier;
use plaudon_notifications::{
    Dispatcher, NotificationSettings as CrateSettings, Notifier, OsPermission,
};

use plaudon_app::commands::{import, jobs, library, settings, transcript};
use plaudon_app::dto::{
    CancelJobRequest, DuplicatePolicy, EventKind, ExportFormat, ExportRecordingRequest,
    GetRecordingRequest, GetTranscriptRequest, ImportFilesRequest, ImportOutcome, JobKind,
    JobState, ListActivityRequest, ListJobsRequest, ListRecordingsRequest, NotificationCategory,
    RecordingFilter, RecordingSort, RecordingSortField, SaveTranscriptRevisionRequest, SegmentEdit,
    SetLocalOnlyModeRequest, SortDirection,
};
use plaudon_app::events::{EventSink, RecordingEventSink};
use plaudon_app::ports::{MachineRevisionSpec, MachineSegment};
use plaudon_app::secrets::{InMemorySecretStore, SecretStore};
use plaudon_app::{build_ports, AppPorts};
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
        plaudon_app::dto::SetOpenRouterKeyRequest {
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
        plaudon_app::media::serve(&harness.ports.library, &format!("/{recording_id}"), None).await;
    assert_eq!(full.status, 200);
    assert_eq!(full.content_type, "audio/wav");
    assert_eq!(full.body.len(), payload.len());

    let partial = plaudon_app::media::serve(
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
        let response = plaudon_app::media::serve(&harness.ports.library, &hostile, None).await;
        assert_eq!(response.status, 404, "{hostile} must not resolve");
        assert_eq!(response.body.len(), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabling_notifications_prompts_once_and_reports_what_the_os_answered() {
    // The OS has not been asked yet; enabling the first category is the only
    // moment the shell prompts (spec §5.5).
    let notifier = RecordingNotifier::with_permission(OsPermission::NotRequested);
    notifier.granting_on_request(OsPermission::Granted).await;
    let harness = Harness::with_notifier(notifier).await;

    assert_eq!(
        harness.notifier.permission_prompts().await,
        0,
        "opening the library never prompts"
    );

    let mut categories = std::collections::BTreeMap::new();
    for category in NotificationCategory::ALL {
        categories.insert(*category, *category == NotificationCategory::ImportFinished);
    }
    let response = settings::set_notification_settings(
        &harness.ports,
        plaudon_app::dto::SetNotificationSettingsRequest {
            settings: plaudon_app::dto::NotificationSettings {
                enabled: true,
                categories,
            },
        },
    )
    .await
    .expect("the settings are stored");
    assert_eq!(
        response.os_permission,
        plaudon_app::dto::OsNotificationPermission::Granted,
        "the response reports what the dispatcher observed"
    );
    assert_eq!(harness.notifier.permission_prompts().await, 1);

    // The same permission is then reported by get_capabilities.
    let capabilities = settings::get_capabilities(&harness.ports)
        .await
        .expect("capabilities are reported");
    assert_eq!(
        capabilities.permissions["notifications"],
        plaudon_app::dto::PermissionState::Granted
    );
}

/// Hermetic drive of the cloud commands: an in-memory key store, a transport
/// that never touches a network and a library in a temporary directory.
mod cloud {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use async_trait::async_trait;
    use plaudon_ai_contracts::capabilities::RequestFamily;
    use plaudon_ai_openrouter::allowlist::ModelTask;
    use plaudon_ai_openrouter::catalog::{ModelProbeRecord, ProbeEvidence, TimestampSupport};
    use plaudon_ai_openrouter::transport::{AsyncHttpTransport, TransportFailure};
    use plaudon_app::backend::LocalBackend;
    use plaudon_app::commands::{cloud, import, jobs, library, settings};
    use plaudon_app::dto::{
        ConsentScopeDto, CostConfidence, DuplicatePolicy, EmptyRequest, GetUsageRequest,
        GrantCloudConsentRequest, ImportFilesRequest, JobState, ListJobsRequest, ListModelsRequest,
        ListModelsResponse, ModelOverride, ModelPreferenceScope, RetryJobRequest,
        RevokeCloudConsentRequest, SetCloudBudgetRequest, SetLocalOnlyModeRequest,
        SetModelPreferenceRequest, StartProcessingRequest, VerifyModelRequest,
    };
    use plaudon_app::error::{envelope, to_command_result};
    use plaudon_app::events::{EventSink, RecordingEventSink};
    use plaudon_app::secrets::{InMemorySecretStore, SecretStore, SecretValue};
    use plaudon_app::AppPorts;
    use plaudon_domain::capability::{Known, Tri};
    use plaudon_domain::error::{AppError, AppErrorCode, ErrorAction};
    use plaudon_domain::ids::RecordingId;
    use plaudon_domain::time::Timestamp;
    use plaudon_notifications::testing::RecordingNotifier;
    use plaudon_notifications::{Dispatcher, NotificationSettings as CrateSettings, Notifier};
    use plaudon_security::outbound::http::{GatedRequest, HttpResponse};
    use sqlx::SqlitePool;

    const KEY: &str = "sk-or-v1-0123456789abcdef0123456789abcdef";
    const MODEL: &str = "vendor/verified-stt";

    /// Records every send and the ledger size at the moment of the send.
    #[derive(Debug, Default)]
    struct FakeTransport {
        calls: AtomicUsize,
        ledger_rows_at_send: Mutex<Vec<i64>>,
        pool: OnceLock<SqlitePool>,
    }

    #[async_trait]
    impl AsyncHttpTransport for FakeTransport {
        async fn send(&self, _request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(pool) = self.pool.get() {
                let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inference_attempts")
                    .fetch_one(pool)
                    .await
                    .expect("count the ledger");
                self.ledger_rows_at_send.lock().expect("lock").push(rows);
            }
            // The body may have been transmitted: the outcome is unknown.
            Err(TransportFailure::TimedOutAfterSend)
        }
    }

    struct CloudHarness {
        backend: Arc<LocalBackend>,
        ports: AppPorts,
        transport: Arc<FakeTransport>,
        _library: tempfile::TempDir,
        sources: tempfile::TempDir,
    }

    impl CloudHarness {
        async fn new(with_key: bool) -> Self {
            let library = tempfile::tempdir().expect("tempdir");
            let sources = tempfile::tempdir().expect("tempdir");
            let sink = Arc::new(RecordingEventSink::new());
            let secrets = Arc::new(InMemorySecretStore::new());
            if with_key {
                secrets
                    .set_openrouter_key(&SecretValue::new(KEY.into()).expect("a key"))
                    .expect("store the key");
            }
            let dispatcher = Arc::new(
                Dispatcher::new(
                    RecordingNotifier::granted() as Arc<dyn Notifier>,
                    CrateSettings::uniform(false),
                )
                .with_timeout(Duration::from_millis(100)),
            );
            let transport = Arc::new(FakeTransport::default());
            let backend = Arc::new(
                LocalBackend::open(
                    library.path(),
                    Arc::clone(&sink) as Arc<dyn EventSink>,
                    dispatcher,
                    Arc::clone(&secrets) as Arc<dyn SecretStore>,
                    vec![sources.path().to_path_buf()],
                )
                .await
                .expect("the library opens")
                .with_cloud_transport(Arc::clone(&transport) as Arc<dyn AsyncHttpTransport>),
            );
            transport
                .pool
                .set(backend.pool().clone())
                .expect("the pool is set once");
            let events = backend.events().clone();
            let ports = AppPorts {
                capabilities: Arc::clone(&backend) as _,
                library: Arc::clone(&backend) as _,
                transcript: Arc::clone(&backend) as _,
                jobs: Arc::clone(&backend) as _,
                activity: Arc::clone(&backend) as _,
                import: Arc::clone(&backend) as _,
                settings: Arc::clone(&backend) as _,
                export: Arc::clone(&backend) as _,
                cloud: Arc::clone(&backend) as _,
                sources: Arc::clone(&backend) as _,
                search: Arc::clone(&backend) as _,
                speakers: Arc::clone(&backend) as _,
                devices: Arc::new(plaudon_app::backend::devices::DevicesBackend::new(
                    plaudon_app::backend::devices::DevicesDeps::production(
                        backend.store().db().clone(),
                        backend.store().archive().clone(),
                        backend.events().clone(),
                        Arc::new(plaudon_app::backend::devices::NoRadio),
                    )
                    .expect("device dependencies"),
                )),
                outputs: Arc::clone(&backend) as Arc<dyn plaudon_app::ports::OutputsPort>,
                tools: Arc::clone(&backend) as Arc<dyn plaudon_app::ports::ToolsPort>,
                templates: Arc::new(plaudon_app::backend::templates::TemplatesBackend::new(
                    library.path(),
                )),
                media_export: Arc::clone(&backend) as Arc<dyn plaudon_app::ports::MediaExportPort>,
                deletion: Arc::clone(&backend) as Arc<dyn plaudon_app::ports::DeletionPort>,
                archive: Arc::clone(&backend) as Arc<dyn plaudon_app::ports::ArchivePort>,
                // Neither port is exercised by the cloud cases, but `AppPorts`
                // is a closed struct; these are the backends
                // `build_ports_with` wires in the shipped application.
                models: Arc::new(plaudon_app::backend::models::ModelsBackend::new(
                    library.path(),
                    events.clone(),
                )),
                update: Arc::new(plaudon_app::backend::update::UpdateBackend::new(
                    library.path(),
                    Arc::clone(&backend) as Arc<dyn plaudon_app::ports::SettingsPort>,
                )),
                secrets,
                events,
                // These cases need a live pool: since EPIC O3 a cloud dispatch
                // is performed by a worker claiming the `cloud_ready` row, not
                // by `start_processing` itself, which is why the covering-grant
                // case below polls for the transport call. One worker keeps
                // that polling deterministic.
                processing: plaudon_app::backend::processing::ProcessingHandle::start(
                    Arc::clone(&backend),
                    Some(1),
                ),
            };
            Self {
                backend,
                ports,
                transport,
                _library: library,
                sources,
            }
        }

        async fn import_recording(&self) -> RecordingId {
            let path = self.sources.path().join("meeting.wav");
            std::fs::write(&path, b"RIFF cloud payload").expect("write media");
            import::import_files(
                &self.ports,
                ImportFilesRequest {
                    paths: vec![path.to_string_lossy().into_owned()],
                    duplicate_policy: DuplicatePolicy::Ask,
                },
            )
            .await
            .expect("import");
            let page =
                library::list_recordings(&self.ports, super::Harness::list_request(10, None))
                    .await
                    .expect("list");
            page.items[0].recording_id
        }

        async fn verify_in_store(&self, model_id: &str) {
            let record = ModelProbeRecord {
                model_id: model_id.to_owned(),
                request_family: RequestFamily::MultipartStt,
                evidence: ProbeEvidence::ObservedExchange,
                accepted_formats: Known::Known(vec!["wav".to_owned()]),
                max_input_bytes: Known::Unknown,
                max_duration_ms: Known::Unknown,
                timestamps: TimestampSupport::ObservedNone,
                diarization: Tri::Unknown,
                language_detection: Tri::Unknown,
                provider_route: Known::Known("vendor".to_owned()),
                retention_control: Known::Unknown,
                audio_input_price: Known::Unknown,
                pricing_unit: Known::Unknown,
                observed_on: "2026-10-11".to_owned(),
                audio_tokens_per_second: plaudon_domain::capability::Known::Unknown,
                structured_output: Tri::Unknown,
                max_output_tokens: plaudon_domain::capability::Known::Unknown,
                notes: String::new(),
            };
            self.backend
                .cloud_ai()
                .store()
                .record_probe(
                    ModelTask::Transcription,
                    &record,
                    "fixture-1",
                    Timestamp::now(),
                )
                .await
                .expect("store the probe");
        }

        async fn set_default(&self, model_id: &str) {
            cloud::set_model_preference(&self.ports, default_preference(model_id))
                .await
                .expect("set the default");
        }

        async fn count(&self, table: &'static str) -> i64 {
            // Audited: `table` is a literal from this file.
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(self.backend.pool())
                .await
                .expect("count")
        }

        async fn assert_nothing_left(&self) {
            let cloud_jobs: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE job_type <> 'import'")
                    .fetch_one(self.backend.pool())
                    .await
                    .expect("count");
            assert_eq!(cloud_jobs, 0, "no cloud job row exists");
            assert_eq!(self.count("inference_attempts").await, 0, "no ledger row");
            assert_eq!(
                self.transport.calls.load(Ordering::SeqCst),
                0,
                "nothing was transmitted"
            );
        }

        async fn grant(&self, kind: &str, recording_id: Option<RecordingId>) -> String {
            cloud::grant_cloud_consent(
                &self.ports,
                GrantCloudConsentRequest {
                    purpose: "transcription".into(),
                    content_type: "audio_bytes".into(),
                    scope: ConsentScopeDto {
                        kind: kind.into(),
                        recording_id: recording_id.map(|id| id.to_string()),
                    },
                    model_id: MODEL.into(),
                    downstream_provider: None,
                    require_zero_retention: false,
                    disclosure_version: "2026-02-01".into(),
                },
            )
            .await
            .expect("grant")
            .consent_id
        }
    }

    fn default_preference(model_id: &str) -> SetModelPreferenceRequest {
        SetModelPreferenceRequest {
            stage: "transcribe".into(),
            scope: ModelPreferenceScope {
                kind: "default".into(),
                recording_id: None,
            },
            engine: "openrouter".into(),
            model_id: Some(model_id.into()),
        }
    }

    fn start(recording_id: RecordingId, acknowledged: bool) -> StartProcessingRequest {
        StartProcessingRequest {
            recording_id: recording_id.to_string(),
            stage: "transcribe".into(),
            model_override: None,
            cost_acknowledged: acknowledged,
            rerun: false,
        }
    }

    /// No response or envelope may carry the key, a URL or a path.
    fn assert_clean<T: serde::Serialize>(value: &T) {
        let json = serde_json::to_string(value).expect("serializes");
        for needle in [
            KEY, "sk-or", "Bearer", "http://", "https://", "/tmp/", "/var/",
        ] {
            assert!(!json.contains(needle), "leaked {needle}: {json}");
        }
    }

    /// `get_capabilities` reports the library location by design (ADR-0020), so
    /// it is only held to the secret rule.
    fn assert_no_key<T: serde::Serialize>(value: &T) {
        let json = serde_json::to_string(value).expect("serializes");
        for needle in [KEY, "sk-or", "Bearer"] {
            assert!(!json.contains(needle), "leaked {needle}");
        }
    }

    fn refused<T: serde::Serialize>(result: Result<T, AppError>) -> AppError {
        match result {
            Ok(value) => panic!(
                "the command must refuse, got {}",
                serde_json::to_string(&value).unwrap_or_default()
            ),
            Err(error) => {
                assert_clean(&envelope(&error));
                error
            }
        }
    }

    async fn list_models(h: &CloudHarness) -> ListModelsResponse {
        let response = cloud::list_models(
            &h.ports,
            ListModelsRequest {
                stage: "transcribe".into(),
                engine: None,
            },
        )
        .await
        .expect("list_models");
        assert_clean(&response);
        response
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_consent_nothing_is_created_or_sent_and_the_envelope_asks_for_review() {
        let h = CloudHarness::new(true).await;
        let recording = h.import_recording().await;
        h.verify_in_store(MODEL).await;
        h.set_default(MODEL).await;

        let error = refused(cloud::start_processing(&h.ports, start(recording, true)).await);
        assert_eq!(error.code(), AppErrorCode::ConsentRequired);
        let wire = to_command_result::<()>(Err(error)).unwrap_err();
        assert_eq!(wire.action, Some(ErrorAction::ReviewConsent));

        // "Ask every time" is recorded but never authorizes.
        h.grant("ask_every_time", None).await;
        let again = refused(cloud::start_processing(&h.ports, start(recording, true)).await);
        assert_eq!(again.code(), AppErrorCode::ConsentRequired);

        h.assert_nothing_left().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_budget_refusal_happens_before_any_job_or_ledger_row() {
        let h = CloudHarness::new(true).await;
        let recording = h.import_recording().await;
        h.verify_in_store(MODEL).await;
        h.set_default(MODEL).await;
        h.grant("model_policy", None).await;

        let budget = cloud::set_cloud_budget(
            &h.ports,
            SetCloudBudgetRequest {
                per_job_limit: Some("0.01".into()),
                monthly_limit: None,
                currency: "USD".into(),
            },
        )
        .await
        .expect("set the budget");
        assert!(budget.blocks_dispatch);
        assert!(budget.enforcement_note.contains("this device only"));
        assert_clean(&budget);

        // No audio price can be derived, so a configured limit needs an
        // explicit acknowledgement: refused with a covering grant in place.
        let error = refused(cloud::start_processing(&h.ports, start(recording, false)).await);
        assert_eq!(error.code(), AppErrorCode::CostAcknowledgementRequired);
        h.assert_nothing_left().await;

        // Hostile amounts never reach the store.
        for bad in ["NaN", "-1", "1e3", "1,5", "0.0000001", "", "inf", " 1"] {
            let error = refused(
                cloud::set_cloud_budget(
                    &h.ports,
                    SetCloudBudgetRequest {
                        per_job_limit: Some(bad.into()),
                        monthly_limit: None,
                        currency: "USD".into(),
                    },
                )
                .await,
            );
            assert_eq!(error.code(), AppErrorCode::InvalidArgument, "{bad}");
        }
        let error = refused(
            cloud::set_cloud_budget(
                &h.ports,
                SetCloudBudgetRequest {
                    per_job_limit: None,
                    monthly_limit: None,
                    currency: "EUR".into(),
                },
            )
            .await,
        );
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unverified_model_is_refused_without_a_job_a_ledger_row_or_a_send() {
        let h = CloudHarness::new(true).await;
        let recording = h.import_recording().await;
        h.grant("model_policy", None).await;
        h.set_default("vendor/never-probed").await;

        let error = refused(cloud::start_processing(&h.ports, start(recording, true)).await);
        assert_eq!(error.code(), AppErrorCode::CapabilityUnverified);

        let mut one_shot = start(recording, true);
        one_shot.model_override = Some(ModelOverride {
            engine: "openrouter".into(),
            model_id: "vendor/also-never-probed".into(),
        });
        let error = refused(cloud::start_processing(&h.ports, one_shot).await);
        assert_eq!(error.code(), AppErrorCode::CapabilityUnverified);

        // A capability check for a model with no known request contract sends
        // nothing either.
        let error = refused(
            cloud::verify_model(
                &h.ports,
                VerifyModelRequest {
                    model_id: "vendor/never-probed".into(),
                    task: "transcription".into(),
                    cost_acknowledged: true,
                },
            )
            .await,
        );
        assert_eq!(error.code(), AppErrorCode::CapabilityUnverified);
        h.assert_nothing_left().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_override_never_touches_the_default() {
        let h = CloudHarness::new(false).await;
        let recording = h.import_recording().await;
        h.verify_in_store("vendor/default-model").await;
        h.verify_in_store("vendor/override-model").await;
        h.set_default("vendor/default-model").await;

        let recording_scope = |model: Option<&str>| SetModelPreferenceRequest {
            stage: "transcribe".into(),
            scope: ModelPreferenceScope {
                kind: "recording".into(),
                recording_id: Some(recording.to_string()),
            },
            engine: "openrouter".into(),
            model_id: model.map(str::to_owned),
        };

        let written =
            cloud::set_model_preference(&h.ports, recording_scope(Some("vendor/override-model")))
                .await
                .expect("write the override");
        assert_eq!(written.scope_kind, "recording");
        assert_eq!(
            written
                .effective_model
                .as_ref()
                .map(|m| m.model_id.as_str()),
            Some("vendor/override-model")
        );
        assert!(written.effective_model.expect("card").selectable);
        assert_eq!(
            list_models(&h).await.default_model_id.as_deref(),
            Some("vendor/default-model"),
            "the default is unchanged"
        );

        let cleared = cloud::set_model_preference(&h.ports, recording_scope(None))
            .await
            .expect("clear the override");
        assert_eq!(cleared.model_id, None);
        assert_eq!(
            cleared.effective_model.map(|m| m.model_id).as_deref(),
            Some("vendor/default-model"),
            "clearing a recording override falls back to the default"
        );
        assert_eq!(
            list_models(&h).await.default_model_id.as_deref(),
            Some("vendor/default-model")
        );

        // A default can only be replaced, never cleared.
        let mut clear_default = default_preference("x");
        clear_default.model_id = None;
        let error = refused(cloud::set_model_preference(&h.ports, clear_default).await);
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);

        // A one-shot override of `start_processing` stores nothing.
        let mut one_shot = start(recording, true);
        one_shot.model_override = Some(ModelOverride {
            engine: "openrouter".into(),
            model_id: "vendor/override-model".into(),
        });
        let _ = cloud::start_processing(&h.ports, one_shot).await;
        let stored = h
            .backend
            .cloud_ai()
            .store()
            .recording_override(
                &recording.to_string(),
                plaudon_ai_openrouter::service::Stage::Transcribe,
            )
            .await
            .expect("read");
        assert_eq!(stored, None, "a one-shot override is never persisted");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_covering_grant_commits_the_ledger_before_the_send_and_an_unknown_outcome_is_never_retried_silently(
    ) {
        let h = CloudHarness::new(true).await;
        let recording = h.import_recording().await;
        h.verify_in_store(MODEL).await;
        h.set_default(MODEL).await;
        let consent = h.grant("model_policy", None).await;

        let started = cloud::start_processing(&h.ports, start(recording, false))
            .await
            .expect("a covered dispatch starts");
        assert_eq!(started.dispatch, "queued");
        assert_eq!(started.estimated_cost.confidence, CostConfidence::Unknown);
        assert!(started.estimated_cost.amount.is_none());
        assert_clean(&started);

        // The dispatch runs in the background; wait for it to settle.
        let mut summary = None;
        for _ in 0..200 {
            let page = jobs::list_jobs(
                &h.ports,
                ListJobsRequest {
                    cursor: None,
                    limit: 10,
                    states: vec![JobState::CloudOutcomeUnknown],
                    kinds: Vec::new(),
                },
            )
            .await
            .expect("list jobs");
            if let Some(job) = page.items.into_iter().next() {
                summary = Some(job);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if summary.is_none() {
            let states: Vec<(String, Option<String>)> = sqlx::query_as(
                "SELECT state, last_error_code FROM jobs WHERE job_type <> 'import'",
            )
            .fetch_all(h.backend.pool())
            .await
            .expect("states");
            panic!("the job did not park as possibly billed: {states:?}");
        }
        let job = summary.expect("checked above");
        assert_clean(&job);
        assert!(job.cost_acknowledgement_required);
        assert_eq!(job.cost.confidence, CostConfidence::Unknown);
        assert_eq!(
            job.error.as_ref().map(|e| e.code),
            Some(AppErrorCode::OutcomeUnknown)
        );

        assert_eq!(h.transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *h.transport.ledger_rows_at_send.lock().expect("lock"),
            vec![1],
            "the ledger row was committed before the request left"
        );
        let attempt_consent: Option<String> =
            sqlx::query_scalar("SELECT consent_id FROM inference_attempts")
                .fetch_one(h.backend.pool())
                .await
                .expect("the attempt");
        assert_eq!(attempt_consent.as_deref(), Some(consent.as_str()));

        // No silent retry of a possibly-billed request.
        let error = refused(
            jobs::retry_job(
                &h.ports,
                RetryJobRequest {
                    job_id: job.job_id.to_string(),
                    cost_acknowledged: false,
                },
            )
            .await,
        );
        assert_eq!(error.code(), AppErrorCode::CostAcknowledgementRequired);
        let error = refused(cloud::start_processing(&h.ports, start(recording, false)).await);
        assert_eq!(error.code(), AppErrorCode::CostAcknowledgementRequired);
        assert_eq!(h.transport.calls.load(Ordering::SeqCst), 1);

        // Revocation takes effect on the very next dispatch.
        cloud::revoke_cloud_consent(
            &h.ports,
            RevokeCloudConsentRequest {
                consent_id: consent.clone(),
            },
        )
        .await
        .expect("revoke");
        let error = refused(cloud::start_processing(&h.ports, start(recording, true)).await);
        assert_eq!(error.code(), AppErrorCode::ConsentRequired);
        assert_eq!(h.transport.calls.load(Ordering::SeqCst), 1);

        let capabilities = settings::get_capabilities(&h.ports).await.expect("caps");
        assert_no_key(&capabilities);
        assert!(capabilities.openrouter_key_present);
        let listed = capabilities
            .cloud_consents
            .iter()
            .find(|grant| grant.consent_id == consent)
            .expect("the grant stays listed");
        assert!(listed.revoked_at.is_some());
        assert_eq!(listed.scope_kind, "model_policy");

        let missing = refused(
            cloud::revoke_cloud_consent(
                &h.ports,
                RevokeCloudConsentRequest {
                    consent_id: "consent-unknown".into(),
                },
            )
            .await,
        );
        assert_eq!(missing.code(), AppErrorCode::RecordNotFound);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn usage_keeps_unknown_attempts_separate_from_zero() {
        let h = CloudHarness::new(false).await;
        let pool = h.backend.pool();
        let job = plaudon_domain::ids::JobId::new().to_string();
        sqlx::query("INSERT INTO jobs (id, job_type, state, created_at) VALUES (?1, 'transcribe', 'completed', ?2)")
            .bind(&job)
            .bind(Timestamp::now().as_millis())
            .execute(pool)
            .await
            .expect("job");
        let now = Timestamp::now().as_millis();
        let finalized = r#"{"confidence":"finalized","amount":"0.001000000000","currency":"USD","origin":"provider_reported"}"#;
        let rows: [(&str, &str, Option<&str>); 3] = [
            ("attempt-finalized", "completed", Some(finalized)),
            ("attempt-unknown", "outcome_unknown", None),
            ("attempt-failed", "failed_before_send", None),
        ];
        for (id, state, cost) in rows {
            sqlx::query(
                "INSERT INTO inference_attempts (id, job_id, provider, model_id, request_family, \
                 dispatch_state, final_cost_json, created_at) \
                 VALUES (?1, ?2, 'openrouter', ?3, 'multipart_stt', ?4, ?5, ?6)",
            )
            .bind(id)
            .bind(&job)
            .bind(MODEL)
            .bind(state)
            .bind(cost)
            .bind(now)
            .execute(pool)
            .await
            .expect("attempt");
        }

        let from = Timestamp::from_millis(now - 3_600_000)
            .to_rfc3339()
            .expect("from");
        let to = Timestamp::from_millis(now + 3_600_000)
            .to_rfc3339()
            .expect("to");
        let usage = cloud::get_usage(&h.ports, GetUsageRequest { from, to })
            .await
            .expect("usage");
        assert_clean(&usage);
        assert_eq!(
            usage.totals.finalized.as_ref().map(|m| m.amount.as_str()),
            Some("0.001000000000")
        );
        assert!(usage.totals.estimated.is_none());
        assert_eq!(
            usage.totals.unknown_count, 2,
            "unknown attempts are counted"
        );
        assert_eq!(usage.by_model.len(), 1);
        assert_eq!(usage.by_model[0].unknown_count, 2);
        let unknown = usage
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == "attempt-unknown")
            .expect("attempt");
        assert_eq!(unknown.cost.confidence, CostConfidence::Unknown);
        assert!(unknown.cost.amount.is_none(), "never a zero amount");
        assert!(unknown.possibly_billed);
        let failed = usage
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == "attempt-failed")
            .expect("attempt");
        assert!(!failed.possibly_billed);
        assert!(failed.cost.amount.is_none());

        // The job's own cost is derived from its attempts; a job without any
        // is unknown.
        let page = jobs::list_jobs(
            &h.ports,
            ListJobsRequest {
                cursor: None,
                limit: 10,
                states: Vec::new(),
                kinds: Vec::new(),
            },
        )
        .await
        .expect("jobs");
        let listed = &page.items[0];
        assert_eq!(listed.cost.confidence, CostConfidence::Estimated);

        // Invalid ranges.
        let day = 86_400_000;
        for (from, to) in [(now, now), (now, now - day), (now - 401 * day, now)] {
            let error = refused(
                cloud::get_usage(
                    &h.ports,
                    GetUsageRequest {
                        from: Timestamp::from_millis(from).to_rfc3339().expect("from"),
                        to: Timestamp::from_millis(to).to_rfc3339().expect("to"),
                    },
                )
                .await,
            );
            assert_eq!(error.code(), AppErrorCode::InvalidArgument);
        }
        let error = refused(
            cloud::get_usage(
                &h.ports,
                GetUsageRequest {
                    from: "not a date".into(),
                    to: "2026-01-01T00:00:00Z".into(),
                },
            )
            .await,
        );
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_catalogue_degrades_with_the_right_reason_and_never_leaks_the_key() {
        // No key: nothing is fetched.
        let h = CloudHarness::new(false).await;
        let response = list_models(&h).await;
        assert_eq!(response.degraded_reason.as_deref(), Some("no_key"));
        assert_eq!(response.catalog_source, "pinned_allowlist");
        assert!(!response.models.is_empty(), "degrades to the pinned list");
        assert!(response.models.iter().all(|model| !model.selectable));
        assert!(response
            .models
            .iter()
            .all(|model| model.pricing.amount_per_unit.is_none()));
        assert_eq!(h.transport.calls.load(Ordering::SeqCst), 0);

        // Local-only Mode: nothing is fetched even with a key.
        let h = CloudHarness::new(true).await;
        settings::set_local_only_mode(
            &h.ports,
            SetLocalOnlyModeRequest {
                enabled: true,
                confirmed: true,
            },
        )
        .await
        .expect("local-only");
        let response = list_models(&h).await;
        assert_eq!(response.degraded_reason.as_deref(), Some("local_only_mode"));
        assert_eq!(h.transport.calls.load(Ordering::SeqCst), 0);

        // Local-only Mode also refuses a cloud preference outright.
        let error = refused(cloud::set_model_preference(&h.ports, default_preference(MODEL)).await);
        assert_eq!(error.code(), AppErrorCode::LocalOnlyMode);

        // Key and network permitted: a fetch is attempted and fails offline.
        let h = CloudHarness::new(true).await;
        let response = list_models(&h).await;
        assert_eq!(h.transport.calls.load(Ordering::SeqCst), 1);
        assert!(response.degraded_reason.is_some());
        assert_eq!(response.catalog_source, "pinned_allowlist");

        // The key never appears in the capabilities snapshot either.
        let capabilities = settings::get_capabilities(&h.ports)
            .await
            .expect("capabilities");
        assert_no_key(&capabilities);
        let _ = EmptyRequest {};
    }
}
