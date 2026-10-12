//! Export, archive and restore failures, mapped onto the stable application
//! taxonomy (spec 15.4).
//!
//! Every variant carries a vendor-neutral explanation. None of them carries a
//! password, a key, a signed URL or transcript text, because the envelope the
//! frontend receives is built from the code's canonical message.

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_persistence::PersistenceError;

/// Result alias for export operations.
pub type ExportResult<T> = Result<T, ExportError>;

/// Why a caption export was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionRefusal {
    /// The revision declared alignment origin `none`: no real times exist.
    NoAlignment,
    /// The revision permits times but contains no segments to caption.
    NoSegments,
}

impl CaptionRefusal {
    /// Explanation shown to the user.
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::NoAlignment => {
                "Captions are unavailable: this transcript has no real timestamps. The engine returned text only, and we do not invent seek times."
            }
            Self::NoSegments => {
                "Captions are unavailable: this transcript revision has no segments."
            }
        }
    }
}

/// Why an archive was rejected before any user data was touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveRejection {
    /// The file does not begin with our container magic.
    NotAnArchive,
    /// The container format version is newer or older than we support.
    UnsupportedFormatVersion {
        /// Version the file declared.
        found: u32,
        /// Version this build writes and reads.
        supported: u32,
    },
    /// The manifest declared a schema version we do not implement.
    UnsupportedManifestSchema {
        /// Schema version the manifest declared.
        found: String,
    },
    /// Structural damage: truncated frame, bad length prefix, invalid JSON.
    Malformed(String),
    /// An entry path violated the path policy.
    PathRejected {
        /// The offending path, as declared in the archive.
        path: String,
        /// Why it was refused.
        reason: String,
    },
    /// Two entries declared the same path, or paths colliding under case folding.
    DuplicatePath(String),
    /// An entry or the archive exceeded a size ceiling.
    SizeCeilingExceeded(String),
    /// Declared and actual bytes disagreed.
    DigestMismatch {
        /// Entry that failed.
        path: String,
    },
    /// The manifest's overall integrity digest did not match its entries.
    IntegrityDigestMismatch,
    /// The archive declared an entry kind we refuse to materialize.
    UnsupportedEntryKind {
        /// Entry that declared it.
        path: String,
        /// The declared kind.
        kind: String,
    },
    /// Key-derivation parameters were abusive or nonsensical.
    KdfParametersRejected(String),
}

impl ArchiveRejection {
    /// Vendor-neutral explanation.
    #[must_use]
    pub fn explanation(&self) -> String {
        match self {
            Self::NotAnArchive => "This file is not a recording archive.".to_owned(),
            Self::UnsupportedFormatVersion { found, supported } => format!(
                "This archive uses container format version {found}; this application reads version {supported}."
            ),
            Self::UnsupportedManifestSchema { found } => {
                format!("This archive declares manifest schema '{found}', which this application does not implement.")
            }
            Self::Malformed(detail) => format!("This archive is damaged: {detail}."),
            Self::PathRejected { path, reason } => {
                format!("This archive contains an unsafe entry path ({reason}): {path}")
            }
            Self::DuplicatePath(path) => {
                format!("This archive contains more than one entry for the same path: {path}")
            }
            Self::SizeCeilingExceeded(detail) => {
                format!("This archive exceeds a safety limit: {detail}.")
            }
            Self::DigestMismatch { path } => {
                format!("An entry in this archive does not match its recorded checksum: {path}")
            }
            Self::IntegrityDigestMismatch => {
                "The archive manifest does not match its own entry list.".to_owned()
            }
            Self::UnsupportedEntryKind { path, kind } => {
                format!("This archive declares an unsupported entry kind '{kind}' for {path}.")
            }
            Self::KdfParametersRejected(detail) => {
                format!("This archive declares unusable password settings: {detail}.")
            }
        }
    }
}

/// An export, archive or restore failure.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The chosen destination is not a usable, safe place to write.
    #[error("export destination rejected: {0}")]
    DestinationRejected(String),
    /// Captions were requested for a transcript that has no real timestamps.
    #[error("{}", .0.explanation())]
    CaptionsUnavailable(CaptionRefusal),
    /// The selected media cannot be sanitized without re-encoding it.
    #[error("sanitized export is not available for this format: {0}")]
    SanitizationUnsupported(String),
    /// An archive was refused.
    #[error("{}", .0.explanation())]
    ArchiveRejected(ArchiveRejection),
    /// The archive is encrypted and the supplied password did not decrypt it.
    #[error("the archive could not be decrypted with that password")]
    DecryptionFailed,
    /// A password is required to read this archive.
    #[error("this archive is encrypted and requires its password")]
    PasswordRequired,
    /// Archive creation found credential-shaped content and refused to commit.
    #[error("archive creation aborted: {0}")]
    SecretExclusionFailed(String),
    /// Restore into a new library found the target library is not empty.
    #[error("the target library already contains recordings")]
    TargetLibraryNotEmpty,
    /// A restore conflict required a user decision that was not supplied.
    #[error("restore conflict requires a decision: {0}")]
    RestoreConflict(String),
    /// Stored bytes did not match their recorded identity.
    #[error("stored bytes failed their integrity check")]
    IntegrityFailed,
    /// The requested record does not exist.
    #[error("the requested record does not exist")]
    RecordNotFound,
    /// A deletion was requested without the required confirmation.
    #[error("deletion requires explicit confirmation of the disclosure")]
    ConfirmationRequired,
    /// The library database or archive failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// A filesystem operation failed.
    #[error("filesystem operation failed")]
    Io(#[source] std::io::Error),
    /// Serialization of an export document failed.
    #[error("export document could not be serialized")]
    Serialization(#[source] serde_json::Error),
}

impl ExportError {
    /// The stable application code for this failure.
    #[must_use]
    pub fn code(&self) -> AppErrorCode {
        match self {
            Self::DestinationRejected(_) => AppErrorCode::ExportDestinationRejected,
            Self::CaptionsUnavailable(_) => AppErrorCode::CaptionsUnavailable,
            Self::SanitizationUnsupported(_) => AppErrorCode::UnsupportedCodec,
            Self::ArchiveRejected(_) => AppErrorCode::ArchiveRejected,
            Self::DecryptionFailed | Self::PasswordRequired => {
                AppErrorCode::ArchiveDecryptionFailed
            }
            Self::SecretExclusionFailed(_) => AppErrorCode::SecretExclusionFailed,
            Self::TargetLibraryNotEmpty | Self::RestoreConflict(_) => AppErrorCode::Conflict,
            Self::IntegrityFailed => AppErrorCode::IntegrityFailed,
            Self::RecordNotFound => AppErrorCode::RecordNotFound,
            Self::ConfirmationRequired => AppErrorCode::ConfirmationRequired,
            Self::Persistence(inner) => inner.code(),
            Self::Io(error) if error.kind() == std::io::ErrorKind::StorageFull => {
                AppErrorCode::StorageFull
            }
            Self::Io(_) => AppErrorCode::StorageUnavailable,
            Self::Serialization(_) => AppErrorCode::InternalError,
        }
    }
}

impl From<std::io::Error> for ExportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ExportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

impl From<ExportError> for AppError {
    fn from(error: ExportError) -> Self {
        let code = error.code();
        Self::new(code, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rejection_explains_itself_without_leaking_bytes() {
        let rejections = [
            ArchiveRejection::NotAnArchive,
            ArchiveRejection::UnsupportedFormatVersion {
                found: 9,
                supported: 1,
            },
            ArchiveRejection::UnsupportedManifestSchema {
                found: "x/2".into(),
            },
            ArchiveRejection::Malformed("truncated frame".into()),
            ArchiveRejection::PathRejected {
                path: "../etc/passwd".into(),
                reason: "parent traversal".into(),
            },
            ArchiveRejection::DuplicatePath("records/a.json".into()),
            ArchiveRejection::SizeCeilingExceeded("entry exceeds 1 byte".into()),
            ArchiveRejection::DigestMismatch {
                path: "media/aa.wav".into(),
            },
            ArchiveRejection::IntegrityDigestMismatch,
            ArchiveRejection::UnsupportedEntryKind {
                path: "x".into(),
                kind: "symlink".into(),
            },
            ArchiveRejection::KdfParametersRejected("memory cost too high".into()),
        ];
        for rejection in rejections {
            let text = rejection.explanation();
            assert_ne!(text, String::new());
            assert!(text.ends_with('.') || text.contains(':'));
        }
    }

    #[test]
    fn failures_map_onto_stable_codes() {
        assert_eq!(
            ExportError::CaptionsUnavailable(CaptionRefusal::NoAlignment).code(),
            AppErrorCode::CaptionsUnavailable
        );
        assert_eq!(
            ExportError::DecryptionFailed.code(),
            AppErrorCode::ArchiveDecryptionFailed
        );
        assert_eq!(
            ExportError::SecretExclusionFailed("x".into()).code(),
            AppErrorCode::SecretExclusionFailed
        );
        assert_eq!(
            ExportError::ArchiveRejected(ArchiveRejection::NotAnArchive).code(),
            AppErrorCode::ArchiveRejected
        );
        assert!(!AppError::from(ExportError::IntegrityFailed).is_retryable());
    }

    #[test]
    fn a_caption_refusal_says_why_and_offers_no_invented_times() {
        let text = CaptionRefusal::NoAlignment.explanation();
        assert!(text.contains("no real timestamps"));
        assert!(text.contains("do not invent"));
    }
}
