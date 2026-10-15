//! Drives the crash-harness worker binary end to end (SPEC v1.3 §14.6).
//!
//! The chaos suite normally runs this binary with a crash point armed, so the
//! process dies mid-scenario. Here it is run *unarmed*: every scenario must run
//! to completion and report the job it drove, and every argument error must be
//! refused with the documented exit code instead of a panic.
//!
//! Only compiled with the `fault-injection` feature, which is what builds the
//! binary in the first place.
#![cfg(feature = "fault-injection")]

use std::path::Path;
use std::process::{Command, Output};

const WORKER: &str = env!("CARGO_BIN_EXE_plaudon-chaos-worker");

struct Workspace {
    _dir: tempfile::TempDir,
    db: std::path::PathBuf,
    staging: std::path::PathBuf,
    archive: std::path::PathBuf,
}

fn workspace() -> Workspace {
    let dir = tempfile::tempdir().expect("tempdir");
    Workspace {
        db: dir.path().join("app.sqlite"),
        staging: dir.path().join("temp/imports"),
        archive: dir.path().join("library"),
        _dir: dir,
    }
}

fn run(workspace: &Workspace, scenario: &str, seed: &str) -> Output {
    run_args(&[
        "--db",
        &workspace.db.to_string_lossy(),
        "--staging",
        &workspace.staging.to_string_lossy(),
        "--archive",
        &workspace.archive.to_string_lossy(),
        "--scenario",
        scenario,
        "--seed",
        seed,
    ])
}

fn run_args(args: &[&str]) -> Output {
    Command::new(WORKER)
        .args(args)
        // The worker must never arm a crash point of its own accord.
        .env_remove("PLAUDON_CRASH_POINT")
        .env_remove("PLAUDON_CRASH_OCCURRENCE")
        .env("PLAUDON_LOCAL_ONLY", "1")
        .output()
        .expect("run the chaos worker")
}

#[test]
fn every_scenario_runs_to_completion_when_no_crash_point_is_armed() {
    for (index, scenario) in SCENARIOS.iter().enumerate() {
        let workspace = workspace();
        let output = run(&workspace, scenario, &index.to_string());
        assert!(
            output.status.success(),
            "{scenario} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("PLAUDON_WORKER_FINISHED") && stdout.contains(scenario),
            "{scenario} did not report completion: {stdout}"
        );
        assert!(workspace.db.exists(), "{scenario} left no database behind");
    }
}

/// Every scenario the worker understands, mirroring its own `SCENARIOS` table.
const SCENARIOS: &[&str] = &[
    "import_happy_path",
    "import_with_journal",
    "cloud_dispatch_no_response",
    "cloud_dispatch_with_response",
    "cancel_mid_flight",
    "suspend_mid_stage",
    "consent_revoked_mid_queue",
];

#[test]
fn the_default_scenario_runs_when_none_is_named() {
    let workspace = workspace();
    let output = run_args(&[
        "--db",
        &workspace.db.to_string_lossy(),
        "--staging",
        &workspace.staging.to_string_lossy(),
        "--archive",
        &workspace.archive.to_string_lossy(),
    ]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("import_happy_path"));
}

#[test]
fn every_required_argument_is_enforced_before_any_work_happens() {
    let workspace = workspace();
    let db = workspace.db.to_string_lossy().to_string();
    let staging = workspace.staging.to_string_lossy().to_string();
    let archive = workspace.archive.to_string_lossy().to_string();

    let cases: Vec<(Vec<&str>, &str)> = vec![
        (Vec::new(), "--db"),
        (vec!["--db", &db], "--staging"),
        (vec!["--db", &db, "--staging", &staging], "--archive"),
        (
            vec![
                "--db",
                &db,
                "--staging",
                &staging,
                "--archive",
                &archive,
                "--scenario",
                "teleport",
            ],
            "unknown scenario",
        ),
    ];

    for (args, expected) in cases {
        let output = run_args(&args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} must be refused with the argument exit code"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?} must print no result");
    }
}

#[test]
fn a_staging_root_that_cannot_be_created_is_refused() {
    let workspace = workspace();
    let blocker = workspace.db.with_file_name("blocker");
    std::fs::write(&blocker, b"regular file").expect("blocker");
    let staging = blocker.join("imports");

    let output = run_args(&[
        "--db",
        &workspace.db.to_string_lossy(),
        "--staging",
        &staging.to_string_lossy(),
        "--archive",
        &workspace.archive.to_string_lossy(),
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("staging dir"));
}

#[test]
fn an_archive_root_that_cannot_be_created_is_refused() {
    let workspace = workspace();
    let blocker = workspace.db.with_file_name("blocker");
    std::fs::write(&blocker, b"regular file").expect("blocker");
    let archive = blocker.join("library");

    let output = run_args(&[
        "--db",
        &workspace.db.to_string_lossy(),
        "--staging",
        &workspace.staging.to_string_lossy(),
        "--archive",
        &archive.to_string_lossy(),
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("archive dir"));
}

#[test]
fn a_database_path_that_cannot_be_opened_fails_the_run_without_a_result() {
    let workspace = workspace();
    let blocker = workspace.db.with_file_name("blocker");
    std::fs::write(&blocker, b"regular file").expect("blocker");

    let output = run_args(&[
        "--db",
        &blocker.join("app.sqlite").to_string_lossy(),
        "--staging",
        &workspace.staging.to_string_lossy(),
        "--archive",
        &workspace.archive.to_string_lossy(),
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("PLAUDON_WORKER_FAILED"));
    assert!(
        output.stdout.is_empty(),
        "a failed run must print no completion marker"
    );
}

#[test]
fn the_worker_binary_is_where_cargo_says_it_is() {
    assert!(Path::new(WORKER).exists(), "{WORKER}");
}
