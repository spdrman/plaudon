//! Structured logging whose redaction happens **at the sink**.
//!
//! Every formatted record passes through [`Redactor`] inside the
//! [`MakeWriter`] that `tracing-subscriber` writes to, after the event has been
//! formatted and before a single byte reaches a file. No call site can bypass
//! it: there is no path from `tracing::info!` to the log file that does not go
//! through [`RedactingWriter`] (spec §1.5, §16.2, App. B.7).

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracing::Subscriber;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

use crate::redact::{Redactor, RegisterSecretError};

/// Base name of the active log file inside the configured directory.
pub const LOG_FILE_NAME: &str = "plaudon.log";

/// Environment variable read for the default filter directive.
pub const FILTER_ENV: &str = "PLAUDON_LOG";

/// Rendering of a log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// One JSON object per record; what support bundles and CI consume.
    #[default]
    Json,
    /// Human-readable single line.
    Human,
}

/// Whether raw wire payloads (BLE/GATT frames, HTTP request/response bodies)
/// may be logged.
///
/// Spec App. B.7 requires wire-payload logging to be **disabled by default**
/// and §7.7 forbids logging raw vendor GATT payloads in production because they
/// can contain credentials or audio. `Enabled` is `#[non_exhaustive]`, so no
/// other crate can construct it directly; the only constructor,
/// [`WirePayloadLogging::enabled_for_debug_build`], exists solely when the
/// `wire-diagnostics` feature is on **and** `debug_assertions` holds. A release
/// build therefore has no way to produce the `Enabled` value at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WirePayloadLogging {
    /// Wire payloads are never formatted and never written.
    #[default]
    Disabled,
    /// Wire payloads are formatted, redacted and written at DEBUG level.
    #[non_exhaustive]
    Enabled,
}

impl WirePayloadLogging {
    /// Enable wire-payload diagnostics. Only exists in a debug build compiled
    /// with the `wire-diagnostics` feature.
    #[cfg(all(feature = "wire-diagnostics", debug_assertions))]
    #[must_use]
    pub const fn enabled_for_debug_build() -> Self {
        Self::Enabled
    }

    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Configuration for [`init_logging`].
#[derive(Debug, Clone)]
pub struct LoggingConfig {
    /// Directory that will hold `plaudon.log` and its rotations. Created if
    /// missing; the caller owns the choice of location (spec §16.5 keeps path
    /// decisions at the trust boundary, not in the logger).
    pub directory: PathBuf,
    /// Rotate once the active file would exceed this many bytes.
    pub max_file_bytes: u64,
    /// Total number of files kept, including the active one. Must be >= 1.
    pub max_files: usize,
    /// Record rendering.
    pub format: LogFormat,
    /// `EnvFilter` directive, e.g. `info,plaudon_jobs=debug`.
    pub filter: String,
    /// Raw wire payload logging; [`WirePayloadLogging::Disabled`] by default.
    pub wire_payload_logging: WirePayloadLogging,
}

impl LoggingConfig {
    /// Defaults: JSON, 8 MiB × 5 files, filter from `PLAUDON_LOG` or `info`,
    /// wire payload logging disabled.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            max_file_bytes: 8 * 1024 * 1024,
            max_files: 5,
            format: LogFormat::Json,
            filter: std::env::var(FILTER_ENV).unwrap_or_else(|_| "info".to_owned()),
            wire_payload_logging: WirePayloadLogging::Disabled,
        }
    }

    #[must_use]
    pub fn with_format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }

    #[must_use]
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = filter.into();
        self
    }

    #[must_use]
    pub fn with_rotation(mut self, max_file_bytes: u64, max_files: usize) -> Self {
        self.max_file_bytes = max_file_bytes;
        self.max_files = max_files;
        self
    }

    #[must_use]
    pub fn with_wire_payload_logging(mut self, wire: WirePayloadLogging) -> Self {
        self.wire_payload_logging = wire;
        self
    }
}

/// Why logging could not be initialised.
#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("log directory {path} could not be prepared: {source}")]
    Directory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("log file {path} could not be opened: {source}")]
    OpenFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid log filter directive: {0}")]
    Filter(String),
    #[error("invalid logging configuration: {0}")]
    Config(&'static str),
    #[error("a global tracing subscriber is already installed")]
    AlreadyInitialized,
}

/// Handle to the installed logging stack.
///
/// Cloneable and `Send + Sync`: hand it to the crash hook, to settings code
/// that needs to register a freshly entered API key, and to subsystems that
/// want to offer wire diagnostics.
#[derive(Debug, Clone)]
pub struct LoggingHandle {
    redactor: Arc<Redactor>,
    sink: Arc<RollingSink>,
    wire: WirePayloadLogging,
}

impl LoggingHandle {
    /// The shared redactor. Also usable directly for text that is rendered
    /// outside `tracing` (error envelopes, notifications, crash reports).
    #[must_use]
    pub fn redactor(&self) -> &Arc<Redactor> {
        &self.redactor
    }

    /// Register a live secret so the sink scrubs it even if it matches no
    /// known shape. See [`Redactor::register_secret`].
    pub fn register_secret(&self, secret: &str) -> Result<(), RegisterSecretError> {
        self.redactor.register_secret(secret)
    }

    /// Path of the active log file.
    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.sink.active_path()
    }

    #[must_use]
    pub fn wire_payload_logging(&self) -> WirePayloadLogging {
        self.wire
    }

    /// Log a raw wire payload (BLE/GATT frame, HTTP body).
    ///
    /// With the default configuration this is a no-op and `describe` is **not
    /// called**, so the payload is never even formatted into a string: no
    /// allocation, no audio bytes, no credentials (spec §7.7, App. B.7).
    pub fn wire_payload<F>(&self, subsystem: &str, describe: F)
    where
        F: FnOnce() -> String,
    {
        match self.wire {
            WirePayloadLogging::Disabled => {}
            WirePayloadLogging::Enabled => {
                let rendered = describe();
                let redacted = self.redactor.redact(&rendered);
                tracing::debug!(subsystem = %subsystem, payload = %redacted, "wire payload");
            }
        }
    }

    /// Write one already-known-to-be-diagnostic line straight to the log file,
    /// redacted. Used by the crash hook, which must not depend on a global
    /// subscriber still being functional while the process is unwinding.
    pub fn write_diagnostic_line(&self, line: &str) -> io::Result<()> {
        let redacted = self.redactor.redact(line);
        self.sink.write_line(&redacted)
    }

    /// Flush the active log file.
    pub fn flush(&self) -> io::Result<()> {
        self.sink.flush()
    }
}

/// Build the subscriber and its handle without installing it globally.
///
/// Tests use this with [`tracing::subscriber::with_default`] so they stay
/// hermetic and can run in parallel.
pub fn build_subscriber(
    config: LoggingConfig,
) -> Result<(impl Subscriber + Send + Sync + 'static, LoggingHandle), LoggingError> {
    let LoggingConfig {
        directory,
        max_file_bytes,
        max_files,
        format,
        filter,
        wire_payload_logging,
    } = config;
    if max_files == 0 {
        return Err(LoggingError::Config("max_files must be at least 1"));
    }
    if max_file_bytes == 0 {
        return Err(LoggingError::Config(
            "max_file_bytes must be greater than 0",
        ));
    }
    let env_filter = EnvFilter::try_new(&filter)
        .map_err(|err| LoggingError::Filter(format!("{filter}: {err}")))?;

    let sink = Arc::new(RollingSink::open(&directory, max_file_bytes, max_files)?);
    let redactor = Arc::new(Redactor::new());
    let make_writer = RedactingMakeWriter {
        sink: Arc::clone(&sink),
        redactor: Arc::clone(&redactor),
    };

    let layer: Box<dyn Layer<Registry> + Send + Sync + 'static> = match format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(make_writer)
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .boxed(),
        LogFormat::Human => tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(make_writer)
            .with_target(true)
            .boxed(),
    };

    let subscriber = Registry::default().with(layer).with(env_filter);
    let handle = LoggingHandle {
        redactor,
        sink,
        wire: wire_payload_logging,
    };
    Ok((subscriber, handle))
}

/// Build and globally install the redacting logging stack.
pub fn init_logging(config: LoggingConfig) -> Result<LoggingHandle, LoggingError> {
    let (subscriber, handle) = build_subscriber(config)?;
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| LoggingError::AlreadyInitialized)?;
    Ok(handle)
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// Bounded rolling file sink: `plaudon.log`, `plaudon.1.log`, …
#[derive(Debug)]
struct RollingSink {
    directory: PathBuf,
    max_file_bytes: u64,
    max_files: usize,
    state: Mutex<SinkState>,
}

#[derive(Debug)]
struct SinkState {
    file: File,
    written: u64,
}

impl RollingSink {
    fn open(directory: &Path, max_file_bytes: u64, max_files: usize) -> Result<Self, LoggingError> {
        fs::create_dir_all(directory).map_err(|source| LoggingError::Directory {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = directory.join(LOG_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| LoggingError::OpenFile {
                path: path.clone(),
                source,
            })?;
        let written = file.metadata().map_or(0, |metadata| metadata.len());
        Ok(Self {
            directory: directory.to_path_buf(),
            max_file_bytes,
            max_files,
            state: Mutex::new(SinkState { file, written }),
        })
    }

    fn active_path(&self) -> PathBuf {
        self.directory.join(LOG_FILE_NAME)
    }

    fn rotated_path(&self, index: usize) -> PathBuf {
        self.directory.join(format!("plaudon.{index}.log"))
    }

    fn write_line(&self, line: &str) -> io::Result<()> {
        let mut state = guard(&self.state);
        let mut bytes = line.as_bytes().to_vec();
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let len = bytes.len() as u64;
        if state.written > 0 && state.written + len > self.max_file_bytes {
            self.rotate(&mut state)?;
        }
        state.file.write_all(&bytes)?;
        state.written += len;
        Ok(())
    }

    /// Shift `plaudon.N.log` upwards and start a fresh active file. The oldest
    /// file is deleted, which is what bounds total log size to
    /// `max_file_bytes * max_files`.
    fn rotate(&self, state: &mut SinkState) -> io::Result<()> {
        state.file.flush()?;
        let keep_rotations = self.max_files.saturating_sub(1);
        if keep_rotations == 0 {
            // Only the active file is kept: truncate in place.
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(self.active_path())?;
            state.file = file;
            state.written = 0;
            return Ok(());
        }
        let oldest = self.rotated_path(keep_rotations);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for index in (1..keep_rotations).rev() {
            let from = self.rotated_path(index);
            if from.exists() {
                fs::rename(&from, self.rotated_path(index + 1))?;
            }
        }
        let active = self.active_path();
        if active.exists() {
            fs::rename(&active, self.rotated_path(1))?;
        }
        state.file = OpenOptions::new().create(true).append(true).open(&active)?;
        state.written = 0;
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        guard(&self.state).file.flush()
    }
}

fn guard(lock: &Mutex<SinkState>) -> MutexGuard<'_, SinkState> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Redacting writer
// ---------------------------------------------------------------------------

/// `MakeWriter` that hands out [`RedactingWriter`]s.
struct RedactingMakeWriter {
    sink: Arc<RollingSink>,
    redactor: Arc<Redactor>,
}

impl fmt::Debug for RedactingMakeWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RedactingMakeWriter")
    }
}

impl<'a> MakeWriter<'a> for RedactingMakeWriter {
    type Writer = RedactingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            buffer: Vec::with_capacity(512),
            sink: Arc::clone(&self.sink),
            redactor: Arc::clone(&self.redactor),
        }
    }
}

/// Buffers one formatted record, redacts it, then writes it to the file.
///
/// The buffering is what makes redaction unbypassable: a secret split across
/// two `write` calls by the formatter is still a single string by the time the
/// redactor sees it.
struct RedactingWriter {
    buffer: Vec<u8>,
    sink: Arc<RollingSink>,
    redactor: Arc<Redactor>,
}

impl fmt::Debug for RedactingWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedactingWriter")
            .field("buffered", &self.buffer.len())
            .field("sink", &self.sink)
            .finish_non_exhaustive()
    }
}

impl RedactingWriter {
    fn emit(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buffer);
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let redacted = self.redactor.redact(line);
            self.sink.write_line(&redacted)?;
        }
        self.buffer.clear();
        Ok(())
    }
}

impl Write for RedactingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit()?;
        self.sink.flush()
    }
}

impl Drop for RedactingWriter {
    fn drop(&mut self) {
        let _ = self.emit();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn wire_payload_logging_defaults_to_disabled() {
        assert_eq!(WirePayloadLogging::default(), WirePayloadLogging::Disabled);
        assert!(!WirePayloadLogging::default().is_enabled());
        let config = LoggingConfig::new("/tmp/plaudon-logs-example");
        assert_eq!(config.wire_payload_logging, WirePayloadLogging::Disabled);
    }

    #[test]
    fn rejects_zero_rotation_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let config = LoggingConfig::new(dir.path()).with_rotation(1024, 0);
        assert!(matches!(
            build_subscriber(config),
            Err(LoggingError::Config(_))
        ));
        let config = LoggingConfig::new(dir.path()).with_rotation(0, 2);
        assert!(matches!(
            build_subscriber(config),
            Err(LoggingError::Config(_))
        ));
    }

    #[test]
    fn rejects_invalid_filter() {
        let dir = tempfile::tempdir().unwrap();
        let config = LoggingConfig::new(dir.path()).with_filter("not a filter=!!");
        assert!(matches!(
            build_subscriber(config),
            Err(LoggingError::Filter(_))
        ));
    }
}
