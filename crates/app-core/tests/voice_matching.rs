//! Golden trials and property invariants for the matching core (§17.1).
//!
//! Two kinds of test live here.
//!
//! **Golden trials.** `fixtures/voice-matching/golden-trials.json` fixes the
//! arithmetic of VOICE_PROFILES_SPEC §6 on synthetic vectors: cohort, profiles,
//! clusters and the expected (score, band, aggregation) per pair. The expected
//! numbers were produced by an implementation written from the spec formulas,
//! and [`the_golden_expectations_match_an_independent_recomputation`] derives
//! them a second time, here, from a naive `f64` transcription of the same
//! formulas. The production matcher is therefore checked against two
//! independent derivations rather than against its own past output.
//!
//! **Properties.** The invariants §17.1 names: aggregation is
//! permutation-invariant, the band partition is total, per-profile offsets stay
//! inside their bound, and the cohort file round-trips.

use std::path::PathBuf;

use plaudon_app_core::voice_profiles::matching::{
    aggregate, as_norm, calibrated_offset, Aggregation, Band, CalibrationOutcome, ClusterEmbedding,
    CohortMatrix, CohortStats, Embedding, Exemplar, Matcher, ModelBinding, NormalisedScore,
    ProfileOffset, ThresholdSet, TopK, VoiceProfile, EXEMPLAR_AGGREGATE_SIZE,
};
use proptest::prelude::*;
use serde::Deserialize;

// ---------------------------------------------------------------- fixtures --

#[derive(Debug, Deserialize)]
struct GoldenDocument {
    model: GoldenModel,
    thresholds: ThresholdSet,
    top_k: usize,
    cohort: Vec<Vec<f32>>,
    profiles: Vec<GoldenProfile>,
    clusters: Vec<GoldenCluster>,
}

/// Field names mirror the `voice_exemplars` columns of §7.2, so they share a
/// prefix on purpose.
#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)]
struct GoldenModel {
    model_id: String,
    model_version: String,
    model_sha256: String,
}

#[derive(Debug, Deserialize)]
struct GoldenProfile {
    person_id: String,
    offset: f32,
    exemplars: Vec<GoldenExemplar>,
}

#[derive(Debug, Deserialize)]
struct GoldenExemplar {
    vector: Vec<f32>,
    clean_speech_ms: u32,
}

#[derive(Debug, Deserialize)]
struct GoldenCluster {
    cluster_key: String,
    vector: Vec<f32>,
    clean_speech_ms: u32,
    expected: GoldenExpectation,
}

#[derive(Debug, Deserialize)]
struct GoldenExpectation {
    recorded: Vec<GoldenOutcome>,
    evaluated: usize,
    below_floor: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct GoldenOutcome {
    person_id: String,
    score: f64,
    band: String,
    aggregation: String,
}

fn golden() -> GoldenDocument {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/voice-matching/golden-trials.json");
    let text = std::fs::read_to_string(&path).expect("golden fixture");
    serde_json::from_str(&text).expect("golden fixture parses")
}

impl GoldenDocument {
    fn binding(&self) -> ModelBinding {
        ModelBinding::new(
            &self.model.model_id,
            &self.model.model_version,
            self.model.model_sha256.parse().expect("digest"),
        )
    }

    fn matcher(&self) -> Matcher {
        Matcher::new(
            self.binding(),
            self.thresholds,
            CohortMatrix::from_vectors(self.binding(), &self.cohort).expect("cohort"),
            TopK::new(self.top_k).expect("top k"),
        )
        .expect("matcher")
    }

    fn voice_profiles(&self) -> Vec<VoiceProfile> {
        self.profiles
            .iter()
            .map(|profile| {
                let exemplars = profile
                    .exemplars
                    .iter()
                    .map(|exemplar| {
                        Exemplar::new(
                            Embedding::l2_normalise(self.binding(), &exemplar.vector)
                                .expect("embedding"),
                            exemplar.clean_speech_ms,
                        )
                        .expect("exemplar")
                    })
                    .collect();
                VoiceProfile::new(
                    &profile.person_id,
                    exemplars,
                    ProfileOffset::new(profile.offset, self.thresholds).expect("offset"),
                )
                .expect("profile")
            })
            .collect()
    }
}

fn band_name(band: Band) -> &'static str {
    match band {
        Band::None => "none",
        Band::Weak => "weak",
        Band::Suggest => "suggest",
    }
}

fn aggregation_name(aggregation: Aggregation) -> &'static str {
    match aggregation {
        Aggregation::Centroid => "centroid",
        Aggregation::TopExemplarMean => "top_exemplar_mean",
    }
}

/// Tolerance between the `f64` reference and the production `f32` scores.
const SCORE_TOLERANCE: f64 = 1e-3;

// ----------------------------------------------------------- golden trials --

#[test]
fn the_matcher_reproduces_every_golden_trial() {
    let golden = golden();
    let matcher = golden.matcher();
    let profiles = golden.voice_profiles();
    let prepared = matcher.prepare_all(&profiles).expect("prepare");

    for cluster in &golden.clusters {
        let embedding = ClusterEmbedding::new(
            &cluster.cluster_key,
            Embedding::l2_normalise(golden.binding(), &cluster.vector).expect("embedding"),
            cluster.clean_speech_ms,
        )
        .expect("cluster");
        let report = matcher.match_cluster(&embedding, &prepared).expect("match");

        assert_eq!(report.cluster_key, cluster.cluster_key);
        assert_eq!(report.evaluated, cluster.expected.evaluated);
        assert_eq!(
            report.below_floor, cluster.expected.below_floor,
            "{} below-floor count",
            cluster.cluster_key
        );
        assert_eq!(
            report.recorded.len(),
            cluster.expected.recorded.len(),
            "{} recorded outcomes: {report:?}",
            cluster.cluster_key
        );
        assert_eq!(
            report.matches_nobody(),
            cluster.expected.recorded.is_empty(),
            "{} open-set outcome",
            cluster.cluster_key
        );

        for (actual, expected) in report.recorded.iter().zip(&cluster.expected.recorded) {
            assert_eq!(actual.person_id, expected.person_id, "{report:?}");
            assert_eq!(band_name(actual.band), expected.band, "{report:?}");
            assert_eq!(
                aggregation_name(actual.aggregation),
                expected.aggregation,
                "{report:?}"
            );
            let difference = (f64::from(actual.score.value()) - expected.score).abs();
            assert!(
                difference < SCORE_TOLERANCE,
                "{} scored {} for {}, expected {}",
                cluster.cluster_key,
                actual.score,
                actual.person_id,
                expected.score
            );
        }
    }
}

#[test]
fn the_golden_expectations_match_an_independent_recomputation() {
    let golden = golden();
    let cohort: Vec<Vec<f64>> = golden
        .cohort
        .iter()
        .map(|row| reference_unit(row))
        .collect();

    for cluster in &golden.clusters {
        let test_vector = reference_unit(&cluster.vector);
        let test_stats = reference_stats(&cohort, &test_vector, golden.top_k);

        let mut recorded: Vec<GoldenOutcome> = Vec::new();
        let mut below_floor = 0usize;
        for profile in &golden.profiles {
            let units: Vec<Vec<f64>> = profile
                .exemplars
                .iter()
                .map(|exemplar| reference_unit(&exemplar.vector))
                .collect();
            let mut summed = vec![0.0f64; units[0].len()];
            for unit in &units {
                for (slot, value) in summed.iter_mut().zip(unit) {
                    *slot += value;
                }
            }
            let centroid = reference_normalise(&summed);

            let centroid_score = reference_as_norm(
                reference_cosine(&test_vector, &centroid),
                reference_stats(&cohort, &centroid, golden.top_k),
                test_stats,
            );
            let mut exemplar_scores: Vec<f64> = units
                .iter()
                .map(|unit| {
                    reference_as_norm(
                        reference_cosine(&test_vector, unit),
                        reference_stats(&cohort, unit, golden.top_k),
                        test_stats,
                    )
                })
                .collect();
            exemplar_scores.sort_by(f64::total_cmp);
            exemplar_scores.reverse();
            let taken = exemplar_scores.len().min(EXEMPLAR_AGGREGATE_SIZE);
            let exemplar_score = exemplar_scores[..taken].iter().sum::<f64>() / taken as f64;

            let (score, aggregation) = if exemplar_score > centroid_score {
                (exemplar_score, "top_exemplar_mean")
            } else {
                (centroid_score, "centroid")
            };

            let floor = f64::from(golden.thresholds.floor().value() + profile.offset);
            let suggest = f64::from(golden.thresholds.suggest().value() + profile.offset);
            let band = if score >= suggest {
                "suggest"
            } else if score >= floor {
                "weak"
            } else {
                below_floor += 1;
                continue;
            };
            recorded.push(GoldenOutcome {
                person_id: profile.person_id.clone(),
                score,
                band: band.to_owned(),
                aggregation: aggregation.to_owned(),
            });
        }
        recorded.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.person_id.cmp(&right.person_id))
        });

        assert_eq!(
            below_floor, cluster.expected.below_floor,
            "{}: below-floor count",
            cluster.cluster_key
        );
        assert_eq!(
            recorded.len(),
            cluster.expected.recorded.len(),
            "{}: recorded count",
            cluster.cluster_key
        );
        for (derived, committed) in recorded.iter().zip(&cluster.expected.recorded) {
            assert_eq!(derived.person_id, committed.person_id);
            assert_eq!(derived.band, committed.band);
            assert_eq!(derived.aggregation, committed.aggregation);
            assert!(
                (derived.score - committed.score).abs() < SCORE_TOLERANCE,
                "{}: derived {} committed {}",
                cluster.cluster_key,
                derived.score,
                committed.score
            );
        }
    }
}

// A naive transcription of the §6 formulas, used only to re-derive the goldens.

fn reference_normalise(values: &[f64]) -> Vec<f64> {
    let norm = values.iter().map(|v| v * v).sum::<f64>().sqrt();
    values
        .iter()
        .map(|v| f64::from((v / norm) as f32))
        .collect()
}

fn reference_unit(values: &[f32]) -> Vec<f64> {
    reference_normalise(&values.iter().map(|v| f64::from(*v)).collect::<Vec<_>>())
}

fn reference_cosine(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(a, b)| a * b)
        .sum::<f64>()
        .clamp(-1.0, 1.0)
}

fn reference_stats(cohort: &[Vec<f64>], vector: &[f64], top_k: usize) -> (f64, f64) {
    let mut similarities: Vec<f64> = cohort
        .iter()
        .map(|row| reference_cosine(row, vector))
        .collect();
    similarities.sort_by(f64::total_cmp);
    similarities.reverse();
    let top = &similarities[..top_k];
    let mean = top.iter().sum::<f64>() / top.len() as f64;
    let variance = top.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / top.len() as f64;
    (mean, variance.sqrt())
}

fn reference_as_norm(raw: f64, enrolment: (f64, f64), test: (f64, f64)) -> f64 {
    f64::midpoint((raw - enrolment.0) / enrolment.1, (raw - test.0) / test.1)
}

// --------------------------------------------------------------- properties --

fn finite_score() -> impl Strategy<Value = NormalisedScore> {
    (-1_000.0f32..1_000.0).prop_map(|value| NormalisedScore::new(value).expect("finite"))
}

fn vector(dim: usize) -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(-4.0f32..4.0, dim)
}

fn usable_vector(dim: usize) -> impl Strategy<Value = Vec<f32>> {
    vector(dim).prop_filter("needs a direction", |values| {
        values
            .iter()
            .map(|v| f64::from(*v) * f64::from(*v))
            .sum::<f64>()
            > 1e-3
    })
}

fn test_binding() -> ModelBinding {
    ModelBinding::new(
        "property-embedder",
        "1.0",
        "11223344556677889900aabbccddeeff00112233445566778899aabbccddeeff"
            .parse()
            .expect("digest"),
    )
}

fn test_thresholds() -> ThresholdSet {
    ThresholdSet::new(
        NormalisedScore::new(1.0).expect("floor"),
        NormalisedScore::new(2.5).expect("suggest"),
        0.25,
    )
    .expect("thresholds")
}

proptest! {
    /// §17.1: "aggregation is permutation-invariant".
    #[test]
    fn aggregation_does_not_depend_on_exemplar_order(
        centroid in finite_score(),
        scores in prop::collection::vec(finite_score(), 1..12),
        rotation in 0usize..12,
    ) {
        let expected = aggregate(centroid, scores.clone());
        let mut rotated = scores.clone();
        rotated.rotate_left(rotation % scores.len());
        prop_assert_eq!(aggregate(centroid, rotated), expected);
        let mut reversed = scores;
        reversed.reverse();
        prop_assert_eq!(aggregate(centroid, reversed), expected);
    }

    /// The aggregate is never below the centroid and never above the best
    /// exemplar: it is a genuine `max` of two aggregates, not a new number.
    #[test]
    fn the_aggregate_lies_between_the_centroid_and_the_best_exemplar(
        centroid in finite_score(),
        scores in prop::collection::vec(finite_score(), 1..12),
    ) {
        let best = scores
            .iter()
            .map(|score| score.value())
            .fold(f32::NEG_INFINITY, f32::max);
        let (score, aggregation) = aggregate(centroid, scores);
        prop_assert!(score.value() >= centroid.value());
        prop_assert!(score.value() <= best.max(centroid.value()) + 1e-4);
        if aggregation == Aggregation::Centroid {
            prop_assert_eq!(score, centroid);
        }
    }

    /// §17.1: "threshold-band partitioning is total and stable".
    #[test]
    fn every_score_falls_in_exactly_one_band(
        score in finite_score(),
        offset in -0.25f32..=0.25,
    ) {
        let thresholds = test_thresholds();
        let offset = ProfileOffset::new(offset, thresholds).expect("offset");
        let band = thresholds.band(score, offset);
        let value = score.value();
        let floor = thresholds.floor().value() + offset.value();
        let suggest = thresholds.suggest().value() + offset.value();

        let memberships = [
            (band == Band::Suggest, value >= suggest),
            (band == Band::Weak, value >= floor && value < suggest),
            (band == Band::None, value < floor),
        ];
        for (classified, expected) in memberships {
            prop_assert_eq!(classified, expected);
        }
        prop_assert_eq!(
            memberships.iter().filter(|(classified, _)| *classified).count(),
            1
        );
        // There is no band above `suggest`, at any score (ADR-0082).
        prop_assert!(band <= Band::Suggest);
        prop_assert_eq!(band.is_recorded(), band != Band::None);
    }

    /// Banding is monotone: a better score never lands in a weaker band.
    #[test]
    fn a_higher_score_never_falls_in_a_weaker_band(
        left in finite_score(),
        right in finite_score(),
    ) {
        let thresholds = test_thresholds();
        let (low, high) = if left.value() <= right.value() { (left, right) } else { (right, left) };
        prop_assert!(
            thresholds.band(low, ProfileOffset::ZERO) <= thresholds.band(high, ProfileOffset::ZERO)
        );
    }

    /// §17.1: "per-profile offset bounds respected".
    #[test]
    fn a_derived_offset_always_stays_inside_the_validated_bound(
        scores in prop::collection::vec(finite_score(), 0..20),
        rejected in prop::collection::vec(any::<bool>(), 20),
    ) {
        let thresholds = test_thresholds();
        let outcomes: Vec<CalibrationOutcome> = scores
            .iter()
            .zip(&rejected)
            .map(|(score, rejected)| if *rejected {
                CalibrationOutcome::rejected(*score)
            } else {
                CalibrationOutcome::confirmed(*score)
            })
            .collect();
        let offset = calibrated_offset(&outcomes, thresholds);
        prop_assert!(offset.value().abs() <= thresholds.max_profile_offset());
        prop_assert!(ProfileOffset::new(offset.value(), thresholds).is_ok());

        // And the derivation does not depend on the order of the decisions.
        let mut shuffled = outcomes.clone();
        shuffled.reverse();
        prop_assert_eq!(calibrated_offset(&shuffled, thresholds), offset);
    }

    /// L2 normalisation makes a cosine scale-free and bounded.
    #[test]
    fn cosine_is_bounded_symmetric_and_scale_free(
        left in usable_vector(12),
        right in usable_vector(12),
        scale in 0.01f32..100.0,
    ) {
        let binding = test_binding();
        let a = Embedding::l2_normalise(binding.clone(), &left).expect("left");
        let b = Embedding::l2_normalise(binding.clone(), &right).expect("right");
        let scaled: Vec<f32> = right.iter().map(|value| value * scale).collect();
        let Ok(c) = Embedding::l2_normalise(binding, &scaled) else {
            return Ok(());
        };

        let similarity = a.cosine(&b).expect("cosine").value();
        prop_assert!((-1.0..=1.0).contains(&similarity));
        prop_assert!((similarity - b.cosine(&a).expect("cosine").value()).abs() < 1e-6);
        prop_assert!((similarity - a.cosine(&c).expect("cosine").value()).abs() < 1e-3);
        prop_assert!((a.cosine(&a).expect("cosine").value() - 1.0).abs() < 1e-5);
    }

    /// The cohort file the model pack carries survives a round trip.
    #[test]
    fn a_cohort_round_trips_through_its_pack_encoding(
        rows in prop::collection::vec(usable_vector(6), 2..12),
    ) {
        let binding = test_binding();
        let cohort = CohortMatrix::from_vectors(binding.clone(), &rows).expect("cohort");
        let decoded = CohortMatrix::decode(binding, &cohort.encode()).expect("decode");
        prop_assert_eq!(decoded, cohort);
    }

    /// AS-Norm is strictly increasing in the raw similarity, so normalisation
    /// never reorders two scores measured against the same pair of cohorts.
    #[test]
    fn as_norm_preserves_the_order_of_raw_similarities(
        lower in -0.99f32..0.98,
        gap in 0.01f32..1.0,
        probe in usable_vector(6),
        other in usable_vector(6),
        rows in prop::collection::vec(usable_vector(6), 4..10),
    ) {
        let binding = test_binding();
        let cohort = CohortMatrix::from_vectors(binding.clone(), &rows).expect("cohort");
        let k = TopK::new(3).expect("k");
        let probe = Embedding::l2_normalise(binding.clone(), &probe).expect("probe");
        let other = Embedding::l2_normalise(binding, &other).expect("other");
        let (Ok(enrolment), Ok(test)) = (cohort.statistics(&probe, k), cohort.statistics(&other, k))
        else {
            return Ok(());
        };

        let higher = (lower + gap).min(1.0);
        let low = as_norm(
            plaudon_app_core::voice_profiles::matching::CosineSimilarity::new(lower)
                .expect("similarity"),
            enrolment,
            test,
        );
        let high = as_norm(
            plaudon_app_core::voice_profiles::matching::CosineSimilarity::new(higher)
                .expect("similarity"),
            enrolment,
            test,
        );
        prop_assert!(high.value() >= low.value(), "{high} vs {low}");
        assert_stats_are_usable(enrolment);
    }
}

fn assert_stats_are_usable(stats: CohortStats) {
    assert!(stats.std_dev() > 0.0, "statistics must have spread");
    assert!((-1.0..=1.0).contains(&stats.mean()), "mean is a cosine");
}

// --------------------------------------------------- no embedding on the wire --

/// VP-002/VP-110: the serialized result of a match carries scores and person
/// ids, and the §6.3A response scanner — extended with the `VoiceEmbedding`
/// shape — accepts it. The real control is that `Embedding` has no
/// `Serialize`; this is the mechanical check over the bytes.
#[test]
fn a_real_match_report_carries_nothing_the_response_scanner_refuses() {
    let golden = golden();
    let matcher = golden.matcher();
    let profiles = golden.voice_profiles();
    let prepared = matcher.prepare_all(&profiles).expect("prepare");

    for cluster in &golden.clusters {
        let embedding = ClusterEmbedding::new(
            &cluster.cluster_key,
            Embedding::l2_normalise(golden.binding(), &cluster.vector).expect("embedding"),
            cluster.clean_speech_ms,
        )
        .expect("cluster");
        let report = matcher.match_cluster(&embedding, &prepared).expect("match");
        let payload = serde_json::to_value(&report).expect("serialize");
        plaudon_security::response::assert_no_secret_in_response(&payload)
            .expect("a match report is safe to return");

        let rendered = serde_json::to_string(&payload).expect("string");
        for exemplar in &golden.profiles[0].exemplars {
            for component in &exemplar.vector {
                assert!(
                    !rendered.contains(&format!("{component}")),
                    "a vector component leaked into {rendered}"
                );
            }
        }
    }
}

/// The `Debug` of a prepared profile — what a panic or a log line would print —
/// never contains a component either.
#[test]
fn debug_output_of_the_matching_types_never_contains_a_component() {
    let golden = golden();
    let matcher = golden.matcher();
    let profiles = golden.voice_profiles();
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let rendered = format!("{matcher:?}{prepared:?}");
    for exemplar in &golden.profiles[0].exemplars {
        for component in &exemplar.vector {
            assert!(
                !rendered.contains(&format!("{component}")),
                "a vector component leaked into Debug output"
            );
        }
    }
}
