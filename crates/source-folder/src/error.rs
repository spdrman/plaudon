//! Failures a watched folder can produce.
//!
//! Every variant is actionable and none of them is destructive: §4.3 requires
//! us to "surface actionable status without destructive cleanup", so there is
//! no error here whose handling involves removing, renaming or truncating
//! anything the user owns.

use std::path::PathBuf;

use plaudon_persistence::error::PersistenceError;
use plaudon_security::fsguard::PathError;

/// What went wrong while watching or importing from a folder.
#[derive(Debug, thiserror::Error)]
pub enum FolderError {
    /// The path failed canonicalisation or left the permitted root (§16.5).
    #[error("path rejected: {0}")]
    PathRejected(#[from] PathError),
    /// A filesystem operation failed.
    #[error("filesystem error at {path}: {source}")]
    Io {
        /// Path the operation was attempted on.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The library database or archive refused the operation.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// The configured root is not a directory we can watch.
    #[error("watched root is unusable: {0}")]
    UnusableRoot(String),
    /// The platform watcher could not be started or kept running.
    #[error("filesystem watcher failed: {0}")]
    Watcher(String),
}

impl FolderError {
    /// Wrap an I/O failure with the path it happened on.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Result alias for folder operations.
pub type FolderResult<T> = Result<T, FolderError>;
