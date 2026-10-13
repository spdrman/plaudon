//! Typed, length-prefixed framing for the helper boundary (§6.3A).
//!
//! # Wire format
//!
//! ```text
//! +--------------------+-------------------------------+
//! | u32 little-endian  | body: UTF-8 JSON, `len` bytes |
//! | body length `len`  |                               |
//! +--------------------+-------------------------------+
//! ```
//!
//! `len` is validated against [`MAX_FRAME_BYTES`] **before** any buffer is
//! allocated, so a forged 4 GiB length prefix costs four bytes of reading and
//! returns [`FrameError::FrameTooLarge`]. Nothing in this module panics on
//! hostile input: every rejection is a typed [`FrameError`].
//!
//! # No raw handles cross the boundary
//!
//! §6.3A: "we will not share raw Bluetooth handles across arbitrary Tokio
//! workers" and the UI "will never receive ... platform-bridge pointers". The
//! same rule applies to the helper boundary, and more strongly: the helper is
//! on the other side of a process boundary, so a pointer, a file descriptor
//! number, a `HANDLE`, a `mach_port_t` or a `usize` address is meaningless at
//! best and a confused-deputy primitive at worst.
//!
//! **Rule:** [`HelperRequest`] and [`HelperResponse`] may only contain
//! `String`, `bool`, fixed-width integers, `f32`, `Vec<u8>`, `Option` of
//! those, and other enums/structs in this module that obey the same rule. No
//! `usize`, no `*const`/`*mut`, no `RawFd`/`RawHandle`, no `Path`-typed handle
//! wrappers. `f32` is on the list because an inference helper's answer is a
//! vector of them and IEEE-754 is as fixed-width and as pointer-free as `u32`;
//! it is carried by [`EmbeddingVector`], whose `Debug` prints a length and
//! never a component.
//! The rule is enforced by `sandbox::tests::ipc_types_contain_no_handle_shaped_fields`,
//! which reads this file's source and rejects the forbidden tokens.
//!
//! Paths are carried as strings and are re-validated by the *parent* against
//! the [`crate::sandbox::SandboxSpec`] grants; a helper cannot widen its own
//! grant by sending a path back.

use std::io::{Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Largest body a single frame may carry: 16 MiB.
///
/// Chosen so one frame can hold a decoded metadata blob or a model manifest
/// without being large enough that a forged length exhausts memory. Audio PCM
/// is passed by *path*, never through the pipe.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Size of the length prefix in bytes.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Upper bound on the buffer reserved up-front while reading a body. A larger
/// body grows the buffer as bytes actually arrive.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// A framing or decoding failure. Every hostile input maps to one of these.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The declared length exceeds [`MAX_FRAME_BYTES`]. No allocation was made.
    #[error("declared frame length {declared} exceeds the {max} byte cap")]
    FrameTooLarge {
        /// Length claimed by the length prefix.
        declared: u32,
        /// [`MAX_FRAME_BYTES`].
        max: u32,
    },
    /// The stream ended before the declared body was complete.
    #[error("truncated frame: declared {declared} bytes, received {received}")]
    Truncated {
        /// Length claimed by the length prefix.
        declared: u32,
        /// Bytes actually received before end of stream.
        received: u32,
    },
    /// The body is not valid UTF-8.
    #[error("frame body is not valid UTF-8 (first invalid byte at offset {offset})")]
    NotUtf8 {
        /// Offset of the first invalid byte.
        offset: usize,
    },
    /// The body is valid UTF-8 but not a valid message.
    #[error("frame body is not a valid helper message: {detail}")]
    Malformed {
        /// Short parser detail; never contains user content beyond the parser's
        /// own position description.
        detail: String,
    },
    /// The body held a valid message followed by extra bytes.
    #[error("frame body has {extra} trailing byte(s) after the JSON value")]
    TrailingGarbage {
        /// Number of bytes after the end of the JSON value.
        extra: usize,
    },
    /// The message is too large to be framed.
    #[error("message of {len} bytes exceeds the {max} byte frame cap")]
    PayloadTooLarge {
        /// Encoded message length.
        len: usize,
        /// [`MAX_FRAME_BYTES`].
        max: u32,
    },
    /// The peer closed the pipe cleanly with no frame in flight.
    #[error("the helper pipe is closed")]
    Closed,
    /// An I/O error on the pipe.
    #[error("i/o error on the helper pipe: {kind}")]
    Io {
        /// `std::io::ErrorKind` rendered as a short static label.
        kind: String,
    },
}

impl FrameError {
    fn io(err: &std::io::Error) -> Self {
        Self::Io {
            kind: format!("{:?}", err.kind()),
        }
    }
}

/// What a helper was asked to do.
///
/// `Hello` and `Shutdown` are the lifecycle messages every helper must answer.
/// `ReadFile`/`WriteFile` are the real work shape for a decoder, and
/// `LoadModel`/`EmbedWindow` the real work shape for an inference helper:
/// a model file in, a window of canonical PCM in, one vector out. `ConnectTcp`,
/// `SpawnChild`, `AllocateBytes` and `BurnCpu` are **negative controls**: they
/// exist so an integration test can prove the sandbox actually refuses them. A
/// production helper that does not implement an operation answers
/// [`HelperResponse::Unsupported`]; it never silently succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HelperRequest {
    /// Liveness and applied-limit handshake, sent by the parent before any
    /// untrusted input reaches the helper.
    Hello,
    /// Echo the payload back. Used to measure the round trip.
    Echo {
        /// Arbitrary text.
        payload: String,
    },
    /// Read a file and report its length and content digest.
    ReadFile {
        /// Absolute path, as a string.
        path: String,
    },
    /// Write bytes to a file.
    WriteFile {
        /// Absolute path, as a string.
        path: String,
        /// Content to write.
        bytes: Vec<u8>,
    },
    /// Attempt an outbound TCP connection. Must fail under the sandbox.
    ConnectTcp {
        /// Host name or literal address.
        host: String,
        /// TCP port.
        port: u16,
    },
    /// Attempt to execute another program. Must fail under the sandbox.
    SpawnChild,
    /// Attempt to allocate and touch `bytes` of memory.
    AllocateBytes {
        /// Requested allocation.
        bytes: u64,
        /// `true` uses a fallible reservation and reports refusal; `false`
        /// allocates the way an unaudited decoder would, so the test can
        /// observe the resource cap terminating the process.
        graceful: bool,
    },
    /// Spin on the CPU for roughly `seconds`.
    BurnCpu {
        /// Target CPU time.
        seconds: u64,
    },
    /// Verify a model file against the record `model_id` names and open an
    /// inference session over it.
    ///
    /// The helper, not the parent, parses the model: a model file is attacker-
    /// influenced input parsed by native code, which is the whole reason this
    /// boundary exists (ADR-0015, §6.3A).
    LoadModel {
        /// Absolute path of the model file, as a string.
        path: String,
        /// Identifier of the artifact record the helper verifies the file
        /// against before any byte of it is parsed. A helper that does not
        /// know the identifier refuses rather than loading what it was given.
        model_id: String,
    },
    /// Embed one window of a canonical PCM file with the loaded model.
    ///
    /// The samples are passed **by path and range**, never through the pipe:
    /// an hour of 16 kHz mono `f32` is 230 MB and the frame cap is 16 MiB.
    EmbedWindow {
        /// Absolute path of the canonical 16 kHz mono `f32` little-endian PCM
        /// derivative, as a string.
        pcm_path: String,
        /// Index of the window's first sample in that file.
        start_frame: u64,
        /// Number of samples in the window.
        frames: u64,
    },
    /// Sleep for `ms` milliseconds.
    SleepMs {
        /// Sleep duration.
        ms: u64,
    },
    /// Exit cleanly.
    Shutdown,
}

/// The operation a response refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// [`HelperRequest::Hello`].
    Hello,
    /// [`HelperRequest::Echo`].
    Echo,
    /// [`HelperRequest::ReadFile`].
    ReadFile,
    /// [`HelperRequest::WriteFile`].
    WriteFile,
    /// [`HelperRequest::ConnectTcp`].
    ConnectTcp,
    /// [`HelperRequest::SpawnChild`].
    SpawnChild,
    /// [`HelperRequest::AllocateBytes`].
    AllocateBytes,
    /// [`HelperRequest::BurnCpu`].
    BurnCpu,
    /// [`HelperRequest::LoadModel`].
    LoadModel,
    /// [`HelperRequest::EmbedWindow`].
    EmbedWindow,
    /// [`HelperRequest::SleepMs`].
    SleepMs,
    /// [`HelperRequest::Shutdown`].
    Shutdown,
}

impl HelperRequest {
    /// The operation this request names.
    #[must_use]
    pub const fn operation(&self) -> Operation {
        match self {
            Self::Hello => Operation::Hello,
            Self::Echo { .. } => Operation::Echo,
            Self::ReadFile { .. } => Operation::ReadFile,
            Self::WriteFile { .. } => Operation::WriteFile,
            Self::ConnectTcp { .. } => Operation::ConnectTcp,
            Self::LoadModel { .. } => Operation::LoadModel,
            Self::EmbedWindow { .. } => Operation::EmbedWindow,
            Self::SpawnChild => Operation::SpawnChild,
            Self::AllocateBytes { .. } => Operation::AllocateBytes,
            Self::BurnCpu { .. } => Operation::BurnCpu,
            Self::SleepMs { .. } => Operation::SleepMs,
            Self::Shutdown => Operation::Shutdown,
        }
    }
}

/// Why an attempted operation did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// The operating system refused: `EPERM`/`EACCES`, a sandbox denial.
    PermissionDenied,
    /// The target does not exist.
    NotFound,
    /// A resource cap was hit (memory, file descriptors).
    ResourceExhausted,
    /// The network was unreachable or the connection was reset.
    NetworkUnreachable,
    /// Something else; see `detail`.
    Other,
}

/// A refusal, carrying the OS error number where one exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// Coarse classification.
    pub kind: RefusalKind,
    /// `errno` / Win32 error code, when the platform supplied one.
    pub os_error: Option<i32>,
    /// Short description from the OS error. Contains no user content beyond
    /// the OS message itself.
    pub detail: String,
}

impl Refusal {
    /// Classify a `std::io::Error` from an attempted operation.
    #[must_use]
    pub fn from_io(err: &std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        let kind = match err.kind() {
            K::PermissionDenied => RefusalKind::PermissionDenied,
            K::NotFound => RefusalKind::NotFound,
            K::OutOfMemory => RefusalKind::ResourceExhausted,
            K::NetworkUnreachable | K::ConnectionRefused | K::HostUnreachable | K::TimedOut => {
                RefusalKind::NetworkUnreachable
            }
            _ => RefusalKind::Other,
        };
        Self {
            kind,
            os_error: err.raw_os_error(),
            detail: format!("{:?}", err.kind()),
        }
    }
}

/// Resource limits the helper observes on itself at start-up.
///
/// This is a *self-report*: it is taken before any untrusted input is read, and
/// it is used to decide whether the parent must start its own enforcement. It
/// is never treated as proof of isolation against a compromised helper — see
/// [`crate::sandbox::IsolationReport`] for what the parent enforces itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AppliedLimits {
    /// `RLIMIT_AS` soft limit, `None` when unlimited or unavailable.
    pub address_space_bytes: Option<u64>,
    /// `RLIMIT_CPU` soft limit in seconds, `None` when unlimited.
    pub cpu_seconds: Option<u64>,
    /// `RLIMIT_NOFILE` soft limit, `None` when unlimited.
    pub open_files: Option<u64>,
}

/// One model output vector, carried from an inference helper to its parent.
///
/// A speaker embedding *is* biometric data (VP-002), so this type exists to
/// make the careless thing impossible rather than merely discouraged: its
/// `Debug` prints the length and never a component, which means a vector
/// cannot reach a log line, a panic message or a diagnostics bundle through
/// the `{:?}` of the response that carries it.
///
/// Equality is **bit-for-bit** rather than IEEE-754: `NaN == NaN` holds here.
/// That is what makes [`Eq`] sound (reflexivity), and it is also the only
/// equality a transport should offer — "these two frames carry the same
/// numbers" is a wire question, not a numerical one.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EmbeddingVector(pub Vec<f32>);

impl EmbeddingVector {
    /// The components, for the one caller that must do arithmetic on them.
    #[must_use]
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// Number of components.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when the helper answered with no components at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for EmbeddingVector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EmbeddingVector({} components, redacted)", self.0.len())
    }
}

impl PartialEq for EmbeddingVector {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && std::iter::zip(&self.0, &other.0).all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl Eq for EmbeddingVector {}

/// What the helper did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum HelperResponse {
    /// Answer to [`HelperRequest::Hello`].
    Hello {
        /// Limits the helper observes on itself.
        limits: AppliedLimits,
        /// The helper's own process id, used by the parent to confirm it is
        /// talking to the process it spawned (or to the process `sandbox-exec`
        /// exec'd on its behalf).
        pid: u32,
    },
    /// Answer to [`HelperRequest::Echo`].
    Echo {
        /// The echoed payload.
        payload: String,
    },
    /// A file was read.
    FileRead {
        /// Byte length read.
        bytes: u64,
        /// Lower-case hex SHA-256 of the content, so the caller can prove the
        /// bytes were not altered.
        sha256: String,
    },
    /// A file was written.
    FileWritten {
        /// Byte length written.
        bytes: u64,
    },
    /// A TCP connection was established. Under a working sandbox profile this
    /// must not happen for a non-loopback destination.
    TcpConnected {
        /// Local port of the established socket.
        local_port: u16,
    },
    /// A child process was executed. Under a working sandbox profile this must
    /// not happen.
    ChildSpawned {
        /// Child exit code.
        exit_code: i32,
    },
    /// Memory was allocated and touched.
    Allocated {
        /// Bytes allocated.
        bytes: u64,
    },
    /// CPU was burned.
    CpuBurned {
        /// Wall time spent spinning.
        elapsed_ms: u64,
    },
    /// The helper slept.
    Slept {
        /// Milliseconds slept.
        ms: u64,
    },
    /// A model file was verified and an inference session was opened over it.
    ModelLoaded {
        /// Identifier of the artifact record the file was verified against.
        model_id: String,
        /// Version of that record.
        version: String,
        /// Lower-case hex SHA-256 the file was verified to have, so the parent
        /// binds its vectors to the same checkpoint the helper used.
        sha256: String,
        /// Dimensionality of every vector this session produces.
        dim: u32,
        /// Wall time the helper spent verifying the file and creating the
        /// session, which is the §4 start-up figure measured where the work
        /// actually happens.
        startup_ms: u64,
    },
    /// A window was embedded.
    Embedded {
        /// The model's raw output. Biometric data: never logged, never
        /// rendered, and redacted by its own `Debug` (VP-002, VP-112).
        vector: EmbeddingVector,
    },
    /// The operation was attempted and refused by the OS.
    Refused {
        /// Operation that was refused.
        operation: Operation,
        /// Why.
        refusal: Refusal,
    },
    /// The helper does not implement this operation.
    Unsupported {
        /// Operation that is not implemented.
        operation: Operation,
    },
}

/// Encode a message as a complete length-prefixed frame.
///
/// # Errors
///
/// [`FrameError::PayloadTooLarge`] if the encoded message exceeds
/// [`MAX_FRAME_BYTES`], [`FrameError::Malformed`] if serialization fails.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let body = serde_json::to_vec(value).map_err(|err| FrameError::Malformed {
        detail: err.to_string(),
    })?;
    let len = body.len();
    if u64::try_from(len).unwrap_or(u64::MAX) > u64::from(MAX_FRAME_BYTES) {
        return Err(FrameError::PayloadTooLarge {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + len);
    // The cast is checked immediately above.
    #[allow(clippy::cast_possible_truncation)]
    frame.extend_from_slice(&(len as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Decode a frame body into a message, rejecting trailing bytes.
///
/// Exactly one JSON value may occupy the frame. Whitespace after the value is
/// tolerated (a helper written in another language may append a newline);
/// anything else is [`FrameError::TrailingGarbage`], whose `extra` counts
/// from the first offending non-whitespace byte. A second JSON value in the
/// same frame is therefore rejected rather than silently ignored.
///
/// # Errors
///
/// [`FrameError::NotUtf8`], [`FrameError::Malformed`] or
/// [`FrameError::TrailingGarbage`].
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    let text = std::str::from_utf8(body).map_err(|err| FrameError::NotUtf8 {
        offset: err.valid_up_to(),
    })?;
    let mut de = serde_json::Deserializer::from_str(text);
    let value = T::deserialize(&mut de).map_err(|err| FrameError::Malformed {
        detail: err.to_string(),
    })?;
    match de.end() {
        Ok(()) => Ok(value),
        Err(err) => {
            // `serde_json` reports a 1-based column for the offending byte.
            let consumed = offset_of(text, &err);
            Err(FrameError::TrailingGarbage {
                extra: body.len().saturating_sub(consumed),
            })
        }
    }
}

fn offset_of(text: &str, err: &serde_json::Error) -> usize {
    let line = err.line();
    let column = err.column();
    if line <= 1 {
        return column.saturating_sub(1);
    }
    let mut offset = 0;
    for (index, l) in text.split_inclusive('\n').enumerate() {
        if index + 1 == line {
            return offset + column.saturating_sub(1);
        }
        offset += l.len();
    }
    offset
}

/// Write a complete frame to `writer` and flush it.
///
/// # Errors
///
/// Encoding errors as for [`encode`], plus [`FrameError::Io`].
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    let frame = encode(value)?;
    writer
        .write_all(&frame)
        .map_err(|err| FrameError::io(&err))?;
    writer.flush().map_err(|err| FrameError::io(&err))?;
    Ok(())
}

/// Read one frame body from `reader`.
///
/// The declared length is checked against [`MAX_FRAME_BYTES`] before a buffer
/// is reserved, and the reservation is capped at 64 KiB so a large-but-legal
/// declaration cannot be used to force a large allocation without sending the
/// bytes.
///
/// # Errors
///
/// [`FrameError::Closed`] at a clean end of stream, [`FrameError::Truncated`],
/// [`FrameError::FrameTooLarge`], [`FrameError::Io`].
pub fn read_frame_body<R: Read>(reader: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
    let mut filled = 0;
    while filled < LENGTH_PREFIX_BYTES {
        match reader.read(&mut prefix[filled..]) {
            Ok(0) => {
                return Err(if filled == 0 {
                    FrameError::Closed
                } else {
                    FrameError::Truncated {
                        declared: u32::try_from(LENGTH_PREFIX_BYTES).unwrap_or(u32::MAX),
                        received: u32::try_from(filled).unwrap_or(u32::MAX),
                    }
                });
            }
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(FrameError::io(&err)),
        }
    }

    let declared = u32::from_le_bytes(prefix);
    if declared > MAX_FRAME_BYTES {
        // Rejected before a single byte of body is read or reserved.
        return Err(FrameError::FrameTooLarge {
            declared,
            max: MAX_FRAME_BYTES,
        });
    }
    if declared == 0 {
        return Err(FrameError::Malformed {
            detail: "empty frame body".to_owned(),
        });
    }

    let declared_usize = usize::try_from(declared).unwrap_or(usize::MAX);
    let mut body = Vec::with_capacity(declared_usize.min(READ_CHUNK_BYTES));
    let read = (&mut *reader)
        .take(u64::from(declared))
        .read_to_end(&mut body)
        .map_err(|err| FrameError::io(&err))?;
    if read != declared_usize {
        return Err(FrameError::Truncated {
            declared,
            received: u32::try_from(read).unwrap_or(u32::MAX),
        });
    }
    Ok(body)
}

/// Read one frame and decode it.
///
/// # Errors
///
/// Any [`FrameError`] from [`read_frame_body`] or [`decode`].
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
    let body = read_frame_body(reader)?;
    decode(&body)
}

/// The resource limits this process currently has, for the `Hello` handshake.
#[must_use]
pub fn current_limits() -> AppliedLimits {
    #[cfg(unix)]
    {
        crate::sandbox::rlimits::observe()
    }
    #[cfg(not(unix))]
    {
        // Windows has no `getrlimit`; the parent's job object is the authority
        // and reports its own enforcement, so an empty self-report is honest.
        AppliedLimits::default()
    }
}
