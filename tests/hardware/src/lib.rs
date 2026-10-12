//! Opt-in real-hardware suite and release-evidence emitter (spec §18.3A,
//! §18.6A, §7.9 "Hardware shipping gate", §24 "Release evidence bundle",
//! App. B.9).
//!
//! The crate has two halves:
//!
//! * a **hermetic half** (this library plus `tests/gate.rs` and
//!   `tests/evidence.rs`) that CI runs: the opt-in gate, the check catalogue,
//!   the detail guard, the evidence assembler/emitter, the JSON Schema and the
//!   promotion rules;
//! * a **hardware half** (`tests/hardware.rs`) in which every test is
//!   `#[ignore]` *and* consults the gate, so it never runs in default CI and
//!   never runs without a named device profile.
//!
//! A simulator result is never hardware evidence, and a Linux `bluer`
//! peripheral result is not accepted as proof of macOS or Windows pairing
//! (§18.3A); both are enforced by the evidence crate and documented in
//! `docs/hardware/`.

pub mod catalogue;
pub mod emitter;
pub mod gate;
pub mod guard;
pub mod host;
pub mod observations;
pub mod profile;
pub mod suite;

/// Name of this package.
pub const PACKAGE_NAME: &str = "plaudon-hardware-tests";
