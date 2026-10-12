//! Tests for the quality_metrics binary emitter (spec 17.4).

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn run_emitter(out_path: &std::path::Path, corpus_path: Option<&std::path::Path>) -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.args(["run", "-q", "--bin", "quality_metrics", "--"])
        .arg("--out")
        .arg(out_path);

    if let Some(corpus) = corpus_path {
        cmd.arg("--tool-corpus").arg(corpus);
    }

    let output = cmd.output().expect("Failed to run quality_metrics binary");

    // Verify stdout is empty
    assert!(
        output.stdout.is_empty(),
        "Binary should print nothing to stdout, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    // Verify exit code
    assert!(
        output.status.success(),
        "Binary should exit successfully, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify file was written
    assert!(
        out_path.exists(),
        "Output file should exist at {}",
        out_path.display()
    );

    // Load and parse the output
    let content = std::fs::read_to_string(out_path).expect("Failed to read output file");
    serde_json::from_str(&content).expect("Output should be valid JSON")
}

#[test]
fn emitter_writes_valid_document_without_corpus() {
    let temp_dir = tempfile::tempdir().unwrap();
    let out_path = temp_dir.path().join("metrics.json");

    let doc = run_emitter(&out_path, None);

    // Verify schema_version
    assert_eq!(
        doc["schema_version"].as_u64(),
        Some(1),
        "schema_version must be 1"
    );

    // Verify generated_at is present and non-empty
    assert!(
        doc["generated_at"].is_string(),
        "generated_at must be a string"
    );
    let generated_at = doc["generated_at"].as_str().unwrap();
    assert!(!generated_at.is_empty(), "generated_at must not be empty");
    assert!(
        generated_at.contains('T') && generated_at.contains('Z'),
        "generated_at should be RFC3339 format"
    );

    // Verify provenance fields
    assert!(doc["provenance"].is_object(), "provenance must be an object");
    for field in ["hardware", "os_build", "toolchain", "commit", "fixture_source"] {
        assert!(
            doc["provenance"][field].is_string(),
            "provenance.{} must be a string",
            field
        );
        assert!(
            !doc["provenance"][field].as_str().unwrap().is_empty(),
            "provenance.{} must not be empty",
            field
        );
    }

    // Verify fixture_source mentions the fixtures directory
    assert!(
        doc["provenance"]["fixture_source"]
            .as_str()
            .unwrap()
            .contains("fixtures/diarization"),
        "fixture_source should mention the fixtures directory"
    );

    // Verify diarization block
    assert!(
        doc["diarization"].is_object(),
        "diarization must be an object"
    );
    assert_eq!(
        doc["diarization"]["status"].as_str(),
        Some("measured"),
        "diarization.status should be 'measured'"
    );

    // Verify all DER fields are present
    for field in [
        "der",
        "missed_ms",
        "false_alarm_ms",
        "confusion_ms",
        "total_reference_ms",
        "overlapping_reference_ms",
    ] {
        assert!(
            doc["diarization"][field].is_number(),
            "diarization.{} must be a number",
            field
        );
    }

    // Verify total_reference_ms is non-zero (we have fixtures)
    assert!(
        doc["diarization"]["total_reference_ms"].as_u64().unwrap() > 0,
        "total_reference_ms should be non-zero with fixtures"
    );

    for field in [
        "reference_speaker_count",
        "hypothesis_cluster_count",
        "scored_frames",
        "excluded_frames",
    ] {
        assert!(
            doc["diarization"][field].is_number(),
            "diarization.{} must be a number",
            field
        );
    }

    assert!(
        doc["diarization"]["overlap_regions_skipped"].is_boolean(),
        "diarization.overlap_regions_skipped must be boolean"
    );

    // Verify tool_quality without corpus
    assert!(
        doc["tool_quality"].is_object(),
        "tool_quality must be an object"
    );
    assert_eq!(
        doc["tool_quality"]["status"].as_str(),
        Some("skipped"),
        "tool_quality.status should be 'skipped' when no corpus provided"
    );
    assert!(
        doc["tool_quality"]["reason"].is_string(),
        "tool_quality.reason must be a string when skipped"
    );
    assert_eq!(
        doc["tool_quality"]["reason"].as_str(),
        Some("corpus_not_available"),
        "tool_quality.reason should indicate corpus not available"
    );
}

#[test]
fn emitter_validates_against_schema() {
    let schema_path = PathBuf::from("schemas/quality-metrics.v1.schema.json");
    if !schema_path.exists() {
        // Skip if schema doesn't exist (it should in CI)
        eprintln!(
            "Skipping schema validation: {} not found",
            schema_path.display()
        );
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let out_path = temp_dir.path().join("metrics.json");

    let doc = run_emitter(&out_path, None);

    // Load and compile schema
    let schema_content =
        std::fs::read_to_string(&schema_path).expect("Failed to read schema file");
    let schema_json: Value =
        serde_json::from_str(&schema_content).expect("Schema should be valid JSON");

    let schema = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema_json)
        .expect("Schema should compile");

    let errors: Vec<String> = schema
        .iter_errors(&doc)
        .map(|e| e.to_string())
        .collect();
    
    if !errors.is_empty() {
        panic!(
            "Document does not validate against schema:\n{}",
            errors.join("\n")
        );
    }
}

#[test]
fn emitter_aggregates_multiple_fixtures() {
    let temp_dir = tempfile::tempdir().unwrap();
    let out_path = temp_dir.path().join("metrics.json");

    let doc = run_emitter(&out_path, None);

    // We know we have three fixtures: two-speaker-clean, three-speaker-overlap, single-speaker-silence
    // Their expected total_reference_ms: 2000 + 3000 + 1000 = 6000
    assert_eq!(
        doc["diarization"]["total_reference_ms"].as_u64(),
        Some(6000),
        "Should aggregate all fixtures"
    );

    // Overlapping: 0 + 1000 + 0 = 1000
    assert_eq!(
        doc["diarization"]["overlapping_reference_ms"].as_u64(),
        Some(1000),
        "Should aggregate overlapping speech"
    );

    // False alarms: 0 + 0 + 1000 = 1000
    assert_eq!(
        doc["diarization"]["false_alarm_ms"].as_u64(),
        Some(1000),
        "Should aggregate false alarms"
    );

    // DER: 1000 / 6000 ≈ 0.1667
    let der = doc["diarization"]["der"].as_f64().unwrap();
    assert!(
        (der - 1000.0 / 6000.0).abs() < 1e-9,
        "DER should be aggregated correctly, got {}",
        der
    );
}
