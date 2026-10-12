//! Release engineering tooling for the desktop application (spec 18.8, 18.9, 24).
//!
//! Everything a release claims must be derived from an artifact this crate
//! produced, never from a hand-edited value:
//!
//! * [`buildmeta`] emits reproducible build metadata for every CI run.
//! * [`coverage`] enforces the spec 18.9 branch-coverage policy and the
//!   critical-state-transition checklist.
//! * [`scenarios`] holds the spec 18.7 acceptance-scenario registry so no
//!   required scenario can silently go missing.
//! * [`compat`] renders the public compatibility matrix from the claim
//!   register, so a `Supported` cell cannot be written as prose.
//! * [`doclint`] rejects documentation that invents evidence vocabulary or
//!   makes unmeasured claims.
//! * [`evidence`] assembles and verifies the signed release evidence bundle
//!   required by spec 24.

pub mod buildmeta;
pub mod compat;
pub mod coverage;
pub mod doclint;
pub mod evidence;
pub mod hash;
pub mod scenarios;
pub mod sign;

pub use hash::{sha256_bytes, sha256_file};
