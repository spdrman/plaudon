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

pub mod bulk;
pub mod frame;
pub mod gatt;
pub mod opcode;

pub use bulk::{BulkAssembler, BulkFrame, TransferOutcome};
pub use frame::{ControlFrame, FrameError, FrameKind};
pub use gatt::{
    CHARACTERISTIC_BATTERY, CHARACTERISTIC_CONTROL, CHARACTERISTIC_NOTIFY, SERVICE_VENDOR,
};
pub use opcode::{ForbiddenOpcode, ReadOnlyOpcode};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-plaud-protocol";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
