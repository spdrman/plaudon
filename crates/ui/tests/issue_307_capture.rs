//! Issue #307: frames of the permissions panel for the pull request, in the
//! shell's light and dark palettes.
//!
//! Ignored by default: it only writes pictures and asserts nothing new. It runs
//! in the e2e container, through the software wgpu adapter the snapshot policy
//! trusts:
//!
//! ```text
//! scripts/e2e-remote.sh cargo test -p plaudon-ui --test issue_307_capture -- --ignored
//! ```
//!
//! Frames land in `tests/visual/issue_307/`, which the runner copies back.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::dto::{
    Capabilities, HardwareInfo, NotificationCategory, NotificationSettings, OsInfo, Platform,
    StorageCapability,
};
use plaudon_app::onboarding::OnboardingStep;
use plaudon_app::os_permissions::{OsPermissionKind, OsPermissionStatus, PermissionSnapshot};
use plaudon_ui::onboarding::OnboardingScreen;
use plaudon_ui::permissions::{manual_steps_label, PermissionsReport};
use plaudon_ui::settings::permissions::PermissionsSection;
use plaudon_ui::settings::{Section, SettingsData, SettingsOutput};
use plaudon_ui::theme::{ThemeKind, Tokens};

/// One size for every frame, so the GIF does not jump between them.
const FRAME: egui::Vec2 = egui::vec2(960.0, 1090.0);

fn report(statuses: [OsPermissionStatus; 4], error: Option<&str>) -> PermissionsReport {
    PermissionsReport {
        snapshot: Some(PermissionSnapshot::from_statuses(
            OsPermissionKind::ALL.iter().copied().zip(statuses),
        )),
        error: error.map(str::to_owned),
    }
}

fn fresh() -> PermissionsReport {
    report(
        [
            OsPermissionStatus::NotDetermined,
            OsPermissionStatus::NotDetermined,
            OsPermissionStatus::NotReported,
            OsPermissionStatus::NotReported,
        ],
        None,
    )
}

fn capabilities() -> Capabilities {
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
            platform: Platform::Macos,
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

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/visual/issue_307");
    std::fs::create_dir_all(&dir).expect("the frame directory");
    dir
}

fn save<S>(harness: &mut Harness<'_, S>, name: &str) {
    harness.run();
    let image = harness.render().expect("the frame renders");
    image
        .save(out_dir().join(format!("{name}.png")))
        .expect("the frame is written");
}

fn theme_name(theme: ThemeKind) -> &'static str {
    match theme {
        ThemeKind::Dark => "dark",
        _ => "light",
    }
}

fn preference(theme: ThemeKind) -> egui::ThemePreference {
    match theme {
        ThemeKind::Dark => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::Light,
    }
}

struct SetupApp {
    screen: OnboardingScreen,
    capabilities: Capabilities,
    out: SettingsOutput,
    tokens: Tokens,
}

#[test]
#[ignore = "writes the #307 pull-request frames; run it in the e2e container"]
fn issue_307_capture_frames() {
    for theme in [ThemeKind::Light, ThemeKind::Dark] {
        let tokens = Tokens::for_theme(theme);
        let suffix = theme_name(theme);

        // First-run setup, arriving at the permissions step.
        let mut screen = OnboardingScreen::for_platform(Platform::Macos);
        screen.set_os_permissions(fresh());
        let mut setup = plaudon_ui_testkit::builder()
            .with_size(FRAME)
            .build_ui_state(
                |ui, app: &mut SetupApp| {
                    app.screen
                        .ui_with_tokens(ui, &app.tokens, &app.capabilities, &mut app.out);
                },
                SetupApp {
                    screen,
                    capabilities: capabilities(),
                    out: SettingsOutput::new(),
                    tokens,
                },
            );
        plaudon_ui::theme::install(&setup.ctx, false, preference(theme));
        setup.run();
        while setup.state().screen.state().step() != OnboardingStep::Permissions {
            setup.get_by_label("Next").click();
            setup.run();
        }
        save(&mut setup, &format!("1-setup-not-asked-{suffix}"));

        // The numbered steps for Bluetooth, opened.
        setup
            .get_by_label(&manual_steps_label(OsPermissionKind::Bluetooth))
            .click();
        save(&mut setup, &format!("2-setup-manual-steps-{suffix}"));

        // After "Ask macOS": Bluetooth allowed, notifications refused.
        setup.state_mut().screen.set_os_permissions(report(
            [
                OsPermissionStatus::Granted,
                OsPermissionStatus::Denied,
                OsPermissionStatus::NotReported,
                OsPermissionStatus::NotReported,
            ],
            None,
        ));
        save(&mut setup, &format!("3-setup-answered-{suffix}"));

        // The same panel under Settings, with a failed System Settings open.
        let data = SettingsData {
            capabilities: Some(capabilities()),
            os_permissions: report(
                [
                    OsPermissionStatus::Granted,
                    OsPermissionStatus::Granted,
                    OsPermissionStatus::NotReported,
                    OsPermissionStatus::Restricted,
                ],
                Some(
                    "System Settings could not be opened. Open it from the Apple menu and go to \
Privacy & Security, then Files and Folders.",
                ),
            ),
            ..SettingsData::default()
        };
        let mut section = PermissionsSection::default();
        let mut out = SettingsOutput::new();
        let mut settings = plaudon_ui_testkit::builder()
            .with_size(FRAME)
            .build_ui(move |ui| section.ui(ui, &tokens, &data, &mut out));
        plaudon_ui::theme::install(&settings.ctx, false, preference(theme));
        save(&mut settings, &format!("4-settings-{suffix}"));
    }
}
