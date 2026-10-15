//! The armed-but-not-reached side of the crash-point registry (SPEC v1.3 §14.6).
//!
//! Every other suite runs with no crash point armed, so the registry's "is this
//! the armed point?" comparison is only ever taken on its negative side. This
//! target arms a real crash point that the process never reaches: the counters
//! still advance, the comparison runs on its positive side for the name check,
//! and nothing aborts.
//!
//! It holds exactly one test on purpose: the armed point is read once per
//! process through a `LazyLock`, so the environment must be set before any
//! other code in this binary can trip a point.
#![cfg(feature = "fault-injection")]

use plaudon_jobs::fault::{
    hits, injection_available, is_registered, journal_point, transition_point,
    transition_point_after, trip, trip_dynamic, CRASH_EXIT_CODE, CRASH_OCCURRENCE_ENV,
    CRASH_POINTS, CRASH_POINT_ENV,
};
use plaudon_jobs::state::JobState;
use plaudon_persistence::journal::JournalState;

#[test]
fn an_armed_crash_point_that_is_never_reached_leaves_the_process_alone() {
    // `journal:abandoned` is a registered point that nothing in this binary
    // reaches, so arming it proves the armed branch without aborting.
    std::env::set_var(CRASH_POINT_ENV, "journal:abandoned");
    std::env::set_var(CRASH_OCCURRENCE_ENV, "3");

    assert!(injection_available());
    assert_eq!(CRASH_EXIT_CODE, 101);
    assert!(is_registered("journal:abandoned"));
    assert!(!is_registered("journal:teleported"));

    assert_eq!(hits("tx:before_commit"), 0);
    trip("tx:before_commit");
    trip("tx:before_commit");
    assert_eq!(hits("tx:before_commit"), 2);
    assert_eq!(hits("tx:after_commit"), 0);
    trip("tx:after_commit");
    assert_eq!(hits("tx:after_commit"), 1);

    for state in JobState::ALL.iter().copied() {
        trip_dynamic(&transition_point(state));
        trip_dynamic(&transition_point_after(state));
        assert_eq!(hits(&transition_point(state)), 1, "{state}");
        assert_eq!(hits(&transition_point_after(state)), 1, "{state}");
    }

    for journal_state in [
        JournalState::Staged,
        JournalState::Hashed,
        JournalState::Flushed,
        JournalState::IntentRecorded,
        JournalState::Renamed,
        JournalState::Committed,
        JournalState::Quarantined,
    ] {
        let point = journal_point(journal_state);
        assert!(CRASH_POINTS.contains(&point.as_str()), "{point}");
        trip_dynamic(&point);
        assert_eq!(hits(&point), 1, "{point}");
    }

    // The armed point itself was never reached, so the process is still here.
    assert_eq!(hits("journal:abandoned"), 0);
}
