//! Repositories over the core identity tables.

use plaudon_domain::ids::{
    MediaAssetId, RecordingId, SourceId, SourceItemId, TranscriptRevisionId,
};
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
use uuid::Uuid;

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

    /// Look up an asset by content digest alone.
    ///
    /// A reconciliation pass learns a digest from a listing before it knows
    /// the byte length, and the digest is already unique in practice because
    /// `ux_media_sha` keys on `(sha256, byte_length)`.
    pub async fn find_by_sha256(&self, sha256: &str) -> PersistenceResult<Option<MediaAsset>> {
        let row = sqlx::query(
            "SELECT id, sha256, byte_length, relative_path, codec, quality_class,
                    verification_state, verified_at
             FROM media_assets WHERE sha256 = ?1 ORDER BY byte_length LIMIT 1",
        )
        .bind(sha256)
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

    /// Lift a recording tombstone, for one recording, on explicit user action.
    ///
    /// Spec 7.10: resurrection is a user decision. Nothing in a sync pass may
    /// call this; the only caller is the Reimport command.
    pub async fn clear_tombstone(&self, id: RecordingId) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("UPDATE recordings SET deleted_at = NULL WHERE id = ?1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM tombstones WHERE entity_type = 'recording' AND entity_id = ?1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Fill in a duration the source reported, but never overwrite one.
    ///
    /// App. A.2 "reconcile metadata": source-side metadata may complete a
    /// record, never contradict a value we measured from the bytes we hold.
    pub async fn set_duration_if_unset(
        &self,
        id: RecordingId,
        duration_ms: u64,
    ) -> PersistenceResult<bool> {
        let result = sqlx::query(
            "UPDATE recordings SET duration_ms = ?2 WHERE id = ?1 AND duration_ms IS NULL",
        )
        .bind(id.to_string())
        .bind(i64::try_from(duration_ms).unwrap_or(i64::MAX))
        .execute(self.db.pool())
        .await?;
        Ok(result.rows_affected() > 0)
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

        row.as_ref().map(association_from_row).transpose()
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

    /// Resolve the configured source for an identity triple without creating
    /// one. Reconciliation must never conjure a source row as a side effect.
    pub async fn find_source_id(
        &self,
        kind: SourceKind,
        scope: &SourceAccountScope,
    ) -> PersistenceResult<Option<SourceId>> {
        let row =
            sqlx::query("SELECT id FROM sources WHERE source_kind = ?1 AND account_scope = ?2")
                .bind(source_kind_str(kind))
                .bind(scope.as_str())
                .fetch_optional(self.db.pool())
                .await?;
        row.map(|row| {
            row.get::<String, _>("id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt source id".into()))
        })
        .transpose()
    }

    /// Every association belonging to one configured source, in insertion
    /// order. Used by a reconciliation pass to find items the listing no
    /// longer mentions (spec 7.5 "removed from listing").
    pub async fn list_items(
        &self,
        source_id: SourceId,
    ) -> PersistenceResult<Vec<SourceAssociation>> {
        let rows = sqlx::query(
            "SELECT si.id, si.source_id, si.external_id, si.recording_id, si.import_status,
                    si.last_seen_at, si.remote_version, si.remote_state, si.tombstone_reason,
                    s.source_kind, s.account_scope
             FROM source_items si JOIN sources s ON s.id = si.source_id
             WHERE si.source_id = ?1
             ORDER BY si.created_at, si.id",
        )
        .bind(source_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(association_from_row).collect()
    }

    /// Load one association by row identity.
    pub async fn get_item(&self, id: SourceItemId) -> PersistenceResult<SourceAssociation> {
        let row = sqlx::query(
            "SELECT si.id, si.source_id, si.external_id, si.recording_id, si.import_status,
                    si.last_seen_at, si.remote_version, si.remote_state, si.tombstone_reason,
                    s.source_kind, s.account_scope
             FROM source_items si JOIN sources s ON s.id = si.source_id
             WHERE si.id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PersistenceError::RecordNotFound)?;
        association_from_row(&row)
    }

    /// What we actually hashed and whether those bytes reached the archive.
    ///
    /// App. A.2: an unchanged remote version still re-downloads when the prior
    /// acquisition never completed, which is what keeps idempotence from
    /// turning into a stall.
    pub async fn acquisition_state(&self, id: SourceItemId) -> PersistenceResult<AcquisitionState> {
        let row = sqlx::query(
            "SELECT observed_content_sha256, acquisition_complete FROM source_items WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PersistenceError::RecordNotFound)?;
        Ok(AcquisitionState {
            observed_content_sha256: row.get("observed_content_sha256"),
            complete: row.get::<i64, _>("acquisition_complete") != 0,
        })
    }

    /// Record the digest of the bytes we hold and whether acquisition finished.
    pub async fn set_acquisition_state(
        &self,
        id: SourceItemId,
        state: &AcquisitionState,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "UPDATE source_items
             SET observed_content_sha256 = ?2, acquisition_complete = ?3
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(state.observed_content_sha256.as_deref())
        .bind(i64::from(state.complete))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Mark the remote side only.
    ///
    /// Spec 7.10 and App. A.2: a trashed or absent remote item changes this
    /// column and nothing else. No media, no revision and no derivative is
    /// touched, because a remote deletion is not deletion authority.
    pub async fn set_remote_state(
        &self,
        id: SourceItemId,
        remote_state: RemoteState,
        last_seen_at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query("UPDATE source_items SET remote_state = ?2, last_seen_at = ?3 WHERE id = ?1")
            .bind(id.to_string())
            .bind(remote_state_str(remote_state))
            .bind(last_seen_at.as_millis())
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Persist a durable refusal to re-import this item.
    ///
    /// The reason is denormalised onto `source_items.tombstone_reason` for
    /// cheap reads and written to the shared `tombstones` table so it survives
    /// restart and outlives the association row's own lifecycle (spec 13.3A).
    pub async fn tombstone_item(
        &self,
        id: SourceItemId,
        reason: &str,
        at: Timestamp,
        retention_until: Option<Timestamp>,
    ) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("UPDATE source_items SET tombstone_reason = ?2 WHERE id = ?1")
            .bind(id.to_string())
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO tombstones (id, entity_type, entity_id, reason, created_at, retention_until)
             VALUES (?1, 'source_item', ?2, ?3, ?4, ?5)
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

    /// Whether a durable refusal exists for this association.
    pub async fn is_item_tombstoned(&self, id: SourceItemId) -> PersistenceResult<bool> {
        let row = sqlx::query(
            "SELECT 1 FROM tombstones WHERE entity_type = 'source_item' AND entity_id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    /// Lift the refusal for exactly one association.
    ///
    /// Spec 7.10: only an explicit user Reimport clears a tombstone. Nothing
    /// in a sync pass may call this, and it clears one entity, never a set.
    pub async fn clear_item_tombstone(&self, id: SourceItemId) -> PersistenceResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("UPDATE source_items SET tombstone_reason = NULL WHERE id = ?1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM tombstones WHERE entity_type = 'source_item' AND entity_id = ?1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// What we actually hashed for a source item, and whether those bytes reached
/// the archive.
///
/// App. A.2 keeps these two facts apart from `remote_version` on purpose: the
/// version is a vendor hint, the digest is evidence, and completeness is the
/// only thing that decides whether an unchanged version still re-downloads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcquisitionState {
    /// Digest of the bytes we hold, once any have been hashed.
    pub observed_content_sha256: Option<String>,
    /// Whether the acquisition that produced them finished.
    pub complete: bool,
}

/// Map one joined `source_items`/`sources` row onto the domain association.
fn association_from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<SourceAssociation> {
    Ok(SourceAssociation {
        id: row
            .get::<String, _>("id")
            .parse::<SourceItemId>()
            .map_err(|_| PersistenceError::Migration("corrupt source item id".into()))?,
        source_id: row
            .get::<String, _>("source_id")
            .parse()
            .map_err(|_| PersistenceError::Migration("corrupt source id".into()))?,
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
}

/// One row of `media_revisions` (spec 13.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRevision {
    /// Row identity.
    pub id: Uuid,
    /// Recording the revision belongs to.
    pub recording_id: RecordingId,
    /// Immutable bytes this revision points at.
    pub media_asset_id: MediaAssetId,
    /// Source association the bytes arrived through, when known.
    pub source_item_id: Option<SourceItemId>,
    /// Vendor version hint recorded alongside the revision, never used as
    /// proof that the bytes changed.
    pub source_version: Option<String>,
    /// The revision this one supersedes, which remains readable.
    pub predecessor_id: Option<Uuid>,
    /// When the bytes were obtained.
    pub obtained_at: Timestamp,
}

/// Append-only reads and writes for `media_revisions`.
#[derive(Debug, Clone)]
pub struct MediaRevisionRepository {
    db: Database,
}

impl MediaRevisionRepository {
    /// Wrap a database handle.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Append a revision, chaining it to the newest existing one.
    ///
    /// Spec 7.5: changed source content appends; it never overwrites. The
    /// previous revision keeps pointing at the original asset, so every
    /// derivative that cited it stays resolvable.
    pub async fn append(
        &self,
        recording_id: RecordingId,
        media_asset_id: MediaAssetId,
        source_item_id: Option<SourceItemId>,
        source_version: Option<&str>,
        at: Timestamp,
    ) -> PersistenceResult<MediaRevision> {
        let predecessor = self.latest(recording_id).await?.map(|rev| rev.id);
        let revision = MediaRevision {
            id: Uuid::now_v7(),
            recording_id,
            media_asset_id,
            source_item_id,
            source_version: source_version.map(ToOwned::to_owned),
            predecessor_id: predecessor,
            obtained_at: at,
        };
        sqlx::query(
            "INSERT INTO media_revisions
                 (id, recording_id, media_asset_id, source_item_id, source_version,
                  predecessor_id, obtained_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(revision.id.to_string())
        .bind(recording_id.to_string())
        .bind(media_asset_id.to_string())
        .bind(source_item_id.map(|id| id.to_string()))
        .bind(revision.source_version.as_deref())
        .bind(predecessor.map(|id| id.to_string()))
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(revision)
    }

    /// Every revision of a recording, oldest first.
    pub async fn list(&self, recording_id: RecordingId) -> PersistenceResult<Vec<MediaRevision>> {
        let rows = sqlx::query(
            "SELECT id, recording_id, media_asset_id, source_item_id, source_version,
                    predecessor_id, obtained_at
             FROM media_revisions WHERE recording_id = ?1 ORDER BY obtained_at, id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::from_row).collect()
    }

    /// The newest revision of a recording, if any.
    pub async fn latest(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Option<MediaRevision>> {
        let row = sqlx::query(
            "SELECT id, recording_id, media_asset_id, source_item_id, source_version,
                    predecessor_id, obtained_at
             FROM media_revisions WHERE recording_id = ?1 ORDER BY obtained_at DESC, id DESC
             LIMIT 1",
        )
        .bind(recording_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        row.as_ref().map(Self::from_row).transpose()
    }

    fn from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<MediaRevision> {
        let parse_uuid = |raw: &str| {
            Uuid::parse_str(raw).map_err(|_| PersistenceError::Migration("corrupt revision".into()))
        };
        Ok(MediaRevision {
            id: parse_uuid(&row.get::<String, _>("id"))?,
            recording_id: row
                .get::<String, _>("recording_id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt recording id".into()))?,
            media_asset_id: row
                .get::<String, _>("media_asset_id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt asset id".into()))?,
            source_item_id: row
                .get::<Option<String>, _>("source_item_id")
                .map(|raw| raw.parse())
                .transpose()
                .map_err(|_| PersistenceError::Migration("corrupt source item id".into()))?,
            source_version: row.get("source_version"),
            predecessor_id: row
                .get::<Option<String>, _>("predecessor_id")
                .map(|raw| parse_uuid(&raw))
                .transpose()?,
            obtained_at: Timestamp::from_millis(row.get::<i64, _>("obtained_at")),
        })
    }
}

/// Provenance of one transcript revision, as far as reconciliation cares.
///
/// [`crate::transcript::TranscriptRepository`] (EPIC F) owns writing revisions
/// and reading their engine detail. This view adds the two facts that decide
/// whether a vendor export may touch anything: who produced the revision, and
/// whether a human corrected it (spec 7.5, App. A.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionProvenance {
    /// Revision identity.
    pub id: TranscriptRevisionId,
    /// `machine`, `human_edit`, `derived_alignment` or `overlap_reconciliation`.
    pub origin: String,
    /// Vendor that produced it; `None` means produced locally.
    pub source_vendor: Option<String>,
    /// When the revision was written.
    pub created_at: Timestamp,
}

impl RevisionProvenance {
    /// Whether a human corrected this revision.
    pub fn is_human_edit(&self) -> bool {
        self.origin == "human_edit"
    }

    /// Whether this revision came out of a vendor export rather than a local
    /// or configured engine.
    pub fn is_vendor(&self) -> bool {
        self.source_vendor.is_some()
    }
}

/// Vendor provenance over the `transcript_revisions` rows EPIC F writes.
#[derive(Debug, Clone)]
pub struct TranscriptProvenanceRepository {
    db: Database,
}

impl TranscriptProvenanceRepository {
    /// Wrap a database handle.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Tag an already-written revision as produced by a vendor export.
    ///
    /// Spec 7.5: vendor content is identified, never disguised as ours. The
    /// tag is write-once; re-tagging a revision with a different vendor is a
    /// conflict rather than a silent relabel.
    pub async fn set_source_vendor(
        &self,
        id: TranscriptRevisionId,
        vendor: &str,
    ) -> PersistenceResult<()> {
        let result = sqlx::query(
            "UPDATE transcript_revisions SET source_vendor = ?2
             WHERE id = ?1 AND (source_vendor IS NULL OR source_vendor = ?2)",
        )
        .bind(id.to_string())
        .bind(vendor)
        .execute(self.db.pool())
        .await?;
        if result.rows_affected() == 0 {
            return Err(PersistenceError::Conflict);
        }
        Ok(())
    }

    /// Vendor recorded for one revision, if any.
    pub async fn source_vendor(
        &self,
        id: TranscriptRevisionId,
    ) -> PersistenceResult<Option<String>> {
        let row = sqlx::query("SELECT source_vendor FROM transcript_revisions WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(self.db.pool())
            .await?
            .ok_or(PersistenceError::RecordNotFound)?;
        Ok(row.get("source_vendor"))
    }

    /// Provenance of every revision of a recording, oldest first.
    pub async fn list_for_recording(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<RevisionProvenance>> {
        let rows = sqlx::query(
            "SELECT id, origin, source_vendor, created_at FROM transcript_revisions
             WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::from_row).collect()
    }

    /// Whether the user has corrected a transcript of this recording.
    ///
    /// App. A.2: a vendor export must never silently replace human-corrected
    /// text, so the importer asks this before doing anything.
    pub async fn has_human_edit(&self, recording_id: RecordingId) -> PersistenceResult<bool> {
        let row = sqlx::query(
            "SELECT 1 FROM transcript_revisions
             WHERE recording_id = ?1 AND origin = 'human_edit' LIMIT 1",
        )
        .bind(recording_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.is_some())
    }

    /// The revision a reader should be shown.
    ///
    /// Precedence is fixed, and it is the reason a vendor import cannot demote
    /// anything: newest human edit, else newest locally produced revision,
    /// else newest vendor revision.
    pub async fn preferred(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Option<RevisionProvenance>> {
        let row = sqlx::query(
            "SELECT id, origin, source_vendor, created_at FROM transcript_revisions
             WHERE recording_id = ?1
             ORDER BY CASE
                          WHEN origin = 'human_edit' THEN 0
                          WHEN source_vendor IS NULL THEN 1
                          ELSE                            2
                      END,
                      created_at DESC, id DESC
             LIMIT 1",
        )
        .bind(recording_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        row.as_ref().map(Self::from_row).transpose()
    }

    fn from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<RevisionProvenance> {
        Ok(RevisionProvenance {
            id: row
                .get::<String, _>("id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt revision id".into()))?,
            origin: row.get("origin"),
            source_vendor: row.get("source_vendor"),
            created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
        })
    }
}

/// One row of `source_export_artifacts` (spec 7.5, 13.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceExportArtifact {
    /// Row identity.
    pub id: Uuid,
    /// Association the artifact was exported alongside.
    pub source_item_id: SourceItemId,
    /// `transcript`, `summary` or `tags`.
    pub artifact_kind: String,
    /// Vendor that produced it, for example `plaud`.
    pub source_vendor: String,
    /// Digest of the original exported bytes.
    pub content_sha256: String,
    /// Length of those bytes.
    pub byte_length: u64,
    /// Name the export used, kept as metadata only.
    pub original_filename: Option<String>,
    /// When the artifact became a revision; `None` while only recorded.
    pub imported_at: Option<Timestamp>,
    /// The mode the user had configured when we saw it.
    pub import_mode: String,
    /// When the row was written.
    pub created_at: Timestamp,
}

/// Reads and writes for `source_export_artifacts`.
#[derive(Debug, Clone)]
pub struct SourceExportArtifactRepository {
    db: Database,
}

impl SourceExportArtifactRepository {
    /// Wrap a database handle.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Record an artifact we observed, imported or refused.
    ///
    /// Recording a refusal matters: it is how the UI can explain that a vendor
    /// transcript exists and was deliberately not imported.
    pub async fn record(&self, artifact: &SourceExportArtifact) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO source_export_artifacts
                 (id, source_item_id, artifact_kind, source_vendor, content_sha256,
                  byte_length, original_filename, imported_at, import_mode, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(source_item_id, artifact_kind, content_sha256) DO UPDATE SET
                 imported_at = excluded.imported_at,
                 import_mode = excluded.import_mode",
        )
        .bind(artifact.id.to_string())
        .bind(artifact.source_item_id.to_string())
        .bind(&artifact.artifact_kind)
        .bind(&artifact.source_vendor)
        .bind(&artifact.content_sha256)
        .bind(i64::try_from(artifact.byte_length).unwrap_or(i64::MAX))
        .bind(artifact.original_filename.as_deref())
        .bind(artifact.imported_at.map(Timestamp::as_millis))
        .bind(&artifact.import_mode)
        .bind(artifact.created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Artifacts recorded for one association, oldest first.
    pub async fn list_for_item(
        &self,
        source_item_id: SourceItemId,
    ) -> PersistenceResult<Vec<SourceExportArtifact>> {
        let rows = sqlx::query(
            "SELECT id, source_item_id, artifact_kind, source_vendor, content_sha256,
                    byte_length, original_filename, imported_at, import_mode, created_at
             FROM source_export_artifacts WHERE source_item_id = ?1 ORDER BY created_at, id",
        )
        .bind(source_item_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::from_row).collect()
    }

    /// Artifacts recorded for every association of one recording.
    ///
    /// A recording can carry associations from more than one source, so the
    /// UI asks by recording and gets the union, each row still naming the
    /// association and vendor it came from.
    pub async fn list_for_recording(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<SourceExportArtifact>> {
        let rows = sqlx::query(
            "SELECT a.id, a.source_item_id, a.artifact_kind, a.source_vendor, a.content_sha256,
                    a.byte_length, a.original_filename, a.imported_at, a.import_mode, a.created_at
             FROM source_export_artifacts a
             JOIN source_items si ON si.id = a.source_item_id
             WHERE si.recording_id = ?1
             ORDER BY a.created_at, a.id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::from_row).collect()
    }

    fn from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<SourceExportArtifact> {
        Ok(SourceExportArtifact {
            id: Uuid::parse_str(&row.get::<String, _>("id"))
                .map_err(|_| PersistenceError::Migration("corrupt artifact id".into()))?,
            source_item_id: row
                .get::<String, _>("source_item_id")
                .parse()
                .map_err(|_| PersistenceError::Migration("corrupt item id".into()))?,
            artifact_kind: row.get("artifact_kind"),
            source_vendor: row.get("source_vendor"),
            content_sha256: row.get("content_sha256"),
            byte_length: u64::try_from(row.get::<i64, _>("byte_length")).unwrap_or(0),
            original_filename: row.get("original_filename"),
            imported_at: row
                .get::<Option<i64>, _>("imported_at")
                .map(Timestamp::from_millis),
            import_mode: row.get("import_mode"),
            created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
        })
    }
}

/// One configured watched folder (spec 4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedFolderRow {
    /// Row identity.
    pub id: Uuid,
    /// Canonical root, as validated at configuration time.
    pub root_path: String,
    /// Whether subdirectories are included.
    pub recursive: bool,
    /// `import_only`, `import_and_queue_local_stt` or `ask_before_cloud`.
    pub rule: String,
    /// Quiescence window in milliseconds.
    pub quiescence_ms: u64,
    /// `active`, `paused`, `unavailable` or `permission_denied`.
    pub status: String,
    /// When the last bounded inventory sweep finished.
    pub last_inventory_at: Option<Timestamp>,
    /// When the folder was configured.
    pub created_at: Timestamp,
}

/// One observed file below a watched folder (spec 4.3(5)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedFolderItemRow {
    /// Row identity.
    pub id: Uuid,
    /// Folder the file was seen under.
    pub folder_id: Uuid,
    /// Where it was seen. Provenance, never identity.
    pub origin_path: String,
    /// Inode and device, or Windows file index, when the platform offers one.
    pub stable_file_id: Option<String>,
    /// Size at the last probe.
    pub size_bytes: u64,
    /// Modification time at the last probe, in milliseconds.
    pub mtime_ms: i64,
    /// Digest once the completion barrier staged one.
    pub content_sha256: Option<String>,
    /// Barrier state.
    pub state: String,
    /// Association created when the file was imported.
    pub source_item_id: Option<SourceItemId>,
    /// First observation.
    pub first_seen_at: Timestamp,
    /// Most recent observation.
    pub last_checked_at: Timestamp,
    /// When the file stopped being reachable, if it has.
    pub unavailable_since: Option<Timestamp>,
}

/// Reads and writes for `watched_folders` and `watched_folder_items`.
#[derive(Debug, Clone)]
pub struct WatchedFolderRepository {
    db: Database,
}

impl WatchedFolderRepository {
    /// Wrap a database handle.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Register a folder, or return the row already registered for this root.
    pub async fn ensure_folder(&self, folder: &WatchedFolderRow) -> PersistenceResult<Uuid> {
        if let Some(existing) = self.find_folder(&folder.root_path).await? {
            return Ok(existing.id);
        }
        sqlx::query(
            "INSERT INTO watched_folders
                 (id, root_path, recursive, rule, quiescence_ms, status, last_inventory_at,
                  created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(folder.id.to_string())
        .bind(&folder.root_path)
        .bind(i64::from(folder.recursive))
        .bind(&folder.rule)
        .bind(i64::try_from(folder.quiescence_ms).unwrap_or(i64::MAX))
        .bind(&folder.status)
        .bind(folder.last_inventory_at.map(Timestamp::as_millis))
        .bind(folder.created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(folder.id)
    }

    /// Look a folder up by its canonical root.
    pub async fn find_folder(
        &self,
        root_path: &str,
    ) -> PersistenceResult<Option<WatchedFolderRow>> {
        let row = sqlx::query(
            "SELECT id, root_path, recursive, rule, quiescence_ms, status, last_inventory_at,
                    created_at
             FROM watched_folders WHERE root_path = ?1",
        )
        .bind(root_path)
        .fetch_optional(self.db.pool())
        .await?;
        row.as_ref()
            .map(|row| {
                Ok(WatchedFolderRow {
                    id: Uuid::parse_str(&row.get::<String, _>("id"))
                        .map_err(|_| PersistenceError::Migration("corrupt folder id".into()))?,
                    root_path: row.get("root_path"),
                    recursive: row.get::<i64, _>("recursive") != 0,
                    rule: row.get("rule"),
                    quiescence_ms: u64::try_from(row.get::<i64, _>("quiescence_ms")).unwrap_or(0),
                    status: row.get("status"),
                    last_inventory_at: row
                        .get::<Option<i64>, _>("last_inventory_at")
                        .map(Timestamp::from_millis),
                    created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
                })
            })
            .transpose()
    }

    /// Record the status of a folder, for example after a permission failure.
    pub async fn set_folder_status(&self, id: Uuid, status: &str) -> PersistenceResult<()> {
        sqlx::query("UPDATE watched_folders SET status = ?2 WHERE id = ?1")
            .bind(id.to_string())
            .bind(status)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Stamp the end of a bounded inventory sweep.
    pub async fn set_last_inventory_at(&self, id: Uuid, at: Timestamp) -> PersistenceResult<()> {
        sqlx::query("UPDATE watched_folders SET last_inventory_at = ?2 WHERE id = ?1")
            .bind(id.to_string())
            .bind(at.as_millis())
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Insert or update the observation for one path.
    ///
    /// `first_seen_at` is preserved on update: the point of the row is to know
    /// how long a file has been waiting for the barrier.
    pub async fn upsert_item(&self, item: &WatchedFolderItemRow) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO watched_folder_items
                 (id, folder_id, origin_path, stable_file_id, size_bytes, mtime_ms,
                  content_sha256, state, source_item_id, first_seen_at, last_checked_at,
                  unavailable_since)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(folder_id, origin_path) DO UPDATE SET
                 stable_file_id    = excluded.stable_file_id,
                 size_bytes        = excluded.size_bytes,
                 mtime_ms          = excluded.mtime_ms,
                 content_sha256    = COALESCE(excluded.content_sha256, watched_folder_items.content_sha256),
                 state             = excluded.state,
                 source_item_id    = COALESCE(excluded.source_item_id, watched_folder_items.source_item_id),
                 last_checked_at   = excluded.last_checked_at,
                 unavailable_since = excluded.unavailable_since",
        )
        .bind(item.id.to_string())
        .bind(item.folder_id.to_string())
        .bind(&item.origin_path)
        .bind(item.stable_file_id.as_deref())
        .bind(i64::try_from(item.size_bytes).unwrap_or(i64::MAX))
        .bind(item.mtime_ms)
        .bind(item.content_sha256.as_deref())
        .bind(&item.state)
        .bind(item.source_item_id.map(|id| id.to_string()))
        .bind(item.first_seen_at.as_millis())
        .bind(item.last_checked_at.as_millis())
        .bind(item.unavailable_since.map(Timestamp::as_millis))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// The observation recorded for one path, if any.
    pub async fn find_item(
        &self,
        folder_id: Uuid,
        origin_path: &str,
    ) -> PersistenceResult<Option<WatchedFolderItemRow>> {
        let row = sqlx::query(
            "SELECT id, folder_id, origin_path, stable_file_id, size_bytes, mtime_ms,
                    content_sha256, state, source_item_id, first_seen_at, last_checked_at,
                    unavailable_since
             FROM watched_folder_items WHERE folder_id = ?1 AND origin_path = ?2",
        )
        .bind(folder_id.to_string())
        .bind(origin_path)
        .fetch_optional(self.db.pool())
        .await?;
        row.as_ref().map(Self::item_from_row).transpose()
    }

    /// Every observation under a folder, in path order.
    pub async fn list_items(
        &self,
        folder_id: Uuid,
    ) -> PersistenceResult<Vec<WatchedFolderItemRow>> {
        let rows = sqlx::query(
            "SELECT id, folder_id, origin_path, stable_file_id, size_bytes, mtime_ms,
                    content_sha256, state, source_item_id, first_seen_at, last_checked_at,
                    unavailable_since
             FROM watched_folder_items WHERE folder_id = ?1 ORDER BY origin_path",
        )
        .bind(folder_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(Self::item_from_row).collect()
    }

    /// Mark a source file we can no longer reach.
    ///
    /// Spec 4.3(6): this changes the association's availability and nothing
    /// else. The archived recording and every derivative stay exactly as they
    /// were, which is why this method cannot touch another table.
    pub async fn mark_unavailable(
        &self,
        folder_id: Uuid,
        origin_path: &str,
        at: Timestamp,
    ) -> PersistenceResult<bool> {
        let result = sqlx::query(
            "UPDATE watched_folder_items
             SET state = 'unavailable', unavailable_since = COALESCE(unavailable_since, ?3),
                 last_checked_at = ?3
             WHERE folder_id = ?1 AND origin_path = ?2 AND state <> 'unavailable'",
        )
        .bind(folder_id.to_string())
        .bind(origin_path)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    fn item_from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<WatchedFolderItemRow> {
        let parse_uuid = |raw: &str| {
            Uuid::parse_str(raw)
                .map_err(|_| PersistenceError::Migration("corrupt watched folder row".into()))
        };
        Ok(WatchedFolderItemRow {
            id: parse_uuid(&row.get::<String, _>("id"))?,
            folder_id: parse_uuid(&row.get::<String, _>("folder_id"))?,
            origin_path: row.get("origin_path"),
            stable_file_id: row.get("stable_file_id"),
            size_bytes: u64::try_from(row.get::<i64, _>("size_bytes")).unwrap_or(0),
            mtime_ms: row.get("mtime_ms"),
            content_sha256: row.get("content_sha256"),
            state: row.get("state"),
            source_item_id: row
                .get::<Option<String>, _>("source_item_id")
                .map(|raw| raw.parse())
                .transpose()
                .map_err(|_| PersistenceError::Migration("corrupt source item id".into()))?,
            first_seen_at: Timestamp::from_millis(row.get::<i64, _>("first_seen_at")),
            last_checked_at: Timestamp::from_millis(row.get::<i64, _>("last_checked_at")),
            unavailable_since: row
                .get::<Option<i64>, _>("unavailable_since")
                .map(Timestamp::from_millis),
        })
    }
}
