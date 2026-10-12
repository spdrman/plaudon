//! BLE central transport for PLAUD devices (spec 7.7, 7.9, 2.5A).
//!
//! The spike result this crate encodes is a negative one, and it is load
//! bearing. `plaude-cli`, the only source of real legacy BLE code, is
//! Linux/BlueZ-first; a cross-platform BLE crate proves that central
//! discovery exists on macOS and Windows, **not** that the PLAUD protocol
//! succeeds there. So:
//!
//! * [`discovery`] decides whether an advertisement is even a candidate, and
//!   it never treats a name or a manufacturer identifier as proof of
//!   identity. There is no auto-connect path.
//! * [`session`] drives the spec 7.9 `DeviceSession` state machine, which
//!   fails closed on an unknown firmware or an unimplemented auth mode.
//! * The real radio scanner lives behind the `hardware-scan` feature and the
//!   `plaud-ble-scan` binary. It is excluded from the default build and from
//!   CI, and it only ever reads.

pub mod discovery;
pub mod session;

#[cfg(feature = "hardware-scan")]
pub mod scanner;

pub use discovery::{AdvertisementVerdict, Candidate, DiscoveryPolicy, ObservedAdvertisement};
pub use session::{SessionDriver, SessionEvent};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-plaud-ble";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
