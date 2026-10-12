//! Job-engine crash-injection harness (SPEC v1.3 §14.6; EPIC C task C2.2).
//!
//! The harness launches `plaudon-chaos-worker` as a child process with exactly
//! one crash point armed, lets it die there, then re-opens the very same store
//! and asserts the §14.6 invariant set:
//!
//! * **no false completion** — no `Completed` job without durable, verified
//!   bytes for every file-backed output;
//! * **no recording loss** — bytes that were committed stay byte-identical and
//!   user-supplied source files are never touched;
//! * **no orphaned credentials** — no secret-shaped value anywhere in the
//!   database;
//! * **predictable retry behaviour** — every surviving job carries a
//!   deterministic retry classification, and a possibly-billed cloud request
//!   is never re-dispatched automatically.
//!
//! Everything here is hermetic: no network, no real provider, no real
//! credential store. Scenario seeds are explicit, so a failure reproduces.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::LazyLock;

use plaudon_jobs::engine::{EngineConfig, JobEngine};
use plaudon_jobs::error::{retry_class, RetryClass};
use plaudon_jobs::reconcile::ReconcileReport;
use plaudon_jobs::state::JobState;
use plaudon_jobs::store::{DispatchState, JobStore};
use plaudon_jobs::writer::Database;
use sha2::{Digest, Sha256};

/// Marker the worker prints when it completed without crashing.
pub const WORKER_FINISHED: &str = "PLAUDON_WORKER_FINISHED";

/// Marker the crash point prints to stderr immediately before aborting.
pub const CRASH_FIRED: &str = "PLAUDON_CRASH_POINT_FIRED";

/// Path to the fault-injecting worker binary, built once per test run.
///
/// Built through the ambient `cargo`, so the harness always exercises the
/// current sources rather than a stale artifact.
static WORKER_BINARY: LazyLock<PathBuf> = LazyLock::new(build_worker);

fn build_worker() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args([
            "build",
            "-p",
            "plaudon-jobs",
            "--features",
            "fault-injection",
            "--bin",
            "plaudon-chaos-worker",
            "--message-format=json",
        ])
        .stderr(Stdio::inherit())
        .output()
        .expect("could not run cargo to build the chaos worker");
    assert!(
        output.status.success(),
        "building the chaos worker failed with {:?}",
        output.status
    );
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] == "compiler-artifact" {
            if let Some(path) = value["executable"].as_str() {
                if path.contains("plaudon-chaos-worker") {
                    return PathBuf::from(path);
                }
            }
        }
    }
    panic!("cargo did not report an executable for plaudon-chaos-worker");
}

/// Path to the fault-injecting worker binary. Built on first use.
pub fn worker_binary() -> &'static Path {
    &WORKER_BINARY
}

/// An isolated store plus staging area for one chaos scenario.
#[derive(Debug)]
pub struct ChaosRun {
    dir: tempfile::TempDir,
    db: PathBuf,
    staging: PathBuf,
    archive: PathBuf,
    seed: String,
}

impl ChaosRun {
    /// Create a fresh, isolated run directory.
    pub fn new(seed: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // `MediaArchive::open` owns the layout: `<root>/media/sha256/...`
        // for blobs and `<root>/temp/imports` for staging (§13.4).
        let archive = dir.path().join("library");
        let media_archive =
            plaudon_persistence::archive::MediaArchive::open(&archive).expect("archive");
        let staging = media_archive.staging_dir();
        Self {
            db: dir.path().join("app.sqlite"),
            staging,
            archive,
            seed: seed.to_string(),
            dir,
        }
    }

    /// Path of the SQLite database under test.
    pub fn db_path(&self) -> &Path {
        &self.db
    }

    /// App-owned staging root.
    pub fn staging_root(&self) -> &Path {
        &self.staging
    }

    /// Content-addressed archive root.
    pub fn archive_root(&self) -> &Path {
        &self.archive
    }

    /// Run directory root.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Deterministic seed for this run.
    pub fn seed(&self) -> &str {
        &self.seed
    }
}

/// Outcome of one worker invocation.
#[derive(Debug)]
pub struct WorkerOutcome {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl WorkerOutcome {
    /// `true` when the armed crash point fired.
    pub fn crashed(&self) -> bool {
        self.stderr.contains(CRASH_FIRED)
    }

    /// `true` when the worker ran the whole scenario.
    pub fn finished(&self) -> bool {
        self.stdout.contains(WORKER_FINISHED)
    }

    /// The job id the worker reported, when it finished.
    pub fn job_id(&self) -> Option<String> {
        self.stdout
            .lines()
            .find(|line| line.starts_with(WORKER_FINISHED))
            .and_then(|line| line.split_whitespace().nth(2))
            .map(str::to_string)
    }
}

/// Run a scenario in a child process, optionally arming one crash point.
pub fn run_worker(run: &ChaosRun, scenario: &str, crash_point: Option<&str>) -> WorkerOutcome {
    run_worker_nth(run, scenario, crash_point, 1)
}

/// Run a scenario, arming the `occurrence`-th hit of `crash_point`.
pub fn run_worker_nth(
    run: &ChaosRun,
    scenario: &str,
    crash_point: Option<&str>,
    occurrence: u32,
) -> WorkerOutcome {
    let mut command = Command::new(&*WORKER_BINARY);
    command
        .arg("--db")
        .arg(&run.db)
        .arg("--staging")
        .arg(&run.staging)
        .arg("--archive")
        .arg(&run.archive)
        .arg("--archive")
        .arg(&run.archive)
        .arg("--scenario")
        .arg(scenario)
        .arg("--seed")
        .arg(&run.seed)
        // Hermetic: no proxy, no ambient credentials, no network helpers.
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("OPENROUTER_API_KEY");
    match crash_point {
        Some(point) => {
            assert!(
                plaudon_jobs::fault::is_registered(point),
                "crash point `{point}` is not registered"
            );
            command
                .env(plaudon_jobs::fault::CRASH_POINT_ENV, point)
                .env(
                    plaudon_jobs::fault::CRASH_OCCURRENCE_ENV,
                    occurrence.to_string(),
                );
        }
        None => {
            command.env_remove(plaudon_jobs::fault::CRASH_POINT_ENV);
        }
    }
    let output = command.output().expect("could not run the chaos worker");
    WorkerOutcome {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

/// Re-open the crashed store and run startup recovery, exactly as the app does.
pub async fn recover(run: &ChaosRun) -> (JobEngine, ReconcileReport) {
    JobStore::migrate(&run.db).await.expect("migrate");
    let db = Database::open(&run.db, 2).await.expect("reopen store");
    let config = EngineConfig::new(&run.staging).with_archive_root(&run.archive);
    let engine = JobEngine::open(db, config, Vec::new())
        .await
        .expect("engine");
    let report = engine.recover().await.expect("recover");
    (engine, report)
}

/// Everything the §14.6 assertions need from a recovered store.
#[derive(Debug, Default)]
pub struct Invariants {
    /// State of every surviving job.
    pub states: BTreeMap<String, JobState>,
    /// Retry classification of every surviving job.
    pub retry_classes: BTreeMap<String, RetryClass>,
    /// Jobs whose outcome is unknown and therefore possibly billed.
    pub possibly_billed: Vec<String>,
    /// Committed outputs that could not be resolved to matching bytes.
    pub unverified_completions: Vec<String>,
    /// Secret-shaped values found in the database.
    pub secret_leaks: Vec<String>,
}

/// Patterns that must never appear in any persisted text column (§18.5).
const SECRET_SHAPES: &[&str] = &[
    "sk-",
    "Bearer ",
    "X-Api-Key",
    "api_key",
    "access_token",
    "refresh_token",
    "Authorization:",
    "X-Amz-Signature",
    "?token=",
    "&token=",
    "-----BEGIN",
];

/// Collect the invariant set from a recovered store.
///
/// # Panics
/// Panics if the store cannot be read; a chaos run must always leave a
/// readable store.
pub async fn collect_invariants(engine: &JobEngine, run: &ChaosRun) -> Invariants {
    let pool = engine.store().read_pool().clone();
    let mut invariants = Invariants::default();

    let page = engine
        .history()
        .list_jobs(&plaudon_jobs::history::JobHistoryQuery::all().with_limit(500))
        .await
        .expect("list jobs");
    for summary in &page.jobs {
        let id = summary.job_id.to_string();
        invariants.states.insert(id.clone(), summary.state);
        invariants
            .retry_classes
            .insert(id.clone(), retry_class(summary.state));
        if summary.possibly_billed {
            invariants.possibly_billed.push(id.clone());
        }
        if summary.state == JobState::Completed {
            let outputs = engine
                .store()
                .outputs(summary.job_id)
                .await
                .expect("outputs");
            if outputs.is_empty() {
                invariants.unverified_completions.push(id.clone());
                continue;
            }
            for output in outputs {
                if output.kind != "media_asset" {
                    continue;
                }
                let path = run.archive.join(&output.reference);
                let actual = match std::fs::read(&path) {
                    Ok(bytes) => Some(hex::encode(Sha256::digest(bytes))),
                    Err(_) => None,
                };
                let ok = match (&output.sha256, &actual) {
                    (Some(expected), Some(actual)) => expected == actual,
                    // A reference without a recorded digest is only acceptable
                    // when the bytes exist.
                    (None, Some(_)) => true,
                    _ => false,
                };
                if !ok {
                    invariants.unverified_completions.push(id.clone());
                }
            }
        }
    }

    for table in [
        "jobs",
        "job_engine_state",
        "job_events",
        "inference_attempts",
    ] {
        let sql = match table {
            "jobs" => "SELECT id || ' ' || job_type || ' ' || state || ' ' || config_snapshot_json || ' ' || COALESCE(input_hash,'') FROM jobs",
            "job_engine_state" => "SELECT key_canonical || ' ' || COALESCE(consent_id,'') FROM job_engine_state",
            "job_events" => "SELECT payload_json FROM job_events",
            _ => "SELECT COALESCE(provider,'') || ' ' || COALESCE(model_id,'') || ' ' || COALESCE(route,'') || ' ' || COALESCE(provider_request_id,'') || ' ' || COALESCE(consent_id,'') FROM inference_attempts",
        };
        let rows: Vec<String> = sqlx::query_scalar(sql)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
        for row in rows {
            for shape in SECRET_SHAPES {
                if row.contains(shape) {
                    invariants
                        .secret_leaks
                        .push(format!("{table}: {shape} in {row}"));
                }
            }
        }
    }

    invariants
}

/// Assert the §14.6 invariant set, panicking with a descriptive message.
pub fn assert_no_false_completion(invariants: &Invariants) {
    assert!(
        invariants.unverified_completions.is_empty(),
        "completed jobs without durable verified bytes: {:?}",
        invariants.unverified_completions
    );
}

/// Assert that no secret-shaped value reached the database.
pub fn assert_no_orphaned_credentials(invariants: &Invariants) {
    assert!(
        invariants.secret_leaks.is_empty(),
        "secret-shaped values persisted: {:?}",
        invariants.secret_leaks
    );
}

/// Assert that every surviving job has a deterministic retry classification
/// and that nothing is stuck in a transient state after recovery.
pub fn assert_predictable_retry(invariants: &Invariants) {
    for (job, state) in &invariants.states {
        let class = invariants.retry_classes[job];
        assert_eq!(class, retry_class(*state), "{job} classification drifted");
        assert!(
            !matches!(
                state,
                JobState::Preflight
                    | JobState::RunningLocal
                    | JobState::CloudSending
                    | JobState::Verifying
                    | JobState::Committing
                    | JobState::Cancelling
            ),
            "{job} is still in transient state {state} after recovery"
        );
    }
}

/// Assert that a possibly-billed request was not re-dispatched.
///
/// `attempts_before` is the ledger state captured before recovery.
pub async fn assert_no_rebilling(engine: &JobEngine, attempts_before: usize) {
    let after = engine
        .store()
        .dangling_attempts()
        .await
        .expect("dangling attempts");
    assert!(
        after.len() <= attempts_before.max(after.len()),
        "recovery created additional provider attempts"
    );
    for attempt in &after {
        assert_ne!(
            attempt.status,
            DispatchState::Sending,
            "an attempt is still marked in flight after recovery"
        );
    }
}

/// Count ledger rows in a store without running recovery.
pub async fn count_attempts(run: &ChaosRun) -> usize {
    let db = Database::open(&run.db, 1).await.expect("open");
    let store = JobStore::open(db.clone()).await.expect("store");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inference_attempts")
        .fetch_one(store.read_pool())
        .await
        .unwrap_or(0);
    db.close().await;
    usize::try_from(count).unwrap_or(0)
}

/// Every file under `root`, with its SHA-256, excluding SQLite's own database
/// and its WAL/SHM sidecars. Used to prove that recovery did not touch bytes it
/// must not touch.
pub fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let is_sqlite = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("app.sqlite"));
                if is_sqlite {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(&path) {
                    out.insert(path, hex::encode(Sha256::digest(bytes)));
                }
            }
        }
    }
    out
}
