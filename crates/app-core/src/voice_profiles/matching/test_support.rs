//! Deterministic fixtures shared by the matching unit tests.
//!
//! Every vector here is synthetic: there is no voice, no corpus and no model
//! anywhere in this module (§17.5).

use plaudon_domain::media::Sha256Digest;

use super::bands::ThresholdSet;
use super::cohort::{CohortMatrix, TopK};
use super::embedding::{Embedding, ModelBinding, NormalisedScore};
use super::{Exemplar, Matcher, ProfileOffset, VoiceProfile};

/// The checkpoint the fixtures pretend to come from.
pub(crate) fn binding() -> ModelBinding {
    ModelBinding::new(
        "test-embedder",
        "1.0",
        Sha256Digest::of(b"test-embedder-1.0"),
    )
}

/// A different checkpoint of the same model: the VP-075 mismatch case.
pub(crate) fn other_binding() -> ModelBinding {
    ModelBinding::new(
        "test-embedder",
        "2.0",
        Sha256Digest::of(b"test-embedder-2.0"),
    )
}

/// L2-normalise `values` under [`binding`].
pub(crate) fn unit(values: &[f32]) -> Embedding {
    Embedding::l2_normalise(binding(), values).expect("fixture vector")
}

/// `rows × dim` deterministic pseudo-random vectors in `[-1, 1)`.
///
/// A plain LCG rather than a dependency: the fixtures must be identical on
/// every platform and must not drift when a crate is upgraded.
pub(crate) fn cohort_vectors(rows: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit_interval = (state >> 33) as f64 / f64::from(1u32 << 31);
        (unit_interval * 2.0 - 1.0) as f32
    };
    (0..rows)
        .map(|_| (0..dim).map(|_| next()).collect())
        .collect()
}

/// An eight-dimensional cohort of sixteen impostors.
pub(crate) fn cohort() -> CohortMatrix {
    CohortMatrix::from_vectors(binding(), &cohort_vectors(16, 8, 7)).expect("fixture cohort")
}

/// Operating points: floor 1.0, suggest 2.5, offsets bounded at ±0.25.
///
/// Synthetic values. VP-062 makes the shipped numbers an output of the §17
/// harness, so no test may pretend to know them.
pub(crate) fn thresholds() -> ThresholdSet {
    ThresholdSet::new(
        NormalisedScore::new(1.0).expect("floor"),
        NormalisedScore::new(2.5).expect("suggest"),
        0.25,
    )
    .expect("fixture thresholds")
}

/// A matcher over [`cohort`] with `K = 4`.
pub(crate) fn matcher() -> Matcher {
    Matcher::new(
        binding(),
        thresholds(),
        cohort(),
        TopK::new(4).expect("top k"),
    )
    .expect("fixture matcher")
}

/// An exemplar over `values` carrying `clean_speech_ms` of speech.
pub(crate) fn exemplar(values: &[f32], clean_speech_ms: u32) -> Exemplar {
    Exemplar::new(unit(values), clean_speech_ms).expect("fixture exemplar")
}

/// A profile whose exemplars each carry six seconds of clean speech.
pub(crate) fn profile(person_id: &str, vectors: &[&[f32]]) -> VoiceProfile {
    let exemplars = vectors
        .iter()
        .map(|values| exemplar(values, 6_000))
        .collect();
    VoiceProfile::new(person_id, exemplars, ProfileOffset::ZERO).expect("fixture profile")
}
