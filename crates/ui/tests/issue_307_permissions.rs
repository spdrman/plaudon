//! Issue #307: the guided macOS Privacy & Security panel, in first-run setup
//! and under Settings.
//!
//! Every test runs against [`FakePermissionProvider`] or a hand-built
//! [`PermissionsReport`]. Nothing here asks the real operating system for
//! anything: a real TCC prompt needs a person to click it, so what is
//! asserted is what the panel shows for each status and what it asks the
//! host to do when a button is pressed.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use plaudon_app::dto::{
    Capabilities, HardwareInfo, NotificationCategory, NotificationSettings, OsInfo, Platform,
    StorageCapability,
};
use plaudon_app::onboarding::OnboardingStep;
use plaudon_app::os_permissions::{
    FakeCall, FakePermissionProvider, OsPermissionKind, OsPermissionStatus, PermissionSnapshot,
    NOTHING_TO_SET_UP, SIGNING_NOTE,
};
use plaudon_ui::onboarding::{OnboardingScreen, SKIP_LATER};
use plaudon_ui::permissions::{
    ask_label, handle_intent, manual_steps_label, open_label, PermissionsHost, PermissionsPanel,
    PermissionsReport, CHECK_AGAIN,
};
use plaudon_ui::settings::permissions::{PermissionsSection, TITLE};
use plaudon_ui::settings::{
    Section, SettingsData, SettingsIntent, SettingsOutput, SettingsScreen, VisualsTokens,
};
use plaudon_ui_testkit::a11y;

/* ---------------------------------------------------------------- fixtures */

fn report(statuses: &[(OsPermissionKind, OsPermissionStatus)]) -> PermissionsReport {
    PermissionsReport {
        snapshot: Some(PermissionSnapshot::from_statuses(statuses.iter().copied())),
        error: None,
    }
}

/// Bluetooth refused, notifications allowed, and the two folder permissions
/// macOS has no read-only API for.
fn mixed() -> PermissionsReport {
    report(&[
        (OsPermissionKind::Bluetooth, OsPermissionStatus::Denied),
        (OsPermissionKind::Notifications, OsPermissionStatus::Granted),
        (
            OsPermissionKind::RemovableVolumes,
            OsPermissionStatus::NotReported,
        ),
        (
            OsPermissionKind::FilesAndFolders,
            OsPermissionStatus::NotReported,
        ),
    ])
}

/// Nothing asked yet anywhere macOS can tell.
fn fresh() -> PermissionsReport {
    report(&[
        (
            OsPermissionKind::Bluetooth,
            OsPermissionStatus::NotDetermined,
        ),
        (
            OsPermissionKind::Notifications,
            OsPermissionStatus::NotDetermined,
        ),
        (
            OsPermissionKind::RemovableVolumes,
            OsPermissionStatus::NotReported,
        ),
        (
            OsPermissionKind::FilesAndFolders,
            OsPermissionStatus::NotReported,
        ),
    ])
}

fn capabilities(platform: Platform) -> Capabilities {
    let mut categories = BTreeMap::new();
    for category in NotificationCategory::ALL {
        categories.insert(*category, false);
    }
    let mut preferences = plaudon_app::backend::default_preferences();
    preferences.notifications = NotificationSettings {
        enabled: false,
        categories,
    };
    Capabilities {
        os: OsInfo {
            platform,
            version: "27.0".to_owned(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".to_owned(),
            logical_cores: 10,
            total_memory_bytes: 34_359_738_368,
        },
        engines: Vec::new(),
        storage: StorageCapability {
            library_path: "/Users/someone/Library/Plaudon".to_owned(),
            free_bytes: 107_374_182_400,
            total_bytes: 536_870_912_000,
        },
        integrations: BTreeMap::new(),
        permissions: BTreeMap::new(),
        preferences,
        openrouter_key_present: false,
        cloud_consents: Vec::new(),
    }
}

/* ----------------------------------------------------------- panel harness */

struct PanelApp {
    panel: PermissionsPanel,
    report: PermissionsReport,
    out: SettingsOutput,
}

impl PanelApp {
    fn view(&mut self, ui: &mut egui::Ui) {
        let tokens = VisualsTokens::from_ui(ui);
        self.panel.ui(ui, &tokens, &self.report, &mut self.out);
    }
}

fn panel(platform: Platform, report: PermissionsReport) -> Harness<'static, PanelApp> {
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 2000.0))
        .build_ui_state(
            |ui, app: &mut PanelApp| app.view(ui),
            PanelApp {
                panel: PermissionsPanel::new(platform),
                report,
                out: SettingsOutput::new(),
            },
        );
    harness.run();
    harness
}

/// What a frame asked the host to do, reduced to the permission intents
/// (`SettingsIntent` carries commands, which do not compare).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asked {
    Check,
    Ask(OsPermissionKind),
    Open(OsPermissionKind),
    Other,
}

fn asked(out: &SettingsOutput) -> Vec<Asked> {
    out.intents
        .iter()
        .map(|intent| match intent {
            SettingsIntent::CheckOsPermissions => Asked::Check,
            SettingsIntent::AskOsPermission(kind) => Asked::Ask(*kind),
            SettingsIntent::OpenOsPermissionSettings(kind) => Asked::Open(*kind),
            _ => Asked::Other,
        })
        .collect()
}

fn panel_intents(harness: &Harness<'_, PanelApp>) -> Vec<Asked> {
    asked(&harness.state().out)
}

fn forget(harness: &mut Harness<'_, PanelApp>) {
    harness.state_mut().out = SettingsOutput::new();
}

fn has<S>(harness: &Harness<'_, S>, label: &str) -> bool {
    harness.query_all_by_label(label).next().is_some()
}

fn says<S>(harness: &Harness<'_, S>, fragment: &str) -> bool {
    a11y::nodes(harness)
        .iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .any(|text| text.contains(fragment))
}

fn click<S>(harness: &mut Harness<'_, S>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

fn click_button<S>(harness: &mut Harness<'_, S>, label: &str) {
    let node = harness
        .query_all_by_label(label)
        .find(|node| node.accesskit_node().role() == egui::accesskit::Role::Button)
        .unwrap_or_else(|| panic!("no button is labelled {label:?}"));
    node.click();
    harness.run();
}

/* ------------------------------------------------------------------ panel */

#[test]
fn issue_307_the_panel_lists_every_macos_permission_with_its_purpose() {
    let harness = panel(Platform::Macos, mixed());
    for kind in OsPermissionKind::for_platform(Platform::Macos) {
        let guide = kind.guide();
        assert!(has(&harness, guide.title), "{kind:?} has no heading");
        assert!(
            says(&harness, guide.needed_for),
            "{kind:?} never says what needs it"
        );
        assert!(
            says(&harness, guide.without_it),
            "{kind:?} never says what happens without it"
        );
    }
    assert!(
        !says(&harness, "Full Disk Access"),
        "no feature needs Full Disk Access, so the panel must not send anyone there"
    );
}

#[test]
fn issue_307_each_status_is_a_glyph_and_a_word_from_the_live_report() {
    let harness = panel(Platform::Macos, mixed());
    for status in [
        OsPermissionStatus::Denied,
        OsPermissionStatus::Granted,
        OsPermissionStatus::NotReported,
    ] {
        let (glyph, word) = status.label();
        assert!(
            says(&harness, &format!("{glyph} {word}")),
            "{status:?} is not shown as {glyph} {word}"
        );
    }
    let fresh = panel(Platform::Macos, fresh());
    let (glyph, word) = OsPermissionStatus::NotDetermined.label();
    assert!(says(&fresh, &format!("{glyph} {word}")));
    assert!(
        !says(&fresh, OsPermissionStatus::Denied.label().1),
        "never asked must not be shown as a refusal"
    );
}

#[test]
fn issue_307_ask_macos_is_offered_only_where_there_is_an_api_and_a_prompt_left() {
    let harness = panel(Platform::Macos, fresh());
    assert!(has(&harness, &ask_label(OsPermissionKind::Bluetooth)));
    assert!(has(&harness, &ask_label(OsPermissionKind::Notifications)));
    assert!(!has(
        &harness,
        &ask_label(OsPermissionKind::RemovableVolumes)
    ));
    assert!(!has(
        &harness,
        &ask_label(OsPermissionKind::FilesAndFolders)
    ));

    // Once refused, macOS never shows the prompt again, so a button that
    // promises one would be a lie: only System Settings can change it now.
    let refused = panel(Platform::Macos, mixed());
    assert!(!has(&refused, &ask_label(OsPermissionKind::Bluetooth)));
    assert!(!has(&refused, &ask_label(OsPermissionKind::Notifications)));
    assert!(has(&refused, &open_label(OsPermissionKind::Bluetooth)));
}

#[test]
fn issue_307_pressing_ask_macos_raises_exactly_that_request() {
    let mut harness = panel(Platform::Macos, fresh());
    forget(&mut harness);
    click(&mut harness, &ask_label(OsPermissionKind::Bluetooth));
    assert_eq!(
        panel_intents(&harness),
        [Asked::Ask(OsPermissionKind::Bluetooth)]
    );
}

#[test]
fn issue_307_every_permission_has_a_button_for_its_exact_system_settings_pane() {
    let mut harness = panel(Platform::Macos, mixed());
    for kind in OsPermissionKind::for_platform(Platform::Macos) {
        forget(&mut harness);
        click(&mut harness, &open_label(*kind));
        assert_eq!(panel_intents(&harness), [Asked::Open(*kind)], "{kind:?}");
    }
}

#[test]
fn issue_307_the_manual_steps_are_numbered_and_one_click_away() {
    let mut harness = panel(Platform::Macos, mixed());
    let kind = OsPermissionKind::Bluetooth;
    click(&mut harness, &manual_steps_label(kind));
    for (index, step) in kind.guide().steps.iter().enumerate() {
        assert!(
            says(&harness, &format!("{}. {step}", index + 1)),
            "step {} of {kind:?} is not shown numbered",
            index + 1
        );
    }
}

#[test]
fn issue_307_the_panel_reads_once_on_first_show_and_again_on_check_again() {
    let mut harness = panel(Platform::Macos, PermissionsReport::default());
    assert_eq!(
        panel_intents(&harness),
        [Asked::Check],
        "the first frame reads the statuses, once"
    );
    harness.run_steps(3);
    assert_eq!(panel_intents(&harness).len(), 1, "and only once");

    forget(&mut harness);
    click_button(&mut harness, CHECK_AGAIN);
    assert_eq!(panel_intents(&harness), [Asked::Check]);
}

#[test]
fn issue_307_coming_back_to_the_app_checks_again() {
    let mut harness = panel(Platform::Macos, mixed());
    forget(&mut harness);
    // The user went to System Settings and came back.
    harness.event(egui::Event::WindowFocused(false));
    harness.run();
    harness.event(egui::Event::WindowFocused(true));
    harness.run();
    assert_eq!(panel_intents(&harness), [Asked::Check]);
}

#[test]
fn issue_307_the_ad_hoc_signature_caveat_is_always_shown() {
    let harness = panel(Platform::Macos, mixed());
    assert!(says(&harness, SIGNING_NOTE));
}

#[test]
fn issue_307_a_failed_settings_open_is_shown_as_a_sentence() {
    let harness = panel(
        Platform::Macos,
        PermissionsReport {
            error: Some("System Settings could not be opened.".to_owned()),
            ..mixed()
        },
    );
    assert!(says(&harness, "System Settings could not be opened."));
}

#[test]
fn issue_307_off_macos_the_panel_has_nothing_to_set_up_and_asks_nothing() {
    for platform in [Platform::Windows, Platform::Linux] {
        let harness = panel(platform, PermissionsReport::default());
        assert!(says(&harness, NOTHING_TO_SET_UP), "{platform:?}");
        assert!(!has(&harness, CHECK_AGAIN), "{platform:?}");
        assert!(
            panel_intents(&harness).is_empty(),
            "{platform:?} has no permission to read: {:?}",
            panel_intents(&harness)
        );
    }
}

#[test]
fn issue_307_every_control_is_named() {
    let harness = panel(Platform::Macos, fresh());
    a11y::assert_every_control_is_named(&harness, "permissions panel", &[]);
}

/* ------------------------------------------------------------------- host */

#[test]
fn issue_307_the_host_reads_asks_and_opens_through_the_provider() {
    let fake = Arc::new(FakePermissionProvider::new());
    fake.set_status(OsPermissionKind::Notifications, OsPermissionStatus::Granted);
    fake.answer_prompt_with(OsPermissionKind::Bluetooth, OsPermissionStatus::Granted);
    let host = PermissionsHost::inline(fake.clone(), Platform::Macos);

    assert!(host.latest().snapshot.is_none(), "nothing read yet");
    assert!(handle_intent(
        &host,
        &SettingsIntent::CheckOsPermissions,
        None
    ));
    let snapshot = host.latest().snapshot.expect("a read happened");
    assert_eq!(
        snapshot.status(OsPermissionKind::Bluetooth),
        Some(OsPermissionStatus::NotDetermined)
    );
    assert_eq!(
        snapshot.status(OsPermissionKind::Notifications),
        Some(OsPermissionStatus::Granted)
    );
    assert!(
        fake.calls()
            .iter()
            .all(|call| matches!(call, FakeCall::Status(_))),
        "checking never prompts: {:?}",
        fake.calls()
    );

    assert!(handle_intent(
        &host,
        &SettingsIntent::AskOsPermission(OsPermissionKind::Bluetooth),
        None
    ));
    assert!(fake
        .calls()
        .contains(&FakeCall::Request(OsPermissionKind::Bluetooth)));
    assert_eq!(
        host.latest()
            .snapshot
            .and_then(|snapshot| snapshot.status(OsPermissionKind::Bluetooth)),
        Some(OsPermissionStatus::Granted),
        "the answer to the prompt is what the panel shows next"
    );

    assert!(handle_intent(
        &host,
        &SettingsIntent::OpenOsPermissionSettings(OsPermissionKind::FilesAndFolders),
        None
    ));
    assert!(fake
        .calls()
        .contains(&FakeCall::OpenSettings(OsPermissionKind::FilesAndFolders)));
    assert_eq!(host.latest().error, None);
}

#[test]
fn issue_307_a_failed_open_lands_in_the_report_and_clears_on_the_next_success() {
    let fake = Arc::new(FakePermissionProvider::new());
    fake.fail_open_settings("System Settings could not be opened.");
    let host = PermissionsHost::inline(fake.clone(), Platform::Macos);
    handle_intent(
        &host,
        &SettingsIntent::OpenOsPermissionSettings(OsPermissionKind::Bluetooth),
        None,
    );
    assert_eq!(
        host.latest().error.as_deref(),
        Some("System Settings could not be opened.")
    );
    handle_intent(&host, &SettingsIntent::CheckOsPermissions, None);
    assert_eq!(host.latest().error, None);
}

#[test]
fn issue_307_the_host_leaves_every_other_intent_alone() {
    let fake = Arc::new(FakePermissionProvider::new());
    let host = PermissionsHost::inline(fake.clone(), Platform::Macos);
    assert!(!handle_intent(
        &host,
        &SettingsIntent::RefreshCapabilities,
        None
    ));
    assert_eq!(fake.calls(), Vec::<FakeCall>::new());
}

#[test]
fn issue_307_off_macos_the_host_never_calls_the_provider() {
    let fake = Arc::new(FakePermissionProvider::new());
    let host = PermissionsHost::inline(fake.clone(), Platform::Windows);
    handle_intent(&host, &SettingsIntent::CheckOsPermissions, None);
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(
        host.latest()
            .snapshot
            .map(|snapshot| snapshot.iter().count()),
        Some(0)
    );
}

/* ---------------------------------------------------------------- settings */

struct SectionApp {
    section: PermissionsSection,
    data: SettingsData,
    out: SettingsOutput,
}

fn section(data: SettingsData) -> Harness<'static, SectionApp> {
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 2000.0))
        .build_ui_state(
            |ui, app: &mut SectionApp| {
                let tokens = VisualsTokens::from_ui(ui);
                app.section.ui(ui, &tokens, &app.data, &mut app.out);
            },
            SectionApp {
                section: PermissionsSection::default(),
                data,
                out: SettingsOutput::new(),
            },
        );
    harness.run();
    harness
}

#[test]
fn issue_307_settings_lists_the_permissions_section() {
    let titles = SettingsScreen::new().section_titles();
    assert!(titles.contains(&TITLE), "{titles:?}");
}

#[test]
fn issue_307_settings_shows_the_same_panel_as_setup() {
    let harness = section(SettingsData {
        capabilities: Some(capabilities(Platform::Macos)),
        os_permissions: mixed(),
        ..SettingsData::default()
    });
    for kind in OsPermissionKind::for_platform(Platform::Macos) {
        assert!(has(&harness, &open_label(*kind)), "{kind:?}");
    }
    assert!(has(&harness, CHECK_AGAIN));
    assert!(says(&harness, SIGNING_NOTE));
    assert!(harness
        .state()
        .out
        .intents
        .iter()
        .any(|intent| matches!(intent, SettingsIntent::CheckOsPermissions)));
}

#[test]
fn issue_307_settings_waits_for_capabilities_before_guessing_the_platform() {
    let harness = section(SettingsData::default());
    assert!(!has(&harness, CHECK_AGAIN));
    assert!(harness.state().out.intents.is_empty());
}

#[test]
fn issue_307_settings_on_windows_has_nothing_to_set_up() {
    let harness = section(SettingsData {
        capabilities: Some(capabilities(Platform::Windows)),
        ..SettingsData::default()
    });
    assert!(says(&harness, NOTHING_TO_SET_UP));
    assert!(harness.state().out.intents.is_empty());
}

/* -------------------------------------------------------------- onboarding */

struct SetupApp {
    screen: OnboardingScreen,
    capabilities: Capabilities,
    out: SettingsOutput,
}

fn setup(report: PermissionsReport) -> Harness<'static, SetupApp> {
    let mut screen = OnboardingScreen::for_platform(Platform::Macos);
    screen.set_os_permissions(report);
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 2000.0))
        .build_ui_state(
            |ui, app: &mut SetupApp| app.screen.ui(ui, &app.capabilities, &mut app.out),
            SetupApp {
                screen,
                capabilities: capabilities(Platform::Macos),
                out: SettingsOutput::new(),
            },
        );
    harness.run();
    while harness.state().screen.state().step() != OnboardingStep::Permissions {
        click_button(&mut harness, "Next");
    }
    harness
}

#[test]
fn issue_307_setup_shows_the_panel_on_its_own_step() {
    let harness = setup(fresh());
    assert!(has(&harness, OnboardingStep::Permissions.title()));
    assert!(has(&harness, "Step 6 of 7"));
    assert!(has(&harness, &ask_label(OsPermissionKind::Bluetooth)));
    assert!(has(
        &harness,
        &open_label(OsPermissionKind::FilesAndFolders)
    ));
    assert!(has(&harness, CHECK_AGAIN));
    assert!(says(&harness, SIGNING_NOTE));
    assert!(harness
        .state()
        .out
        .intents
        .iter()
        .any(|intent| matches!(intent, SettingsIntent::CheckOsPermissions)));
}

#[test]
fn issue_307_setup_never_prompts_before_the_permissions_step() {
    let mut screen = OnboardingScreen::for_platform(Platform::Macos);
    screen.set_os_permissions(fresh());
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 2000.0))
        .build_ui_state(
            |ui, app: &mut SetupApp| app.screen.ui(ui, &app.capabilities, &mut app.out),
            SetupApp {
                screen,
                capabilities: capabilities(Platform::Macos),
                out: SettingsOutput::new(),
            },
        );
    harness.run();
    while harness.state().screen.state().step() != OnboardingStep::Permissions {
        assert!(
            !harness.state().out.intents.iter().any(|intent| matches!(
                intent,
                SettingsIntent::AskOsPermission(_) | SettingsIntent::CheckOsPermissions
            )),
            "{:?} touched permissions",
            harness.state().screen.state().step()
        );
        click_button(&mut harness, "Next");
    }
}

#[test]
fn issue_307_skip_do_later_moves_on_without_blocking_and_is_recorded() {
    let mut harness = setup(mixed());
    click_button(&mut harness, SKIP_LATER);
    assert_eq!(
        harness.state().screen.state().step(),
        OnboardingStep::Finish
    );
    assert_eq!(
        harness.state().screen.state().skipped(),
        &[OnboardingStep::Permissions]
    );
    assert!(
        !harness
            .state()
            .out
            .intents
            .iter()
            .any(|intent| matches!(intent, SettingsIntent::AskOsPermission(_))),
        "leaving the step for later must never prompt"
    );
}

#[test]
fn issue_307_next_is_never_held_back_by_a_refused_permission() {
    let mut harness = setup(report(&[
        (OsPermissionKind::Bluetooth, OsPermissionStatus::Denied),
        (OsPermissionKind::Notifications, OsPermissionStatus::Denied),
        (
            OsPermissionKind::RemovableVolumes,
            OsPermissionStatus::Restricted,
        ),
        (
            OsPermissionKind::FilesAndFolders,
            OsPermissionStatus::Restricted,
        ),
    ]));
    click_button(&mut harness, "Next");
    assert_eq!(
        harness.state().screen.state().step(),
        OnboardingStep::Finish
    );
}
