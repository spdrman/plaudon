//! SQLite migrations, repositories and the content-addressed media archive.

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-persistence";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
///
/// Capability and contract payloads are versioned independently of the
/// application release, so consumers can detect an unknown contract instead of
/// assuming an unavailable feature is present.
pub const CONTRACT_VERSION: u32 = 1;
