//! Interaction tests for the engine, templates, export, shortcut, local-model
//! and update sections of the Settings screen (EPIC O task O2.4).
//!
//! Every assertion is made the way a screen reader or a keyboard user would
//! make it: find a control by its accessible name, press it, and then look at
//! what the section asked the host to do. No test reaches into a section's
//! private state.

use std::collections::BTreeMap;

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, EngineInventoryEntry, EngineStage, ExportFormat, HardwareInfo,
    InspectModelPackResponse, InstallModelFromPackRequest, ListLocalModelsResponse, LocalModelDto,
    ModelDiskUsageDto, ModelOfferDto, ModelOriginDto, OsInfo, Platform, RemoveLocalModelRequest,
    SetEnginePreferencesRequest, SetExportDefaultsRequest, SetShortcutsRequest, StorageCapability,
};
use plaudon_app::settings_presentation::{
    FootprintDisclosure, LicenceDisclosure, ModelInstallGate, UpdateReadiness, UpdateStatus,
    MODEL_SOURCE_NOTE,
};
use plaudon_app::shortcuts::{canonical_key, ShortcutSpec, KEY_NAMES, NON_BINDABLE_KEY_NAMES};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_ui::settings::engines::EnginesSection;
use plaudon_ui::settings::export::ExportSection;
use plaudon_ui::settings::models::LocalModelsSection;
use plaudon_ui::settings::shortcuts::ShortcutsSection;
use plaudon_ui::settings::templates::TemplatesSection;
use plaudon_ui::settings::updater::UpdateSection;
use plaudon_ui::settings::{
    FolderPurpose, PickedPath, Section, SettingsData, SettingsIntent, SettingsOutput, VisualsTokens,
};
use plaudon_ui_testkit::a11y;

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;

/* ------------------------------------------------------------- the harness */

/// One section, the cache it renders from, and everything it asked for.
struct App<S: Section> {
    section: S,
    data: SettingsData,
    out: SettingsOutput,
}

impl<S: Section> App<S> {
    fn view(&mut self, ui: &mut egui::Ui) {
        let tokens = VisualsTokens::from_ui(ui);
        self.section.ui(ui, &tokens, &self.data, &mut self.out);
    }

    fn command_names(&self) -> Vec<&'static str> {
        self.out.command_names()
    }
}

/// A tall window, so that a long section is laid out in one pass and every
/// control is in the AccessKit tree.
fn harness<S: Section + 'static>(section: S, data: SettingsData) -> Harness<'static, App<S>> {
    plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 3000.0))
        .build_ui_state(
            |ui, app: &mut App<S>| app.view(ui),
            App {
                section,
                data,
                out: SettingsOutput::new(),
            },
        )
}

/// Labels of every node that reports itself as a button.
fn button_labels<S: Section>(harness: &Harness<'_, App<S>>) -> Vec<String> {
    a11y::nodes(harness)
        .into_iter()
        .filter(|node| node.role == "Button")
        .filter_map(|node| node.label)
        .collect()
}

/// Whether any node announces text containing `needle`.
fn announces<S: Section>(harness: &Harness<'_, App<S>>, needle: &str) -> bool {
    a11y::nodes(harness)
        .iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .any(|text| text.contains(needle))
}

/* ------------------------------------------------------------- the fixtures */

fn engine(
    id: &str,
    name: &str,
    stage: EngineStage,
    status: CapabilityStatus,
) -> EngineInventoryEntry {
    EngineInventoryEntry {
        engine_id: id.to_owned(),
        display_name: name.to_owned(),
        stage,
        status,
        unavailable_reason: None,
        installed_models: Vec::new(),
        download_bytes_required: None,
        requires_network: false,
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        os: OsInfo {
            platform: Platform::Macos,
            version: "14.5".to_owned(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".to_owned(),
            logical_cores: 10,
            total_memory_bytes: 16 * GB,
        },
        engines: Vec::new(),
        storage: StorageCapability {
            library_path: "/Users/someone/Plaudon".to_owned(),
            free_bytes: 200 * GB,
            total_bytes: 500 * GB,
        },
        integrations: BTreeMap::new(),
        permissions: BTreeMap::new(),
        preferences: plaudon_app::backend::default_preferences(),
        openrouter_key_present: false,
        cloud_consents: Vec::new(),
    }
}

/// Two speech-to-text engines, one of them unsupported with the backend's own
/// reason, and one networked text engine.
fn engine_capabilities() -> Capabilities {
    let mut capabilities = capabilities();
    let mut whisper = engine(
        "whisper.cpp",
        "Whisper (local)",
        EngineStage::Stt,
        CapabilityStatus::Supported,
    );
    whisper.download_bytes_required = Some(142 * MB);
    let mut apple = engine(
        "apple.speech",
        "Apple Speech",
        EngineStage::Stt,
        CapabilityStatus::Unsupported,
    );
    apple.unavailable_reason =
        Some("Apple Speech needs macOS 14 or newer; this machine reports 13.5.".to_owned());
    let mut openrouter = engine(
        "openrouter",
        "OpenRouter",
        EngineStage::Text,
        CapabilityStatus::Supported,
    );
    openrouter.requires_network = true;
    capabilities.engines = vec![whisper, apple, openrouter];
    capabilities
}

fn engine_data() -> SettingsData {
    SettingsData {
        capabilities: Some(engine_capabilities()),
        ..SettingsData::default()
    }
}

fn specs() -> Vec<ShortcutSpec> {
    vec![
        ShortcutSpec::new("nav_library", "Library", "Navigation", "Mod+1"),
        ShortcutSpec::new("nav_jobs", "Jobs", "Navigation", "Mod+2"),
        ShortcutSpec::new("close_overlay", "Close overlay", "Overlays", "Escape").fixed(),
    ]
}

fn shortcut_data() -> SettingsData {
    SettingsData {
        shortcut_specs: specs(),
        ..SettingsData::default()
    }
}

fn installed_model() -> LocalModelDto {
    LocalModelDto {
        model_id: "whisper-small".to_owned(),
        display_name: "Whisper Small".to_owned(),
        engine: "whisper.cpp".to_owned(),
        version: "1.5.4".to_owned(),
        size_bytes: 466 * MB,
        licence_spdx_id: "MIT".to_owned(),
        origin: ModelOriginDto::Pack,
        keep_offline: true,
    }
}

fn local_models(available_disk_bytes: Option<u64>, total_ram_bytes: Option<u64>) -> SettingsData {
    SettingsData {
        local_models: Some(ListLocalModelsResponse {
            models: vec![installed_model()],
            disk: ModelDiskUsageDto {
                installed_bytes: 466 * MB,
                staging_bytes: 12 * MB,
            },
            model_root: "/Users/someone/Plaudon/models".to_owned(),
            available_disk_bytes,
            total_ram_bytes,
            source_note: MODEL_SOURCE_NOTE.to_owned(),
        }),
        ..SettingsData::default()
    }
}

fn offer(already_installed: bool) -> ModelOfferDto {
    ModelOfferDto {
        gate: ModelInstallGate {
            model_id: "whisper-tiny".to_owned(),
            display_name: "Whisper Tiny".to_owned(),
            licence: LicenceDisclosure {
                spdx_id: "MIT".to_owned(),
                name: "MIT License".to_owned(),
                redistributable: true,
                redistribution_note: "These weights may be redistributed inside an offline pack."
                    .to_owned(),
                text_file: "LICENSE.txt".to_owned(),
            },
            footprint: FootprintDisclosure {
                size_bytes: GB,
                required_bytes: GB + GB / 10,
                min_ram_bytes: 8 * GB,
                recommended_ram_bytes: 16 * GB,
                notes: "Runs on the CPU; a discrete GPU is not used.".to_owned(),
            },
            below_recommended_memory: false,
        },
        already_installed,
        languages: vec!["en".to_owned()],
        engine: "whisper.cpp".to_owned(),
        version: "1.5.4".to_owned(),
    }
}

fn pack(
    available_disk_bytes: Option<u64>,
    total_ram_bytes: Option<u64>,
) -> InspectModelPackResponse {
    InspectModelPackResponse {
        pack_path: "/Volumes/Packs/whisper".to_owned(),
        artifact_digest: "sha256:3f1c".to_owned(),
        schema_version: 1,
        offers: vec![offer(false)],
        available_disk_bytes,
        total_ram_bytes,
    }
}

/// A models section that has already inspected `pack`.
fn models_section(pack: InspectModelPackResponse) -> LocalModelsSection {
    let mut section = LocalModelsSection::default();
    let mut out = SettingsOutput::new();
    section.on_response(
        "inspect_model_pack",
        &Ok(AppResponse::InspectModelPack(pack)),
        &mut out,
    );
    section
}

fn update_data(checks_suppressed: bool, readiness: UpdateReadiness) -> SettingsData {
    SettingsData {
        update: Some(UpdateStatus {
            readiness,
            checks_suppressed,
            manifest_endpoint: "https://updates.plaudon.app/stable.json".to_owned(),
            signing_key_id: "plaudon-release-2025".to_owned(),
        }),
        ..SettingsData::default()
    }
}

/* ---------------------------------------------------------------- engines */

#[test]
fn an_unsupported_engine_is_listed_with_its_reason_and_without_a_radio() {
    let mut harness = harness(EnginesSection, engine_data());
    harness.run();

    // Listed, with the backend's own status word and its own reason.
    harness.get_by_label("Apple Speech");
    harness.get_by_label("✕ Unsupported");
    harness.get_by_label("Apple Speech needs macOS 14 or newer; this machine reports 13.5.");

    // But not choosable.
    assert!(harness
        .query_all_by_label("Use Apple Speech for transcription")
        .next()
        .is_none());
    // While a supported engine is.
    harness.get_by_label("Use Whisper (local) for transcription");
}

#[test]
fn an_unknown_download_size_says_so_rather_than_showing_a_number() {
    let mut harness = harness(EnginesSection, engine_data());
    harness.run();

    harness.get_by_label("Needs 142 MB, 200 GB free on this disk.");
    assert!(announces(&harness, "Download size unknown."));
}

#[test]
fn choosing_a_text_engine_carries_both_current_ids() {
    let mut data = engine_data();
    data.capabilities
        .as_mut()
        .expect("capabilities")
        .preferences
        .default_stt_engine_id = Some("whisper.cpp".to_owned());
    let mut harness = harness(EnginesSection, data);
    harness.run();

    harness
        .get_by_label("Use OpenRouter for summarization")
        .click();
    harness.run();

    assert_eq!(harness.state().command_names(), ["set_engine_preferences"]);
    let request = engine_request(&harness.state().out);
    assert_eq!(
        request.default_stt_engine_id.as_deref(),
        Some("whisper.cpp"),
        "the untouched stage must be carried, not cleared"
    );
    assert_eq!(
        request.default_text_engine_id.as_deref(),
        Some("openrouter")
    );
}

#[test]
fn the_application_choice_clears_a_preference() {
    let mut data = engine_data();
    data.capabilities
        .as_mut()
        .expect("capabilities")
        .preferences
        .default_stt_engine_id = Some("whisper.cpp".to_owned());
    let mut harness = harness(EnginesSection, data);
    harness.run();

    harness
        .get_by_label("Use the application's choice for transcription")
        .click();
    harness.run();

    let request = engine_request(&harness.state().out);
    assert_eq!(request.default_stt_engine_id, None);
}

#[test]
fn a_networked_engine_says_local_only_mode_will_not_make_a_request() {
    let mut data = engine_data();
    data.capabilities
        .as_mut()
        .expect("capabilities")
        .preferences
        .local_only_mode = true;
    let mut harness = harness(EnginesSection, data);
    harness.run();

    harness.get_by_label("Using this engine requires network access.");
    assert!(announces(&harness, "will not make a request"));
}

fn engine_request(out: &SettingsOutput) -> &SetEnginePreferencesRequest {
    out.intents
        .iter()
        .find_map(|intent| match intent {
            SettingsIntent::Command(AppCommand::SetEnginePreferences(request)) => Some(request),
            _ => None,
        })
        .expect("a set_engine_preferences command")
}

/* -------------------------------------------------------------- templates */

#[test]
fn the_templates_entry_points_at_the_screen_and_offers_no_control() {
    let mut harness = harness(TemplatesSection, SettingsData::default());
    harness.run();

    harness.get_by_label("Templates are managed on the Templates screen");
    assert!(announces(
        &harness,
        "lists the built-ins and your own copies"
    ));
    assert!(announces(&harness, "There is no default-template setting"));

    assert!(
        button_labels(&harness).is_empty(),
        "no control may be shown"
    );
    assert!(
        a11y::focus_order(&mut harness, 8).is_empty(),
        "nothing here is focusable, because nothing here is settable"
    );
}

/* ----------------------------------------------------------------- export */

fn export_data(destination: Option<&str>) -> SettingsData {
    let mut capabilities = capabilities();
    capabilities
        .preferences
        .export_defaults
        .destination_directory = destination.map(str::to_owned);
    SettingsData {
        capabilities: Some(capabilities),
        ..SettingsData::default()
    }
}

fn export_request(out: &SettingsOutput) -> &SetExportDefaultsRequest {
    out.intents
        .iter()
        .find_map(|intent| match intent {
            SettingsIntent::Command(AppCommand::SetExportDefaults(request)) => Some(request),
            _ => None,
        })
        .expect("a set_export_defaults command")
}

#[test]
fn changing_the_export_format_raises_the_command_and_srt_keeps_its_qualification() {
    let mut harness = harness(ExportSection::default(), export_data(None));
    harness.run();

    // The condition is part of the accessible name, not a disabled control.
    harness
        .get_by_label(
            "SubRip captions (.srt) — only available when the transcript has real timestamps",
        )
        .click();
    harness.run();

    assert_eq!(harness.state().command_names(), ["set_export_defaults"]);
    assert_eq!(
        export_request(&harness.state().out).defaults.format,
        ExportFormat::Srt
    );
    assert!(announces(
        &harness,
        "WebVTT captions (.vtt) — only available when the transcript has real timestamps"
    ));
}

#[test]
fn an_unset_destination_says_ask_every_time() {
    let mut harness = harness(ExportSection::default(), export_data(None));
    harness.run();

    harness.get_by_label("Ask every time");
    assert!(
        harness
            .query_all_by_label("Clear the default export folder")
            .next()
            .is_none(),
        "there is nothing to clear"
    );
}

#[test]
fn choosing_a_folder_asks_the_host_to_open_the_picker() {
    let mut harness = harness(ExportSection::default(), export_data(None));
    harness.run();

    harness.get_by_label("Choose folder").click();
    harness.run();

    assert!(harness.state().out.intents.iter().any(|intent| matches!(
        intent,
        SettingsIntent::PickFolder(FolderPurpose::ExportDestination)
    )));
    assert!(
        harness.state().command_names().is_empty(),
        "expected harness.state().command_names() to be empty, got {:?}",
        harness.state().command_names()
    );
}

#[test]
fn a_picked_folder_is_stored_as_the_default_destination() {
    let mut harness = harness(ExportSection::default(), export_data(None));
    harness.run();

    let mut out = SettingsOutput::new();
    harness.state_mut().section.on_path_picked(
        PickedPath::Folder(FolderPurpose::ExportDestination),
        "/Users/someone/Exports",
        &mut out,
    );

    assert_eq!(out.command_names(), ["set_export_defaults"]);
    assert_eq!(
        export_request(&out)
            .defaults
            .destination_directory
            .as_deref(),
        Some("/Users/someone/Exports")
    );
}

#[test]
fn clearing_the_destination_raises_the_command_without_it() {
    let mut harness = harness(
        ExportSection::default(),
        export_data(Some("/Users/someone/Exports")),
    );
    harness.run();
    harness.get_by_label("/Users/someone/Exports");

    harness
        .get_by_label("Clear the default export folder")
        .click();
    harness.run();

    assert_eq!(
        export_request(&harness.state().out)
            .defaults
            .destination_directory,
        None
    );
}

#[test]
fn toggling_a_column_raises_the_command_with_the_new_value() {
    let mut harness = harness(ExportSection::default(), export_data(None));
    harness.run();

    harness.get_by_label("Include timestamps").click();
    harness.run();

    let defaults = &export_request(&harness.state().out).defaults;
    assert!(!defaults.include_timestamps);
    assert!(defaults.include_speaker_labels, "the other column is kept");
}

/* -------------------------------------------------------------- shortcuts */

fn shortcut_request(out: &SettingsOutput) -> &SetShortcutsRequest {
    out.intents
        .iter()
        .find_map(|intent| match intent {
            SettingsIntent::Command(AppCommand::SetShortcuts(request)) => Some(request),
            _ => None,
        })
        .expect("a set_shortcuts command")
}

#[test]
fn an_empty_registry_says_so_instead_of_listing_nothing() {
    let mut harness = harness(ShortcutsSection::default(), SettingsData::default());
    harness.run();

    harness.get_by_label(
        "The shell has not published its command registry yet, so there are no shortcuts to list.",
    );
    assert!(
        button_labels(&harness).is_empty(),
        "expected button_labels(&harness) to be empty, got {:?}",
        button_labels(&harness)
    );
}

#[test]
fn the_list_renders_the_mac_spelling_and_marks_what_cannot_move() {
    let mut harness = harness(ShortcutsSection::default(), shortcut_data());
    harness.run();

    harness.get_by_label("Cmd+1");
    harness.get_by_label("Cmd+2");
    harness.get_by_label("Escape");
    assert!(announces(&harness, "cannot be reassigned"));
    assert!(
        harness
            .query_all_by_label("Change the shortcut for Close overlay")
            .next()
            .is_none(),
        "a fixed binding offers no control"
    );
}

#[test]
fn a_conflicting_rebind_is_refused_and_names_the_holder() {
    let mut harness = harness(ShortcutsSection::default(), shortcut_data());
    harness.run();

    harness.get_by_label("Change the shortcut for Jobs").click();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num1);
    harness.run();

    assert!(
        announces(&harness, "already assigned to Library"),
        "the refusal names the command that holds the chord"
    );
    assert!(
        harness.state().command_names().is_empty(),
        "nothing is written when the schema refuses"
    );
    harness.get_by_label("Cmd+2");
}

#[test]
fn escape_leaves_capture_mode_without_changing_anything() {
    let mut harness = harness(ShortcutsSection::default(), shortcut_data());
    harness.run();

    harness.get_by_label("Change the shortcut for Jobs").click();
    harness.run();
    assert!(announces(&harness, "Press the new shortcut for Jobs"));

    harness.key_press(egui::Key::Escape);
    harness.run();

    assert!(!announces(&harness, "Press the new shortcut for Jobs"));
    harness.get_by_label("Change the shortcut for Jobs");
    harness.get_by_label("Cmd+2");
    assert!(
        harness.state().command_names().is_empty(),
        "expected harness.state().command_names() to be empty, got {:?}",
        harness.state().command_names()
    );
}

#[test]
fn a_free_chord_is_accepted_and_written_as_an_override() {
    let mut harness = harness(ShortcutsSection::default(), shortcut_data());
    harness.run();

    harness.get_by_label("Change the shortcut for Jobs").click();
    harness.run();
    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::J,
    );
    harness.run();

    assert_eq!(harness.state().command_names(), ["set_shortcuts"]);
    let request = shortcut_request(&harness.state().out);
    assert_eq!(
        request.bindings.overrides().get("nav_jobs"),
        Some(&Some("Mod+Shift+J".to_owned()))
    );
    assert_eq!(request.specs.len(), 3, "the registry travels with the map");
}

#[test]
fn clearing_a_binding_stores_a_deliberate_none() {
    let mut harness = harness(ShortcutsSection::default(), shortcut_data());
    harness.run();

    harness.get_by_label("Clear the shortcut for Jobs").click();
    harness.run();

    assert_eq!(
        shortcut_request(&harness.state().out)
            .bindings
            .overrides()
            .get("nav_jobs"),
        Some(&None)
    );
}

#[test]
fn reset_all_raises_set_shortcuts_with_no_overrides() {
    let mut data = shortcut_data();
    data.shortcuts
        .rebind(&specs(), "nav_jobs", "Mod+Shift+J")
        .expect("a free chord");
    let mut harness = harness(ShortcutsSection::default(), data);
    harness.run();
    assert!(announces(&harness, "Customised"));

    harness.get_by_label("Reset all shortcuts").click();
    harness.run();

    assert_eq!(harness.state().command_names(), ["set_shortcuts"]);
    assert!(shortcut_request(&harness.state().out)
        .bindings
        .overrides()
        .is_empty());
}

#[test]
fn the_key_tables_still_match_egui() {
    for key in egui::Key::ALL {
        let name = key.name();
        // `Questionmark` is written `?`, which `canonical_key` resolves.
        let bindable = canonical_key(name).is_some();
        let non_bindable = NON_BINDABLE_KEY_NAMES.contains(&name);
        assert!(
            bindable != non_bindable,
            "egui::Key::{name} is in neither key table, or in both; \
             plaudon_app::shortcuts::KEY_NAMES has drifted from egui"
        );
    }
    for name in KEY_NAMES {
        assert!(
            !NON_BINDABLE_KEY_NAMES.contains(name),
            "{name} is both bindable and non-bindable"
        );
    }
}

/* ----------------------------------------------------------- local models */

fn install_request(out: &SettingsOutput) -> &InstallModelFromPackRequest {
    out.intents
        .iter()
        .find_map(|intent| match intent {
            SettingsIntent::Command(AppCommand::InstallModelFromPack(request)) => Some(request),
            _ => None,
        })
        .expect("an install_model_from_pack command")
}

fn remove_request(out: &SettingsOutput) -> &RemoveLocalModelRequest {
    out.intents
        .iter()
        .find_map(|intent| match intent {
            SettingsIntent::Command(AppCommand::RemoveLocalModel(request)) => Some(request),
            _ => None,
        })
        .expect("a remove_local_model command")
}

#[test]
fn unknown_free_disk_is_unknown_and_never_zero() {
    let mut harness = harness(LocalModelsSection::default(), local_models(None, None));
    harness.run();

    assert_eq!(
        harness.query_all_by_label("unknown").count(),
        2,
        "free disk and memory are both unmeasured here"
    );
    assert!(
        !announces(&harness, "0 bytes"),
        "an unmeasured quantity is never rendered as a zero"
    );
    assert!(announces(&harness, MODEL_SOURCE_NOTE));
}

#[test]
fn an_unaccepted_licence_blocks_the_install_and_every_blocker_is_explained() {
    let section = models_section(pack(Some(500 * MB), Some(4 * GB)));
    let mut harness = harness(section, local_models(Some(500 * MB), Some(4 * GB)));
    harness.run();

    // All three at once, in the application's words.
    assert!(announces(
        &harness,
        "Accept the MIT licence before installing."
    ));
    assert!(announces(
        &harness,
        "This install needs 1.1 GB including headroom; 500 MB is free."
    ));
    assert!(announces(
        &harness,
        "The model requires 8.0 GB of memory; this machine reports 4.0 GB."
    ));

    harness.get_by_label("Install Whisper Tiny").click();
    harness.run();
    assert!(
        !harness
            .state()
            .command_names()
            .contains(&"install_model_from_pack"),
        "a blocked install must not be dispatched"
    );
}

#[test]
fn accepting_both_gates_installs_with_the_declared_spdx_id() {
    let section = models_section(pack(Some(80 * GB), Some(32 * GB)));
    let mut harness = harness(section, local_models(Some(80 * GB), Some(32 * GB)));
    harness.run();

    harness.get_by_label("I accept the MIT licence").click();
    harness.run();
    harness
        .get_by_label("I understand this needs 1.1 GB of disk")
        .click();
    harness.run();

    harness.get_by_label("Install Whisper Tiny").click();
    // The indeterminate progress bar animates, so the frame loop never settles.
    harness.run_ok();

    assert_eq!(harness.state().command_names(), ["install_model_from_pack"]);
    let request = install_request(&harness.state().out);
    assert_eq!(request.model_id, "whisper-tiny");
    assert_eq!(request.licence_accepted_spdx_id, "MIT");
    assert!(request.footprint_acknowledged);
    assert_eq!(request.pack_path, "/Volumes/Packs/whisper");
}

#[test]
fn an_already_installed_offer_says_so() {
    let mut response = pack(Some(80 * GB), Some(32 * GB));
    response.offers = vec![offer(true)];
    let section = models_section(response);
    let mut harness = harness(section, local_models(Some(80 * GB), Some(32 * GB)));
    harness.run();

    assert!(announces(&harness, "This model is already installed."));
}

#[test]
fn removing_a_model_needs_the_confirmation() {
    let mut harness = harness(
        LocalModelsSection::default(),
        local_models(Some(80 * GB), Some(32 * GB)),
    );
    harness.run();

    harness.get_by_label("Remove Whisper Small").click();
    harness.run();
    assert!(
        harness.state().command_names().is_empty(),
        "the first press only opens the confirmation"
    );
    harness.get_by_label("Remove Whisper Small?");
    assert!(announces(
        &harness,
        "Recordings, transcripts and summaries are not touched."
    ));

    harness.get_by_label("Remove this model").click();
    harness.run();

    assert_eq!(harness.state().command_names(), ["remove_local_model"]);
    let request = remove_request(&harness.state().out);
    assert_eq!(request.model_id, "whisper-small");
    assert!(request.confirmed);
}

#[test]
fn choosing_a_pack_asks_for_a_folder_and_the_path_raises_an_inspection() {
    let mut harness = harness(
        LocalModelsSection::default(),
        local_models(Some(80 * GB), Some(32 * GB)),
    );
    harness.run();

    harness.get_by_label("Choose a model pack").click();
    harness.run();
    assert!(harness
        .state()
        .out
        .intents
        .iter()
        .any(|intent| matches!(intent, SettingsIntent::PickFolder(FolderPurpose::ModelPack))));

    let mut out = SettingsOutput::new();
    harness.state_mut().section.on_path_picked(
        PickedPath::Folder(FolderPurpose::ModelPack),
        "/Volumes/Packs/whisper",
        &mut out,
    );
    assert_eq!(out.command_names(), ["inspect_model_pack"]);
}

#[test]
fn verifying_a_model_shows_the_verifiers_own_sentence_and_re_lists() {
    let mut harness = harness(
        LocalModelsSection::default(),
        local_models(Some(80 * GB), Some(32 * GB)),
    );
    harness.run();

    harness.get_by_label("Verify Whisper Small").click();
    harness.run();
    assert_eq!(harness.state().command_names(), ["verify_local_model"]);

    let mut out = SettingsOutput::new();
    harness.state_mut().section.on_response(
        "verify_local_model",
        &Ok(AppResponse::VerifyLocalModel(
            plaudon_app::dto::VerifyLocalModelResponse {
                model_id: "whisper-small".to_owned(),
                intact: false,
                observed_bytes: 1,
                expected_bytes: 2,
                detail: "One weight file is shorter than recorded.".to_owned(),
            },
        )),
        &mut out,
    );
    assert_eq!(out.command_names(), ["list_local_models"]);

    harness.run();
    assert!(announces(
        &harness,
        "One weight file is shorter than recorded."
    ));
}

#[test]
fn an_install_with_no_reported_size_shows_no_percentage() {
    let section = models_section(pack(Some(80 * GB), Some(32 * GB)));
    let mut harness = harness(section, local_models(Some(80 * GB), Some(32 * GB)));
    harness.run();

    harness.get_by_label("I accept the MIT licence").click();
    harness.run();
    harness
        .get_by_label("I understand this needs 1.1 GB of disk")
        .click();
    harness.run();
    harness.get_by_label("Install Whisper Tiny").click();
    // An indeterminate bar animates: `run` would panic on the step budget.
    harness.run_ok();

    assert!(announces(&harness, "Installing Whisper Tiny"));
    assert!(announces(&harness, "Size not reported yet"));
    assert!(!announces(&harness, "%"));

    harness.state_mut().section.install_progress(512 * MB, GB);
    harness.run();
    assert!(announces(&harness, "512 MB of 1.0 GB"));
}

/* ---------------------------------------------------------------- updates */

#[test]
fn local_only_mode_suppression_is_stated_and_no_control_makes_a_request() {
    let status = UpdateStatus {
        readiness: UpdateReadiness::Ready {
            installed_version: "1.4.2".to_owned(),
        },
        checks_suppressed: true,
        manifest_endpoint: "https://updates.plaudon.app/stable.json".to_owned(),
        signing_key_id: "plaudon-release-2025".to_owned(),
    };
    let mut harness = harness(
        UpdateSection,
        update_data(
            true,
            UpdateReadiness::Ready {
                installed_version: "1.4.2".to_owned(),
            },
        ),
    );
    harness.run();

    harness.get_by_label(status.check_policy_copy());
    harness.get_by_label("1.4.2");
    harness.get_by_label("https://updates.plaudon.app/stable.json");
    harness.get_by_label("plaudon-release-2025");
    assert_eq!(button_labels(&harness), ["Copy the version"]);

    harness.get_by_label("Copy the version").click();
    harness.run();
    assert!(harness.state().out.intents.iter().any(|intent| matches!(
        intent,
        SettingsIntent::CopyText(text) if text == "1.4.2"
    )));
    assert!(
        harness.state().command_names().is_empty(),
        "expected harness.state().command_names() to be empty, got {:?}",
        harness.state().command_names()
    );
}

#[test]
fn a_build_the_updater_did_not_install_says_so_in_the_updaters_words() {
    let detail = "This copy was not installed by the updater, so there is no installation root to \
update."
        .to_owned();
    let mut harness = harness(
        UpdateSection,
        update_data(
            false,
            UpdateReadiness::NotInstalledByUpdater {
                detail: detail.clone(),
            },
        ),
    );
    harness.run();

    harness.get_by_label("⚠ Not installed by the updater");
    harness.get_by_label(detail.as_str());
    harness.get_by_label("Not managed by the updater");
}

/* ---------------------------------------------------------- keyboard reach */

#[test]
fn every_engine_control_is_reachable_by_keyboard() {
    let mut harness = harness(EnginesSection, engine_data());
    harness.run();
    let order = a11y::focus_order(&mut harness, 20);

    for label in [
        "Use the application's choice for transcription",
        "Use Whisper (local) for transcription",
        "Use the application's choice for summarization",
        "Use OpenRouter for summarization",
    ] {
        assert!(
            order.iter().any(|stop| stop == label),
            "{label} unreachable"
        );
    }
}

#[test]
fn every_export_control_is_reachable_by_keyboard() {
    let mut harness = harness(
        ExportSection::default(),
        export_data(Some("/Users/someone/Exports")),
    );
    harness.run();
    let order = a11y::focus_order(&mut harness, 24);

    for label in [
        "Plain text (.txt)",
        "Markdown (.md)",
        "JSON (.json)",
        "Choose folder",
        "Clear the default export folder",
        "Include speaker labels",
        "Include timestamps",
    ] {
        assert!(
            order.iter().any(|stop| stop == label),
            "{label} unreachable"
        );
    }
}

#[test]
fn every_shortcut_control_is_reachable_by_keyboard() {
    let mut data = shortcut_data();
    data.shortcuts
        .rebind(&specs(), "nav_jobs", "Mod+Shift+J")
        .expect("a free chord");
    let mut harness = harness(ShortcutsSection::default(), data);
    harness.run();
    let order = a11y::focus_order(&mut harness, 24);

    for label in [
        "Change the shortcut for Library",
        "Clear the shortcut for Library",
        "Change the shortcut for Jobs",
        "Reset the shortcut for Jobs to its default",
        "Reset all shortcuts",
    ] {
        assert!(
            order.iter().any(|stop| stop == label),
            "{label} unreachable"
        );
    }
}

#[test]
fn every_model_control_is_reachable_by_keyboard() {
    let section = models_section(pack(Some(80 * GB), Some(32 * GB)));
    let mut harness = harness(section, local_models(Some(80 * GB), Some(32 * GB)));
    harness.run();
    harness.get_by_label("I accept the MIT licence").click();
    harness.run();
    harness
        .get_by_label("I understand this needs 1.1 GB of disk")
        .click();
    harness.run();
    let order = a11y::focus_order(&mut harness, 32);

    for label in [
        "Show the model folder in the file manager",
        "Verify Whisper Small",
        "Remove Whisper Small",
        "Choose a model pack",
        "I accept the MIT licence",
        "I understand this needs 1.1 GB of disk",
        "Install Whisper Tiny",
    ] {
        assert!(
            order.iter().any(|stop| stop == label),
            "{label} unreachable"
        );
    }
}

#[test]
fn the_update_control_is_reachable_by_keyboard() {
    let mut harness = harness(
        UpdateSection,
        update_data(
            false,
            UpdateReadiness::Ready {
                installed_version: "1.4.2".to_owned(),
            },
        ),
    );
    harness.run();
    let order = a11y::focus_order(&mut harness, 8);

    assert!(order.iter().any(|stop| stop == "Copy the version"));
}
