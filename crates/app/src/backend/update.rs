//! What the update subsystem can report without making a request (task N2.2,
//! spec §16.2A, §16.5, §22 item 15).
//!
//! The verification, A/B install and rollback logic lives in
//! [`crate::updater`] over `plaudon-updater`. This backend is the read-only
//! half the Settings screen needs, and it is read-only on purpose: the one
//! thing a settings panel must never do is make a network request while the
//! user is reading about network requests.
//!
//! Two honest states, no third:
//!
//! * the installation root exists, so the version that will launch next is a
//!   fact that can be reported;
//! * it does not, which is what a build the updater did not install looks
//!   like, and the section says so instead of printing the compiled-in
//!   version as though it had been verified.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use plaudon_domain::error::AppError;

use crate::ports::{SettingsPort, UpdatePort};
use crate::settings_presentation::{UpdateReadiness, UpdateStatus};
use crate::updater::{pinned_update_configuration, UpdateService};

/// Read-only update facts.
pub struct UpdateBackend {
    app_data_dir: PathBuf,
    settings: std::sync::Arc<dyn SettingsPort>,
}

impl std::fmt::Debug for UpdateBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateBackend")
            .field("app_data_dir", &self.app_data_dir)
            .finish_non_exhaustive()
    }
}

impl UpdateBackend {
    /// Report on the installation root inside `app_data_dir`.
    #[must_use]
    pub fn new(app_data_dir: &Path, settings: std::sync::Arc<dyn SettingsPort>) -> Self {
        Self {
            app_data_dir: app_data_dir.to_path_buf(),
            settings,
        }
    }

    /// The installation root this build would use.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        UpdateService::root_for(&self.app_data_dir)
    }
}

#[async_trait]
impl UpdatePort for UpdateBackend {
    async fn update_status(&self) -> Result<UpdateStatus, AppError> {
        let local_only = self.settings.preferences().await?.local_only_mode;
        let (manifest_endpoint, signing_key_id) = pinned_update_configuration();
        let app_data_dir = self.app_data_dir.clone();
        // Opening the installer runs crash recovery against the journal, which
        // touches the filesystem; it does not touch the network.
        let readiness = tokio::task::spawn_blocking(move || {
            match UpdateService::open(&app_data_dir, local_only) {
                Ok(service) => match service.installed_version() {
                    Ok(version) => UpdateReadiness::Ready {
                        installed_version: version.to_string(),
                    },
                    Err(error) => UpdateReadiness::NotInstalledByUpdater {
                        detail: error.to_string(),
                    },
                },
                Err(error) => UpdateReadiness::NotInstalledByUpdater {
                    detail: error.to_string(),
                },
            }
        })
        .await
        .map_err(|_| {
            AppError::new(
                plaudon_domain::error::AppErrorCode::InternalError,
                "reading the update root panicked",
            )
        })?;

        Ok(UpdateStatus {
            readiness,
            checks_suppressed: local_only,
            manifest_endpoint: manifest_endpoint.to_owned(),
            signing_key_id: signing_key_id.to_owned(),
        })
    }
}
