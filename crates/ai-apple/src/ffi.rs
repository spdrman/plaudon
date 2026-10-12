//! The macOS-only foreign function boundary (spec 11.2).
//!
//! Compiled only with the `apple-bridge` feature on macOS. Every string the
//! library returns is released through the library's own deallocator, never
//! through `free` or a Rust allocator, because the two heaps are different.

use std::ffi::{c_char, c_void, CStr, CString};

use crate::probe::{BridgeDocument, ProbeDocument, TranscriptionDocument};
use crate::EXPECTED_ABI_VERSION;

// The declarations themselves are unsafe by nature; each call site carries
// its own safety argument.
#[allow(unsafe_code)]
#[link(name = "PlaudonAppleBridge", kind = "static")]
unsafe extern "C" {
    fn plaudon_apple_bridge_abi_version() -> i32;
    fn plaudon_apple_string_free(value: *mut c_char);
    fn plaudon_apple_probe_speech(locale: *const c_char) -> *mut c_char;
    fn plaudon_apple_probe_foundation_model() -> *mut c_char;
    fn plaudon_apple_transcribe_file(
        path: *const c_char,
        locale: *const c_char,
        cancel_token: u64,
        timeout_ms: u64,
        progress: Option<unsafe extern "C" fn(*mut c_void, f64)>,
        context: *mut c_void,
    ) -> *mut c_char;
    fn plaudon_apple_generate_text(
        prompt: *const c_char,
        cancel_token: u64,
        timeout_ms: u64,
    ) -> *mut c_char;
    fn plaudon_apple_new_cancel_token() -> u64;
    fn plaudon_apple_cancel(cancel_token: u64);
    fn plaudon_apple_release_cancel_token(cancel_token: u64);
}

/// Why a bridge call could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FfiError {
    /// The library reports an ABI version this build does not understand.
    #[error("the Apple bridge reports ABI version {found}, expected {expected}")]
    AbiMismatch {
        /// Version the library reports.
        found: i32,
        /// Version this build expects.
        expected: i32,
    },
    /// An argument contained an interior NUL and could not cross the boundary.
    #[error("an argument contains an interior NUL byte")]
    InvalidArgument,
    /// The library returned a null pointer, which its contract forbids.
    #[error("the Apple bridge returned a null document")]
    NullDocument,
}

/// A cancellation token owned by the Swift side.
///
/// Dropping the handle releases the token, so a forgotten cancel cannot leak
/// an entry in the library's registry.
#[derive(Debug)]
pub struct CancelToken(u64);

impl CancelToken {
    /// Allocate a token.
    #[must_use]
    pub fn new() -> Self {
        // SAFETY: the library allocates and owns the token; we only hold the
        // integer handle and release it in `Drop`.
        #[allow(unsafe_code)]
        Self(unsafe { plaudon_apple_new_cancel_token() })
    }

    /// Request cancellation. Safe to call from any thread, and idempotent.
    pub fn cancel(&self) {
        // SAFETY: the handle is valid until `Drop`, and the library documents
        // `plaudon_apple_cancel` as thread safe and idempotent.
        #[allow(unsafe_code)]
        unsafe {
            plaudon_apple_cancel(self.0);
        }
    }

    /// The raw handle, for passing into a bridge call.
    #[must_use]
    pub const fn raw(&self) -> u64 {
        self.0
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CancelToken {
    fn drop(&mut self) {
        // SAFETY: released exactly once, from the owning handle.
        #[allow(unsafe_code)]
        unsafe {
            plaudon_apple_release_cancel_token(self.0);
        }
    }
}

/// Verify that the linked library speaks the ABI this build expects.
pub fn check_abi() -> Result<(), FfiError> {
    // SAFETY: a parameterless call returning a plain integer.
    #[allow(unsafe_code)]
    let found = unsafe { plaudon_apple_bridge_abi_version() };
    if found == EXPECTED_ABI_VERSION {
        Ok(())
    } else {
        Err(FfiError::AbiMismatch {
            found,
            expected: EXPECTED_ABI_VERSION,
        })
    }
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
    unsafe { plaudon_apple_string_free(pointer) };
    Ok(text)
}

/// Probe speech availability.
pub fn probe_speech(locale: Option<&str>) -> Result<BridgeDocument<ProbeDocument>, FfiError> {
    check_abi()?;
    let locale = locale
        .map(CString::new)
        .transpose()
        .map_err(|_| FfiError::InvalidArgument)?;
    let pointer = locale
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    // SAFETY: `pointer` is borrowed for the duration of the call only.
    #[allow(unsafe_code)]
    let raw = unsafe { plaudon_apple_probe_speech(pointer) };
    // SAFETY: `raw` came from the library and is freed inside.
    #[allow(unsafe_code)]
    let document = unsafe { take_document(raw) }?;
    Ok(BridgeDocument::parse(&document))
}

/// Probe on-device language model availability.
pub fn probe_foundation_model() -> Result<BridgeDocument<ProbeDocument>, FfiError> {
    check_abi()?;
    // SAFETY: a parameterless call returning an owned string.
    #[allow(unsafe_code)]
    let raw = unsafe { plaudon_apple_probe_foundation_model() };
    // SAFETY: `raw` came from the library and is freed inside.
    #[allow(unsafe_code)]
    let document = unsafe { take_document(raw) }?;
    Ok(BridgeDocument::parse(&document))
}

/// Transcribe a file.
pub fn transcribe_file(
    path: &str,
    locale: Option<&str>,
    cancel: &CancelToken,
    timeout: std::time::Duration,
) -> Result<BridgeDocument<TranscriptionDocument>, FfiError> {
    check_abi()?;
    let path = CString::new(path).map_err(|_| FfiError::InvalidArgument)?;
    let locale = locale
        .map(CString::new)
        .transpose()
        .map_err(|_| FfiError::InvalidArgument)?;
    let locale_pointer = locale
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    // SAFETY: both strings are borrowed for the duration of the call, and no
    // progress callback is registered.
    #[allow(unsafe_code)]
    let raw = unsafe {
        plaudon_apple_transcribe_file(
            path.as_ptr(),
            locale_pointer,
            cancel.raw(),
            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            None,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: `raw` came from the library and is freed inside.
    #[allow(unsafe_code)]
    let document = unsafe { take_document(raw) }?;
    Ok(BridgeDocument::parse(&document))
}

/// Generate text with the on-device model.
pub fn generate_text(
    prompt: &str,
    cancel: &CancelToken,
    timeout: std::time::Duration,
) -> Result<String, FfiError> {
    check_abi()?;
    let prompt = CString::new(prompt).map_err(|_| FfiError::InvalidArgument)?;
    // SAFETY: the prompt is borrowed for the duration of the call.
    #[allow(unsafe_code)]
    let raw = unsafe {
        plaudon_apple_generate_text(
            prompt.as_ptr(),
            cancel.raw(),
            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        )
    };
    // SAFETY: `raw` came from the library and is freed inside.
    #[allow(unsafe_code)]
    unsafe {
        take_document(raw)
    }
}
