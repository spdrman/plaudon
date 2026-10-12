//! Interaction tests for first-run setup (matrix rows C8 and S15).
//!
//! The flow is driven the way a user drives it: find the control by its
//! accessible name, press it, and then look at what the screen asked the
//! application to do. The step machine's own rules are unit-tested in
//! `plaudon_app::onboarding`; what is asserted here is that the view obeys
//! them, that no step invents a fact, and that the key never becomes visible.

use std::collections::BTreeMap;

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, EngineInventoryEntry, EngineStage, HardwareInfo, InstalledModel,
    LocalOnlyModeResponse, NotificationCategory, NotificationSettings, OsInfo, Platform,
    Preferences, StatusResponse, StorageCapability,
};
use plaudon_app::onboarding::{engines_download_note, OnboardingStep};
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::ErrorEnvelope;
use plaudon_ui::onboarding::OnboardingScreen;
use plaudon_ui::settings::{SettingsIntent, SettingsOutput};
use plaudon_ui_testkit::a11y;

const KEY: &str = "sk-or-v1-not-a-real-key";
const NO_ENGINE: &str = "No usable speech-to-text engine was detected yet.";

/// The screen under test, the capabilities it renders from, what it asked for,
/// and whether an Escape press survived the screen.
struct App {
    screen: OnboardingScreen,
    capabilities: Capabilities,
    out: SettingsOutput,
    escape_seen: bool,
}

impl App {
    fn view(&mut self, ui: &mut egui::Ui) {
        self.screen.ui(ui, &self.capabilities, &mut self.out);
        // Spec §5.3: setup is not an overlay. If the screen let Escape
        // through, the shell would act on it.
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.escape_seen = true;
        }
    }
}

fn harness_with(capabilities: Capabilities) -> Harness<'static, App> {
    plaudon_ui_testkit::builder().build_ui_state(
        |ui, app: &mut App| app.view(ui),
        App {
            screen: OnboardingScreen::new(),
            capabilities,
            out: SettingsOutput::new(),
            escape_seen: false,
        },
    )
}

fn harness() -> Harness<'static, App> {
    harness_with(capabilities(vec![
        stt_engine("whisper.cpp", "whisper.cpp", CapabilityStatus::Supported),
        text_engine(),
    ]))
}

fn notifications() -> NotificationSettings {
    let mut categories = BTreeMap::new();
    for category in NotificationCategory::ALL {
        categories.insert(*category, false);
    }
    categories.insert(NotificationCategory::ImportFinished, true);
    NotificationSettings {
        enabled: true,
        categories,
    }
}

fn preferences() -> Preferences {
    let mut preferences = plaudon_app::backend::default_preferences();
    preferences.notifications = notifications();
    preferences
}

fn stt_engine(
    engine_id: &str,
    display_name: &str,
    status: CapabilityStatus,
) -> EngineInventoryEntry {
    EngineInventoryEntry {
        engine_id: engine_id.to_owned(),
        display_name: display_name.to_owned(),
        stage: EngineStage::Stt,
        status,
        unavailable_reason: (status != CapabilityStatus::Supported)
            .then(|| "No whisper.cpp model is installed on this machine.".to_owned()),
        installed_models: vec![InstalledModel {
            model_id: "small.en".to_owned(),
            display_name: "Small (English)".to_owned(),
            size_bytes: 488_000_000,
            installed: true,
        }],
        download_bytes_required: Some(488_000_000),
        requires_network: false,
    }
}

fn text_engine() -> EngineInventoryEntry {
    EngineInventoryEntry {
        engine_id: "openrouter".to_owned(),
        display_name: "OpenRouter".to_owned(),
        stage: EngineStage::Text,
        status: CapabilityStatus::Unknown,
        unavailable_reason: Some("No OpenRouter key is present in the system keychain.".to_owned()),
        installed_models: Vec::new(),
        download_bytes_required: None,
        requires_network: true,
    }
}

fn capabilities(engines: Vec<EngineInventoryEntry>) -> Capabilities {
    let mut integrations = BTreeMap::new();
    integrations.insert("plaud_cloud".to_owned(), CapabilityStatus::Unknown);
    Capabilities {
        os: OsInfo {
            platform: Platform::Macos,
            version: "27.0".to_owned(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".to_owned(),
            logical_cores: 10,
            total_memory_bytes: 34_359_738_368,
        },
        engines,
        storage: StorageCapability {
            library_path: "/Users/someone/Library/Plaudon".to_owned(),
            free_bytes: 107_374_182_400,
            total_bytes: 536_870_912_000,
        },
        integrations,
        permissions: BTreeMap::new(),
        preferences: preferences(),
        openrouter_key_present: false,
        cloud_consents: Vec::new(),
    }
}

fn click(harness: &mut Harness<'_, App>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

/// Click the *button* with this name.
///
/// The last step's heading and its action share a name ("Finish setup"), so
/// the role decides which of the two is pressed.
fn click_button(harness: &mut Harness<'_, App>, label: &str) {
    let node = harness
        .query_all_by_label(label)
        .find(|node| node.accesskit_node().role() == egui::accesskit::Role::Button)
        .unwrap_or_else(|| panic!("no button is labelled {label:?}"));
    node.click();
    harness.run();
}

fn has(harness: &Harness<'_, App>, label: &str) -> bool {
    harness.query_all_by_label(label).next().is_some()
}

fn clear(harness: &mut Harness<'_, App>) {
    harness.state_mut().out = SettingsOutput::new();
}

fn command<'a>(harness: &'a Harness<'_, App>, name: &str) -> &'a AppCommand {
    harness
        .state()
        .out
        .intents
        .iter()
        .find_map(|intent| match intent {
            SettingsIntent::Command(command) if command.name() == name => Some(command),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {name} command was raised"))
}

fn respond(
    harness: &mut Harness<'_, App>,
    command: &'static str,
    result: &Result<AppResponse, ErrorEnvelope>,
) {
    let app = harness.state_mut();
    let mut out = SettingsOutput::new();
    app.screen.on_response(command, result, &mut out);
    harness.run();
}

/// Walk forward to `step`, pressing Next.
fn advance_to(harness: &mut Harness<'_, App>, step: OnboardingStep) {
    harness.run();
    let target = OnboardingStep::ALL
        .iter()
        .position(|candidate| *candidate == step)
        .expect("a known step");
    while harness.state().screen.state().step_index() < target {
        click(harness, "Next");
    }
    assert_eq!(harness.state().screen.state().step(), step);
}

#[test]
fn the_six_steps_appear_in_order_with_a_text_progress_label() {
    let mut harness = harness();
    harness.run();

    for (index, step) in OnboardingStep::ALL.iter().enumerate() {
        assert_eq!(harness.state().screen.state().step(), *step);
        // Progress is a sentence, not a bar on its own.
        harness.get_by_label(&format!("Step {} of 6", index + 1));
        assert!(has(&harness, step.title()), "{step:?} has no heading");
        if index < OnboardingStep::LAST_INDEX {
            click(&mut harness, "Next");
        }
    }

    // The last step ends the flow: there is nowhere further to go.
    assert!(!has(&harness, "Next"));
    click_button(&mut harness, "Finish setup");
    assert_eq!(
        harness.state().screen.state().step(),
        OnboardingStep::Finish
    );
}

#[test]
fn back_is_unavailable_on_the_first_step() {
    let mut harness = harness();
    harness.run();

    let disabled = harness.get_by_label("Back").accesskit_node().is_disabled();
    assert!(disabled, "Back must be unavailable before there is a back");

    click(&mut harness, "Next");
    assert!(!harness.get_by_label("Back").accesskit_node().is_disabled());
    click(&mut harness, "Back");
    assert_eq!(harness.state().screen.state().step_index(), 0);
}

#[test]
fn only_the_account_steps_can_be_skipped_and_skipping_is_recorded() {
    let mut harness = harness();
    harness.run();

    for step in OnboardingStep::ALL {
        assert_eq!(
            has(&harness, "Skip this step"),
            step.skippable(),
            "{step:?} offered the wrong skip affordance"
        );
        if *step == OnboardingStep::Key {
            break;
        }
        click(&mut harness, "Next");
    }

    click(&mut harness, "Skip this step");
    assert_eq!(
        harness.state().screen.state().step(),
        OnboardingStep::Sources
    );
    assert_eq!(
        harness.state().screen.state().skipped(),
        [OnboardingStep::Key]
    );
}

#[test]
fn choosing_cloud_raises_set_local_only_mode_with_an_explicit_confirmation() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Mode);
    assert!(
        harness.state().screen.state().local_only(),
        "the private posture is the default"
    );

    clear(&mut harness);
    click(
        &mut harness,
        "Allow cloud AI later — choosing this now grants nothing by itself. No request is ever \
sent until you pick a provider and consent for that purpose.",
    );

    assert_eq!(harness.state().out.command_names(), ["set_local_only_mode"]);
    let AppCommand::SetLocalOnlyMode(request) = command(&harness, "set_local_only_mode") else {
        panic!("wrong command variant");
    };
    assert!(!request.enabled);
    assert!(request.confirmed);
    assert!(!harness.state().screen.state().local_only());

    // The gate's answer, not the request, is what the flow carries forward.
    respond(
        &mut harness,
        "set_local_only_mode",
        &Ok(AppResponse::SetLocalOnlyMode(LocalOnlyModeResponse {
            enabled: false,
            description: "Outbound requests are allowed only to providers you consent to."
                .to_owned(),
            allowed_hosts: Vec::new(),
        })),
    );
    assert!(!harness.state().screen.state().local_only());
}

#[test]
fn saving_a_key_raises_the_command_and_leaves_no_trace_of_the_key() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Key);
    harness.get_by_label("– No key saved.");

    harness.get_by_label("OpenRouter API key").focus();
    harness.run();
    harness.get_by_label("OpenRouter API key").type_text(KEY);
    harness.run();

    clear(&mut harness);
    click(&mut harness, "Save key");

    assert_eq!(harness.state().out.command_names(), ["set_openrouter_key"]);
    let AppCommand::SetOpenRouterKey(request) = command(&harness, "set_openrouter_key") else {
        panic!("wrong command variant");
    };
    assert_eq!(request.key, KEY);

    for node in a11y::nodes(&harness) {
        if let Some(text) = node.announced() {
            assert!(!text.contains(KEY), "{text:?} announces the key");
        }
    }
    let rendered = format!("{:?}", harness.state().screen);
    assert!(!rendered.contains(KEY), "{rendered}");

    respond(
        &mut harness,
        "set_openrouter_key",
        &Ok(AppResponse::SetOpenRouterKey(StatusResponse::ok())),
    );
    harness.get_by_label("✓ Key saved to the system keychain.");
    assert!(harness.state().screen.state().key_saved());
}

#[test]
fn the_engines_step_reports_the_inventory_and_promises_no_download() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Engines);

    harness.get_by_label("whisper.cpp — Speech to text");
    harness.get_by_label("OpenRouter — Text");
    harness.get_by_label("✓ Supported");
    // The backend's own sentence about why an engine is unavailable.
    harness.get_by_label("No OpenRouter key is present in the system keychain.");
    harness.get_by_label(&format!(
        "Small (English) — {} · installed",
        format_bytes(Some(488_000_000))
    ));
    harness.get_by_label("Download size unknown.");
    // Local-only is the default posture, so the promise is the stronger one.
    harness.get_by_label(engines_download_note(true));

    clear(&mut harness);
    click(&mut harness, "whisper.cpp");
    assert_eq!(
        harness.state().screen.state().default_stt_engine_id(),
        Some("whisper.cpp")
    );
    assert!(
        harness.state().out.command_names().is_empty(),
        "choosing a default engine downloads nothing and commands nothing"
    );
}

#[test]
fn with_no_selectable_engine_the_step_says_so_and_offers_no_choice() {
    let mut harness = harness_with(capabilities(vec![stt_engine(
        "whisper.cpp",
        "whisper.cpp",
        CapabilityStatus::Unsupported,
    )]));
    advance_to(&mut harness, OnboardingStep::Engines);

    harness.get_by_label(NO_ENGINE);
    harness.get_by_label("✕ Unsupported");
    harness.get_by_label("No whisper.cpp model is installed on this machine.");
    assert!(
        !has(&harness, "whisper.cpp"),
        "an unsupported engine must not be offered as the default"
    );
    assert_eq!(harness.state().screen.state().default_stt_engine_id(), None);
}

#[test]
fn the_sources_step_renders_what_the_backend_reports() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Sources);

    harness.get_by_label("Import audio");
    // Reported, with its reported status: "unknown" is not "not shipped".
    harness.get_by_label("Connect PLAUD account (cloud)");
    harness.get_by_label("? Unknown");
    // Not reported at all, so the honest statement is that it is absent.
    harness.get_by_label("Connect PLAUD device (direct)");
    harness.get_by_label("– Not in this build");

    click(&mut harness, "Continue to import audio");
    assert_eq!(
        harness.state().screen.state().step(),
        OnboardingStep::Finish
    );
}

#[test]
fn finishing_carries_the_chosen_engine_the_chosen_mode_and_the_stored_notifications() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Mode);
    click(
        &mut harness,
        "Allow cloud AI later — choosing this now grants nothing by itself. No request is ever \
sent until you pick a provider and consent for that purpose.",
    );
    click(&mut harness, "Next");
    click(&mut harness, "whisper.cpp");
    advance_to(&mut harness, OnboardingStep::Finish);

    harness.get_by_label("Cloud AI allowed later, nothing granted yet");
    harness.get_by_label("whisper.cpp");

    clear(&mut harness);
    click_button(&mut harness, "Finish setup");

    assert_eq!(harness.state().out.command_names(), ["complete_onboarding"]);
    let AppCommand::CompleteOnboarding(request) = command(&harness, "complete_onboarding") else {
        panic!("wrong command variant");
    };
    assert_eq!(
        request.default_stt_engine_id.as_deref(),
        Some("whisper.cpp")
    );
    assert_eq!(request.default_text_engine_id, None);
    assert!(!request.local_only_mode);
    // Setup never turns a notification category on: the stored settings travel
    // through unchanged.
    assert_eq!(request.notifications, notifications());
}

#[test]
fn an_unchosen_engine_is_not_chosen_rather_than_empty() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Finish);
    harness.get_by_label("Not chosen");
}

#[test]
fn escape_does_not_discard_setup() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Engines);

    harness.key_press(egui::Key::Escape);
    harness.run();

    assert!(
        !harness.state().escape_seen,
        "Escape must not reach the shell from first-run setup"
    );
    assert_eq!(
        harness.state().screen.state().step(),
        OnboardingStep::Engines
    );
}

#[test]
fn a_failed_command_shows_the_envelopes_own_message() {
    let mut harness = harness();
    advance_to(&mut harness, OnboardingStep::Finish);
    let envelope = ErrorEnvelope {
        code: plaudon_domain::error::AppErrorCode::InternalError,
        message: "Preferences could not be written.".to_owned(),
        retryable: true,
        action: None,
        diagnostic_id: plaudon_domain::ids::DiagnosticId::new(),
    };
    respond(&mut harness, "complete_onboarding", &Err(envelope.clone()));

    harness.get_by_label(&format!("✕ {}", envelope.message));
}

#[test]
fn every_control_of_every_step_is_reachable_by_keyboard() {
    let mut harness = harness();
    harness.run();

    let expected: [(OnboardingStep, &[&str]); 6] = [
        (
            OnboardingStep::Privacy,
            &["What would leave this machine if you opt in later", "Next"],
        ),
        (
            OnboardingStep::Mode,
            &[
                "Local only (recommended) — all outbound network access is refused by the backend.",
                "Next",
                "Back",
            ],
        ),
        (OnboardingStep::Engines, &["whisper.cpp", "Next", "Back"]),
        (
            OnboardingStep::Key,
            &[
                "OpenRouter API key",
                "Save key",
                "Skip this step",
                "Next",
                "Back",
            ],
        ),
        (
            OnboardingStep::Sources,
            &["Continue to import audio", "Skip this step", "Next", "Back"],
        ),
        (OnboardingStep::Finish, &["Finish setup", "Back"]),
    ];

    for (step, labels) in expected {
        assert_eq!(harness.state().screen.state().step(), step);
        let order = a11y::focus_order(&mut harness, 24);
        for label in labels {
            assert!(
                order.iter().any(|reached| reached == label),
                "{label:?} is unreachable on {step:?}; Tab reached {order:?}"
            );
        }
        if step != OnboardingStep::Finish {
            click(&mut harness, "Next");
        }
    }
}
