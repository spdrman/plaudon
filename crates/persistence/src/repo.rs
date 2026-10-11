//! Repositories over the core identity tables.

use plaudon_domain::ids::{MediaAssetId, RecordingId, SourceId, SourceItemId};
use plaudon_domain::media::{
    MediaAsset, MediaIdentity, QualityClass, Sha256Digest, VerificationState,
};
use plaudon_domain::recording::{Recording, RecordingMedia};
use plaudon_domain::source::{
    ExternalRecordingId, ImportStatus, RemoteState, SourceAccountScope, SourceAssociation,
    SourceIdentityKey, SourceKind,
};
use plaudon_domain::time::Timestamp;
use sqlx::Row;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

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

fn verification_str(state: VerificationState) -> &'static str {
    match state {
        VerificationState::Verified => "verified",
        VerificationState::Failed => "failed",
        VerificationState::Unverified => "unverified",
    }
}

fn verification_from(raw: &str) -> VerificationState {
    match raw {
        "verified" => VerificationState::Verified,
        "failed" => VerificationState::Failed,
        _ => VerificationState::Unverified,
    }
}

fn source_kind_str(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::PlaudDeviceBle => "plaud_device_ble",
        SourceKind::PlaudDeviceUsb => "plaud_device_usb",
        SourceKind::PlaudCloudCli => "plaud_cloud_cli",
        SourceKind::LocalFile => "local_file",
        SourceKind::WatchedFolder => "watched_folder",
        SourceKind::Unknown => "unknown",
    }
}

fn source_kind_from(raw: &str) -> SourceKind {
    match raw {
        "plaud_device_ble" => SourceKind::PlaudDeviceBle,
        "plaud_device_usb" => SourceKind::PlaudDeviceUsb,
        "plaud_cloud_cli" => SourceKind::PlaudCloudCli,
        "local_file" => SourceKind::LocalFile,
        "watched_folder" => SourceKind::WatchedFolder,
        _ => SourceKind::Unknown,
    }
}

fn import_status_str(status: ImportStatus) -> &'static str {
    match status {
        ImportStatus::Discovered => "discovered",
        ImportStatus::Acquiring => "acquiring",
        ImportStatus::Imported => "imported",
        ImportStatus::Failed => "failed",
        ImportStatus::Skipped => "skipped",
        ImportStatus::Unknown => "unknown",
    }
}

fn import_status_from(raw: &str) -> ImportStatus {
    match raw {
        "discovered" => ImportStatus::Discovered,
        "acquiring" => ImportStatus::Acquiring,
        "imported" => ImportStatus::Imported,
        "failed" => ImportStatus::Failed,
        "skipped" => ImportStatus::Skipped,
        _ => ImportStatus::Unknown,
    }
}

fn remote_state_str(state: RemoteState) -> &'static str {
    match state {
        RemoteState::Present => "present",
        RemoteState::Trashed => "trashed",
        RemoteState::Absent => "absent",
        RemoteState::Unknown => "unknown",
    }
}

fn remote_state_from(raw: &str) -> RemoteState {
    match raw {
        "present" => RemoteState::Present,
        "trashed" => RemoteState::Trashed,
        "absent" => RemoteState::Absent,
        _ => RemoteState::Unknown,
    }
}

/// Reads and writes for `media_assets`.
#[derive(Debug, Clone)]
pub struct MediaAssetRepository {
    db: Database,
}

impl MediaAssetRepository {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Insert a new asset. A duplicate `(sha256, byte_length)` is a conflict.
    pub async fn insert(&self, asset: &MediaAsset, created_at: Timestamp) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO media_assets
                 (id, sha256, byte_length, relative_path, codec, quality_class,
                  verification_state, verified_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(asset.id.to_string())
        .bind(asset.identity.sha256.to_hex())
        .bind(i64::try_from(asset.identity.byte_length).unwrap_or(i64::MAX))
        .bind(&asset.relative_path)
        .bind(&asset.identity.observed_codec)
        .bind(quality_class_str(asset.quality_class))
        .bind(verification_str(asset.verification_state))
        .bind(asset.verified_at.map(Timestamp::as_millis))
        .bind(created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Look up an asset by content identity.
    pub async fn find_by_identity(
        &self,
        identity: &MediaIdentity,
    ) -> PersistenceResult<Option<MediaAsset>> {
        let row = sqlx::query(
            "SELECT id, sha256, byte_length, relative_path, codec, quality_class,
                    verification_state, verified_at
             FROM media_assets WHERE sha256 = ?1 AND byte_length = ?2",
        )
        .bind(identity.sha256.to_hex())
        .bind(i64::try_from(identity.byte_length).unwrap_or(i64::MAX))
        .fetch_optional(self.db.pool())
        .await?;
        row.map(|row| Self::from_row(&row)).transpose()
    }

    /// Look up an asset by row identity.
    pub async fn get(&self, id: MediaAssetId) -> PersistenceResult<MediaAsset> {
        let row = sqlx::query(
            "SELECT id, sha256, byte_length, relative_path, codec, quality_class,
                    verification_state, verified_at
             FROM media_assets WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PersistenceError::RecordNotFound)?;
        Self::from_row(&row)
    }

    /// Record the outcome of an integrity check.
    pub async fn set_verification(
        &self,
        id: MediaAssetId,
        state: VerificationState,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "UPDATE media_assets SET verification_state = ?2, verified_at = ?3 WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(verification_str(state))
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Total number of stored assets.
    pub async fn count(&self) -> PersistenceResult<i64> {
        let row = sqlx::query("SELECT COUNT(*) FROM media_assets")
            .fetch_one(self.db.pool())
            .await?;
        Ok(row.get::<i64, _>(0))
    }

    fn from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<MediaAsset> {
        let sha: String = row.get("sha256");
        let length: i64 = row.get("byte_length");
        let codec: Option<String> = row.get("codec");
        Ok(MediaAsset {
            id: row
                .get::<String, _>("id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt media asset id".into()))?,
            identity: MediaIdentity::new(
                sha.parse::<Sha256Digest>()
                    .map_err(|_| PersistenceError::IntegrityFailed)?,
                u64::try_from(length).unwrap_or(0),
                codec.unwrap_or_default(),
            ),
            relative_path: row.get("relative_path"),
            quality_class: quality_class_from(&row.get::<String, _>("quality_class")),
            verification_state: verification_from(&row.get::<String, _>("verification_state")),
            verified_at: row
                .get::<Option<i64>, _>("verified_at")
                .map(Timestamp::from_millis),
        })
    }
}

/// Reads and writes for `recordings` and `recording_media`.
#[derive(Debug, Clone)]
pub struct RecordingRepository {
    db: Database,
}

impl RecordingRepository {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Insert a recording and its tags.
    pub async fn insert(&self, recording: &Recording) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "INSERT INTO recordings (id, title, captured_at, duration_ms, annotation, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(recording.id.to_string())
        .bind(&recording.title)
        .bind(recording.captured_at.map(Timestamp::as_millis))
        .bind(
            recording
                .duration_ms
                .map(|d| i64::try_from(d).unwrap_or(i64::MAX)),
        )
        .bind(recording.annotation.as_deref())
        .bind(recording.created_at.as_millis())
        .execute(&mut *tx)
        .await?;
        for tag in &recording.tags {
            sqlx::query("INSERT INTO recording_tags (recording_id, tag) VALUES (?1, ?2)")
                .bind(recording.id.to_string())
                .bind(tag)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Attach a media candidate without disturbing existing candidates.
    pub async fn attach_media(
        &self,
        recording_id: RecordingId,
        candidate: &RecordingMedia,
    ) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "INSERT INTO recording_media (recording_id, media_asset_id, quality_class, obtained_at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(recording_id.to_string())
        .bind(candidate.media_asset_id.to_string())
        .bind(quality_class_str(candidate.quality_class))
        .bind(candidate.obtained_at.as_millis())
        .execute(&mut *tx)
        .await?;
        // The first candidate becomes preferred; later ones never displace it.
        sqlx::query(
            "UPDATE recordings SET preferred_media_asset_id = ?2
             WHERE id = ?1 AND preferred_media_asset_id IS NULL",
        )
        .bind(recording_id.to_string())
        .bind(candidate.media_asset_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Explicitly promote an attached candidate.
    pub async fn set_preferred_media(
        &self,
        recording_id: RecordingId,
        media_asset_id: MediaAssetId,
    ) -> PersistenceResult<()> {
        let attached = sqlx::query(
            "SELECT 1 FROM recording_media WHERE recording_id = ?1 AND media_asset_id = ?2",
        )
        .bind(recording_id.to_string())
        .bind(media_asset_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        if attached.is_none() {
            return Err(PersistenceError::RecordNotFound);
        }
        sqlx::query("UPDATE recordings SET preferred_media_asset_id = ?2 WHERE id = ?1")
            .bind(recording_id.to_string())
            .bind(media_asset_id.to_string())
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Media candidates attached to a recording, in acquisition order.
    pub async fn media_candidates(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<RecordingMedia>> {
        let rows = sqlx::query(
            "SELECT media_asset_id, quality_class, obtained_at FROM recording_media
             WHERE recording_id = ?1 ORDER BY obtained_at, media_asset_id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RecordingMedia {
                    media_asset_id: row
                        .get::<String, _>("media_asset_id")
                        .parse()
                        .map_err(|_| PersistenceError::Migration("corrupt asset id".into()))?,
                    quality_class: quality_class_from(&row.get::<String, _>("quality_class")),
                    obtained_at: Timestamp::from_millis(row.get::<i64, _>("obtained_at")),
                })
            })
            .collect()
    }

    /// Load a recording with its tags and media candidates.
    pub async fn get(&self, id: RecordingId) -> PersistenceResult<Recording> {
        let row = sqlx::query(
            "SELECT id, title, captured_at, duration_ms, annotation, created_at, deleted_at,
                    preferred_media_asset_id
             FROM recordings WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PersistenceError::RecordNotFound)?;

        let mut recording = Recording::new(
            id,
            row.get::<String, _>("title"),
            Timestamp::from_millis(row.get::<i64, _>("created_at")),
        );
        recording.captured_at = row
            .get::<Option<i64>, _>("captured_at")
            .map(Timestamp::from_millis);
        recording.duration_ms = row
            .get::<Option<i64>, _>("duration_ms")
            .map(|v| u64::try_from(v).unwrap_or(0));
        recording.annotation = row.get("annotation");
        recording.deleted_at = row
            .get::<Option<i64>, _>("deleted_at")
            .map(Timestamp::from_millis);

        let tags =
            sqlx::query("SELECT tag FROM recording_tags WHERE recording_id = ?1 ORDER BY tag")
                .bind(id.to_string())
                .fetch_all(self.db.pool())
                .await?;
        recording.tags = tags
            .into_iter()
            .map(|row| row.get::<String, _>("tag"))
            .collect();

        for candidate in self.media_candidates(id).await? {
            recording
                .attach_media(candidate)
                .map_err(|_| PersistenceError::Migration("duplicate media candidate".into()))?;
        }
        if let Some(preferred) = row.get::<Option<String>, _>("preferred_media_asset_id") {
            let preferred: MediaAssetId = preferred
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt preferred asset id".into()))?;
            recording.set_preferred_media(preferred).map_err(|_| {
                PersistenceError::Migration("preferred asset is not attached".into())
            })?;
        }
        Ok(recording)
    }

    /// Replace the user-editable fields of a recording.
    pub async fn update_user_fields(&self, recording: &Recording) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("UPDATE recordings SET title = ?2, annotation = ?3 WHERE id = ?1")
            .bind(recording.id.to_string())
            .bind(&recording.title)
            .bind(recording.annotation.as_deref())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM recording_tags WHERE recording_id = ?1")
            .bind(recording.id.to_string())
            .execute(&mut *tx)
            .await?;
        for tag in &recording.tags {
            sqlx::query("INSERT INTO recording_tags (recording_id, tag) VALUES (?1, ?2)")
                .bind(recording.id.to_string())
                .bind(tag)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Tombstone a recording without deleting its media or transcripts.
    ///
    /// Spec 7.10: a local deletion must survive a later sync.
    pub async fn tombstone(
        &self,
        id: RecordingId,
        reason: &str,
        at: Timestamp,
        retention_until: Option<Timestamp>,
    ) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("UPDATE recordings SET deleted_at = ?2 WHERE id = ?1")
            .bind(id.to_string())
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO tombstones (id, entity_type, entity_id, reason, created_at, retention_until)
             VALUES (?1, 'recording', ?2, ?3, ?4, ?5)
             ON CONFLICT(entity_type, entity_id) DO UPDATE SET reason = excluded.reason",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(id.to_string())
        .bind(reason)
        .bind(at.as_millis())
        .bind(retention_until.map(Timestamp::as_millis))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Whether a tombstone exists for a recording.
    pub async fn is_tombstoned(&self, id: RecordingId) -> PersistenceResult<bool> {
        let row = sqlx::query(
            "SELECT 1 FROM tombstones WHERE entity_type = 'recording' AND entity_id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }
}

/// Reads and writes for `sources` and `source_items`.
#[derive(Debug, Clone)]
pub struct SourceRepository {
    db: Database,
}

impl SourceRepository {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Register a source, or return the existing one for the same
    /// `(kind, account scope)`.
    pub async fn ensure_source(
        &self,
        kind: SourceKind,
        scope: &SourceAccountScope,
        display_name: &str,
        at: Timestamp,
    ) -> PersistenceResult<SourceId> {
        if let Some(row) =
            sqlx::query("SELECT id FROM sources WHERE source_kind = ?1 AND account_scope = ?2")
                .bind(source_kind_str(kind))
                .bind(scope.as_str())
                .fetch_optional(self.db.pool())
                .await?
        {
            return row
                .get::<String, _>("id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt source id".into()));
        }
        let id = SourceId::new();
        sqlx::query(
            "INSERT INTO sources (id, source_kind, account_scope, display_name, status, created_at)
             VALUES (?1, ?2, ?3, ?4, 'configured', ?5)",
        )
        .bind(id.to_string())
        .bind(source_kind_str(kind))
        .bind(scope.as_str())
        .bind(display_name)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Insert a source association.
    pub async fn insert_item(
        &self,
        item: &SourceAssociation,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO source_items
                 (id, source_id, external_id, recording_id, import_status, last_seen_at,
                  remote_version, remote_state, tombstone_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(item.id.to_string())
        .bind(item.source_id.to_string())
        .bind(
            item.identity
                .external_id
                .as_ref()
                .map(ExternalRecordingId::as_str),
        )
        .bind(item.recording_id.to_string())
        .bind(import_status_str(item.import_status))
        .bind(item.last_seen_at.map(Timestamp::as_millis))
        .bind(item.remote_version.as_deref())
        .bind(remote_state_str(item.remote_state))
        .bind(item.tombstone_reason.as_deref())
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Find an association by its source-scoped identity triple.
    pub async fn find_by_identity(
        &self,
        source_id: SourceId,
        identity: &SourceIdentityKey,
    ) -> PersistenceResult<Option<SourceAssociation>> {
        let Some(external) = identity.external_id.as_ref() else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT si.id, si.source_id, si.external_id, si.recording_id, si.import_status,
                    si.last_seen_at, si.remote_version, si.remote_state, si.tombstone_reason,
                    s.source_kind, s.account_scope
             FROM source_items si JOIN sources s ON s.id = si.source_id
             WHERE si.source_id = ?1 AND si.external_id = ?2",
        )
        .bind(source_id.to_string())
        .bind(external.as_str())
        .fetch_optional(self.db.pool())
        .await?;

        row.map(|row| {
            Ok(SourceAssociation {
                id: row
                    .get::<String, _>("id")
                    .parse::<SourceItemId>()
                    .map_err(|_| PersistenceError::Migration("corrupt source item id".into()))?,
                source_id,
                identity: SourceIdentityKey::new(
                    source_kind_from(&row.get::<String, _>("source_kind")),
                    SourceAccountScope::new(row.get::<String, _>("account_scope")),
                    row.get::<Option<String>, _>("external_id")
                        .map(ExternalRecordingId::new),
                ),
                recording_id: row
                    .get::<String, _>("recording_id")
                    .parse()
                    .map_err(|_| PersistenceError::Migration("corrupt recording id".into()))?,
                import_status: import_status_from(&row.get::<String, _>("import_status")),
                remote_state: remote_state_from(&row.get::<String, _>("remote_state")),
                remote_version: row.get("remote_version"),
                last_seen_at: row
                    .get::<Option<i64>, _>("last_seen_at")
                    .map(Timestamp::from_millis),
                tombstone_reason: row.get("tombstone_reason"),
            })
        })
        .transpose()
    }

    /// Update only the source-side metadata of an association.
    ///
    /// Spec 7.2: a metadata or remote-version change never touches the
    /// immutable media it points at.
    pub async fn update_remote_metadata(
        &self,
        id: SourceItemId,
        remote_version: Option<&str>,
        remote_state: RemoteState,
        last_seen_at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "UPDATE source_items
             SET remote_version = ?2, remote_state = ?3, last_seen_at = ?4
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(remote_version)
        .bind(remote_state_str(remote_state))
        .bind(last_seen_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }
}
