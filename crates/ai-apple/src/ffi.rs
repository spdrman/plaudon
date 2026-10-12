//! The macOS-only foreign function boundary (spec 11.2, 11.6 — task H1.4).
//!
//! Compiled only with the `apple-bridge` feature on macOS. This is the single
//! module in the workspace that contains `unsafe`, and it exists so that no
//! other module has to.
//!
//! The contract it implements is stated in
//! `native/apple-bridge/include/plaudon_apple_bridge.h`. Three of its rules
//! shape the code here:
//!
//! * Every returned buffer is released with `plaudon_apple_string_free`,
//!   never with `free` or a Rust allocator, because the two sides do not
//!   share a heap.
//! * Every input string is borrowed for the duration of the call, so the
//!   `CString` temporaries must outlive the call and are named rather than
//!   inlined.
//! * Progress callbacks arrive on a library-owned thread. A Rust panic must
//!   not unwind into Swift, so the trampoline catches it.

use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::bridge::{AppleBridge, FfiError, ProgressSink};
use crate::envelope::{BridgeOp, ProgressEvent};

type ProgressFn = unsafe extern "C" fn(*mut c_void, *const c_char);

// The declarations themselves are unsafe by nature; each call site carries
// its own safety argument.
#[allow(unsafe_code)]
#[link(name = "PlaudonAppleBridge", kind = "static")]
unsafe extern "C" {
    fn plaudon_apple_bridge_abi_version() -> i32;
    fn plaudon_apple_string_free(value: *mut c_char);
    fn plaudon_apple_probe_speech(request: *const c_char) -> *mut c_char;
    fn plaudon_apple_probe_foundation_model(request: *const c_char) -> *mut c_char;
    fn plaudon_apple_install_speech_assets(
        request: *const c_char,
        progress: Option<ProgressFn>,
        context: *mut c_void,
    ) -> *mut c_char;
    fn plaudon_apple_transcribe_file(
        request: *const c_char,
        progress: Option<ProgressFn>,
        context: *mut c_void,
    ) -> *mut c_char;
    fn plaudon_apple_generate_text(
        request: *const c_char,
        progress: Option<ProgressFn>,
        context: *mut c_void,
    ) -> *mut c_char;
    fn plaudon_apple_new_cancel_token() -> u64;
    fn plaudon_apple_cancel(cancel_token: u64);
    fn plaudon_apple_release_cancel_token(cancel_token: u64);
    fn plaudon_apple_cancel_token_is_cancelled(cancel_token: u64) -> i32;
    fn plaudon_apple_live_cancel_token_count() -> u64;
}

/// Receives progress documents and forwards them to a Rust sink.
///
/// Invoked on a library-owned thread. The library guarantees no delivery
/// after the owning call returns or after cancellation completes, so the
/// `context` pointer is valid for every invocation that can reach here.
#[allow(unsafe_code)]
unsafe extern "C" fn progress_trampoline(context: *mut c_void, document: *const c_char) {
    if context.is_null() || document.is_null() {
        return;
    }
    // A panic crossing back into Swift is undefined behaviour; a dropped
    // progress update is not (spec 6.5).
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `context` is the address of the `&ProgressSink` local in
        // `call_with_progress`, which outlives the bridge call.
        let sink: &&ProgressSink<'_> = unsafe { &*context.cast::<&ProgressSink<'_>>() };
        // SAFETY: the library documents the buffer as a NUL-terminated UTF-8
        // string valid for the duration of this call.
        let text = unsafe { CStr::from_ptr(document) }.to_string_lossy();
        if let Some(event) = ProgressEvent::parse(&text) {
            sink(event);
        }
    }));
}

/// Take ownership of a document the library returned, releasing it correctly.
///
/// # Safety
///
/// `pointer` must be a string returned by this library and not yet freed.
#[allow(unsafe_code)]
unsafe fn take_document(pointer: *mut c_char) -> Result<String, FfiError> {
    if pointer.is_null() {
        return Err(FfiError::NullDocument);
    }
    // SAFETY: the library guarantees a NUL-terminated UTF-8 string.
    let text = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: released through the library's own deallocator, exactly once.
    // `text` already owns a copy, so the pointer is dead after this line.
    unsafe { plaudon_apple_string_free(pointer) };
    Ok(text)
}

/// The linked Swift library, as an [`AppleBridge`].
///
/// `Send + Sync` is sound because the library documents every entry point as
/// callable from any thread and serializes what the platform requires; the
/// struct itself holds no state.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinkedBridge;

impl LinkedBridge {
    /// A handle to the linked library.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl AppleBridge for LinkedBridge {
    fn abi_version(&self) -> Result<i32, FfiError> {
        // SAFETY: a parameterless call returning a plain integer.
        #[allow(unsafe_code)]
        Ok(unsafe { plaudon_apple_bridge_abi_version() })
    }

    fn new_cancel_token(&self) -> u64 {
        // SAFETY: the library owns the token; we hold only the integer.
        #[allow(unsafe_code)]
        unsafe {
            plaudon_apple_new_cancel_token()
        }
    }

    fn cancel(&self, token: u64) {
        // SAFETY: documented as thread safe and idempotent for any value.
        #[allow(unsafe_code)]
        unsafe {
            plaudon_apple_cancel(token);
        }
    }

    fn release_cancel_token(&self, token: u64) {
        // SAFETY: documented as idempotent for any value.
        #[allow(unsafe_code)]
        unsafe {
            plaudon_apple_release_cancel_token(token);
        }
    }

    fn is_cancelled(&self, token: u64) -> bool {
        // SAFETY: a diagnostics query, valid for any value.
        #[allow(unsafe_code)]
        unsafe {
            plaudon_apple_cancel_token_is_cancelled(token) != 0
        }
    }

    fn live_token_count(&self) -> u64 {
        // SAFETY: a parameterless diagnostics query.
        #[allow(unsafe_code)]
        unsafe {
            plaudon_apple_live_cancel_token_count()
        }
    }

    fn call(
        &self,
        op: BridgeOp,
        request: &str,
        progress: Option<&ProgressSink<'_>>,
    ) -> Result<String, FfiError> {
        let request = CString::new(request).map_err(|_| FfiError::InvalidArgument)?;

        // The sink reference must have a stable address for the duration of
        // the call, because that address is what crosses as `context`.
        let mut sink_slot: Option<&ProgressSink<'_>> = progress;
        let (callback, context): (Option<ProgressFn>, *mut c_void) = match &mut sink_slot {
            Some(sink) => (
                Some(progress_trampoline),
                std::ptr::from_mut(sink).cast::<c_void>(),
            ),
            None => (None, std::ptr::null_mut()),
        };

        // SAFETY: `request` is borrowed for the duration of the call; the
        // progress context outlives it because `sink_slot` is a local that is
        // not moved, and the library delivers no callback after returning.
        #[allow(unsafe_code)]
        let raw = unsafe {
            match op {
                BridgeOp::ProbeSpeech => plaudon_apple_probe_speech(request.as_ptr()),
                BridgeOp::ProbeFoundationModel => {
                    plaudon_apple_probe_foundation_model(request.as_ptr())
                }
                BridgeOp::InstallSpeechAssets => {
                    plaudon_apple_install_speech_assets(request.as_ptr(), callback, context)
                }
                BridgeOp::TranscribeFile => {
                    plaudon_apple_transcribe_file(request.as_ptr(), callback, context)
                }
                BridgeOp::GenerateText => {
                    plaudon_apple_generate_text(request.as_ptr(), callback, context)
                }
            }
        };
        // SAFETY: `raw` came from the library and is freed inside.
        #[allow(unsafe_code)]
        unsafe {
            take_document(raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::*;
    use crate::bridge::CancelGuard;
    use crate::envelope::{encode_request, ProbeRequest, EXPECTED_ABI_VERSION};

    /// The handle registry is process-global, so the tests that assert on its
    /// count run one at a time. Everything else in the suite stays parallel.
    static REGISTRY: Mutex<()> = Mutex::new(());

    #[test]
    fn the_linked_library_speaks_the_expected_abi() {
        assert_eq!(LinkedBridge.abi_version(), Ok(EXPECTED_ABI_VERSION));
    }

    #[test]
    fn a_probe_returns_a_well_formed_document() {
        let request = encode_request(BridgeOp::ProbeSpeech, &ProbeRequest { locale: None });
        let document = LinkedBridge
            .call(BridgeOp::ProbeSpeech, &request, None)
            .expect("the library always returns a document");
        assert!(document.contains("\"abi_version\":2"), "{document}");
        assert!(document.contains("\"op\":\"probe_speech\""), "{document}");
    }

    #[test]
    fn an_interior_nul_is_refused_before_crossing() {
        let error = LinkedBridge
            .call(BridgeOp::ProbeSpeech, "{\0}", None)
            .unwrap_err();
        assert_eq!(error, FfiError::InvalidArgument);
    }

    #[test]
    fn token_handles_are_balanced_across_thousands_of_cycles() {
        let _serial = REGISTRY.lock();
        let baseline = LinkedBridge.live_token_count();
        for index in 0..5_000u64 {
            let token = LinkedBridge.new_cancel_token();
            if index % 3 == 0 {
                LinkedBridge.cancel(token);
            }
            LinkedBridge.release_cancel_token(token);
        }
        assert_eq!(LinkedBridge.live_token_count(), baseline);
    }

    #[test]
    fn a_dropped_guard_releases_its_handle_in_the_library() {
        let _serial = REGISTRY.lock();
        let bridge: Arc<dyn AppleBridge> = Arc::new(LinkedBridge);
        let baseline = bridge.live_token_count();
        {
            let guard = CancelGuard::new(bridge.clone());
            assert_eq!(bridge.live_token_count(), baseline + 1);
            guard.cancel();
            assert!(guard.is_cancelled());
        }
        assert_eq!(bridge.live_token_count(), baseline);
    }

    #[test]
    fn cancelling_before_a_call_refuses_without_delivering_progress() {
        let bridge: Arc<dyn AppleBridge> = Arc::new(LinkedBridge);
        let guard = CancelGuard::new(bridge.clone());
        guard.cancel();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let seen = seen.clone();
            move |event: ProgressEvent| seen.lock().push(event)
        };
        let request = encode_request(
            BridgeOp::GenerateText,
            &crate::envelope::GenerateRequest {
                prompt: "hello".into(),
                instructions: None,
                schema: None,
                cancel_token: guard.token(),
                timeout_ms: 30_000,
                max_output_tokens: None,
                temperature: None,
            },
        );
        let document = bridge
            .call(BridgeOp::GenerateText, &request, Some(&sink))
            .expect("a document is always returned");
        assert!(document.contains("cancelled"), "{document}");
        assert!(
            seen.lock().is_empty(),
            "a call cancelled before start must deliver no progress"
        );
    }

    #[test]
    fn a_one_millisecond_budget_never_hangs() {
        let request = encode_request(
            BridgeOp::TranscribeFile,
            &crate::envelope::TranscribeRequest {
                path: "/nonexistent/plaudon/fixture.wav".into(),
                locale: None,
                cancel_token: 0,
                timeout_ms: 1,
                allow_asset_download: false,
                want_word_timestamps: false,
            },
        );
        let started = std::time::Instant::now();
        let document = LinkedBridge
            .call(BridgeOp::TranscribeFile, &request, None)
            .expect("a document is always returned");
        assert!(document.contains("\"ok\":false"), "{document}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "a bounded call must not hang"
        );
    }
}
