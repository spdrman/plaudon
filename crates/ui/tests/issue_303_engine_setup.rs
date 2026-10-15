//! Issue #303 and #305: engine setup from first-run step 3, the same actions
//! in Settings, and an error that offers the fix.
//!
//! Driven the way a user drives it: find the control by its accessible name,
//! press it, and look at what the screen asked the application to do. The
//! screen never fetches anything itself, so "nothing was contacted" is
//! asserted as "no command was raised", and "what would be contacted" as the
//! words on the screen before the click.

use std::collections::BTreeMap;

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, CatalogueOfferDto, EngineInventoryEntry, EngineStage, HardwareInfo,
    InstallModelResponse, LocalModelDto, LocalOnlyModeResponse, ModelDiskUsageDto, ModelOriginDto,
    OsInfo, Platform, PlaudToolchainStatus, StorageCapability, ToolPresence, PLAUD_CONNECT_INTENT,
};
use plaudon_app::job_presentation::envelope_action_control;
use plaudon_app::onboarding::OnboardingStep;
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorAction, ErrorEnvelope};
use plaudon_domain::time::Timestamp;
use plaudon_ui::onboarding::OnboardingScreen;
use plaudon_ui::settings::engines::EnginesSection;
use plaudon_ui::settings::{
    FilePurpose, PickedPath, Section, SettingsData, SettingsIntent, SettingsOutput, VisualsTokens,
};
use plaudon_ui::widgets::error_notice::{error_notice, ErrorNoticeAction};
use plaudon_ui_testkit::a11y;

const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";
const TINY: u64 = 77_691_713;
const BASE_EN: u64 = 147_964_211;
const APPLE_REASON: &str =
    "Apple Speech is not available in this build: the Apple bridge is not linked.";

/* ------------------------------------------------------------- fixtures */

fn file(name: &str) -> String {
    format!("/ggerganov/whisper.cpp/resolve/{REVISION}/{name}")
}

fn offer(
    model_id: &str,
    display_name: &str,
    file_name: &str,
    size_bytes: u64,
) -> CatalogueOfferDto {
    CatalogueOfferDto {
        model_id: model_id.to_owned(),
        display_name: display_name.to_owned(),
        size_bytes,
        licence_spdx_id: "MIT".to_owned(),
        licence_name: "MIT License".to_owned(),
        host: "huggingface.co".to_owned(),
        file: file(file_name),
        delivery_host_suffix: Some("hf.co".to_owned()),
        recommended_ram_bytes: 1 << 30,
        installed: false,
    }
}

fn whisper() -> EngineInventoryEntry {
    EngineInventoryEntry {
        engine_id: "whisper.cpp".to_owned(),
        display_name: "whisper.cpp (local)".to_owned(),
        stage: EngineStage::Stt,
        status: CapabilityStatus::Unknown,
        unavailable_reason: Some("No whisper.cpp model is installed on this machine.".to_owned()),
        installed_models: Vec::new(),
        download_bytes_required: Some(TINY),
        requires_network: false,
        catalogue: vec![
            offer(
                "whisper-tiny",
                "Whisper tiny (multilingual)",
                "ggml-tiny.bin",
                TINY,
            ),
            offer(
                "whisper-base-en",
                "Whisper base (English)",
                "ggml-base.en.bin",
                BASE_EN,
            ),
        ],
    }
}

fn apple() -> EngineInventoryEntry {
    EngineInventoryEntry {
        engine_id: "apple.speech".to_owned(),
        display_name: "Apple Speech (on device)".to_owned(),
        stage: EngineStage::Stt,
        status: CapabilityStatus::Unsupported,
        unavailable_reason: Some(APPLE_REASON.to_owned()),
        installed_models: Vec::new(),
        download_bytes_required: None,
        requires_network: false,
        catalogue: Vec::new(),
    }
}

fn openrouter() -> EngineInventoryEntry {
    EngineInventoryEntry {
        engine_id: "openrouter".to_owned(),
        display_name: "OpenRouter".to_owned(),
        stage: EngineStage::Text,
        status: CapabilityStatus::Unknown,
        unavailable_reason: Some("No OpenRouter key is stored.".to_owned()),
        installed_models: Vec::new(),
        download_bytes_required: None,
        requires_network: true,
        catalogue: Vec::new(),
    }
}

fn capabilities(local_only: bool) -> Capabilities {
    let mut integrations = BTreeMap::new();
    integrations.insert("plaud_cloud".to_owned(), CapabilityStatus::Unknown);
    let mut preferences = plaudon_app::backend::default_preferences();
    preferences.local_only_mode = local_only;
    Capabilities {
        os: OsInfo {
            platform: Platform::Macos,
            version: "15.0".to_owned(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".to_owned(),
            logical_cores: 10,
            total_memory_bytes: 16 << 30,
        },
        engines: vec![whisper(), apple(), openrouter()],
        storage: StorageCapability {
            library_path: "/Users/someone/Library/Plaudon".to_owned(),
            free_bytes: 100 << 30,
            total_bytes: 500 << 30,
        },
        integrations,
        permissions: BTreeMap::new(),
        preferences,
        openrouter_key_present: false,
        cloud_consents: Vec::new(),
    }
}

fn presence(program: &str, path: Option<&str>) -> ToolPresence {
    ToolPresence {
        program: program.to_owned(),
        path: path.map(str::to_owned),
    }
}

const INSTALL_COMMAND: &str =
    "npm install --global --registry=https://registry.npmjs.org/ @plaud-ai/cli@0.3.15";

fn toolchain(node: bool, npm: bool, plaud: bool) -> PlaudToolchainStatus {
    let mut missing = Vec::new();
    for (name, present) in [("node", node), ("npm", npm), ("plaud", plaud)] {
        if !present {
            missing.push(name.to_owned());
        }
    }
    PlaudToolchainStatus {
        node: presence("node", node.then_some("/opt/homebrew/bin/node")),
        npm: presence("npm", npm.then_some("/opt/homebrew/bin/npm")),
        plaud: presence("plaud", plaud.then_some("/opt/homebrew/bin/plaud")),
        plaud_version: plaud.then(|| "0.3.15".to_owned()),
        exercised_release: "0.3.15".to_owned(),
        release_exercised: plaud.then_some(true),
        missing,
        install_command: INSTALL_COMMAND.to_owned(),
        login_command: "plaud login".to_owned(),
        guided_install_available: npm,
        summary: if plaud {
            "The PLAUD CLI 0.3.15 is installed, the release this build has exercised.".to_owned()
        } else {
            "Node.js, npm and the PLAUD CLI are needed for PLAUD Cloud.".to_owned()
        },
    }
}

fn envelope(code: AppErrorCode) -> ErrorEnvelope {
    let mut log = DiagnosticLog::new();
    AppError::new(code, "internal detail").to_envelope(&mut log, Timestamp::now())
}

/* --------------------------------------------------------- the onboarding */

struct App {
    screen: OnboardingScreen,
    capabilities: Capabilities,
    out: SettingsOutput,
}

fn onboarding(capabilities: Capabilities) -> Harness<'static, App> {
    plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 2400.0))
        .build_ui_state(
            |ui, app: &mut App| {
                let App {
                    screen,
                    capabilities,
                    out,
                } = app;
                screen.ui(ui, capabilities, out);
            },
            App {
                screen: OnboardingScreen::new(),
                capabilities,
                out: SettingsOutput::new(),
            },
        )
}

fn click(harness: &mut Harness<'_, App>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

fn has(harness: &Harness<'_, App>, label: &str) -> bool {
    harness.query_all_by_label(label).next().is_some()
}

fn announces(harness: &Harness<'_, App>, needle: &str) -> bool {
    a11y::nodes(harness)
        .iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .any(|text| text.contains(needle))
}

fn buttons(harness: &Harness<'_, App>) -> Vec<String> {
    a11y::nodes(harness)
        .into_iter()
        .filter(|node| node.role == "Button")
        .filter_map(|node| node.label)
        .collect()
}

fn disabled(harness: &Harness<'_, App>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

fn commands<'a>(harness: &'a Harness<'_, App>) -> Vec<&'a AppCommand> {
    harness
        .state()
        .out
        .intents
        .iter()
        .filter_map(|intent| match intent {
            SettingsIntent::Command(command) => Some(command),
            _ => None,
        })
        .collect()
}

fn command<'a>(harness: &'a Harness<'_, App>, name: &str) -> &'a AppCommand {
    commands(harness)
        .into_iter()
        .find(|command| command.name() == name)
        .unwrap_or_else(|| panic!("no {name} command was raised"))
}

fn raised(harness: &Harness<'_, App>, name: &str) -> bool {
    commands(harness)
        .iter()
        .any(|command| command.name() == name)
}

fn clear(harness: &mut Harness<'_, App>) {
    harness.state_mut().out = SettingsOutput::new();
}

fn respond(
    harness: &mut Harness<'_, App>,
    command: &'static str,
    result: &Result<AppResponse, ErrorEnvelope>,
) {
    {
        let App { screen, out, .. } = harness.state_mut();
        screen.on_response(command, result, out);
    }
    harness.run();
}

fn advance_to_engines(harness: &mut Harness<'_, App>) {
    harness.run();
    while harness.state().screen.state().step() != OnboardingStep::Engines {
        click(harness, "Next");
    }
    harness.run();
}

/// Answer step two with "allow cloud AI later", the way the shell would.
fn choose_cloud_later(harness: &mut Harness<'_, App>) {
    harness.run();
    click(harness, "Next");
    click(
        harness,
        "Allow cloud AI later — choosing this now grants nothing by itself. No request is ever \
sent until you pick a provider and consent for that purpose.",
    );
    respond(
        harness,
        "set_local_only_mode",
        &Ok(AppResponse::SetLocalOnlyMode(LocalOnlyModeResponse {
            enabled: false,
            description: "Outbound requests are allowed only to providers you consent to."
                .to_owned(),
            allowed_hosts: Vec::new(),
        })),
    );
    harness.state_mut().capabilities.preferences.local_only_mode = false;
    advance_to_engines(harness);
    clear(harness);
    harness.run();
}

const IMPORT: &str = "Import a model file I already have";

#[test]
fn issue_303_step_three_offers_each_catalogue_model_with_its_real_size_and_licence() {
    let mut harness = onboarding(capabilities(true));
    advance_to_engines(&mut harness);

    for (name, size) in [
        ("Whisper tiny (multilingual)", TINY),
        ("Whisper base (English)", BASE_EN),
    ] {
        let line = format!("{name} — {} · MIT License", format_bytes(Some(size)));
        assert!(announces(&harness, &line), "missing {line:?}");
        assert!(
            has(&harness, &format!("Download {name}")),
            "no download control for {name}: {:?}",
            buttons(&harness)
        );
    }
    assert!(has(&harness, IMPORT), "{:?}", buttons(&harness));
    assert!(
        !announces(&harness, "once a model source is configured"),
        "step three still promises a model source this build does not have"
    );
    assert!(
        !announces(&harness, "offline model pack shipped with the installer"),
        "step three still promises an offline pack this build does not ship"
    );
}

#[test]
fn issue_303_step_three_says_exactly_what_a_download_would_contact_before_any_click() {
    let mut harness = onboarding(capabilities(true));
    advance_to_engines(&mut harness);

    assert!(
        announces(&harness, "huggingface.co"),
        "the host is not named"
    );
    assert!(
        announces(&harness, "hf.co"),
        "the delivery domain is not named"
    );
    assert!(
        announces(&harness, &file("ggml-tiny.bin")),
        "the exact file is not named"
    );
    assert!(
        !raised(&harness, "install_model_download"),
        "a download was raised without a click"
    );
}

#[test]
fn issue_303_local_only_mode_disables_downloads_but_leaves_the_import_working() {
    let mut harness = onboarding(capabilities(true));
    advance_to_engines(&mut harness);

    assert!(
        disabled(&harness, "Download Whisper tiny (multilingual)"),
        "Local-only Mode must not offer a live download button"
    );
    assert!(announces(
        &harness,
        "Local-only mode is on, so Plaudon will not download"
    ));
    harness
        .get_by_label("Download Whisper tiny (multilingual)")
        .click();
    harness.run();
    assert!(!raised(&harness, "install_model_download"));

    assert!(!disabled(&harness, IMPORT), "the import works offline");
    click(&mut harness, IMPORT);
    assert!(
        harness
            .state()
            .out
            .intents
            .iter()
            .any(|intent| matches!(intent, SettingsIntent::PickFile(FilePurpose::ModelFile))),
        "the import must open a file picker"
    );
    clear(&mut harness);
    {
        let App { screen, out, .. } = harness.state_mut();
        screen.path_picked(
            PickedPath::File(FilePurpose::ModelFile),
            "/Users/someone/Downloads/ggml-tiny.bin",
            out,
        );
    }
    harness.run();
    let AppCommand::ImportModelFile(request) = command(&harness, "import_model_file") else {
        panic!("wrong command");
    };
    assert_eq!(request.path, "/Users/someone/Downloads/ggml-tiny.bin");
    assert_eq!(
        request.model_id, None,
        "the file is matched by its checksum"
    );
}

#[test]
fn issue_303_a_download_needs_that_exact_action_approved_first() {
    let mut harness = onboarding(capabilities(false));
    choose_cloud_later(&mut harness);

    click(&mut harness, "Download Whisper tiny (multilingual)");
    assert!(
        !raised(&harness, "install_model_download"),
        "the first click must ask, not download"
    );
    assert!(
        has(&harness, "Approve this download"),
        "{:?}",
        buttons(&harness)
    );
    assert!(announces(&harness, "Plaudon will contact huggingface.co"));

    click(&mut harness, "Approve this download");
    let AppCommand::InstallModelDownload(request) = command(&harness, "install_model_download")
    else {
        panic!("wrong command");
    };
    assert_eq!(request.model_id, "whisper-tiny");
    assert_eq!(request.approved_host, "huggingface.co");
    assert_eq!(request.approved_file, file("ggml-tiny.bin"));
    assert_eq!(request.licence_accepted_spdx_id, "MIT");
    assert!(request.footprint_acknowledged);
}

#[test]
fn issue_303_declining_the_approval_downloads_nothing() {
    let mut harness = onboarding(capabilities(false));
    choose_cloud_later(&mut harness);

    click(&mut harness, "Download Whisper base (English)");
    click(&mut harness, "Don't download");
    assert!(!raised(&harness, "install_model_download"));
    assert!(!has(&harness, "Approve this download"));
}

#[test]
fn issue_303_a_running_download_shows_progress_and_can_be_cancelled() {
    let mut harness = onboarding(capabilities(false));
    choose_cloud_later(&mut harness);
    click(&mut harness, "Download Whisper tiny (multilingual)");
    click(&mut harness, "Approve this download");
    clear(&mut harness);

    harness.state_mut().screen.install_progress(TINY / 2, TINY);
    harness.run();
    assert!(
        announces(&harness, "Downloading Whisper tiny (multilingual)"),
        "the download is not shown as running"
    );
    let figure = format!(
        "{} of {}",
        format_bytes(Some(TINY / 2)),
        format_bytes(Some(TINY))
    );
    assert!(
        announces(&harness, &figure),
        "no measured progress {figure:?}"
    );

    click(&mut harness, "Cancel download");
    let AppCommand::CancelModelInstall(request) = command(&harness, "cancel_model_install") else {
        panic!("wrong command");
    };
    assert_eq!(request.model_id, "whisper-tiny");
}

#[test]
fn issue_303_a_checksum_failure_is_reported_and_the_download_can_be_tried_again() {
    let mut harness = onboarding(capabilities(false));
    choose_cloud_later(&mut harness);
    click(&mut harness, "Download Whisper tiny (multilingual)");
    click(&mut harness, "Approve this download");

    let failure = envelope(AppErrorCode::IntegrityFailed);
    respond(
        &mut harness,
        "install_model_download",
        &Err(failure.clone()),
    );

    assert!(
        announces(&harness, &failure.message),
        "the refusal is not shown"
    );
    assert!(!has(&harness, "Cancel download"));
    assert!(!disabled(&harness, "Download Whisper tiny (multilingual)"));
}

#[test]
fn issue_303_a_finished_install_is_announced_and_the_inventory_is_reread() {
    let mut harness = onboarding(capabilities(false));
    choose_cloud_later(&mut harness);
    click(&mut harness, "Download Whisper tiny (multilingual)");
    click(&mut harness, "Approve this download");
    clear(&mut harness);

    respond(
        &mut harness,
        "install_model_download",
        &Ok(AppResponse::InstallModelDownload(InstallModelResponse {
            model: LocalModelDto {
                model_id: "whisper-tiny".to_owned(),
                display_name: "Whisper tiny (multilingual)".to_owned(),
                engine: "whisper.cpp".to_owned(),
                version: "whisper-tiny".to_owned(),
                size_bytes: TINY,
                licence_spdx_id: "MIT".to_owned(),
                origin: ModelOriginDto::Download,
                keep_offline: false,
            },
            transferred_bytes: TINY,
            disk: ModelDiskUsageDto {
                installed_bytes: TINY,
                staging_bytes: 0,
            },
        })),
    );

    assert!(announces(
        &harness,
        "Whisper tiny (multilingual) is installed"
    ));
    assert!(
        harness
            .state()
            .out
            .intents
            .iter()
            .any(|intent| matches!(intent, SettingsIntent::RefreshCapabilities)),
        "the step must re-read the inventory so the engine becomes choosable"
    );
}

#[test]
fn issue_303_an_engine_that_is_genuinely_unavailable_gets_no_install_control() {
    let mut harness = onboarding(capabilities(true));
    advance_to_engines(&mut harness);

    assert!(
        announces(&harness, APPLE_REASON),
        "the honest reason is kept"
    );
    let labels = buttons(&harness);
    assert!(
        !labels.iter().any(|label| label.contains("Apple")),
        "Apple Speech got a control: {labels:?}"
    );
    assert_eq!(
        labels
            .iter()
            .filter(|label| label.as_str() == IMPORT)
            .count(),
        1,
        "only the engine with a catalogue offers an import: {labels:?}"
    );
}

#[test]
fn issue_303_step_three_detects_the_plaud_cli_and_reports_what_is_missing() {
    let mut harness = onboarding(capabilities(true));
    advance_to_engines(&mut harness);
    assert!(
        raised(&harness, "detect_plaud_toolchain"),
        "step three must look for node, npm and plaud"
    );

    respond(
        &mut harness,
        "detect_plaud_toolchain",
        &Ok(AppResponse::DetectPlaudToolchain(toolchain(
            false, false, false,
        ))),
    );

    assert!(announces(&harness, "PLAUD Cloud CLI"));
    for line in [
        "Node.js: not found",
        "npm: not found",
        "PLAUD CLI: not found",
    ] {
        assert!(announces(&harness, line), "missing {line:?}");
    }
    assert!(
        announces(&harness, "@plaud-ai/cli 0.3.15"),
        "the exercised release"
    );
    assert!(announces(&harness, INSTALL_COMMAND), "the copyable command");
    clear(&mut harness);
    click(&mut harness, "Copy install command");
    assert!(
        harness.state().out.intents.iter().any(
            |intent| matches!(intent, SettingsIntent::CopyText(text) if text == INSTALL_COMMAND)
        ),
        "copying must hand over the exact command"
    );
    assert!(
        disabled(&harness, "Install the PLAUD CLI"),
        "there is no npm to run, and Local-only Mode is on"
    );
}

#[test]
fn issue_303_the_guided_cli_install_asks_for_approval_and_names_the_registry() {
    let mut harness = onboarding(capabilities(false));
    choose_cloud_later(&mut harness);
    respond(
        &mut harness,
        "detect_plaud_toolchain",
        &Ok(AppResponse::DetectPlaudToolchain(toolchain(
            true, true, false,
        ))),
    );
    assert!(announces(
        &harness,
        "Node.js: found at /opt/homebrew/bin/node"
    ));
    assert!(announces(&harness, "registry.npmjs.org"));

    click(&mut harness, "Install the PLAUD CLI");
    assert!(
        !raised(&harness, "install_plaud_cli"),
        "the first click must ask"
    );
    click(&mut harness, "Approve this install");

    let AppCommand::InstallPlaudCli(request) = command(&harness, "install_plaud_cli") else {
        panic!("wrong command");
    };
    assert_eq!(request.approved_host, "registry.npmjs.org");
    assert_eq!(request.approved_package, "@plaud-ai/cli@0.3.15");
}

#[test]
fn issue_303_signing_in_goes_through_the_existing_plaud_connect_launcher() {
    let mut harness = onboarding(capabilities(false));
    choose_cloud_later(&mut harness);
    respond(
        &mut harness,
        "detect_plaud_toolchain",
        &Ok(AppResponse::DetectPlaudToolchain(toolchain(
            true, true, true,
        ))),
    );
    clear(&mut harness);

    click(&mut harness, "Copy sign-in command");
    assert!(harness
        .state()
        .out
        .intents
        .iter()
        .any(|intent| matches!(intent, SettingsIntent::CopyText(text) if text == "plaud login")));

    click(&mut harness, "Sign in to PLAUD");
    let AppCommand::PlaudConnect(request) = command(&harness, "plaud_connect") else {
        panic!("wrong command");
    };
    assert_eq!(request.user_intent, PLAUD_CONNECT_INTENT);
}

/* ------------------------------------------------------------ Settings */

struct SettingsApp {
    section: EnginesSection,
    data: SettingsData,
    out: SettingsOutput,
}

fn settings(local_only: bool) -> Harness<'static, SettingsApp> {
    plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 3000.0))
        .build_ui_state(
            |ui, app: &mut SettingsApp| {
                let tokens = VisualsTokens::from_ui(ui);
                app.section.ui(ui, &tokens, &app.data, &mut app.out);
            },
            SettingsApp {
                section: EnginesSection::default(),
                data: SettingsData {
                    capabilities: Some(capabilities(local_only)),
                    ..SettingsData::default()
                },
                out: SettingsOutput::new(),
            },
        )
}

#[test]
fn issue_303_settings_keeps_the_same_actions_as_step_three() {
    let mut harness = settings(false);
    harness.run();
    assert!(
        harness
            .state()
            .out
            .command_names()
            .contains(&"detect_plaud_toolchain"),
        "Settings must detect the PLAUD CLI too"
    );
    {
        let SettingsApp { section, out, .. } = harness.state_mut();
        section.on_response(
            "detect_plaud_toolchain",
            &Ok(AppResponse::DetectPlaudToolchain(toolchain(
                false, false, false,
            ))),
            out,
        );
    }
    harness.run();

    let labels: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter(|node| node.role == "Button")
        .filter_map(|node| node.label)
        .collect();
    for wanted in [
        "Download Whisper tiny (multilingual)",
        "Download Whisper base (English)",
        IMPORT,
        "Copy install command",
    ] {
        assert!(
            labels.iter().any(|label| label == wanted),
            "Settings lacks {wanted:?}: {labels:?}"
        );
    }
    harness
        .get_by_label("Download Whisper tiny (multilingual)")
        .click();
    harness.run();
    harness.get_by_label("Approve this download").click();
    harness.run();
    assert!(harness
        .state()
        .out
        .command_names()
        .contains(&"install_model_download"));
}

/* ----------------------------------------------------- the error, #305 */

#[test]
fn issue_305_the_no_speech_model_error_offers_install_and_import_directly() {
    let failure = envelope(AppErrorCode::NoSpeechModel);
    assert_eq!(failure.action, Some(ErrorAction::InstallSpeechModel));

    let mut harness = plaudon_ui_testkit::harness_with_state(
        |ui, state: &mut (ErrorEnvelope, Option<ErrorNoticeAction>)| {
            if let Some(action) = error_notice(ui, &state.0, "Transcription could not start.") {
                state.1 = Some(action);
            }
        },
        (failure.clone(), None),
    );
    harness.run();

    harness.get_by_label("Install a model").click();
    harness.run();
    assert_eq!(
        harness.state().1,
        Some(ErrorNoticeAction::InstallSpeechModel)
    );

    harness.state_mut().1 = None;
    harness.get_by_label("Import a model file").click();
    harness.run();
    assert_eq!(harness.state().1, Some(ErrorNoticeAction::ImportModelFile));
    assert!(
        !a11y::nodes(&harness)
            .iter()
            .any(|node| node.label.as_deref() == Some("Choose another engine")),
        "there is no other engine to choose"
    );
}

#[test]
fn issue_305_a_failed_transcription_job_offers_to_install_a_model() {
    let control = envelope_action_control(Some(ErrorAction::InstallSpeechModel), false)
        .expect("the failure offers a control");
    assert_eq!(control.label, "Install a model");
}
