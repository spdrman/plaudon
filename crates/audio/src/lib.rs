//! Audio pipeline: validation, streaming decode, chunk planning and
//! overlap-aware timestamp reconciliation.
//!
//! Spec references: §8.1 (media validation and canonical PCM), §8.2 (chunking
//! and timestamp reconciliation), §7.4 (untrusted import bytes), §14.4
//! (resource preflight), §17.5 (bounded memory).
//!
//! Pipeline order:
//!
//! ```text
//! path ──pathguard──▶ resolved ──validate──▶ ValidatedMedia
//!                                              │
//!                                 decode (streaming, bounded memory)
//!                                              │
//!                                       CanonicalPcm (+digest)
//!                                              │
//!                                   chunk::plan(engine limits)
//!                                              │
//!                                    engine per chunk (F2.x)
//!                                              │
//!                              reconcile::merge(overlap-aware)
//! ```

pub mod chunk;
pub mod decode;
pub mod error;
pub mod pathguard;
pub mod probe;
pub mod reconcile;
pub mod resample;
pub mod silence;
pub mod sniff;
pub mod testing;
pub mod validate;

pub use chunk::{BoundaryStrategy, ChunkPlan, ChunkSpec, EngineLimits};
pub use decode::{
    decode_to_canonical_pcm, CanonicalPcmProfile, ChannelPolicy, DecodeOutcome, PcmSampleFormat,
    PcmSink, ScratchPcm,
};
pub use error::AudioError;
pub use pathguard::{resolve_media_path, sanitize_file_name, PathPolicy, ResolvedMediaPath};
pub use probe::{probe_bytes, probe_path, DurationSource, MediaProfile};
pub use reconcile::{reconcile_chunks, ChunkTranscript, ReconcileError};
pub use silence::{measure_speech_activity, SilenceOptions};
pub use sniff::{extension_matches, sniff_container, SniffedContainer};
pub use validate::{validate_media, ValidatedMedia, ValidationRequest};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-audio";

/// Version of the inter-module contract exposed by this crate (spec §6.6).
pub const CONTRACT_VERSION: u32 = 1;
