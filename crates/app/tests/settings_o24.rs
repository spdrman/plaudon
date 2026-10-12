//! End-to-end tests for the settings commands EPIC O task O2.4 added.
//!
//! Driven through [`AppEngine`] and [`Dispatcher::call_blocking`], the same
//! path the egui shell takes, so a command that validates in isolation but is
//! wired to the wrong port fails here.

use std::sync::Arc;

use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Appearance, Contrast, ExportDefaults, ExportFormat, InspectModelPackRequest,
    InstallModelFromPackRequest, RemoveLocalModelRequest, SetAppearanceRequest,
    SetEnginePreferencesRequest, SetExportDefaultsRequest, SetShortcutsRequest,
    VerifyLocalModelRequest,
};
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app::settings_presentation::UpdateReadiness;
use plaudon_app::shortcuts::{ShortcutBindings, ShortcutSpec};
use plaudon_app_core::models::{
    HardwareRecommendation, ModelEntry, ModelId, ModelLicense, ModelSource, PackFile, PackManifest,
    PACK_SCHEMA_VERSION,
};
use plaudon_domain::media::Sha256Digest;

const WEIGHTS: &[u8] = b"offline whisper weights, redistributable";
const LICENSE: &str = "MIT License\n\nPermission is hereby granted...\n";

struct Harness {
    engine: AppEngine,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = AppEngine::start(
            EngineConfig::new(dir.path().to_path_buf())
                .with_secrets(Arc::new(InMemorySecretStore::new()))
                .with_notifier(Arc::new(UnsupportedNotifier))
                .with_worker_threads(2),
        )
        .expect("engine starts");
        Self { engine, _dir: dir }
    }

    fn call(&self, command: AppCommand) -> AppResponse {
        self.engine
            .dispatcher()
            .call_blocking(command)
            .expect("command succeeds")
    }

    fn reject(&self, command: AppCommand) -> plaudon_domain::error::ErrorEnvelope {
        self.engine
            .dispatcher()
            .call_blocking(command)
            .expect_err("command is refused")
    }

    fn preferences(&self) -> plaudon_app::dto::Preferences {
        match self.call(AppCommand::GetCapabilities) {
            AppResponse::GetCapabilities(capabilities) => capabilities.preferences,
            other => panic!("unexpected response: {other:?}"),
        }
    }
}

fn model_entry() -> ModelEntry {
    ModelEntry {
        id: ModelId::parse("tiny-en").expect("id"),
        engine: "whisper.cpp".into(),
        version: "v1".into(),
        display_name: "Whisper Tiny (English)".into(),
        file_name: "ggml-tiny.en.bin".into(),
        size_bytes: WEIGHTS.len() as u64,
        sha256: Sha256Digest::of(WEIGHTS),
        languages: Vec::new(),
        license: ModelLicense {
            spdx_id: "MIT".into(),
            name: "MIT License".into(),
            redistributable: true,
            text_file: "MIT.txt".into(),
        },
        source: ModelSource::Pack {
            entry_path: "models/whisper.cpp/v1/ggml-tiny.en.bin".into(),
        },
        hardware: HardwareRecommendation {
            min_ram_bytes: 1,
            recommended_ram_bytes: 2,
            notes: "runs anywhere".into(),
        },
    }
}

/// A redistributable offline pack on disk, which is the only install path
/// this build offers.
fn write_pack(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("models/whisper.cpp/v1")).expect("mkdir");
    std::fs::create_dir_all(root.join("licenses")).expect("mkdir");
    std::fs::write(root.join("models/whisper.cpp/v1/ggml-tiny.en.bin"), WEIGHTS).expect("weights");
    std::fs::write(root.join("licenses/MIT.txt"), LICENSE).expect("license");
    let manifest = PackManifest {
        schema_version: PACK_SCHEMA_VERSION,
        pack_version: "2026.10.1".into(),
        models: vec![model_entry()],
        files: vec![
            PackFile {
                path: "models/whisper.cpp/v1/ggml-tiny.en.bin".into(),
                size_bytes: WEIGHTS.len() as u64,
                sha256: Sha256Digest::of(WEIGHTS),
            },
            PackFile {
                path: "licenses/MIT.txt".into(),
                size_bytes: LICENSE.len() as u64,
                sha256: Sha256Digest::of(LICENSE.as_bytes()),
            },
        ],
    };
    std::fs::write(
        root.join("pack.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialise"),
    )
    .expect("manifest");
}

fn specs() -> Vec<ShortcutSpec> {
    vec![
        ShortcutSpec::new("nav_library", "Go to Library", "Navigation", "Mod+1"),
        ShortcutSpec::new("search_library", "Search", "Application", "Mod+Shift+F"),
    ]
}

/* ------------------------------------------------------------- appearance */

#[test]
fn appearance_and_contrast_are_persisted_and_reported_by_capabilities() {
    let harness = Harness::start();
    assert_eq!(harness.preferences().appearance, Appearance::System);

    let AppResponse::SetAppearance(preferences) =
        harness.call(AppCommand::SetAppearance(SetAppearanceRequest {
            appearance: Appearance::Dark,
            contrast: Contrast::High,
        }))
    else {
        panic!("wrong response");
    };
    assert_eq!(preferences.appearance, Appearance::Dark);
    assert_eq!(preferences.contrast, Contrast::High);

    let stored = harness.preferences();
    assert_eq!(stored.appearance, Appearance::Dark);
    assert_eq!(
        stored.contrast,
        Contrast::High,
        "the choice must survive a round trip, not live in the view"
    );
}

/* ----------------------------------------------------------- engine prefs */

#[test]
fn an_engine_preference_naming_an_engine_this_machine_lacks_is_refused() {
    let harness = Harness::start();
    let envelope = harness.reject(AppCommand::SetEnginePreferences(
        SetEnginePreferencesRequest {
            default_stt_engine_id: Some("no.such.engine".to_owned()),
            default_text_engine_id: None,
        },
    ));
    assert_eq!(
        envelope.code,
        plaudon_domain::error::AppErrorCode::InvalidArgument
    );
    assert_eq!(harness.preferences().default_stt_engine_id, None);
}

#[test]
fn clearing_both_engine_preferences_is_always_allowed() {
    let harness = Harness::start();
    let AppResponse::SetEnginePreferences(preferences) = harness.call(
        AppCommand::SetEnginePreferences(SetEnginePreferencesRequest {
            default_stt_engine_id: None,
            default_text_engine_id: None,
        }),
    ) else {
        panic!("wrong response");
    };
    assert_eq!(preferences.default_stt_engine_id, None);
    assert_eq!(preferences.default_text_engine_id, None);
}

#[test]
fn an_engine_this_machine_offers_is_accepted_for_its_own_stage() {
    let harness = Harness::start();
    let AppResponse::GetCapabilities(capabilities) = harness.call(AppCommand::GetCapabilities)
    else {
        panic!("wrong response");
    };
    let Some(stt) = capabilities
        .engines
        .iter()
        .find(|engine| engine.stage == plaudon_app::dto::EngineStage::Stt)
    else {
        // A build with no speech-to-text engine inventory has nothing to
        // assert here, and inventing one would test the fixture, not the
        // command.
        return;
    };
    let AppResponse::SetEnginePreferences(preferences) = harness.call(
        AppCommand::SetEnginePreferences(SetEnginePreferencesRequest {
            default_stt_engine_id: Some(stt.engine_id.clone()),
            default_text_engine_id: None,
        }),
    ) else {
        panic!("wrong response");
    };
    assert_eq!(
        preferences.default_stt_engine_id.as_deref(),
        Some(stt.engine_id.as_str())
    );

    let envelope = harness.reject(AppCommand::SetEnginePreferences(
        SetEnginePreferencesRequest {
            default_stt_engine_id: None,
            default_text_engine_id: Some(stt.engine_id.clone()),
        },
    ));
    assert_eq!(
        envelope.code,
        plaudon_domain::error::AppErrorCode::InvalidArgument,
        "a speech-to-text engine is not a text engine"
    );
}

/* --------------------------------------------------------- export defaults */

#[test]
fn export_defaults_round_trip_and_default_to_markdown() {
    let harness = Harness::start();
    assert_eq!(
        harness.preferences().export_defaults,
        ExportDefaults::default()
    );

    let defaults = ExportDefaults {
        format: ExportFormat::Srt,
        destination_directory: Some("/tmp/exports".to_owned()),
        include_speaker_labels: false,
        include_timestamps: true,
    };
    let AppResponse::SetExportDefaults(preferences) =
        harness.call(AppCommand::SetExportDefaults(SetExportDefaultsRequest {
            defaults: defaults.clone(),
        }))
    else {
        panic!("wrong response");
    };
    assert_eq!(preferences.export_defaults, defaults);
    assert_eq!(harness.preferences().export_defaults, defaults);
}

#[test]
fn an_empty_export_destination_is_refused_rather_than_stored_as_a_folder() {
    let harness = Harness::start();
    harness.reject(AppCommand::SetExportDefaults(SetExportDefaultsRequest {
        defaults: ExportDefaults {
            destination_directory: Some("   ".to_owned()),
            ..ExportDefaults::default()
        },
    }));
}

/* ---------------------------------------------------------------- shortcuts */

#[test]
fn shortcut_overrides_round_trip_and_start_empty() {
    let harness = Harness::start();
    let AppResponse::GetShortcuts(bindings) = harness.call(AppCommand::GetShortcuts) else {
        panic!("wrong response");
    };
    assert_eq!(bindings, ShortcutBindings::defaults());

    let specs = specs();
    let mut changed = ShortcutBindings::defaults();
    changed
        .rebind(&specs, "search_library", "Mod+E")
        .expect("Mod+E is free");
    let AppResponse::SetShortcuts(stored) =
        harness.call(AppCommand::SetShortcuts(SetShortcutsRequest {
            bindings: changed.clone(),
            specs: specs.clone(),
        }))
    else {
        panic!("wrong response");
    };
    assert_eq!(stored, changed);

    let AppResponse::GetShortcuts(reread) = harness.call(AppCommand::GetShortcuts) else {
        panic!("wrong response");
    };
    assert_eq!(reread.chord(&specs[1]).as_deref(), Some("Mod+E"));
}

#[test]
fn a_conflicting_binding_map_is_refused_and_nothing_is_written() {
    let harness = Harness::start();
    let specs = specs();
    let colliding: ShortcutBindings =
        serde_json::from_str(r#"{"overrides":{"search_library":"Mod+1"}}"#).expect("json");
    harness.reject(AppCommand::SetShortcuts(SetShortcutsRequest {
        bindings: colliding,
        specs: specs.clone(),
    }));

    let AppResponse::GetShortcuts(stored) = harness.call(AppCommand::GetShortcuts) else {
        panic!("wrong response");
    };
    assert_eq!(stored, ShortcutBindings::defaults());
}

#[test]
fn bindings_without_a_registry_to_validate_against_are_refused() {
    let harness = Harness::start();
    harness.reject(AppCommand::SetShortcuts(SetShortcutsRequest {
        bindings: ShortcutBindings::defaults(),
        specs: Vec::new(),
    }));
}

/* ------------------------------------------------------------ local models */

#[test]
fn a_fresh_library_has_no_models_and_says_where_they_would_come_from() {
    let harness = Harness::start();
    let AppResponse::ListLocalModels(response) = harness.call(AppCommand::ListLocalModels) else {
        panic!("wrong response");
    };
    assert!(response.models.is_empty());
    assert_eq!(response.disk.installed_bytes, 0);
    assert!(response.model_root.ends_with("models"));
    assert!(
        response.source_note.contains("offline model pack"),
        "{}",
        response.source_note
    );
}

#[test]
fn inspecting_a_pack_reports_the_licence_and_footprint_gate_and_installs_nothing() {
    let harness = Harness::start();
    let pack = tempfile::tempdir().expect("tempdir");
    write_pack(pack.path());

    let AppResponse::InspectModelPack(inspection) =
        harness.call(AppCommand::InspectModelPack(InspectModelPackRequest {
            pack_path: pack.path().display().to_string(),
        }))
    else {
        panic!("wrong response");
    };
    assert_eq!(inspection.offers.len(), 1);
    let offer = &inspection.offers[0];
    assert!(!offer.already_installed);
    assert_eq!(offer.gate.licence.spdx_id, "MIT");
    assert!(offer.gate.licence.redistributable);
    assert_eq!(offer.gate.footprint.size_bytes, WEIGHTS.len() as u64);
    assert_eq!(
        offer.gate.footprint.required_bytes,
        WEIGHTS.len() as u64 + WEIGHTS.len() as u64 / 10,
        "the gate quotes the headroom the installer actually enforces"
    );

    let AppResponse::ListLocalModels(after) = harness.call(AppCommand::ListLocalModels) else {
        panic!("wrong response");
    };
    assert!(after.models.is_empty(), "inspection must install nothing");
}

#[test]
fn an_install_without_the_licence_and_the_footprint_is_refused() {
    let harness = Harness::start();
    let pack = tempfile::tempdir().expect("tempdir");
    write_pack(pack.path());
    let path = pack.path().display().to_string();

    harness.reject(AppCommand::InstallModelFromPack(
        InstallModelFromPackRequest {
            pack_path: path.clone(),
            model_id: "tiny-en".to_owned(),
            licence_accepted_spdx_id: "MIT".to_owned(),
            footprint_acknowledged: false,
            keep_offline: false,
        },
    ));
    harness.reject(AppCommand::InstallModelFromPack(
        InstallModelFromPackRequest {
            pack_path: path.clone(),
            model_id: "tiny-en".to_owned(),
            licence_accepted_spdx_id: String::new(),
            footprint_acknowledged: true,
            keep_offline: false,
        },
    ));
    harness.reject(AppCommand::InstallModelFromPack(
        InstallModelFromPackRequest {
            pack_path: path,
            model_id: "tiny-en".to_owned(),
            licence_accepted_spdx_id: "Apache-2.0".to_owned(),
            footprint_acknowledged: true,
            keep_offline: false,
        },
    ));

    let AppResponse::ListLocalModels(after) = harness.call(AppCommand::ListLocalModels) else {
        panic!("wrong response");
    };
    assert!(after.models.is_empty(), "a refused install writes nothing");
}

#[test]
fn an_approved_install_lands_verifies_and_can_be_removed() {
    let harness = Harness::start();
    let pack = tempfile::tempdir().expect("tempdir");
    write_pack(pack.path());
    let path = pack.path().display().to_string();

    let AppResponse::InstallModelFromPack(installed) = harness.call(
        AppCommand::InstallModelFromPack(InstallModelFromPackRequest {
            pack_path: path.clone(),
            model_id: "tiny-en".to_owned(),
            licence_accepted_spdx_id: "MIT".to_owned(),
            footprint_acknowledged: true,
            keep_offline: true,
        }),
    ) else {
        panic!("wrong response");
    };
    assert_eq!(installed.model.model_id, "tiny-en");
    assert_eq!(installed.model.licence_spdx_id, "MIT");
    assert!(installed.model.keep_offline);
    assert_eq!(installed.transferred_bytes, WEIGHTS.len() as u64);
    assert_eq!(installed.disk.installed_bytes, WEIGHTS.len() as u64);

    let AppResponse::VerifyLocalModel(verified) =
        harness.call(AppCommand::VerifyLocalModel(VerifyLocalModelRequest {
            model_id: "tiny-en".to_owned(),
        }))
    else {
        panic!("wrong response");
    };
    assert!(verified.intact);
    assert_eq!(verified.observed_bytes, WEIGHTS.len() as u64);

    // Installing the same model twice is refused rather than silently redone.
    harness.reject(AppCommand::InstallModelFromPack(
        InstallModelFromPackRequest {
            pack_path: path,
            model_id: "tiny-en".to_owned(),
            licence_accepted_spdx_id: "MIT".to_owned(),
            footprint_acknowledged: true,
            keep_offline: false,
        },
    ));

    harness.reject(AppCommand::RemoveLocalModel(RemoveLocalModelRequest {
        model_id: "tiny-en".to_owned(),
        confirmed: false,
    }));

    let AppResponse::RemoveLocalModel(removed) =
        harness.call(AppCommand::RemoveLocalModel(RemoveLocalModelRequest {
            model_id: "tiny-en".to_owned(),
            confirmed: true,
        }))
    else {
        panic!("wrong response");
    };
    assert_eq!(removed.bytes_freed, WEIGHTS.len() as u64);
    assert_eq!(removed.disk.installed_bytes, 0);
}

#[test]
fn a_pack_path_that_is_not_a_folder_is_refused_before_anything_is_read() {
    let harness = Harness::start();
    harness.reject(AppCommand::InspectModelPack(InspectModelPackRequest {
        pack_path: "/definitely/not/here".to_owned(),
    }));
}

/* ----------------------------------------------------------------- updates */

#[test]
fn a_build_the_updater_did_not_install_reports_that_rather_than_a_version() {
    let harness = Harness::start();
    let AppResponse::GetUpdateStatus(status) = harness.call(AppCommand::GetUpdateStatus) else {
        panic!("wrong response");
    };
    assert!(
        matches!(
            status.readiness,
            UpdateReadiness::NotInstalledByUpdater { .. }
        ),
        "{:?}",
        status.readiness
    );
    assert_eq!(
        status.installed_version_copy(),
        "Not managed by the updater"
    );
    assert!(!status.manifest_endpoint.is_empty());
    assert!(!status.signing_key_id.is_empty());
}

#[test]
fn local_only_mode_suppresses_update_checks_and_the_status_says_so() {
    let harness = Harness::start();
    let AppResponse::GetUpdateStatus(before) = harness.call(AppCommand::GetUpdateStatus) else {
        panic!("wrong response");
    };
    assert!(!before.checks_suppressed);

    harness.call(AppCommand::SetLocalOnlyMode(
        plaudon_app::dto::SetLocalOnlyModeRequest {
            enabled: true,
            confirmed: true,
        },
    ));

    let AppResponse::GetUpdateStatus(after) = harness.call(AppCommand::GetUpdateStatus) else {
        panic!("wrong response");
    };
    assert!(after.checks_suppressed);
    assert!(after.check_policy_copy().contains("no update request"));
}
