//! D2.5 acceptance: what the process sandbox actually enforces on *this*
//! machine (§6.3A, §16.2 row "Malicious audio decoder input", §14/§14.7).
//!
//! These tests spawn the real `plaudon-sandbox-probe` helper, which attempts
//! the things a compromised media decoder would attempt. They are hermetic:
//! the only filesystem writes are inside a `tempfile` directory, and the one
//! network attempt targets a documentation address that must not connect —
//! under a working profile the attempt never reaches the resolver, and when
//! the profile is unavailable the test asserts the honest
//! `ReducedIsolation` report instead of pretending the restriction held.
//!
//! Every isolation assertion is written as a *pair*: "denied when the report
//! claims isolation, and reported as reduced when it does not". A test that
//! only asserted the happy path would silently pass on a machine where no
//! restriction was applied, which is exactly the failure mode §16.6 forbids.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::OsString;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use plaudon_security::error::{self, SecurityError};
use plaudon_security::sandbox::ipc::{
    self, FrameError, HelperRequest, HelperResponse, RefusalKind, MAX_FRAME_BYTES,
};
use plaudon_security::sandbox::{
    HelperHandle, HelperOutcome, IsolationPreference, JobStateHint, MemoryCapMechanism,
    ReducedIsolationReason, SandboxError, SandboxSpec, SandboxedHelper, TimeoutBudget,
};

// ---------------------------------------------------------------------------
// Allocation accounting, used to prove a forged frame length allocates nothing
// ---------------------------------------------------------------------------

thread_local! {
    /// Bytes allocated by this thread since the counter was last reset. A
    /// `const`-initialised `Cell` never allocates on first access, so it is
    /// safe to touch from inside the allocator.
    static THREAD_ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

static GLOBAL_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is arithmetic on a `Cell<usize>` and an atomic counter, neither of
// which allocates or can unwind.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size.saturating_sub(layout.size()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
}

fn record(bytes: usize) {
    GLOBAL_ALLOCATED.fetch_add(bytes, Ordering::Relaxed);
    let _ = THREAD_ALLOCATED.try_with(|cell| cell.set(cell.get().saturating_add(bytes)));
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Bytes this thread allocated while running `body`.
fn allocated_during<T>(body: impl FnOnce() -> T) -> (T, usize) {
    THREAD_ALLOCATED.with(|cell| cell.set(0));
    let value = body();
    let used = THREAD_ALLOCATED.with(Cell::get);
    (value, used)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn probe_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_plaudon-sandbox-probe"))
}

/// A path that exists on every supported host and that the helper is never
/// granted.
fn out_of_grant_path() -> &'static Path {
    #[cfg(windows)]
    {
        Path::new("C:\\Windows\\System32\\drivers\\etc\\hosts")
    }
    #[cfg(not(windows))]
    {
        Path::new("/etc/passwd")
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    input: PathBuf,
    output: PathBuf,
    spec: SandboxSpec,
}

fn fixture(memory_bytes: u64, cpu_seconds: u64, wall_clock: Duration) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("input.wav");
    std::fs::write(&input, INPUT_BYTES).expect("stage input");
    let output = dir.path().join("output.pcm");

    let spec = SandboxSpec::builder()
        .read_path(&input)
        .read_path(probe_binary())
        .write_path(&output)
        .memory_bytes(memory_bytes)
        .cpu_seconds(cpu_seconds)
        .wall_clock(wall_clock)
        .build()
        .expect("valid spec");

    let input = spec.read_paths()[0].clone();
    let output = spec.write_paths()[0].clone();
    Fixture {
        _dir: dir,
        input,
        output,
        spec,
    }
}

const INPUT_BYTES: &[u8] = b"RIFF....WAVEfmt  this stands in for an untrusted vendor recording";

fn spawn(spec: &SandboxSpec) -> HelperHandle {
    SandboxedHelper::spawn(spec, &probe_binary(), &[] as &[OsString]).expect("helper starts")
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill` with signal 0 performs the permission/existence check
    // only; it takes two integers and no pointers.
    unsafe { libc::kill(pid, 0) == 0 }
}

// ---------------------------------------------------------------------------
// Isolation honesty
// ---------------------------------------------------------------------------

#[test]
fn isolation_report_states_exactly_what_was_applied() {
    let fx = fixture(256 * 1024 * 1024, 20, Duration::from_secs(20));
    let helper = spawn(&fx.spec);
    let report = helper.isolation;

    eprintln!("--- observed IsolationReport on this machine ---");
    for line in report.describe_lines() {
        eprintln!("  {line}");
    }
    eprintln!("  summary: {}", report.summary());

    // The limitation notes must travel with the report, not live only in a
    // source comment: the evidence bundle and the UI read them from here.
    assert!(
        !report.notes.is_empty(),
        "every supported platform has a standing limitation to declare"
    );
    for note in report.notes {
        assert!(note.len() > 40, "a note must explain itself: {note}");
        assert!(report
            .describe_lines()
            .iter()
            .any(|line| line == &format!("note: {note}")));
    }
    assert!(report.summary().contains("limitation note"));

    // The parent always owns the wall-clock budget, so that one is never
    // "reduced".
    assert!(
        report.wall_clock,
        "the parent always enforces the wall-clock deadline"
    );

    // Every dimension is either a named applied mechanism or a named reason.
    for level in [report.filesystem, report.network, report.exec] {
        if level.is_isolated() {
            assert_ne!(level.describe().len(), 0);
            assert!(
                !level.describe().starts_with("not restricted"),
                "an applied mechanism must not describe itself as unrestricted"
            );
        } else {
            assert!(
                level.reduced_reason().is_some(),
                "a reduced level must name its reason"
            );
        }
    }

    // `memory` is true exactly when a mechanism holds the cap.
    assert_eq!(report.memory, report.memory_mechanism.is_enforced());
    if report.memory {
        assert_ne!(report.memory_mechanism, MemoryCapMechanism::None);
    }

    // The three strong dimensions are decided together by one primitive, so
    // they must agree: a split would mean the report was assembled from
    // guesses rather than from what was applied.
    assert_eq!(
        report.filesystem.is_isolated(),
        report.network.is_isolated()
    );
    assert_eq!(report.filesystem.is_isolated(), report.exec.is_isolated());

    #[cfg(target_os = "macos")]
    if report.filesystem.is_isolated() {
        assert!(
            report.filesystem.describe().contains("Seatbelt"),
            "on macOS the only V1 mechanism is the Seatbelt profile: {}",
            report.filesystem.describe()
        );
    }
    #[cfg(windows)]
    assert!(
        !report.filesystem.is_isolated(),
        "V1 does not implement a Windows restricted token and must not claim one"
    );
}

#[test]
fn a_granted_file_can_be_read_and_an_ungranted_one_cannot() {
    let fx = fixture(256 * 1024 * 1024, 20, Duration::from_secs(20));
    let mut helper = spawn(&fx.spec);
    let isolated = helper.isolation.filesystem.is_isolated();

    let granted = helper
        .request(HelperRequest::ReadFile {
            path: path_arg(&fx.input),
        })
        .expect("granted read answered");
    match granted {
        HelperResponse::FileRead { bytes, sha256 } => {
            assert_eq!(bytes, INPUT_BYTES.len() as u64);
            assert_eq!(sha256, sha256_hex(INPUT_BYTES));
        }
        other => panic!("granted read must succeed, got {other:?}"),
    }

    let forbidden = helper
        .request(HelperRequest::ReadFile {
            path: path_arg(out_of_grant_path()),
        })
        .expect("ungranted read answered");
    if isolated {
        match forbidden {
            HelperResponse::Refused { refusal, .. } => {
                assert_eq!(
                    refusal.kind,
                    RefusalKind::PermissionDenied,
                    "the sandbox must refuse with a permission error, got {refusal:?}"
                );
            }
            other => panic!("filesystem isolation is reported as applied, so the out-of-grant read must be refused; got {other:?}"),
        }
    } else {
        // Honest alternative: no restriction was applied, so the read may well
        // succeed — and the report must say so rather than claiming isolation.
        assert!(
            helper.isolation.filesystem.reduced_reason().is_some(),
            "without a refusal the report must name why nothing was applied"
        );
        eprintln!(
            "filesystem isolation unavailable here ({}); out-of-grant read returned {forbidden:?}",
            helper.isolation.filesystem.describe()
        );
    }
}

#[test]
fn a_granted_output_file_can_be_written() {
    let fx = fixture(256 * 1024 * 1024, 20, Duration::from_secs(20));
    let mut helper = spawn(&fx.spec);
    let response = helper
        .request(HelperRequest::WriteFile {
            path: path_arg(&fx.output),
            bytes: b"decoded pcm".to_vec(),
        })
        .expect("granted write answered");
    assert_eq!(response, HelperResponse::FileWritten { bytes: 11 });
    assert_eq!(
        std::fs::read(&fx.output).expect("output exists"),
        b"decoded pcm"
    );
}

#[test]
fn an_outbound_connection_is_denied() {
    let fx = fixture(256 * 1024 * 1024, 20, Duration::from_secs(30));
    let mut helper = spawn(&fx.spec);
    let isolated = helper.isolation.network.is_isolated();

    // 198.51.100.0/24 is TEST-NET-2 (RFC 5737): routable nowhere.
    let response = helper
        .request(HelperRequest::ConnectTcp {
            host: "198.51.100.7".to_owned(),
            port: 443,
        })
        .expect("connect attempt answered");

    match response {
        HelperResponse::Refused { refusal, .. } => {
            if isolated {
                eprintln!("network attempt refused under the sandbox profile: {refusal:?}");
            } else {
                eprintln!(
                    "network isolation unavailable here ({}); the attempt failed anyway: {refusal:?}",
                    helper.isolation.network.describe()
                );
            }
        }
        HelperResponse::TcpConnected { .. } => {
            panic!("a connection to TEST-NET-2 must never succeed");
        }
        other => panic!("unexpected response {other:?}"),
    }

    if !isolated {
        assert!(
            helper.isolation.network.reduced_reason().is_some(),
            "a non-isolated network dimension must name its reason"
        );
    }
}

#[test]
fn executing_another_program_is_denied() {
    let fx = fixture(256 * 1024 * 1024, 20, Duration::from_secs(20));
    let mut helper = spawn(&fx.spec);
    let isolated = helper.isolation.exec.is_isolated();

    let response = helper
        .request(HelperRequest::SpawnChild)
        .expect("exec attempt answered");
    if isolated {
        match response {
            HelperResponse::Refused { refusal, .. } => {
                eprintln!("exec refused under the sandbox profile: {refusal:?}");
            }
            other => panic!(
                "exec isolation is reported as applied, so spawning must fail; got {other:?}"
            ),
        }
    } else {
        assert!(helper.isolation.exec.reduced_reason().is_some());
        eprintln!(
            "exec isolation unavailable here ({}); attempt returned {response:?}",
            helper.isolation.exec.describe()
        );
    }
}

// ---------------------------------------------------------------------------
// Resource caps
// ---------------------------------------------------------------------------

#[test]
fn allocation_beyond_the_memory_cap_is_capped_and_recoverable() {
    let cap = 64 * 1024 * 1024;
    let fx = fixture(cap, 30, Duration::from_secs(30));
    let mut helper = spawn(&fx.spec);
    let mechanism = helper.isolation.memory_mechanism;

    if !helper.isolation.memory {
        eprintln!(
            "no memory cap mechanism is available here: {}",
            mechanism.describe()
        );
        assert_eq!(mechanism, MemoryCapMechanism::None);
        return;
    }

    let outcome = helper.request_outcome(HelperRequest::AllocateBytes {
        bytes: cap * 8,
        graceful: false,
    });

    match outcome {
        HelperOutcome::Recoverable(err) => {
            assert!(
                matches!(
                    err,
                    SandboxError::Oom { .. }
                        | SandboxError::Killed { .. }
                        | SandboxError::Crashed { .. }
                ),
                "expected a resource termination, got {err:?}"
            );
            assert_eq!(err.envelope_parts().code, error::HELPER_FAILED);
            assert!(err.envelope_parts().retryable);
        }
        HelperOutcome::Completed(HelperResponse::Refused { refusal, .. }) => {
            assert_eq!(refusal.kind, RefusalKind::ResourceExhausted);
        }
        other => panic!(
            "a memory cap is reported as enforced by {}, so an 8x over-allocation must not \
             succeed; got {other:?}",
            mechanism.describe()
        ),
    }
}

#[test]
fn a_graceful_allocation_within_the_cap_succeeds() {
    let fx = fixture(512 * 1024 * 1024, 30, Duration::from_secs(30));
    let mut helper = spawn(&fx.spec);
    let response = helper
        .request(HelperRequest::AllocateBytes {
            bytes: 8 * 1024 * 1024,
            graceful: true,
        })
        .expect("small allocation answered");
    assert_eq!(
        response,
        HelperResponse::Allocated {
            bytes: 8 * 1024 * 1024
        }
    );
}

#[test]
fn work_beyond_the_wall_clock_budget_times_out_and_the_child_is_gone() {
    let fx = fixture(256 * 1024 * 1024, 60, Duration::from_millis(1_500));
    let mut helper = spawn(&fx.spec);
    let pid = helper.pid();

    let outcome = helper.request_outcome(HelperRequest::SleepMs { ms: 60_000 });
    let HelperOutcome::Recoverable(err) = outcome else {
        panic!("a wall-clock overrun must be a recoverable failure, got {outcome:?}");
    };
    assert!(
        matches!(
            err,
            SandboxError::Timeout {
                budget: TimeoutBudget::WallClock,
                ..
            }
        ),
        "{err:?}"
    );
    let parts = err.envelope_parts();
    assert_eq!(parts.code, error::HELPER_TIMEOUT);
    assert!(parts.retryable);
    assert_eq!(parts.action, Some(error::RETRY_JOB));

    #[cfg(unix)]
    assert!(
        !process_is_alive(pid),
        "the helper must be killed and reaped at the deadline"
    );
    #[cfg(not(unix))]
    let _ = pid;

    // The handle stays failed: a later request cannot report success.
    let after = helper.request_outcome(HelperRequest::Echo {
        payload: "still here?".to_owned(),
    });
    assert!(!after.is_completed());
    assert_eq!(after.job_state(), JobStateHint::FailedRetryable);
}

#[test]
fn burning_cpu_past_the_budget_fails_without_a_false_completion() {
    // The CPU budget is shorter than the wall-clock budget, so whichever the
    // platform enforces first, the result must still be a typed failure.
    let fx = fixture(256 * 1024 * 1024, 1, Duration::from_secs(20));
    let mut helper = spawn(&fx.spec);
    let cpu_capped = helper.isolation.cpu;

    let outcome = helper.request_outcome(HelperRequest::BurnCpu { seconds: 10 });
    if cpu_capped {
        let HelperOutcome::Recoverable(err) = outcome else {
            panic!("a CPU-budget overrun must be recoverable, got {outcome:?}");
        };
        assert!(
            matches!(
                err,
                SandboxError::Timeout {
                    budget: TimeoutBudget::Cpu,
                    ..
                } | SandboxError::Killed { .. }
            ),
            "expected a CPU-limit termination, got {err:?}"
        );
        let code = err.envelope_parts().code;
        assert!(code == error::HELPER_TIMEOUT || code == error::HELPER_FAILED);
    } else {
        eprintln!("no CPU cap is enforced here; the helper ran to completion: {outcome:?}");
        assert!(outcome.is_completed());
    }
}

// ---------------------------------------------------------------------------
// Chaos: the helper dies mid-request
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_helper_killed_mid_request_is_recoverable_and_leaves_the_input_intact() {
    let fx = fixture(256 * 1024 * 1024, 60, Duration::from_secs(30));
    let before = std::fs::read(&fx.input).expect("input readable");
    let mut helper = spawn(&fx.spec);
    let pid = helper.pid();

    #[allow(unsafe_code)]
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        let Ok(pid) = i32::try_from(pid) else { return };
        // SAFETY: `kill` takes two integers; the pid belongs to a child this
        // process has spawned and has not yet reaped.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    });

    let outcome = helper.request_outcome(HelperRequest::SleepMs { ms: 5_000 });
    killer.join().expect("killer thread");

    let HelperOutcome::Recoverable(err) = outcome else {
        panic!("a killed helper must be a recoverable failure, got {outcome:?}");
    };
    assert_eq!(
        err,
        SandboxError::Killed {
            signal: libc::SIGKILL
        }
    );
    let parts = err.envelope_parts();
    assert_eq!(parts.code, error::HELPER_FAILED);
    assert!(
        parts.retryable,
        "§14.7: the job persists as FailedRetryable"
    );

    assert!(
        !process_is_alive(pid),
        "the killed helper must have been reaped"
    );
    assert_eq!(
        std::fs::read(&fx.input).expect("input still readable"),
        before
    );
    assert!(
        !fx.output.exists(),
        "no partial output may be left behind as if it were committed"
    );
}

#[test]
fn a_clean_shutdown_is_not_a_failure() {
    let fx = fixture(256 * 1024 * 1024, 20, Duration::from_secs(20));
    let mut helper = spawn(&fx.spec);
    let echo = helper
        .request(HelperRequest::Shutdown)
        .expect("shutdown acknowledged");
    assert_eq!(
        echo,
        HelperResponse::Echo {
            payload: "shutdown".to_owned()
        }
    );
    let summary = helper
        .wait_with_timeout(Duration::from_secs(5))
        .expect("helper exits");
    assert!(
        summary.success,
        "a helper asked to stop must exit cleanly: {summary:?}"
    );
}

// ---------------------------------------------------------------------------
// IPC contract
// ---------------------------------------------------------------------------

#[test]
fn a_forged_frame_length_is_rejected_without_a_large_allocation() {
    let mut frame = u32::MAX.to_le_bytes().to_vec();
    frame.extend_from_slice(b"not four gibibytes of data");

    let (result, allocated) = allocated_during(|| {
        let mut cursor = Cursor::new(&frame);
        ipc::read_frame_body(&mut cursor)
    });

    assert_eq!(
        result.unwrap_err(),
        FrameError::FrameTooLarge {
            declared: u32::MAX,
            max: MAX_FRAME_BYTES
        }
    );
    assert!(
        allocated < 64 * 1024,
        "rejecting a forged length allocated {allocated} bytes; it must allocate essentially \
         nothing"
    );
}

#[test]
fn the_ipc_contract_rejects_hostile_frames_with_typed_errors() {
    // Round trip.
    let request = HelperRequest::ReadFile {
        path: "/tmp/input.wav".to_owned(),
    };
    let frame = ipc::encode(&request).expect("encodes");
    let decoded: HelperRequest =
        ipc::read_message(&mut Cursor::new(frame.clone())).expect("decodes");
    assert_eq!(decoded, request);

    // Truncated frame.
    let truncated = frame[..frame.len() - 3].to_vec();
    assert!(matches!(
        ipc::read_message::<_, HelperRequest>(&mut Cursor::new(truncated)).unwrap_err(),
        FrameError::Truncated { .. }
    ));

    // Oversized declared length.
    let mut oversized = (MAX_FRAME_BYTES + 1).to_le_bytes().to_vec();
    oversized.extend_from_slice(&frame[4..]);
    assert!(matches!(
        ipc::read_message::<_, HelperRequest>(&mut Cursor::new(oversized)).unwrap_err(),
        FrameError::FrameTooLarge { .. }
    ));

    // Non-UTF-8 body.
    let mut invalid = 3_u32.to_le_bytes().to_vec();
    invalid.extend_from_slice(&[0xFF, 0xFE, 0xFD]);
    assert!(matches!(
        ipc::read_message::<_, HelperRequest>(&mut Cursor::new(invalid)).unwrap_err(),
        FrameError::NotUtf8 { .. }
    ));

    // Trailing garbage inside one frame.
    let body = br#"{"op":"hello"}!!"#;
    let mut trailing = u32::try_from(body.len())
        .expect("fits")
        .to_le_bytes()
        .to_vec();
    trailing.extend_from_slice(body);
    assert!(matches!(
        ipc::read_message::<_, HelperRequest>(&mut Cursor::new(trailing)).unwrap_err(),
        FrameError::TrailingGarbage { extra: 2 }
    ));
}

#[test]
fn a_helper_that_speaks_garbage_is_a_protocol_failure_not_a_completion() {
    // `/bin/echo` is a real program that writes bytes which are not frames.
    #[cfg(unix)]
    let babbler = Path::new("/bin/echo");
    #[cfg(windows)]
    let babbler = Path::new("C:\\Windows\\System32\\cmd.exe");

    if !babbler.is_file() {
        return;
    }
    let spec = SandboxSpec::builder()
        .read_path(babbler)
        .wall_clock(Duration::from_secs(5))
        .build()
        .expect("valid spec");
    let err = SandboxedHelper::spawn(&spec, babbler, &[] as &[OsString])
        .expect_err("a program that does not speak the protocol must not be accepted");
    assert!(
        matches!(
            err,
            SandboxError::Protocol { .. } | SandboxError::Crashed { .. }
        ),
        "{err:?}"
    );
    assert!(!err.is_retryable() || matches!(err, SandboxError::Crashed { .. }));
}

#[test]
fn spawning_a_missing_program_is_a_terminal_spawn_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-helper");
    let spec = SandboxSpec::builder()
        .wall_clock(Duration::from_secs(5))
        .build()
        .expect("spec");
    let err = SandboxedHelper::spawn(&spec, &missing, &[] as &[OsString]).expect_err("no helper");
    assert!(matches!(err, SandboxError::SpawnFailed { .. }), "{err:?}");
    assert_eq!(
        HelperOutcome::from(Err(err)).job_state(),
        JobStateHint::FailedTerminal
    );
}

/// The honest-degradation path, exercised on every machine.
///
/// This is what a host without a usable strong primitive gets — including a
/// macOS on which Apple has finally removed the deprecated `sandbox-exec`.
/// The report must admit that filesystem, network and exec are unrestricted,
/// must not call itself fully restricted, and must still carry the resource
/// caps. Running it next to the strong-path tests also proves the refusals
/// seen there came from the operating system and not from the helper being
/// polite: the same probe, unrestricted, reads the same out-of-grant file.
#[test]
fn the_reduced_isolation_path_reports_exactly_what_it_did_not_apply() {
    let fx = fixture(256 * 1024 * 1024, 20, Duration::from_secs(20));
    let mut helper = SandboxedHelper::spawn_with(
        &fx.spec,
        &probe_binary(),
        &[] as &[OsString],
        IsolationPreference::ResourceCapsOnly,
    )
    .expect("helper starts without the strong primitive");

    let report = helper.isolation;
    assert!(!report.filesystem.is_isolated());
    assert!(!report.network.is_isolated());
    assert!(!report.exec.is_isolated());
    assert!(!report.is_fully_isolated());
    assert!(!report.summary().contains("fully restricted"));

    let expected_reason = if cfg!(target_os = "macos") || cfg!(windows) {
        ReducedIsolationReason::PrimitiveUnavailable
    } else {
        ReducedIsolationReason::PlatformUnsupported
    };
    for level in [report.filesystem, report.network, report.exec] {
        assert_eq!(level.reduced_reason(), Some(expected_reason));
    }

    // The resource caps are independent of the strong primitive and survive.
    assert!(report.wall_clock);
    assert!(
        report.cpu,
        "the CPU cap is a plain rlimit and must still apply"
    );

    // The notes must change with the outcome, not be a fixed banner.
    #[cfg(target_os = "macos")]
    assert!(
        report
            .notes
            .contains(&plaudon_security::sandbox::NOTE_MACOS_NO_PROFILE),
        "a launch with no Seatbelt profile must say so in the report: {:?}",
        report.notes
    );
    assert_ne!(report.notes.len(), 0);

    // Counter-evidence for the strong-path tests: with no profile applied the
    // very same probe binary reads the very same out-of-grant file.
    if std::fs::read(out_of_grant_path()).is_ok() {
        let response = helper
            .request(HelperRequest::ReadFile {
                path: path_arg(out_of_grant_path()),
            })
            .expect("read attempt answered");
        assert!(
            matches!(response, HelperResponse::FileRead { .. }),
            "an unrestricted helper reads what the caller can read; got {response:?}"
        );
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
