//! Export accounting: export events, sanitized derivatives and portable
//! archive records (spec 13.2 `export_events`, 13.3 `archive_backups`, 7.8).
//!
//! Spec 13.2 says we do not store complete sensitive paths by default, so an
//! export event records a *directory hint* and never the file name a user
//! chose. The hint is what the UI needs to say "saved to Documents"; it is not
//! a handle the application later resolves.

use plaudon_domain::ids::{MediaAssetId, RecordingId};
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use plaudon_domain::time::Timestamp;
use sqlx::Row;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// Whether an archive record describes a backup we wrote or a restore we read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveDirection {
    /// We produced the archive.
    Backup,
    /// We consumed the archive.
    Restore,
}

impl ArchiveDirection {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Restore => "restore",
        }
    }
}

/// Whether the archive bytes are readable without a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveProtection {
    /// Anyone holding the file can read every byte.
    Plaintext,
    /// Authenticated encryption with a password-derived key.
    Encrypted,
}

impl ArchiveProtection {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plaintext => "plaintext",
            Self::Encrypted => "encrypted",
        }
    }

    /// The disclosure sentence the UI must show for this protection
    /// (spec 13.6: the plaintext default is conspicuously disclosed).
    #[must_use]
    pub const fn disclosure(self) -> &'static str {
        match self {
            Self::Plaintext => {
                "This archive is NOT encrypted. Anyone who can read the file can read every recording, transcript and note inside it."
            }
            Self::Encrypted => {
                "This archive is encrypted with your password. We cannot recover the contents if you lose that password."
            }
        }
    }
}

/// Outcome of verifying an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveVerifyStatus {
    /// Every entry digest matched the manifest.
    Verified,
    /// At least one entry failed verification.
    Failed,
    /// Verification has not been attempted.
    Unverified,
}

impl ArchiveVerifyStatus {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Unverified => "unverified",
        }
    }
}

/// A recorded portable archive operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveBackupRecord {
    /// Row identity.
    pub id: String,
    /// Backup or restore.
    pub direction: ArchiveDirection,
    /// Manifest schema version the archive declared.
    pub manifest_schema_version: String,
    /// Container format version.
    pub format_version: u32,
    /// Whether the archive is encrypted.
    pub protection: ArchiveProtection,
    /// Directory hint, never the chosen file name.
    pub location_hint: Option<String>,
    /// Number of manifest entries.
    pub entry_count: u64,
    /// Sum of entry byte lengths.
    pub total_byte_length: u64,
    /// Overall integrity digest from the manifest.
    pub integrity_digest: String,
    /// Verification outcome.
    pub verify_status: ArchiveVerifyStatus,
    /// When the operation finished.
    pub created_at: Timestamp,
}

/// A sanitized export derivative (spec 7.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDerivative {
    /// Row identity.
    pub id: String,
    /// Recording the derivative belongs to.
    pub recording_id: RecordingId,
    /// Immutable asset the derivative was produced from.
    pub source_media_asset_id: MediaAssetId,
    /// Content identity of the derivative bytes.
    pub identity: MediaIdentity,
    /// Metadata chunks that were removed, for the provenance record.
    pub stripped_metadata: Vec<String>,
    /// When the derivative was produced.
    pub created_at: Timestamp,
}

/// Reads and writes for export accounting.
#[derive(Debug, Clone)]
pub struct ExportRepository {
    db: Database,
}

impl ExportRepository {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Record that an export was written.
    ///
    /// `destination_hint` must be a directory hint, not a full path.
    pub async fn record_export(
        &self,
        recording_id: Option<RecordingId>,
        format: &str,
        destination_hint: Option<&str>,
        at: Timestamp,
    ) -> PersistenceResult<String> {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO export_events (id, recording_id, format, destination_hint, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&id)
        .bind(recording_id.map(|id| id.to_string()))
        .bind(format)
        .bind(destination_hint)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Export events for a recording, newest first.
    pub async fn exports_for(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<(String, String)>> {
        let rows = sqlx::query(
            "SELECT id, format FROM export_events WHERE recording_id = ?1
             ORDER BY created_at DESC, id DESC",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|row| (row.get::<String, _>("id"), row.get::<String, _>("format")))
            .collect())
    }

    /// Record a sanitized derivative.
    pub async fn record_derivative(
        &self,
        derivative: &ExportDerivative,
    ) -> PersistenceResult<()> {
        let stripped = serde_json::to_string(&derivative.stripped_metadata)
            .unwrap_or_else(|_| "[]".to_owned());
        sqlx::query(
            "INSERT INTO export_derivatives
                 (id, recording_id, source_media_asset_id, kind, sha256, byte_length,
                  stripped_metadata_json, created_at)
             VALUES (?1, ?2, ?3, 'sanitized', ?4, ?5, ?6, ?7)",
        )
        .bind(&derivative.id)
        .bind(derivative.recording_id.to_string())
        .bind(derivative.source_media_asset_id.to_string())
        .bind(derivative.identity.sha256.to_hex())
        .bind(i64::try_from(derivative.identity.byte_length).unwrap_or(i64::MAX))
        .bind(stripped)
        .bind(derivative.created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Sanitized derivatives recorded for a recording.
    pub async fn derivatives_for(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<ExportDerivative>> {
        let rows = sqlx::query(
            "SELECT id, recording_id, source_media_asset_id, sha256, byte_length,
                    stripped_metadata_json, created_at
             FROM export_derivatives WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let sha: String = row.get("sha256");
                let length: i64 = row.get("byte_length");
                let stripped: String = row.get("stripped_metadata_json");
                Ok(ExportDerivative {
                    id: row.get("id"),
                    recording_id: row
                        .get::<String, _>("recording_id")
                        .parse()
                        .map_err(|_| PersistenceError::Migration("corrupt recording id".into()))?,
                    source_media_asset_id: row
                        .get::<String, _>("source_media_asset_id")
                        .parse()
                        .map_err(|_| PersistenceError::Migration("corrupt asset id".into()))?,
                    identity: MediaIdentity::new(
                        sha.parse::<Sha256Digest>()
                            .map_err(|_| PersistenceError::IntegrityFailed)?,
                        u64::try_from(length).unwrap_or(0),
                        String::new(),
                    ),
                    stripped_metadata: serde_json::from_str(&stripped).unwrap_or_default(),
                    created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
                })
            })
            .collect()
    }

    /// Record a portable archive backup or restore.
    pub async fn record_archive(&self, record: &ArchiveBackupRecord) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO archive_backups
                 (id, direction, manifest_schema_version, format_version, protection,
                  location_hint, entry_count, total_byte_length, integrity_digest,
                  verify_status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&record.id)
        .bind(record.direction.as_str())
        .bind(&record.manifest_schema_version)
        .bind(i64::from(record.format_version))
        .bind(record.protection.as_str())
        .bind(record.location_hint.as_deref())
        .bind(i64::try_from(record.entry_count).unwrap_or(i64::MAX))
        .bind(i64::try_from(record.total_byte_length).unwrap_or(i64::MAX))
        .bind(&record.integrity_digest)
        .bind(record.verify_status.as_str())
        .bind(record.created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Number of recorded archive operations.
    pub async fn archive_count(&self) -> PersistenceResult<i64> {
        let row = sqlx::query("SELECT COUNT(*) FROM archive_backups")
            .fetch_one(self.db.pool())
            .await?;
        Ok(row.get::<i64, _>(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::MigrationRunner;
    use plaudon_domain::media::QualityClass;
    use plaudon_domain::media::{MediaAsset, VerificationState};
    use plaudon_domain::recording::Recording;

    use crate::repo::{MediaAssetRepository, RecordingRepository};

    async fn library() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
        MigrationRunner::new().run(&db).await.unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn a_sanitized_derivative_is_a_separate_row_from_its_original() {
        let (_dir, db) = library().await;
        let recordings = RecordingRepository::new(db.clone());
        let assets = MediaAssetRepository::new(db.clone());
        let exports = ExportRepository::new(db.clone());

        let recording = Recording::new(
            RecordingId::new(),
            "Weekly sync",
            Timestamp::from_millis(10),
        );
        recordings.insert(&recording).await.unwrap();
        let asset = MediaAsset {
            id: MediaAssetId::new(),
            identity: MediaIdentity::of_bytes(b"original bytes", "wav"),
            relative_path: "media/sha256/aa/original.wav".into(),
            quality_class: QualityClass::OriginalDeviceRecording,
            verification_state: VerificationState::Verified,
            verified_at: Some(Timestamp::from_millis(10)),
        };
        assets.insert(&asset, Timestamp::from_millis(10)).await.unwrap();

        exports
            .record_derivative(&ExportDerivative {
                id: Uuid::now_v7().to_string(),
                recording_id: recording.id,
                source_media_asset_id: asset.id,
                identity: MediaIdentity::of_bytes(b"sanitized bytes", "wav"),
                stripped_metadata: vec!["LIST/INFO".into()],
                created_at: Timestamp::from_millis(20),
            })
            .await
            .unwrap();

        let derivatives = exports.derivatives_for(recording.id).await.unwrap();
        assert_eq!(derivatives.len(), 1);
        assert_ne!(derivatives[0].identity, asset.identity);
        assert_eq!(derivatives[0].stripped_metadata, vec!["LIST/INFO"]);
        // The original row is untouched.
        assert_eq!(assets.get(asset.id).await.unwrap().identity, asset.identity);
        db.close().await;
    }

    #[tokio::test]
    async fn archive_records_capture_protection_and_verification() {
        let (_dir, db) = library().await;
        let exports = ExportRepository::new(db.clone());
        exports
            .record_archive(&ArchiveBackupRecord {
                id: Uuid::now_v7().to_string(),
                direction: ArchiveDirection::Backup,
                manifest_schema_version: "recording-archive-manifest/1".into(),
                format_version: 1,
                protection: ArchiveProtection::Encrypted,
                location_hint: Some("Documents".into()),
                entry_count: 3,
                total_byte_length: 4096,
                integrity_digest: "0".repeat(64),
                verify_status: ArchiveVerifyStatus::Verified,
                created_at: Timestamp::from_millis(1),
            })
            .await
            .unwrap();
        assert_eq!(exports.archive_count().await.unwrap(), 1);
        db.close().await;
    }

    #[test]
    fn plaintext_protection_discloses_that_nothing_is_encrypted() {
        assert!(ArchiveProtection::Plaintext
            .disclosure()
            .contains("NOT encrypted"));
        assert!(ArchiveProtection::Encrypted
            .disclosure()
            .contains("cannot recover"));
    }
}
