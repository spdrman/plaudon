//! Opt-in hardware suites (spec 18.3A).
//!
//! Every test in this package needs a real machine in a particular state, so
//! each suite is gated behind an environment variable and never runs by
//! default. The crate root holds only the module list; each subsystem owns
//! its own gate, because what counts as "opted in" differs — a device suite
//! needs a named hardware profile, the Apple suite needs a Mac and
//! optionally an audio fixture.

pub mod apple;
