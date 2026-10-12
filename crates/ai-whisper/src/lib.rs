//! whisper.cpp speech-to-text adapter (spec §8.4, §12, §14.4 — EPIC F phase 2).
//!
//! The crate is organised around the three things the spec demands of a local
//! engine:
//!
//! * [`engine`] — the `SpeechToText` implementation: honest capabilities,
//!   prompt cancellation, recorded parameters, no fallback to another engine.
//! * [`backend`] / [`preflight`] / [`arbiter`] — runtime GPU selection with a
//!   CPU baseline, resource checks that refuse instead of OOM, and one active
//!   local workload by default.
//! * [`model`] — cheap inspection of a ggml weight file, so availability and
//!   language support are observed rather than inferred.

pub mod arbiter;
pub mod backend;
pub mod engine;
pub mod model;
pub mod preflight;

pub use arbiter::{LocalWorkloadArbiter, WorkloadBusy, WorkloadLease};
pub use backend::{
    fall_back_to_cpu, select_backend, Backend, BackendAvailability, BackendPreference,
    BackendProbe, BackendSelection, BackendUnavailable, RuntimeBackendProbe,
};
pub use engine::{
    WhisperConfig, WhisperEngine, WhisperParams, MAX_RUN_BYTES, MAX_RUN_DURATION_MS,
    WHISPER_ENGINE_ID, WHISPER_SAMPLE_RATE_HZ,
};
pub use model::{read_model_header, ModelProbeError, WhisperModelHeader};
pub use preflight::{
    preflight, CapacityProbe, HostCapacity, HostCapacityProbe, PreflightError, PreflightReport,
    WorkloadEstimate,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-whisper";

/// Version of the inter-module contract exposed by this crate (spec §6.6).
pub const CONTRACT_VERSION: u32 = 1;
