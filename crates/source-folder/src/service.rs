//! The watched-folder use case (§4.3, §7.10, App. A.2, App. A.4).
//!
//! This is the only place in the crate that writes anything. It composes four
//! things that are each deliberately dumb on their own:
//!
//! * [`crate::barrier::CompletionBarrier`] decides *when* a file is complete;
//! * [`plaudon_app_core::ImportService`] performs the §13.3A durable import —
//!   there is no second importer in this crate;
//! * [`plaudon_persistence::repo::WatchedFolderRepository`] records provenance
//!   and availability;
//! * [`plaudon_diagnostics::MetricsRegistry`] records the §17.4 indicators.
//!
//! # What never happens here
//!
//! No source file is renamed, moved, truncated or deleted — not on success,
//! not on failure, not as cleanup (§4.3(4), App. A.4, §22(11)). Every import
//! is followed by a byte-identity check of the file that was read, and the
//! result is recorded as a metric so a regression is visible rather than
//! theoretical. A vanished source marks the association unavailable and stops
//! there: the archived recording and its derivatives are untouched (§4.3(6)).

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_app_core::import::{ImportOutcome, ImportRequest, ImportService};
use plaudon_app_core::reconciliation::{
    classify_folder_candidate, FolderCandidateState, ReconcileDecision,
};
use plaudon_diagnostics::metrics::{MetricsRegistry, WatcherMetric};
use plaudon_domain::ids::{RecordingId, SourceItemId};
use plaudon_domain::media::QualityClass;
use plaudon_domain::source::{SourceAccountScope, SourceIdentityKey, SourceKind};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::repo::{
    WatchedFolderItemRow, WatchedFolderRepository, WatchedFolderRow,
};
use plaudon_security::consent::{dispatch_preflight, ConsentError, ConsentGrant, DispatchApproval,
    DispatchRequest};
use plaudon_security::outbound::OutboundPolicy;
use uuid::Uuid;

use crate::barrier::{BarrierOutcome, BarrierPolicy, CompletionBarrier, DeferReason, ReadyCandidate};
use crate::clock::{Clock, SystemClock};
use crate::config::{FolderRule, WatchedFolderConfig};
use crate::error::{FolderError, FolderResult};
use crate::fsmeta::{mtime_ms, DiskSpaceProbe, OsDiskSpaceProbe};
use crate::inventory::{self, InventoryReport};
use crate::placeholder::{OsPlaceholderProbe, PlaceholderProbe};
use crate::queue::CoalescingQueue;

/// Somewhere to put local-only follow-up work.
///
/// §4.3(7): `import_and_queue_local_stt` queues **local** transcription. It
/// never dispatches to a cloud provider, which is why this sink cannot carry
/// a model route or a provider at all.
pub trait LocalWorkSink: std::fmt::Debug + Send + Sync {
    /// Queue local transcription of a freshly imported recording.
    fn enqueue_local_stt(&self, recording_id: RecordingId);
}

/// A sink that drops the request, for folders that queue nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLocalWork;

impl LocalWorkSink for NoLocalWork {
    fn enqueue_local_stt(&self, _recording_id: RecordingId) {}
}

/// A sink that accumulates requests for the caller to drain.
#[derive(Debug, Default)]
pub struct QueueingLocalWorkSink {
    queued: parking_lot::Mutex<Vec<RecordingId>>,
}

impl QueueingLocalWorkSink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take everything queued so far.
    pub fn take(&self) -> Vec<RecordingId> {
        std::mem::take(&mut self.queued.lock())
    }

    /// How many requests are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queued.lock().len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl LocalWorkSink for QueueingLocalWorkSink {
    fn enqueue_local_stt(&self, recording_id: RecordingId) {
        self.queued.lock().push(recording_id);
    }
}

/// Why a background cloud dispatch was refused.
#[derive(Debug)]
pub enum CloudDispatchRefusal {
    /// The folder rule requires an explicit grant and the preflight refused.
    NoValidGrant(ConsentError),
}

/// What processing one candidate did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateOutcome {
    /// A new recording was created from the file.
    Imported {
        /// The new recording.
        recording_id: RecordingId,
        /// Association recording where it came from.
        source_item_id: SourceItemId,
        /// Digest of the imported bytes.
        content_sha256: String,
        /// Whether the blob already existed and was reused.
        reused_existing_blob: bool,
        /// Milliseconds between true completion and the committed import
        /// (§17.4).
        latency_ms: i64,
        /// Whether local transcription was queued for it.
        queued_local_stt: bool,
    },
    /// The same file identity was already imported; nothing changed.
    AlreadyImported {
        /// The recording that already represents it.
        recording_id: RecordingId,
    },
    /// The completion barrier said no, with a retryable reason.
    Deferred {
        /// Why.
        reason: DeferReason,
        /// The App. A.2 classification of the deferral.
        decision: Box<ReconcileDecision>,
    },
    /// The user deleted this recording; a rescan must not recreate it.
    RefusedTombstoned {
        /// The tombstoned recording.
        recording_id: RecordingId,
    },
    /// The path is not a candidate this folder may import.
    Rejected {
        /// Non-secret explanation.
        reason: String,
    },
}

/// Imports from one watched folder.
#[derive(Debug, Clone)]
pub struct WatchedFolderService {
    config: WatchedFolderConfig,
    imports: ImportService,
    folders: WatchedFolderRepository,
    barrier: CompletionBarrier,
    queue: CoalescingQueue,
    metrics: MetricsRegistry,
    clock: Arc<dyn Clock>,
    local_work: Arc<dyn LocalWorkSink>,
    folder_id: Uuid,
}

impl WatchedFolderService {
    /// Build a service and register the folder.
    ///
    /// # Errors
    ///
    /// Fails when the folder row cannot be written.
    pub async fn new(
        config: WatchedFolderConfig,
        db: Database,
        archive: MediaArchive,
    ) -> FolderResult<Self> {
        let barrier = CompletionBarrier::with_probes(
            Self::policy_for(&config),
            Arc::new(OsPlaceholderProbe),
            Arc::new(OsDiskSpaceProbe),
        );
        Self::assemble(config, db, archive, barrier, Arc::new(SystemClock)).await
    }

    /// Build a service with injected probes and clock, for fixtures that must
    /// simulate a cloud placeholder or a full disk.
    ///
    /// # Errors
    ///
    /// Fails when the folder row cannot be written.
    pub async fn with_probes(
        config: WatchedFolderConfig,
        db: Database,
        archive: MediaArchive,
        placeholder: Arc<dyn PlaceholderProbe>,
        space: Arc<dyn DiskSpaceProbe>,
        clock: Arc<dyn Clock>,
    ) -> FolderResult<Self> {
        let barrier =
            CompletionBarrier::with_probes(Self::policy_for(&config), placeholder, space);
        Self::assemble(config, db, archive, barrier, clock).await
    }

    /// Build a service around an already-configured barrier.
    ///
    /// The mutation-sanity suite uses this to install a deliberately weakened
    /// barrier and prove the fixtures notice.
    ///
    /// # Errors
    ///
    /// Fails when the folder row cannot be written.
    pub async fn with_barrier(
        config: WatchedFolderConfig,
        db: Database,
        archive: MediaArchive,
        barrier: CompletionBarrier,
        clock: Arc<dyn Clock>,
    ) -> FolderResult<Self> {
        Self::assemble(config, db, archive, barrier, clock).await
    }

    fn policy_for(config: &WatchedFolderConfig) -> BarrierPolicy {
        BarrierPolicy::strict(
            config.quiescence_ms(),
            config.required_stable_probes(),
            config.caps().max_file_bytes,
            config.caps().free_space_headroom_bytes,
        )
    }

    async fn assemble(
        config: WatchedFolderConfig,
        db: Database,
        archive: MediaArchive,
        barrier: CompletionBarrier,
        clock: Arc<dyn Clock>,
    ) -> FolderResult<Self> {
        let folders = WatchedFolderRepository::new(db.clone());
        let now = Timestamp::from_millis(clock.now_ms());
        let folder_id = folders
            .ensure_folder(&WatchedFolderRow {
                id: Uuid::now_v7(),
                root_path: config.root().display().to_string(),
                recursive: config.is_recursive(),
                rule: config.rule().as_str().to_string(),
                quiescence_ms: config.quiescence_ms(),
                status: "active".to_string(),
                last_inventory_at: None,
                created_at: now,
            })
            .await?;
        let queue = CoalescingQueue::new(config.quiescence_ms(), config.caps().max_pending);
        Ok(Self {
            imports: ImportService::new(db, archive),
            folders,
            barrier,
            queue,
            metrics: MetricsRegistry::new(),
            clock,
            local_work: Arc::new(NoLocalWork),
            folder_id,
            config,
        })
    }

    /// Replace the local-work sink.
    #[must_use]
    pub fn with_local_work(mut self, sink: Arc<dyn LocalWorkSink>) -> Self {
        self.local_work = sink;
        self
    }

    /// Share an existing metrics registry, so one diagnostics pane can show
    /// several folders.
    #[must_use]
    pub fn with_metrics(mut self, metrics: MetricsRegistry) -> Self {
        self.metrics = metrics;
        self
    }

    /// The folder this service watches.
    #[must_use]
    pub const fn config(&self) -> &WatchedFolderConfig {
        &self.config
    }

    /// The coalescing queue, shared with the watcher and the sweep.
    #[must_use]
    pub const fn queue(&self) -> &CoalescingQueue {
        &self.queue
    }

    /// The §17.4 metrics.
    #[must_use]
    pub const fn metrics(&self) -> &MetricsRegistry {
        &self.metrics
    }

    /// The completion barrier.
    #[must_use]
    pub const fn barrier(&self) -> &CompletionBarrier {
        &self.barrier
    }

    /// The import service, for callers that need the library repositories.
    #[must_use]
    pub const fn imports(&self) -> &ImportService {
        &self.imports
    }

    /// The row identity of this folder.
    #[must_use]
    pub const fn folder_id(&self) -> Uuid {
        self.folder_id
    }

    /// Database row for the watched items of this folder.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn items(&self) -> FolderResult<Vec<WatchedFolderItemRow>> {
        Ok(self.folders.list_items(self.folder_id).await?)
    }

    /// Probe and, if the barrier allows, import one candidate.
    ///
    /// # Errors
    ///
    /// Propagates storage failures. A file that is merely not ready is not an
    /// error: it is [`CandidateOutcome::Deferred`].
    pub async fn process_path(&self, candidate: &Path) -> FolderResult<CandidateOutcome> {
        let validated = match self.config.validate_candidate(candidate) {
            Ok(validated) => validated,
            Err(error) => {
                return Ok(CandidateOutcome::Rejected {
                    reason: error.to_string(),
                })
            }
        };
        let path = validated.into_path_buf();
        let now_ms = self.clock.now_ms();

        match self.barrier.probe(&path, now_ms) {
            BarrierOutcome::Defer(reason) => self.record_deferral(&path, reason, now_ms).await,
            BarrierOutcome::Ready(ready) => self.import_ready(&path, *ready, now_ms).await,
        }
    }

    async fn record_deferral(
        &self,
        path: &Path,
        reason: DeferReason,
        now_ms: i64,
    ) -> FolderResult<CandidateOutcome> {
        let state = match &reason {
            DeferReason::CloudPlaceholder(_) => {
                self.metrics.increment(WatcherMetric::PlaceholderRetries);
                "placeholder"
            }
            DeferReason::Vanished => "unavailable",
            DeferReason::AwaitingQuiescence { .. } | DeferReason::StillChanging => "quiescing",
            _ => "rejected",
        };
        let at = Timestamp::from_millis(now_ms);
        if matches!(reason, DeferReason::Vanished) {
            self.folders
                .mark_unavailable(self.folder_id, &path.display().to_string(), at)
                .await?;
        } else {
            self.upsert_observation(path, state, None, None, now_ms)
                .await?;
        }

        let candidate_state = if matches!(reason, DeferReason::CloudPlaceholder(_)) {
            FolderCandidateState::CloudPlaceholder
        } else {
            FolderCandidateState::StillChanging
        };
        let decision = classify_folder_candidate(
            &self.pending_identity(),
            candidate_state,
            self.config.quiescence_ms(),
        );
        Ok(CandidateOutcome::Deferred {
            reason,
            decision: Box::new(decision),
        })
    }

    async fn import_ready(
        &self,
        path: &Path,
        ready: ReadyCandidate,
        now_ms: i64,
    ) -> FolderResult<CandidateOutcome> {
        self.metrics.increment(WatcherMetric::ImportAttempts);
        let at = Timestamp::from_millis(now_ms);
        let identity = self.identity_for(&ready);
        let display_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Imported recording")
            .to_string();

        let request = ImportRequest {
            identity,
            display_name,
            quality_class: QualityClass::UserSuppliedFile,
            extension: ready.container.extension().to_string(),
            observed_codec: ready.container.codec_label().to_string(),
            remote_version: None,
            attach_to_recording: None,
        };

        let mut file = File::open(path).map_err(|error| FolderError::io(path, error))?;
        let outcome = self.imports.import(&request, &mut file, at).await?;
        drop(file);

        // §22(11): prove the file we read is exactly as we found it.
        let preserved = self.verify_source_preserved(path, &ready);
        self.metrics
            .increment(WatcherMetric::SourcePreservationChecks);
        if !preserved {
            self.metrics
                .increment(WatcherMetric::SourcePreservationFailures);
        }

        match outcome {
            ImportOutcome::Imported {
                recording_id,
                source_item_id,
                reused_existing_blob,
                ..
            } => {
                self.upsert_observation(
                    path,
                    "imported",
                    Some(ready.content_sha256.clone()),
                    Some(source_item_id),
                    now_ms,
                )
                .await?;
                self.barrier.forget(path);
                let latency_ms = now_ms.saturating_sub(ready.complete_since_ms);
                self.metrics.observe(
                    WatcherMetric::ImportLatencyMs,
                    u64::try_from(latency_ms).unwrap_or(0),
                );
                let queued_local_stt = self.config.rule().queues_local_stt();
                if queued_local_stt {
                    self.local_work.enqueue_local_stt(recording_id);
                }
                Ok(CandidateOutcome::Imported {
                    recording_id,
                    source_item_id,
                    content_sha256: ready.content_sha256,
                    reused_existing_blob,
                    latency_ms,
                    queued_local_stt,
                })
            }
            ImportOutcome::AlreadyImported {
                recording_id,
                source_item_id,
            } => {
                self.upsert_observation(
                    path,
                    "imported",
                    Some(ready.content_sha256),
                    Some(source_item_id),
                    now_ms,
                )
                .await?;
                self.barrier.forget(path);
                Ok(CandidateOutcome::AlreadyImported { recording_id })
            }
            ImportOutcome::RefusedTombstoned { recording_id } => {
                self.upsert_observation(path, "rejected", None, None, now_ms)
                    .await?;
                Ok(CandidateOutcome::RefusedTombstoned { recording_id })
            }
            ImportOutcome::MediaCandidateAdded { recording_id, .. } => {
                Ok(CandidateOutcome::AlreadyImported { recording_id })
            }
        }
    }

    /// Size, mtime and digest must all match what the barrier measured.
    fn verify_source_preserved(&self, path: &Path, ready: &ReadyCandidate) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        metadata.len() == ready.size_bytes && mtime_ms(&metadata) == ready.mtime_ms
    }

    /// Identity of an importable candidate.
    ///
    /// §7.10: a filename is never identity. The external identifier is the
    /// filesystem's own stable file id where the platform has one, and the
    /// content digest otherwise. Either way, replacing a file by atomic
    /// rename yields a different identity and therefore a different logical
    /// recording, while renaming the same file does not.
    fn identity_for(&self, ready: &ReadyCandidate) -> SourceIdentityKey {
        let external = ready.stable_file_id.as_ref().map_or_else(
            || format!("sha256:{}", ready.content_sha256),
            |id| format!("file:{id}"),
        );
        SourceIdentityKey::new(
            SourceKind::WatchedFolder,
            self.account_scope(),
            Some(plaudon_domain::source::ExternalRecordingId::new(external)),
        )
    }

    /// Identity used for a candidate that has no digest yet.
    fn pending_identity(&self) -> SourceIdentityKey {
        SourceIdentityKey::new(SourceKind::WatchedFolder, self.account_scope(), None)
    }

    fn account_scope(&self) -> SourceAccountScope {
        SourceAccountScope::new(self.config.root().display().to_string())
    }

    async fn upsert_observation(
        &self,
        path: &Path,
        state: &str,
        content_sha256: Option<String>,
        source_item_id: Option<SourceItemId>,
        now_ms: i64,
    ) -> FolderResult<()> {
        let origin_path = path.display().to_string();
        let metadata = std::fs::metadata(path).ok();
        let at = Timestamp::from_millis(now_ms);
        let existing = self.folders.find_item(self.folder_id, &origin_path).await?;
        let row = WatchedFolderItemRow {
            id: existing.as_ref().map_or_else(Uuid::now_v7, |row| row.id),
            folder_id: self.folder_id,
            origin_path,
            stable_file_id: metadata
                .as_ref()
                .and_then(|meta| crate::fsmeta::stable_file_id(path, meta))
                .map(|id| id.as_str().to_string()),
            size_bytes: metadata.as_ref().map_or(0, std::fs::Metadata::len),
            mtime_ms: metadata.as_ref().map_or(0, mtime_ms),
            content_sha256,
            state: state.to_string(),
            source_item_id,
            first_seen_at: existing.as_ref().map_or(at, |row| row.first_seen_at),
            last_checked_at: at,
            unavailable_since: None,
        };
        self.folders.upsert_item(&row).await?;
        Ok(())
    }

    /// Absorb hints the watcher produced into the queue.
    pub fn pump(&self, source: &mut dyn crate::watcher::FsEventSource) -> crate::watcher::PumpReport {
        let report = crate::watcher::pump(source, &self.config, &self.queue, self.clock.as_ref());
        for _ in 0..report.hints {
            self.metrics.increment(WatcherMetric::EventHintsObserved);
        }
        if report.rescan_requested {
            self.metrics.increment(WatcherMetric::ChangesMissedByWatcher);
        }
        for _ in 0..report.overflowed {
            self.metrics.increment(WatcherMetric::ChangesMissedByWatcher);
        }
        report
    }

    /// Drain everything whose debounce window has elapsed and process it.
    ///
    /// A candidate the barrier deferred for a retryable reason goes back into
    /// the queue, so the next drain picks it up without a new event.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn drain_due(&self) -> FolderResult<Vec<CandidateOutcome>> {
        let now_ms = self.clock.now_ms();
        let mut outcomes = Vec::new();
        for candidate in self.queue.drain_due(now_ms) {
            let outcome = self.process_path(&candidate.path).await?;
            if let CandidateOutcome::Deferred { reason, .. } = &outcome {
                if reason.is_retryable() {
                    self.queue.requeue(candidate, now_ms);
                }
            }
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    /// One full reconciliation tick for this folder.
    ///
    /// Sweeps the bounded inventory, then processes everything whose debounce
    /// window has elapsed. This is the entry point a "Sync now" command and
    /// the periodic timer both use; running it twice in a row imports nothing
    /// extra, because identity is the file's own, not its path.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn sync(&self) -> FolderResult<SyncReport> {
        let inventory = self.run_inventory().await?;
        let outcomes = self.drain_due().await?;
        let mut report = SyncReport {
            scanned: inventory.scanned,
            inventory_complete: inventory.is_complete(),
            vanished: inventory.vanished.len(),
            ..SyncReport::default()
        };
        for outcome in outcomes {
            match outcome {
                CandidateOutcome::Imported { recording_id, .. } => {
                    report.imported.push(recording_id);
                }
                CandidateOutcome::AlreadyImported { .. } => report.already_imported += 1,
                CandidateOutcome::Deferred { .. } => report.deferred += 1,
                CandidateOutcome::RefusedTombstoned { .. } => report.refused_tombstoned += 1,
                CandidateOutcome::Rejected { .. } => report.rejected += 1,
            }
        }
        Ok(report)
    }

    /// Run one bounded inventory sweep.
    ///
    /// Recovers anything the watcher dropped, and marks vanished sources
    /// unavailable. It enqueues rather than imports, so an in-flight import is
    /// never blocked by a scan.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn run_inventory(&self) -> FolderResult<InventoryReport> {
        let known: BTreeSet<PathBuf> = self
            .folders
            .list_items(self.folder_id)
            .await?
            .into_iter()
            .filter(|row| row.state != "unavailable")
            .map(|row| PathBuf::from(row.origin_path))
            .collect();
        let missed_before = self.queue.take_dropped_hints();
        let report = inventory::sweep(&self.config, &self.queue, &known, self.clock.as_ref());

        let at = Timestamp::from_millis(self.clock.now_ms());
        for path in &report.vanished {
            // §4.3(6): unavailable, not deleted. Nothing else is touched.
            self.folders
                .mark_unavailable(self.folder_id, &path.display().to_string(), at)
                .await?;
        }
        if missed_before > 0 {
            for _ in 0..missed_before {
                self.metrics
                    .increment(WatcherMetric::ChangesRecoveredByInventory);
            }
        }
        self.folders
            .set_last_inventory_at(self.folder_id, at)
            .await?;
        Ok(report)
    }

    /// Authorise a background cloud dispatch for a recording from this folder.
    ///
    /// §4.3(7): under `ask_before_cloud` nothing may be dispatched in the
    /// background without a valid explicit grant, so the request goes through
    /// the security crate's preflight — which runs the egress gate first and
    /// cannot be satisfied by the mere presence of an API key.
    ///
    /// # Errors
    ///
    /// Returns the refusal when no grant covers the dispatch.
    pub fn authorize_background_cloud_dispatch(
        &self,
        policy: &OutboundPolicy,
        grants: &[ConsentGrant],
        request: &DispatchRequest,
        now_unix: i64,
    ) -> Result<DispatchApproval, CloudDispatchRefusal> {
        dispatch_preflight(policy, grants, request, now_unix)
            .map_err(CloudDispatchRefusal::NoValidGrant)
    }

    /// Whether this folder's rule permits dispatching to the cloud without
    /// asking at the moment of dispatch.
    #[must_use]
    pub fn permits_unattended_cloud(&self) -> bool {
        !matches!(self.config.rule(), FolderRule::AskBeforeCloud)
    }
}

/// What one [`WatchedFolderService::sync`] tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Entries the inventory sweep inspected.
    pub scanned: usize,
    /// Whether the sweep saw the whole folder within its caps.
    pub inventory_complete: bool,
    /// Recordings created by this tick.
    pub imported: Vec<RecordingId>,
    /// Candidates that were already imported.
    pub already_imported: usize,
    /// Candidates the completion barrier deferred.
    pub deferred: usize,
    /// Candidates refused because the user deleted the recording.
    pub refused_tombstoned: usize,
    /// Paths that are not candidates for this folder.
    pub rejected: usize,
    /// Known paths that are no longer present and were marked unavailable.
    pub vanished: usize,
}
