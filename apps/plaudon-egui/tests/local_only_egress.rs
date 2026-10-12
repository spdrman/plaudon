//! Local-only Mode and the network (§4.3, §15.4, §16.2A).
//!
//! The question this file answers is narrow and it is the whole point of the
//! mode: **with Local-only Mode on, does the running application ever
//! authorize an outbound request?** It is answered by owning the one seam
//! every cloud byte must pass through —
//! [`plaudon_ai_openrouter::transport::AsyncHttpTransport`], installed with
//! [`EngineConfig::with_cloud_transport`] — and by driving the real engine
//! behind the real shell across import → transcribe → summarize.
//!
//! Three rules shape what is written here.
//!
//! * **The watch records only what §15.4 permits.** An attempt is reduced to
//!   the method, the egress gate's own redacted destination label and the
//!   URL path with the query string dropped. No header value and no body is
//!   read, not even to count bytes: the thing being protected is exactly what
//!   a careless log line would leak.
//! * **A watch that cannot fail proves nothing.** The negative control
//!   ([`the_watch_catches_the_catalogue_refresh_once_the_key_is_stored`])
//!   drives the one flow in this build that genuinely reaches the transport
//!   and asserts the watch sees it. Without that test, the zero in the first
//!   test could just as easily mean the transport was never installed.
//! * **Nothing is asserted that the application does not do.** The local
//!   stages here run for real — the job engine claims them, the worker runs
//!   the executor and the output is committed — against scripted engines
//!   installed at the `plaudon_ai_contracts` boundary, so the zero at the end
//!   is a zero for work that actually happened. There is still no verified
//!   cloud model (a pinned-allowlist card is unverified until a capability
//!   probe runs), so the cloud assertions stop where that behaviour does.

mod support;

use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_ai_openrouter::transport::{AsyncHttpTransport, TransportFailure};
use plaudon_app::cloud_presentation::{degraded_reason_label, ProcessingStage, LOCAL_ONLY_REFUSAL};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, DuplicatePolicy, EngineInventoryEntry, EngineStage, ImportFilesRequest,
    ImportOutcome, JobKind, JobState, ListJobsRequest, ListModelsRequest, ListModelsResponse,
    ListRecordingsRequest, LocalOnlyModeResponse, ModelCard, ModelPreferenceScope, RecordingFilter,
    RecordingPage, RecordingSort, RecordingSortField, SetLocalOnlyModeRequest,
    SetModelPreferenceRequest, SetOpenRouterKeyRequest, SortDirection, StartProcessingRequest,
    StartProcessingResponse,
};
use plaudon_app::runtime::AppEngine;
use plaudon_app::shell_state::ShellStateStore;
use plaudon_app::testing_engines::{ScriptedStt, ScriptedTextModel, SCRIPTED_STT_ENGINE};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::RecordingId;
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_security::outbound::http::{GatedRequest, HttpResponse};
use plaudon_ui::shell::{HostHooks, Route};
use plaudon_ui_testkit::kittest::{NodeT as _, Queryable as _};
use plaudon_ui_testkit::Harness;

/* --------------------------------------------------------------- the watch */

/// One attempted outbound request, reduced to what a test log may keep.
///
/// §15.4 forbids a path, a credential or user content in a log line. The
/// destination is therefore the egress gate's own
/// [`EgressTicket::destination_label`](plaudon_security::outbound::EgressTicket::destination_label),
/// which the security crate guarantees is scheme, host and port and nothing
/// else, and the URL is reduced to its path with the query string and
/// fragment removed — a signed query parameter is a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attempt {
    /// `GET` or `POST`, from the gate's own vocabulary.
    method: &'static str,
    /// `scheme://host:port`, as the egress ticket renders it.
    destination: String,
    /// Request path, without the query string.
    path: String,
}

/// The transport the engine is given instead of the HTTPS one.
///
/// Every attempt is recorded and then refused with
/// [`TransportFailure::ConnectFailed`], which is the one failure whose
/// [`TransportFailure::bytes_transmitted`] is false: the fixture claims
/// nothing left the machine, because nothing did.
#[derive(Debug, Clone, Default)]
struct EgressWatch {
    attempts: Arc<Mutex<Vec<Attempt>>>,
}

impl EgressWatch {
    fn new() -> Self {
        Self::default()
    }

    /// The same watch, as the engine wants it.
    fn transport(&self) -> Arc<dyn AsyncHttpTransport> {
        Arc::new(self.clone())
    }

    fn attempts(&self) -> Vec<Attempt> {
        self.attempts.lock().expect("the egress log").clone()
    }
}

#[async_trait]
impl AsyncHttpTransport for EgressWatch {
    async fn send(&self, request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
        let attempt = Attempt {
            method: request.method().as_str(),
            destination: request.ticket().destination_label().to_owned(),
            path: path_of(request.url()),
        };
        self.attempts.lock().expect("the egress log").push(attempt);
        Err(TransportFailure::ConnectFailed {
            detail: "the egress watch never completes a request".to_owned(),
        })
    }
}

/// The path of `url`, with the query string and fragment dropped.
fn path_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.find('/').map_or("/", |at| &after_scheme[at..]);
    let path = path.split(['?', '#']).next().unwrap_or("/");
    path.to_owned()
}

/* ------------------------------------------------------------- the fixture */

const FIXTURE_RATE: u32 = 16_000;
const FIXTURE_SECONDS: u32 = 2;

/// A small 16 kHz mono PCM file, written where the engine is allowed to read.
fn write_wav(path: &Path) {
    let frames = FIXTURE_RATE * FIXTURE_SECONDS;
    let data_bytes = frames * 2;
    let mut file = std::fs::File::create(path).expect("create the fixture");
    file.write_all(b"RIFF").expect("write");
    file.write_all(&(36 + data_bytes).to_le_bytes())
        .expect("write");
    file.write_all(b"WAVEfmt ").expect("write");
    file.write_all(&16u32.to_le_bytes()).expect("write");
    file.write_all(&1u16.to_le_bytes()).expect("write");
    file.write_all(&1u16.to_le_bytes()).expect("write");
    file.write_all(&FIXTURE_RATE.to_le_bytes()).expect("write");
    file.write_all(&(FIXTURE_RATE * 2).to_le_bytes())
        .expect("write");
    file.write_all(&2u16.to_le_bytes()).expect("write");
    file.write_all(&16u16.to_le_bytes()).expect("write");
    file.write_all(b"data").expect("write");
    file.write_all(&data_bytes.to_le_bytes()).expect("write");
    for frame in 0..frames {
        let phase = i32::try_from(frame % 1_000).expect("a bounded phase");
        let sample = i16::try_from(phase * 16 - 8_000).expect("a bounded sample");
        file.write_all(&sample.to_le_bytes()).expect("write");
    }
    file.flush().expect("flush the fixture");
}

/// The whole application over `data_dir`, with the watch in place of the
/// HTTPS transport and the first-run gate already lifted.
///
/// This is [`support::app`] with one edge swapped: the configuration has to
/// be built here because `with_cloud_transport` is the seam under test. The
/// registration itself is still [`support::app_over`], so the application a
/// test drives is the one the binary ships.
fn watched_app(data_dir: &Path, hooks: HostHooks, watch: &EgressWatch) -> PlaudonShell {
    let engine = AppEngine::start(
        support::config(data_dir)
            .with_cloud_transport(watch.transport())
            // Real executors, scripted engines: the processing path that runs
            // here is the production one, so "nothing left the machine" is a
            // claim about work that actually ran.
            .with_stt_engines(vec![
                ScriptedStt::new(&["We agreed to ship on Friday."]).shared()
            ])
            .with_text_model(ScriptedTextModel::new(serde_json::Value::Null).shared()),
    )
    .expect("the engine starts over the library");
    // Parity row S15: until the first-run flow has been recorded the shell
    // draws setup instead of the window. Local-only Mode is left off here and
    // set afterwards through `set_local_only_mode`, which is the command
    // these tests are about.
    support::complete_onboarding(&engine, false);
    let (audio, _device) = support::fake_audio_out();
    support::app_over(engine, hooks, audio)
}

/* ------------------------------------------------------- dispatcher access */

fn try_call(app: &PlaudonShell, command: AppCommand) -> Result<AppResponse, ErrorEnvelope> {
    app.engine().dispatcher().call_blocking(command)
}

fn call(app: &PlaudonShell, command: AppCommand) -> AppResponse {
    let name = command.name();
    try_call(app, command).unwrap_or_else(|error| panic!("{name} failed: {error:?}"))
}

fn set_local_only(app: &PlaudonShell, enabled: bool) -> LocalOnlyModeResponse {
    match call(
        app,
        AppCommand::SetLocalOnlyMode(SetLocalOnlyModeRequest {
            enabled,
            confirmed: true,
        }),
    ) {
        AppResponse::SetLocalOnlyMode(response) => response,
        other => panic!("set_local_only_mode answered {}", other.command()),
    }
}

fn capabilities(app: &PlaudonShell) -> Capabilities {
    match call(app, AppCommand::GetCapabilities) {
        AppResponse::GetCapabilities(capabilities) => capabilities,
        other => panic!("get_capabilities answered {}", other.command()),
    }
}

fn recordings(app: &PlaudonShell) -> RecordingPage {
    let request = ListRecordingsRequest {
        cursor: None,
        limit: 50,
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
            field: RecordingSortField::ImportedAt,
            direction: SortDirection::Desc,
        },
    };
    match call(app, AppCommand::ListRecordings(request)) {
        AppResponse::ListRecordings(page) => page,
        other => panic!("list_recordings answered {}", other.command()),
    }
}

fn list_models(app: &PlaudonShell, stage: ProcessingStage) -> ListModelsResponse {
    match call(
        app,
        AppCommand::ListModels(ListModelsRequest {
            stage: stage.as_str().to_owned(),
            engine: None,
        }),
    ) {
        AppResponse::ListModels(response) => response,
        other => panic!("list_models answered {}", other.command()),
    }
}

fn prefer_model(
    app: &PlaudonShell,
    stage: ProcessingStage,
    engine: &str,
    model_id: &str,
) -> Result<AppResponse, ErrorEnvelope> {
    try_call(
        app,
        AppCommand::SetModelPreference(SetModelPreferenceRequest {
            stage: stage.as_str().to_owned(),
            scope: ModelPreferenceScope {
                kind: "default".to_owned(),
                recording_id: None,
            },
            engine: engine.to_owned(),
            model_id: Some(model_id.to_owned()),
        }),
    )
}

fn start_processing(
    app: &PlaudonShell,
    recording_id: RecordingId,
    stage: ProcessingStage,
) -> Result<StartProcessingResponse, ErrorEnvelope> {
    match try_call(
        app,
        AppCommand::StartProcessing(StartProcessingRequest {
            recording_id: recording_id.to_string(),
            stage: stage.as_str().to_owned(),
            model_override: None,
            cost_acknowledged: false,
            rerun: true,
        }),
    ) {
        Ok(AppResponse::StartProcessing(response)) => Ok(response),
        Ok(other) => panic!("start_processing answered {}", other.command()),
        Err(error) => Err(error),
    }
}

fn jobs_of(app: &PlaudonShell, recording_id: RecordingId, kind: JobKind) -> Vec<JobState> {
    let page = match call(
        app,
        AppCommand::ListJobs(ListJobsRequest {
            cursor: None,
            limit: 50,
            states: Vec::new(),
            kinds: vec![kind],
        }),
    ) {
        AppResponse::ListJobs(page) => page,
        other => panic!("list_jobs answered {}", other.command()),
    };
    page.items
        .iter()
        .filter(|job| job.recording_id == Some(recording_id))
        .map(|job| job.state)
        .collect()
}

/// The engine inventory rows of `stage` that the build reports as local.
fn local_engines(capabilities: &Capabilities, stage: EngineStage) -> Vec<EngineInventoryEntry> {
    capabilities
        .engines
        .iter()
        .filter(|engine| engine.stage == stage && !engine.requires_network)
        .cloned()
        .collect()
}

/* --------------------------------------------------------------- UI driving */

/// Import the picker's files through the window's own command, and wait for
/// the recording the import produced.
fn import_through_the_ui(harness: &mut Harness<'_, PlaudonShell>) -> RecordingId {
    harness.get_by_label("Import audio…").click();
    support::run_until(harness, "the imported recording", |app| {
        !recordings(app).items.is_empty()
    });
    recordings(harness.state())
        .items
        .first()
        .expect("the import produced a recording")
        .recording_id
}

/// Paint frames until every job of `kind` for `recording_id` is terminal.
///
/// Frames rather than sleeps: the shell is what learns about the worker, and
/// a test that asserts on jobs while the window is frozen is asserting about
/// a different application.
fn run_until_settled(
    harness: &mut Harness<'_, PlaudonShell>,
    recording_id: RecordingId,
    kind: JobKind,
) {
    support::run_until(harness, "the job to finish", |app| {
        let states = jobs_of(app, recording_id, kind);
        !states.is_empty() && states.iter().all(|state| state.is_terminal())
    });
}

/* ------------------------------------------------------------------- tests */

#[test]
fn local_only_mode_authorizes_no_outbound_request_across_import_transcribe_and_summarize() {
    let dir = tempfile::tempdir().expect("temporary library");
    let inbox = dir.path().join("imports");
    std::fs::create_dir_all(&inbox).expect("the import folder");
    let fixture = inbox.join("meeting.wav");
    write_wav(&fixture);

    let watch = EgressWatch::new();
    let store = ShellStateStore::new(dir.path());
    let app = watched_app(
        dir.path(),
        support::hooks_picking(store, vec![fixture]),
        &watch,
    );
    let mut harness = support::harness(app);

    // 1. Local-only Mode, on and reported as enforced.
    let mode = set_local_only(harness.state(), true);
    assert!(
        mode.enabled,
        "the mode the command reports back is the mode"
    );
    assert!(
        mode.description.contains("enforced")
            && mode
                .description
                .contains("no outbound request is permitted"),
        "the description must state what is enforced, not merely that something is: {:?}",
        mode.description
    );
    assert!(
        mode.allowed_hosts.is_empty(),
        "local-only mode reachable hosts must be none, got {:?}",
        mode.allowed_hosts
    );

    // 2. Import, through the shell's own "Import audio…" command and the
    //    host's file picker — the same path the window uses.
    let recording_id = import_through_the_ui(&mut harness);

    // 3. Open the recording so the detail panels actually fetch. The engine
    //    inventory is what the Transcription tab draws first, so waiting for
    //    an engine's display name waits for `get_capabilities` to have landed
    //    in the panel rather than for an arbitrary frame count.
    let report = capabilities(harness.state());
    let stt = local_engines(&report, EngineStage::Stt);
    assert!(
        !stt.is_empty(),
        "this build reports no local speech engine at all: {:?}",
        report.engines
    );
    harness
        .state_mut()
        .shell_mut()
        .navigate(Route::LibraryRecording {
            recording_id: recording_id.to_string(),
        });
    support::run_until_announced(&mut harness, &stt[0].display_name);

    // 4. Transcribe with a local engine. With nothing configured the app
    //    refuses rather than inventing a model, and that refusal is the
    //    honest first observation.
    let unconfigured = start_processing(harness.state(), recording_id, ProcessingStage::Transcribe)
        .expect_err("no model is configured for the transcribe stage yet");
    assert_eq!(unconfigured.code, AppErrorCode::ModelUnavailable);

    // The engine the worker will actually run is the scripted one this test
    // installed, so the stage runs to completion here instead of stopping at
    // `queued` — which is the point: the zero below is now a zero *with the
    // executors wired*, not a zero because nothing ran.
    prefer_model(
        harness.state(),
        ProcessingStage::Transcribe,
        SCRIPTED_STT_ENGINE,
        "scripted-small",
    )
    .expect("a local engine may be preferred while local-only mode is on");
    let queued = start_processing(harness.state(), recording_id, ProcessingStage::Transcribe)
        .expect("a local transcribe is accepted under local-only mode");
    assert_eq!(queued.dispatch, "queued");
    assert!(
        queued.blocked_reason.is_none(),
        "a local run is not blocked: {:?}",
        queued.blocked_reason
    );
    run_until_settled(&mut harness, recording_id, JobKind::Transcribe);
    assert_eq!(
        jobs_of(harness.state(), recording_id, JobKind::Transcribe),
        vec![JobState::Completed],
        "the local transcription ran on this machine and committed its output"
    );

    // 5. Summarize over the committed revision, on the local text model.
    prefer_model(
        harness.state(),
        ProcessingStage::ConsolidatedSummary,
        SCRIPTED_STT_ENGINE,
        "scripted-small",
    )
    .expect("a local text engine may be preferred while local-only mode is on");
    let summary = start_processing(
        harness.state(),
        recording_id,
        ProcessingStage::ConsolidatedSummary,
    )
    .expect("a local summary is accepted under local-only mode");
    assert_eq!(summary.dispatch, "queued");
    run_until_settled(&mut harness, recording_id, JobKind::Summarize);
    assert_eq!(
        jobs_of(harness.state(), recording_id, JobKind::Summarize),
        vec![JobState::Completed],
        "the local summary ran on this machine too"
    );

    // 6. A cloud engine cannot even be chosen while the mode is on.
    let refused = prefer_model(
        harness.state(),
        ProcessingStage::Transcribe,
        "openrouter",
        "openai/whisper-1",
    )
    .expect_err("local-only mode forbids selecting a cloud model");
    assert_eq!(refused.code, AppErrorCode::LocalOnlyMode);

    // 7. The catalogue the picker shows is the pinned allowlist, and it says
    //    why. Nothing was fetched to build it.
    let catalogue = list_models(harness.state(), ProcessingStage::Transcribe);
    assert_eq!(catalogue.catalog_source, "pinned_allowlist");
    assert_eq!(
        catalogue.degraded_reason.as_deref(),
        Some("local_only_mode")
    );

    // 8. Paint a few more frames so every panel that was going to ask for
    //    something has asked, then close the window the way the binary does.
    harness.run_steps(8);
    harness.state_mut().shutdown();

    assert!(
        watch.attempts().is_empty(),
        "local-only mode authorized {} outbound request(s): {:?}",
        watch.attempts().len(),
        watch.attempts()
    );
}

/// The negative control: a watch that cannot fail proves nothing.
///
/// `crates/app/src/backend/cloud.rs` has exactly one flow that reaches the
/// transport without a verified model: `list_models` builds a
/// [`plaudon_ai_openrouter::discovery::CatalogClient`] when Local-only Mode is
/// off **and** a key is stored, and the catalogue refresh is a real `GET`. The
/// cloud dispatch path cannot be used as the control in a hermetic build,
/// because `prepare_cloud_transcription` refuses an unverified card
/// (`CapabilityUnverified`) before it ever authorizes egress — see the end of
/// this test, which asserts that refusal rather than pretending otherwise.
#[test]
fn the_watch_catches_the_catalogue_refresh_once_the_key_is_stored() {
    let dir = tempfile::tempdir().expect("temporary library");
    let watch = EgressWatch::new();
    let store = ShellStateStore::new(dir.path());
    let app = watched_app(dir.path(), support::hooks(store), &watch);
    let mut harness = support::harness(app);

    // Off, explicitly: the control must not rest on a default.
    let mode = set_local_only(harness.state(), false);
    assert!(!mode.enabled);

    // Without a key no client is built, so there is nothing to attempt yet.
    let before = list_models(harness.state(), ProcessingStage::Transcribe);
    assert_eq!(before.degraded_reason.as_deref(), Some("no_key"));
    assert!(
        watch.attempts().is_empty(),
        "nothing should be attempted before a key exists: {:?}",
        watch.attempts()
    );

    call(
        harness.state(),
        AppCommand::SetOpenRouterKey(SetOpenRouterKeyRequest {
            // The in-memory secret store never touches the OS keychain.
            key: "sk-or-v1-0000000000000000000000000000".to_owned(),
        }),
    );

    let after = list_models(harness.state(), ProcessingStage::Transcribe);
    let attempts = watch.attempts();
    assert_eq!(
        attempts.len(),
        1,
        "the catalogue refresh must reach the transport exactly once: {attempts:?}"
    );
    assert_eq!(attempts[0].method, "GET");
    assert!(
        attempts[0].destination.contains("openrouter.ai"),
        "unexpected destination {:?}",
        attempts[0].destination
    );
    assert_eq!(attempts[0].path, "/api/v1/models");
    // The watch refused the connection, so the catalogue degrades rather than
    // inventing entries, and it says which way it degraded.
    assert_eq!(after.degraded_reason.as_deref(), Some("offline"));
    assert_eq!(after.catalog_source, "pinned_allowlist");

    // And the other half of the control: with the mode off and a key stored,
    // a cloud transcription is still refused here — not by the egress gate but
    // by §8.6A, because no card in this build has passed a capability check.
    // So `start_processing` is not a usable positive control, and no further
    // attempt reaches the transport.
    let fixture = dir.path().join("imports");
    std::fs::create_dir_all(&fixture).expect("the import folder");
    let fixture = fixture.join("meeting.wav");
    write_wav(&fixture);
    let imported = match call(
        harness.state(),
        AppCommand::ImportFiles(ImportFilesRequest {
            paths: vec![fixture.to_string_lossy().into_owned()],
            duplicate_policy: DuplicatePolicy::Ask,
        }),
    ) {
        AppResponse::ImportFiles(response) => response,
        other => panic!("import_files answered {}", other.command()),
    };
    assert!(
        matches!(
            imported.results.first(),
            Some(ImportOutcome::Accepted { .. })
        ),
        "the fixture should import: {:?}",
        imported.results
    );
    let recording_id = recordings(harness.state())
        .items
        .first()
        .expect("the imported recording")
        .recording_id;
    // Not `first_selectable_model`: no transcription card in the pinned
    // allowlist is selectable until a capability probe has run, which is the
    // very refusal this half of the control pins down.
    let model = catalogue_head(&after);
    prefer_model(
        harness.state(),
        ProcessingStage::Transcribe,
        &model.engine,
        &model.model_id,
    )
    .expect("a cloud model may be preferred once local-only mode is off");
    let refused = start_processing(harness.state(), recording_id, ProcessingStage::Transcribe)
        .expect_err("an unverified model is refused before anything is sent");
    assert_eq!(refused.code, AppErrorCode::CapabilityUnverified);
    assert_eq!(
        watch.attempts().len(),
        1,
        "a refusal before dispatch sends nothing: {:?}",
        watch.attempts()
    );

    harness.state_mut().shutdown();
}

/// The §4.3 surface of the recording detail, and the exact limit of it.
///
/// What the user can see while Local-only Mode is on is asserted here through
/// the real panels: the Transcription tab offers nothing but engines that run
/// on this computer, and the summary catalogue is the pinned allowlist with
/// every cloud row blocked and unchoosable.
///
/// What this test deliberately does **not** claim is that
/// [`LOCAL_ONLY_REFUSAL`] was rendered. That sentence lives on two controls,
/// and neither can be reached in a hermetic build:
///
/// * `crates/ui/src/ai/transcription.rs` paints it beside a speech engine
///   that requires the network, and the inventory has none — OpenRouter is an
///   `EngineStage::Text` row, so the Transcription tab never lists it;
/// * `crates/ui/src/ai/stage.rs` paints it on the dispatch control, which
///   only exists once a model is *selected*, and
///   `ModelCard::is_selectable` is false for every card until a capability
///   probe has been recorded — a probe is a real, billable provider request
///   (`CapabilitySmokeTest`), so no hermetic test can produce one.
///
/// Both facts are asserted below, so the day either changes this test fails
/// and the refusal sentence can be asserted positively instead.
#[test]
fn the_recording_detail_offers_no_cloud_path_while_local_only_mode_is_on() {
    let dir = tempfile::tempdir().expect("temporary library");
    let inbox = dir.path().join("imports");
    std::fs::create_dir_all(&inbox).expect("the import folder");
    let fixture = inbox.join("meeting.wav");
    write_wav(&fixture);

    let watch = EgressWatch::new();
    let store = ShellStateStore::new(dir.path());
    let app = watched_app(
        dir.path(),
        support::hooks_picking(store, vec![fixture]),
        &watch,
    );
    // Before the first frame: the recording detail reads the capability
    // report once and keeps it, so the mode has to be on by the time the
    // shell's startup `get_capabilities` answers.
    assert!(set_local_only(&app, true).enabled);
    let mut harness = support::harness(app);

    let report = capabilities(harness.state());
    let stt = local_engines(&report, EngineStage::Stt);
    assert_eq!(
        stt.len(),
        report
            .engines
            .iter()
            .filter(|engine| engine.stage == EngineStage::Stt)
            .count(),
        "this build now reports a speech engine that needs the network, so the Transcription \
         tab's own Local-only badge is reachable and should be asserted directly: {:?}",
        report.engines
    );

    let recording_id = import_through_the_ui(&mut harness);
    harness
        .state_mut()
        .shell_mut()
        .navigate(Route::LibraryRecording {
            recording_id: recording_id.to_string(),
        });
    support::run_until_announced(&mut harness, &stt[0].display_name);

    // The Transcription tab, as the user reads it: every engine it offers
    // says it runs here, and none says it goes through a provider.
    let transcription_tab = support::announced(&harness);
    assert!(
        transcription_tab
            .iter()
            .any(|text| text == "Runs: On this computer"),
        "the Transcription tab states where each engine runs: {transcription_tab:?}"
    );
    assert!(
        !transcription_tab
            .iter()
            .any(|text| text.contains("Through a cloud provider")),
        "no engine offered for transcription may go through a provider under local-only mode"
    );

    // The summary catalogue is where the cloud models are listed at all. Every
    // row carries the §4.3 block and none of them can be chosen.
    let catalogue = list_models(harness.state(), ProcessingStage::ConsolidatedSummary);
    assert!(
        !catalogue.models.is_empty(),
        "the pinned allowlist should still list the text models it cannot use"
    );
    assert_eq!(
        catalogue.degraded_reason.as_deref(),
        Some("local_only_mode")
    );
    // Through the accessibility action rather than a synthetic pointer: the
    // outputs column scrolls, and a tab below the fold would never be hit.
    harness.get_by_label("Summarize").click_accesskit();
    // The tab keeps the previous stage's catalogue on screen until the new
    // one lands, so the wait is for this stage's own rows.
    support::run_until_frame(&mut harness, "the summary catalogue", |harness| {
        support::announces(harness, "Blocked by Local-only Mode")
            && catalogue
                .models
                .iter()
                .all(|model| support::announces(harness, &model.display_name))
    });
    assert!(
        support::announces(&harness, degraded_reason_label("local_only_mode")),
        "the picker must say why it is showing the pinned allowlist: {:?}",
        support::announced(&harness)
    );
    for model in &catalogue.models {
        assert!(
            harness
                .get_by_label(model.display_name.as_str())
                .accesskit_node()
                .is_disabled(),
            "{} must not be choosable under local-only mode",
            model.display_name
        );
    }

    // The limit, stated as an assertion rather than as a comment.
    assert!(
        catalogue.models.iter().all(|model| !model.selectable),
        "a card became selectable without a capability probe, so the dispatch control — and \
         with it LOCAL_ONLY_REFUSAL — is now reachable: {:?}",
        catalogue
            .models
            .iter()
            .map(|model| (&model.model_id, model.selectable, &model.verification))
            .collect::<Vec<_>>()
    );
    assert!(
        !support::announces(&harness, LOCAL_ONLY_REFUSAL),
        "the dispatch control cannot exist without a selected model; if this fires, assert \
         the refusal sentence positively instead of asserting its absence"
    );

    harness.state_mut().shutdown();
    assert!(
        watch.attempts().is_empty(),
        "no key was ever stored and the mode is on, so nothing should have been attempted: {:?}",
        watch.attempts()
    );
}

/// The first card of a catalogue response, selectable or not.
fn catalogue_head(catalogue: &ListModelsResponse) -> ModelCard {
    catalogue
        .models
        .first()
        .expect("the pinned allowlist is never empty")
        .clone()
}
