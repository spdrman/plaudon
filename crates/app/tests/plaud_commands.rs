//! Command-level tests of the PLAUD cloud connector and source commands.
//!
//! Every failure path of spec §4.2 is driven through the same
//! `plaudon_app::commands::sources` functions the dispatcher calls, against a
//! scripted `plaud` executable and an in-process loopback origin for the
//! signed download URL. Each path asserts three things: the envelope carries
//! the stable §7.6 code, the diagnostics panel carries our own copy, and **no
//! vendor text appears anywhere**, which is proved by seeding every scripted
//! failure with a sentinel that must never come out the other side.
//!
//! The vendor tool is the real executable `mock_plaud`, built from this crate's
//! `src/bin/mock_plaud.rs` behind the `test-harness` feature (which is why
//! `plaudon-app` dev-depends on itself), copied into a temporary directory
//! under the pinned leaf name and configured through `plaud.mock.json` next to
//! it (the runner scrubs the environment, and parallel tests must not see each
//! other's setup). No network, no vendor account, no real CLI, and no shell, so
//! the suite runs on Windows too (spec §18.3, §18.6).
//!
//! The suite and the mock both lived in the Tauri shell crate until EPIC O task
//! O2.5 deleted it. Neither used Tauri: the connector, the secret store and the
//! source commands have always been application-layer code, so both moved here
//! with the crate name and the fixture root rebased and nothing else changed.
//! The listings and compatibility table still come from the workspace-level
//! `tests/integration/fixtures/plaud-cli`.

#![allow(clippy::too_many_lines, clippy::assert_is_empty)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use plaudon_app::backend::sources::{BuiltConnector, ConnectorFactory, PLAUD_ACCOUNT_SCOPE};
use plaudon_app::backend::LocalBackend;
use plaudon_app::commands::sources;
use plaudon_app::dto::{
    EventKind, ListSourceArtifactsRequest, PlaudCompatibility, PlaudConnectRequest,
    PlaudConnectorSnapshot, PlaudConnectorState, PlaudDisconnectVariant, PlaudImportOutcome,
    PlaudImportRequest, PlaudListRequest, PlaudListingCompleteness, PlaudNextAction,
    SourceAddFolderRequest, SourceReconcileRequest, SourceSyncRequest, SyncTrigger,
    SyncedSourceKind, WatchedFolderRule,
};
use plaudon_app::error::to_command_result;
use plaudon_app::events::{EventSink, RecordingEventSink};
use plaudon_app::ports::{
    ActivityPort, CapabilitiesPort, CloudPort, ExportPort, ImportPort, JobsPort, LibraryPort,
    SearchPort, SettingsPort, SourcesPort, SpeakersPort, TranscriptPort,
};
use plaudon_app::secrets::{InMemorySecretStore, SecretStore};
use plaudon_app::AppPorts;
use plaudon_domain::error::{AppError, AppErrorCode, ErrorAction, ErrorEnvelope};
use plaudon_domain::source::SourceAccountScope;
use plaudon_notifications::testing::RecordingNotifier;
use plaudon_notifications::{Dispatcher, NotificationSettings, Notifier};
use plaudon_security::outbound::subprocess::GatedProcessLauncher;
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
use plaudon_source_plaud_cli::acquire::AcquisitionPipeline;
use plaudon_source_plaud_cli::compatibility::CliCompatibilityTable;
use plaudon_source_plaud_cli::connector::PlaudCloudCliConnector;
use plaudon_source_plaud_cli::discovery::{CliDiscovery, ExecutableProbe, OsProbe};
use plaudon_source_plaud_cli::download::{DownloadPolicy, UrlContract};
use plaudon_source_plaud_cli::runner::CliRunner;
use plaudon_source_plaud_cli::transport::ReqwestFetcher;

/// Seeded into every scripted failure. If it ever reaches an envelope, a
/// diagnostics panel or an event, vendor text has leaked (spec §7.6).
const SENTINEL: &str = "VENDORTRACE-7c1e9a";
/// The token of the signed download URL (spec §7.3).
const URL_TOKEN: &str = "deadbeefcafe";

/* ------------------------------------------------------------------ fixtures */

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/integration/fixtures/plaud-cli")
}

fn fixture_table() -> Arc<CliCompatibilityTable> {
    let document = std::fs::read_to_string(fixture_root().join("cli-compatibility.fixture.yaml"))
        .expect("read the fixture table");
    Arc::new(CliCompatibilityTable::from_yaml(&document).expect("the fixture table parses"))
}

fn fixture_listing(id: &str) -> String {
    std::fs::read_to_string(fixture_root().join(id).join("files.stdout")).expect("listing fixture")
}

/// Only paths under `root` exist, so discovery can never find a real `plaud`.
#[derive(Debug)]
struct ScopedProbe {
    root: PathBuf,
}

impl ExecutableProbe for ScopedProbe {
    fn is_executable_file(&self, path: &Path) -> bool {
        path.starts_with(&self.root) && OsProbe.is_executable_file(path)
    }
    fn exists(&self, path: &Path) -> bool {
        path.starts_with(&self.root) && OsProbe.exists(path)
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        OsProbe.canonicalize(path)
    }
}

#[derive(Debug)]
struct MockFactory {
    dir: PathBuf,
}

impl ConnectorFactory for MockFactory {
    fn build(&self, local_only: bool, now_ms: i64) -> Result<BuiltConnector, AppError> {
        let policy = if local_only {
            OutboundPolicy::local_only()
        } else {
            OutboundPolicy::network_permitted(NetworkGrant::new(
                ApprovalSource::TestHarness,
                0,
                "privacy-v1",
            ))
        };
        let runner = CliRunner::new(
            GatedProcessLauncher::new(Arc::new(policy)),
            Arc::new(plaudon_diagnostics::redact::Redactor::new()),
        );
        let download = Arc::new(DownloadPolicy::loopback_harness(&["127.0.0.1"]));
        let fetcher = ReqwestFetcher::new(Arc::clone(&download)).expect("fetcher");
        let pipeline = AcquisitionPipeline::new(
            download,
            Arc::new(fetcher),
            UrlContract::undocumented("127.0.0.1"),
        );
        let discovery = CliDiscovery::with_probe(
            Arc::new(ScopedProbe {
                root: self.dir.clone(),
            }),
            BTreeMap::from([("PATH".to_owned(), self.dir.display().to_string())]),
        );
        let table = fixture_table();
        let scope = SourceAccountScope::new(PLAUD_ACCOUNT_SCOPE);
        let connector =
            PlaudCloudCliConnector::new(discovery, runner, pipeline, scope.clone(), now_ms)
                .with_table(Arc::clone(&table));
        Ok(BuiltConnector {
            connector,
            table,
            account_scope: scope,
        })
    }
}

/// Serve raw HTTP responses, one per connection.
fn serve(responses: Vec<Vec<u8>>) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for (index, mut stream) in listener.incoming().flatten().enumerate() {
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let response = responses
                .get(index)
                .or_else(|| responses.last())
                .cloned()
                .unwrap_or_default();
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });
    port
}

fn http(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn wav(payload: usize) -> Vec<u8> {
    let data_len = u32::try_from(payload).expect("payload fits");
    let mut bytes = Vec::with_capacity(44 + payload);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&32_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + payload, 0x11);
    bytes
}

/* ------------------------------------------------------------------- harness */

struct Harness {
    ports: AppPorts,
    sink: Arc<RecordingEventSink>,
    cli: tempfile::TempDir,
    library: tempfile::TempDir,
    scratch: tempfile::TempDir,
    script: std::sync::Mutex<serde_json::Value>,
}

impl Harness {
    /// A library with a scripted CLI. `version` is the stdout of the `version`
    /// subcommand, or `None` for an installation with no `plaud` at all.
    async fn new(version: Option<&str>) -> Self {
        let cli = tempfile::tempdir().expect("tempdir");
        let library = tempfile::tempdir().expect("tempdir");
        let scratch = tempfile::tempdir().expect("tempdir");
        let mut script = serde_json::json!({ "record_invocations": true, "unknown_exit_code": 64 });
        if let Some(version) = version {
            let leaf = if cfg!(windows) { "plaud.exe" } else { "plaud" };
            let program = cli.path().join(leaf);
            std::fs::copy(env!("CARGO_BIN_EXE_mock_plaud"), &program).expect("copy the mock");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            script["version"] = serde_json::json!({ "stdout": version });
        }

        let sink = Arc::new(RecordingEventSink::new());
        let notifier = RecordingNotifier::granted();
        let dispatcher = Arc::new(
            Dispatcher::new(
                Arc::clone(&notifier) as Arc<dyn Notifier>,
                NotificationSettings::uniform(false),
            )
            .with_timeout(Duration::from_millis(100)),
        );
        let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let backend = Arc::new(
            LocalBackend::open(
                library.path(),
                Arc::clone(&sink) as Arc<dyn EventSink>,
                dispatcher,
                Arc::clone(&secrets),
                vec![scratch.path().to_path_buf()],
            )
            .await
            .expect("the library opens")
            .with_connector_factory(Arc::new(MockFactory {
                dir: cli.path().to_path_buf(),
            })),
        );
        let ports = AppPorts {
            capabilities: Arc::clone(&backend) as Arc<dyn CapabilitiesPort>,
            library: Arc::clone(&backend) as Arc<dyn LibraryPort>,
            transcript: Arc::clone(&backend) as Arc<dyn TranscriptPort>,
            jobs: Arc::clone(&backend) as Arc<dyn JobsPort>,
            activity: Arc::clone(&backend) as Arc<dyn ActivityPort>,
            import: Arc::clone(&backend) as Arc<dyn ImportPort>,
            settings: Arc::clone(&backend) as Arc<dyn SettingsPort>,
            export: Arc::clone(&backend) as Arc<dyn ExportPort>,
            sources: Arc::clone(&backend) as Arc<dyn SourcesPort>,
            search: Arc::clone(&backend) as Arc<dyn SearchPort>,
            speakers: Arc::clone(&backend) as Arc<dyn SpeakersPort>,
            cloud: Arc::clone(&backend) as Arc<dyn CloudPort>,
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
            // Locally installed models and the update facts are read-only here;
            // the suite never asks for either, but `AppPorts` is a closed
            // struct and these are the same backends `build_ports_with` wires.
            models: Arc::new(plaudon_app::backend::models::ModelsBackend::new(
                library.path(),
                backend.events().clone(),
            )),
            update: Arc::new(plaudon_app::backend::update::UpdateBackend::new(
                library.path(),
                Arc::clone(&backend) as Arc<dyn SettingsPort>,
            )),
            secrets,
            events: backend.events().clone(),
            // One worker, because nothing here needs two. The PLAUD jobs this
            // suite creates are import jobs, which `StagePlan::of` declines, so
            // the worker finds nothing claimable and only the commands under
            // test write to the job table.
            processing: plaudon_app::backend::processing::ProcessingHandle::start(
                Arc::clone(&backend),
                Some(1),
            ),
        };
        let harness = Self {
            ports,
            sink,
            cli,
            library,
            scratch,
            script: std::sync::Mutex::new(script),
        };
        harness.write_script();
        harness
    }

    fn write_script(&self) {
        let script = self.script.lock().expect("script lock");
        std::fs::write(
            self.cli.path().join("plaud.mock.json"),
            serde_json::to_vec_pretty(&*script).expect("script json"),
        )
        .expect("write the script");
    }

    /// Script one subcommand's output.
    fn script(&self, name: &str, stdout: &str, stderr: &str, code: i32) {
        self.script.lock().expect("script lock")[name] =
            serde_json::json!({ "stdout": stdout, "stderr": stderr, "exit_code": code });
        self.write_script();
    }

    /// The common good path: a tested release, a working login, a listing.
    fn script_listing(&self, port: Option<u16>) {
        self.script("login", "", "", 0);
        self.script("files", &fixture_listing("v1.4.2-jsonl"), "", 0);
        if let Some(port) = port {
            self.script(
                "audio",
                &format!("http://127.0.0.1:{port}/audio.bin?sig={URL_TOKEN}\n"),
                "",
                0,
            );
        }
    }

    /// Every argument vector the mock was launched with, joined by spaces.
    fn invocations(&self) -> Vec<String> {
        std::fs::read_to_string(self.cli.path().join("plaud.invocations.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Vec<String>>(line)
                    .expect("an invocation line")
                    .join(" ")
            })
            .collect()
    }

    async fn connect(&self) -> Result<PlaudConnectorSnapshot, ErrorEnvelope> {
        to_command_result(
            sources::plaud_connect(
                &self.ports,
                PlaudConnectRequest {
                    user_intent: "connect_plaud_account".to_owned(),
                },
            )
            .await,
        )
    }

    async fn list(&self) -> Result<plaudon_app::dto::PlaudListResponse, ErrorEnvelope> {
        to_command_result(sources::plaud_list(&self.ports, PlaudListRequest { cursor: None }).await)
    }

    async fn import(
        &self,
        ids: &[&str],
    ) -> Result<plaudon_app::dto::PlaudImportResponse, ErrorEnvelope> {
        to_command_result(
            sources::plaud_import(
                &self.ports,
                PlaudImportRequest {
                    external_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                },
            )
            .await,
        )
    }

    async fn probe(&self) -> PlaudConnectorSnapshot {
        sources::plaud_probe(&self.ports).await.expect("probe")
    }

    fn event_kinds(&self) -> Vec<EventKind> {
        self.sink.kinds()
    }

    /// Everything a user or the UI could see after a run: every event and the
    /// serialized snapshot. None of it may contain vendor text.
    fn assert_no_vendor_text_anywhere(&self, snapshot: &PlaudConnectorSnapshot) {
        let events = serde_json::to_string(&self.sink.events()).expect("events");
        assert!(!events.contains(SENTINEL), "an event leaked vendor text");
        assert!(!events.contains(URL_TOKEN), "an event leaked a signed URL");
        assert_clean(&serde_json::to_string(snapshot).expect("snapshot"));
    }
}

fn assert_clean(rendered: &str) {
    for needle in [SENTINEL, URL_TOKEN, "Bearer", "127.0.0.1", "token="] {
        assert!(
            !rendered.contains(needle),
            "vendor text {needle:?} reached the UI-visible payload: {rendered}"
        );
    }
}

fn assert_envelope(envelope: &ErrorEnvelope, code: AppErrorCode, action: Option<ErrorAction>) {
    assert_eq!(envelope.code, code);
    assert_eq!(envelope.message, code.canonical_message());
    assert_eq!(envelope.retryable, code.is_retryable());
    assert_eq!(envelope.action, action);
    assert_clean(&serde_json::to_string(envelope).expect("envelope"));
}

/// Connect, list and return the first listed ID.
async fn connected_with_listing(harness: &Harness) -> String {
    harness.connect().await.expect("connected");
    let listing = harness.list().await.expect("listed");
    listing.items[0].external_id.clone()
}

const TESTED: &str = "plaud/1.4.2\n";

/* ------------------------------------------------- §4.2 failure paths (1-5) */

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_missing_is_a_typed_envelope_and_the_panel_offers_the_install_link() {
    let harness = Harness::new(None).await;

    let probe = harness.probe().await;
    assert_eq!(probe.status.state, PlaudConnectorState::Unconfigured);
    assert_eq!(probe.status.last_error, Some(AppErrorCode::CliMissing));
    assert_eq!(probe.diagnostics.next_action, PlaudNextAction::InstallCli);
    assert_eq!(probe.diagnostics.compatibility, PlaudCompatibility::Unknown);
    assert!(!probe.diagnostics.headline.is_empty());

    let envelope = harness.connect().await.expect_err("connect fails");
    assert_envelope(
        &envelope,
        AppErrorCode::CliMissing,
        Some(ErrorAction::OpenCliSetup),
    );
    assert!(harness.invocations().is_empty(), "nothing was launched");
    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untested_version_blocks_listing_and_keeps_manual_import() {
    let harness = Harness::new(Some("plaud/9.9.9\n")).await;
    harness.script("files", &format!("{SENTINEL}\n"), "", 0);

    let probe = harness.probe().await;
    assert_eq!(probe.status.state, PlaudConnectorState::IncompatibleVersion);
    assert_eq!(probe.status.detected_version.as_deref(), Some("9.9.9"));
    assert!(!probe.status.version_validated);
    assert_eq!(
        probe.diagnostics.compatibility,
        PlaudCompatibility::Untested
    );
    assert!(probe.diagnostics.detail.contains("9.9.9"));
    assert!(probe.diagnostics.detail.contains("by hand"));
    assert!(matches!(
        probe.diagnostics.next_action,
        PlaudNextAction::InstallTestedVersion { .. }
    ));

    let envelope = harness.connect().await.expect_err("connect fails");
    assert_envelope(
        &envelope,
        AppErrorCode::CliUnsupported,
        Some(ErrorAction::OpenCliSetup),
    );
    let envelope = harness.list().await.expect_err("list fails");
    assert_eq!(envelope.code, AppErrorCode::CliUnsupported);

    // Enumeration was refused without ever asking the tool for a listing.
    assert!(harness
        .invocations()
        .iter()
        .all(|line| line.starts_with("version")));
    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_login_asks_for_sign_in_and_announces_it() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script("login", "", "", 0);
    harness.script(
        "files",
        "",
        &format!("Error: session expired, please re-authenticate {SENTINEL} token=Bearer abc\n"),
        1,
    );
    harness.connect().await.expect("connected");

    let envelope = harness.list().await.expect_err("list fails");
    assert_envelope(
        &envelope,
        AppErrorCode::AuthenticationRequired,
        Some(ErrorAction::OpenPlaudConnect),
    );

    let probe = harness.probe().await;
    assert_eq!(probe.status.state, PlaudConnectorState::ExpiredAuth);
    assert_eq!(probe.diagnostics.next_action, PlaudNextAction::SignIn);
    assert!(harness
        .event_kinds()
        .contains(&EventKind::SourceAuthRequired));
    harness.assert_no_vendor_text_anywhere(&probe);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_browser_login_is_not_a_failure_of_the_tool() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script(
        "login",
        "",
        &format!("login cancelled by user {SENTINEL}\n"),
        130,
    );

    let envelope = harness.connect().await.expect_err("sign-in is cancelled");
    assert_envelope(
        &envelope,
        AppErrorCode::AuthCanceled,
        Some(ErrorAction::OpenPlaudConnect),
    );
    let probe = harness.probe().await;
    assert_eq!(probe.status.state, PlaudConnectorState::AuthRequired);
    assert_eq!(probe.diagnostics.next_action, PlaudNextAction::SignIn);
    assert!(harness
        .event_kinds()
        .contains(&EventKind::SourceAuthRequired));
    harness.assert_no_vendor_text_anywhere(&probe);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_tool_output_is_refused_rather_than_shown() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script("login", "", "", 0);
    harness.script(
        "files",
        &format!("{{\"id\":\"rec_1\",\"title\":\"{SENTINEL}\"}}\n{{\"id\":\"rec_1\"}}\n"),
        "",
        0,
    );
    harness.connect().await.expect("connected");

    let envelope = harness.list().await.expect_err("list fails");
    assert_envelope(
        &envelope,
        AppErrorCode::MalformedOutput,
        Some(ErrorAction::OpenDiagnostics),
    );
    let probe = harness.probe().await;
    assert_eq!(probe.status.state, PlaudConnectorState::Error);
    assert_eq!(
        probe.diagnostics.next_action,
        PlaudNextAction::ImportManually
    );
    harness.assert_no_vendor_text_anywhere(&probe);
}

/* ------------------------------------------------- §4.2 failure paths (6-9) */

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_loss_while_listing_is_retryable_and_names_nothing_vendor() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script("login", "", "", 0);
    harness.script(
        "files",
        "",
        &format!("network is unreachable {SENTINEL} https://api.example/x?sig={URL_TOKEN}\n"),
        1,
    );
    harness.connect().await.expect("connected");

    let envelope = harness.list().await.expect_err("list fails");
    assert_envelope(
        &envelope,
        AppErrorCode::SourceUnavailable,
        Some(ErrorAction::WaitAndRetry),
    );
    assert!(envelope.retryable);
    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

/// Run one failing import and return its outcome envelope plus the job id.
async fn failing_import(harness: &Harness) -> (ErrorEnvelope, plaudon_domain::ids::JobId) {
    let id = connected_with_listing(harness).await;
    let response = harness.import(&[&id]).await.expect("the request is valid");
    assert_eq!(response.results.len(), 1);
    let PlaudImportOutcome::Failed { error, job_id, .. } =
        response.results.into_iter().next().expect("one result")
    else {
        panic!("the import must fail");
    };
    // Nothing was committed and staging is clean.
    let recordings = std::fs::read_dir(harness.library.path().join("staging").join("plaud"))
        .map_or(0, Iterator::count);
    assert_eq!(recordings, 0, "staging must be left clean");
    (error, job_id)
}

async fn assert_job_failed(
    harness: &Harness,
    job_id: plaudon_domain::ids::JobId,
    code: AppErrorCode,
) {
    let page = harness
        .ports
        .jobs
        .list_jobs(plaudon_app::validate::ValidatedListJobs {
            cursor: plaudon_app::validate::PageCursor::START,
            limit: 50,
            states: Vec::new(),
            kinds: Vec::new(),
        })
        .await
        .expect("jobs");
    let job = page
        .items
        .iter()
        .find(|job| job.job_id == job_id)
        .expect("the failed job is recorded");
    assert!(matches!(
        job.state,
        plaudon_app::dto::JobState::FailedRetryable | plaudon_app::dto::JobState::FailedTerminal
    ));
    assert_eq!(
        job.error.as_ref().map(|error| error.code),
        Some(code),
        "the job holds the same stable code"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_audio_is_asset_not_found_and_leaves_the_library_untouched() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(None);
    harness.script(
        "audio",
        "",
        &format!("Error: no such recording {SENTINEL}\n"),
        1,
    );

    let (error, job_id) = failing_import(&harness).await;
    assert_envelope(&error, AppErrorCode::AssetNotFound, None);
    assert_job_failed(&harness, job_id, AppErrorCode::AssetNotFound).await;
    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_link_is_retried_once_then_reported_as_link_expired() {
    let port = serve(vec![
        http("403 Forbidden", "application/xml", b"<Error/>"),
        http("403 Forbidden", "application/xml", b"<Error/>"),
    ]);
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(Some(port));

    let (error, job_id) = failing_import(&harness).await;
    assert_envelope(
        &error,
        AppErrorCode::LinkExpired,
        Some(ErrorAction::RefreshSourceLink),
    );
    assert_job_failed(&harness, job_id, AppErrorCode::LinkExpired).await;
    let audio_calls = harness
        .invocations()
        .iter()
        .filter(|line| line.starts_with("audio"))
        .count();
    assert_eq!(audio_calls, 2, "exactly one fresh-URL retry");
    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_error_page_served_as_audio_is_an_integrity_failure() {
    let port = serve(vec![http(
        "200 OK",
        "audio/wav",
        format!("<html>{SENTINEL}</html>").as_bytes(),
    )]);
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(Some(port));

    let (error, job_id) = failing_import(&harness).await;
    assert_envelope(
        &error,
        AppErrorCode::IntegrityFailed,
        Some(ErrorAction::OpenDiagnostics),
    );
    assert_job_failed(&harness, job_id, AppErrorCode::IntegrityFailed).await;
    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_body_is_an_interrupted_download() {
    let audio = wav(2048);
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        audio.len() + 512
    )
    .into_bytes();
    response.extend_from_slice(&audio);
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(Some(serve(vec![response])));

    let (error, job_id) = failing_import(&harness).await;
    assert_envelope(
        &error,
        AppErrorCode::DownloadInterrupted,
        Some(ErrorAction::RetryImport),
    );
    assert!(error.retryable);
    assert_job_failed(&harness, job_id, AppErrorCode::DownloadInterrupted).await;
    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

/* ------------------------------------------------------------ good paths */

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listed_recording_is_downloaded_verified_and_committed_once() {
    let audio = wav(4096);
    let port = serve(vec![http("200 OK", "audio/wav", &audio)]);
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(Some(port));

    let snapshot = harness.connect().await.expect("connected");
    assert_eq!(snapshot.status.state, PlaudConnectorState::Authenticated);
    assert!(snapshot.status.version_validated);

    let listing = harness.list().await.expect("listed");
    assert_eq!(listing.items.len(), 3);
    assert_eq!(
        listing.completeness,
        PlaudListingCompleteness::PossiblyIncomplete
    );
    assert!(listing.empty_account_advisory.is_none());
    assert!(listing
        .items
        .iter()
        .all(|item| item.imported_recording_id.is_none()));
    let id = listing.items[0].external_id.clone();

    let response = harness.import(&[&id]).await.expect("import request");
    let PlaudImportOutcome::Imported {
        recording_id,
        job_id,
        ..
    } = &response.results[0]
    else {
        panic!("expected an imported recording: {:?}", response.results[0]);
    };

    let detail = harness
        .ports
        .library
        .get_recording(*recording_id)
        .await
        .expect("the recording is in the library");
    assert_eq!(detail.summary.title, "Board sync");
    let kinds = harness.event_kinds();
    for expected in [
        EventKind::SourceSyncFinished,
        EventKind::RecordingImported,
        EventKind::LibraryChanged,
        EventKind::JobUpdated,
    ] {
        assert!(kinds.contains(&expected), "missing {expected:?}: {kinds:?}");
    }
    let _ = job_id;

    // The same item again is recognised, with no second download.
    let again = harness.import(&[&id]).await.expect("second import");
    assert!(matches!(
        again.results[0],
        PlaudImportOutcome::AlreadyImported { .. }
    ));
    assert_eq!(
        harness
            .invocations()
            .iter()
            .filter(|line| line.starts_with("audio"))
            .count(),
        1
    );

    // The next listing says which recordings are already in the library.
    let relisted = harness.list().await.expect("relisted");
    assert_eq!(relisted.items[0].imported_recording_id, Some(*recording_id));

    // No vendor artifacts exist for it; an unknown recording is an error.
    let artifacts = sources::list_source_artifacts(
        &harness.ports,
        ListSourceArtifactsRequest {
            recording_id: recording_id.to_string(),
        },
    )
    .await
    .expect("artifacts");
    assert!(artifacts.artifacts.is_empty());
    let missing = to_command_result(
        sources::list_source_artifacts(
            &harness.ports,
            ListSourceArtifactsRequest {
                recording_id: plaudon_domain::ids::RecordingId::new().to_string(),
            },
        )
        .await,
    )
    .expect_err("unknown recording");
    assert_eq!(missing.code, AppErrorCode::RecordNotFound);

    harness.assert_no_vendor_text_anywhere(&harness.probe().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_id_that_was_not_in_the_last_listing_is_rejected_without_a_launch() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(Some(1));
    let real = connected_with_listing(&harness).await;

    let envelope = harness
        .import(&[&real, "rec_forged"])
        .await
        .expect_err("a forged ID rejects the whole request");
    assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
    assert!(harness
        .invocations()
        .iter()
        .all(|line| !line.starts_with("audio")));

    // Shape validation happens before the backend is asked anything.
    for bad in [vec![], vec!["rec\u{0}x"], vec!["a", "a"]] {
        let envelope = harness
            .import(&bad)
            .await
            .expect_err("a malformed request is rejected");
        assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
    }
    let envelope = to_command_result(
        sources::plaud_connect(
            &harness.ports,
            PlaudConnectRequest {
                user_intent: "run_anything".to_owned(),
            },
        )
        .await,
    )
    .expect_err("an unknown user intent is rejected");
    assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_account_is_an_advisory_not_an_error() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script("login", "", "", 0);
    harness.script("files", "\n", "", 0);
    harness.connect().await.expect("connected");

    let listing = harness
        .list()
        .await
        .expect("an empty listing is not an error");
    assert!(listing.items.is_empty());
    let advisory = listing.empty_account_advisory.expect("advisory");
    assert!(advisory.contains("regional"));
    assert!(advisory
        .to_ascii_lowercase()
        .contains("never ask you to paste"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_only_mode_is_enforced_by_the_policy_and_never_launches_the_cli() {
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(None);
    harness
        .ports
        .settings
        .set_local_only_mode(true)
        .await
        .expect("local-only on");

    let envelope = harness.connect().await.expect_err("connect is denied");
    assert_envelope(
        &envelope,
        AppErrorCode::SourceUnavailable,
        Some(ErrorAction::WaitAndRetry),
    );
    let probe = harness.probe().await;
    assert_eq!(
        probe.diagnostics.next_action,
        PlaudNextAction::ReviewLocalOnlyMode
    );
    assert!(probe.diagnostics.detail.contains("Local-only Mode"));
    assert!(
        harness.invocations().is_empty(),
        "spec §22(15): Local-only Mode never launches the PLAUD Cloud CLI"
    );

    // Turning the mode off lets the same connector work again.
    harness
        .ports
        .settings
        .set_local_only_mode(false)
        .await
        .expect("local-only off");
    harness.connect().await.expect("connected once allowed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_wording_is_truthful_about_session_revocation() {
    // A release with no tested logout command: association only.
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(None);
    harness.connect().await.expect("connected");
    let snapshot = harness.probe().await;
    assert_eq!(
        snapshot.disconnect.variant,
        PlaudDisconnectVariant::AssociationOnly
    );
    let response = sources::plaud_disconnect(&harness.ports)
        .await
        .expect("disconnected");
    assert_eq!(
        response.disconnect.variant,
        PlaudDisconnectVariant::AssociationOnly
    );
    assert!(response.disconnect.body.contains("does not sign you out"));
    assert!(harness
        .invocations()
        .iter()
        .all(|line| !line.starts_with("logout")));
    assert_eq!(
        response.snapshot.status.state,
        PlaudConnectorState::Unconfigured
    );
    // A later probe must not silently restore the association.
    assert_eq!(
        harness.probe().await.status.state,
        PlaudConnectorState::Unconfigured
    );

    // A release that records a logout command: the wording may say so.
    let harness = Harness::new(Some("plaud/1.5.0\n")).await;
    harness.script("login", "", "", 0);
    harness.script("logout", "signed out\n", "", 0);
    harness.connect().await.expect("connected");
    let response = sources::plaud_disconnect(&harness.ports)
        .await
        .expect("disconnected");
    assert_eq!(
        response.disconnect.variant,
        PlaudDisconnectVariant::WithVendorLogout
    );
    assert!(harness
        .invocations()
        .iter()
        .any(|line| line.starts_with("logout")));
}

/* --------------------------------------------------- sources and folders */

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paged_complete_listing_is_reconciled_and_a_partial_one_is_not() {
    // A partial listing (paging never observed) records nothing.
    let harness = Harness::new(Some(TESTED)).await;
    harness.script_listing(None);
    harness.connect().await.expect("connected");
    let listing = harness.list().await.expect("listed");
    let source_id = listing.source_id.clone();
    let reconciled = sources::source_reconcile(
        &harness.ports,
        SourceReconcileRequest {
            source_id: source_id.clone(),
        },
    )
    .await
    .expect("reconciled");
    assert_eq!(reconciled.source_kind, SyncedSourceKind::PlaudCloud);
    assert_eq!(reconciled.listing_complete, Some(false));
    assert!(reconciled.decisions.is_empty());

    // The cloud never syncs unattended.
    let envelope = to_command_result(
        sources::source_sync(
            &harness.ports,
            SourceSyncRequest {
                source_id: source_id.clone(),
                trigger: SyncTrigger::Background,
            },
        )
        .await,
    )
    .expect_err("background sync of the cloud is refused");
    assert_eq!(envelope.code, AppErrorCode::InvalidArgument);

    let synced = sources::source_sync(
        &harness.ports,
        SourceSyncRequest {
            source_id,
            trigger: SyncTrigger::Manual,
        },
    )
    .await
    .expect("synced");
    assert_eq!(synced.listed, Some(3));
    assert_eq!(synced.state, plaudon_app::dto::JobState::Completed);
    assert!(harness
        .event_kinds()
        .contains(&EventKind::SourceSyncFinished));

    // A release whose paging was observed yields a complete listing.
    let harness = Harness::new(Some("plaud/1.5.0\n")).await;
    harness.script("login", "", "", 0);
    harness.script("files", &fixture_listing("v1.5.0-jsonarray"), "", 0);
    harness.script(
        "files_page_2",
        "{\"files\":[{\"id\":\"rec_01HFD000\"}],\"next_cursor\":null}",
        "",
        0,
    );
    harness.connect().await.expect("connected");
    let listing = harness.list().await.expect("page one");
    assert_eq!(listing.completeness, PlaudListingCompleteness::Complete);
    let reconciled = sources::source_reconcile(
        &harness.ports,
        SourceReconcileRequest {
            source_id: listing.source_id,
        },
    )
    .await
    .expect("reconciled");
    assert_eq!(reconciled.listing_complete, Some(true));
    assert_eq!(reconciled.decisions.len(), 3, "both pages were followed");
    assert!(reconciled
        .decisions
        .iter()
        .all(|decision| decision.event == "new_external_id"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watched_folders_are_added_synced_and_reconciled_by_id() {
    let harness = Harness::new(None).await;
    let folder = harness.scratch.path().join("recordings");
    std::fs::create_dir(&folder).expect("folder");

    let added = sources::source_add_folder(
        &harness.ports,
        SourceAddFolderRequest {
            path: folder.display().to_string(),
            recursive: true,
            rule: WatchedFolderRule::ImportOnly,
        },
    )
    .await
    .expect("folder added");
    assert!(added.recursive);

    let synced = sources::source_sync(
        &harness.ports,
        SourceSyncRequest {
            source_id: added.source_id.clone(),
            trigger: SyncTrigger::Background,
        },
    )
    .await
    .expect("a folder may sync in the background");
    assert_eq!(synced.source_kind, SyncedSourceKind::WatchedFolder);
    let counts = synced.folder.expect("folder counts");
    assert!(counts.inventory_complete);
    assert_eq!(counts.imported, 0);

    let reconciled = sources::source_reconcile(
        &harness.ports,
        SourceReconcileRequest {
            source_id: added.source_id.clone(),
        },
    )
    .await
    .expect("reconciled");
    assert_eq!(reconciled.source_kind, SyncedSourceKind::WatchedFolder);
    assert!(reconciled.folder.is_some());
    assert!(harness
        .event_kinds()
        .contains(&EventKind::SourceSyncFinished));

    // Re-adding the same folder is idempotent and restates its policy.
    let again = sources::source_add_folder(
        &harness.ports,
        SourceAddFolderRequest {
            path: folder.display().to_string(),
            recursive: false,
            rule: WatchedFolderRule::AskBeforeCloud,
        },
    )
    .await
    .expect("re-added");
    assert_eq!(again.source_id, added.source_id);
    assert_eq!(again.rule, WatchedFolderRule::AskBeforeCloud);

    // A folder outside every approved root is refused.
    let outside = tempfile::tempdir().expect("tempdir");
    let envelope = to_command_result(
        sources::source_add_folder(
            &harness.ports,
            SourceAddFolderRequest {
                path: outside.path().display().to_string(),
                recursive: false,
                rule: WatchedFolderRule::ImportOnly,
            },
        )
        .await,
    )
    .expect_err("outside the approved roots");
    assert_eq!(envelope.code, AppErrorCode::ArchivePathRejected);

    // An unknown source is an error, not an empty success.
    let envelope = to_command_result(
        sources::source_sync(
            &harness.ports,
            SourceSyncRequest {
                source_id: uuid::Uuid::now_v7().to_string(),
                trigger: SyncTrigger::Manual,
            },
        )
        .await,
    )
    .expect_err("unknown source");
    assert_eq!(envelope.code, AppErrorCode::RecordNotFound);
    let envelope = to_command_result(
        sources::source_sync(
            &harness.ports,
            SourceSyncRequest {
                source_id: "not-a-uuid".to_owned(),
                trigger: SyncTrigger::Manual,
            },
        )
        .await,
    )
    .expect_err("malformed id");
    assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
}
