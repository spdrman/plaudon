//! Trace output: one `write(2)` per line, to the file named by
//! `PLAUDON_EGRESS_LOG`.
//!
//! The file is opened once with `O_APPEND`, so independent processes that
//! inherit the injection (and therefore the monitor) can share one trace file
//! without interleaving partial lines: a single `write` of less than a page to
//! an `O_APPEND` regular file is not split by the kernel.
//!
//! Nothing here allocates, and nothing here calls back into an interposed
//! symbol: `open`, `write`, `time` and `getenv` are not intercepted.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use core::cell::Cell;
use core::ffi::{c_char, c_int};
use core::sync::atomic::{AtomicI32, Ordering};

use crate::wire::{format_attempt, Attempt, Call, LineBuf};

/// Environment variable naming the trace file.
pub const LOG_ENV: &[u8] = b"PLAUDON_EGRESS_LOG\0";

const FD_UNINITIALIZED: c_int = -2;
const FD_DISABLED: c_int = -1;

static LOG_FD: AtomicI32 = AtomicI32::new(FD_UNINITIALIZED);

thread_local! {
    /// Re-entrancy guard: if an intercepted function is reached from inside our
    /// own bookkeeping, we forward to libc without recording.
    static IN_MONITOR: Cell<bool> = const { Cell::new(false) };
}

/// Seconds since the Unix epoch, from `time(2)`; never panics, never allocates.
#[must_use]
pub fn now_unix() -> i64 {
    // SAFETY: `time(NULL)` reads nothing and writes nothing.
    unsafe { libc::time(core::ptr::null_mut()) }
}

fn log_fd() -> c_int {
    let cached = LOG_FD.load(Ordering::Acquire);
    if cached != FD_UNINITIALIZED {
        return cached;
    }
    // SAFETY: `LOG_ENV` is a NUL-terminated byte string.
    let path = unsafe { libc::getenv(LOG_ENV.as_ptr().cast::<c_char>()) };
    let opened = if path.is_null() {
        FD_DISABLED
    } else {
        // SAFETY: `path` is a NUL-terminated string owned by the environment.
        unsafe {
            libc::open(
                path,
                libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        }
    };
    match LOG_FD.compare_exchange(
        FD_UNINITIALIZED,
        opened,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => opened,
        Err(winner) => {
            if opened >= 0 {
                // SAFETY: we own `opened` and lost the race, so close it.
                unsafe { libc::close(opened) };
            }
            winner
        }
    }
}

fn write_all(fd: c_int, bytes: &[u8]) {
    let mut offset = 0usize;
    while offset < bytes.len() {
        // SAFETY: the pointer/length pair stays inside `bytes`.
        let written = unsafe {
            libc::write(
                fd,
                bytes.as_ptr().add(offset).cast::<core::ffi::c_void>(),
                bytes.len() - offset,
            )
        };
        if written > 0 {
            offset += usize::try_from(written).unwrap_or(bytes.len());
        } else {
            // EINTR is worth one more go; anything else means the trace file is
            // gone and there is nothing useful left to do here.
            // SAFETY: `errno` accessor, no arguments.
            let err = crate::platform::errno();
            if written < 0 && err == libc::EINTR {
                continue;
            }
            return;
        }
    }
}

/// Write one already-rendered line.
pub fn write_line(line: &LineBuf) {
    let fd = log_fd();
    if fd >= 0 {
        write_all(fd, line.as_bytes());
    }
}

/// Record one intercepted call.
pub fn record(attempt: &Attempt<'_>) {
    write_line(&format_attempt(attempt));
}

/// Run `body` with the re-entrancy guard held. When the guard is already held
/// (or thread-local state is being torn down) `body` is skipped entirely.
pub fn guarded(body: impl FnOnce()) {
    let entered = IN_MONITOR.try_with(|flag| {
        if flag.get() {
            false
        } else {
            flag.set(true);
            true
        }
    });
    if entered == Ok(true) {
        body();
        let _ = IN_MONITOR.try_with(|flag| flag.set(false));
    }
}

/// Emitted from the library constructor. Its presence in the trace is the only
/// accepted proof that injection actually happened.
pub fn monitor_ready() {
    guarded(|| {
        let mut attempt = Attempt::new(now_unix(), Call::MonitorReady);
        attempt.host_or_ip = crate::STATUS.label().as_bytes();
        record(&attempt);
    });
}
