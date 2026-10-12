//! Hermetic, offline evaluation harness for the meeting extraction pipeline (EPIC I task I2.5).
//!
//! This crate provides:
//! - Corpus loaders for `tests/fixtures/ai/corpus/*.json`
//! - A reference `TextModel` that answers from corpus reference data
//! - Mutation profiles for testing precision/recall
//! - §17.3 metrics computation (precision, recall, F1, anchor correctness, etc.)
//! - JSON report generation and threshold-based gating
//!
//! No network, no real provider, fully deterministic.

pub mod corpus;
pub mod gate;
pub mod metrics;
pub mod model;
pub mod mutation;
pub mod report;

pub use corpus::{load_corpus_file, CorpusFixture, CorpusLoadError};
pub use gate::{apply_gate, GateResult, Thresholds};
pub use metrics::{compute_metrics, EvalMetrics, ItemMatch};
pub use model::ReferenceModel;
pub use mutation::{MutationProfile, MutationSeed};
pub use report::{generate_report, EvalReport, ReportError};
