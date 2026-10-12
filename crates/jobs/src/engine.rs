//! The job engine facade.
//!
//! Wires together the transactional store, the bounded resource scheduler, the
//! cancellation registry, the consent gate, the durable event log and its
//! post-commit dispatcher. This is the surface the Tauri command layer and the
//! optional local CLI call (SPEC v1.3 §14, §15.2).

use std::path::PathBuf;
use std::sync::Arc;

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::JobId;
use tokio_util::sync::CancellationToken;

use crate::cancel::{
    discard_incomplete_derivatives, CancellationAck, CancellationOutcome, CancellationRegistry,
    ConsentGate, InterruptionCause,
};
use crate::error::{DispatchRefusal, JobError};
use crate::events::{EventDispatcher, EventSubscriber};
use crate::history::JobHistory;
use crate::reconcile::{ReconcileReport, Reconciler, ReconcilerConfig};
use crate::scheduler::{
    preflight, Admission, PreflightDecision, ResourceClass, ResourceRequest, ResourceScheduler,
    SchedulerLimits, SystemResources,
};
use crate::state::{JobState, TransitionCause};
use crate::store::{DispatchState, EnqueueOutcome, EnqueueRequest, JobOutput, JobRecord, JobStore};
use crate::writer::Database;

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Per-class concurrency limits (§14.7).
    pub limits: SchedulerLimits,
    /// Startup recovery configuration.
    pub reconciler: ReconcilerConfig,
    /// Root of the managed media archive (§13.4). When set, startup recovery
    /// also reconciles the import journal and verifies that every `Completed`
    /// job resolves to existing, matching bytes.
    pub archive_root: Option<PathBuf>,
}

impl EngineConfig {
    /// Default limits with a staging root.
    pub fn new(staging_root: impl Into<PathBuf>) -> Self {
        Self {
            limits: SchedulerLimits::default(),
            reconciler: ReconcilerConfig::new(staging_root),
            archive_root: None,
        }
    }

    /// Attach the managed media archive root.
    #[must_use]
    pub fn with_archive_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.archive_root = Some(root.into());
        self
    }
}

/// The job engine.
#[derive(Clone)]
pub struct JobEngine {
    store: JobStore,
    history: JobHistory,
    scheduler: ResourceScheduler,
    cancellation: CancellationRegistry,
    consent: ConsentGate,
    dispatcher: EventDispatcher,
    config: EngineConfig,
}

impl std::fmt::Debug for JobEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobEngine")
            .field("limits", self.scheduler.limits())
            .finish_non_exhaustive()
    }
}

impl JobEngine {
    /// Build an engine over an open database.
    pub async fn open(
        db: Database,
        config: EngineConfig,
        subscribers: Vec<Arc<dyn EventSubscriber>>,
    ) -> Result<Self, JobError> {
        let store = JobStore::open(db.clone()).await?;
        let history = JobHistory::new(store.read_pool().clone());
        let dispatcher =
            EventDispatcher::new(subscribers, db.writer().clone(), store.events().clone());
        Ok(Self {
            store,
            history,
            scheduler: ResourceScheduler::new(config.limits.clone()),
            cancellation: CancellationRegistry::new(),
            consent: ConsentGate::new(),
            dispatcher,
            config,
        })
    }

    /// The transactional store.
    pub fn store(&self) -> &JobStore {
        &self.store
    }

    /// Read-only history queries.
    pub fn history(&self) -> &JobHistory {
        &self.history
    }

    /// The bounded resource scheduler.
    pub fn scheduler(&self) -> &ResourceScheduler {
        &self.scheduler
    }

    /// The cooperative cancellation registry.
    pub fn cancellation(&self) -> &CancellationRegistry {
        &self.cancellation
    }

    /// The fail-closed consent gate.
    pub fn consent(&self) -> &ConsentGate {
        &self.consent
    }

    /// The post-commit event dispatcher.
    pub fn dispatcher(&self) -> &EventDispatcher {
        &self.dispatcher
    }

    /// Run startup recovery and replay undelivered events.
    ///
    /// Must complete before the scheduler accepts work: the reconciler assumes
    /// no job is running.
    pub async fn recover(&self) -> Result<ReconcileReport, JobError> {
        let mut reconciler = Reconciler::new(self.store.clone(), self.config.reconciler.clone());
        if let Some(root) = &self.config.archive_root {
            let archive = plaudon_persistence::archive::MediaArchive::open(root)
                .map_err(|e| JobError::Journal(e.to_string()))?;
            let db = plaudon_persistence::Database::open(self.store.path())
                .await
                .map_err(|e| JobError::Journal(e.to_string()))?;
            let journal = plaudon_persistence::journal::ImportJournal::new(db, archive.clone());
            reconciler = reconciler
                .with_import_journal(journal)
                .with_asset_resolver(Arc::new(crate::reconcile::ArchiveAssetResolver::new(
                    archive,
                )));
        }
        let report = reconciler.run().await?;
        self.dispatcher.replay_pending(256).await?;
        Ok(report)
    }

    /// Enqueue a job, deduplicating on its canonical key (§14.2).
    pub async fn enqueue(&self, request: EnqueueRequest) -> Result<EnqueueOutcome, JobError> {
        let outcome = self.store.enqueue(request).await?;
        self.cancellation.register(outcome.job_id());
        self.pump().await;
        Ok(outcome)
    }

    /// Run preflight for a queued job and either admit it or park it visibly.
    pub async fn preflight(
        &self,
        job: JobId,
        request: ResourceRequest,
        system: SystemResources,
    ) -> Result<PreflightDecision, JobError> {
        self.store
            .transition(
                job,
                JobState::Preflight,
                TransitionCause::system("preflight"),
            )
            .await?;
        let decision = preflight(request, system);
        if let PreflightDecision::Park { reason, .. } = decision {
            self.store
                .park(job, reason, TransitionCause::system("preflight_parked"))
                .await?;
            self.pump().await;
        }
        Ok(decision)
    }

    /// Wait for a slot in `class`, honouring cancellation while queued.
    pub async fn admit(&self, job: JobId, class: ResourceClass) -> Result<Admission, JobError> {
        let token = self.cancellation.register(job);
        self.scheduler.admit(class, job, &token).await
    }

    /// The cancellation token for `job`, creating one if needed.
    pub fn token(&self, job: JobId) -> CancellationToken {
        self.cancellation.register(job)
    }

    /// Record a cloud attempt and transition to `CloudSending`, fail-closed on
    /// consent (§10.7, §16.3).
    ///
    /// The ledger row is written **before** the state moves, so a crash in the
    /// window leaves evidence that something may have been sent.
    #[allow(clippy::too_many_arguments)]
    pub async fn begin_dispatch(
        &self,
        job: JobId,
        content_sha256: impl Into<String>,
        request_family: impl Into<String>,
        provider: Option<String>,
        model: impl Into<String>,
        route: Option<String>,
        privacy_policy_snapshot: Option<String>,
        destination_digest: u64,
    ) -> Result<String, JobError> {
        let record = self.store.get(job).await?;
        let consent_id = record
            .consent_id
            .clone()
            .ok_or(JobError::DispatchRefused(DispatchRefusal::NoConsentGrant))?;
        self.consent
            .authorize(
                &consent_id,
                destination_digest,
                record.state == JobState::CloudOutcomeUnknown,
            )
            .map_err(JobError::DispatchRefused)?;

        let attempt_id = self
            .store
            .record_attempt(
                job,
                content_sha256,
                request_family,
                provider,
                model,
                route,
                privacy_policy_snapshot,
                Some(consent_id),
            )
            .await?;

        self.store
            .transition(
                job,
                JobState::CloudSending,
                TransitionCause::system("dispatch").with_consent_verified(),
            )
            .await?;
        self.store
            .update_attempt(&attempt_id, DispatchState::Sending, None)
            .await?;
        crate::fault::trip("dispatch:after_send_before_response");
        self.pump().await;
        Ok(attempt_id)
    }

    /// Record a provider response and move the job on to verification.
    pub async fn finish_dispatch(
        &self,
        job: JobId,
        attempt_id: &str,
        provider_request_id: Option<String>,
    ) -> Result<JobRecord, JobError> {
        self.store
            .update_attempt(attempt_id, DispatchState::Completed, provider_request_id)
            .await?;
        let record = self
            .store
            .transition(
                job,
                JobState::Verifying,
                TransitionCause::system("response_received"),
            )
            .await?;
        self.pump().await;
        Ok(record)
    }

    /// Mark a dispatched request whose response never arrived.
    ///
    /// Possibly billed: never auto-retried (§14.3).
    pub async fn dispatch_outcome_unknown(
        &self,
        job: JobId,
        attempt_id: &str,
        provider_request_id: Option<String>,
    ) -> Result<JobRecord, JobError> {
        self.store
            .update_attempt(
                attempt_id,
                DispatchState::OutcomeUnknown,
                provider_request_id,
            )
            .await?;
        let record = self
            .store
            .transition(
                job,
                JobState::CloudOutcomeUnknown,
                TransitionCause::system("no_response"),
            )
            .await?;
        self.pump().await;
        Ok(record)
    }

    /// Explicit, cost-acknowledged user retry of a possibly-billed job.
    pub async fn user_retry_possibly_billed(&self, job: JobId) -> Result<JobRecord, JobError> {
        let record = self
            .store
            .transition(
                job,
                JobState::CloudReady,
                TransitionCause::user("explicit_retry").with_cost_acknowledged(),
            )
            .await?;
        self.pump().await;
        Ok(record)
    }

    /// Commit outputs and complete the job (§14.1).
    pub async fn complete(
        &self,
        job: JobId,
        outputs: Vec<JobOutput>,
    ) -> Result<JobRecord, JobError> {
        self.store
            .transition(job, JobState::Committing, TransitionCause::system("commit"))
            .await?;
        let record = self
            .store
            .complete(job, outputs, TransitionCause::system("committed"))
            .await?;
        self.cancellation.forget(job);
        self.pump().await;
        Ok(record)
    }

    /// Park a running job because the environment changed (§6.3A).
    ///
    /// Never produces `Completed` and never fails the job.
    pub async fn interrupt(
        &self,
        job: JobId,
        cause: InterruptionCause,
    ) -> Result<JobRecord, JobError> {
        let record = self
            .store
            .park(
                job,
                cause.wait_reason(),
                TransitionCause::system(cause.as_str()),
            )
            .await?;
        self.pump().await;
        Ok(record)
    }

    /// Resume a parked job once its blocking condition cleared.
    ///
    /// Continues from the last committed checkpoint. Cloud work is **not**
    /// re-dispatched: a job whose outcome is unknown stays unknown.
    pub async fn resume(&self, job: JobId) -> Result<JobRecord, JobError> {
        let record = self.store.get(job).await?;
        if record.state == JobState::CloudOutcomeUnknown {
            return Err(JobError::DispatchRefused(DispatchRefusal::OutcomeUnknown));
        }
        let resumed = self
            .store
            .transition(job, JobState::Queued, TransitionCause::system("resume"))
            .await?;
        self.pump().await;
        Ok(resumed)
    }

    /// Record a consent grant: durable row plus the in-memory dispatch gate.
    pub async fn grant_consent(
        &self,
        consent_id: &str,
        destination_digest: u64,
        engine: &str,
        model_or_provider: &str,
    ) -> Result<(), JobError> {
        self.store
            .ensure_consent_grant(
                consent_id,
                "cloud_inference",
                "v1",
                engine,
                model_or_provider,
            )
            .await?;
        self.consent.grant(consent_id, destination_digest);
        Ok(())
    }

    /// Revoke a consent grant: every subsequent dispatch under it is refused
    /// and affected queued jobs park on authorization (§14.7).
    pub async fn revoke_consent(&self, consent_id: &str) -> Result<Vec<JobId>, JobError> {
        self.consent.revoke(consent_id);
        self.store.revoke_consent_grant(consent_id).await?;
        let mut parked = Vec::new();
        for job in self
            .store
            .jobs_in_states(&[JobState::Queued, JobState::Preflight, JobState::CloudReady])
            .await?
        {
            if job.consent_id.as_deref() == Some(consent_id) {
                self.store
                    .park(
                        job.id,
                        crate::scheduler::WaitReason::ConsentRequired,
                        TransitionCause::user("consent_revoked"),
                    )
                    .await?;
                parked.push(job.id);
            }
        }
        self.pump().await;
        Ok(parked)
    }

    /// Acknowledge a cancellation request immediately (§15.2).
    ///
    /// Signals the cooperative token and records the request; the durable
    /// terminal state follows from [`JobEngine::finish_cancellation`].
    pub async fn cancel(&self, job: JobId) -> Result<CancellationAck, JobError> {
        let record = self.store.get(job).await?;
        if record.state.is_terminal() {
            return Ok(CancellationAck {
                job_id: job,
                accepted: false,
                state: record.state,
            });
        }
        self.cancellation.register(job).cancel();
        self.store.mark_cancel_requested(job).await?;
        let record = self
            .store
            .transition(
                job,
                JobState::Cancelling,
                TransitionCause::user("cancel_requested"),
            )
            .await?;
        self.pump().await;
        Ok(CancellationAck {
            job_id: job,
            accepted: true,
            state: record.state,
        })
    }

    /// Complete a cancellation: discard incomplete derivatives and settle in
    /// `Cancelled`.
    pub async fn finish_cancellation(
        &self,
        job: JobId,
        derivatives: Vec<PathBuf>,
    ) -> Result<CancellationOutcome, JobError> {
        let record = self.store.get(job).await?;
        let discarded =
            discard_incomplete_derivatives(&self.config.reconciler.staging_root, &derivatives)
                .map_err(|e| JobError::io("discard incomplete derivatives", e))?;
        self.store
            .transition(job, JobState::Cancelled, TransitionCause::user("cancelled"))
            .await?;
        self.cancellation.forget(job);
        self.pump().await;
        Ok(if record.dispatched {
            CancellationOutcome::cloud_possibly_billed(job, discarded)
        } else {
            CancellationOutcome::local(job, discarded)
        })
    }

    /// Record a failure with a stable code.
    pub async fn fail(
        &self,
        job: JobId,
        terminal: bool,
        code: AppErrorCode,
        reason: impl Into<String>,
    ) -> Result<JobRecord, JobError> {
        let to = if terminal {
            JobState::FailedTerminal
        } else {
            JobState::FailedRetryable
        };
        let record = self
            .store
            .fail(job, to, code, TransitionCause::system(reason))
            .await?;
        if terminal {
            self.cancellation.forget(job);
        }
        self.pump().await;
        Ok(record)
    }

    /// Deliver any events committed but not yet handed to subscribers.
    ///
    /// Called after every mutation. Reads only committed rows, so a failure
    /// here can never affect a job.
    pub async fn pump(&self) {
        crate::fault::trip("event:after_commit_before_dispatch");
        if let Err(error) = self.dispatcher.replay_pending(256).await {
            tracing::warn!(%error, "event dispatch pump failed; events remain in the durable log");
        }
    }
}
