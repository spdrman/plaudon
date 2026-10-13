//! Optional validated local diarization plugin contract.
//!
//! Spec 8.7 treats diarization as a stage independent from transcription:
//! `Audio → STT`, optionally `Audio → Diarization`, then reconciliation of
//! speaker turns against transcript time ranges. This crate owns the first two
//! of those: the plugin contract and the gate that decides whether a provider
//! may run at all. Reconciliation lives in `plaudon_app_core::diarization`,
//! and labelling lives in `plaudon_app_core::speakers`.
//!
//! Three invariants are structural:
//!
//! * **A provider cannot see a transcript.** [`contract::DiarizationProvider`]
//!   takes media and returns turns, so "STT results are unchanged whether or
//!   not diarization runs" is true because there is no path by which it could
//!   be false.
//! * **Unvalidated providers fail closed.** [`provider::run_guarded`] is the
//!   only entry point, and it refuses any provider outside
//!   [`provider::VALIDATED_PROVIDERS`].
//! * **A cluster is not an identity.** Output carries opaque
//!   [`contract::ClusterKey`] values. `Speaker 1` is assigned downstream and
//!   is explicitly a cluster label (spec 17.6).
//!
//! Two of the modules exist for the *measurement* rather than for the product
//! path: [`pipeline`] is the runtime-free half of a segmentation-based
//! provider — powerset decoding, constrained clustering, aggregation — and
//! [`report`] is the type of the DER artifact ADR-0400 condition 4 asks for.
//! Neither admits a provider to the allowlist; the real engine behind them
//! lives in `plaudon-ai-speaker`'s off-by-default `onnx` feature.
//!
//! Adding a real runtime — `pyannote`, a Python interpreter, a gated
//! Hugging Face token or a cloud provider — requires the licensing and
//! footprint decision recorded in
//! `docs/adr/ADR-0400-diarization-runtime-and-licensing.md`.

pub mod contract;
pub mod der;
pub mod fake;
pub mod pipeline;
pub mod provider;
pub mod report;

pub use contract::{
    ClusterKey, DiarizationCapabilities, DiarizationError, DiarizationInput, DiarizationProvider,
    DiarizationResult, SpeakerTurn, DIARIZATION_CONTRACT_VERSION,
};
pub use der::{
    diarization_error_rate, diarization_error_rate_by_overlap, DerBreakdown, DerOptions, DerReport,
    ReferenceTurn,
};
pub use fake::{FakeDiarizationProvider, FAKE_PROVIDER_ID};
pub use pipeline::{
    assemble_turns, cluster_tracks, decode_powerset_frame, AssembleOptions, ClusterOptions,
    FrameGrid, LocalTrack, PipelineError, LOCAL_SPEAKERS, POWERSET_CLASSES, POWERSET_SETS,
};
pub use provider::{authorize, is_validated, run_guarded, RunPolicy, VALIDATED_PROVIDERS};
pub use report::{
    open_conditions, ConditionPool, CorpusRecord, DerArtifact, ModelRecord, ParameterRecord,
    PooledDer, PooledFigures, ProviderRecord, ReleaseEvidence, RuntimeRecord, SessionMeasurement,
    DIARIZATION_DER_REPORT_SCHEMA_VERSION,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-diarization";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
