//! The `.recording-archive` portable backup format (spec 13.6, 13.8).
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`manifest`] | container layout, manifest, integrity digest |
//! | [`container`] | preamble and frame encoding |
//! | [`crypto`] | Argon2id key derivation and STREAM AEAD |
//! | [`documents`] | the restore model, one document per entry |
//! | [`snapshot`] | reading those documents out of a live library |
//! | [`writer`] | streaming creation |
//! | [`reader`] | inspection, validation and verified streaming |
//! | [`restore`] | staging and transactional integration |
//! | [`secrets`] | secret-exclusion enforcement before commit |

pub mod container;
pub mod crypto;
pub mod documents;
pub mod manifest;
pub mod reader;
pub mod restore;
pub mod secrets;
pub mod snapshot;
pub mod writer;

use manifest::ManifestLimits;

/// Size ceilings this build writes and enforces.
///
/// They match `plaudon_security::fsguard::archive`'s defaults, because the
/// restore path hands every entry to that validator and two different limits
/// would mean the stricter one silently wins.
pub const DEFAULT_LIMITS: ManifestLimits = ManifestLimits {
    max_entry_bytes: plaudon_security::fsguard::archive::DEFAULT_MAX_ENTRY_BYTES,
    max_total_bytes: plaudon_security::fsguard::archive::DEFAULT_MAX_TOTAL_BYTES,
    max_entries: plaudon_security::fsguard::archive::DEFAULT_MAX_ENTRIES as u64,
};

pub use manifest::{
    Manifest, ManifestEntry, ProtectionKind, ARCHIVE_EXTENSION, FORMAT_VERSION,
    MANIFEST_SCHEMA_VERSION,
};
pub use reader::{inspect, InspectionReport};
pub use restore::{
    ConflictPolicy, RestoreConflict, RestoreMode, RestoreOptions, RestorePreview, RestoreReport,
    Restorer,
};
pub use writer::{ArchiveArtifact, ArchiveBuilder, Protection};
