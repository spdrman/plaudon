//! Original-audio export and the sanitized derivative (spec 4.6, 7.8, 23).
//!
//! Two rules:
//!
//! * **The original is copied, never transformed.** Bytes stream out of the
//!   content-addressed archive and are re-hashed on the way; the exported file
//!   has the same SHA-256 as the stored asset or the export fails. The stored
//!   asset is opened read-only and is never written.
//! * **A sanitized derivative is a separate, explicitly requested artifact.**
//!   It is tagged as a derivative, recorded as its own asset row, and never
//!   replaces the original. Spec 7.8 exists because an original PLAUD Note WAV
//!   carries the device serial in RIFF metadata: we keep that original intact
//!   and offer a stripped copy only on request.
//!
//! Nothing here renames a `.ASR`-decoded derivative into "the original device
//! WAV": the label comes from the stored `quality_class` (see
//! [`crate::bundle::quality_label`]).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::destination::ExportDestination;
use crate::error::{ExportError, ExportResult};

/// Bytes moved per read. Peak memory is this, whatever the file size is.
pub const COPY_CHUNK: usize = 64 * 1024;

/// Largest metadata chunk we will buffer while rewriting a container.
const MAX_METADATA_CHUNK: u64 = 8 * 1024 * 1024;

/// Outcome of exporting media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaExportReport {
    /// Where the file was written.
    pub path: PathBuf,
    /// Digest of the bytes that were written.
    pub sha256: String,
    /// Number of bytes written.
    pub byte_length: u64,
    /// Whether this is the immutable original or a produced derivative.
    pub is_original: bool,
    /// Honest, source-relative description of what the file is.
    pub label: String,
    /// Metadata chunks removed, for a sanitized derivative.
    pub stripped_metadata: Vec<String>,
}

/// Copy an archived blob to a user-chosen folder, verifying it end to end.
///
/// The source is read in [`COPY_CHUNK`] slices, so a multi-gigabyte recording
/// never lands in memory.
pub fn export_original(
    source: &Path,
    expected: &MediaIdentity,
    destination: &ExportDestination,
    file_name: &str,
    label: &str,
) -> ExportResult<MediaExportReport> {
    let mut reader = BufReader::with_capacity(COPY_CHUNK, File::open(source)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; COPY_CHUNK];

    let (path, written) = destination.write_atomic_with(file_name, |file| {
        let mut total = 0u64;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])?;
            total += read as u64;
        }
        Ok(total)
    })?;

    let digest = Sha256Digest::from_bytes(hasher.finalize().into());
    if digest != expected.sha256 || written != expected.byte_length {
        // The export does not match the asset it claims to be. Remove it
        // rather than leave a file the user would trust.
        let _ = std::fs::remove_file(&path);
        return Err(ExportError::IntegrityFailed);
    }

    Ok(MediaExportReport {
        path,
        sha256: digest.to_hex(),
        byte_length: written,
        is_original: true,
        label: label.to_owned(),
        stripped_metadata: Vec::new(),
    })
}

/// Containers we can sanitize without re-encoding audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanitizableContainer {
    /// RIFF/WAVE: keep `fmt `, `data` and `fact`; drop every other chunk.
    Wav,
    /// MPEG audio: drop a leading ID3v2 tag and a trailing ID3v1 tag.
    Mp3,
}

impl SanitizableContainer {
    /// Detect the container from its magic bytes.
    pub fn detect(path: &Path) -> ExportResult<Self> {
        let mut file = File::open(path)?;
        let mut head = [0u8; 12];
        let read = read_up_to(&mut file, &mut head)?;
        if read >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
            return Ok(Self::Wav);
        }
        if read >= 3 && (&head[0..3] == b"ID3" || (head[0] == 0xFF && head[1] & 0xE0 == 0xE0)) {
            return Ok(Self::Mp3);
        }
        Err(ExportError::SanitizationUnsupported(
            "only WAV and MP3 can be stripped without re-encoding the audio".into(),
        ))
    }
}

/// Chunks a WAV sanitization keeps. Everything else carries provenance we are
/// asked to remove (serials, device identifiers, timestamps, free text).
pub const WAV_KEPT_CHUNKS: &[&[u8; 4]] = &[b"fmt ", b"data", b"fact"];

/// Produce a sanitized derivative of an archived blob.
///
/// Returns the removed chunk identifiers so the provenance record can say
/// exactly what was stripped. The source file is opened read-only and is
/// byte-identical afterwards.
pub fn export_sanitized(
    source: &Path,
    destination: &ExportDestination,
    file_name: &str,
) -> ExportResult<MediaExportReport> {
    let container = SanitizableContainer::detect(source)?;
    let mut stripped = Vec::new();
    let mut hasher = Sha256::new();

    let (path, written) = destination.write_atomic_with(file_name, |out| match container {
        SanitizableContainer::Wav => sanitize_wav(source, out, &mut hasher, &mut stripped),
        SanitizableContainer::Mp3 => sanitize_mp3(source, out, &mut hasher, &mut stripped),
    })?;

    Ok(MediaExportReport {
        path,
        sha256: Sha256Digest::from_bytes(hasher.finalize().into()).to_hex(),
        byte_length: written,
        is_original: false,
        label: "Sanitized derivative (metadata removed; not the original file)".to_owned(),
        stripped_metadata: stripped,
    })
}

fn read_up_to(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = reader.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: &'a mut Sha256,
    written: u64,
}

impl<W: Write> HashingWriter<'_, W> {
    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.hasher.update(bytes);
        self.inner.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}

/// Rewrite a RIFF/WAVE file keeping only the structural chunks.
fn sanitize_wav(
    source: &Path,
    out: &mut impl Write,
    hasher: &mut Sha256,
    stripped: &mut Vec<String>,
) -> ExportResult<u64> {
    let mut file = BufReader::with_capacity(COPY_CHUNK, File::open(source)?);
    let mut header = [0u8; 12];
    if read_up_to(&mut file, &mut header)? != 12 {
        return Err(ExportError::SanitizationUnsupported(
            "the file is too short to be a WAV".into(),
        ));
    }

    // Collect the chunks we keep, in order, before writing: the RIFF size
    // field at the front depends on the total length.
    let mut kept: Vec<([u8; 4], u64, u64)> = Vec::new();
    let mut offset: u64 = 12;
    loop {
        let mut chunk_header = [0u8; 8];
        let read = read_up_to(&mut file, &mut chunk_header)?;
        if read == 0 {
            break;
        }
        if read < 8 {
            return Err(ExportError::SanitizationUnsupported(
                "the file ends inside a chunk header".into(),
            ));
        }
        let id = [
            chunk_header[0],
            chunk_header[1],
            chunk_header[2],
            chunk_header[3],
        ];
        let size = u64::from(u32::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ]));
        let padded = size + (size % 2);
        let body = offset + 8;
        if WAV_KEPT_CHUNKS.contains(&&id) {
            kept.push((id, body, size));
        } else {
            stripped.push(String::from_utf8_lossy(&id).trim_end().to_owned());
        }
        offset = body + padded;
        file.seek(SeekFrom::Start(offset))?;
    }

    if !kept.iter().any(|(id, _, _)| id == b"data") {
        return Err(ExportError::SanitizationUnsupported(
            "the file has no audio data chunk".into(),
        ));
    }

    let payload: u64 = kept
        .iter()
        .map(|(_, _, size)| 8 + size + (size % 2))
        .sum::<u64>()
        + 4; // the "WAVE" form type
    let riff_size = u32::try_from(payload).map_err(|_| {
        ExportError::SanitizationUnsupported("the audio data is too large to rewrite".into())
    })?;

    let mut writer = HashingWriter {
        inner: out,
        hasher,
        written: 0,
    };
    writer.put(b"RIFF")?;
    writer.put(&riff_size.to_le_bytes())?;
    writer.put(b"WAVE")?;

    let mut buffer = vec![0u8; COPY_CHUNK];
    for (id, body, size) in kept {
        writer.put(&id)?;
        writer.put(&u32::try_from(size).unwrap_or(u32::MAX).to_le_bytes())?;
        file.seek(SeekFrom::Start(body))?;
        let mut remaining = size;
        while remaining > 0 {
            let take = usize::try_from(remaining.min(COPY_CHUNK as u64)).unwrap_or(COPY_CHUNK);
            let read = file.read(&mut buffer[..take])?;
            if read == 0 {
                return Err(ExportError::SanitizationUnsupported(
                    "the file ends inside a chunk body".into(),
                ));
            }
            writer.put(&buffer[..read])?;
            remaining -= read as u64;
        }
        if size % 2 == 1 {
            writer.put(&[0u8])?;
        }
    }
    Ok(writer.written)
}

/// Copy an MPEG audio file without its ID3 tags.
fn sanitize_mp3(
    source: &Path,
    out: &mut impl Write,
    hasher: &mut Sha256,
    stripped: &mut Vec<String>,
) -> ExportResult<u64> {
    let mut file = File::open(source)?;
    let total = file.metadata()?.len();

    let mut start = 0u64;
    let mut head = [0u8; 10];
    if read_up_to(&mut file, &mut head)? == 10 && &head[0..3] == b"ID3" {
        // Synchsafe 28-bit size, excluding the 10-byte header.
        let size = (u64::from(head[6] & 0x7f) << 21)
            | (u64::from(head[7] & 0x7f) << 14)
            | (u64::from(head[8] & 0x7f) << 7)
            | u64::from(head[9] & 0x7f);
        let footer = u64::from(head[5] & 0x10 != 0) * 10;
        start = 10 + size + footer;
        if start > total || size > MAX_METADATA_CHUNK {
            return Err(ExportError::SanitizationUnsupported(
                "the ID3 tag length is not consistent with the file".into(),
            ));
        }
        stripped.push("ID3v2".to_owned());
    }

    let mut end = total;
    if total >= start + 128 {
        file.seek(SeekFrom::Start(total - 128))?;
        let mut tail = [0u8; 3];
        if read_up_to(&mut file, &mut tail)? == 3 && &tail == b"TAG" {
            end = total - 128;
            stripped.push("ID3v1".to_owned());
        }
    }

    file.seek(SeekFrom::Start(start))?;
    let mut writer = HashingWriter {
        inner: out,
        hasher,
        written: 0,
    };
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut remaining = end.saturating_sub(start);
    while remaining > 0 {
        let take = usize::try_from(remaining.min(COPY_CHUNK as u64)).unwrap_or(COPY_CHUNK);
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            break;
        }
        writer.put(&buffer[..read])?;
        remaining -= read as u64;
    }
    Ok(writer.written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_with_metadata() -> Vec<u8> {
        let fmt: [u8; 16] = [
            1, 0, // PCM
            1, 0, // mono
            0x44, 0xAC, 0, 0, // 44100
            0x88, 0x58, 0x01, 0, // byte rate
            2, 0, // block align
            16, 0, // bits
        ];
        let data: Vec<u8> = (0u16..64).flat_map(u16::to_le_bytes).collect();
        let info = b"INFOISFT\x0a\x00\x00\x00SERIAL1234";

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        body.extend_from_slice(&fmt);
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(info.len() as u32).to_le_bytes());
        body.extend_from_slice(info);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&data);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn exporting_an_original_preserves_its_digest_and_source_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("blob.wav");
        let bytes = wav_with_metadata();
        std::fs::write(&source, &bytes).unwrap();
        let before = std::fs::read(&source).unwrap();

        let out = tempfile::tempdir().unwrap();
        let destination = ExportDestination::new(out.path()).unwrap();
        let identity = MediaIdentity::of_bytes(&bytes, "wav");
        let report = export_original(
            &source,
            &identity,
            &destination,
            "meeting.wav",
            "Original device recording",
        )
        .unwrap();

        assert_eq!(report.sha256, identity.sha256.to_hex());
        assert_eq!(report.byte_length, bytes.len() as u64);
        assert!(report.is_original);
        assert_eq!(std::fs::read(&report.path).unwrap(), bytes);
        assert_eq!(std::fs::read(&source).unwrap(), before, "source mutated");
    }

    #[test]
    fn an_asset_that_does_not_match_its_identity_is_not_exported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("blob.wav");
        std::fs::write(&source, b"actual bytes").unwrap();
        let out = tempfile::tempdir().unwrap();
        let destination = ExportDestination::new(out.path()).unwrap();

        let wrong = MediaIdentity::of_bytes(b"different bytes", "wav");
        let error = export_original(&source, &wrong, &destination, "a.wav", "x").unwrap_err();
        assert!(matches!(error, ExportError::IntegrityFailed));
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn sanitizing_a_wav_drops_metadata_and_keeps_the_audio() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("blob.wav");
        let bytes = wav_with_metadata();
        std::fs::write(&source, &bytes).unwrap();

        let out = tempfile::tempdir().unwrap();
        let destination = ExportDestination::new(out.path()).unwrap();
        let report = export_sanitized(&source, &destination, "meeting-sanitized.wav").unwrap();

        let sanitized = std::fs::read(&report.path).unwrap();
        assert!(!report.is_original);
        assert_eq!(report.stripped_metadata, vec!["LIST"]);
        assert!(!report.label.contains("original device"));
        assert!(
            !sanitized.windows(10).any(|w| w == b"SERIAL1234"),
            "the serial survived sanitization"
        );
        // The audio payload is byte-identical.
        let data_start = sanitized
            .windows(4)
            .position(|w| w == b"data")
            .expect("data chunk");
        let original_start = bytes.windows(4).position(|w| w == b"data").unwrap();
        assert_eq!(
            sanitized[data_start + 8..],
            bytes[original_start + 8..],
            "audio payload changed"
        );
        // The original is untouched.
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        // The declared RIFF size matches the real one.
        let declared = u32::from_le_bytes([sanitized[4], sanitized[5], sanitized[6], sanitized[7]]);
        assert_eq!(u64::from(declared) + 8, sanitized.len() as u64);
    }

    #[test]
    fn sanitizing_an_mp3_drops_id3_tags_only() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("blob.mp3");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ID3\x03\x00\x00\x00\x00\x00\x0a");
        bytes.extend_from_slice(&[0u8; 10]); // tag body
        let audio: Vec<u8> = (0u8..64).collect();
        bytes.extend_from_slice(&audio);
        bytes.extend_from_slice(b"TAG");
        bytes.extend_from_slice(&[0u8; 125]);
        std::fs::write(&source, &bytes).unwrap();

        let out = tempfile::tempdir().unwrap();
        let destination = ExportDestination::new(out.path()).unwrap();
        let report = export_sanitized(&source, &destination, "song.mp3").unwrap();
        assert_eq!(report.stripped_metadata, vec!["ID3v2", "ID3v1"]);
        assert_eq!(std::fs::read(&report.path).unwrap(), audio);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }

    #[test]
    fn an_unsupported_container_is_refused_rather_than_mangled() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("blob.m4a");
        std::fs::write(&source, b"\x00\x00\x00\x20ftypM4A ").unwrap();
        let out = tempfile::tempdir().unwrap();
        let destination = ExportDestination::new(out.path()).unwrap();
        let error = export_sanitized(&source, &destination, "x.m4a").unwrap_err();
        assert!(matches!(error, ExportError::SanitizationUnsupported(_)));
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    }
}
