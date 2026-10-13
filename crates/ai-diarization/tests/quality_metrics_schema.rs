//! The emitted quality-metrics document conforms to its published schema
//! (M2.4, spec 17.4, 18.8).
//!
//! This is the validation step `scripts/run-quality-metrics.sh` runs before a
//! metrics document is accepted into the release evidence bundle, so it lives
//! in the repository rather than in the script: a schema check that depends on
//! an optional Python module is a check that silently stops running.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// The emitter built for this test run.
///
/// `CARGO_BIN_EXE_<name>` is cargo's own answer and the only one that stays
/// right when the artifact layout changes: deriving the path from
/// `current_exe()` assumed `<profile>/deps/<test>`, which stopped holding the
/// moment the test was compiled under a build-directory layout (the coverage
/// run on nightly puts test binaries in `<profile>/build/<pkg>/<hash>/out/`).
fn emitter() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_quality_metrics"))
}

fn schema() -> jsonschema::Validator {
    let text =
        std::fs::read_to_string(workspace_root().join("schemas/quality-metrics.v1.schema.json"))
            .expect("schema file");
    let value: serde_json::Value = serde_json::from_str(&text).expect("schema parses");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&value)
        .expect("schema compiles")
}

fn emit(extra: &[&str]) -> (serde_json::Value, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("quality-metrics.json");
    let root = workspace_root();
    let output = Command::new(emitter())
        .arg("--out")
        .arg(&out)
        .arg("--fixtures")
        .arg(root.join("tests/fixtures/diarization"))
        .args(extra)
        .current_dir(&root)
        .output()
        .expect("run emitter");
    assert!(
        output.status.success(),
        "emitter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("output file"))
            .expect("output is JSON");
    (
        document,
        String::from_utf8_lossy(&output.stdout).to_string(),
    )
}

#[test]
fn the_emitted_document_validates_against_the_published_schema() {
    let (document, stdout) = emit(&[]);
    assert!(stdout.is_empty(), "the emitter must not write to stdout");

    let validator = schema();
    let errors: Vec<String> = validator
        .iter_errors(&document)
        .map(|error| format!("{}: {error}", error.instance_path))
        .collect();
    assert!(errors.is_empty(), "schema violations: {errors:#?}");
}

#[test]
fn an_absent_corpus_is_reported_as_skipped_and_still_validates() {
    let (document, _) = emit(&[]);
    assert_eq!(document["tool_quality"]["status"], "skipped");
    assert_eq!(document["tool_quality"]["reason"], "corpus_not_available");
    assert!(
        document["tool_quality"]
            .get("factual_correctness")
            .is_none(),
        "a skipped block must not carry scores"
    );
    assert!(schema().is_valid(&document));
}

#[test]
fn every_fixture_is_reported_with_its_own_sample_sizes() {
    let (document, _) = emit(&[]);
    let diarization = &document["diarization"];
    assert_eq!(diarization["status"], "measured");

    let fixtures = diarization["fixtures"].as_array().expect("fixtures array");
    assert_eq!(
        usize::try_from(diarization["fixture_count"].as_u64().unwrap()).unwrap(),
        fixtures.len()
    );
    assert!(fixtures.len() >= 3, "expected the committed fixture set");

    for fixture in fixtures {
        assert!(fixture["report"]["total_reference_ms"].as_u64().unwrap() > 0);
        assert!(fixture["report"]["scored_frames"].as_u64().unwrap() > 0);
        assert_ne!(fixture["provenance"].as_str().unwrap(), "");
    }

    // At least one fixture must actually contain simultaneous speech, or the
    // measurement says nothing about overlap handling (spec 17.4).
    assert!(
        fixtures
            .iter()
            .any(|f| f["report"]["overlapping_reference_ms"].as_u64().unwrap() > 0),
        "no fixture exercises overlapping speech"
    );
    assert!(
        diarization["aggregate"]["overlapping_reference_ms"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn the_aggregate_components_sum_to_the_reported_rate() {
    let (document, _) = emit(&[]);
    let aggregate = &document["diarization"]["aggregate"];
    let total = aggregate["total_reference_ms"].as_f64().unwrap();
    let components = aggregate["missed_ms"].as_f64().unwrap()
        + aggregate["false_alarm_ms"].as_f64().unwrap()
        + aggregate["confusion_ms"].as_f64().unwrap();
    let der = aggregate["der"].as_f64().unwrap();
    assert!((der - components / total).abs() < 1e-9, "{aggregate}");
}

#[test]
fn provenance_is_complete_so_a_number_is_attributable() {
    let (document, _) = emit(&[]);
    let provenance = &document["provenance"];
    for field in [
        "hardware",
        "os_build",
        "toolchain",
        "commit",
        "fixture_source",
    ] {
        let value = provenance[field].as_str().unwrap_or("");
        assert!(!value.is_empty(), "provenance.{field} is empty");
    }
    assert_eq!(document["schema_version"], 1);
}

#[test]
fn an_opt_in_corpus_produces_real_scores_rather_than_nulls() {
    let dir = tempfile::tempdir().expect("temp dir");
    let corpus_path = dir.path().join("tool-fixture.json");
    std::fs::write(
        &corpus_path,
        serde_json::json!({
            "corpus": {
                "name": "fixture-lecture",
                "provenance": "synthetic, authored for this test",
                "locale": "en-US",
                "key_points": [
                    { "id": "kp1",
                      "statement": "Idempotency means the same request can be retried without duplicating work" },
                    { "id": "kp2", "statement": "The migration ships on Friday" }
                ]
            },
            "segments": [
                "We will ship the migration on Friday.",
                "Idempotency means the same request can be retried without duplicating work."
            ],
            "artifacts": {
                "flashcards": [{
                    "front": "What does idempotency mean?",
                    "back": "The same request can be retried without duplicating work.",
                    "anchors": [{ "seq_index": 1, "char_start": 0, "char_end": 11, "excerpt": "Idempotency" }]
                }],
                "quiz": [{
                    "question": "When does the migration ship?",
                    "answer": "Friday",
                    "anchors": [{ "seq_index": 0, "char_start": 0, "char_end": 7, "excerpt": "We will" }]
                }]
            }
        })
        .to_string(),
    )
    .expect("write fixture");

    let (document, _) = emit(&["--tool-corpus", corpus_path.to_str().unwrap()]);
    let tool_quality = &document["tool_quality"];
    assert_eq!(tool_quality["status"], "measured");
    assert_eq!(tool_quality["corpus"]["name"], "fixture-lecture");
    assert_eq!(tool_quality["corpus"]["key_point_count"], 2);

    for metric in ["factual_correctness", "coverage", "grounding_precision"] {
        let value = tool_quality[metric]["value"].as_f64();
        assert!(value.is_some(), "{metric} was not scored");
        assert!(tool_quality[metric]["sample_size"].as_u64().unwrap() > 0);
    }
    assert_eq!(tool_quality["grounding_precision"]["value"], 1.0);
    assert!(schema().is_valid(&document), "{document}");
}

#[test]
fn a_corpus_without_its_transcript_and_artifacts_is_skipped_not_half_measured() {
    let dir = tempfile::tempdir().expect("temp dir");
    let corpus_path = dir.path().join("bare-corpus.json");
    std::fs::write(
        &corpus_path,
        serde_json::json!({
            "name": "fixture-lecture",
            "provenance": "synthetic",
            "locale": "en-US",
            "key_points": []
        })
        .to_string(),
    )
    .expect("write corpus");

    let (document, _) = emit(&["--tool-corpus", corpus_path.to_str().unwrap()]);
    assert_eq!(document["tool_quality"]["status"], "skipped");
    assert!(document["tool_quality"]["reason"]
        .as_str()
        .unwrap()
        .starts_with("corpus_incomplete"));
    assert!(schema().is_valid(&document));
}
