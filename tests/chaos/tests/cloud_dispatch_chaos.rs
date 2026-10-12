//! Process-kill coverage for the cloud request ledger (task G2.1, spec 10.7,
//! 14.3, 18.9).
//!
//! Each case runs a real process that writes real rows to a real SQLite file
//! and then calls `abort()`. The assertions are made by a *different* process
//! afterwards, so nothing in-memory can mask a missing durable write.

use std::path::Path;
use std::process::Command;

use plaudon_app_core::cloud::InferenceLedger;
use plaudon_domain::capability::DispatchState;
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;
use tempfile::TempDir;

fn runner() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_cloud-dispatch-runner"))
}

fn crash_at(db: &Path, point: &str) {
    let status = Command::new(runner())
        .arg(db)
        .arg(point)
        .status()
        .expect("spawn runner");
    assert!(
        !status.success(),
        "the runner must die at {point}, not exit cleanly"
    );
}

async fn open(db: &Path) -> (Database, InferenceLedger) {
    let database = Database::open(db).await.expect("open");
    MigrationRunner::new()
        .run(&database)
        .await
        .expect("migrate");
    let ledger = InferenceLedger::new(database.clone());
    (database, ledger)
}

#[tokio::test]
async fn a_crash_before_the_row_leaves_nothing_to_reconcile() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("app.sqlite");
    crash_at(&path, "before_row");

    let (_db, ledger) = open(&path).await;
    assert!(ledger
        .attempts_for_job("job-1")
        .await
        .expect("attempts")
        .is_empty());
    assert!(ledger
        .reconcile_in_flight(Timestamp::from_millis(1))
        .await
        .expect("reconcile")
        .is_empty());
}

#[tokio::test]
async fn a_crash_after_the_row_but_before_sending_stays_safe_to_retry() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("app.sqlite");
    crash_at(&path, "after_row");

    let (_db, ledger) = open(&path).await;
    let attempts = ledger.attempts_for_job("job-1").await.expect("attempts");
    assert_eq!(attempts.len(), 1, "the pre-dispatch row is durable");
    assert_eq!(attempts[0].state(), DispatchState::NotSent);
    assert!(attempts[0].state().auto_retry_safe());
    assert!(!attempts[0].possibly_billed());

    // Reconciliation leaves it alone: nothing was transmitted.
    assert!(ledger
        .reconcile_in_flight(Timestamp::from_millis(1))
        .await
        .expect("reconcile")
        .is_empty());
    assert_eq!(
        ledger.attempts_for_job("job-1").await.expect("attempts")[0].state(),
        DispatchState::NotSent
    );
}

#[tokio::test]
async fn a_crash_while_sending_becomes_outcome_unknown_and_never_auto_retries() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("app.sqlite");
    crash_at(&path, "after_sending");

    let (_db, ledger) = open(&path).await;
    let before = ledger.attempts_for_job("job-1").await.expect("attempts");
    assert_eq!(before[0].state(), DispatchState::Sending);

    let moved = ledger
        .reconcile_in_flight(Timestamp::from_millis(1))
        .await
        .expect("reconcile");
    assert_eq!(moved.len(), 1);

    let after = ledger.attempts_for_job("job-1").await.expect("attempts");
    assert_eq!(after[0].state(), DispatchState::OutcomeUnknown);
    assert!(after[0].possibly_billed());
    assert!(
        !after[0].state().auto_retry_safe(),
        "startup recovery must never turn an unknown outcome into a fresh request"
    );
}

#[tokio::test]
async fn a_crash_after_the_provider_accepted_is_also_possibly_billed() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("app.sqlite");
    crash_at(&path, "after_accepted");

    let (_db, ledger) = open(&path).await;
    let before = ledger.attempts_for_job("job-1").await.expect("attempts");
    assert_eq!(before[0].state(), DispatchState::AcceptedByProvider);
    assert_eq!(
        before[0].provider_request_id.as_deref(),
        Some("req_chaos"),
        "the vendor request id survives the kill"
    );

    ledger
        .reconcile_in_flight(Timestamp::from_millis(1))
        .await
        .expect("reconcile");
    let after = ledger.attempts_for_job("job-1").await.expect("attempts");
    assert_eq!(after[0].state(), DispatchState::OutcomeUnknown);
    assert_eq!(after[0].provider_request_id.as_deref(), Some("req_chaos"));
}

#[tokio::test]
async fn reconciliation_is_idempotent_across_repeated_startups() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("app.sqlite");
    crash_at(&path, "after_sending");

    let (_db, ledger) = open(&path).await;
    let first = ledger
        .reconcile_in_flight(Timestamp::from_millis(1))
        .await
        .expect("reconcile");
    assert_eq!(first.len(), 1);
    let second = ledger
        .reconcile_in_flight(Timestamp::from_millis(2))
        .await
        .expect("reconcile");
    assert!(
        second.is_empty(),
        "a second startup must not touch an already-settled attempt"
    );
}
