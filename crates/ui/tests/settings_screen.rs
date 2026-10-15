//! The Settings destination as a whole: section order, the reads it issues,
//! how responses are folded in and how a picked path reaches the section that
//! asked for it.
//!
//! The per-section behaviour is covered by `settings_privacy_group.rs`,
//! `settings_engines_group.rs` and `settings_backup.rs`; this suite is about
//! the composition, which is the part that can silently lose a section.

use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Appearance, Capabilities, Contrast, ExportDefaults, HardwareInfo, ListLocalModelsResponse,
    LocalOnlyModeResponse, ModelDiskUsageDto, NotificationSettings, OsInfo, Platform, Preferences,
    StorageCapability,
};
use plaudon_app::settings_presentation::{UpdateReadiness, UpdateStatus};
use plaudon_app::shortcuts::{ShortcutBindings, ShortcutSpec};
use plaudon_ui::settings::{
    FolderPurpose, PickedPath, SettingsIntent, SettingsOutput, SettingsScreen,
};
use plaudon_ui_testkit::a11y;
use plaudon_ui_testkit::kittest::Queryable as _;

struct App {
    screen: SettingsScreen,
    out: SettingsOutput,
}

impl App {
    fn new() -> Self {
        Self {
            screen: SettingsScreen::new(),
            out: SettingsOutput::new(),
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        self.screen.ui(ui, &mut self.out);
    }
}

fn preferences() -> Preferences {
    let mut categories = std::collections::BTreeMap::new();
    for category in plaudon_app::dto::NotificationCategory::ALL {
        categories.insert(*category, false);
    }
    Preferences {
        onboarding_completed: true,
        local_only_mode: true,
        default_stt_engine_id: None,
        default_text_engine_id: None,
        appearance: Appearance::System,
        contrast: Contrast::Standard,
        notifications: NotificationSettings {
            enabled: false,
            categories,
        },
        export_defaults: ExportDefaults::default(),
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        os: OsInfo {
            platform: Platform::Macos,
            version: "26.0".to_owned(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".to_owned(),
            logical_cores: 10,
            total_memory_bytes: 32 * 1024 * 1024 * 1024,
        },
        engines: Vec::new(),
        storage: StorageCapability {
            library_path: "/Users/a/Library/Application Support/Plaudon".to_owned(),
            free_bytes: 42 * 1024 * 1024 * 1024,
            total_bytes: 500 * 1024 * 1024 * 1024,
        },
        integrations: std::collections::BTreeMap::new(),
        permissions: std::collections::BTreeMap::new(),
        preferences: preferences(),
        openrouter_key_present: false,
        cloud_consents: Vec::new(),
    }
}

#[test]
fn the_screen_opens_by_reading_exactly_what_its_sections_render() {
    let names: Vec<&str> = SettingsScreen::initial_commands()
        .iter()
        .map(AppCommand::name)
        .collect();
    assert_eq!(
        names,
        [
            "get_capabilities",
            "get_shortcuts",
            "list_local_models",
            "get_update_status",
            // The Privacy section's voice-profile block (VP-090) renders
            // from this one, and it is off by default until it answers.
            "get_voice_profile_settings"
        ]
    );
}

#[test]
fn every_section_is_listed_once_and_in_the_published_order() {
    let screen = SettingsScreen::new();
    let titles = screen.section_titles();
    let mut sorted = titles.clone();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "a section title is used twice");
    assert_eq!(
        titles.len(),
        15,
        "a section was added or lost without updating this test: {titles:?}"
    );
    assert_eq!(
        titles.first().copied(),
        Some("Appearance"),
        "appearance is what a first-time reader looks for"
    );
}

#[test]
fn every_section_heading_is_in_the_accessibility_tree() {
    let app = App::new();
    let expected: Vec<&'static str> = app.screen.section_titles();
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut App| app.view(ui), app);
    harness.run();
    for title in expected {
        assert!(
            a11y::has_label(&harness, title),
            "the {title} section has no heading in the accessibility tree"
        );
    }
    assert!(a11y::has_label(&harness, "Settings"));
}

#[test]
fn a_capabilities_answer_reaches_every_section_that_renders_from_it() {
    let app = App::new();
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut App| app.view(ui), app);
    harness.run();
    assert!(
        a11y::has_label(&harness, "Storage facts are not available yet."),
        "before the answer, the screen says it does not know"
    );

    harness.state_mut().screen.on_response(
        "get_capabilities",
        &Ok(AppResponse::GetCapabilities(capabilities())),
        &mut SettingsOutput::new(),
    );
    harness.run();
    assert!(
        !a11y::has_label(&harness, "Storage facts are not available yet."),
        "the storage facts replace the not-yet notice"
    );
    assert!(harness
        .query_all_by_label_contains("42 GB")
        .next()
        .is_some());
}

#[test]
fn the_enforced_policy_from_the_gate_is_cached_for_the_privacy_section() {
    let mut screen = SettingsScreen::new();
    let mut out = SettingsOutput::new();
    screen.on_response(
        "set_local_only_mode",
        &Ok(AppResponse::SetLocalOnlyMode(LocalOnlyModeResponse {
            enabled: true,
            description: "All outbound network access is refused.".to_owned(),
            allowed_hosts: Vec::new(),
        })),
        &mut out,
    );
    let policy = screen
        .data()
        .enforced_policy
        .as_ref()
        .expect("the gate's answer is kept");
    assert_eq!(
        policy.description,
        "All outbound network access is refused."
    );
}

#[test]
fn a_preferences_answer_updates_the_cached_capabilities_in_place() {
    let mut screen = SettingsScreen::new();
    let mut out = SettingsOutput::new();
    screen.on_response(
        "get_capabilities",
        &Ok(AppResponse::GetCapabilities(capabilities())),
        &mut out,
    );
    let mut changed = preferences();
    changed.appearance = Appearance::Dark;
    screen.on_response(
        "set_appearance",
        &Ok(AppResponse::SetAppearance(changed)),
        &mut out,
    );
    assert_eq!(
        screen.data().preferences().map(|p| p.appearance),
        Some(Appearance::Dark),
        "the screen must not need a second get_capabilities to be current"
    );
}

#[test]
fn shortcut_and_model_and_update_answers_are_all_absorbed() {
    let mut screen = SettingsScreen::new();
    let mut out = SettingsOutput::new();
    let specs = vec![ShortcutSpec::new(
        "nav_library",
        "Go to Library",
        "Navigation",
        "Mod+1",
    )];
    screen.data_mut().shortcut_specs = specs.clone();

    let mut bindings = ShortcutBindings::defaults();
    bindings
        .rebind(&specs, "nav_library", "Mod+9")
        .expect("Mod+9 is free");
    screen.on_response(
        "get_shortcuts",
        &Ok(AppResponse::GetShortcuts(bindings)),
        &mut out,
    );
    assert_eq!(
        screen.data().shortcuts.chord(&specs[0]).as_deref(),
        Some("Mod+9")
    );

    screen.on_response(
        "list_local_models",
        &Ok(AppResponse::ListLocalModels(ListLocalModelsResponse {
            models: Vec::new(),
            disk: ModelDiskUsageDto {
                installed_bytes: 0,
                staging_bytes: 0,
            },
            model_root: "/tmp/models".to_owned(),
            available_disk_bytes: None,
            total_ram_bytes: None,
            source_note: "offline model pack only".to_owned(),
        })),
        &mut out,
    );
    assert!(screen.data().local_models.is_some());

    screen.on_response(
        "get_update_status",
        &Ok(AppResponse::GetUpdateStatus(UpdateStatus {
            readiness: UpdateReadiness::Ready {
                installed_version: "0.1.0".to_owned(),
            },
            checks_suppressed: true,
            manifest_endpoint: "https://example.invalid/m".to_owned(),
            signing_key_id: "k1".to_owned(),
        })),
        &mut out,
    );
    assert_eq!(
        screen
            .data()
            .update
            .as_ref()
            .map(UpdateStatus::installed_version_copy),
        Some("0.1.0".to_owned())
    );
}

#[test]
fn a_rejected_read_leaves_the_cache_empty_rather_than_inventing_a_value() {
    let mut screen = SettingsScreen::new();
    let mut out = SettingsOutput::new();
    let envelope = plaudon_domain::error::ErrorEnvelope {
        code: plaudon_domain::error::AppErrorCode::StorageUnavailable,
        message: "The library could not be read.".to_owned(),
        retryable: true,
        action: None,
        diagnostic_id: plaudon_domain::ids::DiagnosticId::new(),
    };
    screen.on_response("get_capabilities", &Err(envelope), &mut out);
    assert!(screen.data().capabilities.is_none());
    assert!(screen.data().preferences().is_none());
}

#[test]
fn a_picked_folder_reaches_the_section_that_asked_for_it() {
    let app = App::new();
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut App| app.view(ui), app);
    harness.state_mut().screen.on_response(
        "get_capabilities",
        &Ok(AppResponse::GetCapabilities(capabilities())),
        &mut SettingsOutput::new(),
    );
    // A section only amends what it has actually drawn, so the screen has to
    // have rendered a frame before a dialog result means anything.
    harness.run();
    let app = harness.state_mut();
    let _ = app.out.drain();

    app.screen.path_picked(
        PickedPath::Folder(FolderPurpose::ExportDestination),
        "/Users/a/Exports",
        &mut app.out,
    );
    assert_eq!(
        app.out.command_names(),
        ["set_export_defaults"],
        "only the export section answers to its own purpose"
    );

    let _ = app.out.drain();
    app.screen.path_picked(
        PickedPath::Folder(FolderPurpose::ModelPack),
        "/Users/a/pack",
        &mut app.out,
    );
    assert_eq!(app.out.command_names(), ["inspect_model_pack"]);
}

#[test]
fn every_folder_and_file_purpose_names_its_dialog() {
    for purpose in [
        FolderPurpose::BackupDestination,
        FolderPurpose::ExportDestination,
        FolderPurpose::ModelPack,
        FolderPurpose::WatchedFolder,
    ] {
        assert!(!purpose.dialog_title().is_empty(), "{purpose:?}");
    }
    assert_eq!(
        plaudon_ui::settings::FilePurpose::RestoreArchive.extensions(),
        ["plaudon"]
    );
}

#[test]
fn an_intent_that_is_not_a_command_is_not_counted_as_one() {
    let mut out = SettingsOutput::new();
    out.push(SettingsIntent::RefreshCapabilities);
    out.push(SettingsIntent::CopyText("sha256:abc".to_owned()));
    assert_eq!(out.command_names(), Vec::<&str>::new());
    assert_eq!(out.drain().len(), 2);
    assert!(out.is_empty(), "expected out to be empty, got {out:?}");
}

#[test]
fn the_whole_screen_is_walkable_from_the_keyboard() {
    let mut app = App::new();
    app.screen.on_response(
        "get_capabilities",
        &Ok(AppResponse::GetCapabilities(capabilities())),
        &mut SettingsOutput::new(),
    );
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut App| app.view(ui), app);
    harness.run();
    let order = a11y::focus_order(&mut harness, 40);
    assert!(
        order.len() > 5,
        "the settings screen must expose more than a handful of stops: {order:?}"
    );
    assert!(
        order.iter().any(|label| label.contains("Local-only mode")),
        "the privacy toggle must be reachable by Tab: {order:?}"
    );
}
