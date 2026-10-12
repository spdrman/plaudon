//! `plaudon-eval` command-line behaviour (spec 17.1, 17.2, 17.6).
//!
//! The binary is exercised as a process so that exit codes, which are what CI
//! and the release checklist act on, are part of the contract. Every run is
//! offline: the child process is given an explicit environment, so a stray
//! opt-in variable on the developer's machine cannot enable cloud evaluation.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use plaudon_eval::corpus::CorpusManifest;

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

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plaudon-eval"));
    command.env_clear();
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A manifest holding only the consent-cleared fixture rows, written to a
/// temporary directory with absolute reference paths so it can act as a golden
/// corpus without touching the repository.
fn golden_manifest(dir: &Path) -> PathBuf {
    let mut manifest = CorpusManifest::load(&fixtures().join("corpus.yaml")).expect("manifest");
    manifest
        .items
        .retain(plaudon_eval::corpus::CorpusItem::is_cleared);
    for item in &mut manifest.items {
        item.reference_transcript = fixtures().join(&item.reference_transcript);
    }
    let path = dir.join("golden.yaml");
    std::fs::write(&path, serde_norway::to_string(&manifest).expect("yaml")).expect("write");
    path
}

fn run_report(dir: &Path, manifest: &Path) -> PathBuf {
    let out = dir.join("report.json");
    let output = binary()
        .args([
            "run",
            "--manifest",
            manifest.to_str().expect("utf-8"),
            "--engine",
            "whisper.cpp",
            "--hypotheses",
            fixtures().join("hypotheses").to_str().expect("utf-8"),
            "--out",
            out.to_str().expect("utf-8"),
            "--host-cpu",
            "fixture-cpu",
            "--os-version",
            "fixture-os",
            "--commit",
            "0000000000000000000000000000000000000000",
            "--measured-on",
            "2026-05-29T00:00:00Z",
        ])
        .output()
        .expect("run");
    assert!(output.status.success(), "run failed: {}", stderr(&output));
    out
}

#[test]
fn validate_corpus_names_the_refused_row_and_fails() {
    let output = binary()
        .args([
            "validate-corpus",
            fixtures().join("corpus.yaml").to_str().expect("utf-8"),
        ])
        .output()
        .expect("validate");
    let text = stdout(&output);
    assert!(
        text.contains("4 item(s) declared, 3 cleared, 1 refused"),
        "{text}"
    );
    assert!(
        text.contains("REFUSED  unl-draft-004: no license declared"),
        "{text}"
    );
    assert!(
        text.contains(
            "REFUSED  unl-draft-004: consent status `unverified` does not grant clearance"
        ),
        "{text}"
    );
    assert!(text.contains("cleared  syn-quiet-001"), "{text}");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a corpus with a refused row must not exit zero"
    );
}

#[test]
fn validate_corpus_accepts_a_fully_cleared_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let output = binary()
        .args(["validate-corpus", manifest.to_str().expect("utf-8")])
        .output()
        .expect("validate");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("3 item(s) declared, 3 cleared, 0 refused"));
}

#[test]
fn validate_corpus_reports_a_broken_manifest_instead_of_panicking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("broken.yaml");
    std::fs::write(&path, "schema_version: 9\ncorpus_id: x\n").expect("write");
    let output = binary()
        .args(["validate-corpus", path.to_str().expect("utf-8")])
        .output()
        .expect("validate");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).starts_with("error: "),
        "{}",
        stderr(&output)
    );
}

#[test]
fn run_writes_a_report_and_renders_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let out = run_report(dir.path(), &manifest);
    let first = std::fs::read_to_string(&out).expect("report");
    assert!(first.contains("\"schema_version\": 1"));

    // Re-running the same inputs produces the same bytes.
    let second_out = run_report(dir.path(), &manifest);
    assert_eq!(first, std::fs::read_to_string(second_out).expect("report"));
}

#[test]
fn run_refuses_cloud_material_without_the_opt_in_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let output = binary()
        .args([
            "run",
            "--manifest",
            manifest.to_str().expect("utf-8"),
            "--engine",
            "whisper.cpp",
            "--hypotheses",
            fixtures().join("hypotheses").to_str().expect("utf-8"),
            "--out",
            dir.path().join("cloud.json").to_str().expect("utf-8"),
            "--allow-cloud",
        ])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("PLAUDON_EVAL_CLOUD_OPT_IN"),
        "{}",
        stderr(&output)
    );
    assert!(!dir.path().join("cloud.json").exists());
}

#[test]
fn run_refuses_cloud_material_without_a_cost_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let output = binary()
        .env("PLAUDON_EVAL_CLOUD_OPT_IN", "1")
        .args([
            "run",
            "--manifest",
            manifest.to_str().expect("utf-8"),
            "--engine",
            "whisper.cpp",
            "--hypotheses",
            fixtures().join("hypotheses").to_str().expect("utf-8"),
            "--out",
            dir.path().join("cloud.json").to_str().expect("utf-8"),
            "--allow-cloud",
        ])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("PLAUDON_EVAL_COST_CAP_MICRO_USD"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn gate_grants_recommended_against_the_golden_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let report = run_report(dir.path(), &manifest);
    let output = binary()
        .args([
            "gate",
            "--report",
            report.to_str().expect("utf-8"),
            "--claim",
            "recommended",
            "--manifest",
            manifest.to_str().expect("utf-8"),
            "--as-of",
            "2026-06-01T00:00:00Z",
        ])
        .output()
        .expect("gate");
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("`recommended` granted on corpus"), "{text}");
    assert!(text.contains("3 items"), "{text}");
}

#[test]
fn gate_refuses_recommended_for_a_model_the_record_does_not_cover() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let report = run_report(dir.path(), &manifest);
    let output = binary()
        .args([
            "gate",
            "--report",
            report.to_str().expect("utf-8"),
            "--claim",
            "recommended",
            "--manifest",
            manifest.to_str().expect("utf-8"),
            "--model",
            "ggml-large-v3",
            "--as-of",
            "2026-06-01T00:00:00Z",
        ])
        .output()
        .expect("gate");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("stored record is for model `ggml-small-q5_1`"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn gate_refuses_a_stale_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let report = run_report(dir.path(), &manifest);
    let output = binary()
        .args([
            "gate",
            "--report",
            report.to_str().expect("utf-8"),
            "--claim",
            "recommended",
            "--manifest",
            manifest.to_str().expect("utf-8"),
            "--as-of",
            "2027-06-01T00:00:00Z",
        ])
        .output()
        .expect("gate");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("freshness window"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn gate_requires_the_golden_corpus_for_a_recommendation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let report = run_report(dir.path(), &manifest);
    let output = binary()
        .args([
            "gate",
            "--report",
            report.to_str().expect("utf-8"),
            "--claim",
            "recommended",
        ])
        .output()
        .expect("gate");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("--manifest (the golden corpus) is required"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn gate_grants_supported_without_any_record_and_claims_no_quality() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = golden_manifest(dir.path());
    let report = run_report(dir.path(), &manifest);
    let output = binary()
        .args([
            "gate",
            "--report",
            report.to_str().expect("utf-8"),
            "--claim",
            "supported",
        ])
        .output()
        .expect("gate");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("asserts nothing about quality"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn an_unknown_option_is_a_usage_error() {
    let output = binary()
        .args(["validate-corpus", "--pretty", "x"])
        .output()
        .expect("validate");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("unknown option --pretty"));

    let unknown = binary().args(["summarise"]).output().expect("unknown");
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("unknown command `summarise`"));
}
