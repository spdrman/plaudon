//! Persisted job orchestration (SPEC v1.3 §14).
//!
//! The job engine owns the durable lifecycle of every unit of work: state
//! transitions, idempotent enqueue, stage checkpoints, resource arbitration,
//! cooperative cancellation, the durable local event log and startup recovery.
//!
//! Design invariants, all enforced by tests in this crate and in
//! `tests/chaos`:
//!
//! * **Nothing observable outside a committed transaction.** Every transition,
//!   the rows it depends on and the events it produces commit together through
//!   one single-writer connection ([`writer::WriteCoordinator`]).
//! * **`Completed` implies durable output.** The transition guard reads the
//!   `job_outputs` rows written inside the same transaction.
//! * **A possibly-billed cloud request is never retried automatically.**
//!   `CloudOutcomeUnknown` is terminal for automatic progress; only an
//!   explicit, cost-acknowledged user action can leave it.
//! * **An interruption parks, it does not break.** OS sleep, logout, keychain
//!   lock or transport loss map to a persisted wait state with a resume path.
//! * **The event log is monotonic and gap-free.** Sequence numbers come from a
//!   transactional counter, so a rollback leaves no hole and dispatches
//!   nothing.
//! * **No network.** This crate opens no socket and spawns no listener.

pub mod cancel;
pub mod engine;
pub mod error;
pub mod events;
pub mod fault;
pub mod history;
pub mod key;
pub mod reconcile;
pub mod scheduler;
pub mod state;
pub mod store;
pub mod ui;
pub mod writer;

pub use cancel::{
    CancellationAck, CancellationOutcome, CancellationRegistry, ConsentGate, InterruptionCause,
    CLOUD_CANCELLATION_NOTICE, CONSENT_REVOKED_NOTICE,
};
pub use engine::{EngineConfig, JobEngine};
pub use error::{retry_class, DispatchRefusal, JobError, RetryClass};
pub use events::{
    EventBuffer, EventDispatcher, EventKind, EventLog, EventPayload, EventRecord, EventSubscriber,
    PendingEvent, SourceChange, SubscriberError,
};
pub use history::{ActivityEntry, JobHistory, JobHistoryPage, JobHistoryQuery, JobSummary};
pub use key::{
    ImportJobKey, JobKey, JobKeyDigest, ProcessingJobKey, ProcessingStage, ReprocessToken,
    SourceKind,
};
pub use reconcile::{
    ArchiveAssetResolver, AssetResolver, ReconcileReport, Reconciler, ReconcilerConfig,
};
pub use scheduler::{
    preflight, Admission, PowerSource, PreflightDecision, ResourceClass, ResourceRequest,
    ResourceScheduler, SchedulerLimits, SystemResources, WaitReason,
};
pub use state::{
    check_transition, is_legal_edge, successors, Actor, Edge, GuardFacts, JobPhase, JobState,
    JobTransitionError, TransitionCause, TRANSITIONS,
};
pub use store::{
    Checkpoint, DispatchState, EnqueueOutcome, EnqueueRequest, InferenceAttempt, JobOutput,
    JobRecord, JobStore, StageAction,
};
pub use ui::{EventSink, SinkError, UiBridge, UiEvent, UI_EVENT_CONTRACT_VERSION};
pub use writer::{Database, ReadPool, WriteCoordinator};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-jobs";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
///
/// Capability and contract payloads are versioned independently of the
/// application release, so consumers can detect an unknown contract instead of
/// assuming an unavailable feature is present.
pub const CONTRACT_VERSION: u32 = 1;
