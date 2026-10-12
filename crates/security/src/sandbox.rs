//! Out-of-process isolation for media decoders and model inference helpers
//! (§6.3A, §16.2 row "Malicious audio decoder input", §14/§14.7).
//!
//! # Why a separate process at all
//!
//! §6.3A names downloaded model files and local/vendor audio files as
//! **untrusted input domains**, and states plainly that "pure Rust memory
//! safety does not make untrusted codecs or native C/C++ libraries safe". A
//! decoder is a parser for attacker-chosen bytes, frequently backed by C. The
//! control is therefore not "write careful Rust", it is: run the parser in a
//! different process, give that process the narrowest set of OS rights we can
//! actually apply, cap what it can consume, and treat its death as a normal,
//! recoverable job state rather than an application crash.
//!
//! # What this module provides
//!
//! * [`SandboxSpec`] — the restrictions we *ask* for (paths, memory, CPU, wall
//!   clock; network and further `exec` are not grantable in V1).
//! * [`IsolationReport`] — what was *actually applied*, per dimension. A
//!   dimension is only reported as [`IsolationLevel::Sandboxed`] when the
//!   primitive was applied and the application succeeded; everything else is
//!   [`IsolationLevel::ReducedIsolation`] with a reason. There is no
//!   "probably", no "best effort", and no silent success (§16.6, §2.6/§3.1:
//!   feasibility-conditional capabilities degrade honestly and visibly).
//! * [`ipc`] — a typed, length-prefixed, pointer-free protocol over the
//!   child's stdin/stdout.
//! * [`SandboxedHelper`] / [`HelperHandle`] — spawn, request, kill, wait.
//! * [`HelperOutcome`] — the classification the job engine needs so that a
//!   killed helper becomes a persisted `FailedRetryable`/`FailedTerminal`
//!   state and **never** a false `Completed` (§14.1, §14.7).
//!
//! # Threat model, stated honestly
//!
//! The sandbox reduces what a *compromised helper* can reach. It does not:
//!
//! * protect against kernel vulnerabilities;
//! * protect data the helper is legitimately granted (the input file it is
//!   decoding and the output file it is writing);
//! * say anything about other processes on the machine, or about the user's
//!   own account privileges;
//! * on macOS, survive Apple removing `sandbox-exec` — which is why the
//!   primitive is probed at run time and the report downgrades itself.
//!
//! Per-platform specifics and their limitations are documented on the backend
//! modules: `macos` (Seatbelt + `setrlimit` + resident-set watchdog),
//! `windows` (job object), `unix` (`setrlimit` only).
//!
//! # Example
//!
//! ```no_run
//! use std::ffi::OsString;
//! use std::path::Path;
//! use std::time::Duration;
//! use plaudon_security::sandbox::{HelperOutcome, SandboxSpec, SandboxedHelper};
//! use plaudon_security::sandbox::ipc::HelperRequest;
//!
//! let spec = SandboxSpec::builder()
//!     .read_path("/tmp/input.wav")
//!     .write_path("/tmp/output.pcm")
//!     .memory_bytes(512 * 1024 * 1024)
//!     .cpu_seconds(30)
//!     .wall_clock(Duration::from_secs(60))
//!     .build()?;
//!
//! let mut helper = SandboxedHelper::spawn(&spec, Path::new("/usr/local/bin/decoder"), &[])?;
//! println!("isolation: {}", helper.isolation.summary());
//!
//! let outcome = HelperOutcome::from(
//!     helper.request(HelperRequest::ReadFile { path: "/tmp/input.wav".into() }),
//! );
//! match outcome {
//!     HelperOutcome::Completed(response) => println!("{response:?}"),
//!     // Persist a wait/failure state; never report the job as completed.
//!     HelperOutcome::Recoverable(err) | HelperOutcome::Terminal(err) => eprintln!("{err}"),
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod ipc;
pub mod spec;

#[cfg(unix)]
pub(crate) mod rlimits;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(unix, not(target_os = "macos")))]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(all(unix, not(target_os = "macos")))]
use unix as backend;
#[cfg(windows)]
use windows as backend;

use std::ffi::OsString;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{self, ErrorEnvelopeParts, SecurityError};
use crate::outbound::subprocess::scrubbed_env;

pub use ipc::{AppliedLimits, FrameError, HelperRequest, HelperResponse, Operation};
pub use spec::{ExecAccess, NetworkAccess, SandboxSpec, SandboxSpecBuilder, SpecError};

/// How often the parent samples a helper's resident set when it has to enforce
/// the memory cap itself.
pub const WATCHDOG_INTERVAL: Duration = Duration::from_millis(20);

/// Longest the parent waits for the `Hello` handshake before giving up on a
/// helper (bounded separately from the job's wall-clock budget so a long job
/// budget does not mean a long hang on a helper that cannot start).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest the parent waits for a child to be reaped after it has closed its
/// pipes or been killed.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Bytes of helper stderr retained for diagnosis. Helper stderr is a
/// diagnostic channel only; it never carries results.
const STDERR_CAPTURE_BYTES: usize = 8 * 1024;

// ---------------------------------------------------------------------------
// Isolation reporting
// ---------------------------------------------------------------------------

/// Exactly why a dimension is not isolated. "Something went wrong" is not an
/// option: the UI and the diagnostics bundle get the specific reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReducedIsolationReason {
    /// The platform has a primitive for this, but it is not usable here: the
    /// binary is missing, the probe failed, or the restriction could not be
    /// applied on this machine.
    PrimitiveUnavailable,
    /// This platform has no primitive we are willing to claim for this
    /// dimension.
    PlatformUnsupported,
    /// The primitive exists and was attempted, and the OS refused us.
    PermissionDenied,
    /// The only available primitive is deprecated by its vendor and we refuse
    /// to present it as a durable guarantee.
    DeprecatedApi,
}

impl ReducedIsolationReason {
    /// UI-ready sentence fragment.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::PrimitiveUnavailable => {
                "not restricted: the operating system restriction this build uses is unavailable \
                 on this machine"
            }
            Self::PlatformUnsupported => {
                "not restricted: this platform has no restriction we can apply and verify"
            }
            Self::PermissionDenied => {
                "not restricted: the operating system refused to apply the restriction"
            }
            Self::DeprecatedApi => {
                "not restricted: the only available restriction is deprecated by the platform \
                 vendor and is not treated as a guarantee"
            }
        }
    }
}

/// Isolation state of one dimension (filesystem, network, further `exec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsolationLevel {
    /// The named primitive was applied to this child and the application
    /// succeeded. This variant is never constructed speculatively.
    Sandboxed {
        /// UI-ready description of the mechanism that is in force.
        mechanism: &'static str,
    },
    /// The dimension is not restricted, for the named reason.
    ReducedIsolation {
        /// Why.
        reason: ReducedIsolationReason,
    },
}

impl IsolationLevel {
    /// Convenience constructor for an unrestricted dimension.
    #[must_use]
    pub const fn reduced(reason: ReducedIsolationReason) -> Self {
        Self::ReducedIsolation { reason }
    }

    /// `true` only when a restriction is actually in force.
    #[must_use]
    pub const fn is_isolated(self) -> bool {
        matches!(self, Self::Sandboxed { .. })
    }

    /// UI-ready description of this dimension's state.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Sandboxed { mechanism } => mechanism,
            Self::ReducedIsolation { reason } => reason.describe(),
        }
    }

    /// The reason the dimension is unrestricted, if it is.
    #[must_use]
    pub const fn reduced_reason(self) -> Option<ReducedIsolationReason> {
        match self {
            Self::Sandboxed { .. } => None,
            Self::ReducedIsolation { reason } => Some(reason),
        }
    }
}

/// Which mechanism is holding the memory cap, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryCapMechanism {
    /// Nothing caps the helper's memory.
    None,
    /// `setrlimit(RLIMIT_AS)` was applied in the child and took effect.
    ResourceLimit,
    /// The parent samples the child's resident set and kills it on breach.
    /// Enforcement is real but has the stated sampling delay.
    ParentWatchdog {
        /// Sampling period in milliseconds.
        sample_interval_ms: u64,
    },
    /// A Windows job object enforces the cap in the kernel.
    JobObject,
}

impl MemoryCapMechanism {
    /// `true` when some mechanism actually caps memory.
    #[must_use]
    pub const fn is_enforced(self) -> bool {
        !matches!(self, Self::None)
    }

    /// UI-ready description.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::None => "memory is not capped",
            Self::ResourceLimit => "memory capped by a kernel address-space limit",
            Self::ParentWatchdog { .. } => {
                "memory capped by the parent sampling the helper's resident size and terminating \
                 it on breach (enforced with a short sampling delay, not instantaneously)"
            }
            Self::JobObject => "memory capped by a Windows job object",
        }
    }
}

// ---------------------------------------------------------------------------
// Machine-readable limitation notes
// ---------------------------------------------------------------------------

/// macOS: the strong primitive is a deprecated Apple interface.
pub const NOTE_MACOS_SANDBOX_EXEC_DEPRECATED: &str =
    "macOS: filesystem, network and execution are restricted through \
     /usr/bin/sandbox-exec, an interface Apple has deprecated. It is probed at \
     every launch, and this report degrades to reduced isolation the moment it \
     stops working. The durable replacement is a separately signed, entitled \
     helper running under the App Sandbox.";

/// macOS: there is no hard kernel memory cap available to us.
pub const NOTE_MACOS_NO_KERNEL_MEMORY_CAP: &str =
    "macOS: the kernel rejects RLIMIT_AS and RLIMIT_DATA below the address \
     space the dyld shared cache already reserves, so no hard kernel memory \
     cap is applied. The parent samples the helper's resident size and \
     terminates it on breach, which means enforcement has a sampling delay.";

/// macOS: the Seatbelt profile could not be used for this launch.
pub const NOTE_MACOS_NO_PROFILE: &str =
    "macOS: no Seatbelt profile is in force for this helper, so its filesystem \
     and network access is that of the signed-in user.";

/// Windows: the helper is assigned to the job object just after creation.
pub const NOTE_WINDOWS_ASSIGN_AFTER_SPAWN: &str =
    "Windows: the helper is assigned to its job object immediately after \
     creation rather than being created suspended, because recovering the main \
     thread handle needed to resume a suspended child is not available to this \
     build. The remaining window carries no untrusted work: the helper does \
     nothing until it has read its first IPC frame, and the parent sends that \
     frame only after the job assignment has succeeded.";

/// Windows: no filesystem or network restriction is applied in V1.
pub const NOTE_WINDOWS_NO_FILESYSTEM_RESTRICTION: &str =
    "Windows: V1 creates no restricted token and no AppContainer, so the \
     helper's filesystem and network access is that of the signed-in user. \
     Only the job object's resource, process-count and UI limits are enforced.";

/// Other unix: resource limits only.
pub const NOTE_RESOURCE_CAPS_ONLY: &str =
    "This platform applies POSIX resource limits only; filesystem, network and \
     further process execution are not restricted.";

/// What was actually applied to one helper process, per dimension.
///
/// Dimensions are reported separately on purpose. A backend that can cap
/// resources but cannot restrict the filesystem must say exactly that rather
/// than summarising itself as "sandboxed" (§16.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolationReport {
    /// A memory cap is in force; see [`IsolationReport::memory_mechanism`].
    pub memory: bool,
    /// A CPU-time cap is in force.
    pub cpu: bool,
    /// A wall-clock budget is in force (always parent-side).
    pub wall_clock: bool,
    /// Filesystem restriction state.
    pub filesystem: IsolationLevel,
    /// Network restriction state.
    pub network: IsolationLevel,
    /// Further-`exec` restriction state.
    pub exec: IsolationLevel,
    /// Which mechanism holds the memory cap. `memory` is `true` exactly when
    /// this is not [`MemoryCapMechanism::None`].
    pub memory_mechanism: MemoryCapMechanism,
    /// Limitations of *this* launch that the flags above cannot express, in
    /// machine-readable form so the UI and the diagnostics bundle carry them
    /// instead of a reader having to find a comment in the backend (§16.6).
    ///
    /// Entries come from the `NOTE_*` constants in this module.
    pub notes: &'static [&'static str],
}

impl IsolationReport {
    /// A report in which nothing is restricted, for the named reason. Used as
    /// the starting point so that every `true`/`Sandboxed` has to be earned.
    #[must_use]
    pub const fn nothing_applied(reason: ReducedIsolationReason) -> Self {
        Self {
            memory: false,
            cpu: false,
            wall_clock: false,
            filesystem: IsolationLevel::reduced(reason),
            network: IsolationLevel::reduced(reason),
            exec: IsolationLevel::reduced(reason),
            memory_mechanism: MemoryCapMechanism::None,
            notes: &[],
        }
    }

    /// `true` only when every dimension is restricted.
    #[must_use]
    pub const fn is_fully_isolated(&self) -> bool {
        self.memory
            && self.cpu
            && self.wall_clock
            && self.filesystem.is_isolated()
            && self.network.is_isolated()
            && self.exec.is_isolated()
    }

    /// One line per dimension, followed by one line per limitation note.
    /// Suitable for a settings panel or a diagnostics bundle.
    #[must_use]
    pub fn describe_lines(&self) -> Vec<String> {
        let mut lines = self.dimension_lines();
        lines.extend(self.notes.iter().map(|note| format!("note: {note}")));
        lines
    }

    fn dimension_lines(&self) -> Vec<String> {
        vec![
            format!("memory: {}", self.memory_mechanism.describe()),
            format!(
                "cpu: {}",
                if self.cpu {
                    "capped by a kernel CPU-time limit"
                } else {
                    "not capped"
                }
            ),
            format!(
                "wall clock: {}",
                if self.wall_clock {
                    "bounded by the parent, which terminates the helper at the deadline"
                } else {
                    "not bounded"
                }
            ),
            format!("filesystem: {}", self.filesystem.describe()),
            format!("network: {}", self.network.describe()),
            format!("further process execution: {}", self.exec.describe()),
        ]
    }

    /// Single-line summary. Deliberately refuses the word "sandboxed" unless
    /// every dimension is actually restricted, and always says how many
    /// limitation notes [`IsolationReport::notes`] carries, so a reader of the
    /// summary alone is never told the story is complete when it is not.
    #[must_use]
    pub fn summary(&self) -> String {
        let head = if self.is_fully_isolated() {
            "helper fully restricted: filesystem, network, execution and all resource caps applied"
                .to_owned()
        } else {
            format!(
                "helper partially restricted ({})",
                self.dimension_lines().join("; ")
            )
        };
        match self.notes.len() {
            0 => head,
            1 => format!("{head}; 1 limitation note recorded"),
            n => format!("{head}; {n} limitation notes recorded"),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors and outcomes
// ---------------------------------------------------------------------------

/// Which budget a [`SandboxError::Timeout`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeoutBudget {
    /// The parent's wall-clock deadline expired.
    WallClock,
    /// The kernel reported that the CPU-time limit was exhausted.
    Cpu,
}

impl std::fmt::Display for TimeoutBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WallClock => "wall-clock",
            Self::Cpu => "cpu-time",
        })
    }
}

/// Everything that can go wrong with a sandboxed helper.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    /// A budget expired and the helper was terminated.
    #[error("helper exceeded its {budget} budget after {elapsed_ms}ms and was terminated")]
    Timeout {
        /// Which budget.
        budget: TimeoutBudget,
        /// Elapsed wall time when the helper was terminated.
        elapsed_ms: u64,
    },
    /// The helper was killed by a signal.
    #[error("helper was terminated by signal {signal}")]
    Killed {
        /// POSIX signal number.
        signal: i32,
    },
    /// The helper exited with a non-zero status.
    #[error("helper exited with status {code}")]
    Crashed {
        /// Process exit code.
        code: i32,
    },
    /// The helper hit its memory cap.
    #[error("helper exceeded its memory cap: {detail}")]
    Oom {
        /// How we know.
        detail: String,
    },
    /// The helper violated the IPC contract.
    #[error("helper protocol failure: {detail}")]
    Protocol {
        /// What was wrong.
        detail: String,
    },
    /// The helper could not be started.
    #[error("helper could not be started: {detail}")]
    SpawnFailed {
        /// Why.
        detail: String,
    },
}

impl SandboxError {
    /// `true` when retrying the same work could reasonably succeed.
    ///
    /// Resource exhaustion and termination are retryable: the user can close
    /// other applications, or the job can run later with a larger budget. A
    /// protocol violation or a failed spawn is a deployment problem and will
    /// fail again identically.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. } | Self::Killed { .. } | Self::Crashed { .. } | Self::Oom { .. }
        )
    }
}

impl SecurityError for SandboxError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        match self {
            Self::Timeout { .. } => {
                ErrorEnvelopeParts::new(error::HELPER_TIMEOUT, self.to_string())
                    .retryable(true)
                    .action(error::RETRY_JOB)
            }
            Self::Killed { .. } | Self::Crashed { .. } | Self::Oom { .. } => {
                ErrorEnvelopeParts::new(error::HELPER_FAILED, self.to_string())
                    .retryable(true)
                    .action(error::RETRY_JOB)
            }
            Self::Protocol { .. } | Self::SpawnFailed { .. } => {
                ErrorEnvelopeParts::new(error::HELPER_FAILED, self.to_string())
            }
        }
    }
}

/// The §14.7 job state a helper result maps onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobStateHint {
    /// `Completed` — only ever produced by [`HelperOutcome::Completed`].
    Completed,
    /// `FailedRetryable` — persist and offer a retry.
    FailedRetryable,
    /// `FailedTerminal` — persist; retrying will not help.
    FailedTerminal,
}

impl JobStateHint {
    /// The §14.7 state name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "Completed",
            Self::FailedRetryable => "FailedRetryable",
            Self::FailedTerminal => "FailedTerminal",
        }
    }
}

/// The classification the job engine consumes.
///
/// §14.1/§14.7: a killed helper must become a persisted failure state, never a
/// `Completed` job. The only way to obtain [`HelperOutcome::Completed`] is to
/// hold a [`HelperResponse`] the helper actually sent, so a crash, a kill, a
/// timeout or a truncated frame cannot be mistaken for success anywhere
/// downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperOutcome {
    /// The helper answered.
    Completed(HelperResponse),
    /// The helper failed in a way a retry could fix.
    Recoverable(SandboxError),
    /// The helper failed in a way a retry will not fix.
    Terminal(SandboxError),
}

impl From<Result<HelperResponse, SandboxError>> for HelperOutcome {
    fn from(result: Result<HelperResponse, SandboxError>) -> Self {
        match result {
            Ok(response) => Self::Completed(response),
            Err(err) if err.is_retryable() => Self::Recoverable(err),
            Err(err) => Self::Terminal(err),
        }
    }
}

impl HelperOutcome {
    /// `true` only for [`HelperOutcome::Completed`].
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }

    /// The job state this outcome must be persisted as.
    #[must_use]
    pub const fn job_state(&self) -> JobStateHint {
        match self {
            Self::Completed(_) => JobStateHint::Completed,
            Self::Recoverable(_) => JobStateHint::FailedRetryable,
            Self::Terminal(_) => JobStateHint::FailedTerminal,
        }
    }

    /// The error, when this is not a completion.
    #[must_use]
    pub const fn error(&self) -> Option<&SandboxError> {
        match self {
            Self::Completed(_) => None,
            Self::Recoverable(err) | Self::Terminal(err) => Some(err),
        }
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// The reason this platform reports for a dimension it cannot restrict.
const PLATFORM_REDUCED_REASON: ReducedIsolationReason =
    if cfg!(target_os = "macos") || cfg!(windows) {
        // The platform has a primitive; it was not usable for this launch.
        ReducedIsolationReason::PrimitiveUnavailable
    } else {
        ReducedIsolationReason::PlatformUnsupported
    };

/// Summary of how a helper process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitSummary {
    /// Exit code, when the process exited normally.
    pub code: Option<i32>,
    /// Terminating signal, on unix.
    pub signal: Option<i32>,
    /// Whether the platform considers the exit successful.
    pub success: bool,
}

/// Launcher for sandboxed helper processes.
#[derive(Debug)]
pub struct SandboxedHelper;

/// How hard [`SandboxedHelper::spawn_with`] should try to restrict the helper.
///
/// This is **not** a way to ask for a weaker sandbox silently: both values
/// produce an [`IsolationReport`] describing exactly what was applied, and
/// [`SandboxedHelper::spawn`] always uses [`IsolationPreference::Strongest`].
/// [`IsolationPreference::ResourceCapsOnly`] exists so a diagnostic command
/// can answer "is the OS sandbox what is breaking this helper?", and so the
/// honest-degradation path is testable on a machine where the strong
/// primitive happens to be available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IsolationPreference {
    /// Use the strongest primitive this platform offers, falling back to
    /// resource caps only if the helper cannot start under it.
    #[default]
    Strongest,
    /// Apply only the resource caps and the parent-side deadline, and report
    /// filesystem, network and exec as reduced.
    ResourceCapsOnly,
}

impl SandboxedHelper {
    /// Spawn `program` under `spec` and complete the `Hello` handshake.
    ///
    /// The child's environment is [`scrubbed_env`], so no secret, loader
    /// override or `NODE_OPTIONS`-style injection reaches it (§16.4, §7.1).
    ///
    /// On a platform with a probed strong primitive, the launch is attempted
    /// with the primitive first. If the helper cannot start under it, the
    /// helper is launched again without it and the returned
    /// [`IsolationReport`] says so — the caller is never told the restriction
    /// is in force when it is not.
    ///
    /// # Errors
    ///
    /// [`SandboxError::SpawnFailed`] if the program is unusable or the process
    /// cannot be created; [`SandboxError::Protocol`] if the handshake is not a
    /// well-formed `Hello`; [`SandboxError::Timeout`] if the helper does not
    /// answer the handshake in time.
    pub fn spawn(
        spec: &SandboxSpec,
        program: &Path,
        args: &[OsString],
    ) -> Result<HelperHandle, SandboxError> {
        Self::spawn_with(spec, program, args, IsolationPreference::Strongest)
    }

    /// [`SandboxedHelper::spawn`] with an explicit [`IsolationPreference`].
    ///
    /// # Errors
    ///
    /// As [`SandboxedHelper::spawn`].
    pub fn spawn_with(
        spec: &SandboxSpec,
        program: &Path,
        args: &[OsString],
        preference: IsolationPreference,
    ) -> Result<HelperHandle, SandboxError> {
        let program =
            spec::canonicalize_grant(program).map_err(|err| SandboxError::SpawnFailed {
                detail: err.to_string(),
            })?;
        if !program.is_file() {
            return Err(SandboxError::SpawnFailed {
                detail: "helper program is not a regular file".to_owned(),
            });
        }

        let strong = preference == IsolationPreference::Strongest && backend::strong_available();
        if strong {
            match try_spawn(spec, &program, args, true) {
                Ok(handle) => return Ok(handle),
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "helper could not start under the strong sandbox primitive; retrying with \
                         resource caps only and reporting reduced isolation"
                    );
                }
            }
        }
        try_spawn(spec, &program, args, false)
    }
}

fn try_spawn(
    spec: &SandboxSpec,
    program: &Path,
    args: &[OsString],
    use_strong: bool,
) -> Result<HelperHandle, SandboxError> {
    let started = Instant::now();
    let deadline = started + spec.wall_clock();

    let mut command = backend::command_for(spec, program, args, use_strong);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env_clear();
    command.envs(scrubbed_env());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        let limits = rlimits::ChildLimits {
            address_space_bytes: spec.memory_bytes(),
            cpu_seconds: spec.cpu_seconds(),
            open_files: rlimits::HELPER_OPEN_FILES,
        };
        // SAFETY: the closure runs in the forked child before `exec`. It calls
        // only `setrlimit`, which is async-signal-safe, and it allocates
        // nothing, takes no lock and touches no shared state.
        #[allow(unsafe_code)]
        unsafe {
            command.pre_exec(move || {
                rlimits::apply(limits);
                Ok(())
            });
        }
    }

    let mut child = command.spawn().map_err(|err| SandboxError::SpawnFailed {
        detail: format!("{:?}", err.kind()),
    })?;
    let pid = child.id();

    let (guard, job_memory, job_cpu) = backend::attach(spec, &child);

    let Some(stdin) = child.stdin.take() else {
        kill_and_reap(&mut child);
        return Err(SandboxError::SpawnFailed {
            detail: "no stdin pipe".to_owned(),
        });
    };
    let Some(stdout) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(SandboxError::SpawnFailed {
            detail: "no stdout pipe".to_owned(),
        });
    };
    let Some(stderr) = child.stderr.take() else {
        kill_and_reap(&mut child);
        return Err(SandboxError::SpawnFailed {
            detail: "no stderr pipe".to_owned(),
        });
    };

    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    spawn_stderr_reader(stderr, Arc::clone(&stderr_buffer));
    let responses = spawn_response_reader(stdout);

    let mut handle = HelperHandle {
        isolation: IsolationReport::nothing_applied(PLATFORM_REDUCED_REASON),
        child,
        stdin: Some(stdin),
        responses,
        stderr: stderr_buffer,
        oom: Arc::new(AtomicBool::new(false)),
        watchdog_stop: Arc::new(AtomicBool::new(false)),
        deadline,
        failure: None,
        pid,
        memory_cap: spec.memory_bytes(),
        _guard: guard,
    };

    // The handshake doubles as a liveness check: a helper that cannot start
    // under the strong primitive fails here, before any untrusted input is
    // handed to it, and the caller falls back to the honest reduced launch.
    let handshake_budget = spec.wall_clock().min(HANDSHAKE_TIMEOUT);
    let limits = match handle.exchange(&HelperRequest::Hello, handshake_budget)? {
        HelperResponse::Hello {
            limits,
            pid: reported,
        } => {
            if reported != pid {
                handle.kill();
                return Err(SandboxError::Protocol {
                    detail: "helper reported a different process id than the one we spawned"
                        .to_owned(),
                });
            }
            limits
        }
        other => {
            handle.kill();
            return Err(SandboxError::Protocol {
                detail: format!(
                    "expected a Hello handshake, got {}",
                    discriminant_of(&other)
                ),
            });
        }
    };

    handle.isolation = build_report(spec, &handle, use_strong, job_memory, job_cpu, limits);
    if matches!(
        handle.isolation.memory_mechanism,
        MemoryCapMechanism::ParentWatchdog { .. }
    ) {
        start_watchdog(pid, spec.memory_bytes(), &handle.oom, &handle.watchdog_stop);
    }
    Ok(handle)
}

fn discriminant_of(response: &HelperResponse) -> &'static str {
    match response {
        HelperResponse::Hello { .. } => "Hello",
        HelperResponse::Echo { .. } => "Echo",
        HelperResponse::FileRead { .. } => "FileRead",
        HelperResponse::FileWritten { .. } => "FileWritten",
        HelperResponse::TcpConnected { .. } => "TcpConnected",
        HelperResponse::ChildSpawned { .. } => "ChildSpawned",
        HelperResponse::Allocated { .. } => "Allocated",
        HelperResponse::CpuBurned { .. } => "CpuBurned",
        HelperResponse::Slept { .. } => "Slept",
        HelperResponse::Refused { .. } => "Refused",
        HelperResponse::Unsupported { .. } => "Unsupported",
    }
}

fn build_report(
    spec: &SandboxSpec,
    handle: &HelperHandle,
    use_strong: bool,
    job_memory: bool,
    job_cpu: bool,
    limits: AppliedLimits,
) -> IsolationReport {
    let strong = if use_strong {
        #[cfg(any(target_os = "macos", windows))]
        {
            IsolationLevel::Sandboxed {
                mechanism: backend::MECHANISM,
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            IsolationLevel::reduced(PLATFORM_REDUCED_REASON)
        }
    } else {
        IsolationLevel::reduced(PLATFORM_REDUCED_REASON)
    };

    let rlimit_memory = limits
        .address_space_bytes
        .is_some_and(|applied| applied <= spec.memory_bytes());
    let memory_mechanism = if job_memory {
        MemoryCapMechanism::JobObject
    } else if rlimit_memory {
        MemoryCapMechanism::ResourceLimit
    } else if backend::resident_bytes(handle.pid).is_some() {
        MemoryCapMechanism::ParentWatchdog {
            sample_interval_ms: u64::try_from(WATCHDOG_INTERVAL.as_millis()).unwrap_or(u64::MAX),
        }
    } else {
        MemoryCapMechanism::None
    };

    let cpu = job_cpu
        || limits
            .cpu_seconds
            .is_some_and(|applied| applied <= spec.cpu_seconds());

    IsolationReport {
        memory: memory_mechanism.is_enforced(),
        cpu,
        // The parent holds a monotonic deadline and kills the helper when it
        // expires; that is enforcement we perform ourselves, so it is always
        // in force.
        wall_clock: true,
        filesystem: strong,
        network: strong,
        exec: strong,
        memory_mechanism,
        notes: platform_notes(strong.is_isolated(), memory_mechanism),
    }
}

/// The limitation notes that apply to a launch with this outcome.
///
/// Every combination is spelled out rather than assembled at run time so the
/// vocabulary stays a closed set of `NOTE_*` constants that a UI can match on.
const fn platform_notes(
    strong_applied: bool,
    memory_mechanism: MemoryCapMechanism,
) -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        let sampled = matches!(memory_mechanism, MemoryCapMechanism::ParentWatchdog { .. });
        match (strong_applied, sampled) {
            (true, true) => &[
                NOTE_MACOS_SANDBOX_EXEC_DEPRECATED,
                NOTE_MACOS_NO_KERNEL_MEMORY_CAP,
            ],
            (true, false) => &[NOTE_MACOS_SANDBOX_EXEC_DEPRECATED],
            (false, true) => &[NOTE_MACOS_NO_PROFILE, NOTE_MACOS_NO_KERNEL_MEMORY_CAP],
            (false, false) => &[NOTE_MACOS_NO_PROFILE],
        }
    }
    #[cfg(windows)]
    {
        let _ = (strong_applied, memory_mechanism);
        &[
            NOTE_WINDOWS_ASSIGN_AFTER_SPAWN,
            NOTE_WINDOWS_NO_FILESYSTEM_RESTRICTION,
        ]
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (strong_applied, memory_mechanism);
        &[NOTE_RESOURCE_CAPS_ONLY]
    }
}

fn spawn_stderr_reader(mut stderr: std::process::ChildStderr, sink: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = STDERR_CAPTURE_BYTES.saturating_sub(buffer.len());
                    if room > 0 {
                        buffer.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
        }
        if let Ok(mut guard) = sink.lock() {
            guard.push_str(&String::from_utf8_lossy(&buffer));
        }
    });
}

fn spawn_response_reader(
    stdout: std::process::ChildStdout,
) -> Receiver<Result<HelperResponse, FrameError>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let message = ipc::read_message::<_, HelperResponse>(&mut reader);
            let fatal = message.is_err();
            if tx.send(message).is_err() || fatal {
                break;
            }
        }
    });
    rx
}

#[cfg(unix)]
fn start_watchdog(pid: u32, cap: u64, oom: &Arc<AtomicBool>, stop: &Arc<AtomicBool>) {
    let oom = Arc::clone(oom);
    let stop = Arc::clone(stop);
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(WATCHDOG_INTERVAL);
            match backend::resident_bytes(pid) {
                Some(resident) if resident > cap => {
                    oom.store(true, Ordering::SeqCst);
                    kill_pid(pid);
                    break;
                }
                Some(_) => {}
                // The process is gone (or unobservable); nothing left to do.
                None => break,
            }
        }
    });
}

#[cfg(not(unix))]
fn start_watchdog(_pid: u32, _cap: u64, _oom: &Arc<AtomicBool>, _stop: &Arc<AtomicBool>) {}

/// Send `SIGKILL` to a child we have not yet reaped.
///
/// The pid cannot have been reused: the parent has not waited on the child, so
/// it is at worst a zombie that still owns the pid.
#[cfg(unix)]
#[allow(unsafe_code)]
fn kill_pid(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: `kill` takes two integers and has no pointer arguments.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// The handle
// ---------------------------------------------------------------------------

/// A live sandboxed helper.
///
/// Dropping the handle kills the helper: an abandoned decoder must not keep
/// chewing on hostile input.
#[derive(Debug)]
pub struct HelperHandle {
    /// What was actually applied to this process.
    pub isolation: IsolationReport,
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<Result<HelperResponse, FrameError>>,
    stderr: Arc<Mutex<String>>,
    oom: Arc<AtomicBool>,
    watchdog_stop: Arc<AtomicBool>,
    deadline: Instant,
    failure: Option<SandboxError>,
    pid: u32,
    memory_cap: u64,
    _guard: backend::PlatformGuard,
}

impl HelperHandle {
    /// Operating-system process id of the helper.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// The memory cap being enforced, in bytes.
    #[must_use]
    pub const fn memory_cap(&self) -> u64 {
        self.memory_cap
    }

    /// Whatever the helper wrote to stderr, truncated to a bounded size.
    #[must_use]
    pub fn stderr_snapshot(&self) -> String {
        self.stderr
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Send one request and wait for the answer.
    ///
    /// The remaining wall-clock budget bounds the wait. On expiry the helper
    /// is killed and [`SandboxError::Timeout`] is returned; the handle is then
    /// permanently failed and every later call returns the same error, so a
    /// caller cannot accidentally observe a success after a termination.
    ///
    /// # Errors
    ///
    /// Any [`SandboxError`]; see [`HelperOutcome`] for the job-state mapping.
    // The request is consumed on purpose: it is a one-shot message that has
    // been handed to the helper and must not be resent by the caller.
    #[allow(clippy::needless_pass_by_value)]
    pub fn request(&mut self, request: HelperRequest) -> Result<HelperResponse, SandboxError> {
        if let Some(err) = &self.failure {
            return Err(err.clone());
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(self.fail_with_timeout(TimeoutBudget::WallClock));
        }
        self.exchange(&request, remaining)
    }

    /// Send one request and classify the result for the job engine.
    pub fn request_outcome(&mut self, request: HelperRequest) -> HelperOutcome {
        HelperOutcome::from(self.request(request))
    }

    fn exchange(
        &mut self,
        request: &HelperRequest,
        budget: Duration,
    ) -> Result<HelperResponse, SandboxError> {
        let started = Instant::now();
        // The pipe is moved out for the duration of the write so no borrow of
        // `self` is live while a failure is being classified.
        let Some(mut stdin) = self.stdin.take() else {
            return Err(self.record(SandboxError::Protocol {
                detail: "helper stdin is closed".to_owned(),
            }));
        };
        let write_result = ipc::write_frame(&mut stdin, request);
        self.stdin = Some(stdin);
        if let Err(err) = write_result {
            // A broken pipe means the helper is already gone; find out why
            // rather than reporting the write failure.
            let classified = if matches!(err, FrameError::Io { .. } | FrameError::Closed) {
                self.classify_exit(started)
            } else {
                SandboxError::Protocol {
                    detail: err.to_string(),
                }
            };
            return Err(self.record(classified));
        }

        match self.responses.recv_timeout(budget) {
            Ok(Ok(response)) => Ok(response),
            // Either the helper closed its pipe or the reader thread is gone:
            // both mean the process stopped talking and we must find out why.
            Ok(Err(FrameError::Closed)) | Err(RecvTimeoutError::Disconnected) => {
                let classified = self.classify_exit(started);
                Err(self.record(classified))
            }
            Ok(Err(frame_error)) => {
                // A malformed frame is unrecoverable for this helper: the
                // stream position is unknown, so the process is terminated.
                self.kill();
                Err(self.record(SandboxError::Protocol {
                    detail: frame_error.to_string(),
                }))
            }
            Err(RecvTimeoutError::Timeout) => {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    let classified = self.classify_exit(started);
                    return Err(self.record(classified));
                }
                Err(self.fail_with_timeout(TimeoutBudget::WallClock))
            }
        }
    }

    fn fail_with_timeout(&mut self, budget: TimeoutBudget) -> SandboxError {
        let elapsed = Instant::now().saturating_duration_since(self.deadline);
        self.kill();
        let err = SandboxError::Timeout {
            budget,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        };
        self.record(err)
    }

    fn record(&mut self, err: SandboxError) -> SandboxError {
        if self.failure.is_none() {
            self.failure = Some(err.clone());
        }
        self.watchdog_stop.store(true, Ordering::Relaxed);
        err
    }

    /// Work out why the helper stopped answering.
    fn classify_exit(&mut self, request_started: Instant) -> SandboxError {
        let summary = self.reap(REAP_TIMEOUT);
        if self.oom.load(Ordering::SeqCst) {
            return SandboxError::Oom {
                detail: format!(
                    "resident set exceeded the {} byte cap; the parent watchdog terminated the \
                     helper",
                    self.memory_cap
                ),
            };
        }
        let stderr = self.stderr_snapshot();
        if allocation_failure(&stderr) {
            return SandboxError::Oom {
                detail: "the helper's allocator reported an allocation failure".to_owned(),
            };
        }
        let Some(summary) = summary else {
            self.kill();
            return SandboxError::Protocol {
                detail: "helper stopped answering and did not exit".to_owned(),
            };
        };
        if let Some(signal) = summary.signal {
            #[cfg(unix)]
            if signal == libc::SIGXCPU {
                let elapsed = Instant::now().saturating_duration_since(request_started);
                return SandboxError::Timeout {
                    budget: TimeoutBudget::Cpu,
                    elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                };
            }
            let _ = request_started;
            return SandboxError::Killed { signal };
        }
        match summary.code {
            Some(0) | None => SandboxError::Protocol {
                detail: "helper exited without answering the request".to_owned(),
            },
            Some(code) => SandboxError::Crashed { code },
        }
    }

    fn reap(&mut self, timeout: Duration) -> Option<ExitSummary> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(summarize(status)),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return None,
            }
        }
    }

    /// Wait for the helper to exit, killing it if it outlasts `timeout`.
    ///
    /// # Errors
    ///
    /// [`SandboxError::Timeout`] if the helper had to be killed.
    pub fn wait_with_timeout(&mut self, timeout: Duration) -> Result<ExitSummary, SandboxError> {
        // Closing stdin lets a well-behaved helper notice end-of-input and
        // exit on its own.
        self.stdin = None;
        if let Some(summary) = self.reap(timeout) {
            self.watchdog_stop.store(true, Ordering::Relaxed);
            return Ok(summary);
        }
        Err(self.fail_with_timeout(TimeoutBudget::WallClock))
    }

    /// Terminate the helper now. Safe to call repeatedly.
    pub fn kill(&mut self) {
        self.watchdog_stop.store(true, Ordering::Relaxed);
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// `true` once the handle has recorded a terminal failure.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        self.failure.is_some()
    }
}

impl Drop for HelperHandle {
    fn drop(&mut self) {
        self.watchdog_stop.store(true, Ordering::Relaxed);
        self.stdin = None;
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn summarize(status: std::process::ExitStatus) -> ExitSummary {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    ExitSummary {
        code: status.code(),
        signal,
        success: status.success(),
    }
}

/// Markers the Rust runtime and the C allocator print when an allocation fails.
fn allocation_failure(stderr: &str) -> bool {
    const MARKERS: [&str; 4] = [
        "memory allocation of",
        "out of memory",
        "Cannot allocate memory",
        "AllocErr",
    ];
    MARKERS.iter().any(|marker| stderr.contains(marker))
}

#[cfg(test)]
mod tests;
