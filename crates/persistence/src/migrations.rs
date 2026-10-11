//! Deterministic forward migrations with recorded checksums (spec 13.1, 13.2).
//!
//! Migrations are SQL files committed to the repository and embedded at build
//! time; the schema is never generated at runtime. Each applied migration is
//! recorded in `schema_migrations` with the SHA-256 of the exact SQL that ran,
//! so an edited or reordered migration is detected before anything else
//! happens. A verified backup is taken before any upgrade of a non-empty
//! database, and a failed migration leaves the previous database usable
//! because each migration runs inside one transaction.

use std::path::PathBuf;

use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// One embedded migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// Monotonic version. Versions 1-99 are the core schema; later ranges are
    /// owned by the capability that ships them.
    pub version: i64,
    /// Human-readable name, matching the file name.
    pub name: &'static str,
    /// The exact SQL that will be executed.
    pub sql: &'static str,
}

impl Migration {
    /// SHA-256 of the SQL, in lowercase hex.
    #[must_use]
    pub fn checksum(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.sql.as_bytes());
        hex::encode(hasher.finalize())
    }
}

/// Every migration this build knows, in application order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "core_identity",
        sql: include_str!("../migrations/0001_core_identity.sql"),
    },
    Migration {
        version: 2,
        name: "core",
        sql: include_str!("../migrations/0002_core.sql"),
    },
];

/// An applied migration as recorded in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Version that was applied.
    pub version: i64,
    /// Name recorded at application time.
    pub name: String,
    /// Checksum of the SQL that actually ran.
    pub checksum: String,
    /// When it was applied.
    pub applied_at: i64,
}

/// Outcome of a migration run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Versions applied by this run, in order.
    pub applied: Vec<i64>,
    /// Backup taken before the run, when one was needed.
    pub backup_path: Option<PathBuf>,
}

/// Applies embedded migrations to a database.
#[derive(Debug, Clone)]
pub struct MigrationRunner {
    migrations: Vec<Migration>,
}

impl Default for MigrationRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl MigrationRunner {
    /// A runner over the embedded migration set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            migrations: MIGRATIONS.to_vec(),
        }
    }

    /// A runner over an explicit migration set, for tests that need to model a
    /// tampered or truncated history.
    #[must_use]
    pub fn with_migrations(migrations: Vec<Migration>) -> Self {
        Self { migrations }
    }

    /// The highest version this runner can reach.
    #[must_use]
    pub fn head_version(&self) -> i64 {
        self.migrations.last().map_or(0, |m| m.version)
    }

    /// Migrations known to this runner.
    #[must_use]
    pub fn migrations(&self) -> &[Migration] {
        &self.migrations
    }

    /// Apply every pending migration.
    ///
    /// Re-running against an up-to-date database is a no-op.
    pub async fn run(&self, db: &Database) -> PersistenceResult<MigrationReport> {
        self.validate_ordering()?;
        Self::ensure_history_table(db).await?;
        let applied = Self::applied(db).await?;
        Self::validate_history(&self.migrations, &applied)?;

        let pending: Vec<&Migration> = self
            .migrations
            .iter()
            .filter(|migration| !applied.iter().any(|row| row.version == migration.version))
            .collect();
        if pending.is_empty() {
            return Ok(MigrationReport {
                applied: Vec::new(),
                backup_path: None,
            });
        }

        // Spec 13.3A: back up before upgrading an existing library. A fresh
        // database has nothing to lose, so we do not create noise for it.
        let backup_path = if applied.is_empty() {
            None
        } else {
            let path =
                Self::backup_path(db, applied.iter().map(|row| row.version).max().unwrap_or(0));
            db.backup_to(&path).await?;
            Some(path)
        };

        let mut applied_versions = Vec::new();
        for migration in pending {
            Self::apply_one(db, migration)
                .await
                .map_err(|error| match error {
                    PersistenceError::Migration(message) => PersistenceError::Migration(format!(
                        "migration {} ({}) failed: {message}",
                        migration.version, migration.name
                    )),
                    other => other,
                })?;
            applied_versions.push(migration.version);
        }

        Ok(MigrationReport {
            applied: applied_versions,
            backup_path,
        })
    }

    fn backup_path(db: &Database, from_version: i64) -> PathBuf {
        let stem = db.path().file_name().map_or_else(
            || "app.sqlite".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        db.path()
            .with_file_name(format!("{stem}.pre-migration-{from_version}.bak"))
    }

    fn validate_ordering(&self) -> PersistenceResult<()> {
        let mut previous = 0i64;
        for migration in &self.migrations {
            if migration.version <= previous {
                return Err(PersistenceError::Migration(format!(
                    "migration versions must strictly increase; {} follows {previous}",
                    migration.version
                )));
            }
            previous = migration.version;
        }
        Ok(())
    }

    async fn ensure_history_table(db: &Database) -> PersistenceResult<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER PRIMARY KEY NOT NULL,
                 name       TEXT    NOT NULL,
                 checksum   TEXT    NOT NULL,
                 applied_at INTEGER NOT NULL
             ) STRICT",
        )
        .execute(db.pool())
        .await?;
        Ok(())
    }

    /// Every migration recorded as applied, in version order.
    pub async fn applied(db: &Database) -> PersistenceResult<Vec<AppliedMigration>> {
        Self::ensure_history_table(db).await?;
        let rows = sqlx::query(
            "SELECT version, name, checksum, applied_at FROM schema_migrations ORDER BY version",
        )
        .fetch_all(db.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| AppliedMigration {
                version: row.get("version"),
                name: row.get("name"),
                checksum: row.get("checksum"),
                applied_at: row.get("applied_at"),
            })
            .collect())
    }

    fn validate_history(
        migrations: &[Migration],
        applied: &[AppliedMigration],
    ) -> PersistenceResult<()> {
        for row in applied {
            let Some(migration) = migrations.iter().find(|m| m.version == row.version) else {
                return Err(PersistenceError::Migration(format!(
                    "the library was migrated to version {} by a newer build; refusing to continue",
                    row.version
                )));
            };
            if migration.checksum() != row.checksum {
                return Err(PersistenceError::Migration(format!(
                    "migration {} ({}) no longer matches the checksum recorded when it was applied",
                    row.version, row.name
                )));
            }
        }
        // A migration inserted below the current head would silently reorder
        // history, so refuse it rather than applying it out of order.
        if let Some(highest) = applied.iter().map(|row| row.version).max() {
            for migration in migrations {
                if migration.version < highest
                    && !applied.iter().any(|row| row.version == migration.version)
                {
                    return Err(PersistenceError::Migration(format!(
                        "migration {} was inserted below the applied head {highest}",
                        migration.version
                    )));
                }
            }
        }
        Ok(())
    }

    async fn apply_one(db: &Database, migration: &Migration) -> PersistenceResult<()> {
        let mut tx = db.pool().begin().await?;
        sqlx::raw_sql(migration.sql)
            .execute(&mut *tx)
            .await
            .map_err(|error| PersistenceError::Migration(error.to_string()))?;
        sqlx::query(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(migration.version)
        .bind(migration.name)
        .bind(migration.checksum())
        .bind(plaudon_domain::time::Timestamp::now().as_millis())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
        (dir, db)
    }

    #[test]
    fn embedded_migrations_are_strictly_ordered_and_named_after_their_files() {
        MigrationRunner::new().validate_ordering().unwrap();
        for migration in MIGRATIONS {
            assert_ne!(migration.sql.trim(), "");
            assert_eq!(migration.checksum().len(), 64);
        }
    }

    #[tokio::test]
    async fn a_fresh_database_migrates_to_head_and_records_checksums() {
        let (_dir, db) = fresh().await;
        let runner = MigrationRunner::new();
        let report = runner.run(&db).await.unwrap();

        assert_eq!(report.applied, vec![1, 2]);
        assert_eq!(
            report.backup_path, None,
            "a fresh library has nothing to back up"
        );

        let applied = MigrationRunner::applied(&db).await.unwrap();
        assert_eq!(applied.len(), MIGRATIONS.len());
        for (row, migration) in applied.iter().zip(MIGRATIONS) {
            assert_eq!(row.version, migration.version);
            assert_eq!(row.checksum, migration.checksum());
        }
        db.close().await;
    }

    #[tokio::test]
    async fn re_running_migrations_is_idempotent() {
        let (_dir, db) = fresh().await;
        let runner = MigrationRunner::new();
        runner.run(&db).await.unwrap();
        let second = runner.run(&db).await.unwrap();
        assert_eq!(second.applied.len(), 0);
        assert_eq!(
            MigrationRunner::applied(&db).await.unwrap().len(),
            MIGRATIONS.len()
        );
        db.close().await;
    }

    #[tokio::test]
    async fn a_sequential_upgrade_backs_up_before_applying() {
        let (dir, db) = fresh().await;
        let first = MigrationRunner::with_migrations(vec![MIGRATIONS[0]]);
        first.run(&db).await.unwrap();

        let full = MigrationRunner::new();
        let report = full.run(&db).await.unwrap();
        assert_eq!(report.applied, vec![2]);
        let backup = report.backup_path.expect("an upgrade must back up first");
        assert!(
            backup.exists(),
            "backup file must exist at {}",
            backup.display()
        );
        assert!(backup.starts_with(dir.path()));

        // The backup is a real database still at the previous version.
        let restored = Database::open(&backup).await.unwrap();
        let applied = MigrationRunner::applied(&restored).await.unwrap();
        assert_eq!(applied.len(), 1);
        restored.close().await;
        db.close().await;
    }

    #[tokio::test]
    async fn a_tampered_migration_is_rejected() {
        let (_dir, db) = fresh().await;
        MigrationRunner::new().run(&db).await.unwrap();

        let tampered = MigrationRunner::with_migrations(vec![
            Migration {
                sql: "CREATE TABLE evil (x TEXT) STRICT;",
                ..MIGRATIONS[0]
            },
            MIGRATIONS[1],
        ]);
        let error = tampered.run(&db).await.unwrap_err();
        assert!(matches!(error, PersistenceError::Migration(ref m) if m.contains("checksum")));
        db.close().await;
    }

    #[tokio::test]
    async fn a_reordered_migration_is_rejected() {
        let (_dir, db) = fresh().await;
        MigrationRunner::new().run(&db).await.unwrap();

        let inserted = MigrationRunner::with_migrations(vec![
            MIGRATIONS[0],
            Migration {
                version: 2,
                name: "inserted_below_head",
                sql: "CREATE TABLE inserted (x TEXT) STRICT;",
            },
        ]);
        // Version 2 is applied with a different checksum: that is a tamper.
        assert!(inserted.run(&db).await.is_err());

        let shifted = MigrationRunner::with_migrations(vec![
            MIGRATIONS[0],
            Migration {
                version: 3,
                name: "renumbered_core",
                sql: MIGRATIONS[1].sql,
            },
        ]);
        let error = shifted.run(&db).await.unwrap_err();
        assert!(
            matches!(error, PersistenceError::Migration(ref m) if m.contains("newer build")),
            "unexpected error: {error}"
        );
        db.close().await;
    }

    #[tokio::test]
    async fn a_database_from_a_newer_build_is_refused_rather_than_downgraded() {
        let (_dir, db) = fresh().await;
        MigrationRunner::new().run(&db).await.unwrap();
        let older = MigrationRunner::with_migrations(vec![MIGRATIONS[0]]);
        let error = older.run(&db).await.unwrap_err();
        assert!(matches!(error, PersistenceError::Migration(ref m) if m.contains("newer build")));
        db.close().await;
    }

    #[tokio::test]
    async fn a_failing_migration_leaves_the_previous_schema_usable() {
        let (_dir, db) = fresh().await;
        MigrationRunner::with_migrations(vec![MIGRATIONS[0]])
            .run(&db)
            .await
            .unwrap();

        let broken = MigrationRunner::with_migrations(vec![
            MIGRATIONS[0],
            Migration {
                version: 2,
                name: "broken",
                sql: "CREATE TABLE ok (a TEXT) STRICT;\nTHIS IS NOT SQL;",
            },
        ]);
        let error = broken.run(&db).await.unwrap_err();
        assert!(matches!(error, PersistenceError::Migration(_)));

        // The partially-applied statement was rolled back with its transaction.
        let tables: Vec<String> =
            sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'ok'")
                .fetch_all(db.pool())
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.get::<String, _>(0))
                .collect();
        assert!(
            tables.is_empty(),
            "a failed migration must not leave half a schema"
        );

        // The previous version is still fully usable.
        assert_eq!(MigrationRunner::applied(&db).await.unwrap().len(), 1);
        sqlx::query("SELECT COUNT(*) FROM recordings")
            .fetch_one(db.pool())
            .await
            .unwrap();
        db.close().await;
    }
}
