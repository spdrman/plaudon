//! macOS injection: dyld function interposing.
//!
//! dyld rebinds every reference to the original symbol in all *other* images to
//! the replacement listed in a `__DATA,__interpose` record, which is why the
//! replacements below can call the real function directly without recursing.
//!
//! Activated with `DYLD_INSERT_LIBRARIES=<path to this dylib>`. Note the
//! platform limit: dyld strips `DYLD_*` from the environment of restricted
//! (hardened-runtime, setuid, platform) binaries, so this monitor observes the
//! scenario process we build ourselves — never `/bin/sh`, never a system
//! daemon. The harness therefore treats a missing `monitor_ready` line as a
//! failure rather than assuming injection worked.

#![cfg(target_os = "macos")]
#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_uint, c_void};

use crate::emit::monitor_ready;
use crate::platform::set_errno;
use crate::trace::{numeric_port, record_exec, record_name, record_sockaddr, record_socket};
use crate::wire::{Call, Verdict};

#[repr(C)]
struct Interpose {
    replacement: *const c_void,
    original: *const c_void,
}

// SAFETY: the records are immutable function addresses read only by dyld.
unsafe impl Sync for Interpose {}

macro_rules! interpose {
    ($static_name:ident, $replacement:expr, $original:expr) => {
        #[used]
        #[link_section = "__DATA,__interpose"]
        static $static_name: Interpose = Interpose {
            replacement: $replacement as *const c_void,
            original: $original as *const c_void,
        };
    };
}

// ---------------------------------------------------------------------------
// Library constructor: proof of injection.
// ---------------------------------------------------------------------------

extern "C" fn plaudon_monitor_init() {
    monitor_ready();
}

#[used]
#[link_section = "__DATA,__mod_init_func"]
static MONITOR_INIT: extern "C" fn() = plaudon_monitor_init;

// ---------------------------------------------------------------------------
// macOS-only declarations the `libc` crate does not provide.
// ---------------------------------------------------------------------------

/// `struct sa_endpoints` from `<sys/socket.h>`. Field names are fixed by the
/// platform header.
#[repr(C)]
#[allow(clippy::struct_field_names)]
struct SaEndpoints {
    sae_srcif: c_uint,
    sae_srcaddr: *const libc::sockaddr,
    sae_srcaddrlen: libc::socklen_t,
    sae_dstaddr: *const libc::sockaddr,
    sae_dstaddrlen: libc::socklen_t,
}

extern "C" {
    fn connectx(
        socket: c_int,
        endpoints: *const SaEndpoints,
        associd: u32,
        flags: c_uint,
        iov: *const libc::iovec,
        iovcnt: c_uint,
        len: *mut libc::size_t,
        connid: *mut u32,
    ) -> c_int;

    fn gethostbyname(name: *const c_char) -> *mut libc::hostent;
}

#[link(name = "resolv")]
extern "C" {
    /// `res_query` as actually exported by `libresolv.9.dylib`. The public
    /// `<resolv.h>` name `res_query` is a macro for this symbol on macOS, so
    /// this — not `res_query` — is the binding that has to be interposed.
    fn res_9_query(
        dname: *const c_char,
        class: c_int,
        rtype: c_int,
        answer: *mut libc::c_uchar,
        anslen: c_int,
    ) -> c_int;

    /// `res_nquery`, the resolver-state form used by most modern callers.
    fn res_9_nquery(
        statp: *mut c_void,
        dname: *const c_char,
        class: c_int,
        rtype: c_int,
        answer: *mut libc::c_uchar,
        anslen: c_int,
    ) -> c_int;
}

/// `getaddrinfo` failure code used for a denied lookup (`EAI_FAIL`).
const EAI_FAIL: c_int = 4;

// ---------------------------------------------------------------------------
// Replacements
// ---------------------------------------------------------------------------

unsafe extern "C" fn plaudon_connect(
    socket: c_int,
    address: *const libc::sockaddr,
    len: libc::socklen_t,
) -> c_int {
    // SAFETY: libc contract — `address` is readable for `len` bytes.
    if unsafe { record_sockaddr(Call::Connect, address, len) } == Verdict::Deny {
        set_errno(libc::EPERM);
        return -1;
    }
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::connect(socket, address, len) }
}

unsafe extern "C" fn plaudon_connectx(
    socket: c_int,
    endpoints: *const SaEndpoints,
    associd: u32,
    flags: c_uint,
    iov: *const libc::iovec,
    iovcnt: c_uint,
    len: *mut libc::size_t,
    connid: *mut u32,
) -> c_int {
    let denied = if endpoints.is_null() {
        false
    } else {
        // SAFETY: libc contract — `endpoints` points to a `sa_endpoints`.
        let eps = unsafe { core::ptr::read_unaligned(endpoints) };
        // SAFETY: the destination is readable for `sae_dstaddrlen` bytes.
        let verdict =
            unsafe { record_sockaddr(Call::Connectx, eps.sae_dstaddr, eps.sae_dstaddrlen) };
        verdict == Verdict::Deny
    };
    if denied {
        set_errno(libc::EPERM);
        return -1;
    }
    // SAFETY: arguments are forwarded unchanged.
    unsafe { connectx(socket, endpoints, associd, flags, iov, iovcnt, len, connid) }
}

unsafe extern "C" fn plaudon_sendto(
    socket: c_int,
    buf: *const c_void,
    length: libc::size_t,
    flags: c_int,
    dest_addr: *const libc::sockaddr,
    dest_len: libc::socklen_t,
) -> libc::ssize_t {
    if !dest_addr.is_null() {
        // SAFETY: libc contract — readable for `dest_len` bytes.
        if unsafe { record_sockaddr(Call::Sendto, dest_addr, dest_len) } == Verdict::Deny {
            set_errno(libc::EPERM);
            return -1;
        }
    }
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::sendto(socket, buf, length, flags, dest_addr, dest_len) }
}

unsafe extern "C" fn plaudon_socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int {
    record_socket(domain);
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::socket(domain, ty, protocol) }
}

unsafe extern "C" fn plaudon_getaddrinfo(
    node: *const c_char,
    service: *const c_char,
    hints: *const libc::addrinfo,
    res: *mut *mut libc::addrinfo,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated strings or null.
    let port = unsafe { numeric_port(service) };
    // SAFETY: libc contract — NUL-terminated string or null.
    if unsafe { record_name(Call::Getaddrinfo, node, port) } == Verdict::Deny {
        return EAI_FAIL;
    }
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::getaddrinfo(node, service, hints, res) }
}

unsafe extern "C" fn plaudon_gethostbyname(name: *const c_char) -> *mut libc::hostent {
    // SAFETY: libc contract — NUL-terminated string or null.
    if unsafe { record_name(Call::Gethostbyname, name, 0) } == Verdict::Deny {
        set_errno(libc::EPERM);
        return core::ptr::null_mut();
    }
    // SAFETY: arguments are forwarded unchanged.
    unsafe { gethostbyname(name) }
}

unsafe extern "C" fn plaudon_res_query(
    dname: *const c_char,
    class: c_int,
    rtype: c_int,
    answer: *mut libc::c_uchar,
    anslen: c_int,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    if unsafe { record_name(Call::ResQuery, dname, 0) } == Verdict::Deny {
        set_errno(libc::EPERM);
        return -1;
    }
    // SAFETY: arguments are forwarded unchanged.
    unsafe { res_9_query(dname, class, rtype, answer, anslen) }
}

unsafe extern "C" fn plaudon_res_nquery(
    statp: *mut c_void,
    dname: *const c_char,
    class: c_int,
    rtype: c_int,
    answer: *mut libc::c_uchar,
    anslen: c_int,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    if unsafe { record_name(Call::ResQuery, dname, 0) } == Verdict::Deny {
        set_errno(libc::EPERM);
        return -1;
    }
    // SAFETY: arguments are forwarded unchanged.
    unsafe { res_9_nquery(statp, dname, class, rtype, answer, anslen) }
}

unsafe extern "C" fn plaudon_execve(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(path) };
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::execve(path, argv, envp) }
}

unsafe extern "C" fn plaudon_execvp(file: *const c_char, argv: *const *const c_char) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(file) };
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::execvp(file, argv) }
}

unsafe extern "C" fn plaudon_posix_spawn(
    pid: *mut libc::pid_t,
    path: *const c_char,
    file_actions: *const libc::posix_spawn_file_actions_t,
    attrp: *const libc::posix_spawnattr_t,
    argv: *const *mut c_char,
    envp: *const *mut c_char,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(path) };
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::posix_spawn(pid, path, file_actions, attrp, argv, envp) }
}

unsafe extern "C" fn plaudon_posix_spawnp(
    pid: *mut libc::pid_t,
    file: *const c_char,
    file_actions: *const libc::posix_spawn_file_actions_t,
    attrp: *const libc::posix_spawnattr_t,
    argv: *const *mut c_char,
    envp: *const *mut c_char,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(file) };
    // SAFETY: arguments are forwarded unchanged.
    unsafe { libc::posix_spawnp(pid, file, file_actions, attrp, argv, envp) }
}

interpose!(INTERPOSE_CONNECT, plaudon_connect, libc::connect);
interpose!(INTERPOSE_CONNECTX, plaudon_connectx, connectx);
interpose!(INTERPOSE_SENDTO, plaudon_sendto, libc::sendto);
interpose!(INTERPOSE_SOCKET, plaudon_socket, libc::socket);
interpose!(
    INTERPOSE_GETADDRINFO,
    plaudon_getaddrinfo,
    libc::getaddrinfo
);
interpose!(
    INTERPOSE_GETHOSTBYNAME,
    plaudon_gethostbyname,
    gethostbyname
);
interpose!(INTERPOSE_RES_QUERY, plaudon_res_query, res_9_query);
interpose!(INTERPOSE_RES_NQUERY, plaudon_res_nquery, res_9_nquery);
interpose!(INTERPOSE_EXECVE, plaudon_execve, libc::execve);
interpose!(INTERPOSE_EXECVP, plaudon_execvp, libc::execvp);
interpose!(
    INTERPOSE_POSIX_SPAWN,
    plaudon_posix_spawn,
    libc::posix_spawn
);
interpose!(
    INTERPOSE_POSIX_SPAWNP,
    plaudon_posix_spawnp,
    libc::posix_spawnp
);
