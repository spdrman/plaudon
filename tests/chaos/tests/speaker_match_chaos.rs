//! Killing the speaker-matching job at a checkpoint boundary and starting it
//! again (`VOICE_PROFILES_SPEC` VP-102, VP-105).
//!
//! The pipeline is five stages with one checkpoint each, so the interesting
//! kills are the ones *inside* a commit mark: the work of the stage is durable,
//! the mark is not, and the next process has to decide whether to redo it. Two
//! cases matter and both are here:
//!
//! * killed as the **embed** stage commits — the resumed job must not embed
//!   anything twice and must reach the match stage;
//! * killed as the **suggest** stage commits — the suggestion rows are already
//!   written, so the resumed job re-runs the stage and the `(run, cluster,
//!   person)` unique key has to make that a no-op rather than a duplicate.
//!
//! The child process runs the real pipeline over a real library with the
//! deterministic provider and the scripted embedder. The kill is the job
//! store's own `checkpoint:committed` crash point, armed by occurrence, which
//! aborts the process inside the transaction without unwinding.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use plaudon_jobs::fault;

/// Occurrence of `checkpoint:committed` for each stage: diarize, reconcile,
/// embed, match, suggest, in that order.
const EMBED_COMMIT: u32 = 3;
const SUGGEST_COMMIT: u32 = 5;

static RUNNER: LazyLock<PathBuf> = LazyLock::new(build_runner);

fn build_runner() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args([
            "build",
            "-p",
            "plaudon-chaos-tests",
            "--bin",
            "speaker-match-runner",
            "--message-format=json",
        ])
        .stderr(Stdio::inherit())
        .output()
        .expect("cargo");
    assert!(output.status.success(), "building the runner failed");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] == "compiler-artifact" {
            if let Some(path) = value["executable"].as_str() {
                if path.contains("speaker-match-runner") {
                    return PathBuf::from(path);
                }
            }
        }
    }
    panic!("cargo reported no speaker-match-runner executable");
}

struct Outcome {
    stdout: String,
    stderr: String,
    crashed: bool,
}

fn run(db: &std::path::Path, crash_at: Option<u32>) -> Outcome {
    let mut command = Command::new(&*RUNNER);
    command.arg(db);
    match crash_at {
        Some(occurrence) => {
            assert!(fault::is_registered("checkpoint:committed"));
            command
                .env(fault::CRASH_POINT_ENV, "checkpoint:committed")
                .env(fault::CRASH_OCCURRENCE_ENV, occurrence.to_string());
        }
        None => {
            command.env_remove(fault::CRASH_POINT_ENV);
        }
    }
    let output = command.output().expect("run the speaker-match runner");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Outcome {
        crashed: stderr.contains(plaudon_chaos_tests::job_engine::CRASH_FIRED),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr,
    }
}

async fn count(db: &std::path::Path, sql: &'static str) -> i64 {
    let pool = plaudon_persistence::db::Database::open(db)
        .await
        .expect("open library");
    let value: i64 = sqlx::query_scalar(sql)
        .fetch_one(pool.pool())
        .await
        .expect("count");
    pool.close().await;
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_as_the_embed_stage_commits_resumes_at_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("app.sqlite");

    let killed = run(&db, Some(EMBED_COMMIT));
    assert!(
        killed.crashed,
        "the runner did not die at the embed commit: {} {}",
        killed.stdout, killed.stderr
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM speaker_suggestions").await,
        0,
        "a job killed before matching recorded no suggestion"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM job_checkpoints WHERE committed_at IS NOT NULL"
        )
        .await,
        2,
        "only diarize and reconcile had committed before the kill"
    );

    let resumed = run(&db, None);
    assert!(
        resumed.stdout.contains("PLAUDON_MATCH_FINISHED"),
        "the resumed run did not finish: {} {}",
        resumed.stdout,
        resumed.stderr
    );
    assert!(
        resumed.stdout.contains("resumed=2"),
        "the resumed run did not reuse the two committed stages: {}",
        resumed.stdout
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM speaker_suggestions").await,
        1,
        "the resumed job produced the suggestion"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM diarization_runs").await,
        1,
        "the resumed job reused its diarization run instead of making another"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM speaker_match_cluster_embeddings").await,
        0,
        "the biometric scratch did not outlive the finished job"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_as_the_suggest_stage_commits_reinserts_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("app.sqlite");

    let killed = run(&db, Some(SUGGEST_COMMIT));
    assert!(
        killed.crashed,
        "the runner did not die at the suggest commit: {} {}",
        killed.stdout, killed.stderr
    );
    // The rows were written in their own transaction before the commit mark,
    // so they survive the kill; the mark does not.
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM speaker_suggestions").await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM job_checkpoints WHERE stage = 'suggest' AND committed_at IS NOT NULL"
        )
        .await,
        0,
        "the suggest stage has no commit mark after the kill"
    );

    let resumed = run(&db, None);
    assert!(
        resumed.stdout.contains("PLAUDON_MATCH_FINISHED"),
        "the resumed run did not finish: {} {}",
        resumed.stdout,
        resumed.stderr
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM speaker_suggestions").await,
        1,
        "re-running the suggest stage inserted a duplicate"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM speaker_suggestions WHERE state = 'pending'"
        )
        .await,
        1
    );
}
