//! The cloud request ledger contract (spec 10.7, 14.3).
//!
//! The clients in this crate cannot dispatch without an [`AttemptId`], and the
//! only way to obtain one is [`AttemptLedger::open`], which a caller implements
//! by committing a durable row **before** any user content is sent. The
//! obligation is therefore structural rather than a convention: a code path
//! that forgets the ledger does not compile.
//!
//! The storage implementation lives in `plaudon-app-core`, which owns SQLite.
//! This crate only states what must be recorded and when it may change.

use std::fmt;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_domain::capability::DispatchState;

use crate::pricing::{CostEstimate, SettledCost};

/// Everything written before the first byte of user content leaves (spec 10.7).
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptIntent {
    /// Owning job, as its canonical string form.
    pub job_id: String,
    /// Checksum of the exact content that will be dispatched.
    pub content_sha256: String,
    /// Validated request family.
    pub request_family: RequestFamily,
    /// Relay we connect to.
    pub relay: String,
    /// Model identifier presented to the relay.
    pub model_id: String,
    /// Resolved downstream route label, when one is discoverable.
    pub route_label: Option<String>,
    /// Privacy posture snapshot for this exact route and content type.
    pub privacy_snapshot: String,
    /// Local consent grant that authorized this dispatch.
    pub consent_id: String,
    /// Pre-dispatch estimate, which may be unknown.
    pub estimate: CostEstimate,
}

/// A committed ledger row. Minting one is the proof a dispatch is permitted.
///
/// Deliberately opaque and not constructible outside this module's
/// [`AttemptLedger::open`] implementations, so no caller can fabricate one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttemptId(String);

impl AttemptId {
    /// Wrap an identifier a ledger implementation has just committed.
    ///
    /// Only call this after the row is durably written: the type is the
    /// evidence that it was.
    #[must_use]
    pub fn committed(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AttemptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A state change of one attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum AttemptUpdate {
    /// The request is being transmitted.
    Sending,
    /// The provider accepted the request and we are awaiting a response.
    Accepted {
        /// Vendor request id, when one was returned.
        provider_request_id: Option<String>,
    },
    /// A response arrived and was decoded.
    Completed {
        /// Vendor request id, when one was returned.
        provider_request_id: Option<String>,
        /// Cost, finalized only if the provider reported one.
        cost: SettledCost,
        /// The provider's usage block, verbatim, for auditing.
        usage: Option<serde_json::Value>,
    },
    /// Bytes left the machine and no usable response arrived. Never retried
    /// automatically (spec 10.7).
    OutcomeUnknown {
        /// Vendor request id, when one was returned.
        provider_request_id: Option<String>,
    },
    /// The attempt failed before any byte was transmitted. Safe to retry.
    FailedBeforeSend {
        /// Stable failure label, never a provider message.
        reason: &'static str,
    },
}

impl AttemptUpdate {
    /// The dispatch state this update puts the row in.
    #[must_use]
    pub const fn dispatch_state(&self) -> DispatchState {
        match self {
            AttemptUpdate::Sending => DispatchState::Sending,
            AttemptUpdate::Accepted { .. } => DispatchState::AcceptedByProvider,
            AttemptUpdate::Completed { .. } => DispatchState::Completed,
            AttemptUpdate::OutcomeUnknown { .. } => DispatchState::OutcomeUnknown,
            AttemptUpdate::FailedBeforeSend { .. } => DispatchState::FailedBeforeSend,
        }
    }
}

/// Why the ledger could not record something.
///
/// A ledger failure is always fatal to the dispatch: if the row cannot be
/// written, the request must not be sent (spec 10.7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the cloud request ledger is unavailable")]
pub struct LedgerUnavailable {
    /// Non-secret detail for diagnostics.
    pub detail: String,
}

/// The durable record of every cloud attempt.
#[async_trait]
pub trait AttemptLedger: Send + Sync + fmt::Debug {
    /// Commit the pre-dispatch row and return its identifier.
    ///
    /// Implementations must not return until the row is durable.
    async fn open(&self, intent: &AttemptIntent) -> Result<AttemptId, LedgerUnavailable>;

    /// Record a state change for an open attempt.
    async fn update(&self, id: &AttemptId, update: AttemptUpdate) -> Result<(), LedgerUnavailable>;
}

/// An in-memory ledger for tests and for the opt-in probe harness.
#[derive(Debug, Default)]
pub struct RecordingLedger {
    rows: std::sync::Mutex<Vec<(AttemptId, AttemptIntent)>>,
    updates: std::sync::Mutex<Vec<(AttemptId, AttemptUpdate)>>,
    fail: bool,
}

impl RecordingLedger {
    /// A ledger that accepts writes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A ledger that refuses every write, to prove dispatch is blocked.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }

    /// Committed intents, oldest first.
    #[must_use]
    pub fn intents(&self) -> Vec<AttemptIntent> {
        self.rows
            .lock()
            .expect("ledger lock")
            .iter()
            .map(|(_, intent)| intent.clone())
            .collect()
    }

    /// Recorded updates, oldest first.
    #[must_use]
    pub fn updates(&self) -> Vec<AttemptUpdate> {
        self.updates
            .lock()
            .expect("ledger lock")
            .iter()
            .map(|(_, update)| update.clone())
            .collect()
    }

    /// Dispatch states the attempt passed through.
    #[must_use]
    pub fn states(&self) -> Vec<DispatchState> {
        self.updates()
            .iter()
            .map(AttemptUpdate::dispatch_state)
            .collect()
    }
}

#[async_trait]
impl AttemptLedger for RecordingLedger {
    async fn open(&self, intent: &AttemptIntent) -> Result<AttemptId, LedgerUnavailable> {
        if self.fail {
            return Err(LedgerUnavailable {
                detail: "test ledger is unavailable".to_owned(),
            });
        }
        let mut rows = self.rows.lock().expect("ledger lock");
        let id = AttemptId::committed(format!("attempt-{}", rows.len() + 1));
        rows.push((id.clone(), intent.clone()));
        Ok(id)
    }

    async fn update(&self, id: &AttemptId, update: AttemptUpdate) -> Result<(), LedgerUnavailable> {
        if self.fail {
            return Err(LedgerUnavailable {
                detail: "test ledger is unavailable".to_owned(),
            });
        }
        self.updates
            .lock()
            .expect("ledger lock")
            .push((id.clone(), update));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_update_maps_to_its_dispatch_state() {
        assert_eq!(
            AttemptUpdate::Sending.dispatch_state(),
            DispatchState::Sending
        );
        assert_eq!(
            AttemptUpdate::OutcomeUnknown {
                provider_request_id: None
            }
            .dispatch_state(),
            DispatchState::OutcomeUnknown
        );
        assert!(!AttemptUpdate::OutcomeUnknown {
            provider_request_id: None
        }
        .dispatch_state()
        .auto_retry_safe());
        assert!(AttemptUpdate::FailedBeforeSend { reason: "connect" }
            .dispatch_state()
            .auto_retry_safe());
    }
}
