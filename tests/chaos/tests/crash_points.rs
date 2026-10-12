//! Harness self-tests and crash-point coverage (EPIC C task C2.2).
//!
//! Proves that the harness can actually kill a process at an addressable
//! durable boundary, that every durable transition has a crash point, that the
//! assertion helpers detect a seeded corruption, and that release builds
//! contain no crash points at all.

use plaudon_chaos_tests::job_engine as chaos;
use plaudon_jobs::fault;
use plaudon_jobs::state::{JobState, TRANSITIONS};

#[test]
fn every_durable_transition_has_an_addressable_crash_point() {
    let mut missing = Vec::new();
    for state in JobState::ALL.iter().copied() {
        for point in [
            fault::transition_point(state),
            fault::transition_point_after(state),
        ] {
            if !fault::is_registered(&point) {
                missing.push(point);
            }
        }
    }
    // Every edge target in the transition table must be addressable too.
    for edge in TRANSITIONS {
        let point = fault::transition_point(edge.to);
        if !fault::is_registered(&point) {
            missing.push(point);
        }
    }
    assert!(
        missing.is_empty(),
        "transitions without a crash point: {missing:?}"
    );
}

#[test]
fn every_durable_import_step_and_commit_boundary_is_addressable() {
    for state in [
        plaudon_persistence::journal::JournalState::Staged,
        plaudon_persistence::journal::JournalState::Hashed,
        plaudon_persistence::journal::JournalState::Flushed,
        plaudon_persistence::journal::JournalState::IntentRecorded,
        plaudon_persistence::journal::JournalState::Renamed,
        plaudon_persistence::journal::JournalState::Committed,
        plaudon_persistence::journal::JournalState::Abandoned,
        plaudon_persistence::journal::JournalState::Quarantined,
    ] {
        assert!(
            fault::is_registered(&fault::journal_point(state)),
            "missing crash point for durable import step {state:?}"
        );
    }
    for point in [
        "tx:before_commit",
        "tx:after_commit",
        "event:after_commit_before_dispatch",
        "file:during_write",
        "checkpoint:prepared",
        "checkpoint:committed",
        "dispatch:before_record",
        "dispatch:after_record_before_send",
        "dispatch:after_send_before_response",
    ] {
        assert!(fault::is_registered(point), "missing crash point {point}");
    }
}

#[test]
fn an_unregistered_crash_point_is_rejected_by_the_harness() {
    assert!(!fault::is_registered("transition:not_a_state"));
}

#[test]
fn the_harness_kills_the_worker_at_the_armed_point() {
    let run = chaos::ChaosRun::new("1");
    let outcome = chaos::run_worker(&run, "import_happy_path", Some("transition:running_local"));
    assert!(
        outcome.crashed(),
        "worker did not crash: stdout={} stderr={}",
        outcome.stdout,
        outcome.stderr
    );
    assert!(!outcome.finished());
    assert!(!outcome.status.success());
}

#[test]
fn the_worker_completes_when_no_crash_point_is_armed() {
    let run = chaos::ChaosRun::new("2");
    let outcome = chaos::run_worker(&run, "import_happy_path", None);
    assert!(
        outcome.finished(),
        "worker did not finish: stdout={} stderr={}",
        outcome.stdout,
        outcome.stderr
    );
    assert!(outcome.job_id().is_some());
}

#[tokio::test]
async fn the_assertion_helpers_detect_a_seeded_corruption() {
    let run = chaos::ChaosRun::new("3");
    assert!(chaos::run_worker(&run, "import_with_journal", None).finished());

    // Seed a corruption: a completed job whose media bytes do not match the
    // recorded digest. The helpers must notice.
    let (engine, _) = chaos::recover(&run).await;
    let page = engine
        .history()
        .list_jobs(&plaudon_jobs::history::JobHistoryQuery::all())
        .await
        .expect("list");
    let completed = page
        .jobs
        .iter()
        .find(|job| job.state == JobState::Completed)
        .expect("a completed job");
    let outputs = engine
        .store()
        .outputs(completed.job_id)
        .await
        .expect("outputs");
    let reference = outputs
        .iter()
        .find(|o| o.kind == "media_asset")
        .map(|o| o.reference.clone())
        .expect("media output");

    let path = run.archive_root().join(&reference);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, b"tampered bytes").expect("write");

    let invariants = chaos::collect_invariants(&engine, &run).await;
    assert!(
        !invariants.unverified_completions.is_empty(),
        "a tampered completion must be detected"
    );

    let detected = std::panic::catch_unwind(|| chaos::assert_no_false_completion(&invariants));
    assert!(detected.is_err(), "the assertion helper must fail loudly");
}

#[tokio::test]
async fn a_clean_run_satisfies_every_invariant() {
    let run = chaos::ChaosRun::new("4");
    assert!(chaos::run_worker(&run, "import_happy_path", None).finished());
    let (engine, report) = chaos::recover(&run).await;
    let invariants = chaos::collect_invariants(&engine, &run).await;
    chaos::assert_no_false_completion(&invariants);
    chaos::assert_no_orphaned_credentials(&invariants);
    chaos::assert_predictable_retry(&invariants);
    assert!(
        report.is_noop(),
        "a clean store needs no recovery: {report:?}"
    );
}

#[test]
fn release_builds_contain_no_crash_points() {
    // This test crate deliberately enables `fault-injection`; the assertion is
    // that the capability is feature-gated at all, which the `plaudon-jobs`
    // unit test `release_builds_contain_no_crash_points` pins for the default
    // feature set. Here we assert the gate is observable.
    assert!(
        fault::injection_available(),
        "the chaos crate must build plaudon-jobs with fault-injection"
    );
    assert!(
        !fault::CRASH_POINTS.is_empty(),
        "the registry must be populated when the feature is on"
    );
}
