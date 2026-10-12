//! Linux injection: `LD_PRELOAD` symbol override.
//!
//! The replacements below export the real libc symbol names, so the dynamic
//! linker binds the program to them first; the originals are recovered with
//! `dlsym(RTLD_NEXT, …)` and cached in an atomic pointer.
//!
//! Platform limit, stated plainly: `LD_PRELOAD` is ignored for setuid/setgid
//! binaries and for statically linked executables, and it says nothing about
//! other processes on the host.

#![cfg(target_os = "linux")]
#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::emit::monitor_ready;
use crate::platform::set_errno;
use crate::trace::{numeric_port, record_exec, record_name, record_sockaddr, record_socket};
use crate::wire::{Call, Verdict};

extern "C" fn plaudon_monitor_init() {
    monitor_ready();
}

#[used]
#[link_section = ".init_array"]
static MONITOR_INIT: extern "C" fn() = plaudon_monitor_init;

/// Resolve the next definition of `name` after this library, caching it.
fn real(cache: &AtomicPtr<c_void>, name: &[u8]) -> *mut c_void {
    let cached = cache.load(Ordering::Acquire);
    if !cached.is_null() {
        return cached;
    }
    // SAFETY: `name` is a NUL-terminated byte string.
    let resolved = unsafe { libc::dlsym(libc::RTLD_NEXT, name.as_ptr().cast::<c_char>()) };
    cache.store(resolved, Ordering::Release);
    resolved
}

macro_rules! real_fn {
    ($symbol:literal, $ty:ty) => {{
        static CACHE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
        let ptr = real(&CACHE, concat!($symbol, "\0").as_bytes());
        if ptr.is_null() {
            None
        } else {
            // SAFETY: `dlsym` returned the address of a function with this ABI.
            Some(unsafe { core::mem::transmute::<*mut c_void, $ty>(ptr) })
        }
    }};
}

const EAI_FAIL: c_int = -4;

type ConnectFn = unsafe extern "C" fn(c_int, *const libc::sockaddr, libc::socklen_t) -> c_int;
type SocketFn = unsafe extern "C" fn(c_int, c_int, c_int) -> c_int;
type SendtoFn = unsafe extern "C" fn(
    c_int,
    *const c_void,
    libc::size_t,
    c_int,
    *const libc::sockaddr,
    libc::socklen_t,
) -> libc::ssize_t;
type GetaddrinfoFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const libc::addrinfo,
    *mut *mut libc::addrinfo,
) -> c_int;
type GethostbynameFn = unsafe extern "C" fn(*const c_char) -> *mut libc::hostent;
type ResQueryFn =
    unsafe extern "C" fn(*const c_char, c_int, c_int, *mut libc::c_uchar, c_int) -> c_int;
type ExecveFn =
    unsafe extern "C" fn(*const c_char, *const *const c_char, *const *const c_char) -> c_int;
type ExecvpFn = unsafe extern "C" fn(*const c_char, *const *const c_char) -> c_int;
type PosixSpawnFn = unsafe extern "C" fn(
    *mut libc::pid_t,
    *const c_char,
    *const libc::posix_spawn_file_actions_t,
    *const libc::posix_spawnattr_t,
    *const *mut c_char,
    *const *mut c_char,
) -> c_int;

#[no_mangle]
pub unsafe extern "C" fn connect(
    socket: c_int,
    address: *const libc::sockaddr,
    len: libc::socklen_t,
) -> c_int {
    // SAFETY: libc contract — `address` is readable for `len` bytes.
    if unsafe { record_sockaddr(Call::Connect, address, len) } == Verdict::Deny {
        set_errno(libc::EPERM);
        return -1;
    }
    if let Some(f) = real_fn!("connect", ConnectFn) {
        // SAFETY: arguments are forwarded unchanged.
        unsafe { f(socket, address, len) }
    } else {
        set_errno(libc::ENOSYS);
        -1
    }
}

#[no_mangle]
pub unsafe extern "C" fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int {
    record_socket(domain);
    if let Some(f) = real_fn!("socket", SocketFn) {
        // SAFETY: arguments are forwarded unchanged.
        unsafe { f(domain, ty, protocol) }
    } else {
        set_errno(libc::ENOSYS);
        -1
    }
}

#[no_mangle]
pub unsafe extern "C" fn sendto(
    socket_fd: c_int,
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
    if let Some(f) = real_fn!("sendto", SendtoFn) {
        // SAFETY: arguments are forwarded unchanged.
        unsafe { f(socket_fd, buf, length, flags, dest_addr, dest_len) }
    } else {
        set_errno(libc::ENOSYS);
        -1
    }
}

#[no_mangle]
pub unsafe extern "C" fn getaddrinfo(
    node: *const c_char,
    service: *const c_char,
    hints: *const libc::addrinfo,
    res: *mut *mut libc::addrinfo,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    let port = unsafe { numeric_port(service) };
    // SAFETY: libc contract — NUL-terminated string or null.
    if unsafe { record_name(Call::Getaddrinfo, node, port) } == Verdict::Deny {
        return EAI_FAIL;
    }
    match real_fn!("getaddrinfo", GetaddrinfoFn) {
        // SAFETY: arguments are forwarded unchanged.
        Some(f) => unsafe { f(node, service, hints, res) },
        None => EAI_FAIL,
    }
}

#[no_mangle]
pub unsafe extern "C" fn gethostbyname(name: *const c_char) -> *mut libc::hostent {
    // SAFETY: libc contract — NUL-terminated string or null.
    if unsafe { record_name(Call::Gethostbyname, name, 0) } == Verdict::Deny {
        set_errno(libc::EPERM);
        return core::ptr::null_mut();
    }
    match real_fn!("gethostbyname", GethostbynameFn) {
        // SAFETY: arguments are forwarded unchanged.
        Some(f) => unsafe { f(name) },
        None => core::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn res_query(
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
    match real_fn!("res_query", ResQueryFn) {
        // SAFETY: arguments are forwarded unchanged.
        Some(f) => unsafe { f(dname, class, rtype, answer, anslen) },
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn execve(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(path) };
    if let Some(f) = real_fn!("execve", ExecveFn) {
        // SAFETY: arguments are forwarded unchanged.
        unsafe { f(path, argv, envp) }
    } else {
        set_errno(libc::ENOSYS);
        -1
    }
}

#[no_mangle]
pub unsafe extern "C" fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(file) };
    if let Some(f) = real_fn!("execvp", ExecvpFn) {
        // SAFETY: arguments are forwarded unchanged.
        unsafe { f(file, argv) }
    } else {
        set_errno(libc::ENOSYS);
        -1
    }
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn(
    pid: *mut libc::pid_t,
    path: *const c_char,
    file_actions: *const libc::posix_spawn_file_actions_t,
    attrp: *const libc::posix_spawnattr_t,
    argv: *const *mut c_char,
    envp: *const *mut c_char,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(path) };
    match real_fn!("posix_spawn", PosixSpawnFn) {
        // SAFETY: arguments are forwarded unchanged.
        Some(f) => unsafe { f(pid, path, file_actions, attrp, argv, envp) },
        None => libc::ENOSYS,
    }
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnp(
    pid: *mut libc::pid_t,
    file: *const c_char,
    file_actions: *const libc::posix_spawn_file_actions_t,
    attrp: *const libc::posix_spawnattr_t,
    argv: *const *mut c_char,
    envp: *const *mut c_char,
) -> c_int {
    // SAFETY: libc contract — NUL-terminated string or null.
    unsafe { record_exec(file) };
    match real_fn!("posix_spawnp", PosixSpawnFn) {
        // SAFETY: arguments are forwarded unchanged.
        Some(f) => unsafe { f(pid, file, file_actions, attrp, argv, envp) },
        None => libc::ENOSYS,
    }
}
