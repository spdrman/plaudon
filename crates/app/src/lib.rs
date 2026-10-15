//! The user-interface-free application layer.
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
pub mod cloud_presentation;
pub mod command;
pub mod commands;
pub mod dto;
pub mod error;
pub mod event;
pub mod events;
pub mod folder_pump;
pub mod job_presentation;
pub mod media;
pub mod notify;
pub mod onboarding;
pub mod ports;
pub mod runtime;
pub mod secrets;
pub mod settings_presentation;
pub mod shell_state;
pub mod shortcuts;
/// Scripted AI engines, for tests in this crate and in the shell.
#[cfg(any(test, feature = "test-support"))]
pub mod testing_engines;
pub mod tone;
pub mod tool_presentation;
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
    ImportPort, JobsPort, LibraryPort, MediaExportPort, ModelsPort, OutputsPort, PeoplePort,
    SearchPort, SettingsPort, SourcesPort, SpeakersPort, TemplatesPort, ToolsPort, TranscriptPort,
    UpdatePort, VoiceProfilesPort,
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
    /// Transcript tools, ask-about-recording and study artifacts.
    pub tools: Arc<dyn ToolsPort>,
    /// Output templates and the user's editable copies.
    pub templates: Arc<dyn TemplatesPort>,
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
    /// The people roster: the master speaker list, ungated (VP-011).
    pub people: Arc<dyn PeoplePort>,
    /// The biometric tier: consent gate, enrolments and suggestions.
    pub voice_profiles: Arc<dyn VoiceProfilesPort>,
    /// Locally installed models.
    pub models: Arc<dyn ModelsPort>,
    /// Read-only update facts.
    pub update: Arc<dyn UpdatePort>,
    /// The UI event stream, shared with the notification activation route.
    pub events: crate::events::EventPublisher,
    /// The running job workers, stopped when the engine shuts down.
    pub processing: crate::backend::processing::ProcessingHandle,
}

impl std::fmt::Debug for AppPorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppPorts")
    }
}

/// Which radio the direct-device port is given.
///
/// Two variants and not one `Option`, because the two cases are built at
/// different moments. A supplied radio exists before the library is opened; a
/// *real* one cannot, because it commits the bytes it transfers through a
/// [`plaudon_source_plaud_device::DeviceImportCommitter`] over the database
/// and the media archive, and those do not exist until [`build_ports_with`]
/// has opened them. So the caller states the intent and this module builds
/// the radio at the only point where it can (matrix row D12).
#[derive(Clone)]
pub enum RadioChoice {
    /// The radio the caller built. The default is
    /// [`backend::devices::NoRadio`], which reports
    /// [`dto::RadioState::UnavailableInBuild`]; the device tests supply a
    /// scripted one.
    Supplied(Arc<dyn DeviceRadio>),
    /// The host's own Bluetooth adapter.
    ///
    /// The variant exists on every build so that resolving a `RadioChoice`
    /// stays an ordinary two-arm match. What a default build compiles out is
    /// the *construction*: `EngineConfig::with_host_bluetooth` exists only
    /// under `hardware-scan`, so nothing outside a hardware build can name
    /// this. If one arrived here anyway the resolver yields `NoRadio` and the
    /// screen keeps reporting [`dto::RadioState::UnavailableInBuild`], which
    /// is the honest answer for a binary with no radio code linked into it.
    HostBluetooth,
}

impl Default for RadioChoice {
    fn default() -> Self {
        Self::Supplied(Arc::new(NoRadio))
    }
}

impl std::fmt::Debug for RadioChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Supplied(_) => f.write_str("RadioChoice::Supplied"),
            Self::HostBluetooth => f.write_str("RadioChoice::HostBluetooth"),
        }
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
    build_ports_with(
        app_data_dir,
        sink,
        notifications,
        secrets,
        approved_roots,
        RadioChoice::Supplied(radio),
        EngineOverrides::default(),
    )
    .await
}

/// Outside dependencies a caller substitutes for the ones this build would
/// otherwise reach for.
///
/// Production leaves every field empty. A test fills them so the whole
/// processing path — job states, arbitration, cancellation, progress, the
/// commit and the events — runs against engines that need no model file, no
/// Apple framework and no network.
#[derive(Clone, Default)]
pub struct EngineOverrides {
    /// Speech engines offered to the transcribe executor.
    pub stt: Vec<Arc<dyn plaudon_ai_contracts::stt::SpeechToText>>,
    /// The text model every summarization and tool run uses.
    pub text: Option<Arc<dyn plaudon_ai_contracts::text::TextModel>>,
    /// The HTTPS transport the cloud gateway dispatches through.
    ///
    /// `None` is the real one. A test that must prove nothing left the
    /// process supplies a transport that records and refuses, which is the
    /// only way to tell "no request was made" from "a request was made and
    /// failed".
    pub cloud_transport: Option<Arc<dyn plaudon_ai_openrouter::transport::AsyncHttpTransport>>,
    /// The voice-profile engines, when a caller substitutes them.
    ///
    /// `None` is this build's production arrangement: no diarizer, no
    /// embedder, and the OS credential store.
    pub voice: Option<VoiceEngines>,
    /// The model-setup transport, catalogue and toolchain directories
    /// (issue #303). Empty is production.
    pub models: crate::backend::models::ModelSetupSeams,
}

/// The voice-profile engines and key store a hermetic run substitutes.
///
/// The three travel together because they are useless apart: a match job
/// needs a diarizer and an embedder to produce a score at all, and the
/// enrolment it scores against is sealed with a key from the credential
/// store, so a run with a scripted embedder and the real OS keychain would
/// still touch the host. Production supplies none of them: this build ships
/// no ONNX tier, so a match job parks in `waiting_for_model` rather than
/// pretending a model could run (VP-042).
#[derive(Clone)]
pub struct VoiceEngines {
    /// Provider the matching executor diarizes with.
    pub diarizer: Arc<dyn plaudon_ai_diarization::contract::DiarizationProvider>,
    /// Embedder the matching executor scores clusters with.
    pub embedder: Arc<dyn plaudon_app_core::voice_profiles::embedder::SpeakerEmbedder>,
    /// Credential store the voice-profile data key lives in (VP-071).
    ///
    /// Separate from the application's own OpenRouter-key port, which is a
    /// different trait: the biometric tier needs the
    /// `plaudon_security::secrets` scope API (ADR-0014).
    pub secrets: Arc<dyn plaudon_security::secrets::SecretStore>,
}

impl std::fmt::Debug for VoiceEngines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VoiceEngines")
    }
}

impl std::fmt::Debug for EngineOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOverrides")
            .field("stt", &self.stt.len())
            .field("text", &self.text.is_some())
            .field("cloud_transport", &self.cloud_transport.is_some())
            .field("voice", &self.voice.is_some())
            .field("models", &self.models)
            .finish()
    }
}

/// [`build_ports_with_radio`] with the AI engines, and optionally the cloud
/// HTTPS transport, replaced.
///
/// The one reason to replace the transport is a test that must prove nothing
/// left the process: a transport that records and refuses is the only way to
/// tell "no request was made" apart from "a request was made and failed".
/// It travels inside [`EngineOverrides`] rather than as its own parameter
/// because it is the same kind of thing — a substitution for an outside
/// dependency a hermetic run must not reach — and because seven positional
/// arguments is as many as this function may carry.
pub async fn build_ports_with(
    app_data_dir: &Path,
    sink: Arc<dyn EventSink>,
    notifications: Arc<plaudon_notifications::Dispatcher>,
    secrets: Arc<dyn SecretStore>,
    approved_roots: Vec<PathBuf>,
    radio: RadioChoice,
    engines: EngineOverrides,
) -> Result<AppPorts, AppError> {
    let mut opened = LocalBackend::open(
        app_data_dir,
        sink,
        notifications,
        Arc::clone(&secrets),
        approved_roots,
    )
    .await?;
    if let Some(transport) = engines.cloud_transport {
        opened = opened.with_cloud_transport(transport);
    }
    if !engines.stt.is_empty() {
        opened = opened.with_stt_engines(engines.stt);
    }
    if let Some(text) = engines.text {
        opened = opened.with_text_model(text);
    }
    let model_seams = engines.models;
    if let Some(voice) = engines.voice {
        opened = opened
            .with_voice_engines(voice.diarizer, voice.embedder)
            .with_voice_secrets(voice.secrets);
    }
    let backend = Arc::new(opened);
    // Spec §13.3A, §14.3, §14.7: the import journal, the cloud ledger and the
    // job engine are all reconciled against what is actually on disk before
    // the UI can ask for anything and before a worker starts.
    if let Err(err) = backend.reconcile_startup().await {
        tracing::warn!(
            code = err.code().as_str(),
            "startup reconciliation did not complete"
        );
    }
    let processing = crate::backend::processing::ProcessingHandle::start(&backend, None);
    let events = backend.events().clone();
    // The library is open, so a real radio can now be given the committer it
    // needs. A supplied radio passes straight through.
    let radio: Arc<dyn DeviceRadio> = match radio {
        RadioChoice::Supplied(radio) => radio,
        #[cfg(feature = "hardware-scan")]
        RadioChoice::HostBluetooth => crate::backend::device_radio::host_bluetooth(
            backend.store().db(),
            backend.store().archive(),
        ),
        // Unreachable through the configuration API, which does not expose
        // `with_host_bluetooth` without the feature. `NoRadio` rather than a
        // panic: a binary with no radio code linked in genuinely has no radio,
        // and that is what the screen already says.
        #[cfg(not(feature = "hardware-scan"))]
        RadioChoice::HostBluetooth => Arc::new(NoRadio),
    };
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
        people: Arc::clone(&backend) as Arc<dyn PeoplePort>,
        voice_profiles: Arc::clone(&backend) as Arc<dyn VoiceProfilesPort>,
        models: Arc::new(
            crate::backend::models::ModelsBackend::new(app_data_dir, events.clone())
                .with_seams(model_seams),
        ),
        update: Arc::new(crate::backend::update::UpdateBackend::new(
            app_data_dir,
            Arc::clone(&backend) as Arc<dyn SettingsPort>,
        )),
        cloud: Arc::clone(&backend) as Arc<dyn CloudPort>,
        outputs: Arc::clone(&backend) as Arc<dyn OutputsPort>,
        tools: Arc::clone(&backend) as Arc<dyn ToolsPort>,
        templates: Arc::new(crate::backend::templates::TemplatesBackend::new(
            app_data_dir,
        )),
        devices: Arc::new(devices),
        media_export: Arc::clone(&backend) as Arc<dyn MediaExportPort>,
        deletion: Arc::clone(&backend) as Arc<dyn DeletionPort>,
        archive: backend as Arc<dyn ArchivePort>,
        secrets,
        events,
        processing,
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
