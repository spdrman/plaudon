//! The speaker-matching harness: metrics against hand-computed cases, the
//! licence gate, determinism, and the two embedding tiers (spec §17.2).
//!
//! Every DET/EER assertion here is checked against a case small enough to work
//! out on paper, written out in the test so a future reader can redo the
//! arithmetic rather than trust the implementation that produced it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plaudon_app_core::voice_profiles::matching::{Band, ModelBinding};
use plaudon_domain::media::Sha256Digest;
use plaudon_eval::speaker::calibration::{
    calibrate, CalibrationProvenance, CalibrationStatus, FLOOR_MAX_FALSE_ACCEPT,
    SUGGEST_MAX_FALSE_ACCEPT,
};
use plaudon_eval::speaker::det::{DetCurve, DetError, TrialScore};
use plaudon_eval::speaker::embeddings::{
    EmbeddingFile, EmbeddingSource, ModelPackEmbeddings, ModelPackError, ScriptedEmbeddings,
    SegmentKey, SourceKind, DEFAULT_SCRIPTED_DIM, DEFAULT_SCRIPTED_SEED, EMBEDDING_FILE_NAME,
    EMBEDDING_FILE_SCHEMA_VERSION,
};
use plaudon_eval::speaker::fixtures::{
    Condition, FixtureError, FixtureManifest, FixtureRejection, SessionPlan,
};
use plaudon_eval::speaker::harness::{self, HarnessPlan, RunMetadata};
use plaudon_eval::speaker::report::RuntimeRecord;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn plan_path() -> PathBuf {
    repo_root().join("tests/fixtures/speaker-eval/harness-plan.json")
}

fn committed_plan() -> HarnessPlan {
    let document = std::fs::read_to_string(plan_path()).expect("read the committed plan");
    HarnessPlan::parse(&document).expect("the committed plan is licence-clean and consistent")
}

fn trial(id: &str, is_target: bool, score: f64) -> TrialScore {
    TrialScore {
        trial_id: id.to_owned(),
        condition: Condition::Clean,
        is_target,
        score,
    }
}

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-9
}

// ---------------------------------------------------------------------------
// DET, EER and operating points, against hand-computed cases
// ---------------------------------------------------------------------------

#[test]
fn a_perfectly_separated_trial_set_has_an_equal_error_rate_of_zero() {
    // targets 0.9 0.8 0.7, non-targets 0.6 0.5 0.4. At threshold 0.7 no
    // non-target is accepted and no target is rejected, so both rates are 0.
    let trials = vec![
        trial("t1", true, 0.9),
        trial("t2", true, 0.8),
        trial("t3", true, 0.7),
        trial("n1", false, 0.6),
        trial("n2", false, 0.5),
        trial("n3", false, 0.4),
    ];
    let curve = DetCurve::from_trials(&trials).expect("curve");
    let eer = curve.equal_error_rate();
    assert!(close(eer.rate, 0.0), "{eer:?}");
    assert!(close(eer.threshold, 0.7), "{eer:?}");
    assert!(
        eer.exact,
        "an exactly observed crossing is not interpolated"
    );
}

#[test]
fn an_interleaved_trial_set_has_its_equal_error_rate_at_the_observed_crossing() {
    // targets 0.3 0.6, non-targets 0.4 0.5.
    //   t = 0.5: non-targets accepted {0.5} = 1/2; targets rejected {0.3} = 1/2.
    let trials = vec![
        trial("t1", true, 0.3),
        trial("t2", true, 0.6),
        trial("n1", false, 0.4),
        trial("n2", false, 0.5),
    ];
    let curve = DetCurve::from_trials(&trials).expect("curve");
    let eer = curve.equal_error_rate();
    assert!(close(eer.rate, 0.5), "{eer:?}");
    assert!(close(eer.threshold, 0.5), "{eer:?}");
    assert!(eer.exact);
}

#[test]
fn an_equal_error_rate_between_two_observed_points_is_interpolated() {
    // targets 0.1 0.5 0.9 (3), non-targets 0.2 0.3 (2):
    //   t = 0.3: FA = 1/2,  FR = 1/3  → FA - FR = +1/6
    //   t = 0.5: FA = 0,    FR = 1/3  → FA - FR = -1/3
    // The crossing is 1/3 of the way from 0.3 to 0.5, i.e. threshold
    // 0.3 + (1/3)(0.2) = 0.3666..., where both rates are 1/3.
    let trials = vec![
        trial("t1", true, 0.1),
        trial("t2", true, 0.5),
        trial("t3", true, 0.9),
        trial("n1", false, 0.2),
        trial("n2", false, 0.3),
    ];
    let curve = DetCurve::from_trials(&trials).expect("curve");
    let eer = curve.equal_error_rate();
    assert!((eer.rate - 1.0 / 3.0).abs() < 1e-9, "{eer:?}");
    assert!((eer.threshold - 0.366_666_666_6).abs() < 1e-6, "{eer:?}");
    assert!(
        !eer.exact,
        "this crossing falls between two observed points"
    );
}

#[test]
fn the_operating_point_is_the_loosest_threshold_inside_the_false_accept_ceiling() {
    // 100 non-targets at 0.00..0.99 and 10 targets at 1.0..1.9. A 1% ceiling
    // admits exactly one non-target, so the loosest compliant threshold is the
    // highest non-target score itself, 0.99, and no target is rejected.
    let mut trials: Vec<TrialScore> = (0..100)
        .map(|index| trial(&format!("n{index}"), false, f64::from(index) / 100.0))
        .collect();
    trials.extend(
        (0..10).map(|index| trial(&format!("t{index}"), true, 1.0 + f64::from(index) / 10.0)),
    );
    let curve = DetCurve::from_trials(&trials).expect("curve");
    let point = curve.operating_point(0.01).expect("a compliant threshold");
    assert!(close(point.threshold, 0.99), "{point:?}");
    assert!(close(point.false_accept_rate, 0.01), "{point:?}");
    assert!(close(point.false_reject_rate, 0.0), "{point:?}");

    // A 10% ceiling is met by accepting the ten highest non-targets, i.e. the
    // threshold 0.90.
    let looser = curve.operating_point(0.10).expect("a compliant threshold");
    assert!(close(looser.threshold, 0.90), "{looser:?}");
    assert!(close(looser.false_accept_rate, 0.10), "{looser:?}");
}

#[test]
fn a_trial_set_without_both_kinds_of_trial_has_no_curve() {
    assert_eq!(
        DetCurve::from_trials(&[trial("t", true, 1.0)]).unwrap_err(),
        DetError::NoNonTargets
    );
    assert_eq!(
        DetCurve::from_trials(&[trial("n", false, 1.0)]).unwrap_err(),
        DetError::NoTargets
    );
    assert_eq!(
        DetCurve::from_trials(&[trial("t", true, f64::NAN), trial("n", false, 0.0)]).unwrap_err(),
        DetError::NonFiniteScore("score")
    );
}

#[test]
fn a_sample_too_small_to_resolve_a_rate_says_so() {
    let trials = vec![
        trial("t1", true, 1.0),
        trial("n1", false, 0.0),
        trial("n2", false, 0.1),
    ];
    let curve = DetCurve::from_trials(&trials).expect("curve");
    assert!(close(curve.false_accept_resolution(), 0.5));
    assert!(!curve.resolves_rate(0.01), "two non-targets cannot show 1%");
    assert!(curve.resolves_rate(0.5));
    let published = curve.summary_points();
    assert_eq!(published.len(), 1, "{published:?}");
    assert!(
        close(published[0].max_false_accept_rate, 0.5),
        "only the 50% ceiling is resolvable with two non-target trials: {published:?}"
    );
}

// ---------------------------------------------------------------------------
// Threshold calibration
// ---------------------------------------------------------------------------

fn provenance(source: SourceKind, targets: usize, nontargets: usize) -> CalibrationProvenance {
    CalibrationProvenance {
        embedding_source: source,
        model_id: "test-model".to_owned(),
        model_version: "v1".to_owned(),
        model_sha256: Sha256Digest::of(b"test").to_hex(),
        library_id: "test-library".to_owned(),
        manifest_sha256: Sha256Digest::of(b"manifest").to_hex(),
        targets,
        nontargets,
        false_accept_resolution: 1.0 / nontargets as f64,
    }
}

/// 200 non-targets in `[0, 2)` and 50 targets in `[3, 8)`: the ceilings fall
/// inside the non-target range, so both thresholds are observable.
fn separable_curve() -> DetCurve {
    let mut trials: Vec<TrialScore> = (0..200)
        .map(|index| trial(&format!("n{index}"), false, f64::from(index) / 100.0))
        .collect();
    trials.extend(
        (0..50).map(|index| trial(&format!("t{index}"), true, 3.0 + f64::from(index) / 10.0)),
    );
    DetCurve::from_trials(&trials).expect("curve")
}

#[test]
fn thresholds_are_derived_from_the_curve_at_the_published_ceilings() {
    let curve = separable_curve();
    let calibration = calibrate(&curve, provenance(SourceKind::ModelPack, 50, 200));
    assert_eq!(calibration.status, CalibrationStatus::Derived);

    let suggest = calibration.suggest.expect("suggest point");
    let floor = calibration.floor.expect("floor point");
    assert!(suggest.false_accept_rate <= SUGGEST_MAX_FALSE_ACCEPT);
    assert!(floor.false_accept_rate <= FLOOR_MAX_FALSE_ACCEPT);
    assert!(floor.threshold < suggest.threshold, "the weak band exists");

    // 200 non-targets at 0.00..1.99: a 1% ceiling accepts the top two, so the
    // threshold is 1.98; a 10% ceiling accepts the top twenty, so 1.80.
    assert!(close(suggest.threshold, 1.98), "{suggest:?}");
    assert!(close(floor.threshold, 1.80), "{floor:?}");

    let set = calibration.threshold_set().expect("a usable threshold set");
    assert!((set.suggest().value() - 1.98).abs() < 1e-5);
    assert!((set.floor().value() - 1.80).abs() < 1e-5);
    assert!(
        set.max_profile_offset() > 0.0 && set.max_profile_offset() < set.band_width(),
        "the per-profile offset bound stays inside the band"
    );
    assert!(calibration.is_shippable_default());
}

#[test]
fn thresholds_derived_from_scripted_embeddings_are_never_a_shippable_default() {
    let calibration = calibrate(
        &separable_curve(),
        provenance(SourceKind::Scripted, 50, 200),
    );
    assert_eq!(calibration.status, CalibrationStatus::Derived);
    assert!(
        !calibration.is_shippable_default(),
        "a scripted run measures the harness, not a checkpoint (VP-183)"
    );
}

#[test]
fn a_trial_set_that_cannot_resolve_one_percent_yields_no_thresholds() {
    let mut trials: Vec<TrialScore> = (0..40)
        .map(|index| trial(&format!("n{index}"), false, f64::from(index) / 100.0))
        .collect();
    trials.extend((0..20).map(|index| trial(&format!("t{index}"), true, 2.0 + f64::from(index))));
    let curve = DetCurve::from_trials(&trials).expect("curve");
    let calibration = calibrate(&curve, provenance(SourceKind::ModelPack, 20, 40));
    assert_eq!(
        calibration.status,
        CalibrationStatus::InsufficientTrials {
            required_nontargets: 100,
            observed_nontargets: 40,
        }
    );
    assert!(calibration.suggest.is_none());
    assert!(calibration.threshold_set().is_none());
    assert!(!calibration.is_shippable_default());
}

#[test]
fn a_curve_whose_ceilings_collapse_onto_one_score_has_no_weak_band() {
    // 200 non-targets all at 0.0 and 50 targets at 1.0: every ceiling is met
    // by the same observed threshold, so there is no room for a weak band.
    let mut trials: Vec<TrialScore> = (0..200)
        .map(|index| trial(&format!("n{index}"), false, 0.0))
        .collect();
    trials.extend((0..50).map(|index| trial(&format!("t{index}"), true, 1.0)));
    let curve = DetCurve::from_trials(&trials).expect("curve");
    let calibration = calibrate(&curve, provenance(SourceKind::ModelPack, 50, 200));
    assert!(
        matches!(calibration.status, CalibrationStatus::NoWeakBand { .. }),
        "{:?}",
        calibration.status
    );
    assert!(calibration.floor.is_none());
}

// ---------------------------------------------------------------------------
// Fixture licensing (VP-181, VP-182)
// ---------------------------------------------------------------------------

fn manifest_document(corpus: &str, notes: &str, recipe: &str) -> String {
    format!(
        r#"{{
          "schema_version": 1,
          "library_id": "test-library",
          "notes": "{notes}",
          "fixtures": [
            {{
              "fixture_id": "row-1",
              "corpus": "{corpus}",
              "condition": "clean",
              "speakers": ["a", "b"],
              "duration_ms": 30000,
              "media_sha256": "{digest}",
              "media_committed": false,
              "recipe": "{recipe}"
            }}
          ]
        }}"#,
        digest = Sha256Digest::of(b"fixture bytes the test itself made up").to_hex()
    )
}

#[test]
fn a_fixture_from_a_corpus_outside_the_allowlist_does_not_even_parse() {
    let error = FixtureManifest::parse(&manifest_document(
        "voxceleb",
        "trial fixtures",
        "downloaded",
    ))
    .expect_err("VoxCeleb is not a representable corpus (VP-182)");
    assert!(matches!(error, FixtureError::Malformed(_)), "{error:?}");
}

#[test]
fn a_forbidden_corpus_named_in_free_text_is_refused_wherever_it_appears() {
    let error = FixtureManifest::parse(&manifest_document(
        "libri_speech_test_clean_derived",
        "LibriSpeech sessions",
        "LibriSpeech concatenation mixed with VoxConverse meeting audio",
    ))
    .expect_err("a VoxConverse-contaminated recipe is refused");
    let FixtureError::Refused(rejections) = error else {
        panic!("expected a licence refusal, got {error:?}");
    };
    assert!(
        rejections.iter().any(|rejection| matches!(
            rejection,
            FixtureRejection::ForbiddenCorpus { corpus, field, .. }
                if corpus == "voxconverse" && field == "recipe"
        )),
        "{rejections:?}"
    );

    // Spelling variants fold to the same name.
    let folded = FixtureManifest::parse(&manifest_document(
        "libri_speech_test_clean_derived",
        "includes Vox_Celeb 2 trials",
        "concatenation",
    ))
    .expect_err("Vox_Celeb is VoxCeleb");
    assert!(matches!(folded, FixtureError::Refused(_)), "{folded:?}");
}

#[test]
fn a_download_on_demand_corpus_may_not_be_a_committed_fixture() {
    let error = FixtureManifest::parse(&manifest_document(
        "libri_speech_test_clean_derived",
        "augmented with AMI meeting excerpts",
        "concatenation",
    ))
    .expect_err("AMI is a developer-local accuracy gate, not a fixture");
    let FixtureError::Refused(rejections) = error else {
        panic!("expected a refusal, got {error:?}");
    };
    assert!(
        rejections.iter().any(|rejection| matches!(
            rejection,
            FixtureRejection::DownloadOnDemandOnly { corpus, .. } if corpus == "ami"
        )),
        "{rejections:?}"
    );

    // A word that merely contains the name is not a corpus reference.
    FixtureManifest::parse(&manifest_document(
        "libri_speech_test_clean_derived",
        "dynamic ceramic room responses",
        "concatenation",
    ))
    .expect("`ceramic` is not AMI");
}

#[test]
fn human_voiced_fixtures_need_a_digest_and_may_not_commit_their_media() {
    let document = format!(
        r#"{{
          "schema_version": 1,
          "library_id": "test-library",
          "notes": "",
          "fixtures": [
            {{
              "fixture_id": "no-digest",
              "corpus": "libri_speech_test_clean_derived",
              "condition": "clean",
              "speakers": ["a"],
              "duration_ms": 1000,
              "media_sha256": null,
              "media_committed": true,
              "recipe": "seeded concatenation"
            }},
            {{
              "fixture_id": "no-digest",
              "corpus": "kokoro_tts",
              "condition": "clean",
              "speakers": [],
              "duration_ms": 1000,
              "media_sha256": "{digest}",
              "media_committed": false,
              "recipe": "text to speech"
            }}
          ]
        }}"#,
        digest = Sha256Digest::of(b"tts bytes").to_hex()
    );
    let FixtureError::Refused(rejections) =
        FixtureManifest::parse(&document).expect_err("four rules are broken")
    else {
        panic!("expected a refusal");
    };
    assert!(rejections.iter().any(|r| matches!(
        r,
        FixtureRejection::MissingMediaDigest { fixture_id } if fixture_id == "no-digest"
    )));
    assert!(rejections
        .iter()
        .any(|r| matches!(r, FixtureRejection::MediaMayNotBeCommitted { .. })));
    assert!(rejections
        .iter()
        .any(|r| matches!(r, FixtureRejection::DuplicateId { .. })));
    assert!(rejections
        .iter()
        .any(|r| matches!(r, FixtureRejection::NoSpeakers { .. })));
}

#[test]
fn the_committed_library_publishes_its_attribution_obligations() {
    let plan = committed_plan();
    let attributions = plan.library.attributions();
    assert_eq!(attributions.len(), 1, "{attributions:?}");
    assert!(attributions[0].attribution.contains("fixed seed"));
    assert!(
        plan.library
            .fixtures
            .iter()
            .all(|fixture| fixture.media_sha256.is_none()),
        "the CI tier has no media at all"
    );
}

// ---------------------------------------------------------------------------
// Synthetic sessions and their ground truth
// ---------------------------------------------------------------------------

fn session_plan() -> SessionPlan {
    SessionPlan {
        session_id: "s1".to_owned(),
        speakers: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
        utterances_per_speaker: 3,
        utterance_ms: 2000,
        gap_ms: 250,
        overlap_every: 3,
        overlap_ms: 500,
        seed: 7,
    }
}

#[test]
fn a_session_plan_lays_out_the_same_timeline_every_time() {
    let first = session_plan().synthesize();
    let second = session_plan().synthesize();
    assert_eq!(first, second, "the layout is a pure function of the plan");

    let mut other = session_plan();
    other.seed = 8;
    assert_ne!(
        other.synthesize().turns,
        first.turns,
        "a different seed is a different permutation"
    );
}

#[test]
fn the_synthesized_session_has_exact_ground_truth_including_overlap() {
    let session = session_plan().synthesize();
    assert_eq!(session.turns.len(), 9, "three speakers, three utterances");
    for speaker in ["a", "b", "c"] {
        assert_eq!(
            session
                .turns
                .iter()
                .filter(|turn| turn.speaker == speaker)
                .count(),
            3
        );
    }
    assert_eq!(session.speech_ms(), 9 * 2000);
    assert!(session.overlap_ms() > 0, "the plan asks for overlap");

    let rttm = session.rttm();
    assert_eq!(rttm.lines().count(), 9);
    let first = session.turns.first().expect("a first turn");
    assert!(
        rttm.lines().next().expect("a line").starts_with(&format!(
            "SPEAKER s1 1 {:.3} {:.3} <NA> <NA> {}",
            first.start_ms as f64 / 1000.0,
            (first.end_ms - first.start_ms) as f64 / 1000.0,
            first.speaker
        )),
        "{rttm}"
    );

    let reference = session.reference_turns();
    assert_eq!(reference.len(), session.turns.len());
    assert_eq!(reference[0].start_ms, first.start_ms);
}

#[test]
fn the_scripted_diarization_hypothesis_is_wrong_in_the_ways_it_claims_to_be() {
    let session = session_plan().synthesize();
    let hypothesis = harness::scripted_hypothesis(&session);
    assert_eq!(
        hypothesis.len(),
        session.turns.len() - 1,
        "every seventh turn is dropped"
    );
    assert!(
        hypothesis
            .iter()
            .all(|turn| turn.cluster.as_str().starts_with("cluster-")),
        "the scorer has to find the speaker mapping itself"
    );
    assert!(hypothesis
        .iter()
        .zip(session.turns.iter())
        .all(|(hyp, reference)| hyp.start_ms > reference.start_ms));
}

// ---------------------------------------------------------------------------
// End-to-end harness passes
// ---------------------------------------------------------------------------

fn scripted_report() -> plaudon_eval::speaker::SpeakerMatchReport {
    let plan = committed_plan();
    let source = ScriptedEmbeddings::seeded(DEFAULT_SCRIPTED_SEED, DEFAULT_SCRIPTED_DIM);
    harness::evaluate(&plan, &source, &RunMetadata::default()).expect("a scripted pass")
}

#[test]
fn the_committed_plan_runs_hermetically_and_deterministically() {
    let first = scripted_report();
    let second = scripted_report();
    assert_eq!(
        first.to_json(),
        second.to_json(),
        "the same plan and seed produce the same bytes"
    );
    assert_eq!(first.model.embedding_source, SourceKind::Scripted);
    assert_eq!(
        first.overall.targets,
        12 * 5 * 5,
        "twelve speakers, five trials each, five conditions"
    );
    assert_eq!(first.overall.nontargets, 12 * 5 * 5 * 11);
    assert_eq!(first.conditions.len(), 5, "every condition is reported");
}

#[test]
fn the_committed_artifact_is_what_the_harness_produces_today() {
    let committed = std::fs::read_to_string(
        repo_root().join("docs/benchmarks/speaker-match-scripted-synthetic-eer.json"),
    )
    .expect("the committed artifact exists");
    assert_eq!(
        committed,
        scripted_report().to_json(),
        "regenerate with: cargo run -p plaudon-eval --bin plaudon-eval -- speaker-match \
         --plan tests/fixtures/speaker-eval/harness-plan.json \
         --out docs/benchmarks/speaker-match-scripted-synthetic-eer.json"
    );
}

#[test]
fn a_scripted_report_is_never_release_evidence() {
    let report = scripted_report();
    assert!(!report.release_evidence.qualifies);
    assert!(
        report
            .release_evidence
            .blockers
            .iter()
            .any(|blocker| blocker.contains("scripted")),
        "{:?}",
        report.release_evidence.blockers
    );
    assert!(!report.calibration.is_shippable_default());
    assert!(
        report
            .caveats
            .iter()
            .any(|caveat| caveat.contains("skewed")),
        "the demographic caveat travels with the numbers (VP-170)"
    );
}

#[test]
fn no_embedding_is_anywhere_in_the_artifact() {
    let json: serde_json::Value =
        serde_json::from_str(&scripted_report().to_json()).expect("valid JSON");

    fn longest_number_array(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Array(items) => {
                let own = if items.iter().all(serde_json::Value::is_number) {
                    items.len()
                } else {
                    0
                };
                own.max(items.iter().map(longest_number_array).max().unwrap_or(0))
            }
            serde_json::Value::Object(map) => {
                map.values().map(longest_number_array).max().unwrap_or(0)
            }
            _ => 0,
        }
    }

    assert_eq!(
        longest_number_array(&json),
        0,
        "a report carries scores and counts, never a vector (VP-002, VP-112)"
    );
}

#[test]
fn degraded_conditions_are_reported_separately_and_score_worse() {
    let report = scripted_report();
    let rate = |token: &str| -> f64 {
        report
            .conditions
            .iter()
            .find(|condition| condition.condition == token)
            .unwrap_or_else(|| panic!("condition {token} is reported"))
            .equal_error_rate
            .rate
    };
    assert!(
        rate("clean") < rate("opus_low_bitrate"),
        "clean {:.4} vs opus {:.4}",
        rate("clean"),
        rate("opus_low_bitrate")
    );
    assert!(rate("clean") < rate("room_impulse_response"));
    for condition in &report.conditions {
        assert_eq!(
            condition.targets + condition.nontargets,
            12 * 5 * 12,
            "every cluster is scored against every profile"
        );
    }
}

#[test]
fn the_published_bands_come_from_the_derived_thresholds() {
    let report = scripted_report();
    let bands = report.overall.bands.expect("thresholds were derived");
    assert_eq!(
        bands.target_suggest + bands.target_weak + bands.target_none,
        report.overall.targets
    );
    assert_eq!(
        bands.nontarget_suggest + bands.nontarget_weak + bands.nontarget_none,
        report.overall.nontargets
    );

    let suggest = report.calibration.suggest.expect("a suggestion threshold");
    let expected_false_suggestions =
        (suggest.false_accept_rate * report.overall.nontargets as f64).round() as usize;
    assert_eq!(
        bands.nontarget_suggest, expected_false_suggestions,
        "the band counts and the operating point describe the same trials"
    );
    assert!(
        suggest.false_accept_rate <= SUGGEST_MAX_FALSE_ACCEPT,
        "{suggest:?}"
    );
}

#[test]
fn the_golden_suggestions_are_pinned_to_the_checkpoint_that_produced_them() {
    let report = scripted_report();
    let binding = ScriptedEmbeddings::binding_for(DEFAULT_SCRIPTED_SEED, DEFAULT_SCRIPTED_DIM);
    assert_eq!(report.model.model_sha256, binding.model_sha256.to_hex());
    assert_eq!(report.model.dim, DEFAULT_SCRIPTED_DIM);

    // The pinned clusters are the first trial of the first three roster
    // speakers, each of which must suggest its own person at the top.
    for speaker in ["speaker-01", "speaker-02", "speaker-03"] {
        let cluster = format!("{speaker}/4/clean");
        let top = report
            .golden_suggestions
            .iter()
            .filter(|suggestion| suggestion.cluster_key == cluster)
            .max_by(|left, right| left.score.total_cmp(&right.score))
            .unwrap_or_else(|| panic!("a suggestion for {cluster}"));
        assert_eq!(top.person_id, speaker, "{top:?}");
        assert_eq!(top.band, Band::Suggest, "{top:?}");
        assert!(top.score > 4.0, "{top:?}");
    }

    // A different seed is a different checkpoint, so the golden file's digest
    // changes and the comparison above would fail loudly rather than drift.
    let other = ScriptedEmbeddings::binding_for(DEFAULT_SCRIPTED_SEED + 1, DEFAULT_SCRIPTED_DIM);
    assert_ne!(binding.model_sha256, other.model_sha256);
}

#[test]
fn the_diarization_section_scores_the_scripted_hypothesis_with_the_shipped_der() {
    let plan = committed_plan();
    let report = scripted_report();
    let section = report.diarization.expect("a diarization section");
    assert_eq!(section.session_id, plan.session.session_id);
    assert_eq!(section.collar_ms, 0, "we report the harsher number");
    assert!(!section.skip_overlap_regions, "overlap is scored");
    assert!(section.der > 0.0 && section.der < 1.0, "{section:?}");

    let session = plan.session.synthesize();
    let expected = plaudon_ai_diarization::der::diarization_error_rate(
        &session.reference_turns(),
        &harness::scripted_hypothesis(&session),
        plaudon_ai_diarization::der::DerOptions::default(),
    );
    assert!(
        (section.der - expected.der).abs() < 1e-12,
        "the section is the shipped scorer's own output"
    );
    assert_eq!(section.missed_ms, expected.missed_ms);
    assert_eq!(section.confusion_ms, expected.confusion_ms);
    assert_eq!(section.reference_speech_ms, session.speech_ms());
}

// ---------------------------------------------------------------------------
// The model-pack tier
// ---------------------------------------------------------------------------

/// Every segment the committed plan asks for, as a model pack would supply it.
fn pack_file(binding: &ModelBinding) -> EmbeddingFile {
    let plan = committed_plan();
    let scripted = ScriptedEmbeddings::seeded(DEFAULT_SCRIPTED_SEED, DEFAULT_SCRIPTED_DIM);
    let mut vectors = BTreeMap::new();
    let mut insert = |key: SegmentKey| {
        let vector = scripted.vector(&key).expect("a scripted vector");
        vectors.insert(key.token(), vector);
    };
    for speaker in &plan.cohort_speakers {
        insert(SegmentKey::new(speaker, 0, Condition::Clean));
    }
    for speaker in &plan.roster {
        for utterance in 0..plan.enrol_utterances + plan.test_utterances {
            insert(SegmentKey::new(speaker, utterance, Condition::Clean));
            for condition in &plan.conditions {
                insert(SegmentKey::new(speaker, utterance, *condition));
            }
        }
    }
    EmbeddingFile {
        schema_version: EMBEDDING_FILE_SCHEMA_VERSION,
        model: binding.clone(),
        dim: DEFAULT_SCRIPTED_DIM,
        vectors,
    }
}

fn pack_binding() -> ModelBinding {
    ModelBinding::new(
        "wespeaker-resnet34-lm",
        "2026-01",
        Sha256Digest::of(b"a checkpoint this test pretends to have extracted with"),
    )
}

#[test]
fn a_model_pack_supplies_the_vectors_through_the_same_code_path() {
    let directory = tempfile::tempdir().expect("temp dir");
    let binding = pack_binding();
    std::fs::write(
        directory.path().join(EMBEDDING_FILE_NAME),
        serde_json::to_string(&pack_file(&binding)).expect("serialize"),
    )
    .expect("write the pack file");

    let source = ModelPackEmbeddings::open(directory.path()).expect("the pack opens");
    assert_eq!(source.kind(), SourceKind::ModelPack);
    assert_eq!(source.binding(), &binding);

    let plan = committed_plan();
    let report = harness::evaluate(
        &plan,
        &source,
        &RunMetadata {
            measured_on: None,
            commit: Some("0123456789abcdef".to_owned()),
            runtime: Some(RuntimeRecord {
                platform: "macos-arm64".to_owned(),
                host_cpu: Some("test".to_owned()),
                peak_rss_bytes: Some(128 * 1024 * 1024),
                startup_ms: Some(900),
                real_time_factor: Some(0.12),
            }),
        },
    )
    .expect("a model-pack pass");

    assert_eq!(report.model.model_id, "wespeaker-resnet34-lm");
    assert_eq!(report.model.model_sha256, binding.model_sha256.to_hex());
    assert_eq!(
        report.file_name(),
        "speaker-match-wespeaker-resnet34-lm-eer.json"
    );
    assert!(
        report.release_evidence.qualifies,
        "{:?}",
        report.release_evidence.blockers
    );
    assert!(report.calibration.is_shippable_default());
}

#[test]
fn a_model_pack_missing_its_embedding_file_is_simply_absent() {
    let directory = tempfile::tempdir().expect("temp dir");
    let error = ModelPackEmbeddings::open(directory.path()).expect_err("nothing to open");
    assert!(matches!(error, ModelPackError::NotPresent(_)), "{error:?}");
}

#[test]
fn a_model_pack_without_the_runtime_figures_is_not_release_evidence_either() {
    let binding = pack_binding();
    let source = ModelPackEmbeddings::from_file(pack_file(&binding)).expect("a pack");
    let report = harness::evaluate(&committed_plan(), &source, &RunMetadata::default())
        .expect("a model-pack pass");
    assert!(!report.release_evidence.qualifies);
    assert!(
        report
            .release_evidence
            .blockers
            .iter()
            .any(|blocker| blocker.contains("peak RSS")),
        "{:?}",
        report.release_evidence.blockers
    );
}

#[test]
fn a_pack_that_is_missing_a_segment_fails_loudly_instead_of_scoring_fewer_trials() {
    let binding = pack_binding();
    let mut file = pack_file(&binding);
    file.vectors
        .remove(&SegmentKey::new("speaker-03", 4, Condition::Clean).token());
    let source = ModelPackEmbeddings::from_file(file).expect("a pack");
    let error = harness::evaluate(&committed_plan(), &source, &RunMetadata::default())
        .expect_err("a missing segment is not a silently smaller trial list");
    assert!(error.to_string().contains("speaker-03/4/clean"), "{error}");
}

#[test]
fn the_trial_list_is_enumerable_before_anything_is_scored() {
    let plan = committed_plan();
    let trials = plan.trial_list();
    let targets = trials.iter().filter(|trial| trial.is_target).count();

    assert_eq!(trials.len(), 5 * 12 * 5 * 12, "the full cross product");
    assert_eq!(trials.len(), plan.expected_trials());
    assert_eq!(targets, 5 * 12 * 5);
    assert!(
        trials
            .iter()
            .all(|trial| trial.segment.utterance >= plan.enrol_utterances),
        "no profile is scored against its own enrolment material"
    );

    let report = scripted_report();
    assert_eq!(report.overall.targets, targets);
    assert_eq!(report.overall.nontargets, trials.len() - targets);

    let expected_ids: std::collections::BTreeSet<&str> =
        trials.iter().map(|trial| trial.trial_id.as_str()).collect();
    assert_eq!(expected_ids.len(), trials.len(), "trial ids are unique");
    assert!(expected_ids.contains("speaker-01/speaker-02/4/clean"));
}
