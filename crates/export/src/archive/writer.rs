//! Streaming `.recording-archive` creation (spec 13.6, 13.8).
//!
//! Creation is two passes and never holds the archive in memory:
//!
//! 1. **Stage.** Each metadata document is scanned for credential shapes,
//!    hashed, compressed into the private staging directory and described by a
//!    manifest entry. Media entries are described from the content-addressed
//!    identity the library already holds; their bytes stay on disk.
//! 2. **Emit.** The manifest is written first, then every payload in manifest
//!    order, streaming through a fixed buffer. Media digests are recomputed
//!    while streaming, so an asset that changed under us fails the backup
//!    instead of being written with a stale hash.
//!
//! The output lands in a `.part` file next to the destination and is renamed
//! only after the terminator is written and the file is fsynced, so an
//! interrupted backup leaves no `.recording-archive` behind.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use flate2::write::DeflateEncoder;
use flate2::Compression as FlateLevel;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use plaudon_security::fsguard::{canonicalize_and_validate, PrivateTempDir};
use sha2::{Digest, Sha256};

use super::crypto::{EncryptingWriter, EncryptionHeader, KdfProfile};
use super::manifest::{
    host_os, in_known_namespace, ArchiveHeader, Compression, Manifest, ManifestEntry,
    ManifestLimits, ManifestTotals, ProtectionKind, FORMAT_VERSION, INTEGRITY_NOTE, MANIFEST_PATH,
    MANIFEST_SCHEMA_VERSION, NONPORTABLE_SETTINGS,
};
use super::secrets::ArchiveSecretScanner;
use super::{container, DEFAULT_LIMITS};
use crate::bundle::AppStamp;
use crate::destination::ExportDestination;
use crate::error::{ArchiveRejection, ExportError, ExportResult};

/// Bytes moved per read while emitting payloads.
pub const EMIT_CHUNK: usize = 64 * 1024;

/// Staging-directory prefix for backup creation.
pub const STAGING_PREFIX: &str = "plaudon-backup";

/// How the produced archive is protected.
#[derive(Debug, Clone)]
pub enum Protection {
    /// No encryption. The UI must disclose this conspicuously (spec 13.6).
    Plaintext,
    /// Argon2id + XChaCha20-Poly1305 STREAM.
    Encrypted {
        /// User-chosen password. Never stored, never recoverable.
        password: String,
        /// Argon2id profile.
        profile: KdfProfile,
    },
}

impl Protection {
    /// The protection kind recorded in the container preamble.
    #[must_use]
    pub const fn kind(&self) -> ProtectionKind {
        match self {
            Self::Plaintext => ProtectionKind::Plaintext,
            Self::Encrypted { .. } => ProtectionKind::Encrypted,
        }
    }
}

enum Payload {
    /// Already staged, compressed bytes.
    Staged(PathBuf),
    /// A library blob, streamed verbatim.
    Library(PathBuf),
}

struct PendingEntry {
    entry: ManifestEntry,
    payload: Payload,
}

/// What a completed backup produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveArtifact {
    /// Where the archive was written.
    pub path: PathBuf,
    /// The manifest as written.
    pub manifest: Manifest,
    /// Whether the body is encrypted.
    pub protection: ProtectionKind,
    /// Total archive file size in bytes.
    pub file_bytes: u64,
}

/// Accumulates entries and emits the container.
#[derive(Debug)]
pub struct ArchiveBuilder {
    staging: PrivateTempDir,
    entries: Vec<PendingEntry>,
    next_staged: u64,
    limits: ManifestLimits,
    created_at: Timestamp,
}

impl std::fmt::Debug for PendingEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingEntry")
            .field("path", &self.entry.path)
            .finish_non_exhaustive()
    }
}

impl ArchiveBuilder {
    /// Create a builder staging into a private directory under `staging_base`.
    pub fn new(staging_base: &Path, created_at: Timestamp) -> ExportResult<Self> {
        let staging = PrivateTempDir::create(Some(staging_base), STAGING_PREFIX)
            .map_err(|error| ExportError::DestinationRejected(error.reason().to_string()))?;
        Ok(Self {
            staging,
            entries: Vec::new(),
            next_staged: 0,
            limits: DEFAULT_LIMITS,
            created_at,
        })
    }

    /// Number of entries staged so far.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Add a metadata document.
    ///
    /// The document is scanned before it is staged, so a credential shape
    /// fails the backup before anything is compressed, let alone committed.
    pub fn add_document(
        &mut self,
        path: &str,
        schema_version: &str,
        document: &[u8],
    ) -> ExportResult<()> {
        self.check_path(path)?;
        ArchiveSecretScanner::enforce(path, document)?;
        if document.len() as u64 > self.limits.max_entry_bytes {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::SizeCeilingExceeded(format!("entry {path} is too large")),
            ));
        }

        let name = format!("entry-{:08}", self.next_staged);
        self.next_staged += 1;
        let mut staged = self
            .staging
            .stage_file(&name)
            .map_err(|error| ExportError::DestinationRejected(error.reason().to_string()))?;
        {
            let file = staged
                .file_mut()
                .ok_or_else(|| ExportError::DestinationRejected("staging file closed".into()))?;
            let mut encoder = DeflateEncoder::new(file, FlateLevel::default());
            encoder.write_all(document)?;
            encoder.finish()?;
        }
        // Commit inside the staging directory itself: the `.part` guard removes
        // a partial file on drop, and the builder removes the whole directory
        // when it is dropped, so nothing escapes either way.
        let target =
            canonicalize_and_validate(&self.staging.policy(), &self.staging.path().join(&name))
                .map_err(|error| ExportError::DestinationRejected(error.reason().to_string()))?;
        let staged_path = staged
            .commit_by_rename(&target)
            .map_err(|error| ExportError::DestinationRejected(error.reason().to_string()))?
            .as_path()
            .to_path_buf();
        let stored_length = std::fs::metadata(&staged_path)?.len();

        self.push(PendingEntry {
            entry: ManifestEntry {
                path: path.to_owned(),
                kind: "file".to_owned(),
                sha256: Sha256Digest::of(document).to_hex(),
                byte_length: document.len() as u64,
                stored_length,
                compression: Compression::Deflate,
                schema_version: Some(schema_version.to_owned()),
            },
            payload: Payload::Staged(staged_path),
        })
    }

    /// Add a media blob from the content-addressed library archive.
    ///
    /// Stored uncompressed, so a restore reproduces the exact bytes with no
    /// codec in the path and a constant-memory copy.
    pub fn add_media(
        &mut self,
        path: &str,
        source: &Path,
        sha256: &str,
        byte_length: u64,
    ) -> ExportResult<()> {
        self.check_path(path)?;
        if byte_length > self.limits.max_entry_bytes {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::SizeCeilingExceeded(format!("entry {path} is too large")),
            ));
        }
        self.push(PendingEntry {
            entry: ManifestEntry {
                path: path.to_owned(),
                kind: "file".to_owned(),
                sha256: sha256.to_owned(),
                byte_length,
                stored_length: byte_length,
                compression: Compression::Store,
                schema_version: None,
            },
            payload: Payload::Library(source.to_path_buf()),
        })
    }

    fn check_path(&self, path: &str) -> ExportResult<()> {
        if !in_known_namespace(path) {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::PathRejected {
                    path: path.to_owned(),
                    reason: "outside the archive namespaces".to_owned(),
                },
            ));
        }
        if self
            .entries
            .iter()
            .any(|pending| pending.entry.path.eq_ignore_ascii_case(path))
        {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::DuplicatePath(path.to_owned()),
            ));
        }
        Ok(())
    }

    fn push(&mut self, pending: PendingEntry) -> ExportResult<()> {
        let count = self.entries.len() as u64 + 1;
        if count > self.limits.max_entries {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::SizeCeilingExceeded("too many entries".to_owned()),
            ));
        }
        let total: u64 = self
            .entries
            .iter()
            .map(|pending| pending.entry.byte_length)
            .sum::<u64>()
            + pending.entry.byte_length;
        if total > self.limits.max_total_bytes {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::SizeCeilingExceeded("archive total is too large".to_owned()),
            ));
        }
        self.entries.push(pending);
        Ok(())
    }

    fn manifest(&self) -> Manifest {
        let entries: Vec<ManifestEntry> = self.entries.iter().map(|p| p.entry.clone()).collect();
        let digest = Manifest::compute_integrity_digest(&entries);
        Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
            format_version: FORMAT_VERSION,
            created_at: self
                .created_at
                .to_rfc3339()
                .unwrap_or_else(|| self.created_at.as_millis().to_string()),
            app: AppStamp::default(),
            source_os: host_os().to_owned(),
            totals: ManifestTotals {
                entry_count: entries.len() as u64,
                byte_length: entries.iter().map(|entry| entry.byte_length).sum(),
            },
            limits: self.limits,
            entries,
            integrity_digest: digest,
            integrity_note: INTEGRITY_NOTE.to_owned(),
            nonportable_settings: NONPORTABLE_SETTINGS
                .iter()
                .map(|note| (*note).to_owned())
                .collect(),
        }
    }

    /// Emit the archive into `destination` under `file_name`.
    pub fn finish(
        self,
        destination: &ExportDestination,
        file_name: &str,
        protection: &Protection,
    ) -> ExportResult<ArchiveArtifact> {
        let manifest = self.manifest();
        let manifest_json = serde_json::to_vec(&manifest)?;
        ArchiveSecretScanner::enforce(MANIFEST_PATH, &manifest_json)?;

        let header = match protection {
            Protection::Plaintext => ArchiveHeader {
                format_version: FORMAT_VERSION,
                manifest_schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
                protection: "plaintext".to_owned(),
                encryption: None,
            },
            Protection::Encrypted { profile, .. } => ArchiveHeader {
                format_version: FORMAT_VERSION,
                manifest_schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
                protection: super::crypto::CIPHER_NAME.to_owned(),
                encryption: Some(EncryptionHeader::generate(*profile)),
            },
        };

        let entries = &self.entries;
        let (path, file_bytes) = destination.write_atomic_with(file_name, |file| {
            let header_bytes = container::write_preamble(file, protection.kind(), &header)?;
            match protection {
                Protection::Plaintext => {
                    Self::emit_body(&mut *file, &manifest_json, entries)?;
                }
                Protection::Encrypted { password, .. } => {
                    let encryption = header
                        .encryption
                        .as_ref()
                        .ok_or_else(|| ExportError::IntegrityFailed)?;
                    let mut body =
                        EncryptingWriter::new(&mut *file, encryption, &header_bytes, password)?;
                    Self::emit_body(&mut body, &manifest_json, entries)?;
                    body.finish()?;
                }
            }
            file.flush()?;
            Ok(file.metadata()?.len())
        })?;

        Ok(ArchiveArtifact {
            path,
            manifest,
            protection: protection.kind(),
            file_bytes,
        })
    }

    fn emit_body(
        body: &mut impl Write,
        manifest_json: &[u8],
        entries: &[PendingEntry],
    ) -> ExportResult<()> {
        let mut compressed = Vec::new();
        {
            let mut encoder = DeflateEncoder::new(&mut compressed, FlateLevel::default());
            encoder.write_all(manifest_json)?;
            encoder.finish()?;
        }
        container::write_frame_header(
            body,
            &ManifestEntry {
                path: MANIFEST_PATH.to_owned(),
                kind: "file".to_owned(),
                sha256: Sha256Digest::of(manifest_json).to_hex(),
                byte_length: manifest_json.len() as u64,
                stored_length: compressed.len() as u64,
                compression: Compression::Deflate,
                schema_version: Some(MANIFEST_SCHEMA_VERSION.to_owned()),
            },
        )?;
        body.write_all(&compressed)?;

        let mut buffer = vec![0u8; EMIT_CHUNK];
        for pending in entries {
            container::write_frame_header(body, &pending.entry)?;
            let source = match &pending.payload {
                Payload::Staged(path) | Payload::Library(path) => path,
            };
            let verify = matches!(pending.payload, Payload::Library(_));
            let mut reader = BufReader::with_capacity(EMIT_CHUNK, File::open(source)?);
            let mut hasher = Sha256::new();
            let mut written = 0u64;
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                if verify {
                    hasher.update(&buffer[..read]);
                }
                body.write_all(&buffer[..read])?;
                written += read as u64;
            }
            if written != pending.entry.stored_length {
                return Err(ExportError::IntegrityFailed);
            }
            if verify
                && Sha256Digest::from_bytes(hasher.finalize().into()).to_hex()
                    != pending.entry.sha256
            {
                // A library blob changed between planning and emission.
                return Err(ExportError::IntegrityFailed);
            }
        }
        container::write_terminator(body)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_outside_the_namespaces_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = ArchiveBuilder::new(dir.path(), Timestamp::from_millis(1)).unwrap();
        let error = builder
            .add_document("jobs/1.json", "x/1", b"{}")
            .unwrap_err();
        assert!(matches!(
            error,
            ExportError::ArchiveRejected(ArchiveRejection::PathRejected { .. })
        ));
    }

    #[test]
    fn a_duplicate_path_is_refused_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = ArchiveBuilder::new(dir.path(), Timestamp::from_millis(1)).unwrap();
        builder
            .add_document("records/a.json", "recording-export/1", b"{}")
            .unwrap();
        let error = builder
            .add_document("records/A.JSON", "recording-export/1", b"{}")
            .unwrap_err();
        assert!(matches!(
            error,
            ExportError::ArchiveRejected(ArchiveRejection::DuplicatePath(_))
        ));
    }

    #[test]
    fn a_credential_shaped_document_aborts_before_anything_is_staged() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = ArchiveBuilder::new(dir.path(), Timestamp::from_millis(1)).unwrap();
        let error = builder
            .add_document(
                "records/a.json",
                "recording-export/1",
                br#"{"annotation":"sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
            )
            .unwrap_err();
        assert!(matches!(error, ExportError::SecretExclusionFailed(_)));
        assert_eq!(builder.entry_count(), 0);
    }
}
