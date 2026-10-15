//! What a [`SandboxSpec`] refuses to be built from, and what an
//! [`IsolationReport`] is allowed to claim (§6.3A, §16.6).
//!
//! `sandbox_integration.rs` spawns real helpers; this suite is the pure half:
//! the budget and grant limits at their boundaries, in the order they are
//! checked, and the rule that "fully isolated" means every dimension and not
//! merely the ones a given platform happens to manage.

use std::path::PathBuf;
use std::time::Duration;

use plaudon_security::sandbox::spec::{
    MAX_GRANTED_PATHS, MAX_WALL_CLOCK, MIN_MEMORY_BYTES, MIN_WALL_CLOCK,
};
use plaudon_security::sandbox::{
    IsolationLevel, IsolationReport, MemoryCapMechanism, ReducedIsolationReason, SandboxSpec,
    SpecError,
};

/// The grant cap is checked before any path is resolved, so a spec with too
/// many grants reports the cap rather than whichever grant happened to be
/// unresolvable first.
#[test]
fn too_many_grants_are_refused_before_any_path_is_resolved() {
    let mut builder = SandboxSpec::builder();
    for index in 0..=MAX_GRANTED_PATHS {
        // Deliberately unresolvable: if the cap were checked after
        // canonicalization this would be `Unresolvable` instead.
        builder = builder.read_path(format!("/no-such-root-cov276/{index}"));
    }

    assert_eq!(
        builder.build().expect_err("the cap is enforced"),
        SpecError::TooManyPaths {
            count: MAX_GRANTED_PATHS + 1,
            max: MAX_GRANTED_PATHS,
        }
    );
}

/// Read and write grants share one budget: 32 of one kind is the limit, and one
/// of the other kind on top of it is not.
#[test]
fn read_and_write_grants_share_one_cap() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut builder = SandboxSpec::builder();
    for index in 0..MAX_GRANTED_PATHS {
        builder = builder.read_path(dir.path().join(format!("input-{index}.wav")));
    }
    let spec = builder
        .clone()
        .build()
        .expect("exactly the cap is accepted");
    assert_eq!(spec.read_paths().len(), MAX_GRANTED_PATHS);
    assert_eq!(spec.write_paths(), &[] as &[PathBuf]);

    assert_eq!(
        builder
            .write_path(dir.path().join("output.wav"))
            .build()
            .expect_err("one more grant is one too many"),
        SpecError::TooManyPaths {
            count: MAX_GRANTED_PATHS + 1,
            max: MAX_GRANTED_PATHS,
        }
    );
}

/// The wall-clock budget is bounded at both ends: too short expires during
/// process start-up, too long is not a budget at all.
#[test]
fn a_wall_clock_budget_is_refused_at_both_ends_of_the_range() {
    let out_of_range = |requested: Duration| SpecError::WallClockOutOfRange {
        requested_ms: requested.as_millis(),
        min_ms: MIN_WALL_CLOCK.as_millis(),
        max_ms: MAX_WALL_CLOCK.as_millis(),
    };

    let too_long = MAX_WALL_CLOCK + Duration::from_secs(1);
    assert_eq!(
        SandboxSpec::builder()
            .wall_clock(too_long)
            .build()
            .expect_err("a day and a second is not a lifetime budget"),
        out_of_range(too_long)
    );

    let too_short = MIN_WALL_CLOCK
        .checked_sub(Duration::from_millis(1))
        .expect("the floor is not zero");
    assert_eq!(
        SandboxSpec::builder()
            .wall_clock(too_short)
            .build()
            .expect_err("shorter than start-up is not a budget"),
        out_of_range(too_short)
    );

    // Both ends of the accepted range are accepted.
    for edge in [MIN_WALL_CLOCK, MAX_WALL_CLOCK] {
        let spec = SandboxSpec::builder()
            .wall_clock(edge)
            .build()
            .expect("the boundary itself is in range");
        assert_eq!(spec.wall_clock(), edge);
    }
}

/// A grant that terminates in `..` has no leaf to re-append, so it cannot be
/// resolved — it is refused rather than being guessed at.
#[test]
fn a_grant_that_terminates_in_a_parent_component_is_unresolvable() {
    assert_eq!(
        SandboxSpec::builder()
            .read_path("/no-such-root-cov276/..")
            .build()
            .expect_err("a grant with no leaf cannot be resolved"),
        SpecError::Unresolvable
    );

    // The same shape under an existing parent is still refused: the grant has
    // to name what it grants.
    let dir = tempfile::tempdir().expect("temp dir");
    assert_eq!(
        SandboxSpec::builder()
            .read_path(dir.path().join("missing").join(".."))
            .build()
            .expect_err("a grant with no leaf cannot be resolved"),
        SpecError::Unresolvable
    );
}

/// Memory is the one budget with a floor, and it is checked after the grant cap
/// and before the CPU budget.
#[test]
fn a_memory_budget_below_the_floor_is_refused() {
    assert_eq!(
        SandboxSpec::builder()
            .memory_bytes(MIN_MEMORY_BYTES - 1)
            .cpu_seconds(0)
            .build()
            .expect_err("the floor is enforced"),
        SpecError::MemoryTooSmall {
            requested: MIN_MEMORY_BYTES - 1,
            minimum: MIN_MEMORY_BYTES,
        },
        "memory is reported even though the CPU budget is also invalid"
    );

    assert_eq!(
        SandboxSpec::builder()
            .cpu_seconds(0)
            .build()
            .expect_err("a zero CPU budget is not a budget"),
        SpecError::CpuBudgetZero
    );
}

/// "Fully isolated" is a conjunction: a platform that caps resources but cannot
/// restrict the filesystem must not be able to describe itself as isolated
/// (§16.6).
#[test]
fn a_report_is_fully_isolated_only_when_every_dimension_is() {
    let nothing = IsolationReport::nothing_applied(ReducedIsolationReason::PlatformUnsupported);
    assert!(!nothing.is_fully_isolated());
    assert_eq!(nothing.memory_mechanism, MemoryCapMechanism::None);

    let mut memory_only = nothing;
    memory_only.memory = true;
    assert!(
        !memory_only.is_fully_isolated(),
        "a memory cap alone is not isolation"
    );

    let mut budgets_without_wall_clock = memory_only;
    budgets_without_wall_clock.cpu = true;
    assert!(
        !budgets_without_wall_clock.is_fully_isolated(),
        "a helper with no lifetime budget is not isolated"
    );

    let mut budgets_only = budgets_without_wall_clock;
    budgets_only.wall_clock = true;
    assert!(
        !budgets_only.is_fully_isolated(),
        "resource caps say nothing about the filesystem, network or exec"
    );

    let mut everything = budgets_only;
    everything.filesystem = IsolationLevel::Sandboxed {
        mechanism: "test harness",
    };
    everything.network = IsolationLevel::Sandboxed {
        mechanism: "test harness",
    };
    assert!(
        !everything.is_fully_isolated(),
        "exec is still unrestricted"
    );

    everything.exec = IsolationLevel::Sandboxed {
        mechanism: "test harness",
    };
    assert!(everything.is_fully_isolated());
}
