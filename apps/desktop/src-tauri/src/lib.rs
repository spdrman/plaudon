//! The Tauri 2 desktop shell.
//!
//! The shell owns three things and delegates everything else:
//!
//! * [`commands`] — sixteen narrow IPC commands (spec §15.2) that validate
//!   their arguments in Rust and answer with typed DTOs or the §15.4 error
//!   envelope;
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
pub mod ports;
pub mod secrets;
pub mod validate;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_domain::error::AppError;

use crate::backend::LocalBackend;
use crate::events::{EventSink, TauriEventSink};
use crate::ports::{
    ActivityPort, CapabilitiesPort, ExportPort, ImportPort, JobsPort, LibraryPort, SettingsPort,
    TranscriptPort,
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
    /// OS keychain storage.
    pub secrets: Arc<dyn SecretStore>,
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
/// a change to this function and nothing else.
pub async fn build_ports(
    app_data_dir: &Path,
    sink: Arc<dyn EventSink>,
    secrets: Arc<dyn SecretStore>,
    approved_roots: Vec<PathBuf>,
) -> Result<AppPorts, AppError> {
    let backend = Arc::new(
        LocalBackend::open(app_data_dir, sink, Arc::clone(&secrets), approved_roots).await?,
    );
    // Spec §13.3A: an import interrupted by a crash is reconciled against the
    // actual file hashes before the UI can ask for anything.
    if let Err(err) = backend.reconcile_startup().await {
        tracing::warn!(
            code = err.code().as_str(),
            "import reconciliation did not complete"
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
        export: backend as Arc<dyn ExportPort>,
        secrets,
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
            let ports = tauri::async_runtime::block_on(build_ports(
                &data_dir,
                sink,
                secrets,
                default_approved_roots(),
            ))?;
            tauri::Manager::manage(app, AppState { ports });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the desktop application failed to start");
}

fn tracing_subscriber_init() {
    // Logging is redacted at the source: only `error::envelope` writes error
    // detail, and it writes the domain redactor's output.
    tracing::debug!("desktop shell starting");
}
