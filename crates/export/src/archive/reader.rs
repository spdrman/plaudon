//! Reading, inspecting and verifying a `.recording-archive` (spec 13.6, 13.8, 16.5).
//!
//! The order is fixed and is the whole safety argument:
//!
//! 1. read the preamble and refuse an unknown magic, format or protection;
//! 2. decrypt and read the **manifest only**;
//! 3. validate every declared entry against `plaudon-security`'s archive
//!    policy — traversal, links, duplicates (case-folded), size ceilings,
//!    compression ratio, namespaces — with **no filesystem access at all**;
//! 4. only then stream payloads, verifying each digest against the manifest
//!    before the entry is handed to a sink.
//!
//! A dry run stops after step 3 plus a verifying pass with a discarding sink,
//! so a hostile archive is refused before a single byte of user data moves.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;
use plaudon_domain::media::Sha256Digest;
use plaudon_security::fsguard::{validate_archive_entry, ArchiveEntryMeta, ArchivePolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::crypto::{DecryptingReader, AEAD_FAILURE, FRAMING_FAILURE};
use super::manifest::{
    in_known_namespace, Compression, Manifest, ManifestEntry, ProtectionKind, MANIFEST_PATH,
    MAX_MANIFEST_BYTES,
};
use super::{container, DEFAULT_LIMITS};
use crate::error::{ArchiveRejection, ExportError, ExportResult};

/// Bytes moved per read while streaming payloads.
pub const READ_CHUNK: usize = 64 * 1024;

/// Turn a low-level body failure into the typed failure the UI shows.
///
/// A wrong password, a tampered chunk and a truncated body all arrive here as
/// io errors from the decrypting reader; none of them is a storage problem.
fn classify(error: ExportError) -> ExportError {
    let ExportError::Io(io) = &error else {
        return error;
    };
    if io.kind() != std::io::ErrorKind::InvalidData {
        return error;
    }
    let message = io.to_string();
    if message.contains(AEAD_FAILURE) {
        return ExportError::DecryptionFailed;
    }
    if message.contains(FRAMING_FAILURE) {
        return ExportError::ArchiveRejected(ArchiveRejection::Malformed(
            "the archive body is truncated or malformed".to_owned(),
        ));
    }
    error
}

/// A sink that receives verified entry payloads.
pub trait EntrySink {
    /// Called before the payload of `entry` streams.
    fn begin(&mut self, entry: &ManifestEntry) -> ExportResult<()>;
    /// Called for each plaintext chunk, in order.
    fn chunk(&mut self, bytes: &[u8]) -> ExportResult<()>;
    /// Called once the digest has been verified.
    fn commit(&mut self, entry: &ManifestEntry) -> ExportResult<()>;
}

/// A sink that verifies and discards. Used by dry runs.
#[derive(Debug, Default)]
pub struct VerifyingSink {
    /// Entries seen, in order.
    pub seen: Vec<String>,
}

impl EntrySink for VerifyingSink {
    fn begin(&mut self, _entry: &ManifestEntry) -> ExportResult<()> {
        Ok(())
    }

    fn chunk(&mut self, _bytes: &[u8]) -> ExportResult<()> {
        Ok(())
    }

    fn commit(&mut self, entry: &ManifestEntry) -> ExportResult<()> {
        self.seen.push(entry.path.clone());
        Ok(())
    }
}

/// Outcome of inspecting an archive without writing anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectionReport {
    /// Archive file that was inspected.
    pub path: PathBuf,
    /// Whether the body is encrypted.
    pub encrypted: bool,
    /// The manifest, once it verified against itself.
    pub manifest: Manifest,
    /// Entry count by namespace, for the UI summary.
    pub entries_by_namespace: Vec<(String, u64)>,
    /// Whether every payload digest matched the manifest.
    pub payloads_verified: bool,
    /// Settings the manifest says do not travel.
    pub nonportable_settings: Vec<String>,
}

impl InspectionReport {
    /// Total declared payload bytes.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.manifest.totals.byte_length
    }
}

/// Validate every declared entry before anything is read or written.
///
/// `destination_root` is the directory entries would be written into; it does
/// not need to exist, and nothing under it is touched.
pub fn validate_manifest_entries(manifest: &Manifest, destination_root: &Path) -> ExportResult<()> {
    manifest.verify_self_consistency()?;

    let mut policy = ArchivePolicy::new(destination_root)
        .with_max_entry_bytes(DEFAULT_LIMITS.max_entry_bytes)
        .with_max_total_bytes(DEFAULT_LIMITS.max_total_bytes)
        .with_max_entries(usize::try_from(DEFAULT_LIMITS.max_entries).unwrap_or(usize::MAX))
        .with_windows_short_path_compatible(true);

    for entry in &manifest.entries {
        if !entry.is_file() {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::UnsupportedEntryKind {
                    path: entry.path.clone(),
                    kind: entry.kind.clone(),
                },
            ));
        }
        if !in_known_namespace(&entry.path) {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::PathRejected {
                    path: entry.path.clone(),
                    reason: "outside the archive namespaces".to_owned(),
                },
            ));
        }
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ExportError::ArchiveRejected(ArchiveRejection::Malformed(
                format!("entry {} has no usable digest", entry.path),
            )));
        }
        if entry.compression == Compression::Store && entry.stored_length != entry.byte_length {
            return Err(ExportError::ArchiveRejected(ArchiveRejection::Malformed(
                format!("stored entry {} declares two different lengths", entry.path),
            )));
        }
        let meta = ArchiveEntryMeta::file(
            entry.path.clone(),
            entry.byte_length,
            entry.stored_length.max(1),
        );
        validate_archive_entry(&meta, &mut policy).map_err(|rejection| {
            let reason = rejection.reason();
            ExportError::ArchiveRejected(match reason {
                plaudon_security::fsguard::ArchiveRejection::DuplicatePath => {
                    ArchiveRejection::DuplicatePath(rejection.label().to_owned())
                }
                plaudon_security::fsguard::ArchiveRejection::EntryTooLarge
                | plaudon_security::fsguard::ArchiveRejection::ArchiveTooLarge
                | plaudon_security::fsguard::ArchiveRejection::TooManyEntries
                | plaudon_security::fsguard::ArchiveRejection::CompressionRatioExceeded => {
                    ArchiveRejection::SizeCeilingExceeded(format!(
                        "{} ({})",
                        reason.rule(),
                        rejection.label()
                    ))
                }
                plaudon_security::fsguard::ArchiveRejection::LinkEntry
                | plaudon_security::fsguard::ArchiveRejection::SpecialEntry => {
                    ArchiveRejection::UnsupportedEntryKind {
                        path: rejection.label().to_owned(),
                        kind: rejection.kind().label().to_owned(),
                    }
                }
                other => ArchiveRejection::PathRejected {
                    path: rejection.label().to_owned(),
                    reason: other.rule().to_owned(),
                },
            })
        })?;

        // Media entries are content-addressed: the path must agree with the
        // digest, so a restored blob cannot land under a name that claims
        // different bytes.
        if let Some(name) = entry.path.strip_prefix("media/") {
            let stem = name.split('.').next().unwrap_or_default();
            if !stem.eq_ignore_ascii_case(&entry.sha256) {
                return Err(ExportError::ArchiveRejected(
                    ArchiveRejection::PathRejected {
                        path: entry.path.clone(),
                        reason: "media file name does not match its digest".to_owned(),
                    },
                ));
            }
        }
    }
    Ok(())
}

/// Open an archive and read its manifest without reading any payload.
pub fn read_manifest(path: &Path, password: Option<&str>) -> ExportResult<(Manifest, bool)> {
    let mut body = open_body(path, password)?;
    let manifest = read_manifest_frame(&mut body.reader).map_err(classify)?;
    Ok((manifest, body.encrypted))
}

/// Inspect an archive: manifest, entry validation and a verifying pass.
///
/// Nothing is written. `destination_root` is only used to evaluate path
/// containment; it is never created or touched.
pub fn inspect(
    path: &Path,
    password: Option<&str>,
    destination_root: &Path,
) -> ExportResult<InspectionReport> {
    let mut sink = VerifyingSink::default();
    let (manifest, encrypted) = stream(path, password, destination_root, &mut sink)?;

    let mut by_namespace: Vec<(String, u64)> = Vec::new();
    for entry in &manifest.entries {
        let namespace = entry
            .path
            .split_once('/')
            .map_or("other", |(head, _)| head)
            .to_owned();
        match by_namespace.iter_mut().find(|(name, _)| *name == namespace) {
            Some((_, count)) => *count += 1,
            None => by_namespace.push((namespace, 1)),
        }
    }
    by_namespace.sort();

    Ok(InspectionReport {
        path: path.to_path_buf(),
        encrypted,
        entries_by_namespace: by_namespace,
        payloads_verified: sink.seen.len() == manifest.entries.len(),
        nonportable_settings: manifest.nonportable_settings.clone(),
        manifest,
    })
}

struct Body {
    reader: Box<dyn Read>,
    encrypted: bool,
}

fn open_body(path: &Path, password: Option<&str>) -> ExportResult<Body> {
    let file = BufReader::with_capacity(READ_CHUNK, File::open(path)?);
    let mut file: Box<dyn Read> = Box::new(file);
    let preamble = container::read_preamble(&mut file)?;
    match preamble.protection {
        ProtectionKind::Plaintext => Ok(Body {
            reader: file,
            encrypted: false,
        }),
        ProtectionKind::Encrypted => {
            let Some(password) = password else {
                return Err(ExportError::PasswordRequired);
            };
            let encryption = preamble
                .header
                .encryption
                .as_ref()
                .ok_or_else(|| ExportError::ArchiveRejected(ArchiveRejection::NotAnArchive))?;
            let reader = DecryptingReader::new(file, encryption, &preamble.header_bytes, password)?;
            Ok(Body {
                reader: Box::new(reader),
                encrypted: true,
            })
        }
    }
}

fn read_manifest_frame(reader: &mut impl Read) -> ExportResult<Manifest> {
    let header = container::read_frame_header(reader)?.ok_or_else(|| {
        ExportError::ArchiveRejected(ArchiveRejection::Malformed(
            "the archive has no manifest".to_owned(),
        ))
    })?;
    if header.path != MANIFEST_PATH {
        return Err(ExportError::ArchiveRejected(ArchiveRejection::Malformed(
            "the first entry is not the manifest".to_owned(),
        )));
    }
    if header.byte_length > MAX_MANIFEST_BYTES {
        return Err(ExportError::ArchiveRejected(
            ArchiveRejection::SizeCeilingExceeded("the manifest is too large".to_owned()),
        ));
    }
    let mut document = Vec::new();
    let digest = read_payload(reader, &header, |chunk| {
        document.extend_from_slice(chunk);
        Ok(())
    })
    .map_err(|error| match classify(error) {
        ExportError::Io(_) => ExportError::ArchiveRejected(ArchiveRejection::Malformed(
            "the manifest could not be read".to_owned(),
        )),
        other => other,
    })?;
    if digest != header.sha256 {
        return Err(ExportError::ArchiveRejected(
            ArchiveRejection::DigestMismatch {
                path: MANIFEST_PATH.to_owned(),
            },
        ));
    }
    let manifest: Manifest = serde_json::from_slice(&document).map_err(|_| {
        ExportError::ArchiveRejected(ArchiveRejection::Malformed(
            "the manifest is not valid archive metadata".to_owned(),
        ))
    })?;
    manifest.verify_self_consistency()?;
    Ok(manifest)
}

/// Read one payload, decompressing when needed, feeding `handle` and
/// returning the digest of the plaintext bytes.
///
/// The declared `byte_length` is a hard stop: a deflate stream that expands
/// beyond it is abandoned, so a decompression bomb cannot run away even if it
/// slipped past the declared-ratio check.
fn read_payload<F>(
    reader: &mut impl Read,
    entry: &ManifestEntry,
    mut handle: F,
) -> ExportResult<String>
where
    F: FnMut(&[u8]) -> ExportResult<()>,
{
    let mut stored = reader.take(entry.stored_length);
    let mut hasher = Sha256::new();
    let mut produced = 0u64;
    let mut buffer = vec![0u8; READ_CHUNK];

    let mut source: Box<dyn Read> = match entry.compression {
        Compression::Store => Box::new(&mut stored),
        Compression::Deflate => Box::new(DeflateDecoder::new(&mut stored)),
    };

    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        produced += read as u64;
        if produced > entry.byte_length {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::SizeCeilingExceeded(format!(
                    "entry {} expands beyond its declared length",
                    entry.path
                )),
            ));
        }
        hasher.update(&buffer[..read]);
        handle(&buffer[..read])?;
    }
    drop(source);

    if produced != entry.byte_length {
        return Err(ExportError::ArchiveRejected(
            ArchiveRejection::DigestMismatch {
                path: entry.path.clone(),
            },
        ));
    }
    // Drain any stored bytes the decompressor did not consume so the next
    // frame header starts at the right offset.
    let mut remaining = Vec::new();
    stored.read_to_end(&mut remaining)?;
    Ok(hex::encode(hasher.finalize()))
}

/// Stream an archive through `sink`, verifying structure and digests.
pub fn stream<S: EntrySink>(
    path: &Path,
    password: Option<&str>,
    destination_root: &Path,
    sink: &mut S,
) -> ExportResult<(Manifest, bool)> {
    let mut body = open_body(path, password)?;
    let manifest = read_manifest_frame(&mut body.reader).map_err(classify)?;
    validate_manifest_entries(&manifest, destination_root)?;

    for expected in &manifest.entries {
        let header = container::read_frame_header(&mut body.reader)
            .map_err(classify)?
            .ok_or_else(|| {
                ExportError::ArchiveRejected(ArchiveRejection::Malformed(
                    "the body ends before the manifest does".to_owned(),
                ))
            })?;
        if &header != expected {
            return Err(ExportError::ArchiveRejected(ArchiveRejection::Malformed(
                format!("frame {} does not match the manifest", header.path),
            )));
        }
        sink.begin(expected)?;
        let digest = read_payload(&mut body.reader, expected, |chunk| sink.chunk(chunk))
            .map_err(classify)?;
        if digest != expected.sha256 {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::DigestMismatch {
                    path: expected.path.clone(),
                },
            ));
        }
        sink.commit(expected)?;
    }

    if container::read_frame_header(&mut body.reader)
        .map_err(classify)?
        .is_some()
    {
        return Err(ExportError::ArchiveRejected(ArchiveRejection::Malformed(
            "the body carries entries the manifest does not list".to_owned(),
        )));
    }
    Ok((manifest, body.encrypted))
}

/// Digest of a file on disk, for round-trip assertions.
pub fn digest_file(path: &Path) -> ExportResult<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((
        Sha256Digest::from_bytes(hasher.finalize().into()).to_hex(),
        total,
    ))
}
