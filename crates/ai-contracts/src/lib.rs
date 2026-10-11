//! Speech-to-text and text-model capability contracts shared by all AI adapters.
//!
//! The contracts here are deliberately provider-neutral: spec 6.3(1) keeps the
//! core free of any mandatory OpenRouter, Apple or whisper dependency, and
//! spec 6.6 versions capability payloads independently of the application.
//!
//! Two rules are enforced by the types rather than by documentation:
//!
//! * Unknown is not supported. Every optional capability is a
//!   [`plaudon_domain::capability::Tri`] or
//!   [`plaudon_domain::capability::Known`], and policy only accepts `Yes`.
//! * A cancelled run yields a typed cancellation error, never a partial
//!   success.

pub mod capabilities;
pub mod job;
pub mod policy;
pub mod stt;
pub mod text;

pub use capabilities::{
    AvailabilityReason, HardwareRequirements, MediaFormat, PricingBasis, PrivacyClass,
    RequestFamily, SttCapabilities, TextModelCapabilities,
};
pub use job::{CancellationToken, JobContext, Progress, ProgressSink};
pub use plaudon_domain::capability::{CapabilityEnvelope, CapabilityStatus, Known, Tri};
pub use plaudon_domain::transcript::AlignmentOrigin;
pub use policy::{EngineRejection, EngineSelection, SttRequirements, select_stt_engine};
pub use stt::{SpeechToText, SttError, SttInput, TranscriptDraft, TranscriptSegmentDraft};
pub use text::{StructuredRequest, StructuredResponse, TextModel, TextModelError};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-contracts";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
