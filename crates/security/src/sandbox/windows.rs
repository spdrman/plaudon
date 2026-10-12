//! Windows backend: a job object with resource and UI limits (§6.3A).
//!
//! # What is actually applied
//!
//! A job object created with [`CreateJobObjectW`] and configured through
//! `SetInformationJobObject` with:
//!
//! * `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`
//!   * `JOB_OBJECT_LIMIT_PROCESS_MEMORY` — committed memory cap per process;
//!   * `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 1` — the helper cannot multiply;
//!   * `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` — the helper dies with us, so a
//!     crashed parent cannot leave a decoder chewing on hostile input;
//!   * `JOB_OBJECT_LIMIT_JOB_TIME` — user-mode CPU budget for the whole job;
//! * `JOBOBJECT_BASIC_UI_RESTRICTIONS` — no desktop, clipboard, global atoms,
//!   handle inheritance from other processes, system parameters or
//!   `ExitWindows`.
//!
//! # Known limitations (§16.6)
//!
//! * **No filesystem or network restriction.** Doing that properly needs a
//!   restricted token (`CreateRestrictedToken` + `CreateProcessAsUserW`) or an
//!   AppContainer profile with explicit capability SIDs. Neither is
//!   implemented here, so [`crate::sandbox::IsolationReport::filesystem`],
//!   `network` and `exec` are reported as `ReducedIsolation`
//!   (`PrimitiveUnavailable`) on Windows. The job object is real and is
//!   reported as real; nothing else is claimed.
//! * **The child is not created suspended.** `CREATE_SUSPENDED` would let us
//!   assign the process to the job before its first instruction, but resuming
//!   it afterwards needs the main *thread* handle, which `std::process::Child`
//!   does not expose and which can only be recovered through
//!   `Win32_System_Diagnostics_ToolHelp` (or by driving `CreateProcessW` and
//!   `CreatePipe` directly via `Win32_System_Pipes`); none of those feature
//!   sets are enabled for `windows-sys` in this crate. Instead the helper is
//!   assigned to the job immediately after `spawn`, and the protocol closes
//!   the remaining window: a helper performs **no work at all** until it has
//!   read its first frame from stdin, and the parent does not write that frame
//!   until the job assignment has returned. The residual exposure is the
//!   process start-up path itself, which runs our own signed helper binary.

use std::ffi::{c_void, OsString};
use std::fmt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_TIME, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};

use super::spec::SandboxSpec;

/// UI-ready description of the Windows mechanism.
pub(super) const MECHANISM: &str = "capped by a Windows job object";

/// Windows has no probed strong filesystem primitive in V1.
pub(super) fn strong_available() -> bool {
    false
}

/// Build the launch command.
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

/// Resident size sampling is not used on Windows: the job object enforces the
/// memory cap in the kernel, so there is nothing for a watchdog to add.
pub(super) fn resident_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Owns the job object handle. Dropping it closes the handle, which — because
/// of `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` — terminates the helper.
pub(super) struct PlatformGuard {
    job: HANDLE,
}

impl fmt::Debug for PlatformGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlatformGuard")
            .field("job_assigned", &!self.job.is_null())
            .finish()
    }
}

impl Default for PlatformGuard {
    fn default() -> Self {
        Self {
            job: std::ptr::null_mut(),
        }
    }
}

impl Drop for PlatformGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if !self.job.is_null() {
            // SAFETY: `job` is a handle this type created and has not closed.
            unsafe { CloseHandle(self.job) };
            self.job = std::ptr::null_mut();
        }
    }
}

/// Create the job object, configure it, and assign the freshly spawned child.
///
/// Returns the guard plus which resource dimensions the kernel is now
/// enforcing. On any failure the guard is empty and both flags are `false`:
/// the caller then reports reduced isolation instead of claiming a cap.
#[allow(unsafe_code)]
pub(super) fn attach(spec: &SandboxSpec, child: &Child) -> (PlatformGuard, bool, bool) {
    // SAFETY: a null security descriptor and a null name request an unnamed
    // job object with default security; the call has no other preconditions.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return (PlatformGuard::default(), false, false);
    }
    let guard = PlatformGuard { job };

    let limits = extended_limit_information(spec);
    let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .unwrap_or(u32::MAX);
    // SAFETY: the information class matches the struct type and the length we
    // pass is that struct's size.
    let limits_ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast::<c_void>(),
            size,
        )
    } != 0;

    let ui = ui_restrictions();
    let ui_size =
        u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>()).unwrap_or(u32::MAX);
    // SAFETY: as above, class and struct match.
    let _ui_ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectBasicUIRestrictions,
            (&raw const ui).cast::<c_void>(),
            ui_size,
        )
    } != 0;

    let process: HANDLE = child.as_raw_handle().cast::<c_void>();
    // SAFETY: `process` is the live child handle owned by `child`, which
    // outlives this call, and `job` is our own open job handle.
    let assigned = unsafe { AssignProcessToJobObject(job, process) } != 0;

    if assigned {
        // Keep the guard: the job now owns the child's lifetime through
        // `KILL_ON_JOB_CLOSE`, so releasing it here would kill the helper.
        // The caps are only reported when they were actually installed.
        (guard, limits_ok, limits_ok)
    } else {
        // The child is not in the job, so closing the job affects nothing and
        // no cap is in force.
        drop(guard);
        (PlatformGuard::default(), false, false)
    }
}

/// The limit flags we install on every helper job object.
///
/// `PROCESS_MEMORY` caps committed memory per process, `ACTIVE_PROCESS` (with
/// `ActiveProcessLimit = 1`) stops the helper multiplying, `KILL_ON_JOB_CLOSE`
/// ties the helper's life to ours, and `JOB_TIME` caps user-mode CPU.
pub(super) const JOB_LIMIT_FLAGS: u32 = JOB_OBJECT_LIMIT_PROCESS_MEMORY
    | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    | JOB_OBJECT_LIMIT_JOB_TIME;

/// UI handles a media decoder or inference helper has no business touching.
pub(super) const JOB_UI_RESTRICTION_FLAGS: u32 = JOB_OBJECT_UILIMIT_DESKTOP
    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
    | JOB_OBJECT_UILIMIT_EXITWINDOWS
    | JOB_OBJECT_UILIMIT_GLOBALATOMS
    | JOB_OBJECT_UILIMIT_HANDLES
    | JOB_OBJECT_UILIMIT_READCLIPBOARD
    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD;

/// `PerJobUserTimeLimit` and friends count in 100-nanosecond units.
pub(super) const HUNDRED_NANOS_PER_SECOND: u64 = 10_000_000;

/// Build the extended limit information for `spec`.
///
/// Pure: spec in, struct out. Kept separate from [`attach`] so the field and
/// flag population is unit-testable without creating a process.
#[allow(unsafe_code)]
#[must_use]
pub(super) fn extended_limit_information(
    spec: &SandboxSpec,
) -> JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
    // SAFETY: the struct is plain old data with no pointer fields; zeroing is
    // the documented way to initialise it before filling in the fields the
    // `LimitFlags` bitmask selects.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_LIMIT_FLAGS;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.BasicLimitInformation.PerJobUserTimeLimit =
        i64::try_from(spec.cpu_seconds().saturating_mul(HUNDRED_NANOS_PER_SECOND))
            .unwrap_or(i64::MAX);
    limits.ProcessMemoryLimit = usize::try_from(spec.memory_bytes()).unwrap_or(usize::MAX);
    limits
}

/// Build the UI restrictions. Pure: no arguments, struct out.
#[must_use]
pub(super) fn ui_restrictions() -> JOBOBJECT_BASIC_UI_RESTRICTIONS {
    JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_UI_RESTRICTION_FLAGS,
    }
}
