//! Cross-platform local text generation behind the footprint and licence gate.
//!
//! Spec 3.1 marks cross-platform local LLM summarization P1 "after footprint
//! and model-license gate"; spec 12 explains why: model sizes, context
//! windows, redistribution rights, quantization and low-memory performance all
//! need separate benchmarking before the feature can be honestly offered.
//!
//! This crate ships the feature **and** the gate that keeps it off until the
//! measurement exists:
//!
//! * [`gate`] reads a checked benchmark artifact under `docs/benchmarks/`.
//!   Absent, unreadable, unmeasured for this platform, over budget or
//!   licence-unreviewed all resolve to a typed refusal with a user-facing
//!   explanation. There is no override.
//! * [`artifact`] verifies the weight file against the trusted manifest on
//!   every run, not once at install.
//! * [`context`] sizes work against the verified context length and reports
//!   coverage explicitly, so a long recording is sectioned rather than
//!   truncated (spec 9.4).
//! * [`engine`] implements [`plaudon_ai_contracts::text::TextModel`], so the
//!   rest of the application reaches a local model through the same contract
//!   as a cloud one, and reports `PrivacyClass::OnDevice` with no pricing.
//!
//! The crate never downloads anything. Installation, progress, licence display
//! and deletion belong to the model manager in `plaudon_app_core::models`
//! (spec 12).

pub mod artifact;
pub mod context;
pub mod engine;
pub mod gate;

pub use artifact::{ArtifactError, LocalModelArtifact};
pub use context::{
    ContextBudget, ContextOrigin, Coverage, LocalSectionPlanner, PlanError, Section, SectionPlan,
    SectionPlanner, SegmentExtent,
};
pub use engine::{
    InferenceRequest, LocalInferenceBackend, LocalTextModel, ProcessInferenceBackend,
    LOCAL_LLM_ENGINE_ID,
};
pub use gate::{
    evaluate, evaluate_at, FootprintGateArtifact, FootprintThresholds, GateRefusal, GateVerdict,
    GATE_ARTIFACT_PATH,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-local-llm";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
