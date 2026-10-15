//! Local model management over the existing `plaudon-app-core` installer
//! (matrix row A13, spec §12, §14.4).
//!
//! The installer, the registry and the offline-pack reader already exist and
//! are tested; this backend is the port that lets the Settings screen reach
//! them, and it adds exactly three things:
//!
//! * **An approval gate with teeth.** The command layer refuses an install
//!   that does not carry the licence the manifest declares and an explicit
//!   acknowledgement of the footprint, so the disclosure cannot be skipped by
//!   a view that forgot to render it.
//! * **Observable progress.** `ModelPack::install` verifies, copies and
//!   renames atomically; re-implementing that to thread a callback through it
//!   would duplicate security-critical code. Instead the install runs on a
//!   blocking worker while a sampler watches the staging file grow and
//!   publishes `model.install_progress`. The figure is observed, never
//!   predicted, and the event stops as soon as the install returns.
//! * **No network, provably.** Only pack-sourced models are installable here,
//!   and a pack cannot reach a transport by construction, so there is no
//!   egress decision to get wrong — in Local-only Mode or out of it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use std::sync::atomic::AtomicBool;

use plaudon_app_core::models::{
    DiskUsage, HostResources, InstallOrigin, InstalledModel, ModelEntry, ModelError, ModelId,
    ModelPack, ModelRegistry, SystemResources, TransferError,
};
use plaudon_security::outbound::action::ActionEgressGrant;

use crate::backend::consent::ConsentStore;
use crate::ports::SettingsPort;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::time::Timestamp;
use tokio::sync::Mutex;

use crate::dto::{
    EventKind, InspectModelPackResponse, InstallModelResponse, ListLocalModelsResponse,
    LocalModelDto, ModelDiskUsageDto, ModelOfferDto, ModelOriginDto, RemoveLocalModelResponse,
    VerifyLocalModelResponse,
};
use crate::events::{EventDraft, EventPublisher};
use crate::ports::ModelsPort;
use crate::settings_presentation::{
    FootprintDisclosure, LicenceDisclosure, ModelInstallGate, MODEL_SOURCE_NOTE,
};

/// Directory under the application data directory holding installed weights.
pub const MODEL_ROOT_DIR: &str = "models";

/// How often the staging file is sampled while an install runs.
///
/// Slow enough that it costs nothing, fast enough that a multi-gigabyte copy
/// does not look frozen.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Opens the bytes of an approved catalogue download (issue #303).
///
/// The only argument that can reach the network is a URL the
/// [`ActionEgressGrant`] has authorized; an implementation must call
/// [`ActionEgressGrant::authorize_url`] before it connects and
/// [`ActionEgressGrant::authorize_redirect`] before it follows a hop.
pub trait ModelDownloadTransport: Send + Sync {
    /// Open `url`, positioned `resume_from` bytes in.
    fn open(
        &self,
        grant: &ActionEgressGrant,
        url: &str,
        resume_from: u64,
    ) -> Result<Box<dyn std::io::Read + Send>, TransferError>;
}

/// What a hermetic run substitutes in model setup (issue #303).
///
/// `None` everywhere is production: the real HTTPS transport, the committed
/// catalogue and the platform's search directories for node, npm and plaud.
#[derive(Clone, Default)]
pub struct ModelSetupSeams {
    /// How an approved download's bytes are fetched.
    pub transport: Option<Arc<dyn ModelDownloadTransport>>,
    /// The catalogue downloads and imports are checked against.
    pub catalogue: Option<plaudon_app_core::models::ModelManifest>,
    /// Directories searched for node, npm and plaud.
    pub toolchain_dirs: Option<Vec<PathBuf>>,
}

impl std::fmt::Debug for ModelSetupSeams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelSetupSeams")
            .field("transport", &self.transport.is_some())
            .field("catalogue", &self.catalogue.is_some())
            .field("toolchain_dirs", &self.toolchain_dirs)
            .finish()
    }
}

/// Local model management.
pub struct ModelsBackend {
    root: PathBuf,
    events: EventPublisher,
    resources: Arc<dyn SystemResources>,
    /// One install at a time: two concurrent installs would share one staging
    /// directory and one index file.
    install_lock: Mutex<()>,
    /// Where Local-only Mode is read from. `None` refuses every download, as
    /// if Local-only Mode were on: failing closed is the only safe default.
    settings: Option<Arc<dyn SettingsPort>>,
    /// The consent log every per-action grant is written to.
    consents: Option<ConsentStore>,
    /// How an approved download's bytes are fetched.
    transport: Arc<dyn ModelDownloadTransport>,
    /// Directories searched for node, npm and plaud; `None` is the platform's.
    toolchain_dirs: Option<Vec<PathBuf>>,
    /// Cancellation flags of running downloads, by model id.
    cancels: parking_lot::Mutex<std::collections::BTreeMap<String, Arc<AtomicBool>>>,
    /// The catalogue downloads and imports are checked against; `None` is the
    /// committed one.
    catalogue: Option<plaudon_app_core::models::ModelManifest>,
}

impl std::fmt::Debug for ModelsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsBackend")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl ModelsBackend {
    /// Open the model root inside `app_data_dir`.
    #[must_use]
    pub fn new(app_data_dir: &Path, events: EventPublisher) -> Self {
        Self::with_resources(app_data_dir, events, Arc::new(HostResources))
    }

    /// The same, with a resource probe a test can control.
    #[must_use]
    pub fn with_resources(
        app_data_dir: &Path,
        events: EventPublisher,
        resources: Arc<dyn SystemResources>,
    ) -> Self {
        Self {
            root: app_data_dir.join(MODEL_ROOT_DIR),
            events,
            resources,
            install_lock: Mutex::new(()),
            settings: None,
            consents: None,
            transport: Arc::new(RefusingTransport),
            toolchain_dirs: None,
            cancels: parking_lot::Mutex::new(std::collections::BTreeMap::new()),
            catalogue: None,
        }
    }

    /// Apply whatever a hermetic run substituted.
    #[must_use]
    pub fn with_seams(mut self, seams: ModelSetupSeams) -> Self {
        if let Some(transport) = seams.transport {
            self.transport = transport;
        }
        if let Some(dirs) = seams.toolchain_dirs {
            self.toolchain_dirs = Some(dirs);
        }
        self.catalogue = seams.catalogue;
        self
    }

    /// Read Local-only Mode from `settings` and record every per-action grant
    /// in `consents` (issue #303). Without this, downloads are refused.
    #[must_use]
    pub fn with_egress(mut self, settings: Arc<dyn SettingsPort>, consents: ConsentStore) -> Self {
        self.settings = Some(settings);
        self.consents = Some(consents);
        self
    }

    /// Replace the download transport. Tests use this to count requests.
    #[must_use]
    pub fn with_transport(mut self, transport: Arc<dyn ModelDownloadTransport>) -> Self {
        self.transport = transport;
        self
    }

    /// Search only `dirs` for node, npm and plaud. Tests use this.
    #[must_use]
    pub fn with_toolchain_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.toolchain_dirs = Some(dirs);
        self
    }

    /// The model root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn registry(&self) -> Result<ModelRegistry, AppError> {
        ModelRegistry::open(&self.root).map_err(AppError::from)
    }

    /// Bytes free on the volume holding the model root, when measurable.
    ///
    /// A probe failure is reported as "not measured", never as zero: a zero
    /// would make every install look impossible.
    fn available_disk_bytes(&self) -> Option<u64> {
        self.resources.available_disk_bytes(&self.root).ok()
    }

    /// Physical memory, when measurable.
    fn total_ram_bytes(&self) -> Option<u64> {
        self.resources.total_ram_bytes().ok()
    }

    fn model_id(raw: &str) -> Result<ModelId, AppError> {
        ModelId::parse(raw)
            .map_err(|_| AppError::new(AppErrorCode::InvalidArgument, "model id is not valid"))
    }

    async fn publish(&self, kind: EventKind, payload: serde_json::Value) {
        self.events
            .publish(EventDraft::new(kind, Timestamp::now()).with_hint("model", payload))
            .await;
    }

    /// Sample the staging file until `done` resolves, publishing what is
    /// actually on disk.
    async fn report_progress(&self, staging_path: PathBuf, total_bytes: u64) {
        let mut last = u64::MAX;
        loop {
            tokio::time::sleep(PROGRESS_INTERVAL).await;
            let received = tokio::fs::metadata(&staging_path)
                .await
                .map_or(0, |metadata| metadata.len());
            if received == last {
                continue;
            }
            last = received;
            self.publish(
                EventKind::ModelInstallProgress,
                serde_json::json!({
                    "received_bytes": received,
                    "total_bytes": total_bytes,
                }),
            )
            .await;
        }
    }
}

/// The transport of a backend nobody gave one: refuses everything.
#[derive(Debug, Clone, Copy)]
struct RefusingTransport;

impl ModelDownloadTransport for RefusingTransport {
    fn open(
        &self,
        _grant: &ActionEgressGrant,
        _url: &str,
        _resume_from: u64,
    ) -> Result<Box<dyn std::io::Read + Send>, TransferError> {
        Err(TransferError::Unavailable(
            "no download transport is configured".to_owned(),
        ))
    }
}

/// Open a pack, refusing anything whose payload does not hash to its manifest.
fn open_pack(pack_path: &Path) -> Result<ModelPack, AppError> {
    let pack =
        ModelPack::open(pack_path).map_err(|error| AppError::from(ModelError::Pack(error)))?;
    pack.verify()
        .map_err(|error| AppError::from(ModelError::Pack(error)))?;
    Ok(pack)
}

/// The approval gate for one manifest entry.
#[must_use]
pub fn gate_for(entry: &ModelEntry, total_ram_bytes: Option<u64>) -> ModelInstallGate {
    let footprint = FootprintDisclosure::from_entry(entry);
    ModelInstallGate {
        model_id: entry.id.to_string(),
        display_name: entry.display_name.clone(),
        licence: LicenceDisclosure::from_license(&entry.license),
        below_recommended_memory: total_ram_bytes
            .is_some_and(|ram| ram < footprint.recommended_ram_bytes),
        footprint,
    }
}

fn to_dto(model: &InstalledModel) -> LocalModelDto {
    LocalModelDto {
        model_id: model.id.to_string(),
        display_name: model.display_name.clone(),
        engine: model.engine.clone(),
        version: model.version.clone(),
        size_bytes: model.byte_length,
        licence_spdx_id: model.license_spdx.clone(),
        origin: match model.installed_from {
            InstallOrigin::OfflinePack => ModelOriginDto::Pack,
            InstallOrigin::Download => ModelOriginDto::Download,
            InstallOrigin::ImportedFile => ModelOriginDto::ImportedFile,
        },
        keep_offline: model.keep_offline,
    }
}

const fn disk_dto(usage: DiskUsage) -> ModelDiskUsageDto {
    ModelDiskUsageDto {
        installed_bytes: usage.installed_bytes,
        staging_bytes: usage.staging_bytes,
    }
}

#[async_trait]
impl ModelsPort for ModelsBackend {
    async fn list_local_models(&self) -> Result<ListLocalModelsResponse, AppError> {
        let registry = self.registry()?;
        Ok(ListLocalModelsResponse {
            models: registry.installed().into_iter().map(to_dto).collect(),
            disk: disk_dto(registry.disk_usage()),
            model_root: self.root.display().to_string(),
            available_disk_bytes: self.available_disk_bytes(),
            total_ram_bytes: self.total_ram_bytes(),
            source_note: MODEL_SOURCE_NOTE.to_owned(),
        })
    }

    async fn inspect_pack(&self, pack_path: PathBuf) -> Result<InspectModelPackResponse, AppError> {
        let registry = self.registry()?;
        let total_ram_bytes = self.total_ram_bytes();
        let pack = tokio::task::spawn_blocking(move || open_pack(&pack_path))
            .await
            .map_err(|_| {
                AppError::new(AppErrorCode::InternalError, "pack inspection panicked")
            })??;
        let offers = pack
            .models()
            .iter()
            .map(|entry| ModelOfferDto {
                already_installed: registry.find(&entry.id).is_some(),
                languages: entry
                    .languages
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                engine: entry.engine.clone(),
                version: entry.version.clone(),
                gate: gate_for(entry, total_ram_bytes),
            })
            .collect();
        Ok(InspectModelPackResponse {
            pack_path: pack.root().display().to_string(),
            artifact_digest: pack.artifact_digest().to_hex(),
            schema_version: pack.manifest().schema_version,
            offers,
            available_disk_bytes: self.available_disk_bytes(),
            total_ram_bytes,
        })
    }

    async fn install_from_pack(
        &self,
        pack_path: PathBuf,
        model_id: String,
        keep_offline: bool,
    ) -> Result<InstallModelResponse, AppError> {
        let id = Self::model_id(&model_id)?;
        let _guard = self.install_lock.lock().await;
        let root = self.root.clone();
        let staging_path = ModelRegistry::open(&root)
            .map_err(AppError::from)?
            .staging_dir()
            .join(format!("{id}.part"));

        let pack_for_open = pack_path.clone();
        let id_for_size = id.clone();
        let (pack, total_bytes) = tokio::task::spawn_blocking(move || {
            let pack = open_pack(&pack_for_open)?;
            let total = pack
                .models()
                .iter()
                .find(|entry| entry.id == id_for_size)
                .map(|entry| entry.size_bytes)
                .ok_or_else(|| {
                    AppError::from(ModelError::NotInManifest {
                        id: id_for_size.to_string(),
                    })
                })?;
            Ok::<_, AppError>((pack, total))
        })
        .await
        .map_err(|_| AppError::new(AppErrorCode::InternalError, "pack open panicked"))??;

        let install_id = id.clone();
        let install = tokio::task::spawn_blocking(move || {
            let mut registry = ModelRegistry::open(&root).map_err(AppError::from)?;
            let model = pack
                .install(&mut registry, &install_id, keep_offline)
                .map_err(AppError::from)?;
            Ok::<_, AppError>((model, registry.disk_usage()))
        });

        let (model, usage) = tokio::select! {
            result = install => result
                .map_err(|_| AppError::new(AppErrorCode::InternalError, "model install panicked"))??,
            () = self.report_progress(staging_path, total_bytes) => unreachable!(
                "the progress sampler never returns; it is cancelled when the install finishes"
            ),
        };

        self.publish(
            EventKind::ModelAvailabilityChanged,
            serde_json::json!({ "model_id": model.id.to_string(), "installed": true }),
        )
        .await;

        Ok(InstallModelResponse {
            transferred_bytes: model.byte_length,
            model: to_dto(&model),
            disk: disk_dto(usage),
        })
    }

    async fn remove_local_model(
        &self,
        model_id: String,
    ) -> Result<RemoveLocalModelResponse, AppError> {
        let id = Self::model_id(&model_id)?;
        let root = self.root.clone();
        let (report, usage) = tokio::task::spawn_blocking(move || {
            let mut registry = ModelRegistry::open(&root).map_err(AppError::from)?;
            let report = registry.delete(&id).map_err(AppError::from)?;
            Ok::<_, AppError>((report, registry.disk_usage()))
        })
        .await
        .map_err(|_| AppError::new(AppErrorCode::InternalError, "model removal panicked"))??;

        self.publish(
            EventKind::ModelAvailabilityChanged,
            serde_json::json!({ "model_id": report.id.to_string(), "installed": false }),
        )
        .await;

        Ok(RemoveLocalModelResponse {
            bytes_freed: report.freed_bytes,
            disk: disk_dto(usage),
        })
    }

    async fn verify_local_model(
        &self,
        model_id: String,
    ) -> Result<VerifyLocalModelResponse, AppError> {
        let id = Self::model_id(&model_id)?;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let registry = ModelRegistry::open(&root).map_err(AppError::from)?;
            let expected_bytes = registry
                .find(&id)
                .map(|model| model.byte_length)
                .ok_or_else(|| AppError::from(ModelError::NotInstalled { id: id.to_string() }))?;
            let report = registry.verify(&id).map_err(AppError::from)?;
            let observed_bytes = report.observed_bytes.unwrap_or(0);
            let detail = if report.intact {
                "The installed bytes still match the digest recorded at install time.".to_owned()
            } else if report.observed_bytes.is_none() {
                "The weight file is missing. Install the model again.".to_owned()
            } else {
                "The installed bytes no longer match the digest recorded at install time. \
                 Remove the model and install it again."
                    .to_owned()
            };
            Ok::<_, AppError>(VerifyLocalModelResponse {
                model_id: report.id.to_string(),
                intact: report.intact,
                observed_bytes,
                expected_bytes,
                detail,
            })
        })
        .await
        .map_err(|_| AppError::new(AppErrorCode::InternalError, "model verification panicked"))?
    }

    async fn install_download(
        &self,
        _request: crate::dto::InstallModelDownloadRequest,
    ) -> Result<InstallModelResponse, AppError> {
        Err(AppError::new(
            AppErrorCode::ModelUnavailable,
            "catalogue downloads are not implemented yet",
        ))
    }

    async fn cancel_install(
        &self,
        _model_id: String,
    ) -> Result<crate::dto::CancelModelInstallResponse, AppError> {
        Ok(crate::dto::CancelModelInstallResponse { cancelled: false })
    }

    async fn import_file(
        &self,
        _path: PathBuf,
        _model_id: Option<String>,
    ) -> Result<InstallModelResponse, AppError> {
        Err(AppError::new(
            AppErrorCode::ModelUnavailable,
            "importing a model file is not implemented yet",
        ))
    }

    async fn detect_plaud_toolchain(&self) -> Result<crate::dto::PlaudToolchainStatus, AppError> {
        Err(AppError::new(
            AppErrorCode::CliMissing,
            "toolchain detection is not implemented yet",
        ))
    }

    async fn install_plaud_cli(
        &self,
        _request: crate::dto::InstallPlaudCliRequest,
    ) -> Result<crate::dto::PlaudToolchainStatus, AppError> {
        Err(AppError::new(
            AppErrorCode::CliMissing,
            "the guided install is not implemented yet",
        ))
    }
}
