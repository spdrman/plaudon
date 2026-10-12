//! Deletion journalling and durable suppression records (spec 4.6, 7.10, 13.5).
//!
//! A user-visible deletion is a multi-resource operation: SQLite rows and
//! filesystem blobs. SQLite cannot commit a file unlink, so the same journal
//! shape the importer uses applies in reverse. A deletion row is written and
//! committed *before* anything is removed, every blob the deletion is
//! responsible for is listed, and startup reconciliation finishes any deletion
//! that did not reach `completed`. A half-finished deletion therefore never
//! presents itself as an intact recording.
//!
//! Suppressions are the spec 7.10 tombstone in its matching form. They are
//! keyed by content identity and by stable source identity, never by filename,
//! so a later sync that offers the same bytes or the same external id is
//! refused until the user explicitly reimports.

use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use plaudon_domain::time::Timestamp;
use sqlx::Row;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// Stage a journalled deletion has reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeletionState {
    /// The disclosure was confirmed and the journal row committed. Nothing has
    /// been removed yet.
    Planned,
    /// Dependent database rows are gone; blobs may still be on disk.
    RowsRemoved,
    /// Every blob listed by the deletion has been unlinked.
    MediaRemoved,
    /// The deletion finished and its tombstone is durable.
    Completed,
}

impl DeletionState {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::RowsRemoved => "rows_removed",
            Self::MediaRemoved => "media_removed",
            Self::Completed => "completed",
        }
    }

    fn parse(raw: &str) -> PersistenceResult<Self> {
        match raw {
            "planned" => Ok(Self::Planned),
            "rows_removed" => Ok(Self::RowsRemoved),
            "media_removed" => Ok(Self::MediaRemoved),
            "completed" => Ok(Self::Completed),
            other => Err(PersistenceError::Migration(format!(
                "unknown deletion state {other}"
            ))),
        }
    }
}

/// A journalled deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionRecord {
    /// Journal row identity.
    pub id: String,
    /// Recording the deletion applies to.
    pub recording_id: RecordingId,
    /// User-visible reason recorded with the tombstone.
    pub reason: String,
    /// Stage reached.
    pub state: DeletionState,
    /// The exact disclosure payload the user confirmed, as JSON text.
    pub disclosure_json: String,
    /// When the deletion was confirmed.
    pub created_at: Timestamp,
    /// When the deletion finished.
    pub completed_at: Option<Timestamp>,
}

/// A blob a deletion is still responsible for removing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBlob {
    /// Archive-relative path of the blob.
    pub relative_path: String,
    /// Content identity of the blob.
    pub identity: MediaIdentity,
    /// When the blob was unlinked, if it already was.
    pub removed_at: Option<Timestamp>,
}

/// Reads and writes for the deletion journal and suppression tables.
#[derive(Debug, Clone)]
pub struct DeletionRepository {
    db: Database,
}

impl DeletionRepository {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Commit the journal row and the blob worklist in one transaction.
    ///
    /// Nothing is removed by this call. It exists so that a crash immediately
    /// afterwards still leaves a record of what the user authorized.
    pub async fn open(
        &self,
        record: &DeletionRecord,
        blobs: &[PendingBlob],
    ) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "INSERT INTO deletion_records
                 (id, recording_id, reason, state, disclosure_json, created_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
        )
        .bind(&record.id)
        .bind(record.recording_id.to_string())
        .bind(&record.reason)
        .bind(DeletionState::Planned.as_str())
        .bind(&record.disclosure_json)
        .bind(record.created_at.as_millis())
        .execute(&mut *tx)
        .await?;
        for blob in blobs {
            sqlx::query(
                "INSERT INTO deletion_pending_blobs
                     (deletion_id, relative_path, sha256, byte_length, removed_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
            )
            .bind(&record.id)
            .bind(&blob.relative_path)
            .bind(blob.identity.sha256.to_hex())
            .bind(i64::try_from(blob.identity.byte_length).unwrap_or(i64::MAX))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Advance the journal stage.
    pub async fn set_state(&self, id: &str, state: DeletionState) -> PersistenceResult<()> {
        let affected = sqlx::query("UPDATE deletion_records SET state = ?2 WHERE id = ?1")
            .bind(id)
            .bind(state.as_str())
            .execute(self.db.pool())
            .await?
            .rows_affected();
        if affected == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        Ok(())
    }

    /// Record that a blob was unlinked.
    pub async fn mark_blob_removed(
        &self,
        id: &str,
        relative_path: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "UPDATE deletion_pending_blobs SET removed_at = ?3
             WHERE deletion_id = ?1 AND relative_path = ?2",
        )
        .bind(id)
        .bind(relative_path)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Mark the deletion finished.
    pub async fn complete(&self, id: &str, at: Timestamp) -> PersistenceResult<()> {
        let affected = sqlx::query(
            "UPDATE deletion_records SET state = 'completed', completed_at = ?2 WHERE id = ?1",
        )
        .bind(id)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        Ok(())
    }

    /// Deletions that did not reach `completed`, oldest first.
    pub async fn unfinished(&self) -> PersistenceResult<Vec<DeletionRecord>> {
        let rows = sqlx::query(
            "SELECT id, recording_id, reason, state, disclosure_json, created_at, completed_at
             FROM deletion_records WHERE state <> 'completed' ORDER BY created_at, id",
        )
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::record_from_row).collect()
    }

    /// Fetch one deletion record.
    pub async fn get(&self, id: &str) -> PersistenceResult<DeletionRecord> {
        let row = sqlx::query(
            "SELECT id, recording_id, reason, state, disclosure_json, created_at, completed_at
             FROM deletion_records WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PersistenceError::RecordNotFound)?;
        Self::record_from_row(&row)
    }

    /// Blobs listed by a deletion, including ones already removed.
    pub async fn blobs(&self, id: &str) -> PersistenceResult<Vec<PendingBlob>> {
        let rows = sqlx::query(
            "SELECT relative_path, sha256, byte_length, removed_at
             FROM deletion_pending_blobs WHERE deletion_id = ?1 ORDER BY relative_path",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let sha: String = row.get("sha256");
                let length: i64 = row.get("byte_length");
                Ok(PendingBlob {
                    relative_path: row.get("relative_path"),
                    identity: MediaIdentity::new(
                        sha.parse::<Sha256Digest>()
                            .map_err(|_| PersistenceError::IntegrityFailed)?,
                        u64::try_from(length).unwrap_or(0),
                        String::new(),
                    ),
                    removed_at: row
                        .get::<Option<i64>, _>("removed_at")
                        .map(Timestamp::from_millis),
                })
            })
            .collect()
    }

    /// Record a content-identity suppression.
    pub async fn suppress_media(
        &self,
        identity: &MediaIdentity,
        deletion_id: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO deletion_media_suppressions (sha256, byte_length, deletion_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(sha256, byte_length) DO UPDATE SET
                 deletion_id = excluded.deletion_id, created_at = excluded.created_at",
        )
        .bind(identity.sha256.to_hex())
        .bind(i64::try_from(identity.byte_length).unwrap_or(i64::MAX))
        .bind(deletion_id)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Record a stable-source-identity suppression.
    pub async fn suppress_source_item(
        &self,
        source_id: &str,
        external_id: &str,
        deletion_id: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO deletion_source_suppressions (source_id, external_id, deletion_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_id, external_id) DO UPDATE SET
                 deletion_id = excluded.deletion_id, created_at = excluded.created_at",
        )
        .bind(source_id)
        .bind(external_id)
        .bind(deletion_id)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Whether these exact bytes were deleted by the user.
    pub async fn is_media_suppressed(&self, identity: &MediaIdentity) -> PersistenceResult<bool> {
        let row = sqlx::query(
            "SELECT 1 FROM deletion_media_suppressions WHERE sha256 = ?1 AND byte_length = ?2",
        )
        .bind(identity.sha256.to_hex())
        .bind(i64::try_from(identity.byte_length).unwrap_or(i64::MAX))
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    /// Whether this stable source identity was deleted by the user.
    pub async fn is_source_item_suppressed(
        &self,
        source_id: &str,
        external_id: &str,
    ) -> PersistenceResult<bool> {
        let row = sqlx::query(
            "SELECT 1 FROM deletion_source_suppressions WHERE source_id = ?1 AND external_id = ?2",
        )
        .bind(source_id)
        .bind(external_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    /// Drop every suppression created by a deletion.
    ///
    /// This is the explicit user "reimport" action from spec 7.10. Nothing
    /// else may clear a suppression.
    pub async fn clear_suppressions(&self, deletion_id: &str) -> PersistenceResult<u64> {
        let mut tx = self.db.pool().begin().await?;
        let media = sqlx::query("DELETE FROM deletion_media_suppressions WHERE deletion_id = ?1")
            .bind(deletion_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let source = sqlx::query("DELETE FROM deletion_source_suppressions WHERE deletion_id = ?1")
            .bind(deletion_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(media + source)
    }

    /// Deletions recorded against a recording, newest first.
    pub async fn for_recording(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<DeletionRecord>> {
        let rows = sqlx::query(
            "SELECT id, recording_id, reason, state, disclosure_json, created_at, completed_at
             FROM deletion_records WHERE recording_id = ?1 ORDER BY created_at DESC, id DESC",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::record_from_row).collect()
    }

    fn record_from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<DeletionRecord> {
        Ok(DeletionRecord {
            id: row.get("id"),
            recording_id: row
                .get::<String, _>("recording_id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt recording id".into()))?,
            reason: row.get("reason"),
            state: DeletionState::parse(&row.get::<String, _>("state"))?,
            disclosure_json: row.get("disclosure_json"),
            created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
            completed_at: row
                .get::<Option<i64>, _>("completed_at")
                .map(Timestamp::from_millis),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::MigrationRunner;

    async fn db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
        MigrationRunner::new().run(&db).await.unwrap();
        (dir, db)
    }

    fn record(id: &str) -> DeletionRecord {
        DeletionRecord {
            id: id.to_owned(),
            recording_id: RecordingId::new(),
            reason: "user_requested".into(),
            state: DeletionState::Planned,
            disclosure_json: "{}".into(),
            created_at: Timestamp::from_millis(1),
            completed_at: None,
        }
    }

    fn blob(byte: u8) -> PendingBlob {
        PendingBlob {
            relative_path: format!("media/sha256/{byte:02x}/blob.wav"),
            identity: MediaIdentity::new(Sha256Digest::from_bytes([byte; 32]), 10, String::new()),
            removed_at: None,
        }
    }

    #[tokio::test]
    async fn an_unfinished_deletion_is_visible_to_the_reconciler() {
        let (_dir, db) = db().await;
        let repo = DeletionRepository::new(db.clone());
        let row = record("d1");
        repo.open(&row, &[blob(1)]).await.unwrap();

        let unfinished = repo.unfinished().await.unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].state, DeletionState::Planned);

        repo.set_state("d1", DeletionState::RowsRemoved).await.unwrap();
        repo.mark_blob_removed("d1", &blob(1).relative_path, Timestamp::from_millis(2))
            .await
            .unwrap();
        repo.complete("d1", Timestamp::from_millis(3)).await.unwrap();
        assert!(repo.unfinished().await.unwrap().is_empty());
        assert_eq!(
            repo.get("d1").await.unwrap().completed_at,
            Some(Timestamp::from_millis(3))
        );
        db.close().await;
    }

    #[tokio::test]
    async fn suppressions_match_by_content_and_source_identity_only() {
        let (_dir, db) = db().await;
        let repo = DeletionRepository::new(db.clone());
        repo.open(&record("d2"), &[]).await.unwrap();

        let identity = MediaIdentity::new(Sha256Digest::of(b"bytes"), 5, "wav");
        repo.suppress_media(&identity, "d2", Timestamp::from_millis(1))
            .await
            .unwrap();
        repo.suppress_source_item("src", "ext-1", "d2", Timestamp::from_millis(1))
            .await
            .unwrap();

        assert!(repo.is_media_suppressed(&identity).await.unwrap());
        // Same digest, different length is a different identity.
        let other = MediaIdentity::new(Sha256Digest::of(b"bytes"), 6, "wav");
        assert!(!repo.is_media_suppressed(&other).await.unwrap());
        assert!(repo.is_source_item_suppressed("src", "ext-1").await.unwrap());
        assert!(!repo.is_source_item_suppressed("src", "ext-2").await.unwrap());

        assert_eq!(repo.clear_suppressions("d2").await.unwrap(), 2);
        assert!(!repo.is_media_suppressed(&identity).await.unwrap());
        assert!(!repo.is_source_item_suppressed("src", "ext-1").await.unwrap());
        db.close().await;
    }

    #[tokio::test]
    async fn completing_an_unknown_deletion_is_a_missing_record() {
        let (_dir, db) = db().await;
        let repo = DeletionRepository::new(db.clone());
        let error = repo.complete("nope", Timestamp::from_millis(1)).await.unwrap_err();
        assert!(matches!(error, PersistenceError::RecordNotFound));
        let error = repo
            .set_state("nope", DeletionState::RowsRemoved)
            .await
            .unwrap_err();
        assert!(matches!(error, PersistenceError::RecordNotFound));
        db.close().await;
    }
}
