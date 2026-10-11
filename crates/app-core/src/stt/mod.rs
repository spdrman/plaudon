//! Speech-to-text engine selection policy (spec §8.3, §8.4 — task F1.4).

pub mod selection;

pub use selection::{
    select_engine, CandidateOutcome, EngineCandidate, EngineDecision, Platform, ProbeOutcome,
    SelectionRejection, SelectionRequest,
};
