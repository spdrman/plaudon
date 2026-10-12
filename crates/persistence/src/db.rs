//! Connection management with the pragmas spec 13.1 requires on every
//! connection: enforced foreign keys, write-ahead logging and a busy timeout.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::error::{PersistenceError, PersistenceResult};

/// Begin a write transaction on `pool`.
///
/// Every transaction that will write **must** start here rather than at
/// `pool.begin()`. `BEGIN` is deferred: it takes a read snapshot, and the
/// first write statement then has to upgrade that snapshot to the write lock.
/// SQLite refuses the upgrade with `SQLITE_BUSY` the instant any other
/// connection committed since the snapshot was taken, and it does *not*
/// consult the busy handler for that case, so `busy_timeout` cannot absorb
/// it — the statement simply fails. `BEGIN IMMEDIATE` takes the write lock at
/// the start, where the busy handler does apply, so concurrent writers queue
/// up for `busy_timeout` instead of failing a transaction that had already
/// read.
pub async fn begin_write(pool: &SqlitePool) -> Result<Transaction<'static, Sqlite>, sqlx::Error> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

/// An open library database.
#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
    path: PathBuf,
}

impl Database {
    /// Busy timeout applied to every connection.
    pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

    /// Open, creating the file when absent.
    pub async fn open(path: impl AsRef<Path>) -> PersistenceResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(|e| PersistenceError::StorageUnavailable(Box::new(e)))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Self::BUSY_TIMEOUT);

        // A WAL database serializes writers anyway; one writer plus concurrent
        // readers is the single-writer coordinator shape spec 6.3A asks for.
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        Ok(Self { pool, path })
    }

    /// The connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Begin a write transaction (see [`begin_write`]).
    pub async fn begin_write(&self) -> PersistenceResult<Transaction<'static, Sqlite>> {
        Ok(begin_write(&self.pool).await?)
    }

    /// Path of the database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Close the pool, releasing the WAL files.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Current journal mode, as reported by the engine.
    pub async fn journal_mode(&self) -> PersistenceResult<String> {
        let row = sqlx::query("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<String, _>(0))
    }

    /// Whether foreign keys are enforced on a freshly checked-out connection.
    pub async fn foreign_keys_enabled(&self) -> PersistenceResult<bool> {
        let row = sqlx::query("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>(0) == 1)
    }

    /// Write a consistent copy of the database to `destination`.
    ///
    /// `VACUUM INTO` is used rather than a file copy, so a backup taken while
    /// the WAL has uncheckpointed frames is still a valid database.
    pub async fn backup_to(&self, destination: impl AsRef<Path>) -> PersistenceResult<()> {
        let destination = destination.as_ref();
        if destination.exists() {
            std::fs::remove_file(destination)?;
        }
        let quoted = destination.display().to_string().replace('\'', "''");
        sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{quoted}'")))
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_pooled_connection_has_wal_and_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();

        // Check repeatedly so a connection created later in the pool's life is
        // covered too, not only the first one.
        for _ in 0..8 {
            assert_eq!(db.journal_mode().await.unwrap().to_ascii_lowercase(), "wal");
            assert!(db.foreign_keys_enabled().await.unwrap());
        }
        db.close().await;
    }

    #[tokio::test]
    async fn a_backup_is_a_valid_standalone_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
        sqlx::query("CREATE TABLE t (a TEXT) STRICT")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO t VALUES ('x')")
            .execute(db.pool())
            .await
            .unwrap();

        let backup = dir.path().join("backup.sqlite");
        db.backup_to(&backup).await.unwrap();
        db.close().await;

        let restored = Database::open(&backup).await.unwrap();
        let row = sqlx::query("SELECT a FROM t")
            .fetch_one(restored.pool())
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>(0), "x");
        restored.close().await;
    }
}
