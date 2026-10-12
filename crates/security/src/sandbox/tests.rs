//! Hermetic unit tests for the sandbox module: no process is spawned here and
//! nothing touches the network. Process behaviour is covered by
//! `tests/sandbox_integration.rs`.

use std::io::Cursor;
use std::time::Duration;

use super::ipc::{
    self, AppliedLimits, FrameError, HelperRequest, HelperResponse, Operation, Refusal,
    RefusalKind, MAX_FRAME_BYTES,
};
use super::spec::{SandboxSpec, SpecError, MIN_MEMORY_BYTES};
use super::{
    ExecAccess, HelperOutcome, IsolationLevel, IsolationReport, JobStateHint, MemoryCapMechanism,
    NetworkAccess, ReducedIsolationReason, SandboxError, TimeoutBudget,
};
use crate::error::{self, SecurityError};

// ---------------------------------------------------------------------------
// Spec
// ---------------------------------------------------------------------------

#[test]
fn spec_defaults_deny_network_and_further_exec() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SandboxSpec::builder()
        .read_path(dir.path())
        .build()
        .unwrap();
    assert_eq!(spec.network(), NetworkAccess::Denied);
    assert_eq!(spec.allow_exec(), ExecAccess::DeniedBeyondHelper);
}

#[test]
fn spec_rejects_relative_paths() {
    let err = SandboxSpec::builder()
        .read_path("relative/input.wav")
        .build()
        .unwrap_err();
    assert!(matches!(err, SpecError::RelativePath { .. }), "{err:?}");
}

#[test]
fn spec_rejects_memory_below_the_floor() {
    let err = SandboxSpec::builder()
        .memory_bytes(1024)
        .build()
        .unwrap_err();
    assert_eq!(
        err,
        SpecError::MemoryTooSmall {
            requested: 1024,
            minimum: MIN_MEMORY_BYTES
        }
    );
}

#[test]
fn spec_rejects_zero_cpu_and_out_of_range_wall_clock() {
    assert_eq!(
        SandboxSpec::builder().cpu_seconds(0).build().unwrap_err(),
        SpecError::CpuBudgetZero
    );
    let err = SandboxSpec::builder()
        .wall_clock(Duration::from_millis(1))
        .build()
        .unwrap_err();
    assert!(
        matches!(err, SpecError::WallClockOutOfRange { .. }),
        "{err:?}"
    );
}

#[test]
fn spec_canonicalizes_grants_and_deduplicates() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.wav");
    std::fs::write(&input, b"pcm").unwrap();

    let spec = SandboxSpec::builder()
        .read_path(&input)
        .read_path(&input)
        .build()
        .unwrap();
    assert_eq!(spec.read_paths().len(), 1, "duplicate grants collapse");
    assert_eq!(spec.read_paths()[0], input.canonicalize().unwrap());
}

#[test]
fn spec_resolves_a_write_target_that_does_not_exist_yet() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("nested").join("out.pcm");
    std::fs::create_dir(dir.path().join("nested")).unwrap();

    let spec = SandboxSpec::builder().write_path(&out).build().unwrap();
    let resolved = &spec.write_paths()[0];
    assert!(resolved.is_absolute());
    assert_eq!(resolved.file_name().unwrap(), "out.pcm");
    assert_eq!(
        resolved.parent().unwrap(),
        dir.path().join("nested").canonicalize().unwrap()
    );
}

#[test]
fn spec_rejects_a_grant_whose_parent_directory_is_missing() {
    // Only the leaf may be absent: an unresolved middle component could later
    // be created as a symlink and silently widen the grant.
    let err = SandboxSpec::builder()
        .write_path("/plaudon-does-not-exist-9f1c/sub/out.pcm")
        .build()
        .unwrap_err();
    assert!(
        matches!(
            err,
            SpecError::Unresolvable | SpecError::NotCanonicalizable { .. }
        ),
        "{err:?}"
    );
}

#[test]
fn spec_errors_carry_stable_envelope_codes() {
    let err = SandboxSpec::builder()
        .read_path("relative")
        .build()
        .unwrap_err();
    let parts = err.envelope_parts();
    assert_eq!(parts.code, error::PATH_REJECTED);
    assert_eq!(parts.action, Some(error::CHOOSE_DIFFERENT_FILE));
}

// ---------------------------------------------------------------------------
// Isolation reporting honesty
// ---------------------------------------------------------------------------

#[test]
fn reduced_isolation_is_never_reported_as_isolated() {
    for reason in [
        ReducedIsolationReason::PrimitiveUnavailable,
        ReducedIsolationReason::PlatformUnsupported,
        ReducedIsolationReason::PermissionDenied,
        ReducedIsolationReason::DeprecatedApi,
    ] {
        let level = IsolationLevel::reduced(reason);
        assert!(!level.is_isolated());
        assert_eq!(level.reduced_reason(), Some(reason));
        assert!(
            level.describe().starts_with("not restricted"),
            "UI text must lead with the limitation: {}",
            level.describe()
        );
    }
}

#[test]
fn sandboxed_level_describes_its_mechanism() {
    let level = IsolationLevel::Sandboxed {
        mechanism: "restricted by a test mechanism",
    };
    assert!(level.is_isolated());
    assert_eq!(level.reduced_reason(), None);
    assert_eq!(level.describe(), "restricted by a test mechanism");
}

#[test]
fn nothing_applied_report_claims_nothing() {
    let report = IsolationReport::nothing_applied(ReducedIsolationReason::PlatformUnsupported);
    assert!(!report.memory);
    assert!(!report.cpu);
    assert!(!report.wall_clock);
    assert!(!report.filesystem.is_isolated());
    assert!(!report.network.is_isolated());
    assert!(!report.exec.is_isolated());
    assert!(!report.is_fully_isolated());
    assert_eq!(report.memory_mechanism, MemoryCapMechanism::None);
    assert!(!report.summary().contains("fully restricted"));
}

#[test]
fn summary_only_claims_full_restriction_when_every_dimension_holds() {
    let sandboxed = IsolationLevel::Sandboxed {
        mechanism: "restricted by a test mechanism",
    };
    let mut report = IsolationReport {
        memory: true,
        cpu: true,
        wall_clock: true,
        filesystem: sandboxed,
        network: sandboxed,
        exec: sandboxed,
        memory_mechanism: MemoryCapMechanism::ResourceLimit,
        notes: &[],
    };
    assert!(report.is_fully_isolated());
    assert!(report.summary().contains("fully restricted"));

    report.network = IsolationLevel::reduced(ReducedIsolationReason::PrimitiveUnavailable);
    assert!(!report.is_fully_isolated());
    assert!(report.summary().starts_with("helper partially restricted"));
    assert!(report
        .describe_lines()
        .iter()
        .any(|line| line.starts_with("network: not restricted")));
}

#[test]
fn notes_are_carried_into_the_description_and_counted_in_the_summary() {
    let sandboxed = IsolationLevel::Sandboxed {
        mechanism: "restricted by a test mechanism",
    };
    let report = IsolationReport {
        memory: true,
        cpu: true,
        wall_clock: true,
        filesystem: sandboxed,
        network: sandboxed,
        exec: sandboxed,
        memory_mechanism: MemoryCapMechanism::ResourceLimit,
        notes: &[
            super::NOTE_WINDOWS_ASSIGN_AFTER_SPAWN,
            super::NOTE_WINDOWS_NO_FILESYSTEM_RESTRICTION,
        ],
    };
    // Even a report whose every dimension holds must surface its caveats.
    assert!(report.is_fully_isolated());
    assert!(report.summary().contains("2 limitation notes recorded"));
    let lines = report.describe_lines();
    assert_eq!(lines.len(), 8, "six dimensions plus two notes: {lines:?}");
    assert!(lines
        .iter()
        .any(|line| line.starts_with("note: Windows: the helper is assigned")));
}

/// Whatever this platform reports, the note vocabulary must be a closed set of
/// the module's `NOTE_*` constants, and every entry must say something.
#[test]
fn the_note_vocabulary_is_closed_and_non_empty() {
    const VOCABULARY: [&str; 6] = [
        super::NOTE_MACOS_SANDBOX_EXEC_DEPRECATED,
        super::NOTE_MACOS_NO_KERNEL_MEMORY_CAP,
        super::NOTE_MACOS_NO_PROFILE,
        super::NOTE_WINDOWS_ASSIGN_AFTER_SPAWN,
        super::NOTE_WINDOWS_NO_FILESYSTEM_RESTRICTION,
        super::NOTE_RESOURCE_CAPS_ONLY,
    ];
    for note in VOCABULARY {
        assert!(note.len() > 40, "a note must explain itself: {note}");
    }

    for strong in [true, false] {
        for mechanism in [
            MemoryCapMechanism::None,
            MemoryCapMechanism::ResourceLimit,
            MemoryCapMechanism::ParentWatchdog {
                sample_interval_ms: 20,
            },
            MemoryCapMechanism::JobObject,
        ] {
            let notes = super::platform_notes(strong, mechanism);
            assert!(
                !notes.is_empty(),
                "every supported platform has at least one standing limitation to declare"
            );
            for note in notes {
                assert!(
                    VOCABULARY.contains(note),
                    "note outside the vocabulary: {note}"
                );
            }
        }
    }
}

/// macOS must always declare both standing limitations: the deprecated
/// primitive, and the absence of a hard kernel memory cap.
#[cfg(target_os = "macos")]
#[test]
fn the_macos_report_declares_the_deprecated_primitive_and_the_sampled_memory_cap() {
    let notes = super::platform_notes(
        true,
        MemoryCapMechanism::ParentWatchdog {
            sample_interval_ms: 20,
        },
    );
    assert!(notes.contains(&super::NOTE_MACOS_SANDBOX_EXEC_DEPRECATED));
    assert!(notes.contains(&super::NOTE_MACOS_NO_KERNEL_MEMORY_CAP));
    assert!(
        super::NOTE_MACOS_NO_KERNEL_MEMORY_CAP.contains("no hard kernel memory cap"),
        "a reader must not be left assuming a kernel cap"
    );
    // Without the profile the report must say that instead.
    let reduced = super::platform_notes(false, MemoryCapMechanism::ResourceLimit);
    assert_eq!(reduced, &[super::NOTE_MACOS_NO_PROFILE]);
}

/// Windows must always declare the assign-after-spawn start window and the
/// absence of a filesystem restriction, whatever else happened.
#[cfg(windows)]
#[test]
fn the_windows_report_declares_the_start_window_and_the_missing_filesystem_restriction() {
    for strong in [true, false] {
        let notes = super::platform_notes(strong, MemoryCapMechanism::JobObject);
        assert!(notes.contains(&super::NOTE_WINDOWS_ASSIGN_AFTER_SPAWN));
        assert!(notes.contains(&super::NOTE_WINDOWS_NO_FILESYSTEM_RESTRICTION));
    }
}

#[test]
fn watchdog_memory_cap_admits_its_sampling_delay() {
    let mechanism = MemoryCapMechanism::ParentWatchdog {
        sample_interval_ms: 20,
    };
    assert!(mechanism.is_enforced());
    assert!(
        mechanism.describe().contains("sampling delay"),
        "the UI text must not imply instantaneous enforcement: {}",
        mechanism.describe()
    );
}

// ---------------------------------------------------------------------------
// Errors and job-state mapping (§14.1, §14.7)
// ---------------------------------------------------------------------------

#[test]
fn timeout_maps_to_helper_timeout_and_is_retryable() {
    let err = SandboxError::Timeout {
        budget: TimeoutBudget::WallClock,
        elapsed_ms: 1_500,
    };
    let parts = err.envelope_parts();
    assert_eq!(parts.code, error::HELPER_TIMEOUT);
    assert!(parts.retryable);
    assert_eq!(parts.action, Some(error::RETRY_JOB));
}

#[test]
fn termination_errors_map_to_helper_failed_and_are_retryable() {
    for err in [
        SandboxError::Killed { signal: 9 },
        SandboxError::Crashed { code: 134 },
        SandboxError::Oom {
            detail: "resident set exceeded".to_owned(),
        },
    ] {
        let parts = err.envelope_parts();
        assert_eq!(parts.code, error::HELPER_FAILED, "{err:?}");
        assert!(parts.retryable, "{err:?}");
        assert_eq!(parts.action, Some(error::RETRY_JOB));
        assert!(err.is_retryable());
    }
}

#[test]
fn protocol_and_spawn_failures_are_terminal() {
    for err in [
        SandboxError::Protocol {
            detail: "truncated frame".to_owned(),
        },
        SandboxError::SpawnFailed {
            detail: "NotFound".to_owned(),
        },
    ] {
        let parts = err.envelope_parts();
        assert_eq!(parts.code, error::HELPER_FAILED, "{err:?}");
        assert!(!parts.retryable, "{err:?}");
        assert!(!err.is_retryable());
        assert_eq!(
            HelperOutcome::from(Err(err)).job_state(),
            JobStateHint::FailedTerminal
        );
    }
}

#[test]
fn a_failed_helper_never_produces_a_completed_outcome() {
    let failures = [
        SandboxError::Timeout {
            budget: TimeoutBudget::Cpu,
            elapsed_ms: 10,
        },
        SandboxError::Killed { signal: 9 },
        SandboxError::Crashed { code: 1 },
        SandboxError::Oom {
            detail: "cap".to_owned(),
        },
        SandboxError::Protocol {
            detail: "bad frame".to_owned(),
        },
        SandboxError::SpawnFailed {
            detail: "NotFound".to_owned(),
        },
    ];
    for err in failures {
        let retryable = err.is_retryable();
        let outcome = HelperOutcome::from(Err(err.clone()));
        assert!(
            !outcome.is_completed(),
            "{err:?} must not look like a completion"
        );
        assert_ne!(outcome.job_state(), JobStateHint::Completed);
        assert_eq!(
            outcome.job_state(),
            if retryable {
                JobStateHint::FailedRetryable
            } else {
                JobStateHint::FailedTerminal
            }
        );
        assert_eq!(outcome.error(), Some(&err));
    }
}

#[test]
fn a_response_is_the_only_route_to_completed() {
    let outcome = HelperOutcome::from(Ok(HelperResponse::Echo {
        payload: "ok".to_owned(),
    }));
    assert!(outcome.is_completed());
    assert_eq!(outcome.job_state(), JobStateHint::Completed);
    assert_eq!(outcome.job_state().as_str(), "Completed");
    assert_eq!(outcome.error(), None);
}

#[test]
fn job_state_names_match_the_spec_vocabulary() {
    assert_eq!(JobStateHint::FailedRetryable.as_str(), "FailedRetryable");
    assert_eq!(JobStateHint::FailedTerminal.as_str(), "FailedTerminal");
}

// ---------------------------------------------------------------------------
// IPC framing
// ---------------------------------------------------------------------------

fn sample_requests() -> Vec<HelperRequest> {
    vec![
        HelperRequest::Hello,
        HelperRequest::Echo {
            payload: "hello".to_owned(),
        },
        HelperRequest::ReadFile {
            path: "/tmp/input.wav".to_owned(),
        },
        HelperRequest::WriteFile {
            path: "/tmp/out.pcm".to_owned(),
            bytes: vec![1, 2, 3],
        },
        HelperRequest::ConnectTcp {
            host: "example.invalid".to_owned(),
            port: 443,
        },
        HelperRequest::SpawnChild,
        HelperRequest::AllocateBytes {
            bytes: 1 << 20,
            graceful: true,
        },
        HelperRequest::BurnCpu { seconds: 1 },
        HelperRequest::SleepMs { ms: 5 },
        HelperRequest::Shutdown,
    ]
}

fn sample_responses() -> Vec<HelperResponse> {
    vec![
        HelperResponse::Hello {
            limits: AppliedLimits {
                address_space_bytes: Some(1 << 30),
                cpu_seconds: Some(30),
                open_files: Some(64),
            },
            pid: 4242,
        },
        HelperResponse::Echo {
            payload: "hello".to_owned(),
        },
        HelperResponse::FileRead {
            bytes: 3,
            sha256: "ab".repeat(32),
        },
        HelperResponse::FileWritten { bytes: 3 },
        HelperResponse::TcpConnected { local_port: 51_000 },
        HelperResponse::ChildSpawned { exit_code: 0 },
        HelperResponse::Allocated { bytes: 1 << 20 },
        HelperResponse::CpuBurned { elapsed_ms: 1_000 },
        HelperResponse::Slept { ms: 5 },
        HelperResponse::Refused {
            operation: Operation::ReadFile,
            refusal: Refusal {
                kind: RefusalKind::PermissionDenied,
                os_error: Some(1),
                detail: "PermissionDenied".to_owned(),
            },
        },
        HelperResponse::Unsupported {
            operation: Operation::SpawnChild,
        },
    ]
}

#[test]
fn every_request_and_response_round_trips_through_a_frame() {
    for request in sample_requests() {
        let frame = ipc::encode(&request).unwrap();
        let mut cursor = Cursor::new(frame);
        let decoded: HelperRequest = ipc::read_message(&mut cursor).unwrap();
        assert_eq!(decoded, request);
    }
    for response in sample_responses() {
        let frame = ipc::encode(&response).unwrap();
        let mut cursor = Cursor::new(frame);
        let decoded: HelperResponse = ipc::read_message(&mut cursor).unwrap();
        assert_eq!(decoded, response);
    }
}

#[test]
fn operation_is_derived_from_the_request() {
    assert_eq!(HelperRequest::Hello.operation(), Operation::Hello);
    assert_eq!(HelperRequest::SpawnChild.operation(), Operation::SpawnChild);
    assert_eq!(
        HelperRequest::AllocateBytes {
            bytes: 1,
            graceful: false
        }
        .operation(),
        Operation::AllocateBytes
    );
}

#[test]
fn a_forged_length_is_rejected_before_any_body_is_read() {
    let mut frame = Vec::new();
    frame.extend_from_slice(&u32::MAX.to_le_bytes());
    // Deliberately no body at all: a decoder that trusted the prefix would
    // try to reserve four gibibytes here.
    let mut cursor = Cursor::new(frame);
    let err = ipc::read_frame_body(&mut cursor).unwrap_err();
    assert_eq!(
        err,
        FrameError::FrameTooLarge {
            declared: u32::MAX,
            max: MAX_FRAME_BYTES
        }
    );
    assert_eq!(
        cursor.position(),
        4,
        "nothing beyond the prefix was consumed"
    );
}

#[test]
fn a_length_one_byte_over_the_cap_is_rejected() {
    let mut frame = Vec::new();
    frame.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
    let err = ipc::read_frame_body(&mut Cursor::new(frame)).unwrap_err();
    assert_eq!(
        err,
        FrameError::FrameTooLarge {
            declared: MAX_FRAME_BYTES + 1,
            max: MAX_FRAME_BYTES
        }
    );
}

#[test]
fn a_truncated_body_is_a_typed_error() {
    let frame = ipc::encode(&HelperRequest::Hello).unwrap();
    let truncated = &frame[..frame.len() - 2];
    let err = ipc::read_frame_body(&mut Cursor::new(truncated.to_vec())).unwrap_err();
    assert!(matches!(err, FrameError::Truncated { .. }), "{err:?}");
}

#[test]
fn a_truncated_length_prefix_is_a_typed_error() {
    let err = ipc::read_frame_body(&mut Cursor::new(vec![1_u8, 0])).unwrap_err();
    assert!(matches!(err, FrameError::Truncated { .. }), "{err:?}");
}

#[test]
fn a_clean_end_of_stream_is_closed_not_an_error_frame() {
    let err = ipc::read_frame_body(&mut Cursor::new(Vec::new())).unwrap_err();
    assert_eq!(err, FrameError::Closed);
}

#[test]
fn an_empty_body_is_rejected() {
    let frame = 0_u32.to_le_bytes().to_vec();
    let err = ipc::read_frame_body(&mut Cursor::new(frame)).unwrap_err();
    assert!(matches!(err, FrameError::Malformed { .. }), "{err:?}");
}

#[test]
fn a_non_utf8_body_is_rejected_without_panicking() {
    let body = vec![0xFF_u8, 0xFE, 0xFD];
    let mut frame = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
    frame.extend_from_slice(&body);
    let read = ipc::read_frame_body(&mut Cursor::new(frame)).unwrap();
    let err = ipc::decode::<HelperRequest>(&read).unwrap_err();
    assert_eq!(err, FrameError::NotUtf8 { offset: 0 });
}

#[test]
fn trailing_garbage_after_a_valid_value_is_rejected() {
    let body = br#"{"op":"hello"} trailing"#;
    let err = ipc::decode::<HelperRequest>(body).unwrap_err();
    match err {
        // Whitespace between the value and the garbage is tolerated; the
        // count starts at the first offending byte.
        FrameError::TrailingGarbage { extra } => assert_eq!(extra, "trailing".len()),
        other => panic!("expected TrailingGarbage, got {other:?}"),
    }
}

#[test]
fn a_second_value_in_one_frame_is_rejected() {
    let err = ipc::decode::<HelperRequest>(br#"{"op":"hello"}{"op":"hello"}"#).unwrap_err();
    assert_eq!(
        err,
        FrameError::TrailingGarbage {
            extra: r#"{"op":"hello"}"#.len()
        }
    );
}

#[test]
fn trailing_whitespace_is_tolerated() {
    let decoded: HelperRequest = ipc::decode(b"{\"op\":\"hello\"}\n").unwrap();
    assert_eq!(decoded, HelperRequest::Hello);
}

#[test]
fn malformed_json_is_rejected() {
    let err = ipc::decode::<HelperRequest>(b"{\"op\":").unwrap_err();
    assert!(matches!(err, FrameError::Malformed { .. }), "{err:?}");
}

#[test]
fn an_unknown_operation_is_rejected() {
    let err = ipc::decode::<HelperRequest>(br#"{"op":"exfiltrate"}"#).unwrap_err();
    assert!(matches!(err, FrameError::Malformed { .. }), "{err:?}");
}

#[test]
fn an_oversized_message_cannot_be_encoded() {
    // One byte of JSON overhead is enough to push this over the cap.
    let payload = "x".repeat(MAX_FRAME_BYTES as usize);
    let err = ipc::encode(&HelperRequest::Echo { payload }).unwrap_err();
    assert!(matches!(err, FrameError::PayloadTooLarge { .. }), "{err:?}");
}

#[test]
fn frames_are_little_endian_length_prefixed() {
    let frame = ipc::encode(&HelperRequest::Hello).unwrap();
    let declared = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
    assert_eq!(declared as usize, frame.len() - 4);
    assert_eq!(&frame[4..], br#"{"op":"hello"}"#);
}

#[test]
fn several_frames_can_be_read_back_to_back() {
    let mut stream = Vec::new();
    for request in sample_requests() {
        stream.extend_from_slice(&ipc::encode(&request).unwrap());
    }
    let mut cursor = Cursor::new(stream);
    for expected in sample_requests() {
        let decoded: HelperRequest = ipc::read_message(&mut cursor).unwrap();
        assert_eq!(decoded, expected);
    }
    assert_eq!(
        ipc::read_message::<_, HelperRequest>(&mut cursor).unwrap_err(),
        FrameError::Closed
    );
}

/// §6.3A: no raw pointer, descriptor or platform handle may cross the helper
/// boundary. The rule is enforced on the source of the protocol types so that
/// adding such a field to the wire format fails the build's test suite.
#[test]
fn ipc_types_contain_no_handle_shaped_fields() {
    const SOURCE: &str = include_str!("ipc.rs");
    const FORBIDDEN: [&str; 10] = [
        "*const",
        "*mut",
        "RawFd",
        "RawHandle",
        "HANDLE",
        "NonNull",
        "mach_port",
        "usize",
        "from_raw",
        "as_raw",
    ];
    const TYPES: [&str; 6] = [
        "pub enum HelperRequest {",
        "pub enum HelperResponse {",
        "pub enum Operation {",
        "pub enum RefusalKind {",
        "pub struct Refusal {",
        "pub struct AppliedLimits {",
    ];

    for header in TYPES {
        let body = brace_body(SOURCE, header);
        for token in FORBIDDEN {
            assert!(
                !body.contains(token),
                "`{header}` must not mention `{token}`: the helper boundary carries only paths, \
                 integers, booleans and enums"
            );
        }
    }
}

/// Extract the `{ .. }` body that follows `header`, balancing braces.
fn brace_body<'a>(source: &'a str, header: &str) -> &'a str {
    let start = source
        .find(header)
        .unwrap_or_else(|| panic!("`{header}` not found in ipc.rs"));
    let open = start + header.len() - 1;
    let bytes = source.as_bytes();
    let mut depth = 0_usize;
    for (offset, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open..=offset];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after `{header}`");
}

// ---------------------------------------------------------------------------
// macOS profile generation
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod seatbelt {
    use std::path::Path;

    #[test]
    fn profile_denies_by_default_and_denies_network() {
        let profile = super::super::macos::profile_for(Path::new("/usr/bin/true"), &[], &[]);
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(deny process-exec*)"));
    }

    #[test]
    fn profile_allows_only_the_helper_to_be_executed() {
        let profile = super::super::macos::profile_for(Path::new("/opt/helper"), &[], &[]);
        let exec_rule = profile
            .lines()
            .find(|line| line.starts_with("(allow process-exec"))
            .expect("an exec rule");
        assert_eq!(exec_rule, "(allow process-exec (literal \"/opt/helper\"))");
        // The exec allowance must come after the blanket deny, otherwise the
        // deny would win and the helper could not start at all.
        let deny_at = profile.find("(deny process-exec*)").expect("deny rule");
        let allow_at = profile.find("(allow process-exec ").expect("allow rule");
        assert!(deny_at < allow_at);
    }

    #[test]
    fn profile_grants_exactly_the_requested_paths() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.wav");
        std::fs::write(&input, b"x").unwrap();
        let output = dir.path().join("out.pcm");

        let profile = super::super::macos::profile_for(
            Path::new("/opt/helper"),
            &[input.as_path()],
            &[output.as_path()],
        );
        assert!(profile.contains(&format!("(literal \"{}\")", input.display())));
        assert!(profile.contains("(allow file-write*"));
        assert!(profile.contains(&format!("(literal \"{}\")", output.display())));
        assert!(!profile.contains("/etc/passwd"));
    }

    #[test]
    fn profile_escapes_quotes_in_paths() {
        let profile = super::super::macos::profile_for(Path::new("/tmp/od\"d/helper"), &[], &[]);
        assert!(profile.contains("/tmp/od\\\"d/helper"), "{profile}");
    }

    #[test]
    fn a_directory_grant_becomes_a_subpath_rule() {
        let dir = tempfile::tempdir().unwrap();
        let profile =
            super::super::macos::profile_for(Path::new("/opt/helper"), &[dir.path()], &[]);
        assert!(
            profile.contains(&format!("(subpath \"{}\")", dir.path().display())),
            "{profile}"
        );
    }
}

// ---------------------------------------------------------------------------
// Windows job object construction
// ---------------------------------------------------------------------------

/// Pure-function coverage for the Windows backend: the limit struct is built
/// from a [`SandboxSpec`] with no process involved, so `windows-latest` CI
/// exercises the field and flag population even though spawning is covered by
/// `tests/sandbox_integration.rs`.
#[cfg(windows)]
mod job_object {
    use std::time::Duration;

    use windows_sys::Win32::System::JobObjects::{
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_TIME,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_HANDLES,
        JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };

    use super::super::windows::{
        extended_limit_information, ui_restrictions, HUNDRED_NANOS_PER_SECOND, JOB_LIMIT_FLAGS,
    };
    use super::SandboxSpec;

    fn spec(memory_bytes: u64, cpu_seconds: u64) -> SandboxSpec {
        SandboxSpec::builder()
            .memory_bytes(memory_bytes)
            .cpu_seconds(cpu_seconds)
            .wall_clock(Duration::from_secs(30))
            .build()
            .expect("valid spec")
    }

    #[test]
    fn the_limit_flags_select_every_cap_we_claim() {
        assert_ne!(JOB_LIMIT_FLAGS & JOB_OBJECT_LIMIT_PROCESS_MEMORY, 0);
        assert_ne!(JOB_LIMIT_FLAGS & JOB_OBJECT_LIMIT_ACTIVE_PROCESS, 0);
        assert_ne!(JOB_LIMIT_FLAGS & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, 0);
        assert_ne!(JOB_LIMIT_FLAGS & JOB_OBJECT_LIMIT_JOB_TIME, 0);
    }

    #[test]
    fn the_spec_budgets_land_in_the_matching_fields() {
        let memory = 512 * 1024 * 1024;
        let limits = extended_limit_information(&spec(memory, 30));

        assert_eq!(limits.BasicLimitInformation.LimitFlags, JOB_LIMIT_FLAGS);
        // A decoder is one process; it may not multiply.
        assert_eq!(limits.BasicLimitInformation.ActiveProcessLimit, 1);
        assert_eq!(
            limits.ProcessMemoryLimit,
            usize::try_from(memory).expect("fits")
        );
        // The CPU budget is expressed in 100-nanosecond units.
        assert_eq!(
            limits.BasicLimitInformation.PerJobUserTimeLimit,
            i64::try_from(30 * HUNDRED_NANOS_PER_SECOND).expect("fits")
        );
        // Nothing we do not claim is set.
        assert_eq!(limits.JobMemoryLimit, 0);
        assert_eq!(limits.BasicLimitInformation.MaximumWorkingSetSize, 0);
    }

    #[test]
    fn an_implausible_cpu_budget_saturates_instead_of_wrapping() {
        let limits = extended_limit_information(&spec(64 * 1024 * 1024, u64::MAX / 2));
        assert!(limits.BasicLimitInformation.PerJobUserTimeLimit > 0);
    }

    #[test]
    fn the_ui_restrictions_cover_the_handles_a_decoder_must_not_touch() {
        let ui = ui_restrictions().UIRestrictionsClass;
        for flag in [
            JOB_OBJECT_UILIMIT_DESKTOP,
            JOB_OBJECT_UILIMIT_EXITWINDOWS,
            JOB_OBJECT_UILIMIT_HANDLES,
            JOB_OBJECT_UILIMIT_READCLIPBOARD,
            JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        ] {
            assert_ne!(ui & flag, 0, "missing UI restriction {flag:#x}");
        }
    }
}
