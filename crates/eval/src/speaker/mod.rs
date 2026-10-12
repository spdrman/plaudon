//! The speaker-recognition evaluation harness (VOICE_PROFILES_SPEC §17.2).
//!
//! > `plaudon-eval` grows a speaker-recognition harness: trial lists over
//! > labelled fixtures → DET curve, EER, and the two operating points
//! > (VP-062), plus DER via the existing `diarization_error_rate`. Reports are
//! > committed JSON (`docs/benchmarks/`), schema'd [...]
//!
//! ```text
//!   fixtures  licence gate (VP-181/182)  ─► fixtures::FixtureManifest
//!   vectors   scripted or model pack     ─► embeddings::EmbeddingSource
//!   scoring   the shipped matching core  ─► harness::evaluate
//!   metrics   DET, EER, operating points ─► det::DetCurve
//!   defaults  T_suggest at FA ≤ 1%       ─► calibration::calibrate
//!   artifact  docs/benchmarks/*.json     ─► report::SpeakerMatchReport
//! ```
//!
//! # Two tiers, one code path
//!
//! The harness scores embeddings, never audio, so the hermetic CI tier
//! (scripted vectors from a seed) and the real tier (vectors a model pack's
//! extractor produced) differ only in which [`embeddings::EmbeddingSource`] is
//! passed in. What they do *not* share is standing: a report built from
//! scripted vectors says so in `release_evidence.blockers`, and
//! [`calibration::ThresholdCalibration::is_shippable_default`] is false for
//! it. The harness can therefore be fully tested in CI without ever producing
//! a number that could be mistaken for a measurement of a model.
//!
//! # Biometric invariants this module keeps
//!
//! * No embedding leaves [`embeddings`]: reports carry scores, bands and
//!   counts (VP-002, VP-112).
//! * No fixture outside the VP-181 allowlist is representable, and the VP-182
//!   names are rejected wherever they appear in a manifest.
//! * Nothing here assigns anything: the harness counts bands, and a band is a
//!   suggestion (VP-060, ADR-0082).

pub mod calibration;
pub mod det;
pub mod embeddings;
pub mod fixtures;
pub mod harness;
pub mod report;

pub use calibration::{calibrate, CalibrationStatus, ThresholdCalibration};
pub use det::{DetCurve, DetPoint, EqualErrorRate, OperatingPoint, TrialScore};
pub use embeddings::{
    EmbeddingSource, ModelPackEmbeddings, ScriptedEmbeddings, SegmentKey, SourceKind,
};
pub use fixtures::{Condition, FixtureCorpus, FixtureManifest, SessionPlan, SyntheticSession};
pub use harness::{evaluate, HarnessError, HarnessPlan, RunMetadata};
pub use report::{SpeakerMatchReport, SPEAKER_MATCH_REPORT_SCHEMA_VERSION};

/// One step of a SplitMix64 state.
///
/// The harness needs reproducible pseudo-randomness — the same fixture plan
/// must produce the same vectors and the same turn order on every platform and
/// every toolchain — and *not* cryptographic randomness. SplitMix64 is four
/// lines, has no dependency and is not a generator anyone could mistake for a
/// security primitive, which is exactly the trade we want here.
pub(crate) fn next_u64(state: u64) -> u64 {
    let state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Combine two seeds into one.
pub(crate) fn mix(left: u64, right: u64) -> u64 {
    next_u64(left ^ right.rotate_left(17))
}

/// FNV-1a over a string, so a label can seed a generator.
pub(crate) fn hash_text(text: &str) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// A standard-normal-ish sample in `[-3, 3]` from a 64-bit state.
///
/// The sum of three uniforms (Irwin–Hall) is close enough to a Gaussian for a
/// synthetic embedding cloud and costs no dependency. The scale is chosen so
/// the result has unit variance.
pub(crate) fn unit_gaussian(state: u64) -> f32 {
    let uniform = |shift: u32| -> f32 {
        let bits = (state >> shift) & 0x1F_FFFF;
        (bits as f32 / (0x20_0000u32 as f32)) - 0.5
    };
    (uniform(0) + uniform(21) + uniform(42)) * 2.0
}
