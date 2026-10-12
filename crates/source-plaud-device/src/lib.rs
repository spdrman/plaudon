//! Device capability registry and read-only transfer orchestration.
//!
//! The registry is the machine-readable claim register of spec 2.5B: a
//! committed `compatibility.yaml` whose rows are the *only* source of the
//! compatibility strings the interface shows. Promotional copy and hardcoded
//! labels are forbidden, and an unmatched profile tuple fails closed.

pub mod register;

pub use register::{
    CompatibilityCell, CompatibilityRegister, EvidenceLevel, ProfileKey, RegisterError, Resolution,
    SupportStatus,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-source-plaud-device";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
