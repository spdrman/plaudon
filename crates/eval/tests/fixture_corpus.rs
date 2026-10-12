//! The committed fixture corpus must parse, refuse its uncleared row, and
//! produce the hand-computed metrics (spec 17.1, 17.2, 17.6).
//!
//! Every number asserted here is derivable by hand from the fixture text and
//! the normalization pipeline documented in `plaudon_eval::metrics`. No
//! quality threshold is asserted: spec 17.6 defers those.

use std::path::{Path, PathBuf};

use plaudon_domain::time::Timestamp;
use plaudon_eval::corpus::{ClearanceRejection, ConsentStatus, CorpusManifest};
use plaudon_eval::hypothesis::{HypothesisAlignment, HypothesisSet};
use plaudon_eval::report::{EnvironmentRecord, EvaluationReport};
use plaudon_eval::runner::{self, RunContext, RunError};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn fixtures() -> PathBuf {
    repo_root().join("tests/fixtures/eval")
}

fn manifest() -> CorpusManifest {
    CorpusManifest::load(&fixtures().join("corpus.yaml")).expect("the fixture corpus must parse")
}

fn environment() -> EnvironmentRecord {
    EnvironmentRecord {
        os: "linux".to_owned(),
        os_version: Some("6.1.0".to_owned()),
        cpu: Some("fixture-cpu".to_owned()),
        ram_bytes: Some(16 * 1024 * 1024 * 1024),
        commit: Some("0000000000000000000000000000000000000000".to_owned()),
        measured_on: Timestamp::from_millis(1_780_000_000_000),
    }
}

fn run(engine: &str) -> Result<EvaluationReport, RunError> {
    let manifest = manifest();
    let hypotheses =
        HypothesisSet::load(&fixtures().join("hypotheses")).expect("hypotheses must load");
    runner::run(
        &manifest,
        &fixtures(),
        &hypotheses,
        engine,
        &RunContext::new(environment(), "0.1.0"),
    )
}

fn close(left: f64, right: f64) {
    assert!(
        (left - right).abs() < 1e-9,
        "expected {right}, measured {left}"
    );
}

#[test]
fn the_fixture_corpus_covers_every_duration_bucket_and_consent_status() {
    let manifest = manifest();
    assert_eq!(manifest.items.len(), 4);
    let buckets: Vec<&str> = manifest
        .items
        .iter()
        .map(|item| item.duration_bucket.as_str())
        .collect();
    assert_eq!(buckets, vec!["1min", "10min", "60min", "180min"]);
    assert!(manifest
        .items
        .iter()
        .any(|item| item.consent == ConsentStatus::Synthetic));
    assert!(manifest
        .items
        .iter()
        .any(|item| item.consent == ConsentStatus::OptInConsented));
    assert!(manifest
        .items
        .iter()
        .any(|item| item.consent == ConsentStatus::PublicLicensed));
}

#[test]
fn the_unlicensed_fixture_row_is_refused_with_both_of_its_reasons() {
    let manifest = manifest();
    let cleared = manifest.clear();
    assert_eq!(cleared.accepted().len(), 3);
    assert_eq!(cleared.rejected().len(), 1);
    let rejection = &cleared.rejected()[0];
    assert_eq!(rejection.item_id, "unl-draft-004");
    assert_eq!(
        rejection.reasons,
        vec![
            ClearanceRejection::ConsentNotCleared {
                status: ConsentStatus::Unverified
            },
            ClearanceRejection::MissingLicense
        ]
    );
}

#[test]
fn no_fixture_row_is_confidential_client_material() {
    for item in manifest().items {
        assert!(
            !item.attribution.confidential_real_client,
            "{} is flagged confidential; spec 17.1 forbids committing such material",
            item.id
        );
    }
}

#[test]
fn the_run_measures_only_cleared_items_and_publishes_the_exclusion() {
    let report = run("whisper.cpp").expect("run");
    assert_eq!(report.corpus.items_declared, 4);
    assert_eq!(report.corpus.items_cleared, 3);
    assert_eq!(report.corpus.items_evaluated, 3);
    assert_eq!(report.corpus.excluded.len(), 1);
    assert_eq!(report.corpus.excluded[0].item_id, "unl-draft-004");
    assert_eq!(report.corpus.missing_hypotheses, Vec::<String>::new());
    assert!(
        !report
            .per_item
            .iter()
            .any(|item| item.item_id == "unl-draft-004"),
        "an uncleared item must never reach a metric"
    );
    assert_eq!(report.sample.evaluated_items, 3);
    assert_eq!(report.sample.items_with_overlapping_speech, 1);
    assert_eq!(report.sample.multi_speaker_items, 2);
    assert_eq!(report.sample.locale_breakdown.len(), 3);
    assert_eq!(
        report.sample.total_audio_ms, None,
        "one item recorded no runtime"
    );
    assert_eq!(
        report.reference_quality,
        manifest().reference_quality,
        "the published reference-quality note comes from the corpus"
    );
}

#[test]
fn the_hand_computed_word_and_character_rates_are_reproduced() {
    let report = run("whisper.cpp").expect("run");
    let item = |id: &str| {
        report
            .per_item
            .iter()
            .find(|entry| entry.item_id == id)
            .expect("item")
    };

    // syn-quiet-001: 13 reference words, four hallucinated words appended.
    let quiet = item("syn-quiet-001");
    assert_eq!(quiet.word_error_rate.reference_length, 13);
    assert_eq!(quiet.word_error_rate.insertions, 4);
    assert_eq!(quiet.word_error_rate.substitutions, 0);
    assert_eq!(quiet.word_error_rate.deletions, 0);
    close(quiet.word_error_rate.rate.expect("rate"), 4.0 / 13.0);

    // syn-site-002: 10 reference words; "contractor" becomes "contract or",
    // which is one substitution plus one insertion.
    let site = item("syn-site-002");
    assert_eq!(site.word_error_rate.reference_length, 10);
    assert_eq!(site.word_error_rate.substitutions, 1);
    assert_eq!(site.word_error_rate.insertions, 1);
    close(site.word_error_rate.rate.expect("rate"), 0.2);

    // pub-news-003: 9 reference words, one substituted; 53 reference
    // characters, one substituted.
    let news = item("pub-news-003");
    assert_eq!(news.word_error_rate.reference_length, 9);
    assert_eq!(news.word_error_rate.substitutions, 1);
    close(news.word_error_rate.rate.expect("rate"), 1.0 / 9.0);
    assert_eq!(news.character_error_rate.reference_length, 53);
    assert_eq!(news.character_error_rate.substitutions, 1);
    close(news.character_error_rate.rate.expect("rate"), 1.0 / 53.0);

    // Pooled: 4 + 2 + 1 edits over 13 + 10 + 9 reference words.
    assert_eq!(report.metrics.word_error_rate.edits, 7);
    assert_eq!(report.metrics.word_error_rate.reference_length, 32);
    close(
        report.metrics.word_error_rate.rate.expect("rate"),
        7.0 / 32.0,
    );
}

#[test]
fn the_silence_fixture_exposes_the_hallucinated_tail() {
    let report = run("whisper.cpp").expect("run");
    let quiet = report
        .per_item
        .iter()
        .find(|entry| entry.item_id == "syn-quiet-001")
        .expect("item");
    let silence = quiet.silence_hallucination.expect("measurable");
    assert_eq!(silence.silent_ms, 15_000);
    assert_eq!(silence.words_in_silence, 4);
    assert_eq!(silence.hypothesis_words, 17);
    assert_eq!(silence.spans_with_content, 1);
    close(silence.words_per_silent_minute.expect("rate"), 16.0);
    let hallucinated: Vec<&str> = quiet
        .hallucinated_words
        .iter()
        .map(|word| word.text.as_str())
        .collect();
    assert_eq!(hallucinated, vec!["Thank", "you", "for", "watching."]);

    // Pooled silence comes only from the item that could be measured.
    let pooled = report.metrics.silence_hallucination.expect("pooled");
    assert_eq!(pooled.spans, 1);
    assert_eq!(pooled.words_in_silence, 4);
}

#[test]
fn an_unavailable_alignment_yields_no_timestamp_and_no_silence_metric() {
    let report = run("whisper.cpp").expect("run");
    let news = report
        .per_item
        .iter()
        .find(|entry| entry.item_id == "pub-news-003")
        .expect("item");
    assert_eq!(news.alignment_origin, HypothesisAlignment::Unavailable);
    assert!(
        news.timestamp_error.is_none(),
        "an unavailable alignment must not report a timestamp error of any value"
    );
    assert!(
        news.silence_hallucination.is_none(),
        "silence cannot be judged without word times"
    );
    assert_eq!(news.hallucinated_words, Vec::new());
}

#[test]
fn timestamp_error_pools_only_the_aligned_items() {
    let report = run("whisper.cpp").expect("run");
    // syn-quiet-001 boundaries 0/2000/4000 against 0/2050/4120/6000 -> 0, 50, 120.
    // syn-site-002 boundaries 0/3000 against 0/3200 -> 0, 200.
    let error = report.metrics.timestamp_error.expect("two aligned items");
    assert_eq!(error.compared_boundaries, 5);
    assert_eq!(error.max_abs_ms, 200);
    close(error.mean_abs_ms, 370.0 / 5.0);
    close(error.median_abs_ms, 50.0);
}

#[test]
fn domain_terms_and_numerics_are_scored_per_term() {
    let report = run("whisper.cpp").expect("run");
    // rebar + cubic yards (both kept) and Baustelle (kept) and contractor (lost).
    assert_eq!(report.metrics.domain_term_accuracy.expected, 5);
    assert_eq!(report.metrics.domain_term_accuracy.matched, 4);
    close(
        report.metrics.domain_term_accuracy.recall.expect("recall"),
        0.8,
    );
    assert_eq!(
        report.metrics.domain_term_accuracy.absent_from_reference,
        Vec::<String>::new()
    );

    // Each item states a date and one further figure; all survive.
    assert_eq!(report.metrics.numeric_accuracy.expected, 6);
    assert_eq!(report.metrics.numeric_accuracy.matched, 6);
    assert_eq!(report.metrics.numeric_accuracy.invented, Vec::new());
}

#[test]
fn runtime_figures_are_pooled_from_recorded_inputs_only() {
    let report = run("whisper.cpp").expect("run");
    let runtime = report.metrics.runtime;
    assert_eq!(runtime.items_with_runtime, 2);
    assert_eq!(runtime.items_without_runtime, 1);
    assert_eq!(runtime.total_audio_ms, 660_000);
    assert_eq!(runtime.total_wall_clock_ms, 270_000);
    close(
        runtime.real_time_factor.expect("rtf"),
        270_000.0 / 660_000.0,
    );
    assert_eq!(runtime.time_to_first_result_samples, 2);
    close(runtime.mean_time_to_first_result_ms.expect("ttfr"), 2_000.0);
    assert_eq!(runtime.peak_rss_bytes, Some(629_145_600));
    assert_eq!(
        runtime.metered_cost_micro_usd, None,
        "a local run declares no metered cost"
    );
}

#[test]
fn a_second_engine_in_the_same_directory_is_scored_separately() {
    let report = run("apple.speech").expect("run");
    assert_eq!(report.engine.engine_id, "apple.speech");
    assert_eq!(report.engine.model_id, "SFSpeechRecognizer");
    assert_eq!(report.corpus.items_evaluated, 1);
    assert_eq!(
        report.corpus.missing_hypotheses,
        vec!["syn-site-002".to_owned(), "pub-news-003".to_owned()]
    );
    assert_eq!(report.metrics.word_error_rate.edits, 0);
    assert!(report.metrics.timestamp_error.is_none());
}

#[test]
fn the_run_is_deterministic_and_offline() {
    let first = run("whisper.cpp").expect("run");
    let second = run("whisper.cpp").expect("run");
    assert_eq!(first.digest(), second.digest());
    assert_eq!(
        first.to_json().expect("json"),
        second.to_json().expect("json")
    );
    assert_eq!(first.render_text(), second.render_text());
}

#[test]
fn the_report_carries_the_frozen_threshold_section_even_while_deferred() {
    let report = run("whisper.cpp").expect("run");
    assert_eq!(report.frozen_thresholds.spec_reference, "17.6");
    assert_eq!(report.frozen_thresholds.thresholds, Vec::new());
    assert_eq!(report.evaluate_thresholds(), Vec::new());
    assert!(report.render_text().contains("frozen thresholds:"));
}
