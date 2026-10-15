//! Issue #303 and #305: the rendered evidence for the PR, and the contrast of
//! every colour pair the new controls paint with.
//!
//! The frames are rendered through the software wgpu adapter on MARS
//! (`docs/EGUI_TESTING.md` §4) into `tests/visual/issue_303/`, where the
//! remote runner collects them. They are not baselines: nothing compares
//! against them, so the test is ignored unless asked for:
//!
//! ```text
//! scripts/e2e-remote.sh cargo test -p plaudon-ui --test issue_303_evidence -- --ignored
//! ```
//!
//! The contrast test is not ignored: it measures the shell's real tokens in
//! all four themes and fails if a pair the engine setup uses drops below
//! WCAG AA.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::Color32;
use plaudon_app::command::AppResponse;
use plaudon_app::dto::{
    Capabilities, CatalogueOfferDto, EngineInventoryEntry, EngineStage, HardwareInfo,
    LocalOnlyModeResponse, OsInfo, Platform, PlaudToolchainStatus, StorageCapability, ToolPresence,
};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog};
use plaudon_domain::time::Timestamp;
use plaudon_ui::onboarding::OnboardingScreen;
use plaudon_ui::settings::{SettingsOutput, SettingsTokens};
use plaudon_ui::theme::{contrast_ratio, style_for, ThemeKind, Tokens};
use plaudon_ui::widgets::error_notice::error_notice;

const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

fn offer(model_id: &str, name: &str, file: &str, size_bytes: u64) -> CatalogueOfferDto {
    CatalogueOfferDto {
        model_id: model_id.to_owned(),
        display_name: name.to_owned(),
        size_bytes,
        licence_spdx_id: "MIT".to_owned(),
        licence_name: "MIT License".to_owned(),
        host: "huggingface.co".to_owned(),
        file: format!("/ggerganov/whisper.cpp/resolve/{REVISION}/{file}"),
        delivery_host_suffix: Some("hf.co".to_owned()),
        recommended_ram_bytes: 4 << 30,
        installed: false,
    }
}

fn capabilities() -> Capabilities {
    let mut integrations = BTreeMap::new();
    integrations.insert("plaud_cloud".to_owned(), CapabilityStatus::Unknown);
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
        engines: vec![
            EngineInventoryEntry {
                engine_id: "whisper.cpp".to_owned(),
                display_name: "whisper.cpp (local)".to_owned(),
                stage: EngineStage::Stt,
                status: CapabilityStatus::Unknown,
                unavailable_reason: Some(
                    "No whisper.cpp model is installed on this machine.".to_owned(),
                ),
                installed_models: Vec::new(),
                download_bytes_required: Some(77_691_713),
                requires_network: false,
                catalogue: vec![
                    offer(
                        "whisper-tiny",
                        "Whisper tiny (multilingual)",
                        "ggml-tiny.bin",
                        77_691_713,
                    ),
                    offer(
                        "whisper-base-en",
                        "Whisper base (English)",
                        "ggml-base.en.bin",
                        147_964_211,
                    ),
                    offer(
                        "whisper-small-en",
                        "Whisper small (English)",
                        "ggml-small.en.bin",
                        487_614_201,
                    ),
                ],
            },
            EngineInventoryEntry {
                engine_id: "apple.speech".to_owned(),
                display_name: "Apple Speech (on device)".to_owned(),
                stage: EngineStage::Stt,
                status: CapabilityStatus::Unsupported,
                unavailable_reason: Some(
                    "Apple Speech is not available in this build: the Apple bridge is not linked."
                        .to_owned(),
                ),
                installed_models: Vec::new(),
                download_bytes_required: None,
                requires_network: false,
                catalogue: Vec::new(),
            },
        ],
        storage: StorageCapability {
            library_path: "/Users/someone/Library/Plaudon".to_owned(),
            free_bytes: 100 << 30,
            total_bytes: 500 << 30,
        },
        integrations,
        permissions: BTreeMap::new(),
        preferences: plaudon_app::backend::default_preferences(),
        openrouter_key_present: false,
        cloud_consents: Vec::new(),
    }
}

fn toolchain() -> PlaudToolchainStatus {
    PlaudToolchainStatus {
        node: ToolPresence {
            program: "node".to_owned(),
            path: Some("/opt/homebrew/bin/node".to_owned()),
        },
        npm: ToolPresence {
            program: "npm".to_owned(),
            path: Some("/opt/homebrew/bin/npm".to_owned()),
        },
        plaud: ToolPresence {
            program: "plaud".to_owned(),
            path: None,
        },
        plaud_version: None,
        exercised_release: "0.3.15".to_owned(),
        release_exercised: None,
        missing: vec!["plaud".to_owned()],
        install_command:
            "npm install --global --registry=https://registry.npmjs.org/ @plaud-ai/cli@0.3.15"
                .to_owned(),
        login_command: "plaud login".to_owned(),
        guided_install_available: true,
        summary: "Not found on this machine: the PLAUD CLI. PLAUD Cloud needs Node.js, npm and \
                  the PLAUD CLI."
            .to_owned(),
    }
}

/* ------------------------------------------------------------- frames */

struct App {
    screen: OnboardingScreen,
    capabilities: Capabilities,
    out: SettingsOutput,
    kind: ThemeKind,
}

fn frames_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/visual/issue_303");
    std::fs::create_dir_all(&dir).expect("the frames directory");
    dir
}

fn harness(kind: ThemeKind) -> egui_kittest::Harness<'static, App> {
    let theme = match kind {
        ThemeKind::Light | ThemeKind::HighContrastLight => egui::Theme::Light,
        ThemeKind::Dark | ThemeKind::HighContrastDark => egui::Theme::Dark,
    };
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1000.0, 1500.0))
        .with_theme(theme)
        .build_ui_state(
            |ui, app: &mut App| {
                let tokens = Tokens::for_theme(app.kind);
                egui::Frame::new()
                    .fill(tokens.color.bg_canvas)
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        app.screen
                            .ui_with_tokens(ui, &tokens, &app.capabilities, &mut app.out);
                    });
            },
            App {
                screen: OnboardingScreen::new(),
                capabilities: capabilities(),
                out: SettingsOutput::new(),
                kind,
            },
        );
    harness.ctx.set_style_of(theme, style_for(kind));
    harness.run();
    harness
}

fn save(harness: &mut egui_kittest::Harness<'_, App>, name: &str) {
    harness.run();
    let image = harness.render().expect("the frame renders");
    image
        .save(frames_dir().join(format!("{name}.png")))
        .expect("the frame is written");
}

fn click(harness: &mut egui_kittest::Harness<'_, App>, label: &str) {
    use egui_kittest::kittest::Queryable as _;
    harness.get_by_label(label).click();
    harness.run();
}

fn to_engines(harness: &mut egui_kittest::Harness<'_, App>) {
    click(harness, "Next");
    click(harness, "Next");
}

fn choose_cloud_later(harness: &mut egui_kittest::Harness<'_, App>) {
    click(harness, "Next");
    click(
        harness,
        "Allow cloud AI later — choosing this now grants nothing by itself. No request is ever \
sent until you pick a provider and consent for that purpose.",
    );
    let app = harness.state_mut();
    let mut out = SettingsOutput::new();
    app.screen.on_response(
        "set_local_only_mode",
        &Ok(AppResponse::SetLocalOnlyMode(LocalOnlyModeResponse {
            enabled: false,
            description: String::new(),
            allowed_hosts: Vec::new(),
        })),
        &mut out,
    );
    click(harness, "Next");
}

fn detected(harness: &mut egui_kittest::Harness<'_, App>) {
    let app = harness.state_mut();
    let mut out = SettingsOutput::new();
    app.screen.on_response(
        "detect_plaud_toolchain",
        &Ok(AppResponse::DetectPlaudToolchain(toolchain())),
        &mut out,
    );
    harness.run();
}

#[test]
#[ignore = "renders the PR evidence frames; run it on MARS with --ignored"]
fn issue_303_render_evidence_frames() {
    for (kind, suffix) in [(ThemeKind::Light, "light"), (ThemeKind::Dark, "dark")] {
        // 1. Step 3 with Local-only Mode on: downloads off and saying why,
        //    the import live, the PLAUD CLI detected.
        let mut harness = harness(kind);
        to_engines(&mut harness);
        detected(&mut harness);
        save(&mut harness, &format!("01-step3-local-only-{suffix}"));

        // 2. Local-only Mode off: the downloads are live and name the host.
        let mut harness = self::harness(kind);
        choose_cloud_later(&mut harness);
        detected(&mut harness);
        save(&mut harness, &format!("02-step3-downloads-{suffix}"));

        // 3. The first click asks, naming the exact host and file.
        click(&mut harness, "Download Whisper tiny (multilingual)");
        save(&mut harness, &format!("03-approve-download-{suffix}"));

        // 4. Approved: measured progress and a cancel.
        click(&mut harness, "Approve this download");
        harness
            .state_mut()
            .screen
            .install_progress(38_845_856, 77_691_713);
        save(&mut harness, &format!("04-downloading-{suffix}"));

        // 5. Done: announced, and the inventory is re-read.
        let app = harness.state_mut();
        let mut out = SettingsOutput::new();
        let model = plaudon_app::dto::LocalModelDto {
            model_id: "whisper-tiny".to_owned(),
            display_name: "Whisper tiny (multilingual)".to_owned(),
            engine: "whisper.cpp".to_owned(),
            version: "whisper-tiny".to_owned(),
            size_bytes: 77_691_713,
            licence_spdx_id: "MIT".to_owned(),
            origin: plaudon_app::dto::ModelOriginDto::Download,
            keep_offline: false,
        };
        app.screen.on_response(
            "install_model_download",
            &Ok(AppResponse::InstallModelDownload(
                plaudon_app::dto::InstallModelResponse {
                    model,
                    transferred_bytes: 77_691_713,
                    disk: plaudon_app::dto::ModelDiskUsageDto {
                        installed_bytes: 77_691_713,
                        staging_bytes: 0,
                    },
                },
            )),
            &mut out,
        );
        if let Some(whisper) = app.capabilities.engines.first_mut() {
            whisper.catalogue[0].installed = true;
        }
        save(&mut harness, &format!("05-installed-{suffix}"));

        // 6. The guided PLAUD CLI install asks first, naming the registry.
        click(&mut harness, "Install the PLAUD CLI");
        save(&mut harness, &format!("06-approve-cli-install-{suffix}"));
    }
}

#[test]
#[ignore = "renders the PR evidence frames; run it on MARS with --ignored"]
fn issue_305_render_error_frames() {
    for (kind, theme, suffix) in [
        (ThemeKind::Light, egui::Theme::Light, "light"),
        (ThemeKind::Dark, egui::Theme::Dark, "dark"),
    ] {
        let mut log = DiagnosticLog::new();
        let envelope = AppError::new(AppErrorCode::NoSpeechModel, "no local speech engine")
            .to_envelope(&mut log, Timestamp::now());
        let mut harness = plaudon_ui_testkit::builder()
            .with_size(egui::vec2(720.0, 220.0))
            .with_theme(theme)
            .build_ui_state(
                move |ui, envelope: &mut plaudon_domain::error::ErrorEnvelope| {
                    let tokens = Tokens::for_theme(kind);
                    egui::Frame::new()
                        .fill(tokens.color.bg_canvas)
                        .inner_margin(16.0)
                        .show(ui, |ui| {
                            ui.set_min_size(ui.available_size());
                            let _ = error_notice(ui, envelope, "Transcription could not start.");
                        });
                },
                envelope,
            );
        harness.ctx.set_style_of(theme, style_for(kind));
        harness.run();
        let image = harness.render().expect("the frame renders");
        image
            .save(frames_dir().join(format!("07-no-speech-model-error-{suffix}.png")))
            .expect("the frame is written");
    }
}

/* ------------------------------------------------------------ contrast */

/// Black or white on the accent, the rule `section::primary_button` uses.
fn on_accent(accent: Color32) -> Color32 {
    let [r, g, b, _] = accent.to_array();
    let luminance = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if luminance > 140.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

#[test]
fn issue_303_every_colour_pair_the_engine_setup_paints_meets_wcag_aa() {
    let mut table = Vec::new();
    let mut failures = Vec::new();
    for kind in ThemeKind::ALL.iter().copied() {
        let tokens = Tokens::for_theme(kind);
        let canvas = tokens.color.bg_canvas;
        let surface = SettingsTokens::surface(&tokens);
        let accent = SettingsTokens::accent(&tokens);
        // (name, foreground, background, minimum): text is normal-size, so
        // 4.5:1; the status badges and notes are text too.
        let pairs = [
            ("text on canvas", SettingsTokens::text(&tokens), canvas, 4.5),
            (
                "text on surface",
                SettingsTokens::text(&tokens),
                surface,
                4.5,
            ),
            (
                "note on canvas",
                SettingsTokens::muted_text(&tokens),
                canvas,
                4.5,
            ),
            (
                "note on surface",
                SettingsTokens::muted_text(&tokens),
                surface,
                4.5,
            ),
            (
                "success on canvas",
                SettingsTokens::success(&tokens),
                canvas,
                4.5,
            ),
            (
                "danger on canvas",
                SettingsTokens::danger(&tokens),
                canvas,
                4.5,
            ),
            ("button label on accent", on_accent(accent), accent, 4.5),
            ("accent button on canvas", accent, canvas, 3.0),
        ];
        for (name, fg, bg, minimum) in pairs {
            let ratio = contrast_ratio(fg, bg);
            table.push(format!("{kind:?}: {name} = {ratio:.2}:1"));
            if ratio < minimum {
                failures.push(format!("{kind:?}: {name} = {ratio:.2}:1 < {minimum}:1"));
            }
        }
    }
    println!("{}", table.join("\n"));
    assert!(
        failures.is_empty(),
        "{failures:#?}\n\nall pairs:\n{}",
        table.join("\n")
    );
}
