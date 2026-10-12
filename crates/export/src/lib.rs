//! Transcript, caption, media and portable-archive export (spec 4.6, 13.5-13.8).
//!
//! The crate has four jobs and keeps them separate on purpose:
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`bundle`] | the allowlisted projection every export renders from |
//! | [`render`] / [`captions`] / [`media`] | TXT, Markdown, JSON, SRT, VTT, audio |
//! | [`archive`] | the `.recording-archive` portable backup and its restore |
//! | [`service`] | the four spec 15.2 commands, as typed request/response |
//!
//! Three rules hold everywhere in here:
//!
//! 1. **No fabrication.** Captions are refused when a transcript has no real
//!    timestamps; an unconfirmed speaker name is never rendered as identity; a
//!    compressed device transfer is never labelled an original WAV.
//! 2. **No secrets.** Archives are built from an allowlist and scanned with
//!    the shared `plaudon-diagnostics` pattern table before they are
//!    committed; a hit aborts the backup.
//! 3. **No partial artifacts.** Every export and archive is written to a
//!    temporary file and renamed, and a restore stages and verifies
//!    everything before the library is touched.

pub mod archive;
pub mod bundle;
pub mod captions;
pub mod destination;
pub mod error;
pub mod media;
pub mod render;
pub mod service;

pub use archive::{
    ArchiveBuilder, InspectionReport, Protection, RestoreMode, RestoreOptions, RestoreReport,
    Restorer,
};
pub use bundle::{BundleLoader, RecordingExport};
pub use captions::CaptionFormat;
pub use destination::ExportDestination;
pub use error::{ArchiveRejection, CaptionRefusal, ExportError, ExportResult};
pub use service::{
    BackupRequest, BackupResponse, ExportFormat, ExportRecordingRequest, ExportRecordingResponse,
    ExportService, InspectArchiveRequest, RestoreArchiveRequest,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-export";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
///
/// Capability and contract payloads are versioned independently of the
/// application release, so consumers can detect an unknown contract instead of
/// assuming an unavailable feature is present.
pub const CONTRACT_VERSION: u32 = 1;
