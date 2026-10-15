//! Helper-boundary framing against hostile and failing streams (§6.3A).
//!
//! `src/sandbox/tests.rs` covers the well-formed round trip and the forged
//! length prefix over an in-memory cursor, which can never fail. This suite
//! drives the arms a cursor cannot reach: a pipe that errors mid-prefix, a
//! reader that is interrupted, a dribbling reader that delivers the length
//! prefix one byte at a time, a writer that refuses, and a frame whose trailing
//! garbage sits on a later line.

use std::io::{self, Cursor, Read, Write};

use plaudon_security::sandbox::ipc::{
    self, AppliedLimits, FrameError, HelperRequest, HelperResponse, Operation, Refusal,
    RefusalKind, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES,
};

/// One scripted outcome of a `read` call: bytes to serve, or a failure.
type ReadStep = io::Result<Vec<u8>>;

/// A reader that replays a scripted sequence of read outcomes.
struct ScriptedReader {
    steps: Vec<ReadStep>,
    index: usize,
}

impl ScriptedReader {
    fn new(steps: Vec<ReadStep>) -> Self {
        Self { steps, index: 0 }
    }
}

impl Read for ScriptedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.index >= self.steps.len() {
            return Ok(0);
        }
        self.index += 1;
        match &mut self.steps[self.index - 1] {
            Ok(bytes) => {
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                bytes.drain(..n);
                if !bytes.is_empty() {
                    // Re-serve the remainder on the next call.
                    self.index -= 1;
                }
                Ok(n)
            }
            Err(err) => Err(io::Error::new(err.kind(), "scripted failure")),
        }
    }
}

/// A writer that fails on `write_all`, or succeeds but fails to flush.
struct FailingWriter {
    fail_write: bool,
    written: Vec<u8>,
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe gone"));
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_write {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe gone"));
        }
        Err(io::Error::new(io::ErrorKind::StorageFull, "no room"))
    }
}

/// An I/O failure while reading the length prefix is a typed `Io` error naming
/// the kind, and no body is read.
#[test]
fn a_pipe_that_fails_during_the_length_prefix_is_a_typed_io_error() {
    let mut reader =
        ScriptedReader::new(vec![Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone"))]);
    let err = ipc::read_frame_body(&mut reader).unwrap_err();
    assert_eq!(
        err,
        FrameError::Io {
            kind: "BrokenPipe".to_owned()
        }
    );
    assert_eq!(err.to_string(), "i/o error on the helper pipe: BrokenPipe");
}

/// An I/O failure after a complete prefix but during the body is also typed,
/// and it is distinguishable from a clean truncation.
#[test]
fn a_pipe_that_fails_during_the_body_is_an_io_error_not_a_truncation() {
    let frame = ipc::encode(&HelperRequest::Hello).unwrap();
    let mut reader = ScriptedReader::new(vec![
        Ok(frame[..LENGTH_PREFIX_BYTES].to_vec()),
        Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset")),
    ]);
    assert_eq!(
        ipc::read_frame_body(&mut reader).unwrap_err(),
        FrameError::Io {
            kind: "ConnectionReset".to_owned()
        }
    );
}

/// An interrupted read is retried rather than reported, because `EINTR` is not
/// a protocol failure.
#[test]
fn an_interrupted_read_is_retried_and_the_frame_still_arrives() {
    let frame = ipc::encode(&HelperRequest::Shutdown).unwrap();
    let mut reader = ScriptedReader::new(vec![
        Err(io::Error::new(io::ErrorKind::Interrupted, "eintr")),
        Ok(frame[..2].to_vec()),
        Err(io::Error::new(io::ErrorKind::Interrupted, "eintr")),
        Ok(frame[2..].to_vec()),
    ]);
    let body = ipc::read_frame_body(&mut reader).expect("the frame must survive EINTR");
    let decoded: HelperRequest = ipc::decode(&body).unwrap();
    assert_eq!(decoded, HelperRequest::Shutdown);
}

/// A reader that delivers the prefix one byte at a time is accumulated, not
/// mistaken for a short frame.
#[test]
fn a_dribbling_reader_has_its_length_prefix_accumulated() {
    let frame = ipc::encode(&HelperRequest::Echo {
        payload: "hi".to_owned(),
    })
    .unwrap();
    let mut steps: Vec<ReadStep> = frame[..LENGTH_PREFIX_BYTES]
        .iter()
        .map(|b| Ok(vec![*b]))
        .collect();
    steps.push(Ok(frame[LENGTH_PREFIX_BYTES..].to_vec()));

    let mut reader = ScriptedReader::new(steps);
    let body = ipc::read_frame_body(&mut reader).unwrap();
    let decoded: HelperRequest = ipc::decode(&body).unwrap();
    assert_eq!(
        decoded,
        HelperRequest::Echo {
            payload: "hi".to_owned()
        }
    );
}

/// A stream that ends part-way through the length prefix is a truncation that
/// reports how many of the four bytes arrived — not `Closed`, which means "no
/// frame was in flight".
#[test]
fn a_prefix_that_ends_mid_way_is_a_truncation_with_the_received_count() {
    for received in 1..LENGTH_PREFIX_BYTES {
        let mut reader = Cursor::new(vec![0_u8; received]);
        assert_eq!(
            ipc::read_frame_body(&mut reader).unwrap_err(),
            FrameError::Truncated {
                declared: u32::try_from(LENGTH_PREFIX_BYTES).unwrap(),
                received: u32::try_from(received).unwrap(),
            },
            "{received} byte(s) of prefix"
        );
    }
    assert_eq!(
        ipc::read_frame_body(&mut Cursor::new(Vec::new())).unwrap_err(),
        FrameError::Closed
    );
}

/// The declared length is checked against the cap at its exact boundary: the
/// cap itself is legal (and then truncates), one byte over is refused before
/// any allocation.
#[test]
fn the_frame_cap_is_checked_at_its_exact_boundary() {
    let mut at_cap = MAX_FRAME_BYTES.to_le_bytes().to_vec();
    at_cap.extend_from_slice(b"{}");
    assert_eq!(
        ipc::read_frame_body(&mut Cursor::new(at_cap)).unwrap_err(),
        FrameError::Truncated {
            declared: MAX_FRAME_BYTES,
            received: 2,
        },
        "a legal declaration is accepted and then found short"
    );

    let over_cap = (MAX_FRAME_BYTES + 1).to_le_bytes().to_vec();
    let err = ipc::read_frame_body(&mut Cursor::new(over_cap)).unwrap_err();
    assert_eq!(
        err,
        FrameError::FrameTooLarge {
            declared: MAX_FRAME_BYTES + 1,
            max: MAX_FRAME_BYTES,
        }
    );
    assert_eq!(
        err.to_string(),
        format!(
            "declared frame length {} exceeds the {MAX_FRAME_BYTES} byte cap",
            MAX_FRAME_BYTES + 1
        )
    );

    let forged = u32::MAX.to_le_bytes().to_vec();
    assert!(matches!(
        ipc::read_frame_body(&mut Cursor::new(forged)).unwrap_err(),
        FrameError::FrameTooLarge { declared, .. } if declared == u32::MAX
    ));
}

/// Trailing garbage on a later line is counted from the right offset, so the
/// reported `extra` is the real number of surplus bytes.
#[test]
fn trailing_garbage_on_a_later_line_is_counted_from_the_right_offset() {
    let body = b"{\n  \"op\": \"hello\"\n}\nnot-json";
    let err = ipc::decode::<HelperRequest>(body).unwrap_err();
    match err {
        FrameError::TrailingGarbage { extra } => {
            assert_eq!(extra, "not-json".len(), "counted from the offending byte");
        }
        other => panic!("expected trailing garbage, got {other:?}"),
    }

    // The same shape on the first line.
    let err = ipc::decode::<HelperRequest>(b"{\"op\":\"hello\"} x").unwrap_err();
    match err {
        FrameError::TrailingGarbage { extra } => assert_eq!(extra, 1),
        other => panic!("expected trailing garbage, got {other:?}"),
    }

    // Whitespace of every flavour is tolerated after the value.
    for suffix in ["", " ", "\n", "\r\n", "\t\n  "] {
        let text = format!("{{\"op\":\"hello\"}}{suffix}");
        let decoded: HelperRequest = ipc::decode(text.as_bytes()).unwrap();
        assert_eq!(decoded, HelperRequest::Hello);
    }
}

/// A non-UTF-8 body reports the offset of the first invalid byte, wherever it is.
#[test]
fn a_non_utf8_body_reports_the_offset_of_the_first_invalid_byte() {
    assert_eq!(
        ipc::decode::<HelperRequest>(&[0xFF, 0xFE]).unwrap_err(),
        FrameError::NotUtf8 { offset: 0 }
    );
    let mut body = br#"{"op":"echo","payload":""#.to_vec();
    let offset = body.len();
    body.extend_from_slice(&[0x80, 0x22, 0x7D]);
    assert_eq!(
        ipc::decode::<HelperRequest>(&body).unwrap_err(),
        FrameError::NotUtf8 { offset }
    );
    assert_eq!(
        FrameError::NotUtf8 { offset: 7 }.to_string(),
        "frame body is not valid UTF-8 (first invalid byte at offset 7)"
    );
}

/// A writer that refuses the frame reports a typed I/O error rather than
/// claiming the message was sent; a writer that accepts the bytes but refuses
/// the flush is also a failure, because an unflushed frame is not delivered.
#[test]
fn a_writer_that_refuses_is_reported_rather_than_claimed_as_sent() {
    let mut broken = FailingWriter {
        fail_write: true,
        written: Vec::new(),
    };
    assert_eq!(
        ipc::write_frame(&mut broken, &HelperRequest::Hello).unwrap_err(),
        FrameError::Io {
            kind: "BrokenPipe".to_owned()
        }
    );
    assert!(broken.written.is_empty(), "nothing reached the pipe");

    let mut unflushable = FailingWriter {
        fail_write: false,
        written: Vec::new(),
    };
    assert_eq!(
        ipc::write_frame(&mut unflushable, &HelperRequest::Hello).unwrap_err(),
        FrameError::Io {
            kind: "StorageFull".to_owned()
        }
    );
    assert!(!unflushable.written.is_empty(), "the bytes were accepted");

    // A working writer produces a frame the reader accepts.
    let mut good = Vec::new();
    ipc::write_frame(&mut good, &HelperResponse::FileWritten { bytes: 7 }).unwrap();
    let decoded: HelperResponse = ipc::read_message(&mut Cursor::new(good)).unwrap();
    assert_eq!(decoded, HelperResponse::FileWritten { bytes: 7 });
}

/// A message too large to frame is refused at encode time with the measured
/// length, so nothing is written to the pipe.
#[test]
fn an_oversized_message_is_refused_at_encode_time_with_its_measured_length() {
    let payload = "a".repeat(usize::try_from(MAX_FRAME_BYTES).unwrap() + 1);
    let err = ipc::encode(&HelperRequest::Echo { payload }).unwrap_err();
    match err {
        FrameError::PayloadTooLarge { len, max } => {
            assert!(len > usize::try_from(MAX_FRAME_BYTES).unwrap());
            assert_eq!(max, MAX_FRAME_BYTES);
            assert_eq!(
                FrameError::PayloadTooLarge { len, max }.to_string(),
                format!("message of {len} bytes exceeds the {max} byte frame cap")
            );
        }
        other => panic!("expected a payload-too-large refusal, got {other:?}"),
    }
}

/// Every request names its operation, and the operation vocabulary round-trips
/// through serde so a helper written in another language can answer it.
#[test]
fn every_request_names_its_operation_and_the_vocabulary_round_trips() {
    let cases = [
        (HelperRequest::Hello, Operation::Hello),
        (
            HelperRequest::Echo {
                payload: String::new(),
            },
            Operation::Echo,
        ),
        (
            HelperRequest::ReadFile {
                path: "/tmp/x".to_owned(),
            },
            Operation::ReadFile,
        ),
        (
            HelperRequest::WriteFile {
                path: "/tmp/x".to_owned(),
                bytes: vec![1, 2],
            },
            Operation::WriteFile,
        ),
        (
            HelperRequest::ConnectTcp {
                host: "example.invalid".to_owned(),
                port: 443,
            },
            Operation::ConnectTcp,
        ),
        (HelperRequest::SpawnChild, Operation::SpawnChild),
        (
            HelperRequest::AllocateBytes {
                bytes: 1,
                graceful: true,
            },
            Operation::AllocateBytes,
        ),
        (HelperRequest::BurnCpu { seconds: 1 }, Operation::BurnCpu),
        (HelperRequest::SleepMs { ms: 1 }, Operation::SleepMs),
        (HelperRequest::Shutdown, Operation::Shutdown),
    ];
    assert_eq!(cases.len(), 10);
    for (request, operation) in cases {
        assert_eq!(request.operation(), operation);
        let frame = ipc::encode(&request).unwrap();
        let decoded: HelperRequest = ipc::read_message(&mut Cursor::new(frame)).unwrap();
        assert_eq!(decoded, request);

        let json = serde_json::to_string(&operation).unwrap();
        let back: Operation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, operation);
    }
}

/// An OS refusal is classified into the closed vocabulary, keeping the raw
/// `errno` where the platform gave one, and anything unrecognised lands in
/// `Other` instead of being guessed into a security-relevant class.
#[test]
fn an_os_refusal_is_classified_into_the_closed_vocabulary() {
    let cases = [
        (
            io::ErrorKind::PermissionDenied,
            RefusalKind::PermissionDenied,
        ),
        (io::ErrorKind::NotFound, RefusalKind::NotFound),
        (io::ErrorKind::OutOfMemory, RefusalKind::ResourceExhausted),
        (
            io::ErrorKind::NetworkUnreachable,
            RefusalKind::NetworkUnreachable,
        ),
        (
            io::ErrorKind::ConnectionRefused,
            RefusalKind::NetworkUnreachable,
        ),
        (
            io::ErrorKind::HostUnreachable,
            RefusalKind::NetworkUnreachable,
        ),
        (io::ErrorKind::TimedOut, RefusalKind::NetworkUnreachable),
        (io::ErrorKind::AlreadyExists, RefusalKind::Other),
        (io::ErrorKind::InvalidData, RefusalKind::Other),
        (io::ErrorKind::WriteZero, RefusalKind::Other),
    ];
    for (kind, expected) in cases {
        let refusal = Refusal::from_io(&io::Error::new(kind, "x"));
        assert_eq!(refusal.kind, expected, "{kind:?}");
        assert_eq!(refusal.detail, format!("{kind:?}"));
        assert_eq!(
            refusal.os_error, None,
            "a synthesised error carries no errno"
        );
    }

    // A real OS error keeps its numeric code for the diagnostics bundle.
    let real = std::fs::metadata("/definitely/not/here/plaudon-cov").unwrap_err();
    let refusal = Refusal::from_io(&real);
    assert_eq!(refusal.kind, RefusalKind::NotFound);
    assert!(refusal.os_error.is_some());

    // The refusal travels through a response frame unchanged.
    let response = HelperResponse::Refused {
        operation: Operation::ConnectTcp,
        refusal: Refusal {
            kind: RefusalKind::PermissionDenied,
            os_error: Some(1),
            detail: "PermissionDenied".to_owned(),
        },
    };
    let frame = ipc::encode(&response).unwrap();
    let decoded: HelperResponse = ipc::read_message(&mut Cursor::new(frame)).unwrap();
    assert_eq!(decoded, response);
}

/// Every framing failure renders a distinct, content-free sentence.
#[test]
fn every_framing_failure_renders_a_distinct_content_free_sentence() {
    let errors = [
        FrameError::FrameTooLarge {
            declared: 1,
            max: 2,
        },
        FrameError::Truncated {
            declared: 4,
            received: 1,
        },
        FrameError::NotUtf8 { offset: 0 },
        FrameError::Malformed {
            detail: "expected value".to_owned(),
        },
        FrameError::TrailingGarbage { extra: 3 },
        FrameError::PayloadTooLarge { len: 9, max: 2 },
        FrameError::Closed,
        FrameError::Io {
            kind: "BrokenPipe".to_owned(),
        },
    ];
    let mut rendered = std::collections::BTreeSet::new();
    for error in &errors {
        let text = error.to_string();
        assert!(!text.is_empty(), "{error:?} renders as nothing");
        assert!(rendered.insert(text.clone()), "duplicate sentence: {text}");
    }
    assert_eq!(rendered.len(), errors.len());
    assert_eq!(FrameError::Closed.to_string(), "the helper pipe is closed");
}

/// An unknown operation, a wrong-typed field and a bare scalar are all rejected
/// as malformed rather than coerced into a nearby variant.
#[test]
fn a_message_that_is_not_this_protocol_is_malformed_not_coerced() {
    let hostile: [&[u8]; 6] = [
        br#"{"op":"exfiltrate"}"#,
        br#"{"op":"sleep_ms","ms":"forever"}"#,
        br#"{"op":"connect_tcp","host":"x","port":70000}"#,
        br#"{"op":"echo"}"#,
        b"42",
        b"[]",
    ];
    for body in hostile {
        let err = ipc::decode::<HelperRequest>(body).unwrap_err();
        assert!(
            matches!(err, FrameError::Malformed { .. }),
            "{:?} should be malformed, got {err:?}",
            String::from_utf8_lossy(body)
        );
    }

    // An empty declared body is malformed before any decoding is attempted.
    let mut empty = 0_u32.to_le_bytes().to_vec();
    empty.extend_from_slice(b"ignored");
    assert!(matches!(
        ipc::read_frame_body(&mut Cursor::new(empty)).unwrap_err(),
        FrameError::Malformed { .. }
    ));
}

/// The self-reported limits are a plain data record, with `None` meaning
/// "unlimited or unavailable" rather than zero.
#[test]
fn the_self_reported_limits_default_to_unknown_rather_than_zero() {
    let empty = AppliedLimits::default();
    assert_eq!(empty.address_space_bytes, None);
    assert_eq!(empty.cpu_seconds, None);
    assert_eq!(empty.open_files, None);

    let observed = ipc::current_limits();
    for value in [
        observed.address_space_bytes,
        observed.cpu_seconds,
        observed.open_files,
    ] {
        assert!(
            value.is_none_or(|v| v > 0),
            "a reported limit must be positive, got {value:?}"
        );
    }

    let hello = HelperResponse::Hello {
        limits: observed,
        pid: 1,
    };
    let frame = ipc::encode(&hello).unwrap();
    let decoded: HelperResponse = ipc::read_message(&mut Cursor::new(frame)).unwrap();
    assert_eq!(decoded, hello);
}
