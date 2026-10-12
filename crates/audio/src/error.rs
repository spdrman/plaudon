//! Stable audio-pipeline error taxonomy (§6.6: "stable typed errors", no
//! third-party exception types across the boundary).

use std::path::PathBuf;

use plaudon_domain::error::{AppError, AppErrorCode};

use crate::sniff::SniffedContainer;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// File exists but has no bytes.
    #[error("file is empty: {path}")]
    ZeroLength { path: PathBuf },

    /// Container recognised but the stream ends before the declared data.
    #[error("media is truncated: {detail}")]
    Truncated { detail: String },

    /// The file extension claims one container, the bytes are another.
    /// Fail-closed: we never decode as the extension claims (§8.1, F1.1).
    #[error("extension `.{declared}` does not match sniffed container `{sniffed}`")]
    ExtensionContentMismatch {
        declared: String,
        sniffed: &'static str,
    },

    /// A caller-declared size does not match the bytes on disk.
    #[error("declared size {declared} bytes does not match actual {actual} bytes")]
    DeclaredSizeMismatch { declared: u64, actual: u64 },

    /// Bytes are not audio we can name at all.
    #[error("not a recognised audio container")]
    UnrecognisedContainer,

    /// Container recognised, codec not decodable by the bundled decoder.
    /// Carries the actionable reason — never a silent fallback (§8.1).
    #[error("unsupported codec `{codec}`: {reason}")]
    UnsupportedCodec { codec: String, reason: String },

    #[error("malformed header: {0}")]
    MalformedHeader(String),

    /// A parsed length field would require an allocation beyond the hard cap.
    #[error("declared allocation of {requested} bytes exceeds the {limit} byte cap")]
    AllocationLimitExceeded { requested: u64, limit: u64 },

    /// Path rejected by the hardening rules (traversal, symlink outside the
    /// allowed roots, non-regular file, unreadable).
    #[error("path rejected: {reason}")]
    PathRejected { reason: String },

    /// Decode failed part-way. The partial result is discarded; it is never
    /// presented as complete audio (F1.2).
    #[error("decode failed after {frames_decoded} frames: {detail}")]
    DecodeFailed { frames_decoded: u64, detail: String },

    /// Preflight refused the workload before creating intermediates (§14.4).
    #[error("insufficient disk space: need {required_bytes} bytes, {available_bytes} available at {path}")]
    InsufficientDiskSpace {
        path: PathBuf,
        required_bytes: u64,
        available_bytes: u64,
    },

    /// Requested canonical profile cannot be produced from this source.
    #[error("unsupported canonical conversion: {0}")]
    UnsupportedConversion(String),

    #[error("cancelled")]
    Cancelled,

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl AudioError {
    /// Stable machine-readable code for the frontend error envelope (§15.4).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            AudioError::ZeroLength { .. } => "MEDIA_EMPTY",
            AudioError::Truncated { .. } => "MEDIA_TRUNCATED",
            AudioError::ExtensionContentMismatch { .. } => "MEDIA_EXTENSION_MISMATCH",
            AudioError::DeclaredSizeMismatch { .. } => "MEDIA_DECLARED_SIZE_MISMATCH",
            AudioError::UnrecognisedContainer => "MEDIA_UNRECOGNISED",
            AudioError::UnsupportedCodec { .. } => "MEDIA_UNSUPPORTED_CODEC",
            AudioError::MalformedHeader(_) => "MEDIA_MALFORMED_HEADER",
            AudioError::AllocationLimitExceeded { .. } => "MEDIA_ALLOCATION_LIMIT",
            AudioError::PathRejected { .. } => "MEDIA_PATH_REJECTED",
            AudioError::DecodeFailed { .. } => "MEDIA_DECODE_FAILED",
            AudioError::InsufficientDiskSpace { .. } => "MEDIA_INSUFFICIENT_DISK",
            AudioError::UnsupportedConversion(_) => "MEDIA_UNSUPPORTED_CONVERSION",
            AudioError::Cancelled => "MEDIA_CANCELLED",
            AudioError::Io { .. } => "MEDIA_IO",
        }
    }

    /// Whether reissuing the identical operation could plausibly succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            AudioError::Io { .. } | AudioError::InsufficientDiskSpace { .. }
        )
    }

    pub(crate) fn mismatch(declared: &str, sniffed: SniffedContainer) -> Self {
        AudioError::ExtensionContentMismatch {
            declared: declared.to_ascii_lowercase(),
            sniffed: sniffed.as_str(),
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        AudioError::Io {
            path: path.into(),
            source,
        }
    }
}

impl AudioError {
    /// Map onto the closed application error code set (§15.4).
    #[must_use]
    pub const fn app_error_code(&self) -> AppErrorCode {
        match self {
            AudioError::ZeroLength { .. }
            | AudioError::Truncated { .. }
            | AudioError::DeclaredSizeMismatch { .. }
            | AudioError::MalformedHeader(_)
            | AudioError::AllocationLimitExceeded { .. }
            | AudioError::DecodeFailed { .. } => AppErrorCode::MediaValidationFailed,
            AudioError::ExtensionContentMismatch { .. }
            | AudioError::UnrecognisedContainer
            | AudioError::UnsupportedConversion(_) => AppErrorCode::UnsupportedMedia,
            AudioError::UnsupportedCodec { .. } => AppErrorCode::UnsupportedCodec,
            AudioError::PathRejected { .. } => AppErrorCode::ArchivePathRejected,
            AudioError::InsufficientDiskSpace { .. } => AppErrorCode::StorageFull,
            AudioError::Cancelled => AppErrorCode::Canceled,
            AudioError::Io { .. } => AppErrorCode::StorageUnavailable,
        }
    }
}

impl From<AudioError> for AppError {
    fn from(error: AudioError) -> Self {
        AppError::new(error.app_error_code(), error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_prefixed() {
        let codes = [
            AudioError::ZeroLength { path: "/x".into() }.code(),
            AudioError::Truncated {
                detail: String::new(),
            }
            .code(),
            AudioError::mismatch("wav", SniffedContainer::Mp3).code(),
            AudioError::DeclaredSizeMismatch {
                declared: 1,
                actual: 2,
            }
            .code(),
            AudioError::UnrecognisedContainer.code(),
            AudioError::UnsupportedCodec {
                codec: String::new(),
                reason: String::new(),
            }
            .code(),
            AudioError::MalformedHeader(String::new()).code(),
            AudioError::AllocationLimitExceeded {
                requested: 1,
                limit: 2,
            }
            .code(),
            AudioError::PathRejected {
                reason: String::new(),
            }
            .code(),
            AudioError::DecodeFailed {
                frames_decoded: 0,
                detail: String::new(),
            }
            .code(),
            AudioError::InsufficientDiskSpace {
                path: "/x".into(),
                required_bytes: 1,
                available_bytes: 0,
            }
            .code(),
            AudioError::UnsupportedConversion(String::new()).code(),
            AudioError::Cancelled.code(),
            AudioError::Io {
                path: "/x".into(),
                source: std::io::Error::other("x"),
            }
            .code(),
        ];
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
        assert!(codes.iter().all(|code| code.starts_with("MEDIA_")));
    }

    #[test]
    fn only_transient_conditions_are_retryable() {
        assert!(!AudioError::UnrecognisedContainer.retryable());
        assert!(!AudioError::Cancelled.retryable());
        assert!(AudioError::Io {
            path: "/x".into(),
            source: std::io::Error::other("x")
        }
        .retryable());
    }
}
