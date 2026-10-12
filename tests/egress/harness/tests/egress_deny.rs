//! D2.3 acceptance: Local-only Mode initiates no app traffic, and the fixture
//! can prove it is able to fail.
//!
//! Hermetic: every destination is in a reserved documentation range or an
//! `.invalid` domain, the monitor refuses non-loopback traffic before the
//! kernel sees it, and no real vendor service is contacted at any point.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::LazyLock;

use plaudon_egress_harness::evidence::{EvidenceBundle, ScenarioEvidence};
use plaudon_egress_harness::fixture::{
    evidence_path, monitor_label, require_supported_monitor, target_dirs, Fixture, Injection,
};
use plaudon_egress_harness::now_unix;
use plaudon_egress_harness::verify::{verify_scenario, HarnessFailure};
use plaudon_egress_scenario::Scenario;

/// Building the monitor and the scenario binary is shared by every test in
/// this binary.
static FIXTURE: LazyLock<Result<Fixture, String>> =
    LazyLock::new(|| Fixture::build().map_err(|e| e.to_string()));

fn shared_fixture() -> &'static Fixture {
    match &*FIXTURE {
        Ok(fixture) => fixture,
        Err(message) => panic!("could not build the egress fixture: {message}"),
    }
}

#[test]
fn local_only_mode_initiates_no_app_traffic() {
    require_supported_monitor().unwrap_or_else(|err| panic!("{err}"));
    let fixture = shared_fixture();

    let mut evidence = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut negative_control_attempts = 0usize;
    let mut total_cloud_cli_execs = 0usize;

    for scenario in Scenario::ALL {
        let run = fixture
            .run(scenario, Injection::Enabled)
            .unwrap_or_else(|err| panic!("could not run `{}`: {err}", scenario.name()));

        let outcome = verify_scenario(
            scenario,
            &run.trace,
            run.summary.as_ref(),
            run.exit_code,
            Injection::Enabled.label(),
        );
        let failure = match outcome {
            Ok(()) => None,
            Err(err) => {
                let text = format!(
                    "{err}\n  stdout: {}\n  stderr: {}\n  trace:  {}",
                    run.stdout.trim(),
                    run.stderr.trim(),
                    run.trace_path.display()
                );
                failures.push(text.clone());
                Some(text)
            }
        };

        if scenario.is_negative_control() {
            negative_control_attempts = run.trace.non_loopback_attempts().len();
        } else {
            total_cloud_cli_execs += run.trace.cloud_cli_execs().len();
        }

        evidence.push(ScenarioEvidence::from_run(
            &run,
            monitor_label(),
            now_unix(),
            failure,
        ));
    }

    // The §24 release-evidence artifact is written whether or not the run
    // passed, so a failure leaves the trace behind for the bundle.
    let (target_dir, _) = target_dirs().unwrap();
    let artifact = evidence_path(&target_dir);
    EvidenceBundle::new(now_unix(), monitor_label(), evidence)
        .write_to(&artifact)
        .unwrap_or_else(|err| panic!("could not write {}: {err}", artifact.display()));

    assert!(
        failures.is_empty(),
        "egress acceptance failed for {} scenario(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert!(
        negative_control_attempts > 0,
        "the negative control recorded no attempts, so the fixture cannot detect traffic"
    );
    assert_eq!(
        total_cloud_cli_execs, 0,
        "a cloud-CLI process was executed in Local-only Mode"
    );
    assert!(artifact.is_file(), "{} was not written", artifact.display());
}

#[test]
fn negative_control_proves_the_fixture_can_fail() {
    require_supported_monitor().unwrap_or_else(|err| panic!("{err}"));
    let fixture = shared_fixture();

    let run = fixture
        .run(Scenario::NegativeControl, Injection::Enabled)
        .expect("negative control must run");

    assert!(
        run.trace.monitor_injected(),
        "monitor was not injected into the negative control"
    );

    let attempts = run.trace.non_loopback_attempts();
    assert!(
        !attempts.is_empty(),
        "THE EGRESS HARNESS IS BROKEN: the negative control deliberately connected to {} and \
         looked up {}, and the monitor recorded neither. Every other scenario's pass is \
         therefore meaningless.",
        plaudon_egress_scenario::NEGATIVE_CONTROL_ADDR,
        plaudon_egress_scenario::NEGATIVE_CONTROL_HOST
    );
    assert!(
        attempts.iter().all(|a| a.denied),
        "the monitor must refuse every non-loopback attempt it records"
    );
    assert!(
        attempts
            .iter()
            .any(|a| a.call == "connect" && a.host_or_ip == "203.0.113.1"),
        "expected the deliberate TCP connect to be recorded, got: {attempts:#?}"
    );
    assert!(
        attempts.iter().any(|a| a.call == "getaddrinfo"),
        "expected the deliberate DNS lookup to be recorded, got: {attempts:#?}"
    );
    assert!(
        !run.trace.cloud_cli_execs().is_empty(),
        "expected the deliberate `plaud` execution to be recorded, so that the \
         zero-cloud-CLI-exec assertion in the other scenarios is not vacuous"
    );

    // And the rule set agrees that this is the one scenario allowed to do it.
    verify_scenario(
        Scenario::NegativeControl,
        &run.trace,
        run.summary.as_ref(),
        run.exit_code,
        Injection::Enabled.label(),
    )
    .expect("the negative control is expected to pass its own rule set");
}

#[test]
fn an_unobserved_run_is_reported_as_monitor_unavailable() {
    require_supported_monitor().unwrap_or_else(|err| panic!("{err}"));
    let fixture = shared_fixture();

    // Deliberately start the scenario without injecting the monitor. The
    // scenario itself still succeeds (the gate denies, as always), so a naive
    // harness would report a pass with zero observed attempts.
    let run = fixture
        .run(Scenario::OpenRouterDispatch, Injection::Disabled)
        .expect("scenario must run without injection");

    assert_eq!(
        run.exit_code,
        Some(0),
        "the scenario itself should still succeed"
    );
    assert!(
        run.trace.lines.is_empty(),
        "an uninjected run must produce no trace"
    );

    let err = verify_scenario(
        Scenario::OpenRouterDispatch,
        &run.trace,
        run.summary.as_ref(),
        run.exit_code,
        Injection::Disabled.label(),
    )
    .expect_err("an unobserved run must not be reported as a pass");

    match err {
        HarnessFailure::MonitorUnavailable { ref scenario, .. } => {
            assert_eq!(scenario, Scenario::OpenRouterDispatch.name());
        }
        other => panic!("expected MonitorUnavailable, got {other:?}"),
    }
    assert!(err.to_string().contains("harness failure, not a pass"));
}

#[test]
fn local_import_really_commits_a_file_while_observed() {
    require_supported_monitor().unwrap_or_else(|err| panic!("{err}"));
    let fixture = shared_fixture();

    let run = fixture
        .run(Scenario::LocalImport, Injection::Enabled)
        .expect("local import runs");
    let summary = run.summary.as_ref().expect("local import writes a summary");
    let artifact = summary
        .local_artifact
        .as_ref()
        .expect("local import commits a file");

    assert!(artifact.verified, "{}", summary.outcome);
    assert!(std::path::Path::new(&artifact.committed_path).is_file());
    assert!(!std::path::Path::new(&artifact.staging_path).exists());
    assert_eq!(artifact.sha256.len(), 64);
    assert!(
        run.trace.non_loopback_attempts().is_empty(),
        "the local import path must not reach the network"
    );
    assert!(run.trace.monitor_injected());
}
