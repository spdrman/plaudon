//! `plaudon-egress-harness` — D2.3 network-deny test fixture (spec §16.2A,
//! §1.5, §18.6A "Local-only", §22(15), §24).
//!
//! The fixture answers one question with evidence instead of assertion:
//! **does this application initiate any internet traffic in Local-only Mode?**
//!
//! It does so by running each subsystem's real code path in a child process
//! with `plaudon-egress-monitor` injected. The monitor records every libc
//! networking and process-execution call and refuses every non-loopback
//! destination, so the harness can compare the policy's claims against
//! observed behaviour:
//!
//! | Source | What it shows |
//! |---|---|
//! | `plaudon_security::outbound` decision log | what the gate *decided* |
//! | monitor trace | what the process *did* at the libc boundary |
//!
//! A scenario passes only when both agree. The `negative-control` scenario
//! deliberately bypasses the gate, so a run that reports "all clean" while the
//! negative control is silent is reported as a **broken fixture**, not a pass.
//!
//! # Honest scope (§16.2A, §16.6)
//!
//! This observes the scenario process. It says nothing about the operating
//! system, other applications, platform stores, OS update agents, or a
//! cloud-sync client acting on a user-selected folder. See
//! [`evidence::EvidenceBundle::not_guaranteed`] in the artifact, which is
//! written on every run.
//!
//! # Running
//!
//! ```text
//! cargo test -p plaudon-egress-harness      # hermetic, no network needed
//! scripts/egress-deny-test.sh               # same, plus evidence collection
//! scripts/egress-tcpdump.sh <scenario>      # opt-in, privileged packet capture
//! ```

#![warn(missing_debug_implementations, rust_2018_idioms)]

pub mod evidence;
pub mod fixture;
pub mod trace;
pub mod verify;

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-egress-harness";

/// Wall-clock seconds since the Unix epoch, for stamping the evidence bundle.
///
/// Only ever used for evidence metadata; no decision in this crate depends on
/// the clock.
#[must_use]
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_matches_manifest() {
        assert_eq!(PACKAGE_NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn now_unix_is_after_the_spec_was_written() {
        assert!(now_unix() > 1_700_000_000);
    }
}
