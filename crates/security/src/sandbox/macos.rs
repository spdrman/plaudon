//! macOS backend: Seatbelt profile plus POSIX resource caps (§6.3A).
//!
//! # What is actually applied
//!
//! | Dimension | Primitive | Notes |
//! |---|---|---|
//! | filesystem | Seatbelt `(deny default)` profile via `/usr/bin/sandbox-exec -p` | only the granted paths plus the system read paths the dynamic loader needs |
//! | network | Seatbelt `(deny network*)` | |
//! | exec | Seatbelt `(allow process-exec (literal <helper>))` | nothing else may be executed |
//! | cpu | `setrlimit(RLIMIT_CPU)` in the child before `exec` | the kernel sends `SIGXCPU` |
//! | memory | parent-side resident-set watchdog | `RLIMIT_AS` is not usable here, see below |
//! | wall clock | parent-side monotonic deadline plus `kill` | |
//!
//! # Known limitations (§16.6: say what is not guaranteed)
//!
//! * **`sandbox-exec` is deprecated by Apple.** The `sandbox_init(3)` family
//!   has carried a deprecation notice for years and the CLI may be removed or
//!   further restricted in a future macOS release. We therefore *probe* it at
//!   run time rather than assuming it: if the binary is missing, or a trivial
//!   profile fails to run, or the helper cannot start under the profile, the
//!   backend reports [`crate::sandbox::IsolationLevel::ReducedIsolation`] with
//!   [`crate::sandbox::ReducedIsolationReason::PrimitiveUnavailable`] and still
//!   applies the resource caps. The long-term path is a separately bundled,
//!   signed and *entitled* helper executable running under the App Sandbox
//!   (`com.apple.security.app-sandbox` with scoped-bookmark file access),
//!   which requires the app to be packaged and signed and so cannot be done
//!   from a plain `cargo test` build. That migration is tracked as a known
//!   limitation of this module, not as something this code pretends to do.
//! * **`RLIMIT_AS`/`RLIMIT_DATA` are not usable memory caps on macOS.** The
//!   kernel rejects values below the address space the dyld shared cache
//!   already reserves (measured on macOS 26: a 4 GiB cap fails with `EINVAL`).
//!   The memory cap is therefore enforced by the parent sampling the child's
//!   resident size through `proc_pidinfo(PROC_PIDTASKINFO)` and killing it —
//!   real enforcement, but with a sampling delay, which the report names.
//! * A Seatbelt profile restricts the *helper*. It says nothing about other
//!   processes on the machine, and nothing about kernel bugs.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use super::spec::SandboxSpec;

/// UI-ready description of the strong macOS mechanism.
pub(super) const MECHANISM: &str = "restricted by a macOS Seatbelt profile (sandbox-exec)";

/// Absolute path of the Seatbelt driver.
pub(super) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Read paths every dynamically linked Mach-O binary needs in order to start.
///
/// Verified empirically: a `(deny default)` profile that omits the root
/// directory literal aborts the process before `main`.
const BASE_READS: &[(&str, bool)] = &[
    ("/", false),
    ("/usr/lib", true),
    ("/usr/share", true),
    ("/System", true),
    ("/private/var/db/dyld", true),
    ("/dev/null", false),
    ("/dev/urandom", false),
];

/// Is the strong filesystem/network/exec primitive usable on this machine?
///
/// Probes once per process: the binary must exist and a profile built from the
/// same template must actually run a trivial program.
pub(super) fn strong_available() -> bool {
    static AVAILABLE: LazyLock<bool> = LazyLock::new(|| {
        if !Path::new(SANDBOX_EXEC).is_file() {
            return false;
        }
        let probe_target = Path::new("/usr/bin/true");
        if !probe_target.is_file() {
            return false;
        }
        let profile = profile_for(probe_target, &[], &[]);
        Command::new(SANDBOX_EXEC)
            .arg("-p")
            .arg(&profile)
            .arg(probe_target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    *AVAILABLE
}

/// Build the Seatbelt profile for one helper invocation.
pub(super) fn profile_for(program: &Path, reads: &[&Path], writes: &[&Path]) -> String {
    let mut profile = String::with_capacity(1024);
    profile.push_str("(version 1)\n(deny default)\n(deny network*)\n(deny process-exec*)\n");
    profile.push_str("(allow sysctl-read)\n(allow mach-lookup)\n(allow file-read-metadata)\n");
    profile.push_str("(allow signal (target self))\n");

    profile.push_str("(allow file-read*");
    for (path, is_dir) in BASE_READS {
        push_filter(&mut profile, Path::new(path), *is_dir);
    }
    push_filter(&mut profile, program, false);
    for path in reads {
        push_filter(&mut profile, path, path.is_dir());
    }
    profile.push_str(")\n");

    if !writes.is_empty() {
        profile.push_str("(allow file-write*");
        for path in writes {
            push_filter(&mut profile, path, path.is_dir());
        }
        profile.push_str(")\n");
    }
    // `/dev/null` must stay writable: a child that cannot open it crashes in
    // places that are hard to diagnose, and it leaks nothing.
    profile.push_str("(allow file-write-data (literal \"/dev/null\"))\n");

    // The helper itself is the only program that may be executed. This rule
    // comes last so it overrides `(deny process-exec*)` above for that one
    // path, and for nothing else.
    profile.push_str("(allow process-exec");
    push_filter(&mut profile, program, false);
    profile.push_str(")\n");

    profile
}

fn push_filter(profile: &mut String, path: &Path, as_subpath: bool) {
    profile.push_str(if as_subpath {
        " (subpath \""
    } else {
        " (literal \""
    });
    escape_into(profile, &path.to_string_lossy());
    profile.push_str("\")");
}

/// Escape a path for an SBPL string literal.
fn escape_into(out: &mut String, raw: &str) {
    for ch in raw.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
}

/// Build the `Command` that launches the helper, with or without Seatbelt.
pub(super) fn command_for(
    spec: &SandboxSpec,
    program: &Path,
    args: &[OsString],
    use_seatbelt: bool,
) -> Command {
    if use_seatbelt {
        let reads: Vec<&Path> = spec.read_paths().iter().map(PathBuf::as_path).collect();
        let writes: Vec<&Path> = spec.write_paths().iter().map(PathBuf::as_path).collect();
        let profile = profile_for(program, &reads, &writes);
        let mut command = Command::new(SANDBOX_EXEC);
        command.arg("-p").arg(profile).arg(program).args(args);
        command
    } else {
        let mut command = Command::new(program);
        command.args(args);
        command
    }
}

/// Resident set size of a live process in bytes, or `None` when the process is
/// gone or the query failed.
#[allow(unsafe_code)]
pub(super) fn resident_bytes(pid: u32) -> Option<u64> {
    let mut info = libc::proc_taskinfo {
        pti_virtual_size: 0,
        pti_resident_size: 0,
        pti_total_user: 0,
        pti_total_system: 0,
        pti_threads_user: 0,
        pti_threads_system: 0,
        pti_policy: 0,
        pti_faults: 0,
        pti_pageins: 0,
        pti_cow_faults: 0,
        pti_messages_sent: 0,
        pti_messages_received: 0,
        pti_syscalls_mach: 0,
        pti_syscalls_unix: 0,
        pti_csw: 0,
        pti_threadnum: 0,
        pti_numrunning: 0,
        pti_priority: 0,
    };
    let size = i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    let pid = i32::try_from(pid).ok()?;
    // SAFETY: `proc_pidinfo` writes at most `size` bytes into the buffer we
    // own and whose size we pass. It is read-only with respect to the target
    // process and tolerates a dead pid by returning a non-positive result.
    let rc = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            (&raw mut info).cast::<libc::c_void>(),
            size,
        )
    };
    if rc == size {
        Some(info.pti_resident_size)
    } else {
        None
    }
}

/// No per-process kernel object is created on macOS: the Seatbelt profile is
/// applied by `sandbox-exec` before `exec` and the resource caps live in the
/// child's own `rlimit`s, so there is nothing for the parent to own here.
#[derive(Debug, Default)]
pub(super) struct PlatformGuard;

/// Nothing is attached after spawn on macOS. The flags report that no kernel
/// object is enforcing memory or CPU on the parent's behalf; the child's
/// `Hello` self-report and the resident-set watchdog decide the truth.
pub(super) fn attach(
    _spec: &SandboxSpec,
    _child: &std::process::Child,
) -> (PlatformGuard, bool, bool) {
    (PlatformGuard, false, false)
}
