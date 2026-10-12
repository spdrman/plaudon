//! The Tauri-free application layer.
//!
//! Everything the desktop application does, minus the window. This crate owns
//!
//! * [`ports`] — the 17 async traits the command layer runs against;
//! * [`backend`] — the local implementations over EPIC B's persistence;
//! * [`dto`], [`validate`], [`error`] — the wire types, the argument
//!   validation and the §15.4 error envelope;
//! * [`commands`] — one validated function per published command;
//! * [`command`] — the typed [`command::AppCommand`] surface and the single
//!   [`command::Dispatcher`] every caller (views, shortcuts, palette, tests)
//!   goes through;
//! * [`event`] — [`event::AppEvent`], the bounded coalescing channel and the
//!   injectable [`event::Waker`] a render loop uses to wake itself;
//! * [`runtime`] — one owned Tokio runtime, the data directory, startup
//!   reconciliation and shutdown;
//! * [`events`], [`notify`], [`secrets`], [`media`], [`updater`] — the event
//!   stream, notifications, the keychain, archived-media resolution and the
//!   update root;
//! * [`shell_state`] — the window geometry, panel sizes and appearance a
//!   desktop shell restores between runs.
//!
//! It has no user-interface dependency of any kind: no `tauri`, no `egui`, no
//! `eframe`. A user interface is a caller of [`command::Dispatcher`] and a
//! reader of [`event::EventReceiver`], and nothing else.

pub mod backend;
pub mod command;
pub mod commands;
pub mod dto;
pub mod error;
pub mod event;
pub mod events;
pub mod media;
pub mod notify;
pub mod ports;
pub mod runtime;
pub mod secrets;
pub mod shell_state;
pub mod updater;
pub mod validate;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_domain::error::AppError;

use crate::backend::devices::{DeviceRadio, DevicesBackend, DevicesDeps, NoRadio};
use crate::backend::LocalBackend;
use crate::events::EventSink;
use crate::ports::{
    ActivityPort, ArchivePort, CapabilitiesPort, CloudPort, DeletionPort, DevicesPort, ExportPort,
    ImportPort, JobsPort, LibraryPort, MediaExportPort, OutputsPort, SearchPort, SettingsPort,
    SourcesPort, SpeakersPort, TranscriptPort,
};
use crate::secrets::SecretStore;

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
