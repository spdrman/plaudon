//! Safe structured diagnostics and redacted logs.
//!
//! This crate owns three related jobs:
//!
//! 1. [`redact`] — turning credential-shaped text into stable markers.
//! 2. [`logging`] — a `tracing` stack whose **sink** runs every formatted
//!    record through the redactor, so no call site can bypass it, plus the
//!    wire-payload switch that is off by default and not constructible in a
//!    release build (spec App. B.7).
//! 3. [`scan`] — the scanner behind the `plaudon-secret-scan` binary and the
//!    `scripts/secret-scan.sh` CI step, which uses the *same* pattern set as
//!    the sink (spec §18.5, §1.5).
//!
//! Plus two narrow surfaces that must not leak either: [`crash`] (panic
//! payloads and backtraces) and [`notification`] (OS notification payloads,
//! spec §18.6).
//!
//! # What this crate does not claim
//!
//! Redaction is pattern- and registry-based. It reliably removes the shapes in
//! [`patterns`] and any exactly registered live secret. It cannot recognise an
//! arbitrary unregistered secret that has no distinctive shape, and it cannot
//! stop a caller from writing user content (a transcript) into a log message.
//! Those remain the caller's responsibility; the scanner exists precisely
//! because redaction is not a proof.

pub mod crash;
pub mod device_metrics;
pub mod logging;
pub mod metrics;
pub mod notification;
pub mod patterns;
pub mod redact;
#[cfg(feature = "scanner")]
pub mod scan;

pub use crash::{install_crash_hook, redact_panic_payload};
pub use device_metrics::{
    DeviceMetricsCollector, DeviceMetricsReport, EtaEstimate, FidelityEntry, LatencySummary,
    MediaFidelity, Metric, ProfileEligibility, Tally, Throughput,
};
pub use logging::{
    build_subscriber, init_logging, LogFormat, LoggingConfig, LoggingError, LoggingHandle,
    WirePayloadLogging,
};
pub use metrics::{MetricSeries, MetricsRegistry, WatcherMetric, WatcherMetricsSnapshot};
pub use notification::redact_notification;
pub use redact::{Finding, Redactor, RegisterSecretError};
#[cfg(feature = "scanner")]
pub use scan::{
    Allowlist, AllowlistEntry, ScanError, ScanFinding, ScanOptions, ScanReport, Scanner,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-diagnostics";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
///
/// Capability and contract payloads are versioned independently of the
/// application release, so consumers can detect an unknown contract instead of
/// assuming an unavailable feature is present.
pub const CONTRACT_VERSION: u32 = 1;
