//! The `.recording-archive` container header and manifest (spec 13.6, 13.8).
//!
//! The container is an explicitly versioned format of our own rather than a
//! ZIP, because spec 13.6 permits "a versioned ZIP **or another explicitly
//! versioned container**" and because a bespoke frame layout lets creation and
//! extraction both stream with a fixed buffer, with no central directory to
//! seek to and no archive-library feature (symlinks, external attributes,
//! encryption-by-flag) that we would then have to defend against.
//!
//! ```text
//! "PLAUDARC"            8   magic
//! format_version        2   u16 little-endian
//! protection            1   0 plaintext, 1 encrypted
//! reserved              1   0
//! header_len            4   u32 little-endian
//! header_json      header_len bytes      <- authenticated metadata (AAD)
//! body                  ...  plaintext, or STREAM-AEAD chunks
//!
//! body := frame*  terminator
//! frame      := u32 header_len | frame_header_json | stored_length bytes
//! terminator := u32 0
//! ```
//!
//! The first frame is always `manifest.json`. Everything after it appears in
//! manifest order, so a reader validates the complete entry list *before* it
//! sees a single payload byte — that is what "inspect before writing" (spec
//! 13.8) requires.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ArchiveRejection, ExportError, ExportResult};

/// Container magic.
pub const MAGIC: &[u8; 8] = b"PLAUDARC";
/// Container format version this build writes and reads.
pub const FORMAT_VERSION: u16 = 1;
/// File extension (spec 13.6).
pub const ARCHIVE_EXTENSION: &str = "recording-archive";
/// Manifest schema version.
pub const MANIFEST_SCHEMA_VERSION: &str = "recording-archive-manifest/1";
/// Path of the manifest entry inside the container.
pub const MANIFEST_PATH: &str = "manifest.json";
/// Largest header JSON we will read before deciding the file is malformed.
pub const MAX_HEADER_BYTES: u32 = 64 * 1024;
/// Largest frame header JSON we will read.
pub const MAX_FRAME_HEADER_BYTES: u32 = 64 * 1024;
/// Largest manifest document we will read.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;

/// Honest description of what the integrity digest proves (spec 13.6).
pub const INTEGRITY_NOTE: &str = "This digest proves the archive's entry list has not changed since it was written. It is an integrity check, not an authenticity signature: it says nothing about who produced the archive unless the file is separately signed with a trusted key.";

/// Namespaces an entry path may start with (spec 13.6).
pub const NAMESPACES: &[&str] = &[
    "records/",
    "media/",
    "transcripts/",
    "outputs/",
    "sources/",
    "tools/",
    "annotations/",
];

/// Whether the archive body is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionKind {
    /// Readable by anyone holding the file.
    Plaintext,
    /// Authenticated encryption with a password-derived key.
    Encrypted,
}

impl ProtectionKind {
    /// Byte written in the container preamble.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Plaintext => 0,
            Self::Encrypted => 1,
        }
    }

    /// Parse the preamble byte.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Plaintext),
            1 => Some(Self::Encrypted),
            _ => None,
        }
    }
}

/// Authenticated container metadata. Serialized as the header JSON and used
/// verbatim as AEAD associated data, so it cannot be edited after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveHeader {
    /// Container format version, repeated inside the authenticated metadata.
    pub format_version: u16,
    /// Manifest schema version the body carries.
    pub manifest_schema_version: String,
    /// `plaintext` or the cipher construction name.
    pub protection: String,
    /// Encryption parameters, when the body is encrypted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<super::crypto::EncryptionHeader>,
}

/// Compression applied to one entry payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compression {
    /// Bytes are stored verbatim. Always used for media, so a restore is a
    /// byte-for-byte reproduction without a codec in the path.
    Store,
    /// Raw DEFLATE. Used for metadata documents.
    Deflate,
}

impl Compression {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Deflate => "deflate",
        }
    }
}

/// One entry, as described by the manifest and repeated in its frame header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    /// Archive-relative path, `/`-separated.
    pub path: String,
    /// Declared entry kind. Only `file` is ever materialized.
    pub kind: String,
    /// SHA-256 of the uncompressed payload, lowercase hex.
    pub sha256: String,
    /// Uncompressed payload length.
    pub byte_length: u64,
    /// Stored (possibly compressed) payload length.
    pub stored_length: u64,
    /// How the payload is stored.
    pub compression: Compression,
    /// Schema version of the document, for metadata entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
}

impl ManifestEntry {
    /// Whether this entry is a regular file we may materialize.
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.kind == "file"
    }
}

/// Size ceilings recorded in the manifest and enforced on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestLimits {
    /// Largest single entry.
    pub max_entry_bytes: u64,
    /// Largest total of all entries.
    pub max_total_bytes: u64,
    /// Largest entry count.
    pub max_entries: u64,
}

/// Totals, so a reader can check the entry list against itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestTotals {
    /// Number of entries, excluding the manifest itself.
    pub entry_count: u64,
    /// Sum of uncompressed entry lengths.
    pub byte_length: u64,
}

/// Settings that deliberately do not travel in an archive (spec 13.8).
pub const NONPORTABLE_SETTINGS: &[&str] = &[
    "OpenRouter API key and every other credential: stored in the OS keychain, never in an archive",
    "PLAUD Cloud CLI session and device authorization secrets: vault references only, not transferable",
    "Operating-system permission grants (Bluetooth, folder access, notifications): re-granted on the new machine",
    "Watched-folder paths and other absolute local paths: machine-specific, re-selected after restore",
    "Encrypted raw provider responses: decryptable only with the local key, so they are excluded",
    "Job queue state, caches and staging files: transient, recreated as needed",
    "Model downloads: re-installed by the model manager on the new machine",
];

/// The archive manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Manifest schema version.
    pub schema_version: String,
    /// Container format version.
    pub format_version: u16,
    /// When the archive was produced.
    pub created_at: String,
    /// Producing application.
    pub app: crate::bundle::AppStamp,
    /// Operating system family the archive was produced on, for diagnostics.
    pub source_os: String,
    /// Entries, in the exact order they appear in the body.
    pub entries: Vec<ManifestEntry>,
    /// Totals over `entries`.
    pub totals: ManifestTotals,
    /// Ceilings the writer enforced.
    pub limits: ManifestLimits,
    /// Digest over the canonical entry list.
    pub integrity_digest: String,
    /// What the digest does and does not prove.
    pub integrity_note: String,
    /// Settings that are not portable.
    pub nonportable_settings: Vec<String>,
}

impl Manifest {
    /// Digest over the canonical entry list.
    ///
    /// Canonical form is one `path\0kind\0sha256\0byte_length\0stored_length\0
    /// compression\0schema_version\n` record per entry, in manifest order, so
    /// reordering, adding, removing or editing an entry changes the digest.
    #[must_use]
    pub fn compute_integrity_digest(entries: &[ManifestEntry]) -> String {
        let mut hasher = Sha256::new();
        for entry in entries {
            hasher.update(entry.path.as_bytes());
            hasher.update([0]);
            hasher.update(entry.kind.as_bytes());
            hasher.update([0]);
            hasher.update(entry.sha256.as_bytes());
            hasher.update([0]);
            hasher.update(entry.byte_length.to_string().as_bytes());
            hasher.update([0]);
            hasher.update(entry.stored_length.to_string().as_bytes());
            hasher.update([0]);
            hasher.update(entry.compression.as_str().as_bytes());
            hasher.update([0]);
            hasher.update(entry.schema_version.as_deref().unwrap_or("").as_bytes());
            hasher.update(*b"\n");
        }
        hex::encode(hasher.finalize())
    }

    /// Check the manifest against itself: schema, digest and totals.
    pub fn verify_self_consistency(&self) -> ExportResult<()> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::UnsupportedManifestSchema {
                    found: self.schema_version.clone(),
                },
            ));
        }
        if self.format_version != FORMAT_VERSION {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::UnsupportedFormatVersion {
                    found: u32::from(self.format_version),
                    supported: u32::from(FORMAT_VERSION),
                },
            ));
        }
        if Self::compute_integrity_digest(&self.entries) != self.integrity_digest {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::IntegrityDigestMismatch,
            ));
        }
        let count = self.entries.len() as u64;
        let bytes: u64 = self
            .entries
            .iter()
            .try_fold(0u64, |total, entry| total.checked_add(entry.byte_length))
            .ok_or_else(|| {
                ExportError::ArchiveRejected(ArchiveRejection::SizeCeilingExceeded(
                    "declared entry lengths overflow".to_owned(),
                ))
            })?;
        if count != self.totals.entry_count || bytes != self.totals.byte_length {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::IntegrityDigestMismatch,
            ));
        }
        Ok(())
    }

    /// Entries belonging to the media namespace.
    #[must_use]
    pub fn media_entries(&self) -> Vec<&ManifestEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.path.starts_with("media/"))
            .collect()
    }
}

/// Whether a path sits inside one of the spec 13.6 namespaces.
#[must_use]
pub fn in_known_namespace(path: &str) -> bool {
    NAMESPACES.iter().any(|prefix| path.starts_with(prefix))
}

/// The operating-system family string recorded in a manifest.
#[must_use]
pub fn host_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, sha: &str, len: u64) -> ManifestEntry {
        ManifestEntry {
            path: path.to_owned(),
            kind: "file".to_owned(),
            sha256: sha.to_owned(),
            byte_length: len,
            stored_length: len,
            compression: Compression::Store,
            schema_version: None,
        }
    }

    fn manifest(entries: Vec<ManifestEntry>) -> Manifest {
        let digest = Manifest::compute_integrity_digest(&entries);
        Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
            format_version: FORMAT_VERSION,
            created_at: "1970-01-01T00:00:00Z".to_owned(),
            app: crate::bundle::AppStamp::default(),
            source_os: host_os().to_owned(),
            totals: ManifestTotals {
                entry_count: entries.len() as u64,
                byte_length: entries.iter().map(|e| e.byte_length).sum(),
            },
            limits: ManifestLimits {
                max_entry_bytes: 1 << 30,
                max_total_bytes: 1 << 40,
                max_entries: 1000,
            },
            entries,
            integrity_digest: digest,
            integrity_note: INTEGRITY_NOTE.to_owned(),
            nonportable_settings: NONPORTABLE_SETTINGS.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn a_consistent_manifest_verifies() {
        let manifest = manifest(vec![entry("media/aa.wav", &"a".repeat(64), 10)]);
        manifest.verify_self_consistency().unwrap();
    }

    #[test]
    fn editing_any_entry_field_changes_the_digest() {
        let base = vec![entry("media/aa.wav", &"a".repeat(64), 10)];
        let digest = Manifest::compute_integrity_digest(&base);
        for mutate in [
            |e: &mut ManifestEntry| e.path = "media/bb.wav".into(),
            |e: &mut ManifestEntry| e.sha256 = "b".repeat(64),
            |e: &mut ManifestEntry| e.byte_length = 11,
            |e: &mut ManifestEntry| e.stored_length = 11,
            |e: &mut ManifestEntry| e.compression = Compression::Deflate,
            |e: &mut ManifestEntry| e.kind = "symlink".into(),
            |e: &mut ManifestEntry| e.schema_version = Some("x/1".into()),
        ] {
            let mut entries = base.clone();
            mutate(&mut entries[0]);
            assert_ne!(Manifest::compute_integrity_digest(&entries), digest);
        }
    }

    #[test]
    fn reordering_entries_changes_the_digest() {
        let a = entry("records/a.json", &"a".repeat(64), 1);
        let b = entry("records/b.json", &"b".repeat(64), 2);
        assert_ne!(
            Manifest::compute_integrity_digest(&[a.clone(), b.clone()]),
            Manifest::compute_integrity_digest(&[b, a])
        );
    }

    #[test]
    fn a_tampered_manifest_is_rejected() {
        let mut tampered = manifest(vec![entry("media/aa.wav", &"a".repeat(64), 10)]);
        tampered.entries[0].sha256 = "b".repeat(64);
        assert!(matches!(
            tampered.verify_self_consistency().unwrap_err(),
            ExportError::ArchiveRejected(ArchiveRejection::IntegrityDigestMismatch)
        ));

        let mut wrong_totals = manifest(vec![entry("media/aa.wav", &"a".repeat(64), 10)]);
        wrong_totals.totals.byte_length = 999;
        assert!(wrong_totals.verify_self_consistency().is_err());
    }

    #[test]
    fn an_unknown_schema_or_format_version_is_refused() {
        let mut old = manifest(Vec::new());
        old.schema_version = "recording-archive-manifest/0".into();
        assert!(matches!(
            old.verify_self_consistency().unwrap_err(),
            ExportError::ArchiveRejected(ArchiveRejection::UnsupportedManifestSchema { .. })
        ));

        let mut future = manifest(Vec::new());
        future.format_version = 99;
        assert!(matches!(
            future.verify_self_consistency().unwrap_err(),
            ExportError::ArchiveRejected(ArchiveRejection::UnsupportedFormatVersion { .. })
        ));
    }

    #[test]
    fn the_integrity_note_does_not_claim_authenticity() {
        assert!(INTEGRITY_NOTE.contains("not an authenticity signature"));
    }

    #[test]
    fn namespaces_are_exactly_the_spec_set() {
        assert!(in_known_namespace("records/x.json"));
        assert!(in_known_namespace("media/ab.wav"));
        assert!(!in_known_namespace("jobs/1.json"));
        assert!(!in_known_namespace("secrets/key"));
        assert!(!in_known_namespace("manifest.json"));
    }
}
