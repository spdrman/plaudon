//! The two libc details that differ between the supported platforms.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use core::ffi::c_int;

#[cfg(target_os = "macos")]
fn errno_location() -> *mut c_int {
    // SAFETY: `__error()` returns the per-thread errno slot.
    unsafe { libc::__error() }
}

#[cfg(target_os = "linux")]
fn errno_location() -> *mut c_int {
    // SAFETY: `__errno_location()` returns the per-thread errno slot.
    unsafe { libc::__errno_location() }
}

/// Current `errno`.
#[must_use]
pub fn errno() -> c_int {
    // SAFETY: the slot is valid for the lifetime of the thread.
    unsafe { *errno_location() }
}

/// Set `errno`, so a denied call looks exactly like a kernel refusal.
pub fn set_errno(value: c_int) {
    // SAFETY: the slot is valid for the lifetime of the thread.
    unsafe { *errno_location() = value };
}
