//! Use cases and policies composed over the domain model.
//!
//! This crate owns the rules that span persistence and the domain: how an
//! import deduplicates blobs without merging logical meetings, and how a
//! repeated import of the same source item stays idempotent.

pub mod dedup;
pub mod diarization;
pub mod import;
pub mod library;
pub mod models;
pub mod search;
pub mod speakers;
pub mod stt;

pub use dedup::{BlobDecision, DedupPolicy};
pub use diarization::{reconcile, Reconciliation, SegmentAssignment};
pub use import::{ImportOutcome, ImportRequest, ImportService};
pub use library::{LibraryOrder, LibraryPage, LibraryService, RecordingSummary, TranscriptExport};
pub use models::{ModelError, ModelId, ModelInstaller, ModelPack, ModelRegistry};
pub use search::{LibrarySearch, SearchResult};
pub use speakers::{Speaker, SpeakerDisplay, SpeakerProvenance, SpeakerRegistry};
pub use stt::{select_engine, EngineCandidate, EngineDecision, SelectionRequest};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-app-core";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
