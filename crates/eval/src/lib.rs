//! Offline AI evaluation: consent-cleared corpus, STT metrics, reports, gates.
//!
//! This crate implements the measurable half of spec 17: the corpus register
//! (17.1), the offline-computable metric set (17.2), the published report
//! (17.2) and the `Supported` versus `Recommended` separation (17.6, 1.6).
//!
//! Four properties hold by construction:
//!
//! * **Consent first.** [`corpus::CorpusManifest::clear`] decides what may be
//!   measured. An item without clearance never reaches a metric, and the typed
//!   reason is published in the report instead of being dropped.
//! * **Offline and deterministic.** [`runner::run`] consumes hypothesis
//!   transcripts another process produced. It starts no engine, reads no
//!   audio and opens no socket, so the same inputs always produce the same
//!   bytes.
//! * **Missing is not zero.** Every metric without a denominator or without a
//!   recorded input serializes as `null`. A hypothesis whose alignment origin
//!   is `unavailable` has no timestamp error, not a perfect one.
//! * **Quality is never implied.** `Supported` is a capability claim;
//!   `Recommended` requires a stored evaluation record on the golden corpus
//!   for that exact engine and model.

pub mod corpus;
pub mod gate;
pub mod hypothesis;
pub mod metrics;
pub mod report;
pub mod runner;
pub mod speaker;

pub use corpus::{ClearanceRejection, ConsentStatus, CorpusError, CorpusManifest, ItemRejection};
pub use gate::{evaluate_claim, Claim, GateDecision, GateError, GateRequest, GoldenCorpus};
pub use hypothesis::{
    Deployment, HypothesisAlignment, HypothesisDocument, HypothesisSet, ReferenceTranscript,
};
pub use report::{EvaluationReport, FrozenThresholds, REPORT_SCHEMA_VERSION};
pub use runner::{run, CloudPolicy, RunContext, RunError};
pub use speaker::{
    evaluate, DetCurve, HarnessPlan, ScriptedEmbeddings, SpeakerMatchReport,
    SPEAKER_MATCH_REPORT_SCHEMA_VERSION,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-eval";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
///
/// Capability and contract payloads are versioned independently of the
/// application release, so consumers can detect an unknown contract instead of
/// assuming an unavailable feature is present.
pub const CONTRACT_VERSION: u32 = 1;
