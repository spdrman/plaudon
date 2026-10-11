//! Database handle, directory layout and the desktop-local migration runner.
//!
//! The runner is intentionally tiny: one embedded file, one version row, a
//! checksum so a tampered or half-applied schema is detected rather than
//! silently patched. It writes `desktop_schema_version`, never the
//! `schema_migrations` table owned by `plaudon_persistence::migrations`.

use std::path::{Path, PathBuf};

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

/// File name of the desktop-local database.
///
/// Deliberately distinct from the library database EPIC B will create, so the
/// two migration histories can never meet.
pub const DATABASE_FILE_NAME: &str = "desktop-local.sqlite3";

/// Version number of the only migration this crate owns.
pub const MIGRATION_VERSION: i64 = 9001;

const MIGRATION_SQL: &str = include_str!("migrations/9001_desktop_local.sql");

/// Open database plus the archive directory layout (spec §13.4).
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
    root: PathBuf,
}

impl Store {
    /// Open (creating if needed) the desktop-local store under `root`.
    pub async fn open(root: &Path) -> Result<Self, AppError> {
        let root = root.to_path_buf();
        for dir in [root.clone(), root.join("media"), root.join("staging"), root.join("models")] {
            std::fs::create_dir_all(&dir).map_err(|err| {
                AppError::new(
                    AppErrorCode::StorageUnavailable,
                    format!("cannot create {}: {err}", dir.display()),
                )
            })?;
        }

        let options = SqliteConnectOptions::new()
            .filename(root.join(DATABASE_FILE_NAME))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10));

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(|err| {
                AppError::new(
                    AppErrorCode::StorageUnavailable,
                    format!("cannot open the desktop-local database: {err}"),
                )
            })?;

        let store = Self { pool, root };
        store.migrate().await?;
        Ok(store)
    }

    /// The connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The library root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding content-addressed media.
    #[must_use]
    pub fn media_dir(&self) -> PathBuf {
        self.root.join("media")
    }

    /// Directory holding in-flight import files.
    #[must_use]
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// Directory holding installed model files.
    #[must_use]
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    async fn migrate(&self) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS desktop_schema_version (\
                version    INTEGER PRIMARY KEY NOT NULL,\
                checksum   TEXT    NOT NULL,\
                applied_at INTEGER NOT NULL)",
        )
        .execute(&self.pool)
        .await
        .map_err(|err| migration_failed("cannot create desktop_schema_version", &err))?;

        let checksum = Sha256Digest::of(MIGRATION_SQL.as_bytes()).to_hex();
        let existing: Option<String> =
            sqlx::query("SELECT checksum FROM desktop_schema_version WHERE version = ?1")
                .bind(MIGRATION_VERSION)
                .fetch_optional(&self.pool)
                .await
                .map_err(|err| migration_failed("cannot read desktop_schema_version", &err))?
                .map(|row| row.get::<String, _>("checksum"));

        match existing {
            Some(recorded) if recorded == checksum => Ok(()),
            Some(_) => Err(AppError::new(
                AppErrorCode::MigrationFailed,
                "the recorded desktop-local schema checksum does not match this build",
            )),
            None => {
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|err| migration_failed("cannot begin the migration", &err))?;
                for statement in split_statements(MIGRATION_SQL) {
                    sqlx::query(&statement)
                        .execute(&mut *tx)
                        .await
                        .map_err(|err| migration_failed("statement failed", &err))?;
                }
                sqlx::query(
                    "INSERT INTO desktop_schema_version (version, checksum, applied_at) \
                     VALUES (?1, ?2, ?3)",
                )
                .bind(MIGRATION_VERSION)
                .bind(&checksum)
                .bind(Timestamp::now().as_millis())
                .execute(&mut *tx)
                .await
                .map_err(|err| migration_failed("cannot record the applied version", &err))?;
                tx.commit()
                    .await
                    .map_err(|err| migration_failed("cannot commit the migration", &err))
            }
        }
    }
}

fn migration_failed(context: &str, err: &sqlx::Error) -> AppError {
    AppError::new(AppErrorCode::MigrationFailed, format!("{context}: {err}"))
}

/// Split the embedded migration into statements.
///
/// The file contains no string literals and no triggers, so splitting on `;`
/// after stripping `--` comments is exact for it; the migration test asserts
/// the resulting schema.
fn split_statements(sql: &str) -> Vec<String> {
    let without_comments: String = sql
        .lines()
        .map(|line| match line.find("--") {
            Some(index) => &line[..index],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    without_comments
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Map a sqlx failure that is not expected to happen to a storage error.
pub fn storage_error(context: &str, err: &sqlx::Error) -> AppError {
    AppError::new(AppErrorCode::StorageUnavailable, format!("{context}: {err}"))
}

/// Convert a stored signed integer to an unsigned count.
pub fn as_u64(context: &'static str, value: i64) -> Result<u64, AppError> {
    u64::try_from(value)
        .map_err(|_| AppError::new(AppErrorCode::InternalError, format!("{context} is negative")))
}

/// Convert a stored signed integer to a 32-bit count.
pub fn as_u32(context: &'static str, value: i64) -> Result<u32, AppError> {
    u32::try_from(value).map_err(|_| {
        AppError::new(AppErrorCode::InternalError, format!("{context} is out of range"))
    })
}

/// Render a stored instant as the RFC 3339 string the contract requires.
pub fn rfc3339(context: &'static str, millis: i64) -> Result<String, AppError> {
    Timestamp::from_millis(millis).to_rfc3339().ok_or_else(|| {
        AppError::new(AppErrorCode::InternalError, format!("{context} is out of range"))
    })
}

/// Render an optional stored instant.
pub fn rfc3339_opt(context: &'static str, millis: Option<i64>) -> Result<Option<String>, AppError> {
    millis.map(|value| rfc3339(context, value)).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opening_twice_is_idempotent_and_records_one_version_row() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM desktop_schema_version")
            .fetch_one(first.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
        drop(first);

        let second = Store::open(dir.path()).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM desktop_schema_version")
            .fetch_one(second.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn the_schema_contains_every_expected_table_and_no_schema_migrations() {
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
            "recording_media",
            "transcript_revisions",
            "transcript_segments",
            "jobs",
            "activity_events",
            "settings",
            "event_log",
            "desktop_schema_version",
        ] {
            assert!(names.iter().any(|name| name == expected), "missing table {expected}");
        }
        assert!(
            !names.iter().any(|name| name == "schema_migrations"),
            "schema_migrations belongs to crates/persistence"
        );
    }

    #[tokio::test]
    async fn wal_and_foreign_keys_are_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let journal: String =
            sqlx::query_scalar("PRAGMA journal_mode").fetch_one(store.pool()).await.unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
        let foreign_keys: i64 =
            sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(store.pool()).await.unwrap();
        assert_eq!(foreign_keys, 1);
    }

    #[tokio::test]
    async fn a_tampered_checksum_is_reported_as_a_failed_migration() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        sqlx::query("UPDATE desktop_schema_version SET checksum = 'deadbeef'")
            .execute(store.pool())
            .await
            .unwrap();
        drop(store);
        let err = Store::open(dir.path()).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::MigrationFailed);
    }

    #[test]
    fn statement_splitting_drops_comments_and_blank_fragments() {
        let statements = split_statements("-- c\nCREATE TABLE a (x INTEGER);\n\n-- d\n");
        assert_eq!(statements, vec!["CREATE TABLE a (x INTEGER)".to_owned()]);
    }
}
