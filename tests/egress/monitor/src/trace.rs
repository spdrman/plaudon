//! Translation from raw libc arguments into [`crate::wire`] values.
//!
//! Shared by the macOS dyld-interpose build and the Linux `LD_PRELOAD` build,
//! which differ only in how the replacement functions are hooked up.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use core::ffi::{c_char, c_int};

use crate::emit::{guarded, now_unix, record};
use crate::wire::{
    classify_addr, classify_host, render_addr, Addr, Attempt, Call, Family, Verdict,
};

/// Longest `sun_path`/name we will copy out of a raw pointer.
const NAME_SCAN_LIMIT: usize = 1024;

/// Read a NUL-terminated string, bounded by `limit` bytes.
///
/// # Safety
/// `ptr` must be null or point to at least one readable byte, with a NUL
/// terminator within `limit` bytes or `limit` readable bytes.
pub unsafe fn cstr_bytes<'a>(ptr: *const c_char, limit: usize) -> &'a [u8] {
    if ptr.is_null() {
        return &[];
    }
    let bytes = ptr.cast::<u8>();
    let mut len = 0usize;
    while len < limit {
        // SAFETY: guaranteed readable by the caller contract.
        if unsafe { *bytes.add(len) } == 0 {
            break;
        }
        len += 1;
    }
    // SAFETY: `len` bytes are readable and outlive this borrow (the caller uses
    // the slice before returning to its caller).
    unsafe { core::slice::from_raw_parts(bytes, len) }
}

/// Read the address family out of a `sockaddr` without assuming the whole
/// struct is readable.
///
/// # Safety
/// `addr` must be non-null and at least two bytes readable.
unsafe fn read_family(addr: *const libc::sockaddr) -> c_int {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `sa_len` is byte 0, `sa_family` is byte 1.
        c_int::from(unsafe { core::ptr::read_unaligned(addr.cast::<u8>().add(1)) })
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `sa_family` is a `u16` at offset 0.
        c_int::from(unsafe { core::ptr::read_unaligned(addr.cast::<u16>()) })
    }
}

/// What a `sockaddr` denotes, as far as this monitor is concerned.
struct Parsed {
    family: Family,
    addr: Option<Addr>,
    port: u16,
}

/// # Safety
/// `addr` must be readable for `len` bytes, or null.
unsafe fn parse_sockaddr(addr: *const libc::sockaddr, len: libc::socklen_t) -> Option<Parsed> {
    if addr.is_null() || len < 2 {
        return None;
    }
    // SAFETY: at least two bytes are readable.
    let raw_family = unsafe { read_family(addr) };
    let family = Family::from_raw(raw_family);
    let len = usize::try_from(len).unwrap_or(0);

    if raw_family == libc::AF_INET && len >= core::mem::size_of::<libc::sockaddr_in>() {
        // SAFETY: the caller guarantees `len` readable bytes.
        let sin = unsafe { core::ptr::read_unaligned(addr.cast::<libc::sockaddr_in>()) };
        return Some(Parsed {
            family,
            addr: Some(Addr::V4(sin.sin_addr.s_addr.to_ne_bytes())),
            port: u16::from_be(sin.sin_port),
        });
    }
    if raw_family == libc::AF_INET6 && len >= core::mem::size_of::<libc::sockaddr_in6>() {
        // SAFETY: the caller guarantees `len` readable bytes.
        let sin6 = unsafe { core::ptr::read_unaligned(addr.cast::<libc::sockaddr_in6>()) };
        return Some(Parsed {
            family,
            addr: Some(Addr::V6(sin6.sin6_addr.s6_addr)),
            port: u16::from_be(sin6.sin6_port),
        });
    }
    Some(Parsed {
        family,
        addr: None,
        port: 0,
    })
}

/// Record one address-carrying call and decide whether to forward it.
///
/// Only `AF_INET`/`AF_INET6` destinations outside loopback are denied: other
/// families (`AF_UNIX`, `AF_SYSTEM`, routing sockets…) carry no internet
/// traffic, so refusing them would break the host without proving anything.
///
/// # Safety
/// `addr` must be readable for `len` bytes, or null.
pub unsafe fn record_sockaddr(
    call: Call,
    addr: *const libc::sockaddr,
    len: libc::socklen_t,
) -> Verdict {
    // SAFETY: forwarded caller contract.
    let Some(parsed) = (unsafe { parse_sockaddr(addr, len) }) else {
        return Verdict::Permit;
    };

    let verdict = match parsed.addr {
        Some(ip) if parsed.family.is_internet() => classify_addr(ip),
        // An internet family we could not decode is refused: failing closed is
        // the only honest default for a deny fixture.
        None if parsed.family.is_internet() => Verdict::Deny,
        _ => Verdict::Permit,
    };

    let mut rendered = [0u8; 46];
    let host_len = parsed.addr.map_or(0, |ip| render_addr(ip, &mut rendered));
    let unix_path: &[u8] = if parsed.family == Family::Unix {
        // SAFETY: `sun_path` starts at offset 2 and is NUL-terminated within
        // the `len` bytes the caller guaranteed.
        unsafe { cstr_bytes(addr.cast::<c_char>().add(2), len as usize) }
    } else {
        &[]
    };

    guarded(|| {
        let mut attempt = Attempt::new(now_unix(), call);
        attempt.family = parsed.family;
        attempt.host_or_ip = if unix_path.is_empty() {
            &rendered[..host_len]
        } else {
            unix_path
        };
        attempt.port = parsed.port;
        attempt.denied = verdict.denied();
        record(&attempt);
    });
    verdict
}

/// Record one resolver call keyed by name and decide whether to forward it.
///
/// # Safety
/// `name` must be null or a NUL-terminated string.
pub unsafe fn record_name(call: Call, name: *const c_char, port: u16) -> Verdict {
    // SAFETY: forwarded caller contract.
    let bytes = unsafe { cstr_bytes(name, NAME_SCAN_LIMIT) };
    let verdict = if name.is_null() {
        Verdict::Permit
    } else {
        classify_host(bytes)
    };
    guarded(|| {
        let mut attempt = Attempt::new(now_unix(), call);
        attempt.host_or_ip = bytes;
        attempt.port = port;
        attempt.denied = verdict.denied();
        record(&attempt);
    });
    verdict
}

/// Record a socket creation. Creating a socket transmits nothing, so it is
/// always forwarded; denial happens at `connect`/`sendto`/resolution.
pub fn record_socket(domain: c_int) {
    guarded(|| {
        let mut attempt = Attempt::new(now_unix(), Call::Socket);
        attempt.family = Family::from_raw(domain);
        record(&attempt);
    });
}

/// Record a process execution. Executions are observed, never blocked: the
/// claim under test is "the app never launches the cloud CLI", which is proved
/// by the absence of a record, not by the monitor preventing it.
///
/// # Safety
/// `path` must be null or a NUL-terminated string.
pub unsafe fn record_exec(path: *const c_char) {
    // SAFETY: forwarded caller contract.
    let bytes = unsafe { cstr_bytes(path, NAME_SCAN_LIMIT) };
    guarded(|| {
        let mut attempt = Attempt::new(now_unix(), Call::Exec);
        attempt.program = bytes;
        record(&attempt);
    });
}

/// Parse a `getaddrinfo` service string that is already numeric. Named
/// services are reported as port 0 rather than guessed.
///
/// # Safety
/// `service` must be null or a NUL-terminated string.
pub unsafe fn numeric_port(service: *const c_char) -> u16 {
    // SAFETY: forwarded caller contract.
    let bytes = unsafe { cstr_bytes(service, 16) };
    let mut value: u32 = 0;
    if bytes.is_empty() {
        return 0;
    }
    for &b in bytes {
        if !b.is_ascii_digit() {
            return 0;
        }
        value = value * 10 + u32::from(b - b'0');
        if value > u32::from(u16::MAX) {
            return 0;
        }
    }
    u16::try_from(value).unwrap_or(0)
}
