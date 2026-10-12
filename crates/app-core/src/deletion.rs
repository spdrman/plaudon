//! Deleting a recording: disclosure, journal, tombstone (spec 4.6, 7.10, 13.5).
//!
//! Deletion is the one destructive operation the product performs on user
//! data, so it is built the same way the importer is: as a journalled
//! sequence that can be finished after a crash, never as a single statement
//! whose failure leaves a half-deleted recording looking intact.
//!
//! ```text
//! disclose -> user confirms the exact disclosure -> journal (planned)
//!   -> one transaction: remove rows, write tombstone and suppressions
//!   -> unlink blobs that no other recording references
//!   -> complete
//! ```
//!
//! Two honesty rules are enforced in code rather than in copy:
//!
//! * The confirmation carries the **digest of the disclosure the user saw**.
//!   If the library changed between showing the list and pressing delete, the
//!   digest no longer matches and the deletion is refused.
//! * The disclosure says what we cannot do: exports already written elsewhere,
//!   user backups, synchronized folders, OS snapshots and anything already
//!   sent to a provider are outside our reach, and we do not claim forensic
//!   erasure on an SSD.

use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::deletion::{
    DeletionRecord, DeletionRepository, DeletionState, PendingBlob,
};
use plaudon_persistence::{Database, PersistenceError, PersistenceResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

/// Version of the disclosure contract, recorded with every deletion.
pub const DISCLOSURE_VERSION: &str = "deletion-disclosure/1";

/// What we cannot undo or reach, stated plainly (spec 4.6, 13.5).
pub const DELETION_LIMITS: &[&str] = &[
    "Exports you already saved outside this application are not touched. Delete those files yourself if you want them gone.",
    "Copies in your own backups, portable archives, synchronized folders or operating-system snapshots are not touched.",
    "Anything already sent to a cloud provider is outside our control. Deleting it here does not delete it there.",
    "We remove the files and database rows listed above and ask the operating system to free the space. On an SSD, in a snapshotted filesystem or on a backed-up volume we cannot guarantee the bytes are unrecoverable.",
    "A tombstone is kept so a later import or synchronization does not bring this recording back by itself. It records identifiers and hashes, never the deleted content.",
];

/// One media blob the deletion will or will not remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosedMedia {
    /// Digest of the stored bytes.
    pub sha256: String,
    /// Length of the stored bytes.
    pub byte_length: u64,
    /// Archive-relative path.
    pub relative_path: String,
    /// Honest fidelity label.
    pub quality_class: String,
    /// Whether another recording also references these exact bytes.
    pub shared_with_other_recordings: bool,
    /// Whether the bytes themselves will be removed from disk.
    pub bytes_will_be_removed: bool,
}

/// Row counts the deletion will remove, by category (spec 4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisclosedCounts {
    /// Transcript revisions.
    pub transcript_revisions: u64,
    /// Transcript segments.
    pub transcript_segments: u64,
    /// Word alignments.
    pub transcript_words: u64,
    /// Quality annotations.
    pub transcript_annotations: u64,
    /// Generated outputs.
    pub ai_outputs: u64,
    /// Evidence anchors.
    pub output_evidence: u64,
    /// Speaker labels.
    pub speakers: u64,
    /// Tags.
    pub tags: u64,
    /// Source associations.
    pub source_items: u64,
    /// Media revisions.
    pub media_revisions: u64,
    /// Export event records.
    pub export_events: u64,
    /// Sanitized derivative records.
    pub export_derivatives: u64,
    /// Activity history entries.
    pub activity_events: u64,
}

/// Everything the user must see before confirming (spec 4.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionDisclosure {
    /// Contract version.
    pub disclosure_version: String,
    /// Recording that will be deleted.
    pub recording_id: RecordingId,
    /// Title as currently stored.
    pub title: String,
    /// Whether the recording carries a user note that will be removed.
    pub has_annotation: bool,
    /// Audio that will be removed, or kept because it is shared.
    pub media: Vec<DisclosedMedia>,
    /// Row counts by category.
    pub counts: DisclosedCounts,
    /// Bytes that will actually be freed.
    pub bytes_to_free: u64,
    /// What this deletion cannot reach.
    pub limits: Vec<String>,
}

impl DeletionDisclosure {
    /// Digest of the disclosure, as the user saw it.
    ///
    /// The confirmation must carry this value, so a library that changed
    /// between disclosure and confirmation fails closed.
    #[must_use]
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        hex::encode(Sha256::digest(canonical))
    }
}

/// The user's confirmation of a specific disclosure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionConfirmation {
    /// Digest of the disclosure that was shown.
    pub disclosure_digest: String,
    /// Reason recorded with the tombstone.
    pub reason: String,
}

/// What a completed deletion did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionReceipt {
    /// Journal row identity.
    pub deletion_id: String,
    /// Recording that was deleted.
    pub recording_id: RecordingId,
    /// Blobs unlinked from the archive.
    pub blobs_removed: u64,
    /// Blobs kept because another recording references them.
    pub blobs_retained_shared: u64,
    /// Bytes freed.
    pub bytes_freed: u64,
    /// Whether a durable tombstone exists.
    pub tombstoned: bool,
}

/// A deletion whose journal row is committed and whose disclosure the user
/// confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedDeletion {
    /// Journal row identity.
    pub deletion_id: String,
    /// Recording being deleted.
    pub recording_id: RecordingId,
    /// The disclosure the user confirmed.
    pub disclosure: DeletionDisclosure,
}

/// Why a deletion could not proceed.
#[derive(Debug, thiserror::Error)]
pub enum DeletionError {
    /// The recording does not exist.
    #[error("the requested record does not exist")]
    RecordNotFound,
    /// The confirmation did not match the disclosure.
    #[error("deletion requires confirmation of the exact disclosure that was shown")]
    ConfirmationMismatch,
    /// The library failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

impl From<DeletionError> for plaudon_domain::error::AppError {
    fn from(error: DeletionError) -> Self {
        use plaudon_domain::error::AppErrorCode;
        let code = match &error {
            DeletionError::RecordNotFound => AppErrorCode::RecordNotFound,
            DeletionError::ConfirmationMismatch => AppErrorCode::ConfirmationRequired,
            DeletionError::Persistence(inner) => inner.code(),
        };
        Self::new(code, error.to_string())
    }
}

type DeletionResult<T> = Result<T, DeletionError>;

/// Deletes recordings, with disclosure and durable tombstones.
#[derive(Debug, Clone)]
pub struct DeletionService {
    db: Database,
    archive: MediaArchive,
    journal: DeletionRepository,
}

impl DeletionService {
    /// Wrap a library.
    #[must_use]
    pub fn new(db: Database, archive: MediaArchive) -> Self {
        Self {
            journal: DeletionRepository::new(db.clone()),
            db,
            archive,
        }
    }

    /// Build the disclosure for a recording. Reads only.
    pub async fn disclose(&self, recording_id: RecordingId) -> DeletionResult<DeletionDisclosure> {
        let id = recording_id.to_string();
        let row = sqlx::query("SELECT title, annotation FROM recordings WHERE id = ?1")
            .bind(&id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(PersistenceError::from)?
            .ok_or(DeletionError::RecordNotFound)?;

        let media_rows = sqlx::query(
            "SELECT a.id, a.sha256, a.byte_length, a.relative_path, a.quality_class,
                    (SELECT COUNT(*) FROM recording_media other
                      WHERE other.media_asset_id = a.id AND other.recording_id <> ?1) AS others
             FROM recording_media rm
             JOIN media_assets a ON a.id = rm.media_asset_id
             WHERE rm.recording_id = ?1
             ORDER BY a.id",
        )
        .bind(&id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut media = Vec::with_capacity(media_rows.len());
        let mut bytes_to_free = 0u64;
        for row in &media_rows {
            let shared = row.get::<i64, _>("others") > 0;
            let byte_length = u64::try_from(row.get::<i64, _>("byte_length")).unwrap_or(0);
            if !shared {
                bytes_to_free += byte_length;
            }
            media.push(DisclosedMedia {
                sha256: row.get("sha256"),
                byte_length,
                relative_path: row.get("relative_path"),
                quality_class: row.get("quality_class"),
                shared_with_other_recordings: shared,
                bytes_will_be_removed: !shared,
            });
        }

        let counts = DisclosedCounts {
            transcript_revisions: self
                .count(
                    "SELECT COUNT(*) FROM transcript_revisions WHERE recording_id = ?1",
                    &id,
                )
                .await?,
            transcript_segments: self
                .count(
                    "SELECT COUNT(*) FROM transcript_segments s
                     JOIN transcript_revisions r ON r.id = s.transcript_revision_id
                     WHERE r.recording_id = ?1",
                    &id,
                )
                .await?,
            transcript_words: self
                .count(
                    "SELECT COUNT(*) FROM transcript_words w
                     JOIN transcript_segments s ON s.id = w.segment_id
                     JOIN transcript_revisions r ON r.id = s.transcript_revision_id
                     WHERE r.recording_id = ?1",
                    &id,
                )
                .await?,
            transcript_annotations: self
                .count(
                    "SELECT COUNT(*) FROM transcript_annotations a
                     JOIN transcript_revisions r ON r.id = a.transcript_revision_id
                     WHERE r.recording_id = ?1",
                    &id,
                )
                .await?,
            ai_outputs: self
                .count(
                    "SELECT COUNT(*) FROM ai_outputs WHERE recording_id = ?1",
                    &id,
                )
                .await?,
            output_evidence: self
                .count(
                    "SELECT COUNT(*) FROM output_evidence e
                     JOIN ai_outputs o ON o.id = e.output_id
                     WHERE o.recording_id = ?1",
                    &id,
                )
                .await?,
            speakers: self
                .count("SELECT COUNT(*) FROM speakers WHERE recording_id = ?1", &id)
                .await?,
            tags: self
                .count(
                    "SELECT COUNT(*) FROM recording_tags WHERE recording_id = ?1",
                    &id,
                )
                .await?,
            source_items: self
                .count(
                    "SELECT COUNT(*) FROM source_items WHERE recording_id = ?1",
                    &id,
                )
                .await?,
            media_revisions: self
                .count(
                    "SELECT COUNT(*) FROM media_revisions WHERE recording_id = ?1",
                    &id,
                )
                .await?,
            export_events: self
                .count(
                    "SELECT COUNT(*) FROM export_events WHERE recording_id = ?1",
                    &id,
                )
                .await?,
            export_derivatives: self
                .count(
                    "SELECT COUNT(*) FROM export_derivatives WHERE recording_id = ?1",
                    &id,
                )
                .await?,
            activity_events: self
                .count_optional(
                    "activity_events",
                    "SELECT COUNT(*) FROM activity_events
                      WHERE target_kind = 'recording' AND target_id = ?1",
                    &id,
                )
                .await?,
        };

        Ok(DeletionDisclosure {
            disclosure_version: DISCLOSURE_VERSION.to_owned(),
            recording_id,
            title: row.get("title"),
            has_annotation: row.get::<Option<String>, _>("annotation").is_some(),
            media,
            counts,
            bytes_to_free,
            limits: DELETION_LIMITS
                .iter()
                .map(|line| (*line).to_owned())
                .collect(),
        })
    }

    /// Delete a recording after the user confirmed its disclosure.
    ///
    /// This is [`Self::plan`], [`Self::remove_rows`], [`Self::unlink_blobs`]
    /// and [`Self::finish`] in order. The steps are public so a crash-
    /// injection test can stop between any two of them without the
    /// production path knowing the test exists.
    pub async fn delete(
        &self,
        recording_id: RecordingId,
        confirmation: &DeletionConfirmation,
        now: Timestamp,
    ) -> DeletionResult<DeletionReceipt> {
        let planned = self.plan(recording_id, confirmation, now).await?;
        self.remove_rows_for(&planned, now).await?;
        let (removed, freed) = self.unlink_blobs(&planned.deletion_id, now).await?;
        self.finish(&planned, removed, freed, now).await
    }

    /// Validate the confirmation and commit the deletion journal row.
    ///
    /// Nothing is removed yet. A crash straight after this call leaves a
    /// durable record of what the user authorized, which
    /// [`Self::reconcile`] finishes at the next start.
    pub async fn plan(
        &self,
        recording_id: RecordingId,
        confirmation: &DeletionConfirmation,
        now: Timestamp,
    ) -> DeletionResult<PlannedDeletion> {
        let disclosure = self.disclose(recording_id).await?;
        if disclosure.digest() != confirmation.disclosure_digest {
            return Err(DeletionError::ConfirmationMismatch);
        }

        let deletion_id = Uuid::now_v7().to_string();
        let blobs: Vec<PendingBlob> = disclosure
            .media
            .iter()
            .filter(|media| media.bytes_will_be_removed)
            .map(|media| PendingBlob {
                relative_path: media.relative_path.clone(),
                identity: MediaIdentity::new(
                    media
                        .sha256
                        .parse()
                        .unwrap_or(Sha256Digest::from_bytes([0; 32])),
                    media.byte_length,
                    String::new(),
                ),
                removed_at: None,
            })
            .collect();

        let record = DeletionRecord {
            id: deletion_id.clone(),
            recording_id,
            reason: confirmation.reason.clone(),
            state: DeletionState::Planned,
            disclosure_json: serde_json::to_string(&disclosure).unwrap_or_else(|_| "{}".to_owned()),
            created_at: now,
            completed_at: None,
        };
        self.journal.open(&record, &blobs).await?;

        Ok(PlannedDeletion {
            deletion_id,
            recording_id,
            disclosure,
        })
    }

    /// Remove every dependent row, write the tombstone and the suppressions.
    pub async fn remove_rows_for(
        &self,
        planned: &PlannedDeletion,
        now: Timestamp,
    ) -> DeletionResult<()> {
        self.remove_rows(
            &planned.deletion_id,
            planned.recording_id,
            &planned.disclosure,
            now,
        )
        .await?;
        self.journal
            .set_state(&planned.deletion_id, DeletionState::RowsRemoved)
            .await?;
        Ok(())
    }

    /// Mark the deletion finished and return its receipt.
    pub async fn finish(
        &self,
        planned: &PlannedDeletion,
        blobs_removed: u64,
        bytes_freed: u64,
        now: Timestamp,
    ) -> DeletionResult<DeletionReceipt> {
        self.journal
            .set_state(&planned.deletion_id, DeletionState::MediaRemoved)
            .await?;
        self.journal.complete(&planned.deletion_id, now).await?;
        Ok(DeletionReceipt {
            deletion_id: planned.deletion_id.clone(),
            recording_id: planned.recording_id,
            blobs_removed,
            blobs_retained_shared: planned
                .disclosure
                .media
                .iter()
                .filter(|media| media.shared_with_other_recordings)
                .count() as u64,
            bytes_freed,
            tombstoned: true,
        })
    }

    /// Finish deletions that a crash interrupted.
    ///
    /// Every step is idempotent, so re-running a deletion that already removed
    /// its rows simply removes nothing and proceeds to the blobs.
    pub async fn reconcile(&self, now: Timestamp) -> DeletionResult<Vec<DeletionReceipt>> {
        let mut receipts = Vec::new();
        for record in self.journal.unfinished().await? {
            let disclosure: DeletionDisclosure = serde_json::from_str(&record.disclosure_json)
                .map_err(|_| {
                    DeletionError::Persistence(PersistenceError::Migration(
                        "deletion disclosure is unreadable".into(),
                    ))
                })?;
            if record.state == DeletionState::Planned {
                self.remove_rows(&record.id, record.recording_id, &disclosure, now)
                    .await?;
                self.journal
                    .set_state(&record.id, DeletionState::RowsRemoved)
                    .await?;
            }
            let (removed, freed) = self.unlink_blobs(&record.id, now).await?;
            self.journal
                .set_state(&record.id, DeletionState::MediaRemoved)
                .await?;
            self.journal.complete(&record.id, now).await?;
            receipts.push(DeletionReceipt {
                deletion_id: record.id,
                recording_id: record.recording_id,
                blobs_removed: removed,
                blobs_retained_shared: 0,
                bytes_freed: freed,
                tombstoned: true,
            });
        }
        Ok(receipts)
    }

    /// Whether these exact bytes were deleted by the user (spec 7.10).
    pub async fn is_media_suppressed(&self, identity: &MediaIdentity) -> PersistenceResult<bool> {
        self.journal.is_media_suppressed(identity).await
    }

    /// Whether this stable source identity was deleted by the user.
    pub async fn is_source_item_suppressed(
        &self,
        source_id: &str,
        external_id: &str,
    ) -> PersistenceResult<bool> {
        self.journal
            .is_source_item_suppressed(source_id, external_id)
            .await
    }

    /// The explicit user action that allows a deleted recording to come back.
    pub async fn allow_reimport(&self, deletion_id: &str) -> PersistenceResult<u64> {
        self.journal.clear_suppressions(deletion_id).await
    }

    /// The deletion journal, for callers that need its records.
    #[must_use]
    pub fn journal(&self) -> &DeletionRepository {
        &self.journal
    }

    async fn count(&self, sql: &str, id: &str) -> DeletionResult<u64> {
        let row = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
            .bind(id)
            .fetch_one(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;
        Ok(u64::try_from(row.get::<i64, _>(0)).unwrap_or(0))
    }

    /// Count rows in a table that a later capability may not have shipped yet.
    async fn count_optional(&self, table: &str, sql: &str, id: &str) -> DeletionResult<u64> {
        let exists = sqlx::query("SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1")
            .bind(table)
            .fetch_optional(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;
        if exists.is_none() {
            return Ok(0);
        }
        self.count(sql, id).await
    }

    /// One transaction: remove every dependent row, write the tombstone and
    /// the suppressions, then remove the recording itself.
    async fn remove_rows(
        &self,
        deletion_id: &str,
        recording_id: RecordingId,
        disclosure: &DeletionDisclosure,
        now: Timestamp,
    ) -> DeletionResult<()> {
        let id = recording_id.to_string();
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(PersistenceError::from)?;

        // Suppressions first: if the transaction commits at all, the deletion
        // is durable against a later sync.
        for media in &disclosure.media {
            sqlx::query(
                "INSERT INTO deletion_media_suppressions (sha256, byte_length, deletion_id, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(sha256, byte_length) DO UPDATE SET
                     deletion_id = excluded.deletion_id, created_at = excluded.created_at",
            )
            .bind(&media.sha256)
            .bind(i64::try_from(media.byte_length).unwrap_or(i64::MAX))
            .bind(deletion_id)
            .bind(now.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?;
        }

        let source_items =
            sqlx::query("SELECT source_id, external_id FROM source_items WHERE recording_id = ?1")
                .bind(&id)
                .fetch_all(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
        for item in &source_items {
            let Some(external_id) = item.get::<Option<String>, _>("external_id") else {
                continue;
            };
            sqlx::query(
                "INSERT INTO deletion_source_suppressions (source_id, external_id, deletion_id, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(source_id, external_id) DO UPDATE SET
                     deletion_id = excluded.deletion_id, created_at = excluded.created_at",
            )
            .bind(item.get::<String, _>("source_id"))
            .bind(external_id)
            .bind(deletion_id)
            .bind(now.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?;
        }

        sqlx::query(
            "INSERT INTO tombstones (id, entity_type, entity_id, reason, created_at, retention_until)
             VALUES (?1, 'recording', ?2, ?3, ?4, NULL)
             ON CONFLICT(entity_type, entity_id) DO UPDATE SET reason = excluded.reason",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&id)
        .bind(&disclosure.title)
        .bind(now.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(PersistenceError::from)?;

        // Children first; every foreign key into `recordings` is RESTRICT, so
        // a forgotten table fails loudly instead of orphaning rows.
        let statements = [
            "DELETE FROM output_evidence WHERE output_id IN (SELECT id FROM ai_outputs WHERE recording_id = ?1)",
            "DELETE FROM ai_outputs WHERE recording_id = ?1",
            "DELETE FROM transcript_words WHERE segment_id IN (
                 SELECT s.id FROM transcript_segments s
                 JOIN transcript_revisions r ON r.id = s.transcript_revision_id
                 WHERE r.recording_id = ?1)",
            "DELETE FROM transcript_annotations WHERE transcript_revision_id IN
                 (SELECT id FROM transcript_revisions WHERE recording_id = ?1)",
            "DELETE FROM transcript_segments WHERE transcript_revision_id IN
                 (SELECT id FROM transcript_revisions WHERE recording_id = ?1)",
            // Child revisions reference their parent, so delete leaves first.
            "DELETE FROM transcript_revisions WHERE recording_id = ?1 AND id NOT IN
                 (SELECT parent_id FROM transcript_revisions WHERE parent_id IS NOT NULL
                   AND recording_id = ?1)",
            "DELETE FROM transcript_revisions WHERE recording_id = ?1",
            "DELETE FROM speakers WHERE recording_id = ?1",
            "DELETE FROM recording_tags WHERE recording_id = ?1",
            "DELETE FROM media_revisions WHERE recording_id = ?1",
            "DELETE FROM source_items WHERE recording_id = ?1",
            "DELETE FROM export_derivatives WHERE recording_id = ?1",
            "DELETE FROM export_events WHERE recording_id = ?1",
            "UPDATE recordings SET preferred_media_asset_id = NULL WHERE id = ?1",
            "DELETE FROM recording_media WHERE recording_id = ?1",
        ];
        for statement in statements {
            sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
        }

        // Tables owned by capabilities that may not have shipped in this
        // build. The activity history addresses a recording through
        // `target_kind`/`target_id`, not a foreign key.
        for (table, statement) in [
            (
                "activity_events",
                "DELETE FROM activity_events WHERE target_kind = 'recording' AND target_id = ?1",
            ),
            (
                "recording_import_metadata",
                "DELETE FROM recording_import_metadata WHERE recording_id = ?1",
            ),
        ] {
            let exists =
                sqlx::query("SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1")
                    .bind(table)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(PersistenceError::from)?;
            if exists.is_some() {
                sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(PersistenceError::from)?;
            }
        }

        // Media assets no other recording references.
        for media in &disclosure.media {
            if media.shared_with_other_recordings {
                continue;
            }
            sqlx::query(
                "DELETE FROM media_assets
                  WHERE sha256 = ?1 AND byte_length = ?2
                    AND id NOT IN (SELECT media_asset_id FROM recording_media)
                    AND id NOT IN (SELECT media_asset_id FROM media_revisions)",
            )
            .bind(&media.sha256)
            .bind(i64::try_from(media.byte_length).unwrap_or(i64::MAX))
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?;
        }

        sqlx::query("DELETE FROM recordings WHERE id = ?1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?;

        tx.commit().await.map_err(PersistenceError::from)?;
        Ok(())
    }

    /// Unlink blobs the journal still owns, re-checking that nothing points at
    /// them. Returns `(blobs removed, bytes freed)`.
    pub async fn unlink_blobs(
        &self,
        deletion_id: &str,
        now: Timestamp,
    ) -> DeletionResult<(u64, u64)> {
        let mut removed = 0u64;
        let mut freed = 0u64;
        for blob in self.journal.blobs(deletion_id).await? {
            if blob.removed_at.is_some() {
                continue;
            }
            let still_referenced =
                sqlx::query("SELECT 1 FROM media_assets WHERE sha256 = ?1 AND byte_length = ?2")
                    .bind(blob.identity.sha256.to_hex())
                    .bind(i64::try_from(blob.identity.byte_length).unwrap_or(i64::MAX))
                    .fetch_optional(self.db.pool())
                    .await
                    .map_err(PersistenceError::from)?;
            if still_referenced.is_some() {
                // Something re-imported these bytes between planning and now.
                // Leave them alone; the row that points at them is valid.
                self.journal
                    .mark_blob_removed(deletion_id, &blob.relative_path, now)
                    .await?;
                continue;
            }

            let path = self.archive.absolute(&blob.relative_path)?;
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    removed += 1;
                    freed += blob.identity.byte_length;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(DeletionError::Persistence(PersistenceError::Io(error))),
            }
            self.journal
                .mark_blob_removed(deletion_id, &blob.relative_path, now)
                .await?;
        }
        Ok((removed, freed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disclosure_digest_changes_with_every_field() {
        let base = DeletionDisclosure {
            disclosure_version: DISCLOSURE_VERSION.to_owned(),
            recording_id: RecordingId::new(),
            title: "Weekly sync".into(),
            has_annotation: false,
            media: Vec::new(),
            counts: DisclosedCounts::default(),
            bytes_to_free: 0,
            limits: DELETION_LIMITS.iter().map(|l| (*l).to_owned()).collect(),
        };
        let digest = base.digest();

        let mut changed = base.clone();
        changed.title = "Weekly sync (renamed)".into();
        assert_ne!(changed.digest(), digest);

        let mut changed = base.clone();
        changed.counts.ai_outputs = 1;
        assert_ne!(changed.digest(), digest);

        let mut changed = base.clone();
        changed.bytes_to_free = 1;
        assert_ne!(changed.digest(), digest);

        assert_eq!(base.digest(), base.clone().digest());
    }

    #[test]
    fn the_limits_do_not_claim_forensic_erasure() {
        let text = DELETION_LIMITS.join(" ");
        assert!(text.contains("cannot guarantee"));
        assert!(text.contains("not touched"));
        assert!(text.contains("outside our control"));
        assert!(!text.to_lowercase().contains("permanently erased"));
    }
}
