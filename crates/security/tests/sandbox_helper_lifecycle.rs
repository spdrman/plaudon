//! Helper-handle lifecycle: the refusals that happen before a helper starts,
//! and the state a handle keeps after one fails (§6.3A, §14.1, §14.7).
//!
//! `tests/sandbox_integration.rs` proves what the sandbox enforces on a live
//! helper. This suite covers the surrounding state machine: a launch that is
//! refused before `fork`, and the rule that a handle which has recorded a
//! terminal failure keeps reporting it rather than letting a later call look
//! like a success.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use plaudon_security::sandbox::ipc::{HelperRequest, HelperResponse};
use plaudon_security::sandbox::{
    HelperOutcome, JobStateHint, SandboxError, SandboxSpec, SandboxedHelper, TimeoutBudget,
};

fn probe_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_plaudon-sandbox-probe"))
}

fn spec(wall_clock: Duration) -> SandboxSpec {
    SandboxSpec::builder()
        .read_path(probe_binary())
        .memory_bytes(256 * 1024 * 1024)
        .cpu_seconds(30)
        .wall_clock(wall_clock)
        .build()
        .expect("valid spec")
}

/// A program that is not an absolute, existing regular file is refused before
/// any process is created, and the refusal is terminal because retrying the
/// same deployment will fail identically.
#[test]
fn a_launch_is_refused_before_fork_when_the_program_is_unusable() {
    let spec = spec(Duration::from_secs(5));
    let args: [OsString; 0] = [];

    let relative = SandboxedHelper::spawn(&spec, Path::new("sandbox-probe"), &args).unwrap_err();
    assert!(
        matches!(&relative, SandboxError::SpawnFailed { detail } if detail.contains("absolute")),
        "got {relative:?}"
    );
    assert!(!relative.is_retryable(), "a deployment problem is terminal");
    assert_eq!(
        HelperOutcome::from(Err::<HelperResponse, _>(relative)).job_state(),
        JobStateHint::FailedTerminal
    );

    let directory = probe_binary()
        .parent()
        .expect("the probe lives in a directory")
        .to_path_buf();
    let not_a_file = SandboxedHelper::spawn(&spec, &directory, &args).unwrap_err();
    assert!(
        matches!(
            &not_a_file,
            SandboxError::SpawnFailed { detail } if detail == "helper program is not a regular file"
        ),
        "got {not_a_file:?}"
    );

    let missing = probe_binary().with_file_name("plaudon-sandbox-probe-does-not-exist");
    let gone = SandboxedHelper::spawn(&spec, &missing, &args).unwrap_err();
    assert!(
        matches!(&gone, SandboxError::SpawnFailed { .. }),
        "got {gone:?}"
    );
}

/// A live helper answers, and the handle reports the identity and the cap it is
/// actually holding rather than the one that was requested in the abstract.
#[test]
fn a_live_helper_answers_and_the_handle_reports_what_it_is_holding() {
    let spec = spec(Duration::from_secs(20));
    let args: [OsString; 0] = [];
    let mut helper =
        SandboxedHelper::spawn(&spec, &probe_binary(), &args).expect("the probe must start");

    assert!(helper.pid() > 0);
    assert_eq!(helper.memory_cap(), spec.memory_bytes());
    assert!(!helper.is_failed());
    assert!(
        helper.stderr_snapshot().is_empty(),
        "a healthy helper says nothing on stderr: {}",
        helper.stderr_snapshot()
    );

    let echoed = helper
        .request(HelperRequest::Echo {
            payload: "round trip".to_owned(),
        })
        .expect("echo");
    assert_eq!(
        echoed,
        HelperResponse::Echo {
            payload: "round trip".to_owned()
        }
    );

    let slept = helper.request_outcome(HelperRequest::SleepMs { ms: 5 });
    assert!(slept.is_completed());
    assert_eq!(slept.job_state(), JobStateHint::Completed);
    assert_eq!(slept.error(), None);
    assert!(matches!(
        slept,
        HelperOutcome::Completed(HelperResponse::Slept { ms: 5 })
    ));

    // A clean shutdown is an exit, not a failure.
    let farewell = helper.request(HelperRequest::Shutdown).expect("shutdown");
    assert_eq!(
        farewell,
        HelperResponse::Echo {
            payload: "shutdown".to_owned()
        }
    );
    let summary = helper
        .wait_with_timeout(Duration::from_secs(10))
        .expect("the helper exits on its own");
    assert!(summary.success, "{summary:?}");
    assert_eq!(summary.code, Some(0));
    assert_eq!(summary.signal, None);
    assert!(
        !helper.is_failed(),
        "a clean exit is not a recorded failure"
    );
}

/// Once a handle has recorded a terminal failure it keeps returning it: a
/// caller cannot observe a success after the helper was terminated, which is
/// what keeps a killed helper out of the `Completed` job state (§14.1).
#[test]
fn a_failed_handle_keeps_reporting_the_same_failure() {
    let spec = spec(Duration::from_millis(1500));
    let args: [OsString; 0] = [];
    let mut helper =
        SandboxedHelper::spawn(&spec, &probe_binary(), &args).expect("the probe must start");

    let first = helper
        .request(HelperRequest::SleepMs { ms: 60_000 })
        .expect_err("the wall-clock budget must expire first");
    assert!(
        matches!(
            &first,
            SandboxError::Timeout {
                budget: TimeoutBudget::WallClock,
                ..
            }
        ),
        "got {first:?}"
    );
    assert!(first.is_retryable(), "a budget can be raised and retried");
    assert!(helper.is_failed());

    // Every later call repeats the recorded failure, never a fresh success.
    for _ in 0..3 {
        let again = helper
            .request(HelperRequest::Echo {
                payload: "are you there".to_owned(),
            })
            .expect_err("the handle is permanently failed");
        assert_eq!(again, first);
    }

    let outcome = helper.request_outcome(HelperRequest::Hello);
    assert!(!outcome.is_completed());
    assert_eq!(outcome.job_state(), JobStateHint::FailedRetryable);
    assert_eq!(outcome.error(), Some(&first));

    // Killing an already-terminated helper is safe and idempotent.
    helper.kill();
    helper.kill();
    assert!(helper.is_failed());
}

/// Dropping a handle reaps the helper, so a second helper can start
/// immediately afterwards and the first leaves nothing behind.
#[test]
fn dropping_a_handle_reaps_the_helper() {
    let spec = spec(Duration::from_secs(30));
    let args: [OsString; 0] = [];

    let first_pid = {
        let mut helper =
            SandboxedHelper::spawn(&spec, &probe_binary(), &args).expect("the probe must start");
        helper
            .request(HelperRequest::Echo {
                payload: "alive".to_owned(),
            })
            .expect("the first helper answers");
        helper.pid()
    };

    let mut second =
        SandboxedHelper::spawn(&spec, &probe_binary(), &args).expect("a second helper starts");
    assert_ne!(second.pid(), first_pid);
    second
        .request(HelperRequest::Echo {
            payload: "alive".to_owned(),
        })
        .expect("the second helper answers");
    second.kill();
    assert!(!second.is_failed(), "an explicit kill records no failure");
}
