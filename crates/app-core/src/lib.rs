//! Use cases and policies composed over the domain model.
//!
//! This crate owns the rules that span persistence and the domain: how an
//! import deduplicates blobs without merging logical meetings, and how a
//! repeated import of the same source item stays idempotent.

pub mod chunking;
pub mod dedup;
pub mod diarization;
pub mod engine_policy;
pub mod import;
pub mod library;
pub mod models;
pub mod pipeline;
pub mod reconciliation;
pub mod search;
pub mod speakers;
pub mod stt;
pub mod templates;
pub mod transcript_tools;
pub mod vendor_artifacts;

pub use chunking::{
    plan_chunks, plan_chunks_for_revision, Chunk, ChunkError, ChunkPlan, ChunkPolicy,
    ContextBudget, ContextOrigin, Coverage, SegmentExtent, UnknownContext,
};
pub use dedup::{BlobDecision, DedupPolicy};
pub use diarization::{reconcile, Reconciliation, SegmentAssignment};
pub use engine_policy::{
    candidate_from_probe, degrade, explain, select_text_model, CorrectiveAction, Degradation,
    TextEngineCandidate, TextEngineDecision, TextRequirements, UnavailabilityExplanation,
};
pub use import::{ImportOutcome, ImportRequest, ImportService};
pub use library::{LibraryOrder, LibraryPage, LibraryService, RecordingSummary, TranscriptExport};
pub use models::{ModelError, ModelId, ModelInstaller, ModelPack, ModelRegistry};
pub use pipeline::{
    PipelineError, PipelineOutcome, PipelineRequest, StageKind, StageProvenance,
    SummarizationPipeline, PIPELINE_VERSION,
};
pub use reconciliation::{
    classify_folder_candidate, ObservedItem, ReconcileAction, ReconcileDecision,
    ReconciliationService, SourceEvent,
};
pub use search::{LibrarySearch, SearchResult};
pub use speakers::{Speaker, SpeakerDisplay, SpeakerProvenance, SpeakerRegistry};
pub use stt::{select_engine, EngineCandidate, EngineDecision, SelectionRequest};
pub use templates::{TemplateLoadError, TemplateRegistry, TemplateSummary};
pub use transcript_tools::{AnsweredQuestion, ConversationService, StudyOutputWriter};
pub use vendor_artifacts::{
    VendorArtifactImporter, VendorArtifactRef, VendorImportCapability, VendorImportMode,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-app-core";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
