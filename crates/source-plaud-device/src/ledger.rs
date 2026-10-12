//! The per-transfer ledger state machine (spec App. B.6, §7.9, §13.2).
//!
//! App. B.6 defines the transfer lifecycle:
//!
//! ```text
//! DISCOVERED → QUEUED → CONNECTING → AUTHENTICATING → REQUESTING →
//! RECEIVING → VERIFYING → COMMITTING → IMPORTED
//!       ↘ INTERRUPTED / WAITING_FOR_DEVICE / PAUSED / CANCELLED
//!       ↘ UNSUPPORTED (terminal)
//! ```
//!
//! Two invariants are enforced structurally rather than by convention:
//!
//! * `IMPORTED` is reachable only through `VERIFYING → COMMITTING`, so bytes
//!   that failed verification can never become a playable archive record
//!   (spec §7.9).
//! * `CANCELLED` and `UNSUPPORTED` are terminal. A cancelled transfer cannot
//!   be resurrected by a late device event; the caller must queue a new one.

use plaudon_recording_sources::device::DeviceError;
use serde::{Deserialize, Serialize};

/// State of one device transfer (spec App. B.6, persisted in `device_transfers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// The recording was seen in a listing.
    Discovered,
    /// The user selected it and it is waiting for the device actor.
    Queued,
    /// Establishing the link.
    Connecting,
    /// Presenting the device credential.
    Authenticating,
    /// The read request has been sent.
    Requesting,
    /// Bytes are arriving.
    Receiving,
    /// The stream ended; completeness is being checked.
    Verifying,
    /// Verified bytes are being committed durably.
    Committing,
    /// Committed to the library.
    Imported,
    /// The link dropped mid-transfer; the staged bytes are retained for a
    /// verified resume or a clean restart.
    Interrupted,
    /// The device is out of range or busy; we are waiting for it.
    WaitingForDevice,
    /// The user paused the transfer.
    Paused,
    /// The user cancelled the transfer. Terminal.
    Cancelled,
    /// The profile, codec or auth mode is not supported. Terminal.
    Unsupported,
}

impl TransferState {
    /// Every state, for exhaustive tests.
    pub const ALL: &'static [Self] = &[
        Self::Discovered,
        Self::Queued,
        Self::Connecting,
        Self::Authenticating,
        Self::Requesting,
        Self::Receiving,
        Self::Verifying,
        Self::Committing,
        Self::Imported,
        Self::Interrupted,
        Self::WaitingForDevice,
        Self::Paused,
        Self::Cancelled,
        Self::Unsupported,
    ];

    /// Whether this state can never change again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Imported | Self::Cancelled | Self::Unsupported)
    }

    /// Whether a committed, playable archive record exists for this transfer.
    #[must_use]
    pub const fn has_committed_media(self) -> bool {
        matches!(self, Self::Imported)
    }

    /// Whether staged bytes may exist on disk in this state.
    ///
    /// The startup reconciler uses this to decide which staging files are
    /// candidates for cleanup after the grace period (spec §13.3A).
    #[must_use]
    pub const fn may_hold_staging_bytes(self) -> bool {
        matches!(
            self,
            Self::Receiving
                | Self::Verifying
                | Self::Committing
                | Self::Interrupted
                | Self::Paused
                | Self::WaitingForDevice
        )
    }

    /// The label the interface shows for this state (spec §4.2A.8, App. B.8).
    ///
    /// `On device`, `Transferred locally` and `Archived` are distinct states;
    /// nothing here says "downloaded" or "synced", which would blur them.
    #[must_use]
    pub const fn ui_label(self) -> &'static str {
        match self {
            Self::Discovered => "On device",
            Self::Queued => "Queued",
            Self::Connecting => "Connecting",
            Self::Authenticating => "Authenticating",
            Self::Requesting => "Requesting",
            Self::Receiving => "Transferring",
            Self::Verifying => "Verifying",
            Self::Committing => "Transferred locally",
            Self::Imported => "Archived",
            Self::Interrupted => "Interrupted",
            Self::WaitingForDevice => "Waiting for device",
            Self::Paused => "Paused",
            Self::Cancelled => "Cancelled",
            Self::Unsupported => "Unsupported",
        }
    }
}

/// Something that happened to a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TransferEvent {
    /// The user queued the recording.
    Queued,
    /// The actor started connecting.
    ConnectStarted,
    /// The link is up; authentication begins.
    LinkEstablished,
    /// The device accepted our credential.
    Authenticated,
    /// The read request was written.
    RequestSent,
    /// The first bytes arrived.
    BytesArriving,
    /// The terminal marker arrived.
    StreamEnded,
    /// Completeness checks passed.
    VerificationSucceeded,
    /// Completeness checks failed.
    VerificationFailed,
    /// The durable commit finished.
    CommitSucceeded,
    /// The durable commit failed.
    CommitFailed,
    /// The link dropped.
    Disconnected,
    /// The device reported it is busy or went out of range.
    DeviceUnavailable,
    /// The device became reachable again.
    DeviceReturned,
    /// The user paused.
    Paused,
    /// The user resumed.
    Resumed,
    /// The user cancelled.
    Cancelled,
    /// The profile, codec or auth mode turned out to be unsupported.
    Unsupported,
}

/// Why a transition was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a transfer in state {state:?} cannot accept this event")]
pub struct IllegalTransition {
    /// State the ledger was in.
    pub state: TransferState,
}

/// One transfer's state, with its audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferLedger {
    state: TransferState,
    history: Vec<TransferState>,
    verified: bool,
}

impl Default for TransferLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferLedger {
    /// A ledger for a recording that has only been listed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: TransferState::Discovered,
            history: vec![TransferState::Discovered],
            verified: false,
        }
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> TransferState {
        self.state
    }

    /// Every state this transfer has been in, in order.
    #[must_use]
    pub fn history(&self) -> &[TransferState] {
        &self.history
    }

    /// Whether verification has succeeded for this transfer.
    #[must_use]
    pub const fn verified(&self) -> bool {
        self.verified
    }

    /// Apply an event.
    ///
    /// Illegal transitions are refused rather than ignored, so a caller
    /// cannot skip verification by sending `CommitSucceeded` early.
    pub fn apply(&mut self, event: &TransferEvent) -> Result<TransferState, IllegalTransition> {
        use TransferEvent as E;
        use TransferState as S;

        if self.state.is_terminal() {
            return Err(IllegalTransition { state: self.state });
        }

        let next = match (self.state, event) {
            // Universal exits, available from any non-terminal state.
            (_, E::Cancelled) => S::Cancelled,
            (_, E::Unsupported) => S::Unsupported,
            (_, E::Disconnected) => S::Interrupted,
            (_, E::DeviceUnavailable) => S::WaitingForDevice,
            (_, E::Paused) => S::Paused,

            (S::Discovered, E::Queued) => S::Queued,
            (S::Queued | S::Interrupted | S::WaitingForDevice | S::Paused, E::ConnectStarted) => {
                S::Connecting
            }
            (S::Paused, E::Resumed) | (S::WaitingForDevice, E::DeviceReturned) => S::Queued,
            (S::Connecting, E::LinkEstablished) => S::Authenticating,
            (S::Authenticating, E::Authenticated) => S::Requesting,
            (S::Requesting, E::RequestSent) => S::Requesting,
            (S::Requesting, E::BytesArriving) => S::Receiving,
            (S::Receiving, E::BytesArriving) => S::Receiving,
            (S::Receiving, E::StreamEnded) => S::Verifying,
            (S::Verifying, E::VerificationSucceeded) => {
                self.verified = true;
                S::Committing
            }
            (S::Verifying, E::VerificationFailed) => S::Interrupted,
            (S::Committing, E::CommitSucceeded) => {
                // Unreachable without `verified`, because `Committing` is
                // reachable only through `VerificationSucceeded`; asserted by
                // a property test rather than trusted.
                if !self.verified {
                    return Err(IllegalTransition { state: self.state });
                }
                S::Imported
            }
            (S::Committing, E::CommitFailed) => S::Interrupted,

            _ => return Err(IllegalTransition { state: self.state }),
        };

        if next != self.state {
            self.history.push(next);
        }
        self.state = next;
        Ok(next)
    }

    /// The device error this state reports, if any.
    #[must_use]
    pub const fn as_device_error(&self) -> Option<DeviceError> {
        match self.state {
            TransferState::Interrupted => Some(DeviceError::TransferIncomplete),
            TransferState::Cancelled => Some(DeviceError::Cancelled),
            TransferState::Unsupported => Some(DeviceError::UnsupportedCodec),
            TransferState::WaitingForDevice => Some(DeviceError::DeviceDisconnected),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn drive(events: &[TransferEvent]) -> TransferLedger {
        let mut ledger = TransferLedger::new();
        for event in events {
            let _ = ledger.apply(event);
        }
        ledger
    }

    const HAPPY_PATH: &[TransferEvent] = &[
        TransferEvent::Queued,
        TransferEvent::ConnectStarted,
        TransferEvent::LinkEstablished,
        TransferEvent::Authenticated,
        TransferEvent::RequestSent,
        TransferEvent::BytesArriving,
        TransferEvent::StreamEnded,
        TransferEvent::VerificationSucceeded,
        TransferEvent::CommitSucceeded,
    ];

    #[test]
    fn the_documented_happy_path_reaches_imported() {
        let ledger = drive(HAPPY_PATH);
        assert_eq!(ledger.state(), TransferState::Imported);
        assert!(ledger.verified());
        assert!(ledger.state().has_committed_media());
        assert_eq!(ledger.state().ui_label(), "Archived");
        assert_eq!(
            ledger.history(),
            &[
                TransferState::Discovered,
                TransferState::Queued,
                TransferState::Connecting,
                TransferState::Authenticating,
                TransferState::Requesting,
                TransferState::Receiving,
                TransferState::Verifying,
                TransferState::Committing,
                TransferState::Imported,
            ]
        );
    }

    #[test]
    fn a_commit_cannot_be_reached_without_verification() {
        let mut ledger = drive(&HAPPY_PATH[..6]);
        assert_eq!(ledger.state(), TransferState::Receiving);
        assert!(ledger.apply(&TransferEvent::CommitSucceeded).is_err());
        assert_eq!(ledger.state(), TransferState::Receiving);
        assert!(!ledger.state().has_committed_media());
    }

    #[test]
    fn a_failed_verification_never_becomes_an_import() {
        let mut ledger = drive(&HAPPY_PATH[..7]);
        assert_eq!(ledger.state(), TransferState::Verifying);
        ledger.apply(&TransferEvent::VerificationFailed).unwrap();
        assert_eq!(ledger.state(), TransferState::Interrupted);
        assert!(!ledger.verified());
        assert_eq!(
            ledger.as_device_error(),
            Some(DeviceError::TransferIncomplete)
        );
        assert!(ledger.apply(&TransferEvent::CommitSucceeded).is_err());
    }

    #[test]
    fn a_disconnect_at_any_stage_leaves_an_interrupted_transfer() {
        for prefix in 1..HAPPY_PATH.len() {
            let mut ledger = drive(&HAPPY_PATH[..prefix]);
            if ledger.state().is_terminal() {
                continue;
            }
            ledger.apply(&TransferEvent::Disconnected).unwrap();
            assert_eq!(
                ledger.state(),
                TransferState::Interrupted,
                "disconnect after {prefix} events"
            );
            assert!(!ledger.state().has_committed_media());
        }
    }

    #[test]
    fn cancellation_is_terminal_and_a_late_device_event_cannot_undo_it() {
        let mut ledger = drive(&HAPPY_PATH[..5]);
        ledger.apply(&TransferEvent::Cancelled).unwrap();
        assert_eq!(ledger.state(), TransferState::Cancelled);
        for event in [
            TransferEvent::BytesArriving,
            TransferEvent::StreamEnded,
            TransferEvent::VerificationSucceeded,
            TransferEvent::CommitSucceeded,
            TransferEvent::DeviceReturned,
        ] {
            assert!(ledger.apply(&event).is_err(), "{event:?} after cancel");
            assert_eq!(ledger.state(), TransferState::Cancelled);
        }
    }

    #[test]
    fn an_unsupported_transfer_is_terminal() {
        let mut ledger = drive(&[TransferEvent::Queued, TransferEvent::Unsupported]);
        assert_eq!(ledger.state(), TransferState::Unsupported);
        assert!(ledger.apply(&TransferEvent::ConnectStarted).is_err());
    }

    #[test]
    fn a_pause_and_resume_returns_to_the_queue_rather_than_mid_stream() {
        let mut ledger = drive(&HAPPY_PATH[..6]);
        ledger.apply(&TransferEvent::Paused).unwrap();
        assert_eq!(ledger.state(), TransferState::Paused);
        assert_eq!(ledger.apply(&TransferEvent::Resumed).unwrap(), TransferState::Queued);
        assert!(
            !ledger.verified(),
            "a resumed transfer re-verifies from scratch"
        );
    }

    #[test]
    fn an_out_of_range_device_waits_rather_than_failing() {
        let mut ledger = drive(&HAPPY_PATH[..6]);
        ledger.apply(&TransferEvent::DeviceUnavailable).unwrap();
        assert_eq!(ledger.state(), TransferState::WaitingForDevice);
        assert!(ledger.state().may_hold_staging_bytes());
        assert_eq!(
            ledger.apply(&TransferEvent::DeviceReturned).unwrap(),
            TransferState::Queued
        );
    }

    #[test]
    fn every_state_has_a_distinct_label() {
        let mut labels: Vec<&str> = TransferState::ALL.iter().map(|s| s.ui_label()).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count, "two states share a label");

        // §4.2A.8: the three library-visible stages are distinct.
        assert_eq!(TransferState::Discovered.ui_label(), "On device");
        assert_eq!(TransferState::Committing.ui_label(), "Transferred locally");
        assert_eq!(TransferState::Imported.ui_label(), "Archived");
    }

    fn any_event() -> impl Strategy<Value = TransferEvent> {
        prop_oneof![
            Just(TransferEvent::Queued),
            Just(TransferEvent::ConnectStarted),
            Just(TransferEvent::LinkEstablished),
            Just(TransferEvent::Authenticated),
            Just(TransferEvent::RequestSent),
            Just(TransferEvent::BytesArriving),
            Just(TransferEvent::StreamEnded),
            Just(TransferEvent::VerificationSucceeded),
            Just(TransferEvent::VerificationFailed),
            Just(TransferEvent::CommitSucceeded),
            Just(TransferEvent::CommitFailed),
            Just(TransferEvent::Disconnected),
            Just(TransferEvent::DeviceUnavailable),
            Just(TransferEvent::DeviceReturned),
            Just(TransferEvent::Paused),
            Just(TransferEvent::Resumed),
            Just(TransferEvent::Cancelled),
            Just(TransferEvent::Unsupported),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 1024, ..ProptestConfig::default() })]

        /// No event sequence reaches `Imported` without a successful
        /// verification immediately preceding the commit.
        #[test]
        fn imported_is_unreachable_without_verification(
            events in proptest::collection::vec(any_event(), 0..24),
        ) {
            let ledger = drive(&events);
            if ledger.state() == TransferState::Imported {
                prop_assert!(ledger.verified());
                let history = ledger.history();
                let at = history.len() - 1;
                prop_assert_eq!(history[at - 1], TransferState::Committing);
                prop_assert_eq!(history[at - 2], TransferState::Verifying);
            }
        }

        /// A cancelled or unsupported transfer never moves again.
        #[test]
        fn terminal_states_absorb_everything(
            prefix in proptest::collection::vec(any_event(), 0..12),
            suffix in proptest::collection::vec(any_event(), 1..12),
        ) {
            let mut ledger = drive(&prefix);
            if !ledger.state().is_terminal() {
                return Ok(());
            }
            let terminal = ledger.state();
            for event in &suffix {
                prop_assert!(ledger.apply(event).is_err());
                prop_assert_eq!(ledger.state(), terminal);
            }
        }
    }
}
