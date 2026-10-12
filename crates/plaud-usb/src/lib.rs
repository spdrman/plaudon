//! Read-only import from a genuinely mounted legacy PLAUD Note volume
//! (spec 4.2B, 7.8, App. B.2).
//!
//! Three constraints shape this crate, and all three are enforced rather than
//! documented:
//!
//! * We never mount, unmount or write. [`scan`] opens files for reading only,
//!   and there is no function in this crate that creates, moves or deletes
//!   anything on the device volume.
//! * We do not generalize the legacy profile by name. Note Pro and NotePin S
//!   are recorded as known-unsupported in [`model`], and an unrecognized
//!   volume fails closed instead of being treated as a Note.
//! * We do not claim "zero physical writes". The host operating system mounts
//!   the volume read-write in the common case and writes its own metadata;
//!   [`mount::HostWriteBehaviour`] records that honestly.

pub mod gate;
pub mod import;
pub mod model;
pub mod mount;
pub mod scan;

pub use gate::{MountInstruction, UsbRefusal, HOST_READ_ONLY_NOTICE, HOST_WRITE_NOTICE};
pub use import::{
    import_recording, ImportError, ImportedDerivative, ImportedRecording, MemoryStagingSink,
    SinkState, StagingSink,
};
pub use model::{UsbSupport, VendorModel};
pub use mount::{HostWriteBehaviour, MountError, MountedVolume};
pub use scan::{ScanError, ScannedRecording, VolumeScan};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-plaud-usb";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
