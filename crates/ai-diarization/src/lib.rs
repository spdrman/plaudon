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
//! Adding a real runtime — `pyannote`, a Python interpreter, a gated
//! Hugging Face token or a cloud provider — requires the licensing and
//! footprint decision recorded in
//! `docs/adr/ADR-0400-diarization-runtime-and-licensing.md`.

pub mod contract;
pub mod der;
pub mod fake;
pub mod provider;

pub use contract::{
    ClusterKey, DiarizationCapabilities, DiarizationError, DiarizationInput, DiarizationProvider,
    DiarizationResult, SpeakerTurn, DIARIZATION_CONTRACT_VERSION,
};
pub use der::{diarization_error_rate, DerOptions, DerReport, ReferenceTurn};
pub use fake::{FakeDiarizationProvider, FAKE_PROVIDER_ID};
pub use provider::{authorize, is_validated, run_guarded, RunPolicy, VALIDATED_PROVIDERS};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-diarization";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
