//! Unit tests for the matcher itself: refusals, bands, aggregation and the
//! open-set default.

use super::test_support::{
    binding, cohort, cohort_vectors, exemplar, matcher, other_binding, profile, thresholds, unit,
};
use super::{
    aggregate, Aggregation, Band, ClusterEmbedding, CohortError, CohortMatrix, Embedding, Exemplar,
    MatchError, Matcher, NormalisedScore, ProfileError, ProfileOffset, SpeechGate, ThresholdSet,
    TopK, VoiceProfile, SCORE_KIND,
};

fn cluster(values: &[f32], clean_speech_ms: u32) -> ClusterEmbedding {
    ClusterEmbedding::new("speaker_1", unit(values), clean_speech_ms).expect("cluster")
}

/// Two reasonably distinct eight-dimensional "voices" plus a probe near one.
fn priya() -> VoiceProfile {
    profile(
        "person-priya",
        &[
            &[0.9, 0.3, 0.1, 0.05, 0.0, 0.0, 0.0, 0.0],
            &[0.85, 0.35, 0.15, 0.0, 0.05, 0.0, 0.0, 0.0],
        ],
    )
}

fn ana() -> VoiceProfile {
    profile(
        "person-ana",
        &[
            &[0.0, 0.0, 0.1, 0.2, 0.9, 0.3, 0.0, 0.0],
            &[0.05, 0.0, 0.0, 0.25, 0.85, 0.35, 0.0, 0.0],
        ],
    )
}

#[test]
fn a_cluster_that_sounds_like_an_enrolled_person_reaches_the_suggest_band() {
    let matcher = matcher();
    let profiles = vec![priya(), ana()];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let report = matcher
        .match_cluster(
            &cluster(&[0.88, 0.32, 0.12, 0.03, 0.0, 0.0, 0.0, 0.0], 12_000),
            &prepared,
        )
        .expect("match");

    assert!(!report.matches_nobody());
    assert_eq!(report.evaluated, 2);
    let best = report.recorded.first().expect("a recorded outcome");
    assert_eq!(best.person_id, "person-priya");
    assert_eq!(best.band, Band::Suggest);
    assert_eq!(report.suggestions().count(), 1);
    assert_eq!(report.score_kind(), SCORE_KIND);
    assert_eq!(SCORE_KIND, "cosine_similarity_asnorm");
}

#[test]
fn an_unknown_voice_matches_nobody_and_records_nothing() {
    let matcher = matcher();
    let profiles = vec![priya(), ana()];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let report = matcher
        .match_cluster(
            &cluster(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.9, 0.4], 30_000),
            &prepared,
        )
        .expect("match");

    assert!(report.matches_nobody(), "{report:?}");
    assert_eq!(report.evaluated, 2);
    assert_eq!(report.below_floor, 2);
    assert_eq!(report.suggestions().count(), 0);
    assert_eq!(report.weak_signals().count(), 0);
}

#[test]
fn with_nobody_enrolled_the_answer_is_nobody_without_scoring_anything() {
    // A cohort whose rows are all the same direction cannot produce AS-Norm
    // statistics: any attempt to score would fail. The empty-profile path
    // succeeds anyway, which is the proof that it does no work (VP-060).
    let degenerate = CohortMatrix::from_vectors(
        binding(),
        &[
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ],
    )
    .expect("cohort");
    let matcher = Matcher::new(
        binding(),
        thresholds(),
        degenerate,
        TopK::new(3).expect("top k"),
    )
    .expect("matcher");

    let report = matcher
        .match_cluster(
            &cluster(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 5_000),
            &[],
        )
        .expect("match");
    assert!(report.matches_nobody());
    assert_eq!(report.evaluated, 0);
    assert_eq!(report.below_floor, 0);
    assert_eq!(report.cluster_key, "speaker_1");

    // The same matcher cannot score a real profile: the work the empty path
    // skipped is work that would have failed.
    let profile = priya();
    assert_eq!(
        matcher.prepare(&profile),
        Err(MatchError::Cohort(CohortError::DegenerateStatistics))
    );
}

#[test]
fn a_cluster_below_the_identification_minimum_is_refused_not_scored() {
    let matcher = matcher();
    let profiles = vec![priya()];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let short = cluster(&[0.88, 0.32, 0.12, 0.03, 0.0, 0.0, 0.0, 0.0], 2_999);
    let error = matcher
        .match_cluster(&short, &prepared)
        .expect_err("refusal");
    match error {
        MatchError::NotEnoughSpeech(insufficient) => {
            assert_eq!(insufficient.gate, SpeechGate::Identify);
            assert_eq!(insufficient.required_ms, 3_000);
            assert_eq!(insufficient.available_ms, 2_999);
        }
        other => panic!("expected a speech refusal, got {other:?}"),
    }
}

#[test]
fn an_embedding_below_the_embed_minimum_cannot_become_an_exemplar_or_a_cluster() {
    let error = Exemplar::new(unit(&[1.0, 0.0]), 999).expect_err("refusal");
    assert_eq!(error.gate, SpeechGate::Embed);
    let error = ClusterEmbedding::new("speaker_1", unit(&[1.0, 0.0]), 999).expect_err("refusal");
    assert_eq!(error.gate, SpeechGate::Embed);
    assert!(Exemplar::new(unit(&[1.0, 0.0]), 1_000).is_ok());
}

#[test]
fn a_profile_below_the_enrolment_minimum_is_refused() {
    let exemplars = vec![
        exemplar(&[1.0, 0.0, 0.0], 4_000),
        exemplar(&[0.9, 0.2, 0.0], 5_999),
    ];
    let error = VoiceProfile::new("person-1", exemplars, ProfileOffset::ZERO).expect_err("refusal");
    match error {
        ProfileError::NotEnoughSpeech(insufficient) => {
            assert_eq!(insufficient.gate, SpeechGate::Enrol);
            assert_eq!(insufficient.required_ms, 10_000);
            assert_eq!(insufficient.available_ms, 9_999);
        }
        other => panic!("expected an enrolment refusal, got {other:?}"),
    }

    let enough = vec![
        exemplar(&[1.0, 0.0, 0.0], 4_000),
        exemplar(&[0.9, 0.2, 0.0], 6_000),
    ];
    assert!(VoiceProfile::new("person-1", enough, ProfileOffset::ZERO).is_ok());
}

#[test]
fn a_profile_needs_a_person_at_least_one_exemplar_and_a_direction() {
    assert_eq!(
        VoiceProfile::new(
            "  ",
            vec![exemplar(&[1.0, 0.0], 12_000)],
            ProfileOffset::ZERO
        ),
        Err(ProfileError::MissingPerson)
    );
    assert_eq!(
        VoiceProfile::new("person-1", Vec::new(), ProfileOffset::ZERO),
        Err(ProfileError::NoExemplars)
    );
    assert_eq!(
        VoiceProfile::new(
            "person-1",
            vec![exemplar(&[1.0, 0.0], 6_000), exemplar(&[-1.0, 0.0], 6_000),],
            ProfileOffset::ZERO,
        ),
        Err(ProfileError::DegenerateCentroid)
    );
}

#[test]
fn exemplars_from_two_checkpoints_or_two_dimensions_cannot_share_a_profile() {
    let other = Embedding::l2_normalise(other_binding(), &[0.9, 0.2, 0.0]).expect("embedding");
    let mixed = vec![
        exemplar(&[1.0, 0.0, 0.0], 6_000),
        Exemplar::new(other, 6_000).expect("exemplar"),
    ];
    assert!(matches!(
        VoiceProfile::new("person-1", mixed, ProfileOffset::ZERO),
        Err(ProfileError::MixedCheckpoints { index: 1, .. })
    ));

    let ragged = vec![
        exemplar(&[1.0, 0.0, 0.0], 6_000),
        exemplar(&[1.0, 0.0], 6_000),
    ];
    assert_eq!(
        VoiceProfile::new("person-1", ragged, ProfileOffset::ZERO),
        Err(ProfileError::MixedDimensions {
            index: 1,
            expected: 3,
            found: 2
        })
    );
}

#[test]
fn a_profile_or_cluster_from_another_checkpoint_refuses_to_score() {
    let matcher = matcher();

    let foreign_profile = VoiceProfile::new(
        "person-priya",
        vec![
            Exemplar::new(
                Embedding::l2_normalise(
                    other_binding(),
                    &[0.9, 0.3, 0.1, 0.05, 0.0, 0.0, 0.0, 0.0],
                )
                .expect("embedding"),
                6_000,
            )
            .expect("exemplar"),
            Exemplar::new(
                Embedding::l2_normalise(
                    other_binding(),
                    &[0.85, 0.35, 0.15, 0.0, 0.05, 0.0, 0.0, 0.0],
                )
                .expect("embedding"),
                6_000,
            )
            .expect("exemplar"),
        ],
        ProfileOffset::ZERO,
    )
    .expect("profile");
    let error = matcher.prepare(&foreign_profile).expect_err("refusal");
    assert!(
        matches!(&error, MatchError::ModelMismatch { what, .. } if *what == "profile"),
        "{error:?}"
    );
    let rendered = error.to_string();
    assert!(rendered.contains("test-embedder@2.0"), "{rendered}");
    assert!(rendered.contains("test-embedder@1.0"), "{rendered}");

    let profiles = vec![priya()];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let foreign_cluster = ClusterEmbedding::new(
        "speaker_1",
        Embedding::l2_normalise(
            other_binding(),
            &[0.88, 0.32, 0.12, 0.03, 0.0, 0.0, 0.0, 0.0],
        )
        .expect("embedding"),
        12_000,
    )
    .expect("cluster");
    let error = matcher
        .match_cluster(&foreign_cluster, &prepared)
        .expect_err("refusal");
    assert!(
        matches!(&error, MatchError::ModelMismatch { what, .. } if *what == "cluster embedding"),
        "{error:?}"
    );
}

#[test]
fn a_cohort_from_another_checkpoint_cannot_build_a_matcher() {
    let foreign =
        CohortMatrix::from_vectors(other_binding(), &cohort_vectors(16, 8, 7)).expect("cohort");
    let error = Matcher::new(
        binding(),
        thresholds(),
        foreign,
        TopK::new(4).expect("top k"),
    )
    .expect_err("refusal");
    assert!(
        matches!(&error, MatchError::ModelMismatch { what, .. } if *what == "AS-Norm cohort"),
        "{error:?}"
    );
}

#[test]
fn top_k_cannot_exceed_the_shipped_cohort() {
    let error = Matcher::new(
        binding(),
        thresholds(),
        cohort(),
        TopK::new(17).expect("top k"),
    )
    .expect_err("refusal");
    assert_eq!(
        error,
        MatchError::Cohort(CohortError::TopKTooLarge { k: 17, rows: 16 })
    );
}

#[test]
fn a_profile_spanning_several_devices_is_not_dragged_down_by_its_centroid() {
    // Exemplars 1 and 2 are the same voice through one microphone; 3 and 4 are
    // the same person through chains that land somewhere else entirely. The
    // centroid of all four resembles none of them, so centroid-only matching
    // would lose the person it just heard (VP-064, VP-067).
    let matcher = matcher();
    let near_one: &[f32] = &[0.95, 0.30, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0];
    let near_two: &[f32] = &[0.90, 0.35, 0.10, 0.0, 0.0, 0.0, 0.0, 0.0];
    let far_one: &[f32] = &[0.0, 0.30, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    let far_two: &[f32] = &[0.0, 0.0, 0.0, 0.30, 0.0, 1.0, 0.0, 0.0];
    let probe = cluster(&[0.93, 0.32, 0.07, 0.0, 0.0, 0.0, 0.0, 0.0], 12_000);

    let four_devices = vec![profile(
        "person-priya",
        &[near_one, near_two, far_one, far_two],
    )];
    let prepared = matcher.prepare_all(&four_devices).expect("prepare");
    let report = matcher.match_cluster(&probe, &prepared).expect("match");
    let multi_exemplar = report.recorded.first().expect("recorded");
    assert_eq!(multi_exemplar.aggregation, Aggregation::TopExemplarMean);
    assert_eq!(multi_exemplar.band, Band::Suggest);

    // What centroid-only matching would have produced for the same enrolment:
    // a profile whose single exemplar *is* that centroid.
    let centroid_only = vec![VoiceProfile::new(
        "person-priya",
        vec![Exemplar::new(four_devices[0].centroid().clone(), 24_000).expect("exemplar")],
        ProfileOffset::ZERO,
    )
    .expect("profile")];
    let prepared = matcher.prepare_all(&centroid_only).expect("prepare");
    let report = matcher.match_cluster(&probe, &prepared).expect("match");
    let centroid_score = report
        .recorded
        .first()
        .map_or(f32::NEG_INFINITY, |scored| scored.score.value());

    assert!(
        multi_exemplar.score.value() > centroid_score,
        "multi-exemplar {} should beat centroid-only {centroid_score}",
        multi_exemplar.score
    );
}

#[test]
fn a_single_exemplar_profile_is_scored_by_its_centroid() {
    let matcher = matcher();
    let single = VoiceProfile::new(
        "person-solo",
        vec![exemplar(&[0.9, 0.3, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0], 12_000)],
        ProfileOffset::ZERO,
    )
    .expect("profile");
    let profiles = vec![single];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let report = matcher
        .match_cluster(
            &cluster(&[0.9, 0.3, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0], 12_000),
            &prepared,
        )
        .expect("match");
    let best = report.recorded.first().expect("recorded");
    assert_eq!(best.aggregation, Aggregation::Centroid);
}

#[test]
fn an_identical_vector_still_only_produces_a_suggestion() {
    // ADR-0082 decision 1: there is no band above `suggest`, so even a perfect
    // score cannot become an assignment.
    let matcher = matcher();
    let profiles = vec![priya()];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let identical = ClusterEmbedding::new("speaker_1", profiles[0].centroid().clone(), 600_000)
        .expect("cluster");
    let report = matcher.match_cluster(&identical, &prepared).expect("match");
    let best = report.recorded.first().expect("recorded");
    assert_eq!(best.band, Band::Suggest);
    assert_eq!(best.band.column_value(), Some("suggest"));
    assert_eq!(Band::Suggest.max(best.band), Band::Suggest);
}

#[test]
fn a_profile_offset_moves_only_that_profile_out_of_the_suggestion_band() {
    let probe = cluster(&[0.88, 0.32, 0.12, 0.03, 0.0, 0.0, 0.0, 0.0], 12_000);
    let baseline = vec![priya(), ana()];

    // Read the score this cluster produces, then set the operating point just
    // below it: the shipped numbers are a harness output (VP-062), so a test
    // may only ever work relative to a score it measured.
    let reference = matcher();
    let prepared = reference.prepare_all(&baseline).expect("prepare");
    let measured = reference
        .match_cluster(&probe, &prepared)
        .expect("match")
        .recorded
        .first()
        .expect("recorded")
        .score
        .value();

    let thresholds = ThresholdSet::new(
        NormalisedScore::new(measured - 1.0).expect("floor"),
        NormalisedScore::new(measured - 0.05).expect("suggest"),
        0.5,
    )
    .expect("thresholds");
    let matcher =
        Matcher::new(binding(), thresholds, cohort(), TopK::new(4).expect("k")).expect("matcher");

    let before = matcher.match_cluster(&probe, &prepared).expect("match");
    assert_eq!(before.suggestions().count(), 1, "{before:?}");

    // Raise this one profile's operating point past the score it just produced.
    let offset = ProfileOffset::new(0.2, thresholds).expect("offset");
    let calibrated = vec![priya().with_offset(offset), ana()];
    let prepared = matcher.prepare_all(&calibrated).expect("prepare");
    let after = matcher.match_cluster(&probe, &prepared).expect("match");

    assert_eq!(after.suggestions().count(), 0, "{after:?}");
    let demoted = after
        .recorded
        .iter()
        .find(|scored| scored.person_id == "person-priya")
        .expect("still recorded as a weak signal");
    assert_eq!(demoted.band, Band::Weak);
}

#[test]
fn recorded_outcomes_are_ordered_strongest_first_and_ties_break_by_person() {
    let matcher = matcher();
    // Two profiles built from the same vectors: identical scores, so only the
    // person id can order them.
    let shape: &[f32] = &[0.9, 0.3, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0];
    let first = profile("person-b", &[shape, shape]);
    let second = profile("person-a", &[shape, shape]);
    let profiles = vec![first, second];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let report = matcher
        .match_cluster(
            &cluster(&[0.9, 0.3, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0], 12_000),
            &prepared,
        )
        .expect("match");
    let people: Vec<&str> = report
        .recorded
        .iter()
        .map(|scored| scored.person_id.as_str())
        .collect();
    assert_eq!(people, vec!["person-a", "person-b"]);
    assert_eq!(report.recorded[0].score, report.recorded[1].score);
}

#[test]
fn a_report_serializes_scores_and_people_but_never_a_vector() {
    let matcher = matcher();
    let profiles = vec![priya(), ana()];
    let prepared = matcher.prepare_all(&profiles).expect("prepare");
    let report = matcher
        .match_cluster(
            &cluster(&[0.88, 0.32, 0.12, 0.03, 0.0, 0.0, 0.0, 0.0], 12_000),
            &prepared,
        )
        .expect("match");

    let json = serde_json::to_value(&report).expect("serialize");
    let object = json.as_object().expect("object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["below_floor", "cluster_key", "evaluated", "recorded"]
    );
    for entry in json["recorded"].as_array().expect("array") {
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["aggregation", "band", "person_id", "score"]);
    }
}

#[test]
fn the_aggregate_is_the_better_of_the_centroid_and_the_top_two_exemplar_mean() {
    let score = |value: f32| NormalisedScore::new(value).expect("finite");

    // Centroid wins.
    assert_eq!(
        aggregate(score(4.0), [score(3.0), score(3.5), score(1.0)]),
        (score(4.0), Aggregation::Centroid)
    );
    // Top-2 mean (3.5 + 3.9)/2 = 3.7 wins.
    assert_eq!(
        aggregate(score(3.0), [score(3.5), score(0.5), score(3.9)]),
        (score(3.7), Aggregation::TopExemplarMean)
    );
    // A single exemplar aggregates to itself.
    assert_eq!(
        aggregate(score(1.0), [score(2.0)]),
        (score(2.0), Aggregation::TopExemplarMean)
    );
    // A tie goes to the centroid, so the aggregation is never ambiguous.
    assert_eq!(
        aggregate(score(2.0), [score(2.0), score(2.0)]),
        (score(2.0), Aggregation::Centroid)
    );
    // No exemplars at all (unreachable through `VoiceProfile`) is the centroid.
    assert_eq!(
        aggregate(score(2.0), []),
        (score(2.0), Aggregation::Centroid)
    );
}

#[test]
fn the_aggregate_does_not_depend_on_the_order_of_the_exemplars() {
    let score = |value: f32| NormalisedScore::new(value).expect("finite");
    let scores = [score(1.5), score(3.9), score(0.2), score(3.5), score(2.0)];
    let expected = aggregate(score(1.0), scores);
    for rotation in 1..scores.len() {
        let mut rotated = scores;
        rotated.rotate_left(rotation);
        assert_eq!(aggregate(score(1.0), rotated), expected);
    }
    let mut reversed = scores;
    reversed.reverse();
    assert_eq!(aggregate(score(1.0), reversed), expected);
}
