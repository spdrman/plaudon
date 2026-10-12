//! Job state machine (SPEC v1.3 §14.1, §14.7).
//!
//! §14.1 defines the coarse lifecycle `QUEUED → PREFLIGHT → RUNNING → COMMITTING
//! → COMPLETED` with exceptional transitions to `WAITING_FOR_USER`,
//! `WAITING_FOR_NETWORK`, `RETRYABLE_FAILURE`, `TERMINAL_FAILURE`, `CANCELLING`
//! and `CANCELLED`. §14.7 refines the *persisted* vocabulary into `Queued`,
//! `WaitingForModel`, `WaitingForSource`, `WaitingForAuthorization`,
//! `RunningLocal`, `CloudReady`, `CloudSending`, `CloudOutcomeUnknown`,
//! `Verifying`, `Completed`, `FailedRetryable`, `FailedTerminal`, `Cancelled`.
//!
//! Both vocabularies are persisted here as one closed enum. The §14.1 generic
//! `RUNNING` is represented by the two refined running states `RunningLocal`
//! and `CloudSending`; every other §14.1 name has a 1:1 member below.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Every persisted job state. Serialised as `snake_case` in the database, in
/// the durable event log and in UI payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// §14.1 `QUEUED` / §14.7 `Queued`. Queue intent is durable.
    Queued,
    /// §14.1 `PREFLIGHT`. Capability, consent and resource checks.
    Preflight,
    /// §14.1 `WAITING_FOR_USER`. Blocked on a human decision (e.g. logout,
    /// duplicate resolution, cost acknowledgement).
    WaitingForUser,
    /// §14.1 `WAITING_FOR_NETWORK`.
    WaitingForNetwork,
    /// §14.7 `WaitingForModel`. Required model asset absent or removed.
    WaitingForModel,
    /// §14.7 `WaitingForSource`. Source/device/volume unavailable.
    WaitingForSource,
    /// §14.7 `WaitingForAuthorization`. Keychain locked, consent missing or
    /// revoked, credential unavailable.
    WaitingForAuthorization,
    /// §14.7 `RunningLocal`. Local compute stage in progress.
    RunningLocal,
    /// §14.7 `CloudReady`. Preflight passed, dispatch authorised, not yet sent.
    CloudReady,
    /// §14.7 `CloudSending`. Request in flight; bytes may already have left.
    CloudSending,
    /// §14.7 `CloudOutcomeUnknown`. Dispatched without a recorded response:
    /// **possibly billed**. Never auto-retried (§14.3, §14.7).
    CloudOutcomeUnknown,
    /// §14.7 `Verifying`. Output produced, integrity being verified.
    Verifying,
    /// §14.1 `COMMITTING`. Durable commit of outputs and metadata in progress.
    Committing,
    /// §14.1/§14.7 `Completed`. Only reachable with committed outputs.
    Completed,
    /// §14.1 `RETRYABLE_FAILURE` / §14.7 `FailedRetryable`.
    FailedRetryable,
    /// §14.1 `TERMINAL_FAILURE` / §14.7 `FailedTerminal`.
    FailedTerminal,
    /// §14.1 `CANCELLING`. Cooperative cancellation requested and observed.
    Cancelling,
    /// §14.1/§14.7 `Cancelled`.
    Cancelled,
}

/// Coarse UI bucket for a state. Lets consumers avoid matching 18 arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    /// Accepted, not started.
    Pending,
    /// Parked on an external condition; resumable.
    Waiting,
    /// Doing work.
    Active,
    /// Producing/committing durable output.
    Finishing,
    /// No further automatic progress.
    Terminal,
}

impl JobState {
    /// All states, in declaration order. Used by coverage tests.
    pub const ALL: &'static [JobState] = &[
        JobState::Queued,
        JobState::Preflight,
        JobState::WaitingForUser,
        JobState::WaitingForNetwork,
        JobState::WaitingForModel,
        JobState::WaitingForSource,
        JobState::WaitingForAuthorization,
        JobState::RunningLocal,
        JobState::CloudReady,
        JobState::CloudSending,
        JobState::CloudOutcomeUnknown,
        JobState::Verifying,
        JobState::Committing,
        JobState::Completed,
        JobState::FailedRetryable,
        JobState::FailedTerminal,
        JobState::Cancelling,
        JobState::Cancelled,
    ];

    /// Stable persisted/serde name.
    pub const fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Preflight => "preflight",
            JobState::WaitingForUser => "waiting_for_user",
            JobState::WaitingForNetwork => "waiting_for_network",
            JobState::WaitingForModel => "waiting_for_model",
            JobState::WaitingForSource => "waiting_for_source",
            JobState::WaitingForAuthorization => "waiting_for_authorization",
            JobState::RunningLocal => "running_local",
            JobState::CloudReady => "cloud_ready",
            JobState::CloudSending => "cloud_sending",
            JobState::CloudOutcomeUnknown => "cloud_outcome_unknown",
            JobState::Verifying => "verifying",
            JobState::Committing => "committing",
            JobState::Completed => "completed",
            JobState::FailedRetryable => "failed_retryable",
            JobState::FailedTerminal => "failed_terminal",
            JobState::Cancelling => "cancelling",
            JobState::Cancelled => "cancelled",
        }
    }

    /// Parse a persisted name.
    pub fn parse(raw: &str) -> Option<JobState> {
        JobState::ALL.iter().copied().find(|s| s.as_str() == raw)
    }

    /// `Completed`, `FailedTerminal` and `Cancelled` are absorbing: they have no
    /// outgoing edges at all (§14.1).
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Completed | JobState::FailedTerminal | JobState::Cancelled
        )
    }

    /// States that park the job until an external condition clears.
    pub const fn is_waiting(self) -> bool {
        matches!(
            self,
            JobState::WaitingForUser
                | JobState::WaitingForNetwork
                | JobState::WaitingForModel
                | JobState::WaitingForSource
                | JobState::WaitingForAuthorization
        )
    }

    /// `true` when the engine may make progress on its own.
    pub const fn is_retryable(self) -> bool {
        matches!(self, JobState::FailedRetryable)
    }

    /// `true` when only a deliberate human action can move the job on.
    /// `CloudOutcomeUnknown` qualifies: automatic progress is forbidden because
    /// the request may already have been billed (§14.3, §14.7).
    pub const fn requires_user_action(self) -> bool {
        matches!(
            self,
            JobState::WaitingForUser
                | JobState::WaitingForAuthorization
                | JobState::CloudOutcomeUnknown
        )
    }

    /// `true` when the job has dispatched user content to a cloud provider and
    /// the outcome is not known. Used for "possibly billed" messaging.
    pub const fn is_possibly_billed(self) -> bool {
        matches!(self, JobState::CloudOutcomeUnknown)
    }

    /// Coarse UI bucket.
    pub const fn phase(self) -> JobPhase {
        match self {
            JobState::Queued | JobState::Preflight => JobPhase::Pending,
            // Parked until something external changes. `CloudOutcomeUnknown`
            // belongs here because only a human can move it on, and
            // `FailedRetryable` because the engine will requeue it.
            JobState::WaitingForUser
            | JobState::WaitingForNetwork
            | JobState::WaitingForModel
            | JobState::WaitingForSource
            | JobState::WaitingForAuthorization
            | JobState::CloudOutcomeUnknown
            | JobState::FailedRetryable => JobPhase::Waiting,
            JobState::RunningLocal | JobState::CloudReady | JobState::CloudSending => {
                JobPhase::Active
            }
            JobState::Verifying | JobState::Committing | JobState::Cancelling => {
                JobPhase::Finishing
            }
            JobState::Completed | JobState::FailedTerminal | JobState::Cancelled => {
                JobPhase::Terminal
            }
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who requested a transition. Guards distinguish automatic progress from a
/// deliberate human decision (§14.7: "Startup recovery must never turn
/// `CloudOutcomeUnknown` into an unapproved fresh request").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// The scheduler / worker making automatic progress.
    System,
    /// An explicit, attributable user action from the UI or local CLI.
    User,
    /// The startup recovery reconciler (C2.3).
    Recovery,
}

/// Why a transition is being applied. Carries the evidence guards need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionCause {
    pub actor: Actor,
    /// Short stable reason code, e.g. `preflight_ok`, `keychain_locked`.
    pub reason: String,
    /// Set when a live consent grant has been verified immediately before a
    /// cloud dispatch (§16.3 fail-closed preflight).
    #[serde(default)]
    pub consent_verified: bool,
    /// Set when the user explicitly acknowledged that re-running a
    /// possibly-billed cloud request may incur a second charge (§10.4).
    #[serde(default)]
    pub cost_acknowledged: bool,
    /// Optional human-readable detail persisted on the transition audit row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl TransitionCause {
    /// Automatic engine progress.
    pub fn system(reason: impl Into<String>) -> Self {
        Self {
            actor: Actor::System,
            reason: reason.into(),
            consent_verified: false,
            cost_acknowledged: false,
            detail: None,
        }
    }

    /// Explicit user action.
    pub fn user(reason: impl Into<String>) -> Self {
        Self {
            actor: Actor::User,
            reason: reason.into(),
            consent_verified: false,
            cost_acknowledged: false,
            detail: None,
        }
    }

    /// Startup reconciler action.
    pub fn recovery(reason: impl Into<String>) -> Self {
        Self {
            actor: Actor::Recovery,
            reason: reason.into(),
            consent_verified: false,
            cost_acknowledged: false,
            detail: None,
        }
    }

    /// Mark that a consent grant resolved for this exact destination.
    #[must_use]
    pub fn with_consent_verified(mut self) -> Self {
        self.consent_verified = true;
        self
    }

    /// Mark that the user accepted possible duplicate billing.
    #[must_use]
    pub fn with_cost_acknowledged(mut self) -> Self {
        self.cost_acknowledged = true;
        self
    }

    /// Attach free-form detail for the audit row.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Extra evidence a guard may require, gathered inside the same transaction as
/// the transition itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuardFacts {
    /// A committed row exists in `job_outputs` for this job.
    pub has_committed_output: bool,
}

/// Reason a transition was rejected. Typed, stable, never leaks SQL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobTransitionError {
    #[error("illegal job transition {from} -> {to}")]
    IllegalEdge { from: JobState, to: JobState },
    #[error("job is already in terminal state {state}; no further transitions are possible")]
    AlreadyTerminal { state: JobState },
    #[error("transition {from} -> {to} requires an explicit user action, actor was {actor:?}")]
    RequiresUserAction {
        from: JobState,
        to: JobState,
        actor: Actor,
    },
    #[error(
        "re-dispatching a possibly-billed cloud request requires explicit cost acknowledgement"
    )]
    RequiresCostAcknowledgement,
    #[error("cloud dispatch requires a verified consent grant for this exact destination")]
    RequiresVerifiedConsent,
    #[error(
        "a job may only reach Completed once its outputs are committed in the same transaction"
    )]
    MissingCommittedOutput,
    #[error("expected job to be in state {expected}, found {actual}")]
    StateConflict {
        expected: JobState,
        actual: JobState,
    },
}

/// One legal edge of the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: JobState,
    pub to: JobState,
}

const fn e(from: JobState, to: JobState) -> Edge {
    Edge { from, to }
}

use JobState as S;

/// The complete legal transition table. Any pair absent from this table is
/// rejected with [`JobTransitionError::IllegalEdge`].
pub const TRANSITIONS: &[Edge] = &[
    // --- queue / preflight -------------------------------------------------
    e(S::Queued, S::Preflight),
    e(S::Queued, S::WaitingForUser),
    e(S::Queued, S::WaitingForNetwork),
    e(S::Queued, S::WaitingForModel),
    e(S::Queued, S::WaitingForSource),
    e(S::Queued, S::WaitingForAuthorization),
    e(S::Queued, S::FailedTerminal),
    e(S::Queued, S::Cancelling),
    e(S::Preflight, S::RunningLocal),
    e(S::Preflight, S::CloudReady),
    e(S::Preflight, S::WaitingForUser),
    e(S::Preflight, S::WaitingForNetwork),
    e(S::Preflight, S::WaitingForModel),
    e(S::Preflight, S::WaitingForSource),
    e(S::Preflight, S::WaitingForAuthorization),
    e(S::Preflight, S::FailedRetryable),
    e(S::Preflight, S::FailedTerminal),
    e(S::Preflight, S::Cancelling),
    // --- waiting states resume via the queue or straight back to preflight --
    e(S::WaitingForUser, S::Queued),
    e(S::WaitingForUser, S::Preflight),
    e(S::WaitingForUser, S::FailedTerminal),
    e(S::WaitingForUser, S::Cancelling),
    e(S::WaitingForNetwork, S::Queued),
    e(S::WaitingForNetwork, S::Preflight),
    e(S::WaitingForNetwork, S::FailedTerminal),
    e(S::WaitingForNetwork, S::Cancelling),
    e(S::WaitingForModel, S::Queued),
    e(S::WaitingForModel, S::Preflight),
    e(S::WaitingForModel, S::FailedTerminal),
    e(S::WaitingForModel, S::Cancelling),
    e(S::WaitingForSource, S::Queued),
    e(S::WaitingForSource, S::Preflight),
    e(S::WaitingForSource, S::FailedTerminal),
    e(S::WaitingForSource, S::Cancelling),
    e(S::WaitingForAuthorization, S::Queued),
    e(S::WaitingForAuthorization, S::Preflight),
    e(S::WaitingForAuthorization, S::FailedTerminal),
    e(S::WaitingForAuthorization, S::Cancelling),
    // --- local execution ----------------------------------------------------
    e(S::RunningLocal, S::Verifying),
    e(S::RunningLocal, S::WaitingForUser),
    e(S::RunningLocal, S::WaitingForNetwork),
    e(S::RunningLocal, S::WaitingForModel),
    e(S::RunningLocal, S::WaitingForSource),
    e(S::RunningLocal, S::WaitingForAuthorization),
    e(S::RunningLocal, S::FailedRetryable),
    e(S::RunningLocal, S::FailedTerminal),
    e(S::RunningLocal, S::Cancelling),
    // --- cloud execution ----------------------------------------------------
    e(S::CloudReady, S::CloudSending),
    e(S::CloudReady, S::WaitingForNetwork),
    e(S::CloudReady, S::WaitingForAuthorization),
    e(S::CloudReady, S::WaitingForUser),
    e(S::CloudReady, S::FailedRetryable),
    e(S::CloudReady, S::FailedTerminal),
    e(S::CloudReady, S::Cancelling),
    e(S::CloudSending, S::Verifying),
    e(S::CloudSending, S::CloudOutcomeUnknown),
    e(S::CloudSending, S::FailedRetryable),
    e(S::CloudSending, S::FailedTerminal),
    e(S::CloudSending, S::Cancelling),
    // `CloudOutcomeUnknown` is terminal for *automatic* progress. The only way
    // back into a dispatch is an explicit, cost-acknowledged user action, and
    // it must pass through `CloudReady` (which re-runs the consent preflight).
    e(S::CloudOutcomeUnknown, S::CloudReady),
    e(S::CloudOutcomeUnknown, S::FailedTerminal),
    e(S::CloudOutcomeUnknown, S::Cancelling),
    // --- verify / commit ----------------------------------------------------
    e(S::Verifying, S::Committing),
    e(S::Verifying, S::FailedRetryable),
    e(S::Verifying, S::FailedTerminal),
    e(S::Verifying, S::Cancelling),
    e(S::Committing, S::Completed),
    // A failed commit rolls the transaction back; if the engine re-classifies
    // explicitly it may step back to verification or to a failure state.
    e(S::Committing, S::Verifying),
    e(S::Committing, S::FailedRetryable),
    e(S::Committing, S::FailedTerminal),
    // --- failure / cancellation --------------------------------------------
    e(S::FailedRetryable, S::Queued),
    e(S::FailedRetryable, S::FailedTerminal),
    e(S::FailedRetryable, S::Cancelling),
    e(S::Cancelling, S::Cancelled),
    e(S::Cancelling, S::FailedTerminal),
];

/// `true` when `from -> to` appears in [`TRANSITIONS`].
pub fn is_legal_edge(from: JobState, to: JobState) -> bool {
    TRANSITIONS.iter().any(|t| t.from == from && t.to == to)
}

/// Legal successors of `state`.
pub fn successors(state: JobState) -> Vec<JobState> {
    TRANSITIONS
        .iter()
        .filter(|t| t.from == state)
        .map(|t| t.to)
        .collect()
}

/// Validate a transition against the table and all guards.
///
/// This is pure: it performs no I/O. Callers run it *inside* the same SQLite
/// transaction that writes the new state, passing [`GuardFacts`] read in that
/// transaction.
pub fn check_transition(
    from: JobState,
    to: JobState,
    cause: &TransitionCause,
    facts: GuardFacts,
) -> Result<(), JobTransitionError> {
    if from.is_terminal() {
        return Err(JobTransitionError::AlreadyTerminal { state: from });
    }
    if !is_legal_edge(from, to) {
        return Err(JobTransitionError::IllegalEdge { from, to });
    }

    // Guard 1 (§14.1): never `Completed` without durably committed output.
    if to == S::Completed && !facts.has_committed_output {
        return Err(JobTransitionError::MissingCommittedOutput);
    }

    // Guard 2 (§14.3, §14.7): leaving `CloudOutcomeUnknown` towards another
    // dispatch requires an explicit user action with cost acknowledgement.
    // Recovery and the scheduler can never do this.
    if from == S::CloudOutcomeUnknown && to == S::CloudReady {
        if cause.actor != Actor::User {
            return Err(JobTransitionError::RequiresUserAction {
                from,
                to,
                actor: cause.actor,
            });
        }
        if !cause.cost_acknowledged {
            return Err(JobTransitionError::RequiresCostAcknowledgement);
        }
    }

    // Guard 3 (§16.3): a dispatch only starts with a freshly verified consent
    // grant. Fail closed.
    if to == S::CloudSending && !cause.consent_verified {
        return Err(JobTransitionError::RequiresVerifiedConsent);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips_through_its_persisted_name() {
        for state in JobState::ALL {
            assert_eq!(JobState::parse(state.as_str()), Some(*state));
            let json = serde_json::to_string(state).expect("serialize");
            assert_eq!(json, format!("\"{}\"", state.as_str()));
        }
    }

    #[test]
    fn terminal_states_have_no_outgoing_edges() {
        for state in JobState::ALL.iter().copied().filter(|s| s.is_terminal()) {
            assert!(
                successors(state).is_empty(),
                "{state} must be absorbing but has successors"
            );
        }
    }

    #[test]
    fn completed_is_only_reachable_from_committing() {
        let preds: Vec<_> = TRANSITIONS
            .iter()
            .filter(|t| t.to == S::Completed)
            .map(|t| t.from)
            .collect();
        assert_eq!(preds, vec![S::Committing]);
    }

    #[test]
    fn cloud_sending_is_only_reachable_from_cloud_ready() {
        let preds: Vec<_> = TRANSITIONS
            .iter()
            .filter(|t| t.to == S::CloudSending)
            .map(|t| t.from)
            .collect();
        assert_eq!(preds, vec![S::CloudReady]);
    }

    #[test]
    fn transition_table_has_no_duplicate_or_self_edges() {
        let mut seen = std::collections::HashSet::new();
        for t in TRANSITIONS {
            assert_ne!(t.from, t.to, "self edge on {}", t.from);
            assert!(seen.insert((t.from, t.to)), "duplicate edge {t:?}");
        }
    }

    #[test]
    fn every_cancellable_state_can_reach_cancellation() {
        for state in JobState::ALL.iter().copied().filter(|s| !s.is_terminal()) {
            // `Committing` is deliberately not cancellable: the durable commit
            // of outputs and metadata is the one window where abandoning work
            // could leave a torn record. A cancel requested during it is
            // honoured once the commit finishes (§14.7 cooperative
            // cancellation). `Cancelling` is already cancelling.
            if matches!(state, S::Cancelling | S::Committing) {
                continue;
            }
            assert!(
                is_legal_edge(state, S::Cancelling),
                "{state} cannot be cancelled"
            );
        }
        assert!(!is_legal_edge(S::Committing, S::Cancelling));
        assert!(!is_legal_edge(S::Committing, S::Cancelled));
    }

    #[test]
    fn committing_to_completed_requires_committed_output() {
        let cause = TransitionCause::system("commit");
        assert_eq!(
            check_transition(
                S::Committing,
                S::Completed,
                &cause,
                GuardFacts {
                    has_committed_output: false
                }
            ),
            Err(JobTransitionError::MissingCommittedOutput)
        );
        assert!(check_transition(
            S::Committing,
            S::Completed,
            &cause,
            GuardFacts {
                has_committed_output: true
            }
        )
        .is_ok());
    }

    #[test]
    fn recovery_can_never_redispatch_a_possibly_billed_job() {
        let facts = GuardFacts::default();
        for cause in [
            TransitionCause::recovery("startup"),
            TransitionCause::system("retry"),
        ] {
            assert!(matches!(
                check_transition(S::CloudOutcomeUnknown, S::CloudReady, &cause, facts),
                Err(JobTransitionError::RequiresUserAction { .. })
            ));
        }
        assert_eq!(
            check_transition(
                S::CloudOutcomeUnknown,
                S::CloudReady,
                &TransitionCause::user("retry"),
                facts
            ),
            Err(JobTransitionError::RequiresCostAcknowledgement)
        );
        assert!(check_transition(
            S::CloudOutcomeUnknown,
            S::CloudReady,
            &TransitionCause::user("retry").with_cost_acknowledged(),
            facts
        )
        .is_ok());
    }

    #[test]
    fn dispatch_requires_verified_consent() {
        let facts = GuardFacts::default();
        assert_eq!(
            check_transition(
                S::CloudReady,
                S::CloudSending,
                &TransitionCause::system("dispatch"),
                facts
            ),
            Err(JobTransitionError::RequiresVerifiedConsent)
        );
        assert!(check_transition(
            S::CloudReady,
            S::CloudSending,
            &TransitionCause::system("dispatch").with_consent_verified(),
            facts
        )
        .is_ok());
    }

    #[test]
    fn illegal_edges_are_rejected_exhaustively() {
        let facts = GuardFacts {
            has_committed_output: true,
        };
        let cause = TransitionCause::user("x")
            .with_consent_verified()
            .with_cost_acknowledged();
        for from in JobState::ALL.iter().copied() {
            for to in JobState::ALL.iter().copied() {
                let result = check_transition(from, to, &cause, facts);
                if from.is_terminal() {
                    assert!(matches!(
                        result,
                        Err(JobTransitionError::AlreadyTerminal { .. })
                    ));
                } else if is_legal_edge(from, to) {
                    assert!(result.is_ok(), "{from} -> {to} should be legal");
                } else {
                    assert!(
                        matches!(result, Err(JobTransitionError::IllegalEdge { .. })),
                        "{from} -> {to} should be illegal"
                    );
                }
            }
        }
    }

    #[test]
    fn spec_14_1_coarse_lifecycle_is_walkable() {
        // QUEUED -> PREFLIGHT -> RUNNING -> COMMITTING -> COMPLETED
        let happy = [
            S::Queued,
            S::Preflight,
            S::RunningLocal,
            S::Verifying,
            S::Committing,
            S::Completed,
        ];
        for pair in happy.windows(2) {
            assert!(
                is_legal_edge(pair[0], pair[1]),
                "{} -> {} missing",
                pair[0],
                pair[1]
            );
        }
    }
}
