//! Observed machine capabilities (`get_capabilities`).
//!
//! Everything reported here is measured or read from storage. The hard rule of
//! spec §6.6 applies: **unknown is not supported**. No engine is reported
//! `supported` without evidence that it ran on this machine; an engine we have
//! not probed is `unknown` and carries the reason.

use async_trait::async_trait;
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::AppError;
use sysinfo::{Disks, System};

use crate::backend::LocalBackend;
use crate::dto::{
    Capabilities, EngineInventoryEntry, EngineStage, HardwareInfo, InstalledModel, OsInfo,
    PermissionState, Platform, StorageCapability,
};
use plaudon_ai_apple::probe::{AppleCapabilities, AssetDownloadConsent, ProbeCache};
use plaudon_ai_contracts::capabilities::AvailabilityReason;
use plaudon_app_core::engine_policy::explain;

use crate::ports::{CapabilitiesPort, SettingsPort};

/// Reason attached to every engine this build has not validated on this host.
const NO_EVIDENCE: &str =
    "No validation run has been recorded for this engine on this machine, so it is reported as unknown rather than supported.";

#[async_trait]
impl CapabilitiesPort for LocalBackend {
    async fn capabilities(&self) -> Result<Capabilities, AppError> {
        let preferences = self.preferences().await?;
        let openrouter_key_present = self.secrets().openrouter_key_present().unwrap_or(false);

        let mut system = System::new();
        system.refresh_memory();
        system.refresh_cpu_usage();

        let os = OsInfo {
            platform: host_platform(),
            version: System::os_version()
                .or_else(System::kernel_version)
                .unwrap_or_else(|| "unknown".to_owned()),
        };
        let hardware = HardwareInfo {
            arch: std::env::consts::ARCH.to_owned(),
            logical_cores: u32::try_from(system.cpus().len()).unwrap_or(u32::MAX),
            total_memory_bytes: system.total_memory(),
        };
        let storage = self.storage_capability();
        let engines = self.engine_inventory(openrouter_key_present);

        let mut integrations = std::collections::BTreeMap::new();
        // Neither integration has been probed by this build; §6.6 forbids
        // claiming anything better than unknown without evidence.
        integrations.insert("plaud_cloud".to_owned(), CapabilityStatus::Unknown);
        integrations.insert("openrouter".to_owned(), CapabilityStatus::Unknown);

        let mut permissions = std::collections::BTreeMap::new();
        permissions.insert(
            "notifications".to_owned(),
            self.notification_permission_state().await,
        );

        Ok(Capabilities {
            os,
            hardware,
            engines,
            storage,
            integrations,
            permissions,
            preferences,
            openrouter_key_present,
        })
    }
}

impl LocalBackend {
    /// Free and total bytes of the volume that holds the library.
    pub(crate) fn storage_capability(&self) -> StorageCapability {
        let root = self.store().root().to_path_buf();
        let disks = Disks::new_with_refreshed_list();
        let mut best: Option<(usize, u64, u64)> = None;
        for disk in &disks {
            let mount = disk.mount_point();
            if root.starts_with(mount) {
                let depth = mount.components().count();
                let candidate = (depth, disk.available_space(), disk.total_space());
                if best.is_none_or(|(best_depth, _, _)| depth > best_depth) {
                    best = Some(candidate);
                }
            }
        }
        let (free_bytes, total_bytes) = best.map_or((0, 0), |(_, free, total)| (free, total));
        StorageCapability {
            library_path: root.to_string_lossy().into_owned(),
            free_bytes,
            total_bytes,
        }
    }

    /// Engine inventory derived from what is actually installed on disk and,
    /// for the Apple engines, from a runtime probe (§11.5, task H1.3).
    ///
    /// The Apple rows are the only ones here that can reach `supported`,
    /// because they are the only ones with a probe behind them. Their
    /// `unavailable_reason` names the observed cause and the first corrective
    /// action, which is what §5.4 asks for: "Apple AI unavailable → explain
    /// device/OS/model/locale reason, offer available alternatives."
    pub(crate) fn engine_inventory(
        &self,
        openrouter_key_present: bool,
    ) -> Vec<EngineInventoryEntry> {
        let models_dir = self.store().models_dir();
        let whisper_models = installed_models(&models_dir.join("whisper.cpp"));
        let apple = apple_engine_entries();

        let mut entries = vec![EngineInventoryEntry {
            engine_id: "whisper.cpp".to_owned(),
            display_name: "whisper.cpp (local)".to_owned(),
            stage: EngineStage::Stt,
            status: CapabilityStatus::Unknown,
            unavailable_reason: Some(if whisper_models.is_empty() {
                "No whisper.cpp model is installed on this machine.".to_owned()
            } else {
                NO_EVIDENCE.to_owned()
            }),
            installed_models: whisper_models,
            // The download size depends on the model the user picks; this
            // build has no catalog, so it reports unknown rather than a guess.
            download_bytes_required: None,
            requires_network: false,
        }];
        entries.extend(apple);
        entries.push(EngineInventoryEntry {
            engine_id: "openrouter".to_owned(),
            display_name: "OpenRouter".to_owned(),
            stage: EngineStage::Text,
            status: CapabilityStatus::Unknown,
            unavailable_reason: Some(if openrouter_key_present {
                NO_EVIDENCE.to_owned()
            } else {
                "No OpenRouter key is stored, and no capability probe has run.".to_owned()
            }),
            installed_models: Vec::new(),
            download_bytes_required: None,
            requires_network: true,
        });
        entries
    }
}

/// Shared availability cache for the Apple engines.
///
/// The capability surface is asked for on every Settings and Model picker
/// render, and a probe costs tens of milliseconds, so the result is cached
/// and re-taken when the environment fingerprint changes (task H1.3). One
/// cache per process: the thing it describes is the machine, not the window.
static APPLE_PROBE_CACHE: std::sync::LazyLock<ProbeCache> =
    std::sync::LazyLock::new(|| ProbeCache::new(plaudon_ai_apple::host_bridge()));

/// The two Apple rows, derived from a runtime probe.
///
/// On a build without the bridge — every Windows build, and a macOS build
/// without the `apple-bridge` feature — the probe answers
/// `platform_unsupported` without touching an Apple framework, and these
/// rows say so with the same vocabulary a real refusal would use.
fn apple_engine_entries() -> Vec<EngineInventoryEntry> {
    // Blocking, and bounded at 100 ms by `PROBE_BUDGET`; a cached read is
    // microseconds. Availability is a precondition of every engine decision,
    // so it has to be answerable synchronously wherever one is made.
    let (availability, _) = APPLE_PROBE_CACHE.availability(None);
    let capabilities =
        AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);

    vec![
        EngineInventoryEntry {
            engine_id: plaudon_ai_apple::APPLE_SPEECH_ENGINE.to_owned(),
            display_name: "Apple Speech (on device)".to_owned(),
            stage: EngineStage::Stt,
            status: capabilities.speech.status,
            unavailable_reason: unavailability_text(
                &capabilities.speech.engine_id,
                &capabilities.speech.availability_reason,
            ),
            installed_models: installed_locales_as_models(&availability),
            // The platform publishes no download size before an install
            // starts, and a guessed figure in a consent prompt would be worse
            // than none (§12).
            download_bytes_required: None,
            requires_network: false,
        },
        EngineInventoryEntry {
            engine_id: plaudon_ai_apple::APPLE_FOUNDATION_ENGINE.to_owned(),
            display_name: "Apple on-device language model".to_owned(),
            stage: EngineStage::Text,
            status: capabilities.text_model.status,
            unavailable_reason: unavailability_text(
                &capabilities.text_model.engine_id,
                &capabilities.text_model.availability_reason,
            ),
            installed_models: Vec::new(),
            download_bytes_required: None,
            requires_network: false,
        },
    ]
}

/// One sentence naming the observed cause, plus the first thing to do.
///
/// `None` when the engine is available: the field means "why not", and
/// filling it on success would put a reason next to a working engine.
fn unavailability_text(
    engine_id: &plaudon_domain::model::EngineId,
    reason: &AvailabilityReason,
) -> Option<String> {
    if reason.is_available() {
        return None;
    }
    let explanation = explain(engine_id, reason);
    if explanation.actions.is_empty() {
        return Some(explanation.headline);
    }
    // Every action, not only the first: §5.4 asks for the alternatives as
    // well as the cause, and on this engine the alternative — transcribe
    // locally with whisper.cpp — is usually the last one in the list.
    let actions: Vec<&str> = explanation
        .actions
        .iter()
        .map(|action| action.label())
        .collect();
    Some(format!("{} {}.", explanation.headline, actions.join("; ")))
}

/// Installed speech locales, shown in the picker the way models are.
///
/// A locale *is* the installed artefact for this engine, and the picker
/// already has a column for what is on disk. Size is unknown: the platform
/// manages the files and does not report their footprint.
fn installed_locales_as_models(
    availability: &plaudon_ai_apple::AppleAvailability,
) -> Vec<InstalledModel> {
    availability
        .speech
        .as_ref()
        .map(|probe| {
            probe
                .installed_locales
                .iter()
                .map(|locale| InstalledModel {
                    display_name: locale.replace('_', "-"),
                    model_id: locale.clone(),
                    size_bytes: 0,
                    installed: true,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Installed models of one engine: one directory per model id.
fn installed_models(engine_dir: &std::path::Path) -> Vec<InstalledModel> {
    let Ok(entries) = std::fs::read_dir(engine_dir) else {
        return Vec::new();
    };
    let mut models: Vec<InstalledModel> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let model_id = entry.file_name().to_string_lossy().into_owned();
            let size_bytes = directory_size(&entry.path());
            InstalledModel {
                display_name: model_id.clone(),
                model_id,
                size_bytes,
                installed: size_bytes > 0,
            }
        })
        .collect();
    models.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    models
}

fn directory_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            Ok(metadata) if metadata.is_dir() => directory_size(&entry.path()),
            _ => 0,
        })
        .sum()
}

fn host_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// Permission states this build can honestly report.
pub(crate) fn permission_from_stored(stored: Option<&str>) -> PermissionState {
    match stored {
        Some("granted") => PermissionState::Granted,
        Some("denied") => PermissionState::Denied,
        // Nothing has ever asked the OS, which is exactly `not_requested`.
        _ => PermissionState::NotRequested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::backend_fixture;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capabilities_report_real_hardware_and_no_unproven_engine() {
        let fixture = backend_fixture().await;
        let capabilities = fixture.backend.capabilities().await.unwrap();

        assert_eq!(capabilities.hardware.arch, std::env::consts::ARCH);
        assert!(capabilities.hardware.logical_cores >= 1);
        assert!(capabilities.hardware.total_memory_bytes > 0);
        assert_ne!(capabilities.os.version, "");
        assert_eq!(
            capabilities.storage.library_path,
            fixture.backend.store().root().to_string_lossy()
        );
        assert!(
            capabilities.storage.total_bytes > 0,
            "the library volume reports a size"
        );

        for engine in &capabilities.engines {
            // The Apple rows are the only ones with a runtime probe behind
            // them, so they are the only ones that may claim support — and
            // only on a build that actually links the bridge (§11.5).
            let probe_backed = engine.engine_id.starts_with("apple-");
            if !probe_backed {
                assert_ne!(
                    engine.status,
                    CapabilityStatus::Supported,
                    "{} must not claim support without evidence",
                    engine.engine_id
                );
            }
            if engine.status == CapabilityStatus::Supported {
                assert!(
                    engine.unavailable_reason.is_none(),
                    "{} is available and must not carry a reason it is not",
                    engine.engine_id
                );
            } else {
                assert!(
                    engine.unavailable_reason.is_some(),
                    "{} is unavailable and must say why",
                    engine.engine_id
                );
            }
            assert_eq!(engine.download_bytes_required, None);
        }
        assert!(!capabilities.openrouter_key_present);
        assert_eq!(
            capabilities.integrations["openrouter"],
            CapabilityStatus::Unknown
        );
        assert_eq!(
            capabilities.permissions["notifications"],
            PermissionState::NotRequested
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_apple_rows_come_from_a_probe_and_explain_themselves() {
        // §5.4: "Apple AI unavailable → explain device/OS/model/locale
        // reason, offer available alternatives." The row must therefore name
        // the observed cause and something to do about it, never a generic
        // "not supported".
        let fixture = backend_fixture().await;
        let capabilities = fixture.backend.capabilities().await.unwrap();

        let speech = capabilities
            .engines
            .iter()
            .find(|engine| engine.engine_id == plaudon_ai_apple::APPLE_SPEECH_ENGINE)
            .expect("the Apple speech row is always present, even off macOS");
        assert_eq!(speech.stage, EngineStage::Stt);
        assert!(!speech.requires_network, "Apple speech runs on device");

        let text = capabilities
            .engines
            .iter()
            .find(|engine| engine.engine_id == plaudon_ai_apple::APPLE_FOUNDATION_ENGINE)
            .expect("the Apple language model row is always present");
        assert_eq!(text.stage, EngineStage::Text);
        assert!(!text.requires_network);

        for engine in [speech, text] {
            match &engine.unavailable_reason {
                Some(reason) => {
                    assert!(
                        reason.len() > 20 && reason.ends_with('.'),
                        "{} gave a non-explanation: {reason}",
                        engine.engine_id
                    );
                    assert!(
                        !reason.contains("No validation run has been recorded"),
                        "{} fell back to the no-evidence placeholder instead of a probe result",
                        engine.engine_id
                    );
                }
                None => assert_eq!(
                    engine.status,
                    CapabilityStatus::Supported,
                    "{} gave no reason while not being available",
                    engine.engine_id
                ),
            }
        }
    }

    #[test]
    fn a_build_without_the_bridge_reports_the_platform_not_a_placeholder() {
        // Compiled on every platform. Without the `apple-bridge` feature the
        // probe answers `platform_unsupported` with no Apple framework
        // loaded, which is exactly what a Windows build must show (§11.1).
        let entries = apple_engine_entries();
        assert_eq!(entries.len(), 2);
        if cfg!(all(target_os = "macos", feature = "apple-bridge")) {
            return;
        }
        for entry in entries {
            assert_eq!(entry.status, CapabilityStatus::Unsupported);
            let reason = entry
                .unavailable_reason
                .expect("an unavailable row says why");
            assert!(
                reason.contains("frameworks") || reason.contains("macOS"),
                "{reason}"
            );
            assert!(
                reason.contains("whisper.cpp"),
                "no alternative offered: {reason}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn installed_models_are_discovered_from_disk() {
        let fixture = backend_fixture().await;
        let model_dir = fixture
            .backend
            .store()
            .models_dir()
            .join("whisper.cpp")
            .join("small-q5");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("model.bin"), vec![7u8; 2048]).unwrap();

        let engines = fixture.backend.engine_inventory(false);
        let whisper = engines
            .iter()
            .find(|entry| entry.engine_id == "whisper.cpp")
            .unwrap();
        assert_eq!(whisper.installed_models.len(), 1);
        assert_eq!(whisper.installed_models[0].model_id, "small-q5");
        assert_eq!(whisper.installed_models[0].size_bytes, 2048);
        assert!(whisper.installed_models[0].installed);
        assert_eq!(
            whisper.status,
            CapabilityStatus::Unknown,
            "an installed model is not evidence that the engine runs"
        );
    }
}
