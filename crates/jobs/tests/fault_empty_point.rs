//! An empty crash-point variable arms nothing (SPEC v1.3 §14.6).
//!
//! A harness that clears the variable by setting it to the empty string,
//! rather than unsetting it, must get an ordinary process: every point is
//! counted and none of them fires, the empty name included.
//!
//! One test on purpose: the armed point is read once per process through a
//! `LazyLock`, so the environment has to be set before anything in this binary
//! trips a point.
#![cfg(feature = "fault-injection")]

use plaudon_jobs::fault::{hits, trip, CRASH_OCCURRENCE_ENV, CRASH_POINT_ENV};

#[test]
fn an_empty_crash_point_variable_arms_nothing() {
    std::env::set_var(CRASH_POINT_ENV, "");
    std::env::set_var(CRASH_OCCURRENCE_ENV, "1");

    trip("tx:before_commit");
    trip("tx:before_commit");
    trip("journal:abandoned");

    assert_eq!(hits("tx:before_commit"), 2);
    assert_eq!(hits("journal:abandoned"), 1);
}
