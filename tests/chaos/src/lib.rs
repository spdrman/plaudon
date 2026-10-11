//! Crash-injection and interruption suites for durable state transitions.
//!
//! Each suite drives a real durable component to a chosen step, simulates an
//! abrupt stop, then asserts that recovery reaches a valid state.

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-chaos-tests";
