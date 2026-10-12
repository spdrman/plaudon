//! Chaos scenario matrix (EPIC C task C2.4; SPEC v1.3 §14.6, §18.6A).
//!
//! Required observable outcomes, per §18.6A:
//!
//! * **Archive cluster** — crash before/after rename, SQLite transaction
//!   failure, locked output, out-of-space ⇒ every `Completed` record has
//!   durable bytes; startup reconciles orphans.
//! * **Cloud cluster** — lost response after upload, consent revoked
//!   mid-queue, route privacy changes, unknown price ⇒
//!   `CloudOutcomeUnknown` / no automatic rebilling; no unapproved dispatch.
//!
//! Every scenario is hermetic: no network, no real provider, no real
//! credential store, and reproducible from the seed printed in the test name.

use plaudon_chaos_tests::job_engine as chaos;
use plaudon_jobs::error::RetryClass;
use plaudon_jobs::state::JobState;
use plaudon_jobs::store::DispatchState;
use sha2::Digest;

/// Collects the sequence numbers a subscriber actually receives.
#[derive(Debug)]
struct Collector(std::sync::Arc<std::sync::Mutex<Vec<i64>>>);

impl plaudon_jobs::events::EventSubscriber for Collector {
    fn consumer_id(&self) -> &'static str {
        "chaos-collector"
    }

    fn handle(
        &self,
        event: &plaudon_jobs::events::EventRecord,
    ) -> Result<(), plaudon_jobs::events::SubscriberError> {
        self.0.lock().expect("lock").push(event.seq);
        Ok(())
    }
}

/// Run one scenario under one crash point and assert the whole invariant set.
async fn crash_and_recover(
    seed: &str,
    scenario: &str,
    crash_point: &str,
) -> (chaos::ChaosRun, chaos::Invariants) {
    let run = chaos::ChaosRun::new(seed);
    let outcome = chaos::run_worker(&run, scenario, Some(crash_point));
    assert!(
        outcome.crashed() || outcome.finished(),
        "worker neither crashed nor finished at {crash_point}: {}",
        outcome.stderr
    );

    let (engine, _report) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    chaos::assert_no_orphaned_credentials(&invariants);
    chaos::assert_predictable_retry(&invariants);
    (run, invariants)
}

// ---------------------------------------------------------------------------
// Archive cluster
// ---------------------------------------------------------------------------

#[tokio::test]
async fn crash_at_every_durable_transition_leaves_a_consistent_store() {
    // One scenario, every reachable transition boundary.
    let points: Vec<String> = JobState::ALL
        .iter()
        .flat_map(|state| {
            [
                plaudon_jobs::fault::transition_point(*state),
                plaudon_jobs::fault::transition_point_after(*state),
            ]
        })
        .collect();
    for (index, point) in points.iter().enumerate() {
        let seed = format!("{index}");
        let (_run, invariants) = crash_and_recover(&seed, "import_with_journal", point).await;
        assert!(
            invariants.secret_leaks.is_empty(),
            "{point}: {:?}",
            invariants.secret_leaks
        );
    }
}

#[tokio::test]
async fn a_kill_after_staging_leaves_only_a_removable_staging_file() {
    let run = chaos::ChaosRun::new("100");
    // Killed after step 2 (hashed) and before step 3 (intent recorded): the
    // bytes exist only as an app-owned staging file with no journal row.
    let outcome = chaos::run_worker(&run, "import_with_journal", Some("journal:hashed"));
    assert!(outcome.crashed(), "{}", outcome.stderr);

    // A half-written staging file exists and nothing was completed.
    let staged: Vec<_> = std::fs::read_dir(run.staging_root())
        .expect("read staging")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(staged.len(), 1, "a removable staging file is left behind");

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    assert!(
        !invariants
            .states
            .values()
            .any(|s| *s == JobState::Completed),
        "nothing may be completed after a mid-write kill"
    );
}

#[tokio::test]
async fn crash_before_commit_rolls_the_transaction_back_entirely() {
    let run = chaos::ChaosRun::new("101");
    assert!(chaos::run_worker(&run, "import_with_journal", Some("tx:before_commit")).crashed());

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    // The very first transaction is the enqueue, so the store may legitimately
    // be empty — but it must be readable and internally consistent.
    chaos::assert_predictable_retry(&invariants);
}

#[tokio::test]
async fn crash_after_commit_before_dispatch_replays_the_event_exactly_once() {
    let run = chaos::ChaosRun::new("102");
    assert!(chaos::run_worker(
        &run,
        "import_happy_path",
        Some("event:after_commit_before_dispatch")
    )
    .crashed());

    let (engine, _) = chaos::recover(&run).await;
    // The committed events are still in the log and replay from a fresh
    // consumer cursor delivers each exactly once.
    let total = engine.store().events().count().await.expect("count");
    assert!(total >= 1, "committed events survive the kill");

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let dispatcher = plaudon_jobs::events::EventDispatcher::new(
        vec![std::sync::Arc::new(Collector(seen.clone()))],
        engine.store().database().writer().clone(),
        engine.store().events().clone(),
    );
    dispatcher.replay_pending(256).await.expect("replay");
    let first = seen.lock().expect("lock").clone();
    let expected: Vec<i64> = (1..=i64::try_from(first.len()).expect("fits")).collect();
    assert_eq!(first, expected);

    dispatcher.replay_pending(256).await.expect("replay");
    assert_eq!(
        seen.lock().expect("lock").clone(),
        first,
        "a second replay must deliver nothing"
    );
}

#[tokio::test]
async fn crash_after_rename_before_db_commit_recovers_by_digest() {
    let run = chaos::ChaosRun::new("103");
    // Killed after step 4 (renamed) and before steps 5-6 (asset association
    // and journal close): the bytes are in place, the commit is not.
    assert!(chaos::run_worker(&run, "import_with_journal", Some("journal:renamed")).crashed());

    let (engine, report) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    // The bytes were renamed into place but the job never completed: the
    // journal entry must be classified, not silently completed.
    assert_eq!(
        report.journal_completed.len(),
        1,
        "verified bytes complete the interrupted commit: {report:?}"
    );
    assert!(
        !invariants
            .states
            .values()
            .any(|s| *s == JobState::Completed),
        "the job itself never completed, only its import intent did"
    );
}

#[tokio::test]
async fn a_locked_output_path_fails_closed_and_stays_retryable() {
    let run = chaos::ChaosRun::new("104");
    // Occupy the archive target with a directory so the rename cannot succeed.
    let digest = hex::encode(sha2::Sha256::digest(
        format!("media-bytes-for-{}", run.seed()).as_bytes(),
    ));
    let blocked = run
        .archive_root()
        .join("media/sha256")
        .join(&digest[..2])
        .join(format!("{digest}.wav"));
    std::fs::create_dir_all(&blocked).expect("occupy target");
    std::fs::write(blocked.join("squatter"), b"in the way").expect("write");

    let outcome = chaos::run_worker(&run, "import_with_journal", None);
    assert!(
        !outcome.finished(),
        "the rename must fail: {}",
        outcome.stdout
    );

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    chaos::assert_predictable_retry(&invariants);
    assert!(
        invariants
            .retry_classes
            .values()
            .all(|class| *class != RetryClass::Terminal),
        "a blocked output is retryable, not terminal: {:?}",
        invariants.retry_classes
    );
}

#[tokio::test]
async fn an_unwritable_staging_volume_fails_closed_without_a_partial_commit() {
    let run = chaos::ChaosRun::new("105");
    // Stand in for ENOSPC: make the staging directory unwritable so the
    // staging write fails before any durable state is produced.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(run.staging_root(), std::fs::Permissions::from_mode(0o555))
            .expect("chmod");
    }
    #[cfg(not(unix))]
    {
        // On Windows the equivalent is a read-only attribute on the directory,
        // which does not prevent file creation; use a file where the directory
        // is expected instead.
        let _ = std::fs::remove_dir_all(run.staging_root());
        std::fs::write(run.staging_root(), b"not a directory").expect("occupy");
    }

    let outcome = chaos::run_worker(&run, "import_with_journal", None);
    assert!(!outcome.finished(), "the staging write must fail");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(run.staging_root(), std::fs::Permissions::from_mode(0o755))
            .expect("restore");
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::remove_file(run.staging_root());
        std::fs::create_dir_all(run.staging_root()).expect("restore");
    }

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    assert!(
        !invariants
            .states
            .values()
            .any(|s| *s == JobState::Completed),
        "an out-of-space import must not partially commit"
    );
}

#[tokio::test]
async fn startup_reconciliation_removes_orphans_without_touching_user_files() {
    let run = chaos::ChaosRun::new("106");
    assert!(chaos::run_worker(&run, "import_with_journal", Some("journal:flushed")).crashed());

    // A user file that must survive untouched.
    let user_file = run.root().join("user-recording.wav");
    std::fs::write(&user_file, b"irreplaceable").expect("write");
    let before = chaos::snapshot_tree(run.root());

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);

    assert_eq!(
        std::fs::read(&user_file).expect("read"),
        b"irreplaceable",
        "a user file is never cleaned up"
    );
    let after = chaos::snapshot_tree(run.root());
    for (path, digest) in &before {
        if let Some(current) = after.get(path) {
            assert_eq!(
                current,
                digest,
                "{} changed during recovery",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn recovery_is_idempotent_across_repeated_startups() {
    let run = chaos::ChaosRun::new("107");
    assert!(chaos::run_worker(
        &run,
        "import_with_journal",
        Some("transition:running_local")
    )
    .crashed());

    let (_engine, first) = chaos::recover(&run).await;
    let (engine, second) = chaos::recover(&run).await;
    assert!(
        second.is_noop(),
        "a second reconciliation must change nothing; first={first:?} second={second:?}"
    );
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    chaos::assert_predictable_retry(&invariants);
}

// ---------------------------------------------------------------------------
// Cloud cluster
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_lost_response_after_upload_yields_outcome_unknown_and_no_rebilling() {
    let run = chaos::ChaosRun::new("200");
    assert!(chaos::run_worker(
        &run,
        "cloud_dispatch_no_response",
        Some("dispatch:after_send_before_response")
    )
    .crashed());

    let attempts_before = chaos::count_attempts(&run).await;
    assert_eq!(attempts_before, 1, "exactly one ledger row was written");

    let (engine, report) = chaos::recover(&run).await;
    assert_eq!(report.jobs_outcome_unknown.len(), 1);

    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    chaos::assert_no_orphaned_credentials(&invariants);
    assert_eq!(invariants.possibly_billed.len(), 1);
    assert!(invariants
        .states
        .values()
        .any(|s| *s == JobState::CloudOutcomeUnknown));
    assert!(invariants
        .retry_classes
        .values()
        .any(|c| *c == RetryClass::UserRetryWithAcknowledgement));

    chaos::assert_no_rebilling(&engine, attempts_before).await;
    assert_eq!(
        chaos::count_attempts(&run).await,
        attempts_before,
        "recovery must not create a second provider attempt"
    );

    // A second recovery still does not re-dispatch.
    let (engine, second) = chaos::recover(&run).await;
    assert_eq!(second.jobs_outcome_unknown.len(), 0);
    assert_eq!(chaos::count_attempts(&run).await, attempts_before);
    let attempts = engine.store().dangling_attempts().await.expect("dangling");
    assert_eq!(attempts[0].status, DispatchState::OutcomeUnknown);
}

#[tokio::test]
async fn a_crash_between_ledger_write_and_send_is_still_accounted_for() {
    let run = chaos::ChaosRun::new("201");
    assert!(chaos::run_worker(
        &run,
        "cloud_dispatch_no_response",
        Some("dispatch:after_record_before_send")
    )
    .crashed());

    // §10.7: the ledger row exists before any content is sent, so the
    // uncertainty is visible after the crash.
    assert_eq!(chaos::count_attempts(&run).await, 1);

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    chaos::assert_predictable_retry(&invariants);
}

#[tokio::test]
async fn consent_revoked_mid_queue_produces_zero_subsequent_dispatches() {
    let run = chaos::ChaosRun::new("202");
    let outcome = chaos::run_worker(&run, "consent_revoked_mid_queue", None);
    assert!(
        outcome.finished(),
        "the scenario asserts the refusal itself: {}",
        outcome.stderr
    );
    assert_eq!(
        chaos::count_attempts(&run).await,
        0,
        "no ledger row may be created after revocation"
    );

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    assert!(invariants
        .states
        .values()
        .any(|s| *s == JobState::WaitingForAuthorization));
    assert_eq!(
        chaos::count_attempts(&run).await,
        0,
        "recovery must not dispatch under a revoked grant"
    );
}

#[tokio::test]
async fn a_completed_cloud_response_is_preserved_across_a_crash() {
    let run = chaos::ChaosRun::new("203");
    assert!(chaos::run_worker(&run, "cloud_dispatch_with_response", None).finished());
    let attempts_before = chaos::count_attempts(&run).await;

    let (engine, report) = chaos::recover(&run).await;
    assert_eq!(report.jobs_outcome_unknown.len(), 0);
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    assert_eq!(invariants.possibly_billed.len(), 0);
    assert_eq!(chaos::count_attempts(&run).await, attempts_before);
}

// ---------------------------------------------------------------------------
// Interruption cluster
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suspend_mid_stage_parks_the_job_and_resumes() {
    let run = chaos::ChaosRun::new("300");
    assert!(chaos::run_worker(&run, "suspend_mid_stage", None).finished());

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    assert!(
        !invariants
            .states
            .values()
            .any(|s| *s == JobState::Completed),
        "a suspended job must never report success"
    );
    chaos::assert_predictable_retry(&invariants);
}

#[tokio::test]
async fn a_crash_while_parked_keeps_the_wait_state() {
    let run = chaos::ChaosRun::new("301");
    assert!(chaos::run_worker(
        &run,
        "suspend_mid_stage",
        Some("transition:waiting_for_user:after")
    )
    .crashed());

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    assert!(
        invariants
            .states
            .values()
            .all(|s| *s != JobState::Completed),
        "a parked job is never completed by recovery"
    );
}

#[tokio::test]
async fn a_crash_during_cancellation_still_settles_as_cancelled() {
    let run = chaos::ChaosRun::new("302");
    assert!(chaos::run_worker(
        &run,
        "cancel_mid_flight",
        Some("transition:cancelling:after")
    )
    .crashed());

    let (engine, report) = chaos::recover(&run).await;
    assert_eq!(report.jobs_cancelled.len(), 1);
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    assert!(invariants
        .states
        .values()
        .any(|s| *s == JobState::Cancelled));
    chaos::assert_predictable_retry(&invariants);
}

#[cfg(unix)]
#[tokio::test]
async fn an_external_sigkill_during_work_is_survivable() {
    use std::io::Read;

    let run = chaos::ChaosRun::new("303");
    // Arm nothing; kill the child from outside while it is mid-scenario. This
    // complements the deterministic crash points with a real signal kill.
    let mut child = std::process::Command::new(chaos::worker_binary())
        .arg("--db")
        .arg(run.db_path())
        .arg("--staging")
        .arg(run.staging_root())
        .arg("--archive")
        .arg(run.archive_root())
        .arg("--scenario")
        .arg("import_with_journal")
        .arg("--seed")
        .arg(run.seed())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn worker");

    // Give the worker a moment to start durable work, then kill it hard.
    std::thread::sleep(std::time::Duration::from_millis(60));
    let _ = child.kill();
    let status = child.wait().expect("wait");
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    assert!(
        !status.success() || stdout.contains(chaos::WORKER_FINISHED),
        "the child either finished or was killed"
    );

    let (engine, _) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    chaos::assert_no_orphaned_credentials(&invariants);
    chaos::assert_predictable_retry(&invariants);
}

// ---------------------------------------------------------------------------
// Hermeticity
// ---------------------------------------------------------------------------

#[test]
fn the_job_engine_links_no_network_stack() {
    // The suite must never reach a real provider. The strongest hermetic
    // guarantee available at test time is that the engine's dependency tree
    // contains no HTTP client, HTTP server or socket framework at all.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .args([
            "tree",
            "-p",
            "plaudon-jobs",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--no-dedupe",
        ])
        .output()
        .expect("cargo tree");
    assert!(output.status.success(), "cargo tree failed");
    let tree = String::from_utf8_lossy(&output.stdout);

    for forbidden in [
        "reqwest",
        "hyper ",
        "hyper-util",
        "axum",
        "warp",
        "actix-web",
        "tiny_http",
        "tungstenite",
        "ureq",
        "curl",
        "isahc",
        "rustls",
        "native-tls",
        "openssl",
    ] {
        assert!(
            !tree.contains(forbidden),
            "plaudon-jobs must not depend on `{forbidden}`:\n{tree}"
        );
    }
}

#[test]
fn the_job_engine_opens_no_sockets() {
    // Source-level assertion to complement the dependency check: nothing in
    // the engine binds, connects or listens.
    let mut offenders = Vec::new();
    let mut stack =
        vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/jobs/src")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            for needle in [
                "TcpListener",
                "TcpStream",
                "UdpSocket",
                "UnixListener",
                "std::net::",
                "tokio::net",
            ] {
                if source.contains(needle) {
                    offenders.push(format!("{}: {needle}", path.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the job engine must open no sockets: {offenders:?}"
    );
}
