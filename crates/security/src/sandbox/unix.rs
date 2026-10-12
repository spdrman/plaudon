//! Backend for unix platforms other than macOS (§6.3A, §2.6/§3.1 honest
//! degradation).
//!
//! What is applied here is exactly `setrlimit`: `RLIMIT_AS`, `RLIMIT_CPU` and
//! `RLIMIT_NOFILE`, plus the parent-side wall-clock deadline that is platform
//! independent. Filesystem, network and exec isolation are reported as
//! [`crate::sandbox::IsolationLevel::ReducedIsolation`] with
//! [`crate::sandbox::ReducedIsolationReason::PlatformUnsupported`].
//!
//! This is deliberate rather than a gap we are hiding: the usable primitives
//! here (Landlock, seccomp-bpf, namespaces, bubblewrap) differ by distribution
//! and kernel configuration, and shipping a Linux build is not in the V1 scope
//! of the desktop app (§6.4 targets macOS and Windows). Claiming
//! `Sandboxed` because a *typical* Linux can do it would be exactly the kind of
//! unverifiable promise §16.2A forbids. When a Linux target is added, this
//! module is where a probed Landlock ruleset belongs, reporting `Sandboxed`
//! only after `landlock_restrict_self` has actually returned success.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use super::spec::SandboxSpec;

/// There is no strong primitive on this platform.
pub(super) fn strong_available() -> bool {
    false
}

/// Build the launch command. `use_strong` is always `false` here.
pub(super) fn command_for(
    _spec: &SandboxSpec,
    program: &Path,
    args: &[OsString],
    _use_strong: bool,
) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

/// Resident set size in bytes from `/proc/<pid>/statm`, when procfs is present.
pub(super) fn resident_bytes(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page_size = page_size()?;
    Some(pages.saturating_mul(page_size))
}

#[allow(unsafe_code)]
fn page_size() -> Option<u64> {
    // SAFETY: `sysconf` takes an integer and returns an integer; it has no
    // pointer arguments and no side effects.
    let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(raw).ok().filter(|v| *v > 0)
}

/// No per-process kernel object is created on this platform; the resource
/// caps live in the child's own `rlimit`s and the deadline lives in the
/// parent, so there is nothing to own here.
#[derive(Debug, Default)]
pub(super) struct PlatformGuard;

/// Nothing is attached after spawn on this platform. The two flags report that
/// no kernel object is enforcing memory or CPU from the parent side; the
/// child's `Hello` self-report is what tells us whether `setrlimit` took.
pub(super) fn attach(
    _spec: &SandboxSpec,
    _child: &std::process::Child,
) -> (PlatformGuard, bool, bool) {
    (PlatformGuard, false, false)
}
