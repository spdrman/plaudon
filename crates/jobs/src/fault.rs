//! Crash-injection points (SPEC v1.3 §14.6; issue C2.2).
//!
//! Every durable transition and every step of the §13.3A durable import order
//! calls [`trip`] with a stable name. With the `fault-injection` feature
//! disabled — the default, and therefore every release build — [`trip`] is an
//! empty inline function and the crash-point registry does not exist.
//!
//! When the feature is enabled, a process is armed through the
//! `PLAUDON_CRASH_POINT` environment variable. The first time the named point
//! is reached the process aborts immediately, without unwinding, without
//! flushing buffered writes and without running destructors — the closest
//! in-process approximation of `SIGKILL`.

/// Environment variable naming the crash point to trip.
pub const CRASH_POINT_ENV: &str = "PLAUDON_CRASH_POINT";

/// Environment variable selecting which occurrence to trip (1-based, default 1).
pub const CRASH_OCCURRENCE_ENV: &str = "PLAUDON_CRASH_OCCURRENCE";

/// Exit code used when a crash point fires, distinguishing an injected crash
/// from an ordinary failure.
pub const CRASH_EXIT_CODE: i32 = 101;

/// Every crash point the engine registers.
///
/// Naming scheme:
/// * `transition:<state>` — immediately before the row for a transition into
///   `<state>` is written, inside the transaction.
/// * `transition:<state>:after` — after the transaction committed but before
///   any post-commit work (event dispatch, file cleanup).
/// * `journal:<step>` — the six durable import steps of §13.3A.
/// * `tx:before_commit` / `tx:after_commit` — generic SQLite commit boundary.
/// * `event:after_commit_before_dispatch` — §14.5 post-commit dispatch window.
/// * `file:during_write` — mid-write of a staging file.
pub const CRASH_POINTS: &[&str] = &[
    "tx:before_commit",
    "tx:after_commit",
    "event:after_commit_before_dispatch",
    "file:during_write",
    "journal:staged",
    "journal:hashed",
    "journal:flushed",
    "journal:intent_recorded",
    "journal:renamed",
    "journal:committed",
    "checkpoint:prepared",
    "checkpoint:committed",
    "dispatch:before_record",
    "dispatch:after_record_before_send",
    "dispatch:after_send_before_response",
    "transition:queued",
    "transition:queued:after",
    "transition:preflight",
    "transition:preflight:after",
    "transition:waiting_for_user",
    "transition:waiting_for_user:after",
    "transition:waiting_for_network",
    "transition:waiting_for_network:after",
    "transition:waiting_for_model",
    "transition:waiting_for_model:after",
    "transition:waiting_for_source",
    "transition:waiting_for_source:after",
    "transition:waiting_for_authorization",
    "transition:waiting_for_authorization:after",
    "transition:running_local",
    "transition:running_local:after",
    "transition:cloud_ready",
    "transition:cloud_ready:after",
    "transition:cloud_sending",
    "transition:cloud_sending:after",
    "transition:cloud_outcome_unknown",
    "transition:cloud_outcome_unknown:after",
    "transition:verifying",
    "transition:verifying:after",
    "transition:committing",
    "transition:committing:after",
    "transition:completed",
    "transition:completed:after",
    "transition:failed_retryable",
    "transition:failed_retryable:after",
    "transition:failed_terminal",
    "transition:failed_terminal:after",
    "transition:cancelling",
    "transition:cancelling:after",
    "transition:cancelled",
    "transition:cancelled:after",
];

/// `true` when `name` is a registered crash point.
pub fn is_registered(name: &str) -> bool {
    CRASH_POINTS.contains(&name)
}

/// `true` when this build contains crash points at all.
pub const fn injection_available() -> bool {
    cfg!(feature = "fault-injection")
}

#[cfg(feature = "fault-injection")]
mod imp {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    struct Armed {
        point: String,
        occurrence: u32,
    }

    static ARMED: LazyLock<Option<Armed>> = LazyLock::new(|| {
        let point = std::env::var(super::CRASH_POINT_ENV).ok()?;
        if point.is_empty() {
            return None;
        }
        let occurrence = std::env::var(super::CRASH_OCCURRENCE_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        Some(Armed { point, occurrence })
    });

    static COUNTERS: LazyLock<Mutex<HashMap<&'static str, u32>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Record that `name` was reached and abort if it is the armed point.
    pub fn trip(name: &'static str) {
        debug_assert!(
            super::is_registered(name),
            "crash point `{name}` is not in CRASH_POINTS"
        );
        let hits = {
            let mut guard = COUNTERS.lock().expect("crash counter poisoned");
            let entry = guard.entry(name).or_insert(0);
            *entry += 1;
            *entry
        };
        if let Some(armed) = ARMED.as_ref() {
            if armed.point == name && hits == armed.occurrence {
                // Bypass stdio buffering so the harness can attribute the kill.
                let msg = format!("PLAUDON_CRASH_POINT_FIRED {name}\n");
                crate::fault::write_stderr(&msg);
                std::process::abort();
            }
        }
    }

    /// How many times `name` has been reached in this process.
    pub fn hits(name: &str) -> u32 {
        COUNTERS
            .lock()
            .expect("crash counter poisoned")
            .get(name)
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(not(feature = "fault-injection"))]
mod imp {
    /// No-op in builds without the `fault-injection` feature.
    #[inline(always)]
    pub fn trip(_name: &'static str) {}

    /// Always zero without the `fault-injection` feature.
    #[inline(always)]
    pub fn hits(_name: &str) -> u32 {
        0
    }
}

pub use imp::{hits, trip};

#[cfg(feature = "fault-injection")]
pub(crate) fn write_stderr(message: &str) {
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = err.write_all(message.as_bytes());
    let _ = err.flush();
}

/// Name of the crash point fired just before a transition into `state` is
/// written.
pub fn transition_point(state: crate::state::JobState) -> String {
    format!("transition:{}", state.as_str())
}

/// Name of the crash point fired just after the transition into `state`
/// committed.
pub fn transition_point_after(state: crate::state::JobState) -> String {
    format!("transition:{}:after", state.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::JobState;

    #[test]
    fn every_transition_target_has_a_registered_crash_point() {
        for state in JobState::ALL {
            assert!(
                is_registered(&transition_point(*state)),
                "missing crash point for transition into {state}"
            );
            assert!(
                is_registered(&transition_point_after(*state)),
                "missing post-commit crash point for {state}"
            );
        }
    }

    #[test]
    fn every_durable_import_step_has_a_registered_crash_point() {
        for step in crate::journal::JournalStep::ALL {
            assert!(
                is_registered(&format!("journal:{}", step.as_str())),
                "missing crash point for durable import step {step:?}"
            );
        }
    }

    #[test]
    fn registry_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for point in CRASH_POINTS {
            assert!(seen.insert(*point), "duplicate crash point {point}");
        }
    }

    #[test]
    fn release_builds_contain_no_crash_points() {
        // The default feature set is what ships. If this ever becomes true by
        // default, fault injection would be reachable in production.
        assert_eq!(injection_available(), cfg!(feature = "fault-injection"));
        #[cfg(not(feature = "fault-injection"))]
        {
            assert!(!injection_available());
            trip("tx:before_commit"); // provably a no-op
            assert_eq!(hits("tx:before_commit"), 0);
        }
    }
}
