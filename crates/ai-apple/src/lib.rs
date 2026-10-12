//! macOS-only Apple speech and language bridge bindings (spec 2.3, 11).
//!
//! The crate is split so that the part that can be wrong is testable
//! everywhere:
//!
//! * [`probe`] turns a bridge document into a capability, and it is pure. It
//!   compiles and is tested on Windows and in CI with no Apple framework in
//!   sight.
//! * [`ffi`] is the actual boundary and exists only behind the
//!   `apple-bridge` feature on macOS.
//!
//! Spec 2.3 is the rule the mapping encodes: `SystemLanguageModel` is an
//! on-device **text** model and is never a substitute for transcription, and
//! availability is a runtime answer, never an operating-system version
//! inference.

#[cfg(all(target_os = "macos", feature = "apple-bridge"))]
pub mod ffi;
pub mod probe;

pub use probe::{
    speech_availability, text_model_availability, BridgeDocument, BridgeError, ProbeDocument,
    TranscriptionDocument,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-apple";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;

/// ABI version this build expects from the Swift library.
///
/// The bridge reports its own version; a mismatch is refused rather than
/// reinterpreted, because the ownership rules could have changed.
pub const EXPECTED_ABI_VERSION: i32 = 1;
