//! Persistence failures, mapped onto the stable application taxonomy.

use plaudon_domain::error::{AppError, AppErrorCode};

/// Result alias for persistence operations.
pub type PersistenceResult<T> = Result<T, PersistenceError>;

/// A persistence or archive failure.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// The database could not be opened or queried.
    #[error("the local library is unavailable")]
    StorageUnavailable(#[source] Box<sqlx::Error>),
    /// A write failed because the volume is full.
    #[error("the volume has no free space")]
    StorageFull,
    /// A migration could not be applied, or history does not match.
    #[error("migration failure: {0}")]
    Migration(String),
    /// The requested row does not exist.
    #[error("the requested record does not exist")]
    RecordNotFound,
    /// A uniqueness or precondition check rejected the write.
    #[error("the write conflicts with an existing record")]
    Conflict,
    /// Stored bytes did not match their recorded identity.
    #[error("stored bytes failed their integrity check")]
    IntegrityFailed,
    /// A path was rejected by the archive path policy.
    #[error("path rejected by the archive policy: {0}")]
    ArchivePathRejected(String),
    /// Staging and archive are on different volumes.
    #[error("staging and archive are on different volumes")]
    CrossVolumeCommit,
    /// A filesystem operation failed.
    #[error("filesystem operation failed")]
    Io(#[source] std::io::Error),
    /// A stored row, or a row about to be stored, does not satisfy the
    /// schema contract this build enforces.
    #[error("a record did not match the schema contract: {0}")]
    Invalid(String),
}

impl PersistenceError {
    /// The stable application code for this failure.
    #[must_use]
    pub fn code(&self) -> AppErrorCode {
        match self {
            Self::StorageUnavailable(_) | Self::Io(_) => AppErrorCode::StorageUnavailable,
            Self::StorageFull => AppErrorCode::StorageFull,
            Self::Migration(_) => AppErrorCode::MigrationFailed,
            Self::RecordNotFound => AppErrorCode::RecordNotFound,
            Self::Conflict => AppErrorCode::Conflict,
            Self::IntegrityFailed => AppErrorCode::IntegrityFailed,
            Self::ArchivePathRejected(_) => AppErrorCode::ArchivePathRejected,
            Self::CrossVolumeCommit => AppErrorCode::CrossVolumeCommit,
            Self::Invalid(_) => AppErrorCode::MalformedOutput,
        }
    }
}

impl From<sqlx::Error> for PersistenceError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &error {
            let message = db.message().to_ascii_lowercase();
            if message.contains("unique constraint")
                || message.contains("constraint failed")
                || message.contains("foreign key")
            {
                return Self::Conflict;
            }
            if message.contains("disk is full") || message.contains("database or disk is full") {
                return Self::StorageFull;
            }
        }
        Self::StorageUnavailable(Box::new(error))
    }
}

impl From<std::io::Error> for PersistenceError {
    fn from(error: std::io::Error) -> Self {
        #[allow(clippy::match_same_arms)]
        match error.kind() {
            std::io::ErrorKind::StorageFull => Self::StorageFull,
            _ => Self::Io(error),
        }
    }
}

impl From<PersistenceError> for AppError {
    fn from(error: PersistenceError) -> Self {
        let code = error.code();
        Self::new(code, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_maps_to_a_stable_code() {
        let pairs: Vec<(PersistenceError, AppErrorCode)> = vec![
            (PersistenceError::StorageFull, AppErrorCode::StorageFull),
            (
                PersistenceError::Migration("x".into()),
                AppErrorCode::MigrationFailed,
            ),
            (
                PersistenceError::RecordNotFound,
                AppErrorCode::RecordNotFound,
            ),
            (PersistenceError::Conflict, AppErrorCode::Conflict),
            (
                PersistenceError::IntegrityFailed,
                AppErrorCode::IntegrityFailed,
            ),
            (
                PersistenceError::ArchivePathRejected("x".into()),
                AppErrorCode::ArchivePathRejected,
            ),
            (
                PersistenceError::CrossVolumeCommit,
                AppErrorCode::CrossVolumeCommit,
            ),
        ];
        for (error, expected) in pairs {
            assert_eq!(error.code(), expected);
        }
    }

    #[test]
    fn integrity_and_cross_volume_failures_are_not_retryable() {
        assert!(!AppError::from(PersistenceError::IntegrityFailed).is_retryable());
        assert!(!AppError::from(PersistenceError::CrossVolumeCommit).is_retryable());
    }
}
