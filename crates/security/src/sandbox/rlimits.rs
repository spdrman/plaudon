//! POSIX resource limits applied to a helper child (§6.3A resource caps,
//! §16.2 row "Malicious audio decoder input").
//!
//! [`apply`] runs **in the child, between `fork` and `exec`**, where the only
//! legal operations are async-signal-safe ones. It therefore calls nothing but
//! `setrlimit`, allocates nothing, and formats nothing.
//!
//! # Platform truth
//!
//! * `RLIMIT_CPU` and `RLIMIT_NOFILE` work on macOS and Linux.
//! * `RLIMIT_AS` works on Linux. On macOS the kernel rejects any value below
//!   the address space already reserved by the shared cache (observed: values
//!   under ~1 TiB fail with `EINVAL`), so it is not a usable memory cap there.
//!   [`apply`] therefore *tries* it and tolerates failure; the parent learns
//!   the truth from the helper's `Hello` self-report and starts an external
//!   resident-set watchdog when the limit did not take.
//!
//! Failures are deliberately swallowed here instead of aborting the spawn: an
//! unavailable limit must downgrade the reported isolation, not break the
//! feature (§2.6/§3.1 feasibility-conditional capabilities degrade honestly).

use crate::sandbox::ipc::AppliedLimits;

/// Caps requested for a child process.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChildLimits {
    /// Address-space cap in bytes.
    pub(crate) address_space_bytes: u64,
    /// CPU-seconds cap.
    pub(crate) cpu_seconds: u64,
    /// Open-file-descriptor cap.
    pub(crate) open_files: u64,
}

/// Descriptor cap for a helper: enough for stdio, the granted files, and the
/// handful of descriptors the runtime opens, and far below anything a
/// descriptor-exhaustion bug could use.
pub(crate) const HELPER_OPEN_FILES: u64 = 64;

/// Apply the caps to the current process.
///
/// Called from `CommandExt::pre_exec`, i.e. in the forked child before `exec`.
/// Every call is a bare `setrlimit(2)`; results are ignored because an
/// unsupported limit must not prevent the helper from starting.
///
/// # Safety
///
/// `setrlimit` is async-signal-safe and this function allocates nothing, takes
/// no lock and touches no global state, which is what `pre_exec` requires.
#[allow(unsafe_code)]
pub(crate) fn apply(limits: ChildLimits) {
    #[cfg(target_os = "linux")]
    type Resource = libc::__rlimit_resource_t;
    #[cfg(not(target_os = "linux"))]
    type Resource = libc::c_int;

    // SAFETY: `setrlimit` only reads the `rlimit` value we pass by pointer and
    // is async-signal-safe. The pointer is to a live stack local.
    let set = |resource: Resource, value: u64| {
        // `rlim_t` is `u64` on every platform this crate targets, so the
        // requested value always fits.
        let rl = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        unsafe { libc::setrlimit(resource, &raw const rl) };
    };

    set(libc::RLIMIT_AS, limits.address_space_bytes);
    set(libc::RLIMIT_CPU, limits.cpu_seconds);
    set(libc::RLIMIT_NOFILE, limits.open_files);
}

/// Read back the limits of the current process for the `Hello` handshake.
#[allow(unsafe_code)]
#[must_use]
pub(crate) fn observe() -> AppliedLimits {
    #[cfg(target_os = "linux")]
    type Resource = libc::__rlimit_resource_t;
    #[cfg(not(target_os = "linux"))]
    type Resource = libc::c_int;

    fn get(resource: Resource) -> Option<u64> {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `getrlimit` writes only into the `rlimit` we own.
        let rc = unsafe { libc::getrlimit(resource, &raw mut rl) };
        if rc != 0 || rl.rlim_cur == libc::RLIM_INFINITY {
            return None;
        }
        Some(rl.rlim_cur)
    }

    AppliedLimits {
        address_space_bytes: get(libc::RLIMIT_AS),
        cpu_seconds: get(libc::RLIMIT_CPU),
        open_files: get(libc::RLIMIT_NOFILE),
    }
}
