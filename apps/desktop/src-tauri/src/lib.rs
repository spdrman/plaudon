//! The Tauri 2 desktop shell.
//!
//! The shell owns three things and delegates everything else:
//!
//! * [`commands`] — narrow IPC commands (spec §15.2, ADR-0020, ADR-0022) plus
//!   the seven `device_*` commands of EPIC L, which validate their arguments
//!   in Rust and answer with typed DTOs or the §15.4 error envelope;
//! * [`events`] — the §15.3 UI event stream, sequenced after the database
//!   commit that caused each event;
//! * [`media`] — the `plaudon-media` scheme, which serves archived audio
//!   read-only with `Range` support and never accepts a path from a URL.
//!
//! Everything durable comes from EPIC B (`plaudon_persistence`,
//! `plaudon_app_core`); [`build_ports`] is the single place that decides which
//! implementations satisfy the [`ports`] traits, so EPIC C's job engine can be
//! swapped in with one edit.

pub mod backend;
pub mod commands;
pub mod dto;
pub mod error;
pub mod events;
pub mod media;
pub mod notify;
pub mod ports;
pub mod secrets;
pub mod updater;
pub mod validate;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_domain::error::AppError;

use crate::backend::devices::{DeviceRadio, DevicesBackend, DevicesDeps, NoRadio};
use crate::backend::LocalBackend;
use crate::events::EventSink;
use crate::events::TauriEventSink;
use crate::ports::{
    ActivityPort, ArchivePort, CapabilitiesPort, CloudPort, DeletionPort, DevicesPort, ExportPort,
    ImportPort, JobsPort, LibraryPort, MediaExportPort, OutputsPort, SearchPort, SettingsPort,
    SourcesPort, SpeakersPort, TranscriptPort,
};
use crate::secrets::{KeyringSecretStore, SecretStore};

/// The port implementations the command layer runs against.
#[derive(Clone)]
pub struct AppPorts {
    /// Machine capabilities and stored preferences.
    pub capabilities: Arc<dyn CapabilitiesPort>,
    /// The recording library.
    pub library: Arc<dyn LibraryPort>,
    /// Transcript revisions.
    pub transcript: Arc<dyn TranscriptPort>,
    /// The job table.
    pub jobs: Arc<dyn JobsPort>,
    /// The activity history.
    pub activity: Arc<dyn ActivityPort>,
    /// Manual import.
    pub import: Arc<dyn ImportPort>,
    /// Preferences and policy switches.
    pub settings: Arc<dyn SettingsPort>,
    /// Transcript export.
    pub export: Arc<dyn ExportPort>,
    /// The PLAUD cloud connector and watched folders.
    pub sources: Arc<dyn SourcesPort>,
    /// Cloud AI: catalogue, usage, budget, consent and dispatch.
    pub cloud: Arc<dyn CloudPort>,
    /// Direct PLAUD device access (read-only).
    pub devices: Arc<dyn DevicesPort>,
    /// Generated outputs and claim review.
    pub outputs: Arc<dyn OutputsPort>,
    /// Original-audio and sanitized-derivative export.
    pub media_export: Arc<dyn MediaExportPort>,
    /// Deletion with disclosure and tombstones.
    pub deletion: Arc<dyn DeletionPort>,
    /// The portable archive.
    pub archive: Arc<dyn ArchivePort>,
    /// OS keychain storage.
    pub secrets: Arc<dyn SecretStore>,
    /// Cross-recording search.
    pub search: Arc<dyn SearchPort>,
    /// Speaker labelling and confirmation.
    pub speakers: Arc<dyn SpeakersPort>,
    /// The UI event stream, shared with the notification activation route.
    pub events: crate::events::EventPublisher,
}

impl std::fmt::Debug for AppPorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppPorts")
    }
}

/// Managed Tauri state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The port implementations.
    pub ports: AppPorts,
}

/// Build every port over the library in `app_data_dir`.
///
/// This is the swap point: replacing the job engine or the library backend is
/// a change to this function and nothing else. The direct-device port gets no
/// radio here (it reports [`dto::RadioState::UnavailableInBuild`]); a build
/// with a BLE radio calls [`build_ports_with_radio`].
pub async fn build_ports(
    app_data_dir: &Path,
    sink: Arc<dyn EventSink>,
    notifications: Arc<plaudon_notifications::Dispatcher>,
    secrets: Arc<dyn SecretStore>,
    approved_roots: Vec<PathBuf>,
) -> Result<AppPorts, AppError> {
    build_ports_with_radio(
        app_data_dir,
        sink,
        notifications,
        secrets,
        approved_roots,
        Arc::new(NoRadio),
    )
    .await
}

/// [`build_ports`] with a BLE radio actor for the direct-device port.
pub async fn build_ports_with_radio(
    app_data_dir: &Path,
    sink: Arc<dyn EventSink>,
    notifications: Arc<plaudon_notifications::Dispatcher>,
    secrets: Arc<dyn SecretStore>,
    approved_roots: Vec<PathBuf>,
    radio: Arc<dyn DeviceRadio>,
) -> Result<AppPorts, AppError> {
    let backend = Arc::new(
        LocalBackend::open(
            app_data_dir,
            sink,
            notifications,
            Arc::clone(&secrets),
            approved_roots,
        )
        .await?,
    );
    // Spec §13.3A: an import interrupted by a crash is reconciled against the
    // actual file hashes before the UI can ask for anything.
    if let Err(err) = backend.reconcile_startup().await {
        tracing::warn!(
            code = err.code().as_str(),
            "import reconciliation did not complete"
        );
    }
    let events = backend.events().clone();
    let devices = DevicesBackend::new(DevicesDeps::production(
        backend.store().db().clone(),
        backend.store().archive().clone(),
        events.clone(),
        radio,
    )?);
    // A transfer a previous run left `in_flight` is `interrupted` before the
    // UI can ask for anything (spec §13.3A).
    if let Err(err) = devices.recover_interrupted().await {
        tracing::warn!(
            code = err.code().as_str(),
            "device transfers were not reconciled"
        );
    }
    Ok(AppPorts {
        capabilities: Arc::clone(&backend) as Arc<dyn CapabilitiesPort>,
        library: Arc::clone(&backend) as Arc<dyn LibraryPort>,
        transcript: Arc::clone(&backend) as Arc<dyn TranscriptPort>,
        jobs: Arc::clone(&backend) as Arc<dyn JobsPort>,
        activity: Arc::clone(&backend) as Arc<dyn ActivityPort>,
        import: Arc::clone(&backend) as Arc<dyn ImportPort>,
        settings: Arc::clone(&backend) as Arc<dyn SettingsPort>,
        sources: Arc::clone(&backend) as Arc<dyn SourcesPort>,
        export: Arc::clone(&backend) as Arc<dyn ExportPort>,
        search: Arc::clone(&backend) as Arc<dyn SearchPort>,
        speakers: Arc::clone(&backend) as Arc<dyn SpeakersPort>,
        cloud: Arc::clone(&backend) as Arc<dyn CloudPort>,
        outputs: Arc::clone(&backend) as Arc<dyn OutputsPort>,
        devices: Arc::new(devices),
        media_export: Arc::clone(&backend) as Arc<dyn MediaExportPort>,
        deletion: Arc::clone(&backend) as Arc<dyn DeletionPort>,
        archive: backend as Arc<dyn ArchivePort>,
        secrets,
        events,
    })
}

/// Roots a user-selected file may resolve into.
///
/// A path from the OS picker or a window drop is still validated: its resolved
/// target must stay inside one of these roots, so a symlink that escapes to a
/// system location is rejected (spec §7.4).
#[must_use]
pub fn default_approved_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home);
    }
    for mount in ["/Volumes", "/media", "/mnt", "/run/media"] {
        let path = PathBuf::from(mount);
        if path.is_dir() {
            roots.push(path);
        }
    }
    roots
}

/// Start the desktop application.
///
/// # Panics
///
/// Panics only if the Tauri context cannot be built, which is a build-time
/// configuration error rather than a runtime condition.
pub fn run() {
    tracing_subscriber_init();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(commands::handler());
    let builder = media::register(builder);

    builder
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = tauri::Manager::path(app)
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(handle.clone()));
            let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::new());

            // A notification click routes back through the same handle.
            let activation_handle = handle.clone();
            let on_activated: notify::ActivationHandler = Arc::new(
                move |target: Option<plaudon_notifications::NotificationTarget>| {
                    if let Some(target) = target {
                        activate(&activation_handle, target);
                    }
                },
            );
            // Preferences are loaded by the backend; the dispatcher starts
            // disabled so nothing can be delivered before they are applied.
            let dispatcher = Arc::new(plaudon_notifications::Dispatcher::new(
                notify::platform_notifier(on_activated),
                plaudon_notifications::NotificationSettings::uniform(false),
            ));

            let ports = tauri::async_runtime::block_on(build_ports(
                &data_dir,
                sink,
                dispatcher,
                secrets,
                default_approved_roots(),
            ))?;

            // Spec 16.5 and 22(8): opening the update root runs crash
            // recovery, discarding any uncommitted attempt so the launched
            // version is always a complete, previously verified one. A root
            // that does not exist yet is not an error: this build was
            // installed by the packaged installer, not by the updater.
            match updater::UpdateService::open(&data_dir, true) {
                Ok(service) => match service.installed_version() {
                    Ok(version) => tracing::info!(%version, "update state recovered"),
                    Err(error) => tracing::warn!(%error, "update state is unreadable"),
                },
                Err(error) => {
                    tracing::debug!(%error, "no update root yet; nothing to recover");
                }
            }

            tauri::Manager::manage(app, AppState { ports });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the desktop application failed to start");
}

/// Route a notification activation: focus the window, then tell the UI where
/// to go.
///
/// Called by the Windows toast handler and by the macOS application delegate
/// with the token from `targetContentIdentifier`. Never force-raises outside
/// this callback (spec §5.5: subject to OS activation rules).
pub fn activate<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: plaudon_notifications::NotificationTarget,
) {
    use tauri::Manager as _;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let Some(state) = app.try_state::<AppState>() else {
        tracing::debug!("notification activated before the shell was ready");
        return;
    };
    let events = state.ports.events.clone();
    tauri::async_runtime::spawn(async move {
        notify::publish_activation(&events, target).await;
    });
}

/// Route a notification activation from a platform token.
///
/// A token this build did not write is ignored.
pub fn activate_token<R: tauri::Runtime>(app: &tauri::AppHandle<R>, token: &str) {
    if let Some(target) = notify::activation_target(token) {
        activate(app, target);
    }
}

fn tracing_subscriber_init() {
    // Logging is redacted at the source: only `error::envelope` writes error
    // detail, and it writes the domain redactor's output.
    tracing::debug!("desktop shell starting");
}
