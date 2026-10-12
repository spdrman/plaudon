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
//! * [`transport`] defines the GATT contract. Its only write method takes a
//!   [`plaudon_plaud_protocol::ControlFrame`], so a mutating opcode cannot be
//!   placed on the wire by any implementation.
//! * [`actor`] owns one transport as a single-owner actor; concurrent callers
//!   are serialized or refused with `DEVICE_BUSY`, never interleaved.
//! * [`fake`] replays versioned wire fixtures deterministically, which is how
//!   every hostile case is exercised without a radio. A simulator result is
//!   never compatibility evidence.
//! * The real radio scanner and transport live behind the `hardware-scan`
//!   feature. They are excluded from the default build and from CI, and they
//!   only ever read.

pub mod actor;
pub mod discovery;
pub mod session;
pub mod transport;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

#[cfg(feature = "hardware-scan")]
pub mod scanner;

#[cfg(feature = "hardware-scan")]
pub mod hardware;

pub use actor::{DeviceActor, DeviceHandle, FrameSink, TransferSummary};
pub use discovery::{AdvertisementVerdict, Candidate, DiscoveryPolicy, ObservedAdvertisement};
pub use session::{SessionDriver, SessionEvent};
pub use transport::{Characteristics, DeviceEvent, GattTransport, ProbeResult, TransferProgress};

#[cfg(any(test, feature = "test-support"))]
pub use fake::{DeviceScenario, FakeDevice, ScenarioBuilder, TransferFault};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-plaud-ble";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
