//! Container framing: preamble, frames and the terminator.
//!
//! Everything in this module is deliberately dumb. It reads and writes the
//! byte layout documented in [`super::manifest`] and performs exactly the
//! structural checks needed to avoid unbounded reads; policy (paths, sizes,
//! digests, namespaces) belongs to the reader and to `plaudon-security`.

use std::io::{Read, Write};

use super::manifest::{
    ArchiveHeader, ManifestEntry, ProtectionKind, FORMAT_VERSION, MAGIC, MAX_FRAME_HEADER_BYTES,
    MAX_HEADER_BYTES,
};
use crate::error::{ArchiveRejection, ExportError, ExportResult};

/// The preamble as read from a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preamble {
    /// Whether the body is encrypted.
    pub protection: ProtectionKind,
    /// Parsed authenticated metadata.
    pub header: ArchiveHeader,
    /// Exact header bytes, used verbatim as AEAD associated data.
    pub header_bytes: Vec<u8>,
}

fn malformed(detail: &str) -> ExportError {
    ExportError::ArchiveRejected(ArchiveRejection::Malformed(detail.to_owned()))
}

/// Write the fixed preamble and the authenticated header JSON.
pub fn write_preamble(
    writer: &mut impl Write,
    protection: ProtectionKind,
    header: &ArchiveHeader,
) -> ExportResult<Vec<u8>> {
    let header_bytes = serde_json::to_vec(header)?;
    let length = u32::try_from(header_bytes.len()).map_err(|_| malformed("header is too large"))?;
    if length > MAX_HEADER_BYTES {
        return Err(malformed("header is too large"));
    }
    writer.write_all(MAGIC)?;
    writer.write_all(&FORMAT_VERSION.to_le_bytes())?;
    writer.write_all(&[protection.code(), 0])?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&header_bytes)?;
    Ok(header_bytes)
}

/// Read and validate the preamble.
pub fn read_preamble(reader: &mut impl Read) -> ExportResult<Preamble> {
    let mut magic = [0u8; 8];
    if read_full(reader, &mut magic)? != magic.len() || &magic != MAGIC {
        return Err(ExportError::ArchiveRejected(ArchiveRejection::NotAnArchive));
    }
    let mut fixed = [0u8; 8];
    if read_full(reader, &mut fixed)? != fixed.len() {
        return Err(malformed("the file ends inside its header"));
    }
    let version = u16::from_le_bytes([fixed[0], fixed[1]]);
    if version != FORMAT_VERSION {
        return Err(ExportError::ArchiveRejected(
            ArchiveRejection::UnsupportedFormatVersion {
                found: u32::from(version),
                supported: u32::from(FORMAT_VERSION),
            },
        ));
    }
    let protection =
        ProtectionKind::from_code(fixed[2]).ok_or_else(|| malformed("unknown protection mode"))?;
    if fixed[3] != 0 {
        return Err(malformed("reserved header byte is not zero"));
    }
    let length = u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]);
    if length == 0 || length > MAX_HEADER_BYTES {
        return Err(malformed("header length is out of range"));
    }
    let mut header_bytes = vec![0u8; length as usize];
    if read_full(reader, &mut header_bytes)? != header_bytes.len() {
        return Err(malformed("the file ends inside its header"));
    }
    let header: ArchiveHeader = serde_json::from_slice(&header_bytes)
        .map_err(|_| malformed("the header is not valid archive metadata"))?;
    if header.format_version != FORMAT_VERSION {
        return Err(ExportError::ArchiveRejected(
            ArchiveRejection::UnsupportedFormatVersion {
                found: u32::from(header.format_version),
                supported: u32::from(FORMAT_VERSION),
            },
        ));
    }
    match (protection, &header.encryption) {
        (ProtectionKind::Encrypted, None) => {
            return Err(malformed(
                "the archive claims encryption without parameters",
            ))
        }
        (ProtectionKind::Plaintext, Some(_)) => {
            return Err(malformed(
                "a plaintext archive carries encryption parameters",
            ))
        }
        _ => {}
    }
    Ok(Preamble {
        protection,
        header,
        header_bytes,
    })
}

/// Write one frame header. The caller streams `stored_length` payload bytes
/// immediately afterwards.
pub fn write_frame_header(writer: &mut impl Write, entry: &ManifestEntry) -> ExportResult<()> {
    let bytes = serde_json::to_vec(entry)?;
    let length = u32::try_from(bytes.len()).map_err(|_| malformed("frame header is too large"))?;
    if length == 0 || length > MAX_FRAME_HEADER_BYTES {
        return Err(malformed("frame header is too large"));
    }
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&bytes)?;
    Ok(())
}

/// Write the end-of-body terminator.
pub fn write_terminator(writer: &mut impl Write) -> ExportResult<()> {
    writer.write_all(&0u32.to_le_bytes())?;
    Ok(())
}

/// Read the next frame header, or `None` at the terminator.
pub fn read_frame_header(reader: &mut impl Read) -> ExportResult<Option<ManifestEntry>> {
    let mut length = [0u8; 4];
    let read = read_full(reader, &mut length)?;
    if read == 0 {
        return Err(malformed("the archive ends without a terminator"));
    }
    if read != length.len() {
        return Err(malformed("the archive ends inside a frame header"));
    }
    let length = u32::from_le_bytes(length);
    if length == 0 {
        return Ok(None);
    }
    if length > MAX_FRAME_HEADER_BYTES {
        return Err(malformed("frame header length is out of range"));
    }
    let mut bytes = vec![0u8; length as usize];
    if read_full(reader, &mut bytes)? != bytes.len() {
        return Err(malformed("the archive ends inside a frame header"));
    }
    let entry: ManifestEntry = serde_json::from_slice(&bytes)
        .map_err(|_| malformed("a frame header is not valid entry metadata"))?;
    Ok(Some(entry))
}

/// Read exactly `buffer.len()` bytes, or fewer at end of input.
pub fn read_full(reader: &mut impl Read, buffer: &mut [u8]) -> ExportResult<usize> {
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

#[cfg(test)]
mod tests {
    use super::super::crypto::{EncryptionHeader, KdfProfile};
    use super::super::manifest::{Compression, MANIFEST_SCHEMA_VERSION};
    use super::*;

    fn plaintext_header() -> ArchiveHeader {
        ArchiveHeader {
            format_version: FORMAT_VERSION,
            manifest_schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
            protection: "plaintext".to_owned(),
            encryption: None,
        }
    }

    #[test]
    fn a_preamble_round_trips() {
        let mut buffer = Vec::new();
        let header = plaintext_header();
        let written = write_preamble(&mut buffer, ProtectionKind::Plaintext, &header).unwrap();
        let read = read_preamble(&mut buffer.as_slice()).unwrap();
        assert_eq!(read.protection, ProtectionKind::Plaintext);
        assert_eq!(read.header, header);
        assert_eq!(read.header_bytes, written);
    }

    #[test]
    fn a_foreign_file_is_not_mistaken_for_an_archive() {
        let error = read_preamble(&mut b"PK\x03\x04 and a zip".as_slice()).unwrap_err();
        assert!(matches!(
            error,
            ExportError::ArchiveRejected(ArchiveRejection::NotAnArchive)
        ));
    }

    #[test]
    fn a_future_format_version_is_refused_rather_than_guessed() {
        let mut buffer = Vec::new();
        write_preamble(&mut buffer, ProtectionKind::Plaintext, &plaintext_header()).unwrap();
        buffer[8] = 99;
        let error = read_preamble(&mut buffer.as_slice()).unwrap_err();
        assert!(matches!(
            error,
            ExportError::ArchiveRejected(ArchiveRejection::UnsupportedFormatVersion {
                found: 99,
                ..
            })
        ));
    }

    #[test]
    fn protection_and_parameters_must_agree() {
        let mut header = plaintext_header();
        header.encryption = Some(EncryptionHeader::generate(KdfProfile::Test));
        let mut buffer = Vec::new();
        write_preamble(&mut buffer, ProtectionKind::Plaintext, &header).unwrap();
        assert!(read_preamble(&mut buffer.as_slice()).is_err());

        let mut buffer = Vec::new();
        write_preamble(&mut buffer, ProtectionKind::Encrypted, &plaintext_header()).unwrap();
        assert!(read_preamble(&mut buffer.as_slice()).is_err());
    }

    #[test]
    fn frames_round_trip_and_the_terminator_ends_the_body() {
        let entry = ManifestEntry {
            path: "records/a.json".into(),
            kind: "file".into(),
            sha256: "a".repeat(64),
            byte_length: 3,
            stored_length: 3,
            compression: Compression::Store,
            schema_version: Some("recording-export/1".into()),
        };
        let mut buffer = Vec::new();
        write_frame_header(&mut buffer, &entry).unwrap();
        buffer.extend_from_slice(b"abc");
        write_terminator(&mut buffer).unwrap();

        let mut cursor = buffer.as_slice();
        let read = read_frame_header(&mut cursor).unwrap().unwrap();
        assert_eq!(read, entry);
        let mut payload = [0u8; 3];
        read_full(&mut cursor, &mut payload).unwrap();
        assert_eq!(&payload, b"abc");
        assert!(read_frame_header(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn a_frame_header_with_an_unknown_field_is_refused() {
        let json = br#"{"path":"a","kind":"file","sha256":"x","byte_length":1,"stored_length":1,"compression":"store","link_target":"/etc/passwd"}"#;
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
        buffer.extend_from_slice(json);
        let error = read_frame_header(&mut buffer.as_slice()).unwrap_err();
        assert!(matches!(
            error,
            ExportError::ArchiveRejected(ArchiveRejection::Malformed(_))
        ));
    }

    #[test]
    fn a_body_without_a_terminator_is_malformed() {
        let mut buffer = Vec::new();
        write_frame_header(
            &mut buffer,
            &ManifestEntry {
                path: "records/a.json".into(),
                kind: "file".into(),
                sha256: "a".repeat(64),
                byte_length: 0,
                stored_length: 0,
                compression: Compression::Store,
                schema_version: None,
            },
        )
        .unwrap();
        let mut cursor = buffer.as_slice();
        read_frame_header(&mut cursor).unwrap().unwrap();
        assert!(read_frame_header(&mut cursor).is_err());
    }
}
