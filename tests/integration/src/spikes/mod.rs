//! Feasibility spike harnesses (EPIC A).
//!
//! A spike harness lives here rather than in a product crate because its job
//! is to produce *evidence*: a reproducible measurement, a support matrix or
//! a refusal. The production code those findings justify lives in the crates
//! named by each finding document under `docs/spikes/`.

pub mod audio_decoder;
pub mod whisper_bench;
