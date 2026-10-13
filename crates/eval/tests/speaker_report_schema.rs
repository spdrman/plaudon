//! The committed speaker-match artifact schema must accept what the Rust
//! types write, reject what they never write, and stay stable (VP-180,
//! ADR-0081 §4).
//!
//! The stability check is a content hash of the schema file: changing the
//! published artifact shape means bumping `schema_version`, adding
//! `schemas/speaker-match-report.v2.schema.json` and updating the digest here
//! in the same commit — the same discipline as the evaluation report.

use std::path::{Path, PathBuf};

use plaudon_domain::media::Sha256Digest;
use plaudon_eval::speaker::embeddings::{
    ScriptedEmbeddings, DEFAULT_SCRIPTED_DIM, DEFAULT_SCRIPTED_SEED,
};
use plaudon_eval::speaker::harness::{self, HarnessPlan, RunMetadata};
use plaudon_eval::speaker::SpeakerMatchReport;

/// SHA-256 of `schemas/speaker-match-report.v1.schema.json`.
const COMMITTED_SCHEMA_SHA256: &str =
    "e3a20c64cb827b78a9a957630f4619b4daaa6a495dfc36c63ccbbbd8df639092";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn schema_path() -> PathBuf {
    repo_root().join("schemas/speaker-match-report.v1.schema.json")
}

fn validator() -> jsonschema::Validator {
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schema_path()).expect("read schema"))
            .expect("parse schema");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("the schema must compile")
}

fn errors(instance: &serde_json::Value) -> Vec<String> {
    validator()
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect()
}

fn report() -> SpeakerMatchReport {
    let document =
        std::fs::read_to_string(repo_root().join("tests/fixtures/speaker-eval/harness-plan.json"))
            .expect("read the committed plan");
    let plan = HarnessPlan::parse(&document).expect("the committed plan parses");
    let source = ScriptedEmbeddings::seeded(DEFAULT_SCRIPTED_SEED, DEFAULT_SCRIPTED_DIM);
    harness::evaluate(&plan, &source, &RunMetadata::default()).expect("a scripted pass")
}

#[test]
fn the_schema_file_has_not_changed_without_a_version_bump() {
    let bytes = std::fs::read(schema_path()).expect("read schema");
    assert_eq!(
        Sha256Digest::of(&bytes).to_hex(),
        COMMITTED_SCHEMA_SHA256,
        "the published artifact shape changed: bump schema_version, add a v2 schema and update \
         this digest in the same commit"
    );
}

#[test]
fn a_report_the_harness_produces_satisfies_the_schema() {
    let instance = serde_json::to_value(report()).expect("serialize");
    let errors = errors(&instance);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn the_committed_artifact_satisfies_the_schema() {
    let committed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            repo_root().join("docs/benchmarks/speaker-match-scripted-synthetic-eer.json"),
        )
        .expect("the committed artifact exists"),
    )
    .expect("valid JSON");
    let errors = errors(&committed);
    assert!(errors.is_empty(), "{errors:?}");
}

/// Every committed artifact, not just the scripted one.
///
/// `docs/benchmarks/` gained its first real measurement in P2.5
/// (`speaker-match-wespeaker-resnet34-lm-eer.json`), and a measured artifact is
/// exactly the document that will be quoted: it has to satisfy the published
/// shape, carry the standing caveats and name the platform its runtime figures
/// came from.
#[test]
fn every_committed_speaker_match_artifact_satisfies_the_schema() {
    let directory = repo_root().join("docs/benchmarks");
    let mut checked = 0;
    for entry in std::fs::read_dir(&directory)
        .expect("docs/benchmarks")
        .flatten()
    {
        let path = entry.path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !name.starts_with("speaker-match-") || !name.ends_with("-eer.json") {
            continue;
        }
        let committed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read artifact"))
                .unwrap_or_else(|error| panic!("{name} is not valid JSON: {error}"));
        let errors = errors(&committed);
        assert!(errors.is_empty(), "{name}: {errors:?}");

        // A measurement of a real checkpoint must say which machine produced
        // its runtime figures; ADR-0081 §4 is a per-platform gate.
        if committed["model"]["embedding_source"] == serde_json::json!("model_pack") {
            let runtime = &committed["runtime"];
            assert!(
                runtime["platform"].is_string()
                    && runtime["peak_rss_bytes"].is_number()
                    && runtime["startup_ms"].is_number()
                    && runtime["real_time_factor"].is_number(),
                "{name} measures a checkpoint but records no complete runtime footprint"
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected the scripted artifact and at least one measured one, saw {checked}"
    );
}

#[test]
fn the_schema_refuses_a_document_that_hides_what_it_measured() {
    let mut instance = serde_json::to_value(report()).expect("serialize");
    instance
        .as_object_mut()
        .expect("object")
        .remove("release_evidence");
    assert!(
        !errors(&instance).is_empty(),
        "an artifact without its release-evidence verdict is not publishable"
    );

    let mut instance = serde_json::to_value(report()).expect("serialize");
    instance["model"]["embedding_source"] = serde_json::json!("real_enough");
    assert!(
        !errors(&instance).is_empty(),
        "there are two embedding sources, and inventing a third is not how a claim is upgraded"
    );

    let mut instance = serde_json::to_value(report()).expect("serialize");
    instance["caveats"] = serde_json::json!([]);
    assert!(
        !errors(&instance).is_empty(),
        "the standing caveats travel with the numbers (VP-170, VP-173)"
    );
}

#[test]
fn the_schema_refuses_an_embedding_smuggled_into_an_artifact() {
    let mut instance = serde_json::to_value(report()).expect("serialize");
    instance
        .as_object_mut()
        .expect("object")
        .insert("exemplars".to_owned(), serde_json::json!([[0.1, 0.2, 0.3]]));
    assert!(
        !errors(&instance).is_empty(),
        "the artifact is closed: a vector cannot be added to it (VP-002, VP-112)"
    );
}
