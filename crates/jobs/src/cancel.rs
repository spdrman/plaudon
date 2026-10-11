//! Cancellation, interruption handling and consent gating
//! (SPEC v1.3 §6.3A, §14.4, §14.7, §16.3).
//!
//! * Cancellation is cooperative: a token is signalled, the worker observes it
//!   at the next checkpoint, incomplete derivatives are discarded and the job
//!   transitions `Cancelling → Cancelled` durably.
//! * An OS sleep, user logout, keychain lock or transport interruption maps to
//!   a **persisted wait state**, never to a broken successful import.
//! * Cloud cancellation stops *our* future work only. It never claims a refund
//!   or reversal.
//! * Consent revocation is fail-closed: zero subsequent dispatches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use plaudon_domain::ids::JobId;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::DispatchRefusal;
use crate::journal::is_app_owned_staging_file;
use crate::scheduler::WaitReason;
use crate::state::JobState;

/// Message shown whenever a cloud job is cancelled or its outcome is unknown.
///
/// §14.7: "Cloud cancellation only stops our future work and may not reverse an
/// accepted request or upstream charge." Kept as a constant so the UI, the CLI
/// and the tests cannot drift apart.
pub const CLOUD_CANCELLATION_NOTICE: &str = "Cancellation stops further work on this device. A request that already reached the provider may have been accepted and billed; it cannot be reversed from here.";

/// Message shown when consent is revoked while a transfer is in flight.
///
/// §14.7: "we stop further dispatches and explain that transmitted bytes cannot
/// be recalled."
pub const CONSENT_REVOKED_NOTICE: &str = "Consent was revoked. No further content will be sent. Bytes already transmitted to the provider cannot be recalled.";

/// Outcome reported to the user after cancelling a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationOutcome {
    pub job_id: JobId,
    /// The durable state the job settled in.
    pub final_state: JobState,
    /// `true` when content had already been dispatched to a provider.
    pub possibly_billed: bool,
    /// Always `false`: we never claim an upstream reversal.
    pub refunded: bool,
    /// Human-readable explanation; `CLOUD_CANCELLATION_NOTICE` when
    /// `possibly_billed`.
    pub message: String,
    /// Incomplete derivatives removed as part of cancellation.
    pub discarded_derivatives: Vec<PathBuf>,
}

impl CancellationOutcome {
    /// Build the outcome for a local-only job.
    pub fn local(job_id: JobId, discarded: Vec<PathBuf>) -> Self {
        Self {
            job_id,
            final_state: JobState::Cancelled,
            possibly_billed: false,
            refunded: false,
            message: "Cancelled. Incomplete intermediate files were discarded.".to_string(),
            discarded_derivatives: discarded,
        }
    }

    /// Build the outcome for a job that had already dispatched cloud work.
    pub fn cloud_possibly_billed(job_id: JobId, discarded: Vec<PathBuf>) -> Self {
        Self {
            job_id,
            final_state: JobState::Cancelled,
            possibly_billed: true,
            refunded: false,
            message: CLOUD_CANCELLATION_NOTICE.to_string(),
            discarded_derivatives: discarded,
        }
    }
}

/// Acknowledgement returned immediately by `cancel_job` (§15.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationAck {
    pub job_id: JobId,
    /// `true` when the engine knew the job and signalled its token.
    pub accepted: bool,
    /// The state recorded at the moment of acknowledgement.
    pub state: JobState,
}

/// Registry of live cooperative cancellation tokens.
#[derive(Clone, Default)]
pub struct CancellationRegistry {
    tokens: Arc<Mutex<HashMap<JobId, CancellationToken>>>,
}

impl CancellationRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or return the existing) token for `job`.
    pub fn register(&self, job: JobId) -> CancellationToken {
        let mut guard = self.tokens.lock().expect("cancellation registry poisoned");
        guard.entry(job).or_default().clone()
    }

    /// Token for `job`, if registered.
    pub fn get(&self, job: JobId) -> Option<CancellationToken> {
        self.tokens
            .lock()
            .expect("cancellation registry poisoned")
            .get(&job)
            .cloned()
    }

    /// Signal cancellation. Returns `true` when a token existed.
    ///
    /// Idempotent: repeated cancellation of the same job is a no-op after the
    /// first call.
    pub fn cancel(&self, job: JobId) -> bool {
        let token = self
            .tokens
            .lock()
            .expect("cancellation registry poisoned")
            .get(&job)
            .cloned();
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Signal every live job, e.g. on application shutdown.
    pub fn cancel_all(&self) {
        for token in self
            .tokens
            .lock()
            .expect("cancellation registry poisoned")
            .values()
        {
            token.cancel();
        }
    }

    /// Drop the token for a finished job.
    pub fn forget(&self, job: JobId) {
        self.tokens
            .lock()
            .expect("cancellation registry poisoned")
            .remove(&job);
    }

    /// `true` when `job`'s token has been signalled.
    pub fn is_cancelled(&self, job: JobId) -> bool {
        self.get(job).is_some_and(|t| t.is_cancelled())
    }

    /// Number of registered tokens.
    pub fn len(&self) -> usize {
        self.tokens
            .lock()
            .expect("cancellation registry poisoned")
            .len()
    }

    /// `true` when no tokens are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An environmental interruption that must park a job rather than fail it
/// (§6.3A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionCause {
    /// The OS is about to suspend.
    OsSleep,
    /// The user session is ending.
    UserLogout,
    /// The OS credential store locked.
    KeychainLocked,
    /// A BLE link dropped.
    BluetoothLost,
    /// A mounted volume, watched folder or remote source went away.
    SourceDisconnected,
    /// Network connectivity was lost mid-request.
    NetworkLost,
    /// A model asset was deleted or moved while a job needed it.
    ModelAssetRemoved,
    /// The user revoked a consent grant.
    ConsentRevoked,
    /// The accelerator is occupied by another process.
    GpuBusy,
}

impl InterruptionCause {
    /// Stable persisted name.
    pub const fn as_str(self) -> &'static str {
        match self {
            InterruptionCause::OsSleep => "os_sleep",
            InterruptionCause::UserLogout => "user_logout",
            InterruptionCause::KeychainLocked => "keychain_locked",
            InterruptionCause::BluetoothLost => "bluetooth_lost",
            InterruptionCause::SourceDisconnected => "source_disconnected",
            InterruptionCause::NetworkLost => "network_lost",
            InterruptionCause::ModelAssetRemoved => "model_asset_removed",
            InterruptionCause::ConsentRevoked => "consent_revoked",
            InterruptionCause::GpuBusy => "gpu_busy",
        }
    }

    /// All causes.
    pub const ALL: &'static [InterruptionCause] = &[
        InterruptionCause::OsSleep,
        InterruptionCause::UserLogout,
        InterruptionCause::KeychainLocked,
        InterruptionCause::BluetoothLost,
        InterruptionCause::SourceDisconnected,
        InterruptionCause::NetworkLost,
        InterruptionCause::ModelAssetRemoved,
        InterruptionCause::ConsentRevoked,
        InterruptionCause::GpuBusy,
    ];

    /// The persisted wait reason for this interruption.
    pub const fn wait_reason(self) -> WaitReason {
        match self {
            InterruptionCause::OsSleep => WaitReason::SystemSuspend,
            InterruptionCause::UserLogout => WaitReason::UserDecisionRequired,
            InterruptionCause::KeychainLocked => WaitReason::KeychainLocked,
            InterruptionCause::BluetoothLost | InterruptionCause::SourceDisconnected => {
                WaitReason::SourceUnavailable
            }
            InterruptionCause::NetworkLost => WaitReason::NetworkUnavailable,
            InterruptionCause::ModelAssetRemoved => WaitReason::ModelAssetMissing,
            InterruptionCause::ConsentRevoked => WaitReason::ConsentRequired,
            InterruptionCause::GpuBusy => WaitReason::UserDecisionRequired,
        }
    }

    /// The persisted wait state for this interruption. Never terminal, never
    /// `Completed`.
    pub const fn wait_state(self) -> JobState {
        self.wait_reason().wait_state()
    }

    /// `true` when the job resumes automatically once the condition clears.
    pub const fn resumes_automatically(self) -> bool {
        matches!(
            self,
            InterruptionCause::OsSleep
                | InterruptionCause::BluetoothLost
                | InterruptionCause::SourceDisconnected
                | InterruptionCause::NetworkLost
                | InterruptionCause::ModelAssetRemoved
                | InterruptionCause::GpuBusy
        )
    }
}

/// Fail-closed gate in front of every cloud dispatch (§16.3, §10.8).
///
/// The gate records *attempted* dispatches so the "consent revoked mid-queue ⇒
/// zero subsequent dispatch attempts" property is directly observable.
#[derive(Clone, Default)]
pub struct ConsentGate {
    inner: Arc<ConsentGateInner>,
}

#[derive(Default)]
struct ConsentGateInner {
    grants: Mutex<HashMap<String, GrantState>>,
    authorized: AtomicU64,
    refused: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GrantState {
    revoked: bool,
    expired: bool,
    /// Digest of the destination (provider + model + route + retention) the
    /// grant covers. A change invalidates the grant (§10.8).
    destination: u64,
}

impl ConsentGate {
    /// Empty gate: with no grants recorded, every dispatch is refused.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a live grant covering `destination`.
    pub fn grant(&self, consent_id: impl Into<String>, destination: u64) {
        self.inner
            .grants
            .lock()
            .expect("consent gate poisoned")
            .insert(
                consent_id.into(),
                GrantState {
                    revoked: false,
                    expired: false,
                    destination,
                },
            );
    }

    /// Revoke a grant. Subsequent dispatches under it are refused.
    pub fn revoke(&self, consent_id: &str) {
        if let Some(state) = self
            .inner
            .grants
            .lock()
            .expect("consent gate poisoned")
            .get_mut(consent_id)
        {
            state.revoked = true;
        }
    }

    /// Mark a grant expired.
    pub fn expire(&self, consent_id: &str) {
        if let Some(state) = self
            .inner
            .grants
            .lock()
            .expect("consent gate poisoned")
            .get_mut(consent_id)
        {
            state.expired = true;
        }
    }

    /// Authorise one dispatch. Fail-closed on every unknown condition.
    pub fn authorize(
        &self,
        consent_id: &str,
        destination: u64,
        outcome_unknown: bool,
    ) -> Result<(), DispatchRefusal> {
        let decision = {
            let grants = self.inner.grants.lock().expect("consent gate poisoned");
            match grants.get(consent_id) {
                None => Err(DispatchRefusal::NoConsentGrant),
                Some(state) if state.revoked => Err(DispatchRefusal::ConsentRevoked),
                Some(state) if state.expired => Err(DispatchRefusal::ConsentExpired),
                Some(state) if state.destination != destination => {
                    Err(DispatchRefusal::DestinationChanged)
                }
                Some(_) if outcome_unknown => Err(DispatchRefusal::OutcomeUnknown),
                Some(_) => Ok(()),
            }
        };
        match decision {
            Ok(()) => {
                self.inner.authorized.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            Err(refusal) => {
                self.inner.refused.fetch_add(1, Ordering::SeqCst);
                Err(refusal)
            }
        }
    }

    /// Number of dispatches authorised so far.
    pub fn authorized_count(&self) -> u64 {
        self.inner.authorized.load(Ordering::SeqCst)
    }

    /// Number of dispatches refused so far.
    pub fn refused_count(&self) -> u64 {
        self.inner.refused.load(Ordering::SeqCst)
    }
}

/// Remove incomplete derivatives belonging to a cancelled or abandoned job.
///
/// Only app-owned `*.part` files beneath `staging_root` are ever touched. A
/// path outside the staging root — in particular a user's source recording — is
/// skipped and reported, never deleted (§13.3A).
pub fn discard_incomplete_derivatives(
    staging_root: &Path,
    candidates: &[PathBuf],
) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut removed = Vec::new();
    for candidate in candidates {
        if !is_app_owned_staging_file(staging_root, candidate) {
            tracing::warn!(
                path = %candidate.display(),
                "refusing to discard a path outside the app-owned staging root"
            );
            continue;
        }
        match std::fs::remove_file(candidate) {
            Ok(()) => removed.push(candidate.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_idempotent_and_observable() {
        let registry = CancellationRegistry::new();
        let job = JobId::new();
        let token = registry.register(job);
        assert!(!token.is_cancelled());
        assert!(registry.cancel(job));
        assert!(token.is_cancelled());
        assert!(registry.is_cancelled(job));
        // Second cancellation is a no-op, not an error.
        assert!(registry.cancel(job));
        registry.forget(job);
        assert!(!registry.cancel(job));
    }

    #[test]
    fn registering_twice_returns_the_same_token() {
        let registry = CancellationRegistry::new();
        let job = JobId::new();
        let a = registry.register(job);
        let b = registry.register(job);
        a.cancel();
        assert!(b.is_cancelled(), "tokens must be shared, not cloned anew");
    }

    #[test]
    fn every_interruption_maps_to_a_persisted_wait_state() {
        for cause in InterruptionCause::ALL.iter().copied() {
            let state = cause.wait_state();
            assert!(state.is_waiting(), "{cause:?} -> {state}");
            assert_ne!(state, JobState::Completed);
            assert!(!state.is_terminal());
        }
    }

    #[test]
    fn interruption_wait_state_mapping_table() {
        use InterruptionCause as C;
        let expected = [
            (C::OsSleep, JobState::WaitingForUser, WaitReason::SystemSuspend),
            (
                C::UserLogout,
                JobState::WaitingForUser,
                WaitReason::UserDecisionRequired,
            ),
            (
                C::KeychainLocked,
                JobState::WaitingForAuthorization,
                WaitReason::KeychainLocked,
            ),
            (
                C::BluetoothLost,
                JobState::WaitingForSource,
                WaitReason::SourceUnavailable,
            ),
            (
                C::SourceDisconnected,
                JobState::WaitingForSource,
                WaitReason::SourceUnavailable,
            ),
            (
                C::NetworkLost,
                JobState::WaitingForNetwork,
                WaitReason::NetworkUnavailable,
            ),
            (
                C::ModelAssetRemoved,
                JobState::WaitingForModel,
                WaitReason::ModelAssetMissing,
            ),
            (
                C::ConsentRevoked,
                JobState::WaitingForAuthorization,
                WaitReason::ConsentRequired,
            ),
            (
                C::GpuBusy,
                JobState::WaitingForUser,
                WaitReason::UserDecisionRequired,
            ),
        ];
        for (cause, state, reason) in expected {
            assert_eq!(cause.wait_state(), state, "{cause:?}");
            assert_eq!(cause.wait_reason(), reason, "{cause:?}");
        }
    }

    #[test]
    fn cloud_cancellation_never_claims_a_refund() {
        let outcome = CancellationOutcome::cloud_possibly_billed(JobId::new(), Vec::new());
        assert!(outcome.possibly_billed);
        assert!(!outcome.refunded);
        assert_eq!(outcome.message, CLOUD_CANCELLATION_NOTICE);
        let lowered = outcome.message.to_lowercase();
        assert!(!lowered.contains("refund"));
        assert!(!lowered.contains("reversed and"));
        assert!(lowered.contains("cannot be reversed"));
    }

    #[test]
    fn consent_revocation_refuses_every_subsequent_dispatch() {
        let gate = ConsentGate::new();
        gate.grant("consent-1", 42);
        assert!(gate.authorize("consent-1", 42, false).is_ok());
        gate.revoke("consent-1");
        for _ in 0..5 {
            assert_eq!(
                gate.authorize("consent-1", 42, false),
                Err(DispatchRefusal::ConsentRevoked)
            );
        }
        assert_eq!(gate.authorized_count(), 1);
        assert_eq!(gate.refused_count(), 5);
    }

    #[test]
    fn gate_fails_closed_for_unknown_expired_and_changed_destinations() {
        let gate = ConsentGate::new();
        assert_eq!(
            gate.authorize("missing", 1, false),
            Err(DispatchRefusal::NoConsentGrant)
        );
        gate.grant("c", 1);
        assert_eq!(
            gate.authorize("c", 2, false),
            Err(DispatchRefusal::DestinationChanged)
        );
        assert_eq!(
            gate.authorize("c", 1, true),
            Err(DispatchRefusal::OutcomeUnknown)
        );
        gate.expire("c");
        assert_eq!(
            gate.authorize("c", 1, false),
            Err(DispatchRefusal::ConsentExpired)
        );
        assert_eq!(gate.authorized_count(), 0);
    }

    #[test]
    fn only_app_owned_staging_files_are_discarded() {
        let root = tempfile::tempdir().expect("tempdir");
        let staging = root.path().join("imports");
        std::fs::create_dir_all(&staging).expect("mkdir");

        let derivative = staging.join("abc.part");
        std::fs::write(&derivative, b"partial").expect("write");
        let source = root.path().join("original.wav");
        std::fs::write(&source, b"user audio").expect("write");

        let removed = discard_incomplete_derivatives(
            &staging,
            &[derivative.clone(), source.clone()],
        )
        .expect("discard");

        assert_eq!(removed, vec![derivative.clone()]);
        assert!(!derivative.exists());
        assert_eq!(std::fs::read(&source).expect("read"), b"user audio");
    }

    #[test]
    fn discarding_a_missing_derivative_is_not_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let staging = root.path().join("imports");
        std::fs::create_dir_all(&staging).expect("mkdir");
        let removed =
            discard_incomplete_derivatives(&staging, &[staging.join("gone.part")]).expect("discard");
        assert!(removed.is_empty());
    }

    #[test]
    fn consent_revoked_notice_is_explicit_about_transmitted_bytes() {
        let lowered = CONSENT_REVOKED_NOTICE.to_lowercase();
        assert!(lowered.contains("cannot be recalled"));
        assert!(lowered.contains("no further content"));
    }
}
