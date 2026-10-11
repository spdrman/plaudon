//! Hermetic cross-crate integration support: in-memory fakes and shared
//! fixture helpers used by the workspace test suites.
//!
//! Nothing in this crate contacts a network service, a vendor CLI or a paid
//! inference endpoint (spec 6.3(9)). Every capability contract in the
//! workspace has a fake here, so core use cases can be driven end to end with
//! no vendor dependency at all.

pub mod fakes;

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-integration-tests";
