//! Recording sources: the PLAUD cloud connector and watched folders.
//!
//! The commands never see the connector, the process runner or the folder
//! service; they call [`crate::ports::SourcesPort`], which this module
//! implements over [`LocalBackend`]. Three rules are enforced here and nowhere
//! else:
//!
//! * **Local-only Mode is not special-cased.** The connector is built over an
//!   [`OutboundPolicy`] that matches the stored preference, so the policy
//!   itself refuses the launch (spec §22(15)); this module only rebuilds the
//!   connector when the preference flips.
//! * **No vendor text crosses this boundary.** Failures are
//!   [`plaudon_recording_sources::source::SourceError`] values, converted to
//!   [`AppError`] with a fixed detail, and the diagnostics copy comes from
//!   `plaudon_source_plaud_cli::diagnostics`, never from CLI output (§7.6).
//! * **An import ID must come from the last listing** (§15.1): the allowlist
//!   is the set of items the last `plaud_list` returned, so the frontend can
//!   never name an ID the connector did not just enumerate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use plaudon_app_core::import::{ImportOutcome as CoreOutcome, ImportRequest};
use plaudon_app_core::reconciliation::{
    ObservedItem, ReconcileAction, ReconcileDecision, ReconciliationService, SourceEvent,
};
use plaudon_app_core::vendor_artifacts::VendorArtifactImporter;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, RecordingId, SourceId};
use plaudon_domain::source::{SourceAccountScope, SourceKind};
use plaudon_domain::time::Timestamp;
use plaudon_notifications::privacy::NotificationContext;
use plaudon_recording_sources::source::{SourceError, SourceItem};
use plaudon_security::outbound::subprocess::GatedProcessLauncher;
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
use plaudon_source_folder::config::{FolderRule, WatchedFolderConfig};
use plaudon_source_folder::error::FolderError;
use plaudon_source_folder::service::{SyncReport, WatchedFolderService};
use plaudon_source_plaud_cli::acquire::{part_path, AcquisitionPipeline};
use plaudon_source_plaud_cli::compatibility::CliCompatibilityTable;
use plaudon_source_plaud_cli::connector::PlaudCloudCliConnector;
use plaudon_source_plaud_cli::diagnostics::ConnectorDiagnostics;
use plaudon_source_plaud_cli::discovery::CliDiscovery;
use plaudon_source_plaud_cli::download::{DownloadPolicy, UrlContract};
use plaudon_source_plaud_cli::parser::ListingCompleteness;
use plaudon_source_plaud_cli::runner::{CliRunner, UserInitiated};
use plaudon_source_plaud_cli::state::ConnectorState;
use plaudon_source_plaud_cli::transport::ReqwestFetcher;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::backend::store::{rfc3339, rfc3339_opt, storage_error};
use crate::backend::{activity, jobs, LocalBackend};
use crate::dto::{
    ActivityTarget, ActivityTargetKind, EventKind, FolderSyncCounts, JobKind, JobState,
    ListSourceArtifactsResponse, NotificationCategory, OutputLink, OutputLinkKind,
    PlaudCompatibility, PlaudConnectorSnapshot, PlaudConnectorStatus, PlaudDiagnostics,
    PlaudDisconnectCopy, PlaudDisconnectResponse, PlaudImportOutcome, PlaudImportResponse,
    PlaudItemSummary, PlaudListResponse, PlaudListingCompleteness, ReconcileDecisionSummary,
    SourceAddFolderRequest, SourceAddFolderResponse, SourceArtifactSummary,
    SourceReconcileResponse, SourceSyncResponse, SyncTrigger, SyncedSourceKind, WatchedFolderRule,
};
use crate::error::{envelope, internal, invalid_argument};
use crate::events::EventDraft;
use crate::ports::{SettingsPort, SourcesPort};
use crate::validate::validate_folder_path;

/// Account scope of the PLAUD cloud connector's identity triples (spec §7.2).
///
/// The official CLI exposes no account or region identifier, so every item the
/// connector imports is scoped to "the session of this CLI on this machine".
pub const PLAUD_ACCOUNT_SCOPE: &str = "plaud_cloud_cli:local_session";

/// Version of the privacy disclosure recorded with the network grant that
/// lets the connector launch the CLI (spec §4.4(5)).
pub const PRIVACY_DISCLOSURE_VERSION: &str = "privacy-v1";

/// Most listing pages one sync or reconcile will follow.
pub const MAX_LISTING_PAGES: usize = 64;

/// The action label recorded for each user-initiated CLI launch.
const PROBE_INTENT: &str = "probe_plaud_cli";
const LIST_INTENT: &str = "list_plaud_recordings";
const IMPORT_INTENT: &str = "import_plaud_recordings";
const SYNC_INTENT: &str = "sync_plaud_recordings";
const DISCONNECT_INTENT: &str = "disconnect_plaud_account";

/* --------------------------------------------------------------- connector */

/// A connector together with the facts the command layer needs about it.
#[derive(Debug)]
pub struct BuiltConnector {
    /// The connector itself.
    pub connector: PlaudCloudCliConnector,
    /// The compatibility table the connector was built with, used to render
    /// error copy with the same list of tested versions.
    pub table: Arc<CliCompatibilityTable>,
    /// Scope of the identity triples the connector produces.
    pub account_scope: SourceAccountScope,
}

/// Builds the connector for the current Local-only Mode setting.
///
/// Production uses [`ProductionConnectorFactory`]; tests inject a factory
/// whose runner points at a scripted executable.
pub trait ConnectorFactory: std::fmt::Debug + Send + Sync {
    /// Build a connector whose process runner is gated by the policy that
    /// matches `local_only`.
    fn build(&self, local_only: bool, now_ms: i64) -> Result<BuiltConnector, AppError>;
}

/// The connector the shipped application runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProductionConnectorFactory;

impl ConnectorFactory for ProductionConnectorFactory {
    fn build(&self, local_only: bool, now_ms: i64) -> Result<BuiltConnector, AppError> {
        let policy = if local_only {
            OutboundPolicy::local_only()
        } else {
            OutboundPolicy::network_permitted(NetworkGrant::new(
                ApprovalSource::UserSettingsConfirmation,
                now_ms / 1000,
                PRIVACY_DISCLOSURE_VERSION,
            ))
        };
        let runner = CliRunner::new(
            GatedProcessLauncher::new(Arc::new(policy)),
            Arc::new(plaudon_diagnostics::redact::Redactor::new()),
        );
        // No vendor download host has been observed yet, so the default
        // policy's host list is empty and every download is refused until a
        // host is recorded in the URL contract (spec §7.3).
        let download = Arc::new(DownloadPolicy::default());
        let fetcher = ReqwestFetcher::new(Arc::clone(&download))
            .map_err(|err| internal(format!("cannot build the download client: {err}")))?;
        let pipeline = AcquisitionPipeline::new(
            download,
            Arc::new(fetcher),
            UrlContract::undocumented("plaud.ai"),
        );
        let account_scope = SourceAccountScope::new(PLAUD_ACCOUNT_SCOPE);
        let connector = PlaudCloudCliConnector::new(
            CliDiscovery::new(),
            runner,
            pipeline,
            account_scope.clone(),
            now_ms,
        );
        Ok(BuiltConnector {
            connector,
            table: Arc::new(CliCompatibilityTable::committed().clone()),
            account_scope,
        })
    }
}

#[derive(Debug, Clone)]
struct Handle {
    connector: Arc<PlaudCloudCliConnector>,
    table: Arc<CliCompatibilityTable>,
    scope: SourceAccountScope,
}

#[derive(Debug, Default)]
struct Session {
    local_only: Option<bool>,
    handle: Option<Handle>,
    /// Items of the last listing; the only IDs `plaud_import` accepts.
    listing: BTreeMap<String, SourceItem>,
    /// The failure of the last connect, sign-in or listing attempt.
    last_failure: Option<SourceError>,
    /// The user dropped the association; probing must not silently restore it.
    disconnected: bool,
}

/// Runtime state of the sources port: the connector session and the folder
/// services, both rebuilt from durable state when missing.
#[derive(Debug, Clone)]
pub struct SourcesRuntime {
    factory: Arc<dyn ConnectorFactory>,
    session: Arc<Mutex<Session>>,
    folders: Arc<Mutex<BTreeMap<Uuid, Arc<WatchedFolderService>>>>,
}

impl SourcesRuntime {
    /// A runtime over the given connector factory.
    #[must_use]
    pub fn new(factory: Arc<dyn ConnectorFactory>) -> Self {
        Self {
            factory,
            session: Arc::new(Mutex::new(Session::default())),
            folders: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// The runtime the shipped application uses.
    #[must_use]
    pub fn production() -> Self {
        Self::new(Arc::new(ProductionConnectorFactory))
    }

    fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn folders(&self) -> MutexGuard<'_, BTreeMap<Uuid, Arc<WatchedFolderService>>> {
        self.folders.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The connector for the current setting. A flipped Local-only Mode
    /// preference builds a fresh connector under the matching policy and
    /// forgets the previous listing.
    fn handle(&self, local_only: bool, now_ms: i64) -> Result<Handle, AppError> {
        let mut session = self.session();
        if session.local_only != Some(local_only) || session.handle.is_none() {
            let built = self.factory.build(local_only, now_ms)?;
            *session = Session {
                local_only: Some(local_only),
                handle: Some(Handle {
                    connector: Arc::new(built.connector),
                    table: built.table,
                    scope: built.account_scope,
                }),
                ..Session::default()
            };
        }
        session
            .handle
            .clone()
            .ok_or_else(|| internal("the connector session has no connector"))
    }

    fn record_failure(&self, error: SourceError) -> SourceError {
        self.session().last_failure = Some(error.clone());
        error
    }

    fn clear_failure(&self) {
        self.session().last_failure = None;
    }
}

impl LocalBackend {
    /// Replace the connector factory. Tests use this to point the runner at a
    /// scripted executable; the shipped application never calls it.
    #[must_use]
    pub fn with_connector_factory(mut self, factory: Arc<dyn ConnectorFactory>) -> Self {
        self.source_runtime = SourcesRuntime::new(factory);
        self
    }

    async fn plaud_handle(&self) -> Result<Handle, AppError> {
        let local_only = self.preferences().await?.local_only_mode;
        self.source_runtime
            .handle(local_only, Timestamp::now().as_millis())
    }

    fn snapshot(&self, handle: &Handle) -> PlaudConnectorSnapshot {
        let status = handle.connector.status();
        let failure = self.source_runtime.session().last_failure.clone();
        // State-based copy is richer for two states (it names the detected
        // version, or says the session expired); everywhere else the copy of
        // the failure that just happened says more than the state does. A
        // launch the policy denied changes no state at all, so the failure is
        // the only record that Local-only Mode is why nothing happened.
        let diagnostics = match failure {
            Some(error)
                if !matches!(
                    status.state,
                    ConnectorState::IncompatibleVersion | ConnectorState::ExpiredAuth
                ) =>
            {
                ConnectorDiagnostics::for_error(&status, error, &handle.table)
            }
            _ => handle.connector.diagnostics(),
        };
        let copy = handle.connector.disconnect_copy();
        PlaudConnectorSnapshot {
            status: PlaudConnectorStatus {
                state: status.state,
                detected_version: status.detected_version.clone(),
                version_validated: status.version_validated,
                program_origin: status.program_origin.clone(),
                last_error: status.last_error,
                listing_possibly_incomplete: status.listing_possibly_incomplete,
                scraping_warning: status.scraping_warning,
                updated_at_ms: status.updated_at_ms,
            },
            diagnostics: PlaudDiagnostics {
                state: diagnostics.state,
                headline: diagnostics.headline,
                detail: diagnostics.detail,
                next_action: diagnostics.next_action,
                detected_version: diagnostics.detected_version,
                compatibility: match diagnostics.compatibility {
                    "tested" => PlaudCompatibility::Tested,
                    "untested" => PlaudCompatibility::Untested,
                    _ => PlaudCompatibility::Unknown,
                },
                program_origin: diagnostics.program_origin,
                last_error: diagnostics.last_error,
                warnings: diagnostics
                    .warnings
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            disconnect: PlaudDisconnectCopy {
                variant: copy,
                label: copy.label().to_owned(),
                body: copy.body().to_owned(),
            },
        }
    }

    /// Publish `source.auth_required` when a failure means the user must sign
    /// in (spec §15.3).
    async fn announce_auth(&self, error: &SourceError) {
        if matches!(
            error,
            SourceError::AuthenticationRequired | SourceError::AuthCanceled
        ) {
            self.events()
                .publish(
                    EventDraft::new(EventKind::SourceAuthRequired, Timestamp::now())
                        .with_hint("source", json!("plaud_cloud_cli"))
                        .with_hint("code", json!(AppError::from(error.clone()).code().as_str())),
                )
                .await;
        }
    }

    async fn announce_sync_finished(
        &self,
        source: &str,
        operation: &str,
        job_id: Option<JobId>,
        outcome: Result<(), &AppError>,
        counts: serde_json::Value,
    ) {
        let mut draft = EventDraft::new(EventKind::SourceSyncFinished, Timestamp::now())
            .with_hint("source", json!(source))
            .with_hint("operation", json!(operation))
            .with_hint("counts", counts);
        draft = match outcome {
            Ok(()) => draft.with_hint("outcome", json!("succeeded")),
            Err(error) => draft
                .with_hint("outcome", json!("failed"))
                .with_hint("code", json!(error.code().as_str())),
        };
        if let Some(job_id) = job_id {
            draft = draft.with_job(job_id);
        }
        self.events().publish(draft).await;
    }

    async fn plaud_source_id(&self, scope: &SourceAccountScope) -> Result<SourceId, AppError> {
        self.sources()
            .ensure_source(
                SourceKind::PlaudCloudCli,
                scope,
                "PLAUD cloud",
                Timestamp::now(),
            )
            .await
            .map_err(AppError::from)
    }
}

/* ----------------------------------------------------------------- listing */

/// Follow every page of the listing, within [`MAX_LISTING_PAGES`].
async fn enumerate_all(
    connector: &PlaudCloudCliConnector,
    intent: &UserInitiated,
) -> Result<(Vec<SourceItem>, ListingCompleteness), SourceError> {
    let mut items = Vec::new();
    let mut completeness = ListingCompleteness::Complete;
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_LISTING_PAGES {
        let page = connector.enumerate(cursor.as_deref(), intent).await?;
        if page.completeness == ListingCompleteness::PossiblyIncomplete {
            completeness = ListingCompleteness::PossiblyIncomplete;
        }
        items.extend(page.page.items);
        match page.page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok((items, completeness)),
        }
    }
    // The page cap stopped us before the listing ended.
    Ok((items, ListingCompleteness::PossiblyIncomplete))
}

fn external_id_of(item: &SourceItem) -> Option<String> {
    item.identity
        .external_id
        .as_ref()
        .map(|id| id.as_str().to_owned())
}

fn completeness_dto(completeness: ListingCompleteness) -> PlaudListingCompleteness {
    match completeness {
        ListingCompleteness::Complete => PlaudListingCompleteness::Complete,
        ListingCompleteness::PossiblyIncomplete => PlaudListingCompleteness::PossiblyIncomplete,
    }
}

/* ------------------------------------------------------------------- jobs */

/// A `source_sync` job opened for one pass.
struct PassJob {
    id: JobId,
}

impl LocalBackend {
    async fn open_pass(
        &self,
        label: &str,
        source_id: &str,
        trigger: &str,
    ) -> Result<PassJob, AppError> {
        let id = jobs::create(
            self.pool(),
            &jobs::JobDraft {
                kind: JobKind::SourceSync,
                label: label.to_owned(),
                recording_id: None,
                config_snapshot: json!({ "source_id": source_id, "trigger": trigger }),
            },
        )
        .await?;
        jobs::transition(
            self.pool(),
            id,
            JobState::RunningLocal,
            Some("syncing"),
            None,
        )
        .await?;
        publish_job(self, id).await;
        Ok(PassJob { id })
    }

    async fn close_pass_ok(&self, job: &PassJob) -> Result<(), AppError> {
        let mut tx = plaudon_persistence::begin_write(self.pool())
            .await
            .map_err(|err| storage_error("cannot begin a source sync commit", &err))?;
        jobs::complete_in_tx(&mut tx, job.id, None).await?;
        tx.commit()
            .await
            .map_err(|err| storage_error("cannot commit a source sync", &err))?;
        publish_job(self, job.id).await;
        Ok(())
    }

    /// Record `error` on the job and return it for the command envelope.
    async fn close_pass_err(&self, job: &PassJob, error: AppError) -> AppError {
        if let Err(secondary) = jobs::fail(self.pool(), job.id, &error).await {
            tracing::warn!(
                code = secondary.code().as_str(),
                "a failed sync could not be recorded on its job"
            );
        }
        publish_job(self, job.id).await;
        error
    }
}

async fn publish_job(backend: &LocalBackend, job_id: JobId) {
    if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
        jobs::publish_update(backend.events(), &summary).await;
    }
}

/* -------------------------------------------------------------- the port */

#[async_trait]
impl SourcesPort for LocalBackend {
    async fn plaud_probe(&self) -> Result<crate::dto::PlaudConnectorSnapshot, AppError> {
        let handle = self.plaud_handle().await?;
        let needs_discovery = matches!(
            handle.connector.status().state,
            ConnectorState::Unconfigured | ConnectorState::IncompatibleVersion
        ) && !self.source_runtime.session().disconnected;
        if needs_discovery {
            // A read-only `--version` launch; failures are recorded on the
            // connector and surface as diagnostics, not as a command error.
            let intent = UserInitiated::new(PROBE_INTENT, Timestamp::now().as_millis());
            match handle.connector.connect(None, &intent).await {
                Ok(_) => self.source_runtime.clear_failure(),
                Err(error) => {
                    self.source_runtime.record_failure(error);
                }
            }
        }
        Ok(self.snapshot(&handle))
    }

    async fn plaud_connect(
        &self,
        user_intent: &'static str,
    ) -> Result<crate::dto::PlaudConnectorSnapshot, AppError> {
        let handle = self.plaud_handle().await?;
        self.source_runtime.session().disconnected = false;
        let intent = UserInitiated::new(user_intent, Timestamp::now().as_millis());

        let result = async {
            handle.connector.connect(None, &intent).await?;
            handle.connector.sign_in(&intent).await
        }
        .await;
        match result {
            Ok(_) => {
                self.source_runtime.clear_failure();
                Ok(self.snapshot(&handle))
            }
            Err(error) => {
                self.source_runtime.record_failure(error.clone());
                self.announce_auth(&error).await;
                Err(AppError::from(error))
            }
        }
    }

    async fn plaud_disconnect(&self) -> Result<PlaudDisconnectResponse, AppError> {
        let handle = self.plaud_handle().await?;
        let intent = UserInitiated::new(DISCONNECT_INTENT, Timestamp::now().as_millis());
        let copy = handle
            .connector
            .disconnect(&intent)
            .await
            .map_err(AppError::from)?;
        {
            let mut session = self.source_runtime.session();
            session.listing.clear();
            session.last_failure = None;
            session.disconnected = true;
        }
        let snapshot = self.snapshot(&handle);
        Ok(PlaudDisconnectResponse {
            disconnect: PlaudDisconnectCopy {
                variant: copy,
                label: copy.label().to_owned(),
                body: copy.body().to_owned(),
            },
            snapshot,
        })
    }

    async fn plaud_list(
        &self,
        cursor: Option<String>,
    ) -> Result<crate::dto::PlaudListResponse, AppError> {
        let handle = self.plaud_handle().await?;
        let intent = UserInitiated::new(LIST_INTENT, Timestamp::now().as_millis());
        let listing = match handle.connector.enumerate(cursor.as_deref(), &intent).await {
            Ok(listing) => listing,
            Err(error) => {
                self.source_runtime.record_failure(error.clone());
                self.announce_auth(&error).await;
                let error = AppError::from(error);
                self.announce_sync_finished(
                    "plaud_cloud_cli",
                    "list",
                    None,
                    Err(&error),
                    json!({}),
                )
                .await;
                return Err(error);
            }
        };
        self.source_runtime.clear_failure();

        let source_id = self.plaud_source_id(&handle.scope).await?;
        let mut items = Vec::with_capacity(listing.page.items.len());
        for item in &listing.page.items {
            let Some(external_id) = external_id_of(item) else {
                continue;
            };
            let imported = self
                .sources()
                .find_by_identity(source_id, &item.identity)
                .await
                .map_err(AppError::from)?
                .filter(|association| association.tombstone_reason.is_none())
                .map(|association| association.recording_id);
            items.push(PlaudItemSummary {
                external_id,
                display_name: item.display_name.clone(),
                captured_at: rfc3339_opt(
                    "listing capture time",
                    item.captured_at.map(Timestamp::as_millis),
                )?,
                duration_ms: item.duration_ms,
                declared_byte_length: item.declared_byte_length,
                imported_recording_id: imported,
            });
        }

        {
            let mut session = self.source_runtime.session();
            if cursor.is_none() {
                session.listing.clear();
            }
            for item in listing.page.items {
                if let Some(id) = external_id_of(&item) {
                    session.listing.insert(id, item);
                }
            }
        }

        let count = items.len();
        self.announce_sync_finished(
            "plaud_cloud_cli",
            "list",
            None,
            Ok(()),
            json!({ "listed": count }),
        )
        .await;
        Ok(PlaudListResponse {
            source_id: source_id.to_string(),
            items,
            next_cursor: listing.page.next_cursor,
            completeness: completeness_dto(listing.completeness),
            empty_account_advisory: listing.empty_account_advisory.map(str::to_owned),
        })
    }

    async fn plaud_import(
        &self,
        external_ids: Vec<String>,
    ) -> Result<PlaudImportResponse, AppError> {
        let handle = self.plaud_handle().await?;

        // §15.1: only IDs from the last listing may be imported. The whole
        // request is refused, so a forged ID never reaches the connector.
        let items: Vec<SourceItem> = {
            let session = self.source_runtime.session();
            let mut items = Vec::with_capacity(external_ids.len());
            for id in &external_ids {
                match session.listing.get(id) {
                    Some(item) => items.push(item.clone()),
                    None => {
                        return Err(invalid_argument(
                            "a requested recording was not in the last listing",
                        ))
                    }
                }
            }
            items
        };

        let source_id = self.plaud_source_id(&handle.scope).await?;
        let intent = UserInitiated::new(IMPORT_INTENT, Timestamp::now().as_millis());
        let mut results = Vec::with_capacity(items.len());
        let (mut imported, mut failed) = (0u32, 0u32);
        for item in items {
            let outcome = self.import_one(&handle, source_id, &item, &intent).await?;
            match &outcome {
                PlaudImportOutcome::Imported { .. } => imported += 1,
                PlaudImportOutcome::Failed { .. } => failed += 1,
                PlaudImportOutcome::AlreadyImported { .. } => {}
            }
            results.push(outcome);
        }
        // A failed import is reported per item; the pass itself finished.
        self.announce_sync_finished(
            "plaud_cloud_cli",
            "import",
            None,
            Ok(()),
            json!({ "imported": imported, "failed": failed }),
        )
        .await;
        Ok(PlaudImportResponse { results })
    }

    async fn source_add_folder(
        &self,
        request: SourceAddFolderRequest,
    ) -> Result<SourceAddFolderResponse, AppError> {
        let root = validate_folder_path(&request.path, self.approved_roots())?;
        let config = WatchedFolderConfig::new(&root, folder_rule(request.rule))
            .map_err(folder_error)?
            .recursive(request.recursive);
        let service =
            WatchedFolderService::new(config, self.store().db().clone(), self.archive().clone())
                .await
                .map_err(folder_error)?;
        let id = service.folder_id();
        // Re-adding a folder restates its policy: the stored row follows the
        // request, and any cached service is replaced by this one.
        sqlx::query("UPDATE watched_folders SET rule = ?2, recursive = ?3 WHERE id = ?1")
            .bind(id.to_string())
            .bind(request.rule.as_str())
            .bind(i64::from(request.recursive))
            .execute(self.pool())
            .await
            .map_err(|err| storage_error("cannot store the watched folder policy", &err))?;
        self.source_runtime.folders().insert(id, Arc::new(service));
        Ok(SourceAddFolderResponse {
            source_id: id.to_string(),
            rule: request.rule,
            recursive: request.recursive,
        })
    }

    async fn watched_folder_ids(&self) -> Result<Vec<String>, AppError> {
        // `paused` is the one status that means "the user asked for this
        // folder not to be swept". `unavailable` and `permission_denied` are
        // facts about the disk at the time of the last pass — an unmounted
        // volume, a folder the sandbox refused — and they clear themselves
        // when the next pass succeeds, so a timer that skipped them would
        // never find out that they had.
        let rows = sqlx::query(
            "SELECT id FROM watched_folders WHERE status <> 'paused' ORDER BY created_at, id",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot list the watched folders", &err))?;
        Ok(rows.iter().map(|row| row.get::<String, _>("id")).collect())
    }

    async fn source_sync(
        &self,
        source_id: Uuid,
        trigger: SyncTrigger,
    ) -> Result<SourceSyncResponse, AppError> {
        let trigger_label = match trigger {
            SyncTrigger::Manual => "manual",
            SyncTrigger::Background => "background",
        };
        if let Some(service) = self.folder_service(source_id).await? {
            let job = self
                .open_pass("Sync watched folder", &source_id.to_string(), trigger_label)
                .await?;
            return match run_folder_sync(&service).await {
                Ok(report) => {
                    self.close_pass_ok(&job).await?;
                    self.publish_folder_report(&job, &report).await;
                    Ok(SourceSyncResponse {
                        job_id: job.id,
                        source_id: source_id.to_string(),
                        source_kind: SyncedSourceKind::WatchedFolder,
                        state: JobState::Completed,
                        folder: Some(folder_counts(&report)),
                        listed: None,
                        imported_recording_ids: report.imported,
                    })
                }
                Err(error) => {
                    let error = self.close_pass_err(&job, folder_error(error)).await;
                    self.announce_sync_finished(
                        "watched_folder",
                        "sync",
                        Some(job.id),
                        Err(&error),
                        json!({}),
                    )
                    .await;
                    Err(error)
                }
            };
        }

        // The PLAUD cloud is only ever synced because the user asked: an
        // unattended pass would launch the vendor CLI in the background
        // (spec §7.1, §24).
        if trigger == SyncTrigger::Background {
            return Err(invalid_argument(
                "the PLAUD cloud connector does not sync in the background",
            ));
        }
        let handle = self.plaud_handle().await?;
        self.require_plaud_source(&handle, source_id).await?;
        let job = self
            .open_pass(
                "Refresh PLAUD recordings",
                &source_id.to_string(),
                trigger_label,
            )
            .await?;
        let intent = UserInitiated::new(SYNC_INTENT, Timestamp::now().as_millis());
        match enumerate_all(&handle.connector, &intent).await {
            Ok((items, _completeness)) => {
                let listed = items.len();
                {
                    let mut session = self.source_runtime.session();
                    session.listing.clear();
                    for item in items {
                        if let Some(id) = external_id_of(&item) {
                            session.listing.insert(id, item);
                        }
                    }
                }
                self.source_runtime.clear_failure();
                self.close_pass_ok(&job).await?;
                self.announce_sync_finished(
                    "plaud_cloud_cli",
                    "sync",
                    Some(job.id),
                    Ok(()),
                    json!({ "listed": listed }),
                )
                .await;
                Ok(SourceSyncResponse {
                    job_id: job.id,
                    source_id: source_id.to_string(),
                    source_kind: SyncedSourceKind::PlaudCloud,
                    state: JobState::Completed,
                    folder: None,
                    listed: Some(u32::try_from(listed).unwrap_or(u32::MAX)),
                    imported_recording_ids: Vec::new(),
                })
            }
            Err(error) => {
                self.source_runtime.record_failure(error.clone());
                self.announce_auth(&error).await;
                let error = self.close_pass_err(&job, AppError::from(error)).await;
                self.announce_sync_finished(
                    "plaud_cloud_cli",
                    "sync",
                    Some(job.id),
                    Err(&error),
                    json!({}),
                )
                .await;
                Err(error)
            }
        }
    }

    async fn source_reconcile(&self, source_id: Uuid) -> Result<SourceReconcileResponse, AppError> {
        if let Some(service) = self.folder_service(source_id).await? {
            let job = self
                .open_pass("Reconcile watched folder", &source_id.to_string(), "manual")
                .await?;
            return match run_folder_sync(&service).await {
                Ok(report) => {
                    self.close_pass_ok(&job).await?;
                    self.publish_folder_report(&job, &report).await;
                    Ok(SourceReconcileResponse {
                        job_id: job.id,
                        source_id: source_id.to_string(),
                        source_kind: SyncedSourceKind::WatchedFolder,
                        decisions: Vec::new(),
                        listing_complete: None,
                        folder: Some(folder_counts(&report)),
                    })
                }
                Err(error) => {
                    let error = self.close_pass_err(&job, folder_error(error)).await;
                    self.announce_sync_finished(
                        "watched_folder",
                        "reconcile",
                        Some(job.id),
                        Err(&error),
                        json!({}),
                    )
                    .await;
                    Err(error)
                }
            };
        }

        let handle = self.plaud_handle().await?;
        let plaud_source = self.require_plaud_source(&handle, source_id).await?;
        let job = self
            .open_pass(
                "Reconcile PLAUD recordings",
                &source_id.to_string(),
                "manual",
            )
            .await?;
        let intent = UserInitiated::new(SYNC_INTENT, Timestamp::now().as_millis());
        let (items, completeness) = match enumerate_all(&handle.connector, &intent).await {
            Ok(listed) => listed,
            Err(error) => {
                self.source_runtime.record_failure(error.clone());
                self.announce_auth(&error).await;
                let error = self.close_pass_err(&job, AppError::from(error)).await;
                self.announce_sync_finished(
                    "plaud_cloud_cli",
                    "reconcile",
                    Some(job.id),
                    Err(&error),
                    json!({}),
                )
                .await;
                return Err(error);
            }
        };
        self.source_runtime.clear_failure();

        // A listing that may be partial cannot say which items are gone, and
        // reconciliation marks every unlisted item absent. Spec §7.5 forbids
        // concluding removal from an incomplete listing, so the pass records
        // nothing and says why.
        let decisions = if completeness == ListingCompleteness::Complete {
            let observed: Vec<ObservedItem> = items
                .iter()
                .map(|item| {
                    let mut observed =
                        ObservedItem::listed(item.identity.clone(), item.display_name.clone());
                    if let Some(version) = &item.remote_version {
                        observed = observed.with_remote_version(version.clone());
                    }
                    observed
                })
                .collect();
            let service =
                ReconciliationService::new(self.store().db().clone(), self.archive().clone());
            match service
                .reconcile_listing(plaud_source, &observed, Timestamp::now())
                .await
            {
                Ok(decisions) => decisions,
                Err(error) => {
                    let error = self.close_pass_err(&job, AppError::from(error)).await;
                    self.announce_sync_finished(
                        "plaud_cloud_cli",
                        "reconcile",
                        Some(job.id),
                        Err(&error),
                        json!({}),
                    )
                    .await;
                    return Err(error);
                }
            }
        } else {
            Vec::new()
        };

        self.close_pass_ok(&job).await?;
        self.publish_source_updates(plaud_source, &decisions).await;
        let summaries: Vec<ReconcileDecisionSummary> =
            decisions.iter().map(decision_summary).collect();
        self.announce_sync_finished(
            "plaud_cloud_cli",
            "reconcile",
            Some(job.id),
            Ok(()),
            json!({ "decisions": summaries.len(), "listing_complete": completeness == ListingCompleteness::Complete }),
        )
        .await;
        Ok(SourceReconcileResponse {
            job_id: job.id,
            source_id: source_id.to_string(),
            source_kind: SyncedSourceKind::PlaudCloud,
            decisions: summaries,
            listing_complete: Some(completeness == ListingCompleteness::Complete),
            folder: None,
        })
    }

    async fn list_source_artifacts(
        &self,
        recording_id: RecordingId,
    ) -> Result<ListSourceArtifactsResponse, AppError> {
        // An unknown recording is an error, not an empty list.
        self.recordings()
            .get(recording_id)
            .await
            .map_err(AppError::from)?;
        let importer = VendorArtifactImporter::new(self.store().db().clone());
        let rows = importer
            .artifacts_for_recording(recording_id)
            .await
            .map_err(AppError::from)?;
        let mut artifacts = Vec::with_capacity(rows.len());
        for row in rows {
            artifacts.push(SourceArtifactSummary {
                artifact_id: row.id.to_string(),
                kind: row.artifact_kind,
                source_vendor: row.source_vendor,
                content_sha256: row.content_sha256,
                byte_length: row.byte_length,
                original_filename: row.original_filename,
                imported_at: rfc3339_opt(
                    "artifact import time",
                    row.imported_at.map(Timestamp::as_millis),
                )?,
                import_mode: row.import_mode,
                recorded_at: rfc3339("artifact record time", row.created_at.as_millis())?,
            });
        }
        Ok(ListSourceArtifactsResponse { artifacts })
    }
}

/* ------------------------------------------------------------ plaud import */

impl LocalBackend {
    /// Download, verify and commit one listed recording as an import job.
    ///
    /// Returns `Err` only for a failure to even record the job; every failure
    /// of the import itself is an `Failed` outcome carrying the job.
    async fn import_one(
        &self,
        handle: &Handle,
        source_id: SourceId,
        item: &SourceItem,
        intent: &UserInitiated,
    ) -> Result<PlaudImportOutcome, AppError> {
        let external_id = external_id_of(item)
            .ok_or_else(|| internal("a listed item has no external identifier"))?;

        // The same identity already in the library needs no download.
        if let Some(existing) = self
            .sources()
            .find_by_identity(source_id, &item.identity)
            .await
            .map_err(AppError::from)?
            .filter(|association| association.tombstone_reason.is_none())
        {
            return Ok(PlaudImportOutcome::AlreadyImported {
                external_id,
                recording_id: existing.recording_id,
            });
        }

        let job_id = jobs::create(
            self.pool(),
            &jobs::JobDraft {
                kind: JobKind::Import,
                label: item.display_name.clone(),
                recording_id: None,
                config_snapshot: json!({
                    "source": "plaud_cloud_cli",
                    "external_id": external_id,
                }),
            },
        )
        .await?;
        let staging = self.staging_target(job_id)?;

        let result = self
            .run_import(handle, item, job_id, &staging, intent)
            .await;
        // Staging never outlives the attempt, whatever its outcome.
        let _ = std::fs::remove_file(&staging);
        let _ = std::fs::remove_file(part_path(&staging));

        match result {
            Ok(CommitOutcome::Committed {
                recording_id,
                source_updated,
            }) => {
                if source_updated {
                    self.events()
                        .publish(
                            EventDraft::new(EventKind::RecordingSourceUpdated, Timestamp::now())
                                .with_job(job_id)
                                .with_recording(recording_id),
                        )
                        .await;
                }
                Ok(PlaudImportOutcome::Imported {
                    external_id,
                    job_id,
                    recording_id,
                })
            }
            Ok(CommitOutcome::AlreadyImported { recording_id }) => {
                Ok(PlaudImportOutcome::AlreadyImported {
                    external_id,
                    recording_id,
                })
            }
            Err(error) => {
                if let ImportFailure::Source(source) = &error {
                    self.announce_auth(source).await;
                }
                let app_error = error.into_app_error();
                let rendered = match jobs::fail(self.pool(), job_id, &app_error).await {
                    Ok(rendered) => rendered,
                    Err(secondary) => envelope(&secondary),
                };
                publish_job(self, job_id).await;
                Ok(PlaudImportOutcome::Failed {
                    external_id,
                    job_id,
                    error: rendered,
                })
            }
        }
    }

    fn staging_target(&self, job_id: JobId) -> Result<PathBuf, AppError> {
        let dir = self.store().root().join("staging").join("plaud");
        std::fs::create_dir_all(&dir).map_err(|err| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot create the staging directory: {err}"),
            )
        })?;
        Ok(dir.join(format!("{job_id}.incoming")))
    }

    async fn run_import(
        &self,
        handle: &Handle,
        item: &SourceItem,
        job_id: JobId,
        staging: &Path,
        intent: &UserInitiated,
    ) -> Result<CommitOutcome, ImportFailure> {
        jobs::transition(
            self.pool(),
            job_id,
            JobState::Preflight,
            Some("validating"),
            None,
        )
        .await?;
        jobs::transition(
            self.pool(),
            job_id,
            JobState::RunningLocal,
            Some("downloading"),
            None,
        )
        .await?;
        publish_job(self, job_id).await;

        let report = handle
            .connector
            .acquire_item(item, staging, intent)
            .await
            .map_err(ImportFailure::Source)?;

        jobs::transition(
            self.pool(),
            job_id,
            JobState::Verifying,
            Some("verifying"),
            None,
        )
        .await?;
        let extension = report
            .container
            .canonical_extensions()
            .first()
            .copied()
            .unwrap_or("bin")
            .to_owned();
        let request = ImportRequest {
            identity: item.identity.clone(),
            display_name: item.display_name.clone(),
            quality_class: report.asset.quality_class,
            extension,
            observed_codec: report.container.as_str().to_owned(),
            remote_version: item.remote_version.clone(),
            attach_to_recording: None,
        };

        jobs::transition(
            self.pool(),
            job_id,
            JobState::Committing,
            Some("committing"),
            None,
        )
        .await?;
        let now = Timestamp::now();
        // `ImportService::import` streams from a `&mut dyn Read`, which is not
        // `Send`, so the use case runs on a blocking worker.
        let service = self.imports().clone();
        let source = staging.to_path_buf();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut file = std::fs::File::open(&source).map_err(|err| {
                AppError::new(
                    AppErrorCode::ArchivePathRejected,
                    format!("cannot open the staged file: {err}"),
                )
            })?;
            tokio::runtime::Handle::current()
                .block_on(service.import(&request, &mut file, now))
                .map_err(AppError::from)
        })
        .await
        .map_err(|err| internal(format!("the import worker failed: {err}")))??;

        let (recording_id, source_updated) = match outcome {
            CoreOutcome::Imported { recording_id, .. } => (recording_id, false),
            CoreOutcome::MediaCandidateAdded { recording_id, .. } => (recording_id, true),
            CoreOutcome::AlreadyImported { recording_id, .. } => {
                jobs::transition(
                    self.pool(),
                    job_id,
                    JobState::Cancelled,
                    Some("already_imported"),
                    None,
                )
                .await?;
                publish_job(self, job_id).await;
                return Ok(CommitOutcome::AlreadyImported { recording_id });
            }
            CoreOutcome::RefusedTombstoned { .. } => {
                return Err(ImportFailure::App(AppError::new(
                    AppErrorCode::Conflict,
                    "the recording was deleted locally and will not be resurrected",
                )))
            }
        };

        self.attach_recording(job_id, recording_id).await?;
        let activity_id = self
            .commit_import(job_id, recording_id, &item.display_name, now)
            .await?;
        self.publish_import_events(job_id, recording_id, &item.display_name)
            .await;
        self.notifications()
            .notify(
                activity_id,
                NotificationContext::ImportFinished {
                    imported: 1,
                    failed: 0,
                    recording: Some(recording_id),
                },
                now,
            )
            .await;
        Ok(CommitOutcome::Committed {
            recording_id,
            source_updated,
        })
    }

    async fn commit_import(
        &self,
        job_id: JobId,
        recording_id: RecordingId,
        title: &str,
        now: Timestamp,
    ) -> Result<String, AppError> {
        let mut tx = plaudon_persistence::begin_write(self.pool())
            .await
            .map_err(|err| storage_error("cannot begin an import commit", &err))?;
        jobs::complete_in_tx(
            &mut tx,
            job_id,
            Some(OutputLink {
                kind: OutputLinkKind::Recording,
                id: recording_id.to_string(),
            }),
        )
        .await?;
        // Spec §22(13): the activity entry is committed with the job.
        let activity_id = activity::record_in_tx(
            &mut tx,
            &activity::ActivityDraft {
                category: NotificationCategory::ImportFinished,
                occurred_at: now,
                title: "Import finished".to_owned(),
                body: format!("{title} was added to your library."),
                target: Some(ActivityTarget {
                    kind: ActivityTargetKind::Recording,
                    id: recording_id.to_string(),
                }),
            },
        )
        .await?;
        tx.commit()
            .await
            .map_err(|err| storage_error("cannot commit an import", &err))?;
        Ok(activity_id)
    }

    async fn attach_recording(
        &self,
        job_id: JobId,
        recording_id: RecordingId,
    ) -> Result<(), AppError> {
        let mut config = jobs::config_snapshot(self.pool(), job_id).await?;
        if let Some(object) = config.as_object_mut() {
            object.insert("recording_id".to_owned(), json!(recording_id.to_string()));
        }
        sqlx::query("UPDATE jobs SET config_snapshot_json = ?2 WHERE id = ?1")
            .bind(job_id.to_string())
            .bind(config.to_string())
            .execute(self.pool())
            .await
            .map_err(|err| storage_error("cannot link a job to its recording", &err))?;
        Ok(())
    }

    async fn publish_import_events(&self, job_id: JobId, recording_id: RecordingId, name: &str) {
        let now = Timestamp::now();
        publish_job(self, job_id).await;
        self.events()
            .publish(
                EventDraft::new(EventKind::RecordingImported, now)
                    .with_job(job_id)
                    .with_recording(recording_id)
                    .with_hint("original_file_name", json!(name)),
            )
            .await;
        self.events()
            .publish(EventDraft::new(EventKind::ActivityAdded, now).with_recording(recording_id))
            .await;
        self.events()
            .publish(EventDraft::new(EventKind::LibraryChanged, now))
            .await;
    }
}

enum CommitOutcome {
    Committed {
        recording_id: RecordingId,
        source_updated: bool,
    },
    AlreadyImported {
        recording_id: RecordingId,
    },
}

/// Why one import failed. Kept apart so a source failure can announce a
/// required sign-in before it is flattened into an [`AppError`].
enum ImportFailure {
    Source(SourceError),
    App(AppError),
}

impl ImportFailure {
    fn into_app_error(self) -> AppError {
        match self {
            Self::Source(error) => AppError::from(error),
            Self::App(error) => error,
        }
    }
}

impl From<AppError> for ImportFailure {
    fn from(error: AppError) -> Self {
        Self::App(error)
    }
}

/* ---------------------------------------------------------------- folders */

impl LocalBackend {
    /// The service for a watched folder, rebuilt from its stored row after a
    /// restart. `None` means the ID is not a watched folder.
    async fn folder_service(
        &self,
        id: Uuid,
    ) -> Result<Option<Arc<WatchedFolderService>>, AppError> {
        if let Some(service) = self.source_runtime.folders().get(&id) {
            return Ok(Some(Arc::clone(service)));
        }
        let row = sqlx::query(
            "SELECT root_path, recursive, rule, quiescence_ms FROM watched_folders WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(|err| storage_error("cannot read a watched folder", &err))?;
        let Some(row) = row else { return Ok(None) };

        let root: String = row.get("root_path");
        let recursive: i64 = row.get("recursive");
        let rule: String = row.get("rule");
        let quiescence: i64 = row.get("quiescence_ms");
        let config = WatchedFolderConfig::new(Path::new(&root), FolderRule::parse(&rule))
            .map_err(folder_error)?
            .recursive(recursive != 0)
            .with_quiescence_ms(u64::try_from(quiescence).unwrap_or(0));
        let service = Arc::new(
            WatchedFolderService::new(config, self.store().db().clone(), self.archive().clone())
                .await
                .map_err(folder_error)?,
        );
        self.source_runtime
            .folders()
            .insert(id, Arc::clone(&service));
        Ok(Some(service))
    }

    /// Resolve `id` to the connector's source row, or `RecordNotFound`.
    async fn require_plaud_source(&self, handle: &Handle, id: Uuid) -> Result<SourceId, AppError> {
        let known = self
            .sources()
            .find_source_id(SourceKind::PlaudCloudCli, &handle.scope)
            .await
            .map_err(AppError::from)?;
        match known {
            Some(source) if source.to_string() == id.to_string() => Ok(source),
            _ => Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "no such source",
            )),
        }
    }

    async fn publish_folder_report(&self, job: &PassJob, report: &SyncReport) {
        let now = Timestamp::now();
        for recording_id in &report.imported {
            self.events()
                .publish(
                    EventDraft::new(EventKind::RecordingImported, now)
                        .with_job(job.id)
                        .with_recording(*recording_id),
                )
                .await;
        }
        if !report.imported.is_empty() {
            self.events()
                .publish(EventDraft::new(EventKind::LibraryChanged, now))
                .await;
        }
        self.announce_sync_finished(
            "watched_folder",
            "sync",
            Some(job.id),
            Ok(()),
            json!({
                "scanned": report.scanned,
                "imported": report.imported.len(),
                "already_imported": report.already_imported,
                "deferred": report.deferred,
                "vanished": report.vanished,
            }),
        )
        .await;
    }

    /// Publish `recording.source_updated` for every decision that changed a
    /// recording's source-side state.
    async fn publish_source_updates(&self, source_id: SourceId, decisions: &[ReconcileDecision]) {
        for decision in decisions {
            let changed = decision.actions.iter().any(|action| {
                matches!(
                    action,
                    ReconcileAction::ReconcileMetadata
                        | ReconcileAction::AppendMediaRevision { .. }
                        | ReconcileAction::MarkRemoteState { .. }
                )
            });
            if !changed {
                continue;
            }
            let Ok(Some(association)) = self
                .sources()
                .find_by_identity(source_id, &decision.identity)
                .await
            else {
                continue;
            };
            self.events()
                .publish(
                    EventDraft::new(EventKind::RecordingSourceUpdated, Timestamp::now())
                        .with_recording(association.recording_id),
                )
                .await;
        }
    }
}

/// Run one folder sync on a blocking worker.
///
/// The folder service streams file bytes through a `&mut dyn Read` that is
/// held across an await, which makes its futures non-`Send`; a blocking
/// worker drives it to completion without occupying an async worker thread.
async fn run_folder_sync(service: &Arc<WatchedFolderService>) -> Result<SyncReport, FolderError> {
    let service = Arc::clone(service);
    tokio::task::spawn_blocking(move || tokio::runtime::Handle::current().block_on(service.sync()))
        .await
        .map_err(|err| FolderError::Watcher(format!("the folder sync worker failed: {err}")))?
}

const fn folder_rule(rule: WatchedFolderRule) -> FolderRule {
    match rule {
        WatchedFolderRule::ImportOnly => FolderRule::ImportOnly,
        WatchedFolderRule::ImportAndQueueLocalStt => FolderRule::ImportAndQueueLocalStt,
        WatchedFolderRule::AskBeforeCloud => FolderRule::AskBeforeCloud,
    }
}

/// Map a folder failure to a stable code. The detail stays in the redacted
/// diagnostic log; the envelope carries only the code's canonical message.
fn folder_error(error: FolderError) -> AppError {
    match error {
        FolderError::Persistence(inner) => AppError::from(inner),
        FolderError::PathRejected(_) | FolderError::UnusableRoot(_) => {
            AppError::new(AppErrorCode::ArchivePathRejected, error.to_string())
        }
        FolderError::Io { .. } | FolderError::Watcher(_) => {
            AppError::new(AppErrorCode::StorageUnavailable, error.to_string())
        }
    }
}

fn folder_counts(report: &SyncReport) -> FolderSyncCounts {
    let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    FolderSyncCounts {
        scanned: count(report.scanned),
        inventory_complete: report.inventory_complete,
        imported: count(report.imported.len()),
        already_imported: count(report.already_imported),
        deferred: count(report.deferred),
        refused_tombstoned: count(report.refused_tombstoned),
        rejected: count(report.rejected),
        vanished: count(report.vanished),
    }
}

/* -------------------------------------------------------------- decisions */

const fn event_name(event: &SourceEvent) -> &'static str {
    match event {
        SourceEvent::NewExternalId => "new_external_id",
        SourceEvent::UnchangedVersion => "unchanged_version",
        SourceEvent::UnchangedVersionIncompleteAcquisition => {
            "unchanged_version_incomplete_acquisition"
        }
        SourceEvent::ChangedVersionMetadataOnly => "changed_version_metadata_only",
        SourceEvent::ChangedVersionContentChanged => "changed_version_content_changed",
        SourceEvent::SameBytesFromDifferentSource => "same_bytes_from_different_source",
        SourceEvent::RemotelyTrashedOrRemoved => "remotely_trashed_or_removed",
        SourceEvent::LocallyDeleted => "locally_deleted",
        SourceEvent::FolderFileStillChanging => "folder_file_still_changing",
        SourceEvent::FolderCloudPlaceholder => "folder_cloud_placeholder",
        SourceEvent::VendorExportTranscriptAppeared => "vendor_export_transcript_appeared",
        SourceEvent::UserRequestedRetranscription => "user_requested_retranscription",
    }
}

const fn action_name(action: &ReconcileAction) -> &'static str {
    match action {
        ReconcileAction::QueueAcquisition { .. } => "queue_acquisition",
        ReconcileAction::SkipAcquisition => "skip_acquisition",
        ReconcileAction::ReconcileMetadata => "reconcile_metadata",
        ReconcileAction::AppendMediaRevision { .. } => "append_media_revision",
        ReconcileAction::ReuseExistingMedia { .. } => "reuse_existing_media",
        ReconcileAction::KeepSourceAssociation { .. } => "keep_source_association",
        ReconcileAction::MarkRemoteState { .. } => "mark_remote_state",
        ReconcileAction::PersistTombstone { .. } => "persist_tombstone",
        ReconcileAction::RefuseResurrection { .. } => "refuse_resurrection",
        ReconcileAction::DeferAndRescan { .. } => "defer_and_rescan",
        ReconcileAction::AwaitPlaceholderHydration { .. } => "await_placeholder_hydration",
        ReconcileAction::ImportVendorArtifact { .. } => "import_vendor_artifact",
        ReconcileAction::CreateTranscriptRevision { .. } => "create_transcript_revision",
    }
}

fn decision_summary(decision: &ReconcileDecision) -> ReconcileDecisionSummary {
    ReconcileDecisionSummary {
        external_id: decision
            .identity
            .external_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        event: event_name(&decision.event).to_owned(),
        actions: decision
            .actions
            .iter()
            .map(|action| action_name(action).to_owned())
            .collect(),
    }
}
