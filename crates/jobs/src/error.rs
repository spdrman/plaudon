//! Typed job-engine errors.
//!
//! Per SPEC v1.3 §7.6/§15.4 there is exactly one user-facing error taxonomy:
//! `plaudon_domain::error::{AppError, AppErrorCode, ErrorEnvelope}`. This module
//! defines the engine's internal errors and maps every one of them onto that
//! taxonomy; it never defines a second envelope and never leaks SQL text,
//! secrets or vendor payloads.

use plaudon_domain::error::{AppError, AppErrorCode, ErrorAction};
use plaudon_domain::ids::JobId;

use crate::state::{JobState, JobTransitionError};

/// Errors raised by the job engine.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Transition(#[from] JobTransitionError),

    #[error("job {0} not found")]
    JobNotFound(JobId),

    #[error("checkpoint for stage `{stage}` of job {job} not found")]
    CheckpointNotFound { job: JobId, stage: String },

    #[error("stage `{stage}` was already committed; committed checkpoints are immutable")]
    CheckpointAlreadyCommitted { stage: String },

    #[error("stage `{stage}` has no prepared intent; prepare before committing")]
    CheckpointNotPrepared { stage: String },

    #[error("digest mismatch for {what}: recorded {expected}, actual {actual}")]
    DigestMismatch {
        what: String,
        expected: String,
        actual: String,
    },

    #[error("required asset is missing: {0}")]
    MissingAsset(String),

    #[error("resource class `{class}` is not configured")]
    UnknownResourceClass { class: String },

    #[error("insufficient {resource}: need {needed} bytes, {available} available")]
    InsufficientResources {
        resource: &'static str,
        needed: u64,
        available: u64,
    },

    #[error("job {0} was cancelled")]
    Cancelled(JobId),

    #[error("cloud dispatch refused: {0}")]
    DispatchRefused(DispatchRefusal),

    #[error("the single-writer coordinator has shut down")]
    WriterClosed,

    #[error("storage failure during {context}")]
    Storage {
        context: &'static str,
        #[source]
        source: sqlx::Error,
    },

    #[error("could not encode {context} as JSON")]
    Encoding {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("filesystem failure during {context}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("corrupt persisted job record: {0}")]
    CorruptRecord(String),
}

/// Why a cloud dispatch was refused before any bytes left the machine.
/// Every variant is fail-closed (§16.3, §10.5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchRefusal {
    #[error("no consent grant covers this destination")]
    NoConsentGrant,
    #[error("the consent grant was revoked")]
    ConsentRevoked,
    #[error("the consent grant has expired")]
    ConsentExpired,
    #[error("the destination changed since consent was granted")]
    DestinationChanged,
    #[error("the resolved privacy posture does not satisfy the user's requirement")]
    PrivacyRequirementUnmet,
    #[error("the outcome of a previous dispatch is unknown; explicit user retry required")]
    OutcomeUnknown,
}

impl JobError {
    /// Convenience constructor for storage failures.
    pub fn storage(context: &'static str, source: sqlx::Error) -> Self {
        JobError::Storage { context, source }
    }

    /// Convenience constructor for filesystem failures.
    pub fn io(context: &'static str, source: std::io::Error) -> Self {
        JobError::Io { context, source }
    }

    /// Convenience constructor for JSON failures.
    pub fn encoding(context: &'static str, source: serde_json::Error) -> Self {
        JobError::Encoding { context, source }
    }

    /// `true` when re-running the same operation unchanged may succeed.
    /// Dispatch refusals and illegal transitions are never retryable; resource
    /// shortage and storage contention are.
    pub fn is_retryable(&self) -> bool {
        match self {
            JobError::Transition(_)
            | JobError::JobNotFound(_)
            | JobError::CheckpointAlreadyCommitted { .. }
            | JobError::CheckpointNotPrepared { .. }
            | JobError::DispatchRefused(_)
            | JobError::Cancelled(_)
            | JobError::CorruptRecord(_)
            | JobError::UnknownResourceClass { .. }
            | JobError::Encoding { .. } => false,
            JobError::CheckpointNotFound { .. }
            | JobError::DigestMismatch { .. }
            | JobError::MissingAsset(_)
            | JobError::InsufficientResources { .. }
            | JobError::WriterClosed
            | JobError::Storage { .. }
            | JobError::Io { .. } => true,
        }
    }

    /// Stable code from the shared closed set (§15.4).
    pub fn code(&self) -> AppErrorCode {
        match self {
            JobError::Transition(_) => AppErrorCode::InvalidState,
            JobError::JobNotFound(_) => AppErrorCode::NotFound,
            JobError::CheckpointNotFound { .. } => AppErrorCode::NotFound,
            JobError::CheckpointAlreadyCommitted { .. }
            | JobError::CheckpointNotPrepared { .. } => AppErrorCode::InvalidState,
            JobError::DigestMismatch { .. } => AppErrorCode::IntegrityFailed,
            JobError::MissingAsset(_) => AppErrorCode::NotFound,
            JobError::UnknownResourceClass { .. } => AppErrorCode::InvalidState,
            JobError::InsufficientResources { .. } => AppErrorCode::ResourceUnavailable,
            JobError::Cancelled(_) => AppErrorCode::Cancelled,
            JobError::DispatchRefused(_) => AppErrorCode::ConsentRequired,
            JobError::WriterClosed | JobError::Storage { .. } => AppErrorCode::StorageFailure,
            JobError::Io { .. } => AppErrorCode::StorageFailure,
            JobError::CorruptRecord(_) => AppErrorCode::IntegrityFailed,
        }
    }

    /// The corrective action a user can take.
    pub fn action(&self) -> ErrorAction {
        match self {
            JobError::DispatchRefused(_) => ErrorAction::GrantConsent,
            JobError::InsufficientResources { .. } => ErrorAction::FreeResources,
            JobError::MissingAsset(_) => ErrorAction::InstallModel,
            JobError::Cancelled(_) => ErrorAction::None,
            other if other.is_retryable() => ErrorAction::Retry,
            _ => ErrorAction::ContactSupport,
        }
    }
}

impl From<JobError> for AppError {
    /// Maps engine errors onto the shared taxonomy. The message is the
    /// `Display` form of the engine error, which by construction contains no
    /// secret, no signed URL and no raw vendor payload.
    fn from(error: JobError) -> AppError {
        AppError::new(error.code(), error.to_string())
            .with_retryable(error.is_retryable())
            .with_action(error.action())
    }
}

/// Deterministic retry classification used by the reconciler and chaos
/// assertions (§14.6 "predictable retry behavior").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    /// The engine may retry automatically when resources allow.
    AutomaticRetry,
    /// Only a deliberate user action may retry; may already have been billed.
    UserRetryWithAcknowledgement,
    /// Parked until an external condition clears; resumes automatically.
    ResumeWhenAvailable,
    /// Never retried.
    Terminal,
}

/// Classify a persisted state into its retry behaviour. Total and pure, so the
/// chaos suite can assert the classification for every reachable state.
pub fn retry_class(state: JobState) -> RetryClass {
    match state {
        JobState::Queued
        | JobState::Preflight
        | JobState::RunningLocal
        | JobState::CloudReady
        | JobState::CloudSending
        | JobState::Verifying
        | JobState::Committing
        | JobState::Cancelling => RetryClass::AutomaticRetry,
        JobState::FailedRetryable => RetryClass::AutomaticRetry,
        JobState::WaitingForNetwork | JobState::WaitingForModel | JobState::WaitingForSource => {
            RetryClass::ResumeWhenAvailable
        }
        JobState::WaitingForUser | JobState::WaitingForAuthorization => {
            RetryClass::UserRetryWithAcknowledgement
        }
        JobState::CloudOutcomeUnknown => RetryClass::UserRetryWithAcknowledgement,
        JobState::Completed | JobState::FailedTerminal | JobState::Cancelled => {
            RetryClass::Terminal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_a_deterministic_retry_class() {
        for state in JobState::ALL {
            let class = retry_class(*state);
            if state.is_terminal() {
                assert_eq!(class, RetryClass::Terminal, "{state}");
            } else {
                assert_ne!(class, RetryClass::Terminal, "{state}");
            }
        }
    }

    #[test]
    fn possibly_billed_state_never_classifies_as_automatic() {
        assert_eq!(
            retry_class(JobState::CloudOutcomeUnknown),
            RetryClass::UserRetryWithAcknowledgement
        );
    }

    #[test]
    fn dispatch_refusals_are_never_retryable() {
        for refusal in [
            DispatchRefusal::NoConsentGrant,
            DispatchRefusal::ConsentRevoked,
            DispatchRefusal::ConsentExpired,
            DispatchRefusal::DestinationChanged,
            DispatchRefusal::PrivacyRequirementUnmet,
            DispatchRefusal::OutcomeUnknown,
        ] {
            let err = JobError::DispatchRefused(refusal);
            assert!(!err.is_retryable());
            assert_eq!(err.code(), AppErrorCode::ConsentRequired);
        }
    }
}
