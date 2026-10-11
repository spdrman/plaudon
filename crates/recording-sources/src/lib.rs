//! Generic recording-source and direct-device capability contracts (spec 6.5).
//!
//! `RecordingSource` covers anything that can enumerate and acquire audio.
//! `DirectDevice` is deliberately separate so the library can enumerate files
//! without coupling itself to a radio handle, and so a device session can stay
//! a single-owner actor rather than being forced to be `Sync`.

pub mod device;
pub mod sink;
pub mod source;

pub use device::{
    AuthScheme, DeviceCapabilities, DeviceError, DeviceRecording, DeviceSessionState, DirectDevice,
    ResumeSemantics, Transport, VerifiedTransfer,
};
pub use sink::{AsyncWriteSink, InMemorySink, SinkError};
pub use source::{
    AcquiredAsset, RecordingSource, SourceCapabilities, SourceError, SourceItem, SourcePage,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-recording-sources";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
