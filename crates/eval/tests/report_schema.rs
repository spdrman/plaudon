//! The committed evaluation-report schema must accept what the Rust types
//! write, reject what they never write, and stay stable (spec 6.6, 17.2, 18.2).
//!
//! The stability check is a content hash of the committed schema file. A
//! change to the published report shape must therefore be deliberate: bump
//! `schema_version`, add `schemas/evaluation-report.v2.schema.json` and update
//! the digest below in the same commit.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use plaudon_eval::corpus::CorpusManifest;
use plaudon_eval::hypothesis::HypothesisSet;
use plaudon_eval::report::{
    Comparator, EnvironmentRecord, EvaluationReport, FrozenThreshold, FrozenThresholds,
    ThresholdMetric, REPORT_SCHEMA_VERSION,
};
use plaudon_eval::runner::{self, RunContext};

/// SHA-256 of `schemas/evaluation-report.v1.schema.json`.
const COMMITTED_SCHEMA_SHA256: &str =
    "37df1eaeaff6a1dd75f19f060af0a93df1f26eadc893e7e495795fab79e08f61";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn schema_path() -> PathBuf {
    repo_root().join("schemas/evaluation-report.v1.schema.json")
}

fn schema() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(schema_path()).expect("read schema"))
        .expect("parse schema")
}

fn validator() -> jsonschema::Validator {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema())
        .expect("the schema must compile")
}

fn assert_valid(instance: &serde_json::Value) {
    let validator = validator();
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "report failed its schema: {errors:?}");
}

fn report() -> EvaluationReport {
    let fixtures = repo_root().join("tests/fixtures/eval");
    let manifest = CorpusManifest::load(&fixtures.join("corpus.yaml")).expect("manifest");
    let hypotheses = HypothesisSet::load(&fixtures.join("hypotheses")).expect("hypotheses");
    let context = RunContext::new(
        EnvironmentRecord {
            os: "linux".to_owned(),
            os_version: Some("6.1.0".to_owned()),
            cpu: Some("fixture-cpu".to_owned()),
            ram_bytes: Some(16 * 1024 * 1024 * 1024),
            commit: Some("0".repeat(40)),
            measured_on: Timestamp::from_millis(1_780_000_000_000),
        },
        "0.1.0",
    );
    runner::run(&manifest, &fixtures, &hypotheses, "whisper.cpp", &context).expect("run")
}

fn json(report: &EvaluationReport) -> serde_json::Value {
    serde_json::to_value(report).expect("serialize")
}

fn keys(value: &serde_json::Value) -> BTreeSet<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

fn required(schema: &serde_json::Value, pointer: &str) -> BTreeSet<String> {
    schema
        .pointer(pointer)
        .unwrap_or_else(|| panic!("schema has no node at {pointer}"))
        .as_array()
        .expect("required array")
        .iter()
        .map(|value| value.as_str().expect("string").to_owned())
        .collect()
}

#[test]
fn the_committed_schema_compiles_as_draft_2020_12() {
    let schema = schema();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["properties"]["schema_version"]["const"], 1);
    assert_eq!(u32::from(1u8), REPORT_SCHEMA_VERSION);
    let _ = validator();
}

#[test]
fn the_committed_schema_is_byte_stable() {
    let bytes = std::fs::read(schema_path()).expect("read schema");
    assert_eq!(
        Sha256Digest::of(&bytes).to_hex(),
        COMMITTED_SCHEMA_SHA256,
        "the published report shape changed; bump schema_version, publish a v2 schema and update this digest deliberately"
    );
}

#[test]
fn a_generated_report_satisfies_the_schema() {
    assert_valid(&json(&report()));
}

#[test]
fn a_report_with_frozen_thresholds_satisfies_the_schema() {
    let mut report = report();
    report.frozen_thresholds = FrozenThresholds::frozen(
        "example freeze used by the schema test only",
        vec![FrozenThreshold {
            metric: ThresholdMetric::WordErrorRate,
            comparator: Comparator::AtMost,
            value: 0.5,
            frozen_on: Some(Timestamp::from_millis(1_780_000_000_000)),
        }],
    )
    .in_evidence_bundle("release-0.1.0");
    assert_valid(&json(&report));
}

#[test]
fn the_schema_required_set_matches_what_the_rust_types_write() {
    let schema = schema();
    let instance = json(&report());
    assert_eq!(required(&schema, "/required"), keys(&instance));
    for (section, pointer) in [
        ("engine", "/properties/engine/required"),
        ("corpus", "/properties/corpus/required"),
        ("sample", "/properties/sample/required"),
        ("environment", "/properties/environment/required"),
        ("metrics", "/properties/metrics/required"),
        (
            "frozen_thresholds",
            "/properties/frozen_thresholds/required",
        ),
    ] {
        assert_eq!(
            required(&schema, pointer),
            keys(&instance[section]),
            "{section} section drifted from the schema"
        );
    }
    assert_eq!(
        required(&schema, "/properties/metrics/properties/runtime/required"),
        keys(&instance["metrics"]["runtime"])
    );
    assert_eq!(
        required(&schema, "/properties/per_item/items/required"),
        keys(&instance["per_item"][0])
    );
}

#[test]
fn the_schema_rejects_an_unknown_field() {
    let mut instance = json(&report());
    instance["overall_grade"] = serde_json::json!("A+");
    assert!(
        validator().iter_errors(&instance).next().is_some(),
        "an unknown report field must be rejected, not silently published"
    );
}

#[test]
fn the_schema_rejects_a_report_without_a_reference_quality_note() {
    let mut instance = json(&report());
    instance
        .as_object_mut()
        .expect("object")
        .remove("reference_quality");
    assert!(validator().iter_errors(&instance).next().is_some());

    let mut blank = json(&report());
    blank["reference_quality"] = serde_json::json!("");
    assert!(validator().iter_errors(&blank).next().is_some());
}

#[test]
fn the_schema_accepts_a_null_timestamp_error_and_rejects_an_unknown_alignment() {
    let mut instance = json(&report());
    instance["metrics"]["timestamp_error"] = serde_json::Value::Null;
    assert_valid(&instance);

    let mut wrong = json(&report());
    // `none` is the domain vocabulary; a report must say `unavailable`.
    wrong["per_item"][0]["alignment_origin"] = serde_json::json!("none");
    assert!(validator().iter_errors(&wrong).next().is_some());
}

#[test]
fn the_schema_rejects_an_unknown_exclusion_code() {
    let mut instance = json(&report());
    instance["corpus"]["excluded"][0]["reasons"][0]["code"] = serde_json::json!("probably_fine");
    assert!(validator().iter_errors(&instance).next().is_some());
}

#[test]
fn the_schema_refuses_thresholds_while_the_section_claims_to_be_deferred() {
    let mut instance = json(&report());
    instance["frozen_thresholds"]["thresholds"] = serde_json::json!([{
        "metric": "word_error_rate",
        "comparator": "at_most",
        "value": 0.1,
        "frozen_on": null
    }]);
    assert!(
        validator().iter_errors(&instance).next().is_some(),
        "a deferred section must not carry numbers"
    );
}
