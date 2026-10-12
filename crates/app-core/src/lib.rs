//! Use cases and policies composed over the domain model.
//!
//! This crate owns the rules that span persistence and the domain: how an
//! import deduplicates blobs without merging logical meetings, and how a
//! repeated import of the same source item stays idempotent.

pub mod chunking;
pub mod dedup;
pub mod import;
pub mod models;
pub mod pipeline;
pub mod stt;
pub mod templates;

pub use chunking::{
    plan_chunks, plan_chunks_for_revision, Chunk, ChunkError, ChunkPlan, ChunkPolicy,
    ContextBudget, ContextOrigin, Coverage, SegmentExtent, UnknownContext,
};
pub use dedup::{BlobDecision, DedupPolicy};
pub use import::{ImportOutcome, ImportRequest, ImportService};
pub use models::{ModelError, ModelId, ModelInstaller, ModelPack, ModelRegistry};
pub use pipeline::{
    PipelineError, PipelineOutcome, PipelineRequest, StageKind, StageProvenance,
    SummarizationPipeline, PIPELINE_VERSION,
};
pub use stt::{select_engine, EngineCandidate, EngineDecision, SelectionRequest};
pub use templates::{TemplateLoadError, TemplateRegistry, TemplateSummary};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-app-core";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
