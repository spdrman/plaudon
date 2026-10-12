//! Validated PLAUD legacy frames with a static read-only opcode allowlist.
//!
//! The protocol detail is the one recorded in spec App. B.3 for original
//! PLAUD Note firmware `V0095`-like legacy BLE, as observed in the MIT
//! `plaude-cli` repository. We implement **decode and read-only encode only**.
//!
//! The safety property this crate provides is structural, not procedural:
//! [`ReadOnlyOpcode`] is a closed enum containing only inspect, authenticate,
//! list and read operations, and it is the only way to construct a control
//! frame. A delete, clear, recording-control, settings-write, OTA or
//! sync-acknowledgement frame is therefore not merely forbidden, it cannot be
//! expressed (spec 7.9).
//!
//! # The allowlist is a compile-time property
//!
//! There is no `ReadOnlyOpcode` variant for a mutating command, so the
//! following does not compile:
//!
//! ```compile_fail
//! use plaudon_plaud_protocol::ReadOnlyOpcode;
//! let _ = ReadOnlyOpcode::DeleteFile;
//! ```
//!
//! ```compile_fail
//! use plaudon_plaud_protocol::ReadOnlyOpcode;
//! let _ = ReadOnlyOpcode::ClearAllFile;
//! ```
//!
//! ```compile_fail
//! use plaudon_plaud_protocol::ReadOnlyOpcode;
//! let _ = ReadOnlyOpcode::SendSyncFileInd;
//! ```
//!
//! Nor is there a constructor from a raw wire value, so a mutating opcode
//! cannot be smuggled in as an integer either:
//!
//! ```compile_fail
//! use plaudon_plaud_protocol::ReadOnlyOpcode;
//! let _ = ReadOnlyOpcode::from_raw(0x0002);
//! ```
//!
//! ```compile_fail
//! use plaudon_plaud_protocol::{ControlFrame, ReadOnlyOpcode};
//! // `ControlFrame` has no constructor that takes a `u16` opcode.
//! let _ = ControlFrame::new(0x0002u16, vec![]);
//! ```
//!
//! The positive direction still works, and a forbidden value decodes to an
//! error rather than to a frame:
//!
//! ```
//! use plaudon_plaud_protocol::{ControlFrame, ReadOnlyOpcode};
//! let frame = ControlFrame::new(ReadOnlyOpcode::ListDelta, vec![]).unwrap();
//! let mut bytes = frame.encode();
//! bytes[1..3].copy_from_slice(&0x0002u16.to_be_bytes()); // deleteFile
//! assert!(ControlFrame::decode(&bytes).is_err());
//! ```

pub mod asr;
pub mod bulk;
pub mod frame;
pub mod gatt;
pub mod opcode;
pub mod payload;
pub mod profile;
pub mod stream;

pub use asr::{remux_to_ogg_opus, AsrError, AsrScanner, AsrSummary};
pub use bulk::{BulkAssembler, BulkFrame, TransferOutcome};
pub use frame::{ControlFrame, FrameError, FrameKind};
pub use gatt::{
    CHARACTERISTIC_BATTERY, CHARACTERISTIC_CONTROL, CHARACTERISTIC_NOTIFY, SERVICE_VENDOR,
};
pub use opcode::{ForbiddenOpcode, ReadOnlyOpcode};
pub use payload::{
    AuthResponse, AuthStatus, DataRequest, DeltaListing, DeviceCodec, ListedRecording, PayloadError,
};
pub use profile::{profile_by_name, profile_for, ProfileAuthMode, ProtocolProfile, PROFILES};
pub use stream::{StreamError, StreamStep, StreamingAssembler};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-plaud-protocol";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
