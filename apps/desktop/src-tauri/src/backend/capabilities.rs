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
use crate::ports::{CapabilitiesPort, CloudPort, SettingsPort};

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
            cloud_consents: self.cloud_consents().await?,
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

    /// Engine inventory derived from what is actually installed on disk.
    pub(crate) fn engine_inventory(
        &self,
        openrouter_key_present: bool,
    ) -> Vec<EngineInventoryEntry> {
        let models_dir = self.store().models_dir();
        let whisper_models = installed_models(&models_dir.join("whisper.cpp"));
        let apple_supported = cfg!(target_os = "macos");

        vec![
            EngineInventoryEntry {
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
            },
            EngineInventoryEntry {
                engine_id: "apple.speech".to_owned(),
                display_name: "Apple Speech".to_owned(),
                stage: EngineStage::Stt,
                status: if apple_supported {
                    CapabilityStatus::Unknown
                } else {
                    CapabilityStatus::Unsupported
                },
                unavailable_reason: Some(if apple_supported {
                    NO_EVIDENCE.to_owned()
                } else {
                    "Apple Speech is only available on macOS.".to_owned()
                }),
                installed_models: Vec::new(),
                download_bytes_required: None,
                requires_network: false,
            },
            EngineInventoryEntry {
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
            },
        ]
    }
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
            assert_ne!(
                engine.status,
                CapabilityStatus::Supported,
                "{} must not claim support without evidence",
                engine.engine_id
            );
            assert!(engine.unavailable_reason.is_some());
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
