//! Device capability registry and read-only transfer orchestration.
//!
//! The registry is the machine-readable claim register of spec 2.5B: a
//! committed `compatibility.yaml` whose rows are the *only* source of the
//! compatibility strings the interface shows. Promotional copy and hardcoded
//! labels are forbidden, and an unmatched profile tuple fails closed.
//!
//! Around that registry sit the pieces of a read-only acquisition:
//!
//! | Module | Responsibility | Spec |
//! |---|---|---|
//! | [`register`] | the committed `compatibility.yaml` claim register | §2.5B, App. B.2 |
//! | [`gating`] | fail-closed profile selection and the one-shot experimental probe | §7.0, §7.9 |
//! | [`credentials`] | device-scoped vault credentials and provisioning boundaries | §16.4, App. B.5 |
//! | [`ledger`] | the per-transfer state machine | App. B.6 |
//! | [`transfer`] | streaming acquisition, verification and the derived conversion | §7.7, §7.9 |
//! | [`audit`] | the non-destructive audit that gates automatic sync | §7.9, §17.3A |
//! | [`evidence`] | the hardware evidence record a `Supported` cell requires | §7.9, App. B.9 |

pub mod audit;
pub mod credentials;
pub mod evidence;
pub mod gating;
pub mod ledger;
pub mod register;
pub mod transfer;

pub use audit::{AuditObservation, AuditOutcome, NonDestructiveAudit, SyncEligibility};
pub use credentials::{
    CredentialError, DeviceBinding, DeviceCredentials, IdentityAssurance, ProvisioningRoute,
    RouteDisposition, LEGACY_DISCLOSURES,
};
pub use evidence::{EvidenceRecord, EvidenceRecordError, TransferEvidence};
pub use gating::{
    ExperimentalProbeGrant, ExperimentalProbeSession, GateDecision, GatePolicy, GateRefusal,
    ProbedProfile,
};
pub use ledger::{TransferEvent, TransferLedger, TransferState};
pub use register::{
    CompatibilityCell, CompatibilityRegister, EvidenceLevel, ProfileKey, RegisterError, Resolution,
    SupportStatus,
};
pub use transfer::{
    derive_playable, plan_resume, AcquisitionReport, DerivativeKind, DerivativeOutcome,
    DerivativeRefusal, RestartReason, ResumePlan, StagedTransfer, StagingAcquisition,
    TransferCheckpoint, TransferError, TransferVerdict, TransferVerification, UnverifiedReason,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-source-plaud-device";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
