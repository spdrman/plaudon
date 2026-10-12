//! Apple native speech and language bridge bindings (spec 2.3, 11).
//!
//! The crate is split so that the part that can be wrong is testable
//! everywhere:
//!
//! * [`envelope`], [`probe`] and [`compatibility`] are pure. They compile and
//!   are tested on Windows and in CI with no Apple framework in sight.
//! * [`bridge`] is the safe boundary, with a [`bridge::FakeBridge`] that lets
//!   the adapters in [`stt`] and [`text`] be exercised on any platform.
//! * [`ffi`] is the only module containing `unsafe`, and it exists only behind
//!   the `apple-bridge` feature on macOS.
//!
//! Spec 2.3 is the rule the mapping encodes: `SystemLanguageModel` is an
//! on-device **text** model and is never a substitute for transcription, and
//! availability is a runtime answer, never an operating-system version
//! inference.
//!
//! # Building the bridge
//!
//! ```text
//! scripts/build-apple-bridge.sh                 # static library + symbols
//! cargo test -p plaudon-ai-apple --features apple-bridge
//! scripts/apple-bridge-sanitizers.sh            # ASan/UBSan/TSan ABI suite
//! ```

pub mod bridge;
pub mod compatibility;
pub mod envelope;
#[cfg(all(target_os = "macos", feature = "apple-bridge"))]
pub mod ffi;
pub mod probe;
pub mod stt;
pub mod text;

pub use bridge::{AppleBridge, CancelGuard, FakeBridge, FfiError};
pub use compatibility::{
    label_for, CompatibilityCell, CompatibilityMatrix, EvidenceLevel, EvidenceRecord, MacClass,
};
pub use envelope::{
    BridgeError, BridgeErrorCode, BridgeOp, BridgeReply, ENVELOPE_VERSION, EXPECTED_ABI_VERSION,
};
pub use probe::{
    probe_once, speech_capabilities, text_model_capabilities, AppleAvailability, AppleCapabilities,
    AssetDownloadConsent, CacheOutcome, ProbeCache, APPLE_FOUNDATION_ENGINE, APPLE_SPEECH_ENGINE,
    PROBE_BUDGET,
};
pub use stt::{AppleSpeechToText, AssetPlan};
pub use text::{AppleTextModel, GenerationProvenance};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-apple";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;

/// Build a bridge handle for this host.
///
/// On macOS with the `apple-bridge` feature this links the Swift library. On
/// every other build it returns a bridge that answers `platform_unsupported`
/// without touching an Apple framework, which is what makes a Windows binary
/// contain no Apple symbols at all (spec 11.1).
#[must_use]
pub fn host_bridge() -> std::sync::Arc<dyn AppleBridge> {
    #[cfg(all(target_os = "macos", feature = "apple-bridge"))]
    {
        std::sync::Arc::new(ffi::LinkedBridge::new())
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-bridge")))]
    {
        std::sync::Arc::new(UnlinkedBridge)
    }
}

/// The bridge on a host that has none.
///
/// Every operation answers with the same typed refusal, so the capability
/// mapping reports `PlatformUnsupported` and the engine policy selects
/// whisper.cpp. Nothing here can fail, block or allocate a handle.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnlinkedBridge;

impl AppleBridge for UnlinkedBridge {
    fn abi_version(&self) -> Result<i32, FfiError> {
        Err(FfiError::NotLinked)
    }

    fn new_cancel_token(&self) -> u64 {
        0
    }

    fn cancel(&self, _token: u64) {}

    fn release_cancel_token(&self, _token: u64) {}

    fn is_cancelled(&self, _token: u64) -> bool {
        false
    }

    fn live_token_count(&self) -> u64 {
        0
    }

    fn call(
        &self,
        _op: BridgeOp,
        _request: &str,
        _progress: Option<&bridge::ProgressSink<'_>>,
    ) -> Result<String, FfiError> {
        Err(FfiError::NotLinked)
    }
}

#[cfg(test)]
mod tests {
    use plaudon_domain::capability::CapabilityStatus;

    use super::*;

    #[test]
    fn a_host_without_the_bridge_reports_platform_unsupported() {
        let availability = probe_once(&UnlinkedBridge, Some("en-US"), PROBE_BUDGET);
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);
        assert_eq!(caps.speech.status, CapabilityStatus::Unsupported);
        assert_eq!(caps.text_model.status, CapabilityStatus::Unsupported);
        assert!(!caps.speech.is_selectable());
        assert!(!caps.text_model.is_selectable());
    }

    #[test]
    fn the_host_bridge_is_usable_without_panicking_on_every_platform() {
        let bridge = host_bridge();
        let availability = probe_once(bridge.as_ref(), None, PROBE_BUDGET);
        // Either a real answer or a typed refusal; never a panic and never a
        // claim of support the probe did not establish.
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);
        assert!(matches!(
            caps.speech.status,
            CapabilityStatus::Supported
                | CapabilityStatus::Unsupported
                | CapabilityStatus::Unknown
                | CapabilityStatus::Experimental
        ));
    }

    #[test]
    fn the_engine_identity_matches_the_selection_policy() {
        // `plaudon_app_core` cannot be a dependency here without inverting the
        // layering, so the shared constant is asserted from the integration
        // suite as well; this keeps the local value from drifting silently.
        assert_eq!(APPLE_SPEECH_ENGINE, "apple-speech");
        assert_eq!(APPLE_FOUNDATION_ENGINE, "apple-foundation");
    }
}
