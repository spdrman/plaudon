//! Provider-neutral AI capability and result contracts.
//!
//! Spec references: §6.5 (principal traits), §6.6 (versioned inter-module
//! contracts), §8.3 (capability model), §8.5 (transcript representation),
//! §8.6/§8.6A (request families and the truthfulness contract).
//!
//! This crate holds no vendor dependency and no I/O. Engine adapters
//! (`plaudon-ai-whisper`, `plaudon-ai-openrouter`, `plaudon-ai-apple`) depend
//! on it; it depends on none of them.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod capabilities;
pub mod hallucination;
pub mod job;
pub mod stt;
pub mod transcript;
pub mod tri;
pub mod truthfulness;

pub use capabilities::{
    Accelerator, AvailabilityReason, HardwareRequirements, MediaFormat, PricingBasis, PrivacyClass,
    RequestFamily, SttCapabilities, CAPABILITY_PAYLOAD_VERSION,
};
pub use hallucination::{detect_hallucinations, HallucinationPolicy, SpeechActivity};
pub use job::{
    Cancelled, CancellationToken, JobContext, NullProgressSink, Progress, ProgressSink,
    RecordingProgressSink,
};
pub use stt::{SpeechToText, SttError, SttInput};
pub use transcript::{
    AlignmentOrigin, AnnotationKind, ChunkingStrategy, Confidence, NormalizationProfile,
    SourceTime, TimePrecision, TranscriptAnnotation, TranscriptDraft, TranscriptSegmentDraft,
    WordAlignment,
};
pub use tri::{Known, LimitCheck, Tri};
pub use truthfulness::{
    assert_no_claim_inflation, validate_draft, ClaimInflation, TruthfulnessViolation,
};
