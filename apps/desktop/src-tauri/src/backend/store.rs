//! Opening the library: one database, one migration runner, one archive.
//!
//! The desktop shell owns no schema of its own beyond migration 0300
//! (`crates/persistence/migrations/0300_desktop_shell.sql`), which is applied
//! by [`plaudon_persistence::migrations::MigrationRunner`] together with the
//! core migrations. There is no second database file and no second migration
//! history, so the tamper-detection gate stays valid.

use std::path::{Path, PathBuf};

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;
use sqlx::SqlitePool;

/// File name of the library database, created by `plaudon_persistence`.
pub const DATABASE_FILE_NAME: &str = "app.sqlite";

/// Open database plus archive.
#[derive(Debug, Clone)]
pub struct Store {
    db: Database,
    archive: MediaArchive,
    root: PathBuf,
}

impl Store {
    /// Open (creating if needed) the library under `root` and migrate it.
    pub async fn open(root: &Path) -> Result<Self, AppError> {
        let root = root.to_path_buf();
        std::fs::create_dir_all(&root).map_err(|err| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot create the library directory: {err}"),
            )
        })?;
        let db = Database::open(root.join(DATABASE_FILE_NAME))
            .await
            .map_err(AppError::from)?;
        MigrationRunner::new()
            .run(&db)
            .await
            .map_err(AppError::from)?;
        let archive = MediaArchive::open(&root).map_err(AppError::from)?;
        Ok(Self { db, archive, root })
    }

    /// The database handle.
    #[must_use]
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// The connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        self.db.pool()
    }

    /// The media archive.
    #[must_use]
    pub fn archive(&self) -> &MediaArchive {
        &self.archive
    }

    /// The library root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding installed model files.
    #[must_use]
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }
}

/// Map a sqlx failure that is not expected to happen to a storage error.
pub fn storage_error(context: &str, err: &sqlx::Error) -> AppError {
    AppError::new(
        AppErrorCode::StorageUnavailable,
        format!("{context}: {err}"),
    )
}

/// Convert a stored signed integer to an unsigned count.
pub fn as_u64(context: &'static str, value: i64) -> Result<u64, AppError> {
    u64::try_from(value).map_err(|_| {
        AppError::new(
            AppErrorCode::InternalError,
            format!("{context} is negative"),
        )
    })
}

/// Convert a stored signed integer to a 32-bit count.
pub fn as_u32(context: &'static str, value: i64) -> Result<u32, AppError> {
    u32::try_from(value).map_err(|_| {
        AppError::new(
            AppErrorCode::InternalError,
            format!("{context} is out of range"),
        )
    })
}

/// Render a stored instant as the RFC 3339 string the contract requires.
pub fn rfc3339(context: &'static str, millis: i64) -> Result<String, AppError> {
    Timestamp::from_millis(millis).to_rfc3339().ok_or_else(|| {
        AppError::new(
            AppErrorCode::InternalError,
            format!("{context} is out of range"),
        )
    })
}

/// Render an optional stored instant.
pub fn rfc3339_opt(context: &'static str, millis: Option<i64>) -> Result<Option<String>, AppError> {
    millis.map(|value| rfc3339(context, value)).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opening_twice_is_idempotent_and_runs_one_migration_history() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).await.unwrap();
        let applied = MigrationRunner::applied(first.db()).await.unwrap();
        assert_eq!(
            applied.len(),
            plaudon_persistence::migrations::MIGRATIONS.len()
        );
        assert!(applied.iter().any(|row| row.version == 300));
        first.db().close().await;

        let second = Store::open(dir.path()).await.unwrap();
        assert_eq!(
            MigrationRunner::applied(second.db()).await.unwrap().len(),
            plaudon_persistence::migrations::MIGRATIONS.len()
        );
        second.db().close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_desktop_tables_exist_next_to_the_core_tables() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
                .fetch_all(store.pool())
                .await
                .unwrap();
        for expected in [
            "recordings",
            "media_assets",
            "transcript_revisions",
            "jobs",
            "activity_events",
            "event_log",
            "recording_import_metadata",
            "schema_migrations",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing table {expected}"
            );
        }
        assert!(
            !names.iter().any(|name| name == "desktop_schema_version"),
            "the shell does not keep a second migration history"
        );
        store.db().close().await;
    }
}
