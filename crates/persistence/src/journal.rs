//! The durable import journal and its startup reconciler (spec 6.3B, 13.3A).
//!
//! A SQLite transaction cannot atomically commit a filesystem rename, so an
//! import is a journalled sequence instead of a single step:
//!
//! 1. write a staging file inside the archive and hash it with a bounded buffer;
//! 2. flush the file and its directory;
//! 3. record a prepared intent;
//! 4. rename atomically inside the same filesystem;
//! 5. transact the asset association;
//! 6. mark the journal committed.
//!
//! Each step is a separate public method, so a test can stop at any step and
//! run [`ImportJournal::reconcile`] exactly as a restart would. There is no
//! crash-injection hook in the production path.

use std::io::Read;
use std::path::PathBuf;

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::{ImportIntentId, JobId, MediaAssetId, RecordingId};
use plaudon_domain::media::{
    MediaAsset, MediaIdentity, QualityClass, Sha256Digest, VerificationState,
};
use plaudon_domain::time::Timestamp;
use sqlx::Row;

use crate::archive::{self, MediaArchive};
use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};
use crate::repo::MediaAssetRepository;

/// State of one import intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalState {
    /// Bytes written to staging, not yet hashed.
    Staged,
    /// Staging file hashed.
    Hashed,
    /// Staging file and directory flushed.
    Flushed,
    /// The prepared intent is durable (step 3).
    IntentRecorded,
    /// The staging file has been renamed into the archive (step 4).
    Renamed,
    /// The asset association is committed (steps 5 and 6).
    Committed,
    /// The intent was abandoned; no archive entry exists.
    Abandoned,
    /// The staged bytes did not verify and were set aside for inspection.
    Quarantined,
}

impl JournalState {
    /// The wire string stored in `import_journal.state`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Hashed => "hashed",
            Self::Flushed => "flushed",
            Self::IntentRecorded => "intent_recorded",
            Self::Renamed => "renamed",
            Self::Committed => "committed",
            Self::Abandoned => "abandoned",
            Self::Quarantined => "quarantined",
        }
    }

    /// Parse a stored state.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "staged" => Self::Staged,
            "hashed" => Self::Hashed,
            "flushed" => Self::Flushed,
            "intent_recorded" => Self::IntentRecorded,
            "renamed" => Self::Renamed,
            "committed" => Self::Committed,
            "abandoned" => Self::Abandoned,
            "quarantined" => Self::Quarantined,
            _ => return None,
        })
    }

    /// Whether the intent has reached a state that needs no recovery.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Committed | Self::Abandoned | Self::Quarantined)
    }
}

/// A staged file plus the identity of its bytes (steps 1 and 2 complete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// Intent this staging file belongs to.
    pub intent_id: ImportIntentId,
    /// Absolute staging path.
    pub path: PathBuf,
    /// Identity of the staged bytes.
    pub identity: MediaIdentity,
}

/// A durable prepared intent (step 3 complete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedIntent {
    /// Intent identity.
    pub id: ImportIntentId,
    /// Current state.
    pub state: JournalState,
    /// Staging path recorded in the journal.
    pub staging_path: PathBuf,
    /// Archive-relative path the bytes will occupy.
    pub target_relative_path: String,
    /// Identity the staged bytes must have.
    pub identity: MediaIdentity,
    /// Fidelity class of the bytes.
    pub quality_class: QualityClass,
    /// Recording the asset will be associated with.
    pub recording_id: Option<RecordingId>,
    /// Job that owns the import.
    pub job_id: Option<JobId>,
    /// Asset created at commit, when the intent is committed.
    pub media_asset_id: Option<MediaAssetId>,
}

/// What reconciliation did to one intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// An intent that had already been renamed was completed.
    Committed(ImportIntentId),
    /// An intent with no usable bytes was abandoned.
    Abandoned(ImportIntentId),
    /// Staged bytes did not match the recorded identity.
    Quarantined(ImportIntentId),
    /// An app-owned staging file with no live intent was removed.
    OrphanRemoved(PathBuf),
    /// An intent could not be settled on this pass and was left for the next
    /// one.
    ///
    /// Startup must not abort because one intent is unrecoverable right now:
    /// a single stuck import would make the whole library unopenable.
    Deferred {
        /// The intent that could not be settled.
        intent_id: ImportIntentId,
        /// Stable code describing why.
        code: AppErrorCode,
    },
}

/// The import journal.
#[derive(Debug, Clone)]
pub struct ImportJournal {
    db: Database,
    archive: MediaArchive,
}

impl ImportJournal {
    /// Wrap a database and archive.
    #[must_use]
    pub fn new(db: Database, archive: MediaArchive) -> Self {
        Self { db, archive }
    }

    /// The archive this journal commits into.
    #[must_use]
    pub fn archive(&self) -> &MediaArchive {
        &self.archive
    }

    /// Steps 1 and 2: stream bytes into staging, hash them, flush.
    pub fn stage(
        &self,
        source: &mut dyn Read,
        observed_codec: &str,
    ) -> PersistenceResult<StagedFile> {
        let intent_id = ImportIntentId::new();
        let path = self.archive.staging_path(intent_id);
        let identity = self
            .archive
            .stage_from_reader(&path, source, observed_codec)?;
        Ok(StagedFile {
            intent_id,
            path,
            identity,
        })
    }

    /// Step 3: record the prepared intent durably.
    pub async fn record_intent(
        &self,
        staged: &StagedFile,
        extension: &str,
        quality_class: QualityClass,
        recording_id: Option<RecordingId>,
        job_id: Option<JobId>,
        at: Timestamp,
    ) -> PersistenceResult<PreparedIntent> {
        let target_relative_path = archive::relative_path(&staged.identity, extension);
        sqlx::query(
            "INSERT INTO import_journal
                 (id, state, job_id, recording_id, staging_path, target_relative_path,
                  expected_sha256, expected_byte_length, quality_class,
                  created_at, updated_at, hashed_at, flushed_at, intent_at)
             VALUES (?1, 'intent_recorded', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9, ?9, ?9)",
        )
        .bind(staged.intent_id.to_string())
        .bind(job_id.map(|id| id.to_string()))
        .bind(recording_id.map(|id| id.to_string()))
        .bind(staged.path.display().to_string())
        .bind(&target_relative_path)
        .bind(staged.identity.sha256.to_hex())
        .bind(i64::try_from(staged.identity.byte_length).unwrap_or(i64::MAX))
        .bind(quality_class_str(quality_class))
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;

        Ok(PreparedIntent {
            id: staged.intent_id,
            state: JournalState::IntentRecorded,
            staging_path: staged.path.clone(),
            target_relative_path,
            identity: staged.identity.clone(),
            quality_class,
            recording_id,
            job_id,
            media_asset_id: None,
        })
    }

    /// Step 4: atomically rename the staged file into the archive.
    ///
    /// If the archive already holds verified bytes for this identity, the
    /// staged duplicate is discarded instead of being written twice.
    pub async fn rename_into_archive(
        &self,
        intent: &PreparedIntent,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        let already_present = self.archive.exists(&intent.target_relative_path)?;
        if already_present {
            // Verify the existing blob before trusting it, then drop ours.
            self.archive
                .verify(&intent.target_relative_path, &intent.identity)?;
            if intent.staging_path.exists() {
                std::fs::remove_file(&intent.staging_path)?;
            }
        } else {
            self.archive
                .commit_staged(&intent.staging_path, &intent.target_relative_path)?;
        }
        self.set_state(intent.id, JournalState::Renamed, at, None)
            .await
    }

    /// Steps 5 and 6: transact the asset association and close the journal.
    pub async fn commit(
        &self,
        intent: &PreparedIntent,
        at: Timestamp,
    ) -> PersistenceResult<MediaAssetId> {
        let assets = MediaAssetRepository::new(self.db.clone());
        let asset_id = if let Some(existing) = assets.find_by_identity(&intent.identity).await? {
            // Spec 6.3B: reuse of an existing blob is only safe after the
            // bytes behind it have been re-verified.
            self.archive
                .verify(&existing.relative_path, &existing.identity)?;
            existing.id
        } else {
            let asset = MediaAsset {
                id: MediaAssetId::new(),
                identity: intent.identity.clone(),
                relative_path: intent.target_relative_path.clone(),
                quality_class: intent.quality_class,
                verification_state: VerificationState::Verified,
                verified_at: Some(at),
            };
            assets.insert(&asset, at).await?;
            asset.id
        };

        self.set_state(intent.id, JournalState::Committed, at, Some(asset_id))
            .await?;
        Ok(asset_id)
    }

    /// Mark an intent abandoned and remove its staging file.
    pub async fn abandon(
        &self,
        intent_id: ImportIntentId,
        failure_code: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        let intent = self.get(intent_id).await?;
        if intent.staging_path.exists() {
            std::fs::remove_file(&intent.staging_path)?;
        }
        sqlx::query(
            "UPDATE import_journal
             SET state = 'abandoned', failure_code = ?2, updated_at = ?3, abandoned_at = ?3
             WHERE id = ?1",
        )
        .bind(intent_id.to_string())
        .bind(failure_code)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Load one intent.
    pub async fn get(&self, id: ImportIntentId) -> PersistenceResult<PreparedIntent> {
        let row = sqlx::query(
            "SELECT id, state, job_id, recording_id, staging_path, target_relative_path,
                    expected_sha256, expected_byte_length, quality_class, media_asset_id
             FROM import_journal WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PersistenceError::RecordNotFound)?;
        Self::intent_from_row(&row)
    }

    /// Every intent that has not settled.
    pub async fn incomplete(&self) -> PersistenceResult<Vec<PreparedIntent>> {
        let rows = sqlx::query(
            "SELECT id, state, job_id, recording_id, staging_path, target_relative_path,
                    expected_sha256, expected_byte_length, quality_class, media_asset_id
             FROM import_journal
             WHERE state NOT IN ('committed','abandoned','quarantined')
             ORDER BY created_at",
        )
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::intent_from_row).collect()
    }

    /// Reconcile every incomplete intent against the bytes actually on disk.
    ///
    /// Each intent ends either fully committed or cleanly abandoned. A staged
    /// file whose hash no longer matches is quarantined rather than committed,
    /// and only app-owned orphan staging files are removed.
    pub async fn reconcile(&self, at: Timestamp) -> PersistenceResult<Vec<ReconcileOutcome>> {
        let mut outcomes = Vec::new();
        let incomplete = self.incomplete().await?;

        for intent in &incomplete {
            match self.reconcile_one(intent, at).await {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => outcomes.push(ReconcileOutcome::Deferred {
                    intent_id: intent.id,
                    code: error.code(),
                }),
            }
        }

        // Only staging files we could have written, and only when no intent
        // still needs them. Quarantined bytes are kept for inspection, and a
        // source recording is never touched.
        for orphan in self
            .archive
            .orphan_staging_files(&self.retained_intent_ids().await?)?
        {
            std::fs::remove_file(&orphan)?;
            outcomes.push(ReconcileOutcome::OrphanRemoved(orphan));
        }
        Ok(outcomes)
    }

    /// Settle one intent against the bytes actually on disk.
    async fn reconcile_one(
        &self,
        intent: &PreparedIntent,
        at: Timestamp,
    ) -> PersistenceResult<ReconcileOutcome> {
        let archived = self.archive.exists(&intent.target_relative_path)?;
        if archived {
            return match self
                .archive
                .verify(&intent.target_relative_path, &intent.identity)
            {
                Ok(()) => {
                    self.commit(intent, at).await?;
                    Ok(ReconcileOutcome::Committed(intent.id))
                }
                Err(PersistenceError::IntegrityFailed) => {
                    self.quarantine(intent.id, "INTEGRITY_FAILED", at).await?;
                    Ok(ReconcileOutcome::Quarantined(intent.id))
                }
                Err(other) => Err(other),
            };
        }

        if intent.staging_path.is_file() {
            let (digest, length) = MediaArchive::digest_file(&intent.staging_path)?;
            if digest == intent.identity.sha256 && length == intent.identity.byte_length {
                self.rename_into_archive(intent, at).await?;
                self.commit(intent, at).await?;
                Ok(ReconcileOutcome::Committed(intent.id))
            } else {
                self.quarantine(intent.id, "INTEGRITY_FAILED", at).await?;
                Ok(ReconcileOutcome::Quarantined(intent.id))
            }
        } else {
            self.abandon(intent.id, "DOWNLOAD_INTERRUPTED", at).await?;
            Ok(ReconcileOutcome::Abandoned(intent.id))
        }
    }

    /// Intents whose staging file must be preserved: everything that has not
    /// been committed or cleanly abandoned, including quarantined bytes kept
    /// for inspection.
    pub async fn retained_intent_ids(&self) -> PersistenceResult<Vec<ImportIntentId>> {
        let rows = sqlx::query(
            "SELECT id FROM import_journal WHERE state NOT IN ('committed','abandoned')",
        )
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                row.get::<String, _>("id")
                    .parse()
                    .map_err(|_| PersistenceError::Migration("corrupt intent id".into()))
            })
            .collect()
    }

    /// Mark an intent quarantined, keeping its staged bytes for inspection.
    pub async fn quarantine(
        &self,
        intent_id: ImportIntentId,
        failure_code: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "UPDATE import_journal
             SET state = 'quarantined', failure_code = ?2, updated_at = ?3
             WHERE id = ?1",
        )
        .bind(intent_id.to_string())
        .bind(failure_code)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    async fn set_state(
        &self,
        id: ImportIntentId,
        state: JournalState,
        at: Timestamp,
        media_asset_id: Option<MediaAssetId>,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "UPDATE import_journal
             SET state = ?2,
                 updated_at = ?3,
                 media_asset_id = COALESCE(?4, media_asset_id),
                 renamed_at   = CASE WHEN ?2 = 'renamed'   THEN ?3 ELSE renamed_at   END,
                 committed_at = CASE WHEN ?2 = 'committed' THEN ?3 ELSE committed_at END
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(state.as_str())
        .bind(at.as_millis())
        .bind(media_asset_id.map(|id| id.to_string()))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    fn intent_from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<PreparedIntent> {
        let sha: String = row.get("expected_sha256");
        let length: i64 = row.get("expected_byte_length");
        Ok(PreparedIntent {
            id: row
                .get::<String, _>("id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt intent id".into()))?,
            state: JournalState::parse(&row.get::<String, _>("state"))
                .ok_or_else(|| PersistenceError::Migration("unknown journal state".into()))?,
            staging_path: PathBuf::from(row.get::<String, _>("staging_path")),
            target_relative_path: row.get("target_relative_path"),
            identity: MediaIdentity::new(
                sha.parse::<Sha256Digest>()
                    .map_err(|_| PersistenceError::IntegrityFailed)?,
                u64::try_from(length).unwrap_or(0),
                String::new(),
            ),
            quality_class: quality_class_from(&row.get::<String, _>("quality_class")),
            recording_id: row
                .get::<Option<String>, _>("recording_id")
                .and_then(|raw| raw.parse().ok()),
            job_id: row
                .get::<Option<String>, _>("job_id")
                .and_then(|raw| raw.parse().ok()),
            media_asset_id: row
                .get::<Option<String>, _>("media_asset_id")
                .and_then(|raw| raw.parse().ok()),
        })
    }
}

fn quality_class_str(class: QualityClass) -> &'static str {
    match class {
        QualityClass::OriginalDeviceRecording => "original_device_recording",
        QualityClass::CompressedDeviceTransfer => "compressed_device_transfer",
        QualityClass::VendorCloudExport => "vendor_cloud_export",
        QualityClass::UserSuppliedFile => "user_supplied_file",
        QualityClass::LocalDerivative => "local_derivative",
        QualityClass::Unknown => "unknown",
    }
}

fn quality_class_from(raw: &str) -> QualityClass {
    match raw {
        "original_device_recording" => QualityClass::OriginalDeviceRecording,
        "compressed_device_transfer" => QualityClass::CompressedDeviceTransfer,
        "vendor_cloud_export" => QualityClass::VendorCloudExport,
        "user_supplied_file" => QualityClass::UserSuppliedFile,
        "local_derivative" => QualityClass::LocalDerivative,
        _ => QualityClass::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::MigrationRunner;

    struct Harness {
        _dir: tempfile::TempDir,
        db: Database,
        journal: ImportJournal,
    }

    async fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
        MigrationRunner::new().run(&db).await.unwrap();
        let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
        let journal = ImportJournal::new(db.clone(), archive);
        Harness {
            _dir: dir,
            db,
            journal,
        }
    }

    #[tokio::test]
    async fn the_full_six_step_import_commits_a_verified_asset() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        let intent = h
            .journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        h.journal
            .rename_into_archive(&intent, Timestamp::EPOCH)
            .await
            .unwrap();
        let asset_id = h.journal.commit(&intent, Timestamp::EPOCH).await.unwrap();

        assert_eq!(
            h.journal.get(intent.id).await.unwrap().state,
            JournalState::Committed
        );
        let asset = MediaAssetRepository::new(h.db.clone())
            .get(asset_id)
            .await
            .unwrap();
        assert_eq!(asset.identity, MediaIdentity::of_bytes(b"audio", "wav"));
        assert_eq!(
            h.journal
                .archive()
                .read_verified(&asset.relative_path, &asset.identity)
                .unwrap(),
            b"audio"
        );
        assert_eq!(h.journal.incomplete().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_crash_after_staging_leaves_no_intent_and_only_an_orphan() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        assert!(staged.path.is_file());

        let outcomes = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert_eq!(
            outcomes,
            vec![ReconcileOutcome::OrphanRemoved(staged.path.clone())]
        );
        assert!(!staged.path.exists());
        assert_eq!(
            MediaAssetRepository::new(h.db.clone())
                .count()
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_crash_after_the_intent_is_recorded_recovers_by_committing() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        let intent = h
            .journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();

        let outcomes = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert_eq!(outcomes, vec![ReconcileOutcome::Committed(intent.id)]);
        assert_eq!(
            h.journal.get(intent.id).await.unwrap().state,
            JournalState::Committed
        );
        assert_eq!(
            MediaAssetRepository::new(h.db.clone())
                .count()
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn a_crash_after_the_rename_recovers_by_committing() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        let intent = h
            .journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        h.journal
            .rename_into_archive(&intent, Timestamp::EPOCH)
            .await
            .unwrap();

        let outcomes = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert_eq!(outcomes, vec![ReconcileOutcome::Committed(intent.id)]);
        let asset = MediaAssetRepository::new(h.db.clone())
            .find_by_identity(&staged.identity)
            .await
            .unwrap()
            .expect("asset");
        h.journal
            .archive()
            .verify(&asset.relative_path, &asset.identity)
            .unwrap();
    }

    #[tokio::test]
    async fn a_truncated_staging_file_is_quarantined_not_committed() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        let intent = h
            .journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        std::fs::write(&staged.path, b"aud").unwrap();

        let outcomes = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert_eq!(outcomes, vec![ReconcileOutcome::Quarantined(intent.id)]);
        assert_eq!(
            h.journal.get(intent.id).await.unwrap().state,
            JournalState::Quarantined
        );
        assert_eq!(
            MediaAssetRepository::new(h.db.clone())
                .count()
                .await
                .unwrap(),
            0
        );
        assert!(
            staged.path.is_file(),
            "quarantined bytes are kept for inspection"
        );
    }

    #[tokio::test]
    async fn a_missing_staging_file_abandons_the_intent() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        let intent = h
            .journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        std::fs::remove_file(&staged.path).unwrap();

        let outcomes = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert_eq!(outcomes, vec![ReconcileOutcome::Abandoned(intent.id)]);
        assert_eq!(
            MediaAssetRepository::new(h.db.clone())
                .count()
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_corrupted_archive_blob_quarantines_rather_than_completing() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        let intent = h
            .journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        h.journal
            .rename_into_archive(&intent, Timestamp::EPOCH)
            .await
            .unwrap();
        let path = h
            .journal
            .archive()
            .absolute(&intent.target_relative_path)
            .unwrap();
        std::fs::write(path, b"tampered").unwrap();

        let outcomes = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert_eq!(outcomes, vec![ReconcileOutcome::Quarantined(intent.id)]);
        assert_eq!(
            MediaAssetRepository::new(h.db.clone())
                .count()
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn reconciliation_is_idempotent() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
            .unwrap();
        h.journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        let second = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert_eq!(second.len(), 0);
        assert_eq!(
            MediaAssetRepository::new(h.db.clone())
                .count()
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn importing_identical_bytes_twice_stores_one_blob() {
        let h = harness().await;
        let mut asset_ids = Vec::new();
        for _ in 0..2 {
            let staged = h
                .journal
                .stage(&mut std::io::Cursor::new(b"same bytes".to_vec()), "wav")
                .unwrap();
            let intent = h
                .journal
                .record_intent(
                    &staged,
                    "wav",
                    QualityClass::UserSuppliedFile,
                    None,
                    None,
                    Timestamp::EPOCH,
                )
                .await
                .unwrap();
            h.journal
                .rename_into_archive(&intent, Timestamp::EPOCH)
                .await
                .unwrap();
            asset_ids.push(h.journal.commit(&intent, Timestamp::EPOCH).await.unwrap());
        }
        assert_eq!(
            asset_ids[0], asset_ids[1],
            "the second import reuses the existing blob"
        );
        assert_eq!(
            MediaAssetRepository::new(h.db.clone())
                .count()
                .await
                .unwrap(),
            1
        );
        assert_eq!(h.journal.archive().list_blobs().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reusing_a_corrupted_existing_blob_fails_instead_of_silently_succeeding() {
        let h = harness().await;
        let staged = h
            .journal
            .stage(&mut std::io::Cursor::new(b"payload".to_vec()), "wav")
            .unwrap();
        let intent = h
            .journal
            .record_intent(
                &staged,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        h.journal
            .rename_into_archive(&intent, Timestamp::EPOCH)
            .await
            .unwrap();
        h.journal.commit(&intent, Timestamp::EPOCH).await.unwrap();

        std::fs::write(
            h.journal
                .archive()
                .absolute(&intent.target_relative_path)
                .unwrap(),
            b"x",
        )
        .unwrap();

        let second = h
            .journal
            .stage(&mut std::io::Cursor::new(b"payload".to_vec()), "wav")
            .unwrap();
        let second_intent = h
            .journal
            .record_intent(
                &second,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        let error = h
            .journal
            .rename_into_archive(&second_intent, Timestamp::EPOCH)
            .await
            .unwrap_err();
        assert!(matches!(error, PersistenceError::IntegrityFailed));
    }

    #[tokio::test]
    async fn an_unrecoverable_intent_is_deferred_rather_than_aborting_startup() {
        let h = harness().await;

        // A stuck intent: the staging file is gone and the archive target is
        // occupied by bytes that do not match, so neither commit nor
        // quarantine-by-staging applies.
        let stuck = h
            .journal
            .stage(&mut std::io::Cursor::new(b"stuck".to_vec()), "wav")
            .unwrap();
        let stuck_intent = h
            .journal
            .record_intent(
                &stuck,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        let occupied = h
            .journal
            .archive()
            .absolute(&stuck_intent.target_relative_path)
            .unwrap();
        std::fs::create_dir_all(occupied.parent().unwrap()).unwrap();
        std::fs::write(&occupied, b"different bytes entirely").unwrap();

        // A healthy intent queued behind it.
        let healthy = h
            .journal
            .stage(&mut std::io::Cursor::new(b"healthy".to_vec()), "wav")
            .unwrap();
        let healthy_intent = h
            .journal
            .record_intent(
                &healthy,
                "wav",
                QualityClass::UserSuppliedFile,
                None,
                None,
                Timestamp::EPOCH,
            )
            .await
            .unwrap();

        let outcomes = h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert!(
            outcomes.contains(&ReconcileOutcome::Committed(healthy_intent.id)),
            "one stuck intent must not block the rest of startup: {outcomes:?}"
        );
        assert!(
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, ReconcileOutcome::Quarantined(id) | ReconcileOutcome::Deferred { intent_id: id, .. } if *id == stuck_intent.id)),
            "the stuck intent must be reported, not silently dropped: {outcomes:?}"
        );
    }

    #[tokio::test]
    async fn a_foreign_file_in_the_staging_directory_is_left_alone() {
        let h = harness().await;
        let foreign = h.journal.archive().staging_dir().join("user-file.wav");
        std::fs::write(&foreign, b"not ours").unwrap();
        h.journal.reconcile(Timestamp::EPOCH).await.unwrap();
        assert!(
            foreign.is_file(),
            "reconciliation must never delete files we did not create"
        );
    }

    #[test]
    fn journal_states_round_trip_and_settle_correctly() {
        for state in [
            JournalState::Staged,
            JournalState::Hashed,
            JournalState::Flushed,
            JournalState::IntentRecorded,
            JournalState::Renamed,
            JournalState::Committed,
            JournalState::Abandoned,
            JournalState::Quarantined,
        ] {
            assert_eq!(JournalState::parse(state.as_str()), Some(state));
        }
        assert!(JournalState::Committed.is_settled());
        assert!(!JournalState::Renamed.is_settled());
        assert_eq!(JournalState::parse("teleported"), None);
    }
}
