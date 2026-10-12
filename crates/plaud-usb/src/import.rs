//! Read-only import of one recording from a mounted Note volume into caller
//! supplied staging (spec 4.2B, 7.8, 18.6A USB media cluster, App. B.4).
//!
//! # Guarantees
//!
//! * The device file is opened for reading only; nothing here creates,
//!   renames, removes or re-permissions anything on the volume. The
//!   `no_write_scan` integration test enforces that over this crate's source.
//! * A file qualifies by **content**: RIFF/WAVE for `.WAV`, a fully valid
//!   length-prefixed Opus packet stream for the `.ASR` sidecar. A misleading
//!   extension is reported as [`ImportError::SignatureMismatch`] and is never
//!   imported under the wrong label.
//! * Bytes stream into a [`StagingSink`] while being hashed and counted. The
//!   sink is told to [`StagingSink::commit`] only after the copy matched the
//!   directory entry's declared length *and* the file still exists afterwards;
//!   any failure calls [`StagingSink::abort`] so nothing is half-committed.
//! * The source bytes (including any vendor serial) are copied unchanged. A
//!   sanitized derivative is produced only when a derivative sink is supplied,
//!   is tagged [`QualityClass::LocalDerivative`], and is a RIFF chunk-level
//!   rewrite keeping only `fmt ` and `data`.

use std::io::{self, Read, Write};
use std::path::Path;

use plaudon_audio::{sniff_container, SniffedContainer};
use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_plaud_protocol::{AsrError, AsrScanner, AsrSummary};
use sha2::{Digest, Sha256};

use crate::gate::UsbRefusal;
use crate::mount::{HostWriteBehaviour, MountedVolume};
use crate::scan::ScannedRecording;

/// Bytes read from the device per call.
const READ_CHUNK: usize = 64 * 1024;

/// Where an import writes its bytes.
///
/// The sink is staging owned by the caller (never the device). The importer
/// calls exactly one of [`commit`](Self::commit) or [`abort`](Self::abort)
/// per import, so a sink can write to a temporary file and only publish it on
/// commit.
pub trait StagingSink: Write {
    /// Publish everything written. Called only after full verification.
    fn commit(&mut self) -> io::Result<()>;
    /// Discard everything written.
    fn abort(&mut self);
}

/// Lifecycle of a [`MemoryStagingSink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkState {
    /// Accepting bytes; neither committed nor aborted.
    Open,
    /// Committed after verification.
    Committed,
    /// Aborted; the bytes were discarded.
    Aborted,
}

/// An in-memory [`StagingSink`] that exposes bytes only once committed.
#[derive(Debug)]
pub struct MemoryStagingSink {
    pending: Vec<u8>,
    committed: Vec<u8>,
    state: SinkState,
}

impl Default for MemoryStagingSink {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStagingSink {
    /// An empty, open sink.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: Vec::new(),
            committed: Vec::new(),
            state: SinkState::Open,
        }
    }

    /// Current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> SinkState {
        self.state
    }

    /// The committed bytes. Empty unless the sink was committed.
    #[must_use]
    pub fn committed_bytes(&self) -> &[u8] {
        &self.committed
    }
}

impl Write for MemoryStagingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StagingSink for MemoryStagingSink {
    fn commit(&mut self) -> io::Result<()> {
        self.committed = std::mem::take(&mut self.pending);
        self.state = SinkState::Committed;
        Ok(())
    }

    fn abort(&mut self) {
        self.pending.clear();
        self.committed.clear();
        self.state = SinkState::Aborted;
    }
}

/// Why an import failed. Each variant is a distinct, typed condition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// The model is not offered USB import. Carries the vendor-documented reason.
    #[error("{0}")]
    ModelRefused(UsbRefusal),
    /// The file extension is neither `.WAV` nor `.ASR`.
    #[error("only .WAV and .ASR device files can be imported")]
    UnsupportedExtension,
    /// The file has no bytes.
    #[error("the device file is empty")]
    EmptyFile,
    /// The content does not match the signature its extension promises.
    #[error(
        "the file content does not match its {claimed} extension (content looks like {detected})"
    )]
    SignatureMismatch {
        /// Format the extension claims (`wav` or `asr`).
        claimed: &'static str,
        /// What sniffing found (`unknown` when nothing matched).
        detected: &'static str,
    },
    /// The `.ASR` sidecar failed Opus framing validation.
    #[error("the .ASR sidecar is not a valid Opus packet stream: {0}")]
    InvalidAsr(AsrError),
    /// The device volume disappeared (ejected or unplugged) mid-operation.
    #[error("the device volume is no longer available")]
    VolumeDisappeared,
    /// Read permission on the device file was lost or denied.
    #[error("permission to read the device file was lost")]
    PermissionLost,
    /// The device file could not be read.
    #[error("the device file could not be read")]
    Unreadable,
    /// The file ended before the length its directory entry declared.
    #[error("the device file ended before its declared length")]
    ShortRead {
        /// Length declared by the directory entry.
        declared: u64,
        /// Bytes actually read.
        actual: u64,
    },
    /// The file is longer than its directory entry declared.
    #[error("the device file grew beyond its declared length")]
    GrewDuringCopy {
        /// Length declared by the directory entry.
        declared: u64,
    },
    /// The entry resolves outside the volume (symlink or path escape).
    #[error("the entry resolves outside the device volume")]
    EscapesVolume,
    /// Writing to caller staging failed.
    #[error("writing to staging failed: {0:?}")]
    Staging(io::ErrorKind),
    /// A sanitized derivative was requested for something that is not a WAV.
    #[error("a sanitized derivative is only available for .WAV recordings")]
    DerivativeUnavailable,
    /// The WAV cannot be rewritten at the chunk level.
    #[error("the WAV structure cannot be sanitized: {0}")]
    InvalidWav(&'static str),
    /// The source changed between the copy and the derivative pass.
    #[error("the device file changed while the derivative was being produced")]
    SourceChangedDuringDerivative,
}

/// A sanitized export derivative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedDerivative {
    /// Identity of the derivative bytes (never the source's).
    pub media: MediaIdentity,
    /// Always [`QualityClass::LocalDerivative`].
    pub quality_class: QualityClass,
    /// Digest of the source bytes this was derived from.
    pub derived_from: Sha256Digest,
    /// Four-character ids of the RIFF chunks that were dropped.
    pub dropped_chunks: Vec<String>,
}

/// The outcome of a successful import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedRecording {
    /// File name on the device. Metadata only.
    pub file_name: String,
    /// Device directory (`NOTES` or `CALLS`).
    pub directory: String,
    /// Identity of the staged source bytes, identical to the device file.
    pub media: MediaIdentity,
    /// `OriginalDeviceRecording` for `.WAV`, `CompressedDeviceTransfer` for `.ASR`.
    pub quality_class: QualityClass,
    /// Container the content was sniffed as (`wav`, or `asr` for a valid sidecar).
    pub detected_container: &'static str,
    /// Measurement of the `.ASR` stream; `None` for a WAV.
    pub asr_summary: Option<AsrSummary>,
    /// How the host mounted the volume; surfaced so callers cannot claim zero writes.
    pub host_write_behaviour: HostWriteBehaviour,
    /// The sanitized derivative, only when one was requested.
    pub derivative: Option<ImportedDerivative>,
}

impl ImportedRecording {
    /// The exact user-facing sentence about host writes to the device volume.
    #[must_use]
    pub const fn host_write_notice(&self) -> &'static str {
        self.host_write_behaviour.disclosure()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Wav,
    Asr,
}

impl Kind {
    const fn label(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Asr => "asr",
        }
    }
    const fn quality(self) -> QualityClass {
        match self {
            Self::Wav => QualityClass::OriginalDeviceRecording,
            Self::Asr => QualityClass::CompressedDeviceTransfer,
        }
    }
    const fn codec_hint(self) -> &'static str {
        match self {
            Self::Wav => "pcm_s16le",
            Self::Asr => "opus",
        }
    }
}

/// Import one scanned recording into `staging`, optionally also producing a
/// sanitized derivative into `derivative`.
///
/// Both sinks are committed only after every check passed; on any error both
/// are aborted.
///
/// # Errors
///
/// See [`ImportError`]; each failure mode is a distinct variant.
pub fn import_recording(
    volume: &MountedVolume,
    recording: &ScannedRecording,
    staging: &mut dyn StagingSink,
    derivative: Option<&mut dyn StagingSink>,
) -> Result<ImportedRecording, ImportError> {
    let result = run_import(volume, recording, staging, derivative.as_deref().is_some());
    match result {
        Ok((imported, plan)) => {
            let Some(sink) = derivative else {
                return finish(staging, None, imported);
            };
            let outcome = produce_derivative(volume, recording, &imported, plan, sink);
            match outcome {
                Ok(derived) => finish(
                    staging,
                    Some(sink),
                    ImportedRecording {
                        derivative: Some(derived),
                        ..imported
                    },
                ),
                Err(error) => {
                    staging.abort();
                    sink.abort();
                    Err(error)
                }
            }
        }
        Err(error) => {
            staging.abort();
            if let Some(sink) = derivative {
                sink.abort();
            }
            Err(error)
        }
    }
}

fn finish(
    staging: &mut dyn StagingSink,
    derivative: Option<&mut dyn StagingSink>,
    imported: ImportedRecording,
) -> Result<ImportedRecording, ImportError> {
    if let Err(error) = staging.commit() {
        staging.abort();
        if let Some(sink) = derivative {
            sink.abort();
        }
        return Err(ImportError::Staging(error.kind()));
    }
    if let Some(sink) = derivative {
        if let Err(error) = sink.commit() {
            sink.abort();
            return Err(ImportError::Staging(error.kind()));
        }
    }
    Ok(imported)
}

fn kind_of(file_name: &str) -> Result<Kind, ImportError> {
    match Path::new(file_name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("wav") => Ok(Kind::Wav),
        Some("asr") => Ok(Kind::Asr),
        _ => Err(ImportError::UnsupportedExtension),
    }
}

fn map_io(error: &io::Error) -> ImportError {
    match error.kind() {
        io::ErrorKind::NotFound
        | io::ErrorKind::NotConnected
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionAborted => ImportError::VolumeDisappeared,
        io::ErrorKind::PermissionDenied => ImportError::PermissionLost,
        _ => match error.raw_os_error() {
            // ENXIO / ENODEV: the backing device is gone.
            Some(6 | 19) => ImportError::VolumeDisappeared,
            _ => ImportError::Unreadable,
        },
    }
}

/// Confirm the entry is a regular file inside the volume and open it read-only.
fn open_confined(
    volume: &MountedVolume,
    recording: &ScannedRecording,
) -> Result<std::fs::File, ImportError> {
    let root = std::fs::canonicalize(&volume.root).map_err(|e| map_io(&e))?;
    let metadata = std::fs::symlink_metadata(&recording.path).map_err(|e| map_io(&e))?;
    if metadata.file_type().is_symlink() {
        return Err(ImportError::EscapesVolume);
    }
    if !metadata.is_file() {
        return Err(ImportError::Unreadable);
    }
    let resolved = std::fs::canonicalize(&recording.path).map_err(|e| map_io(&e))?;
    if !resolved.starts_with(&root) {
        return Err(ImportError::EscapesVolume);
    }
    std::fs::File::open(&resolved).map_err(|e| map_io(&e))
}

/// Fill `buffer` as far as the source allows; returns bytes read (0 = EOF).
fn fill(file: &mut std::fs::File, buffer: &mut [u8]) -> Result<usize, ImportError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(map_io(&e)),
        }
    }
    Ok(filled)
}

/// After reading `actual` bytes, confirm the entry is still present with the
/// declared length. A file that vanished is a disappeared volume, not a read.
fn revalidate(
    volume: &MountedVolume,
    recording: &ScannedRecording,
    actual: u64,
) -> Result<(), ImportError> {
    let declared = recording.declared_byte_length;
    if !volume.root.is_dir() {
        return Err(ImportError::VolumeDisappeared);
    }
    let on_disk = std::fs::metadata(&recording.path)
        .map_err(|e| map_io(&e))?
        .len();
    if actual < declared {
        return Err(ImportError::ShortRead { declared, actual });
    }
    match on_disk.cmp(&declared) {
        std::cmp::Ordering::Less => Err(ImportError::ShortRead {
            declared,
            actual: on_disk,
        }),
        std::cmp::Ordering::Greater => Err(ImportError::GrewDuringCopy { declared }),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

fn write_staging(sink: &mut dyn StagingSink, bytes: &[u8]) -> Result<(), ImportError> {
    sink.write_all(bytes)
        .map_err(|e| ImportError::Staging(e.kind()))
}

fn run_import(
    volume: &MountedVolume,
    recording: &ScannedRecording,
    staging: &mut dyn StagingSink,
    derivative_requested: bool,
) -> Result<(ImportedRecording, Option<WavLayout>), ImportError> {
    if let Some(refusal) = volume.model.usb_refusal() {
        return Err(ImportError::ModelRefused(refusal));
    }
    let kind = kind_of(&recording.file_name)?;
    if derivative_requested && kind != Kind::Wav {
        return Err(ImportError::DerivativeUnavailable);
    }
    let mut file = open_confined(volume, recording)?;

    let mut buffer = vec![0u8; READ_CHUNK];
    let mut filled = fill(&mut file, &mut buffer)?;
    if filled == 0 {
        // A file that vanished reads as empty on some hosts; distinguish.
        if !volume.root.is_dir() || std::fs::metadata(&recording.path).is_err() {
            return Err(ImportError::VolumeDisappeared);
        }
        if recording.declared_byte_length > 0 {
            return Err(ImportError::ShortRead {
                declared: recording.declared_byte_length,
                actual: 0,
            });
        }
        return Err(ImportError::EmptyFile);
    }

    let sniffed = sniff_container(&buffer[..filled]);
    let is_riff_wave = sniffed == SniffedContainer::Wav && buffer.starts_with(b"RIFF");
    let mut asr = AsrScanner::new();
    match kind {
        Kind::Wav if !is_riff_wave => {
            return Err(ImportError::SignatureMismatch {
                claimed: kind.label(),
                detected: sniffed.as_str(),
            })
        }
        Kind::Asr if sniffed != SniffedContainer::Unknown => {
            return Err(ImportError::SignatureMismatch {
                claimed: kind.label(),
                detected: sniffed.as_str(),
            })
        }
        _ => {}
    }

    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut walker = WavWalker::new();
    loop {
        let chunk = &buffer[..filled];
        total += chunk.len() as u64;
        if total > recording.declared_byte_length {
            return Err(ImportError::GrewDuringCopy {
                declared: recording.declared_byte_length,
            });
        }
        match kind {
            Kind::Asr => asr.push(chunk).map_err(ImportError::InvalidAsr)?,
            Kind::Wav => walker.push(chunk),
        }
        hasher.update(chunk);
        write_staging(staging, chunk)?;
        if filled < buffer.len() {
            break;
        }
        filled = fill(&mut file, &mut buffer)?;
        if filled == 0 {
            break;
        }
    }
    revalidate(volume, recording, total)?;

    let asr_summary = match kind {
        Kind::Asr => Some(asr.finish().map_err(ImportError::InvalidAsr)?),
        Kind::Wav => None,
    };
    let digest = Sha256Digest::from_bytes(hasher.finalize().into());
    let layout = (kind == Kind::Wav).then(|| walker.finish(total));
    let imported = ImportedRecording {
        file_name: recording.file_name.clone(),
        directory: recording.directory.clone(),
        media: MediaIdentity::new(digest, total, kind.codec_hint()),
        quality_class: kind.quality(),
        detected_container: kind.label(),
        asr_summary,
        host_write_behaviour: volume.host_write_behaviour.clone(),
        derivative: None,
    };
    let plan = if derivative_requested {
        Some(layout.ok_or(ImportError::DerivativeUnavailable)??)
    } else {
        None
    };
    Ok((imported, plan))
}

fn produce_derivative(
    volume: &MountedVolume,
    recording: &ScannedRecording,
    imported: &ImportedRecording,
    plan: Option<WavLayout>,
    sink: &mut dyn StagingSink,
) -> Result<ImportedDerivative, ImportError> {
    let layout = plan.ok_or(ImportError::DerivativeUnavailable)?;
    let mut file = open_confined(volume, recording)?;
    let mut out_hasher = Sha256::new();
    let mut out_len: u64 = 0;
    let mut emit = |sink: &mut dyn StagingSink, bytes: &[u8]| -> Result<(), ImportError> {
        out_hasher.update(bytes);
        out_len += bytes.len() as u64;
        write_staging(sink, bytes)
    };

    let mut header = Vec::with_capacity(12);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&layout.output_riff_size.to_le_bytes());
    header.extend_from_slice(b"WAVE");
    emit(sink, &header)?;

    let mut in_hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut pos: u64 = 0;
    let mut next_span = 0usize;
    let mut headers_written = 0usize;
    loop {
        let n = fill(&mut file, &mut buffer)?;
        if n == 0 {
            break;
        }
        let chunk = &buffer[..n];
        in_hasher.update(chunk);
        let end = pos + n as u64;
        while let Some(span) = layout.kept.get(next_span) {
            let span_end = span.data_offset + span.len;
            if span.data_offset > end {
                break;
            }
            if headers_written == next_span {
                let mut chunk_header = [0u8; 8];
                chunk_header[..4].copy_from_slice(&span.id);
                #[allow(clippy::cast_possible_truncation)]
                chunk_header[4..].copy_from_slice(&(span.len as u32).to_le_bytes());
                emit(sink, &chunk_header)?;
                headers_written += 1;
            }
            let from = span.data_offset.max(pos);
            let to = span_end.min(end);
            if from < to {
                #[allow(clippy::cast_possible_truncation)]
                emit(sink, &chunk[(from - pos) as usize..(to - pos) as usize])?;
            }
            if span_end <= end {
                if span.len % 2 == 1 {
                    emit(sink, &[0])?;
                }
                next_span += 1;
            } else {
                break;
            }
        }
        pos = end;
    }
    let source_digest = Sha256Digest::from_bytes(in_hasher.finalize().into());
    if source_digest != imported.media.sha256 || pos != imported.media.byte_length {
        return Err(ImportError::SourceChangedDuringDerivative);
    }
    if out_len != layout.output_len() {
        return Err(ImportError::InvalidWav(
            "derivative length disagreed with its plan",
        ));
    }
    Ok(ImportedDerivative {
        media: MediaIdentity::new(
            Sha256Digest::from_bytes(out_hasher.finalize().into()),
            out_len,
            imported.media.observed_codec.clone(),
        ),
        quality_class: QualityClass::LocalDerivative,
        derived_from: imported.media.sha256,
        dropped_chunks: layout.dropped,
    })
}

/// A kept top-level RIFF chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeptChunk {
    id: [u8; 4],
    data_offset: u64,
    len: u64,
}

/// Chunk-level plan for the sanitized derivative.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WavLayout {
    kept: Vec<KeptChunk>,
    dropped: Vec<String>,
    output_riff_size: u32,
}

impl WavLayout {
    /// Total bytes of the derivative: `RIFF` header plus the RIFF payload.
    const fn output_len(&self) -> u64 {
        8 + self.output_riff_size as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkPhase {
    Riff,
    Header,
    Body,
    Done,
}

/// Streaming walker over the top-level RIFF chunks of a WAV.
struct WavWalker {
    phase: WalkPhase,
    pos: u64,
    head: Vec<u8>,
    riff_end: u64,
    remaining: u64,
    current: Option<([u8; 4], u64, u64)>,
    spans: Vec<([u8; 4], u64, u64)>,
}

impl WavWalker {
    const fn new() -> Self {
        Self {
            phase: WalkPhase::Riff,
            pos: 0,
            head: Vec::new(),
            riff_end: 0,
            remaining: 0,
            current: None,
            spans: Vec::new(),
        }
    }

    fn push(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            match self.phase {
                WalkPhase::Done => return,
                WalkPhase::Riff => {
                    let take = (12 - self.head.len()).min(bytes.len());
                    self.head.extend_from_slice(&bytes[..take]);
                    bytes = &bytes[take..];
                    self.pos += take as u64;
                    if self.head.len() == 12 {
                        let size = u32::from_le_bytes([
                            self.head[4],
                            self.head[5],
                            self.head[6],
                            self.head[7],
                        ]);
                        self.riff_end = 8 + u64::from(size);
                        self.head.clear();
                        self.phase = WalkPhase::Header;
                    }
                }
                WalkPhase::Header => {
                    if self.pos >= self.riff_end {
                        self.phase = WalkPhase::Done;
                        return;
                    }
                    let take = (8 - self.head.len()).min(bytes.len());
                    self.head.extend_from_slice(&bytes[..take]);
                    bytes = &bytes[take..];
                    self.pos += take as u64;
                    if self.head.len() == 8 {
                        let id = [self.head[0], self.head[1], self.head[2], self.head[3]];
                        let len = u64::from(u32::from_le_bytes([
                            self.head[4],
                            self.head[5],
                            self.head[6],
                            self.head[7],
                        ]));
                        self.head.clear();
                        self.current = Some((id, self.pos, len));
                        self.remaining = len + (len & 1);
                        self.phase = WalkPhase::Body;
                        if self.remaining == 0 {
                            self.close_chunk();
                        }
                    }
                }
                WalkPhase::Body => {
                    let take = self.remaining.min(bytes.len() as u64);
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        bytes = &bytes[take as usize..];
                    }
                    self.pos += take;
                    self.remaining -= take;
                    if self.remaining == 0 {
                        self.close_chunk();
                    }
                }
            }
        }
    }

    fn close_chunk(&mut self) {
        if let Some(span) = self.current.take() {
            self.spans.push(span);
        }
        self.phase = WalkPhase::Header;
    }

    /// Turn the walk into a derivative plan for a file of `total` bytes.
    fn finish(self, total: u64) -> Result<WavLayout, ImportError> {
        let mut spans = self.spans;
        // A chunk still open at end of file ran past the end of the file.
        if let Some(open) = self.current {
            if open.1 + open.2 > total {
                return Err(ImportError::InvalidWav(
                    "a chunk extends past the end of the file",
                ));
            }
            spans.push(open);
        }
        if spans.iter().any(|(_, offset, len)| offset + len > total) {
            return Err(ImportError::InvalidWav(
                "a chunk extends past the end of the file",
            ));
        }
        let fmt = spans.iter().position(|(id, _, _)| id == b"fmt ");
        let data = spans.iter().position(|(id, _, _)| id == b"data");
        let (Some(fmt), Some(data)) = (fmt, data) else {
            return Err(ImportError::InvalidWav("the WAV lacks a fmt or data chunk"));
        };
        if fmt > data {
            return Err(ImportError::InvalidWav(
                "the data chunk precedes the fmt chunk",
            ));
        }
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for (index, (id, data_offset, len)) in spans.iter().copied().enumerate() {
            if index == fmt || index == data {
                kept.push(KeptChunk {
                    id,
                    data_offset,
                    len,
                });
            } else {
                dropped.push(String::from_utf8_lossy(&id).into_owned());
            }
        }
        let payload: u64 = 4 + kept.iter().map(|c| 8 + c.len + (c.len & 1)).sum::<u64>();
        let output_riff_size = u32::try_from(payload)
            .map_err(|_| ImportError::InvalidWav("the WAV is too large to rewrite"))?;
        Ok(WavLayout {
            kept,
            dropped,
            output_riff_size,
        })
    }
}
