//! Versioned grounded question-answering and learning artifact tools.
//!
//! This crate implements spec 9.7 (extensible transcript tools and
//! per-recording conversations), 9.8 (ask-about-recording) and the study
//! artifacts of 9.3. Four rules are enforced by the types rather than by
//! convention:
//!
//! * **A tool definition is immutable.** Editing a prompt or schema appends a
//!   version ([`registry::ToolRegistry::apply_override`]), so a past
//!   [`run::ToolRunRecord`] always resolves to the definition that produced it.
//! * **A conversation has provenance.** A
//!   [`conversation::RecordingConversation`] cannot exist without a recording
//!   and an exact transcript revision, and an `Answered` turn cannot exist
//!   without a verified anchor.
//! * **Transcript content is data.** [`prompt`] fences it, strips fence
//!   sentinels from it and states the rule to the model; no code path
//!   interpolates transcript text into an instruction.
//! * **Unknown cost is not zero.** [`run::aggregate_cost`] returns
//!   [`plaudon_domain::money::MoneyEstimate::Unknown`] for any total that
//!   contains an unknown contribution.
//!
//! The crate has no database, no HTTP client and no filesystem dependency
//! beyond reading user tool overrides, so it is usable from tests and from the
//! automation CLI without starting the application.

pub mod anchor;
pub mod ask;
pub mod conversation;
pub mod error;
pub mod metrics;
pub mod prompt;
pub mod registry;
pub mod render;
pub mod retrieval;
pub mod run;
pub mod study;
pub mod testing;
pub mod tool;
pub mod validate;

pub use anchor::{verify_anchor, AnchorRejection, AnchorVerification, SourceAnchor};
pub use ask::{ask_recording, AskAnswer, AskExecution, AskOutcomeKind, AskRequest};
pub use conversation::{BasisState, ConversationTurn, RecordingConversation, TurnRole};
pub use error::ToolError;
pub use registry::{ToolConfigOverride, ToolRegistry};
pub use render::{render_study_json, render_study_markdown, render_study_text};
pub use retrieval::{Retriever, ScoredPassage};
pub use run::{aggregate_cost, ConsentPolicySnapshot, ToolRunDraft, ToolRunRecord, ToolRunStatus};
pub use study::{
    study_artifacts_from_value, Definition, Flashcard, QuizItem, StudyArtifacts, StudyValidation,
};
pub use tool::{ToolId, ToolKind, TranscriptToolDefinition};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-tools";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
