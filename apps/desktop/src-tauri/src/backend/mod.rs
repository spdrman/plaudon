//! The EPIC E port implementation behind the [`crate::ports`] traits.
//!
//! Everything durable lives in EPIC B's library: `plaudon_persistence`
//! provides the database, the migration runner, the content-addressed
//! [`MediaArchive`] and the repositories, and `plaudon_app_core::import`
//! provides the import use case. This module only adapts those to the wire
//! contract the UI consumes, plus the three tables nothing else owns yet
//! (`activity_events`, `event_log`, `recording_import_metadata`, migration
//! 0300).
//!
//! EPIC B's `crates/persistence` and EPIC C's `crates/jobs` supersede this
//! implementation by swapping the port implementations constructed in
//! [`crate::build_ports`], with no change to the commands, the schemas or the
//! UI.

pub mod activity;
pub mod capabilities;
pub mod export;
pub mod import;
pub mod jobs;
pub mod store;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use plaudon_app_core::import::ImportService;
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorEnvelope};
use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment as DomainSegment,
};
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::repo::{MediaAssetRepository, RecordingRepository, SourceRepository};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use sqlx::{Row, SqlitePool};

use crate::backend::store::{as_u32, as_u64, rfc3339, rfc3339_opt, storage_error, Store};
use crate::dto::{
    Appearance, Contrast, EventKind, ImportStatus, LocalOnlyModeResponse, NotificationCategory,
    NotificationSettings, NotificationSettingsResponse, OsNotificationPermission, PermissionState,
    Preferences, RecordingDetail, RecordingMediaInfo, RecordingPage, RecordingSortField,
    RecordingSourceRef, RecordingSummary, RevisionKind, SaveTranscriptRevisionResponse,
    SegmentConfidence, SortDirection, SourceKind, TranscriptPage, TranscriptRevisionRef,
    TranscriptSegmentDto, TranscriptionStatus,
};
use crate::error::{internal, invalid_argument};
use crate::events::{EventDraft, EventPublisher, EventSink};
use crate::notify::NotificationService;
use crate::ports::{LibraryPort, MachineRevisionSpec, ResolvedMedia, SearchPort, SettingsPort,
    SpeakersPort, TranscriptPort};
use crate::secrets::SecretStore;
use crate::validate::{
    PageCursor, ValidatedGetTranscript, ValidatedListRecordings, ValidatedRecordingFilter,
    ValidatedSaveRevision,
};

/// `app_preferences` key holding the serialized [`Preferences`].
const PREFERENCES_KEY: &str = "desktop.preferences";
/// `app_preferences` key holding the last observed OS notification permission.
const NOTIFICATION_PERMISSION_KEY: &str = "desktop.notification_os_permission";
/// Separator used to pack a tag list into one scalar column.
const TAG_SEPARATOR: char = '\u{1f}';

/// The desktop implementation of every port.
#[derive(Debug, Clone)]
pub struct LocalBackend {
    store: Store,
    imports: ImportService,
    recordings: RecordingRepository,
    assets: MediaAssetRepository,
    sources: SourceRepository,
    events: EventPublisher,
    notifications: NotificationService,
    secrets: Arc<dyn SecretStore>,
    approved_roots: Vec<PathBuf>,
}

impl LocalBackend {
    /// Open the library under `root` and build the backend.
    pub async fn open(
        root: &Path,
        sink: Arc<dyn EventSink>,
        notifications: Arc<plaudon_notifications::Dispatcher>,
        secrets: Arc<dyn SecretStore>,
        approved_roots: Vec<PathBuf>,
    ) -> Result<Self, AppError> {
        let store = Store::open(root).await?;
        let db: Database = store.db().clone();
        let imports = ImportService::new(db.clone(), store.archive().clone());
        let events = EventPublisher::new(store.pool().clone(), sink);
        let notifications = NotificationService::new(notifications, store.pool().clone());
        let backend = Self {
            recordings: RecordingRepository::new(db.clone()),
            assets: MediaAssetRepository::new(db.clone()),
            sources: SourceRepository::new(db),
            imports,
            events,
            notifications,
            secrets,
            approved_roots,
            store,
        };
        // The dispatcher starts with whatever the user last chose; nothing
        // prompts the OS here (spec §5.5: notifications are opt-in).
        let preferences = backend.preferences().await?;
        backend
            .notifications
            .dispatcher()
            .set_settings(crate::notify::to_crate_settings(&preferences.notifications))
            .await;
        Ok(backend)
    }

    /// Reconcile any import interrupted by a crash (spec §13.3A).
    pub async fn reconcile_startup(&self) -> Result<usize, AppError> {
        let outcomes = self
            .imports
            .journal()
            .reconcile(Timestamp::now())
            .await
            .map_err(AppError::from)?;
        Ok(outcomes.len())
    }

    /// The connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        self.store.pool()
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The media archive.
    #[must_use]
    pub fn archive(&self) -> &MediaArchive {
        self.store.archive()
    }

    /// The import use case.
    #[must_use]
    pub fn imports(&self) -> &ImportService {
        &self.imports
    }

    /// The recording repository.
    #[must_use]
    pub fn recordings(&self) -> &RecordingRepository {
        &self.recordings
    }

    /// The media asset repository.
    #[must_use]
    pub fn assets(&self) -> &MediaAssetRepository {
        &self.assets
    }

    /// The source repository.
    #[must_use]
    pub fn sources(&self) -> &SourceRepository {
        &self.sources
    }

    /// The event publisher.
    #[must_use]
    pub fn events(&self) -> &EventPublisher {
        &self.events
    }

    /// The notification service.
    #[must_use]
    pub fn notifications(&self) -> &NotificationService {
        &self.notifications
    }

    /// The secret store.
    #[must_use]
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// Roots the user has approved for import.
    #[must_use]
    pub fn approved_roots(&self) -> &[PathBuf] {
        &self.approved_roots
    }

    /* ------------------------------------------------------------ settings */

    async fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, AppError> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT value_json FROM app_preferences WHERE key = ?1")
                .bind(key)
                .fetch_optional(self.pool())
                .await
                .map_err(|err| storage_error("cannot read a preference", &err))?;
        raw.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|err| internal(format!("stored preference {key} is invalid: {err}")))
    }

    async fn put_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<(), AppError> {
        let encoded = serde_json::to_string(value)
            .map_err(|err| internal(format!("cannot serialize preference {key}: {err}")))?;
        sqlx::query(
            "INSERT INTO app_preferences (key, version, value_json) VALUES (?1, 1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value_json = ?2",
        )
        .bind(key)
        .bind(encoded)
        .execute(self.pool())
        .await
        .map_err(|err| storage_error("cannot write a preference", &err))?;
        Ok(())
    }

    pub(crate) async fn notification_permission_state(&self) -> PermissionState {
        let stored: Option<String> = self
            .setting(NOTIFICATION_PERMISSION_KEY)
            .await
            .ok()
            .flatten();
        capabilities::permission_from_stored(stored.as_deref())
    }

    /// Push preferences into the dispatcher and record what the OS answered.
    ///
    /// The observed permission is stored so `get_capabilities` can report it
    /// without touching the platform API on every call.
    pub(crate) async fn apply_notification_settings(
        &self,
        preferences: &Preferences,
    ) -> Result<OsNotificationPermission, AppError> {
        let observed = self
            .notifications
            .apply_settings(&preferences.notifications)
            .await;
        self.put_setting(NOTIFICATION_PERMISSION_KEY, &permission_str(observed))
            .await?;
        Ok(observed)
    }

    /* ------------------------------------------------------------- library */

    /// Read one recording summary.
    pub(crate) async fn recording_row(
        &self,
        recording_id: RecordingId,
    ) -> Result<RecordingSummary, AppError> {
        let sql = format!("{RECORDING_SELECT} WHERE r.deleted_at IS NULL AND r.id = ?1");
        // Audited: a constant select plus a constant WHERE clause.
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(recording_id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(|err| storage_error("cannot read a recording", &err))?
            .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such recording"))?;
        row_to_summary(&row)
    }

    /// Store the original file name of a manual import as display metadata.
    pub(crate) async fn record_original_file_name(
        &self,
        recording_id: RecordingId,
        original_file_name: &str,
        at: Timestamp,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO recording_import_metadata (recording_id, original_file_name, imported_at) \
             VALUES (?1, ?2, ?3) ON CONFLICT(recording_id) DO NOTHING",
        )
        .bind(recording_id.to_string())
        .bind(original_file_name)
        .bind(at.as_millis())
        .execute(self.pool())
        .await
        .map_err(|err| storage_error("cannot record import metadata", &err))?;
        Ok(())
    }

    /* ---------------------------------------------------------- transcript */

    /// Header of one revision.
    pub(crate) async fn revision_ref(
        &self,
        revision_id: TranscriptRevisionId,
    ) -> Result<TranscriptRevisionRef, AppError> {
        let sql = format!("{REVISION_SELECT} WHERE tr.id = ?1");
        // Audited: a constant select plus a constant WHERE clause.
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(revision_id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(|err| storage_error("cannot read a revision", &err))?
            .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such revision"))?;
        row_to_revision_ref(&row)
    }

    /// Recording a revision belongs to.
    pub(crate) async fn revision_recording(
        &self,
        revision_id: TranscriptRevisionId,
    ) -> Result<RecordingId, AppError> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT recording_id FROM transcript_revisions WHERE id = ?1")
                .bind(revision_id.to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(|err| storage_error("cannot read a revision owner", &err))?;
        raw.ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such revision"))?
            .parse()
            .map_err(|_| internal("stored recording id is not a uuid"))
    }

    /// Segments of a revision, ordered by index.
    pub(crate) async fn revision_segments(
        &self,
        revision_id: TranscriptRevisionId,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<TranscriptSegmentDto>, AppError> {
        let engine: Option<String> =
            sqlx::query_scalar("SELECT engine FROM transcript_revisions WHERE id = ?1")
                .bind(revision_id.to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(|err| storage_error("cannot read a revision engine", &err))?;
        let engine = engine
            .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such revision"))?;

        let rows = sqlx::query(
            "SELECT ts.seq_index AS seq_index, ts.text AS text, ts.start_ms AS start_ms, \
             ts.end_ms AS end_ms, ts.alignment_origin AS alignment_origin, \
             ts.confidence AS confidence, sp.label AS speaker_label \
             FROM transcript_segments ts \
             LEFT JOIN speakers sp ON sp.id = ts.speaker_id \
             WHERE ts.transcript_revision_id = ?1 ORDER BY ts.seq_index ASC LIMIT ?2 OFFSET ?3",
        )
        .bind(revision_id.to_string())
        .bind(i64::from(limit))
        .bind(i64::from(offset))
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot read segments", &err))?;
        rows.iter()
            .map(|row| row_to_segment(row, &engine))
            .collect()
    }

    /// The revision that has no child, i.e. the current head.
    pub(crate) async fn head_revision(
        &self,
        recording_id: RecordingId,
    ) -> Result<Option<TranscriptRevisionId>, AppError> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT tr.id FROM transcript_revisions tr WHERE tr.recording_id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM transcript_revisions c WHERE c.parent_id = tr.id) \
             ORDER BY tr.created_at DESC, tr.id DESC LIMIT 1",
        )
        .bind(recording_id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(|err| storage_error("cannot read the head revision", &err))?;
        raw.map(|value| value.parse())
            .transpose()
            .map_err(|_| internal("stored revision id is not a uuid"))
    }

    async fn media_digest(&self, recording_id: RecordingId) -> Result<Sha256Digest, AppError> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT a.sha256 FROM media_assets a JOIN recordings r \
             ON r.preferred_media_asset_id = a.id WHERE r.id = ?1",
        )
        .bind(recording_id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(|err| storage_error("cannot read the media digest", &err))?;
        raw.ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no media for recording"))?
            .parse()
            .map_err(|_| internal("stored digest is not a sha-256"))
    }
}

/* ------------------------------------------------------------ row mapping */

/// Columns of a library row, assembled from the core tables.
///
/// `import_status` and `source_kind` are read with scalar subqueries rather
/// than a join so a recording with several source associations still produces
/// exactly one row.
const RECORDING_SELECT: &str = "SELECT r.id AS id, r.title AS title, \
     r.captured_at AS captured_at, r.created_at AS created_at, \
     r.duration_ms AS duration_ms, \
     a.sha256 AS sha256, a.byte_length AS byte_length, a.relative_path AS relative_path, \
     a.codec AS codec, a.sample_rate AS sample_rate, a.channels AS channels, \
     (SELECT si.import_status FROM source_items si WHERE si.recording_id = r.id \
        ORDER BY si.created_at ASC LIMIT 1) AS import_status, \
     (SELECT s.source_kind FROM source_items si JOIN sources s ON s.id = si.source_id \
        WHERE si.recording_id = r.id ORDER BY si.created_at ASC LIMIT 1) AS source_kind, \
     (SELECT group_concat(t.tag, char(31)) FROM recording_tags t \
        WHERE t.recording_id = r.id) AS tags, \
     (SELECT COUNT(*) FROM transcript_revisions tr WHERE tr.recording_id = r.id) \
        AS revision_count, \
     (SELECT j.last_error_code FROM jobs j \
        WHERE json_extract(j.config_snapshot_json, '$.recording_id') = r.id \
          AND j.last_error_code IS NOT NULL \
        ORDER BY j.created_at DESC LIMIT 1) AS last_error_code, \
     (SELECT m.original_file_name FROM recording_import_metadata m \
        WHERE m.recording_id = r.id) AS original_file_name \
     FROM recordings r JOIN media_assets a ON a.id = r.preferred_media_asset_id";

const REVISION_SELECT: &str = "SELECT tr.id AS id, tr.origin AS origin, \
     tr.created_at AS created_at, tr.engine AS engine, tr.model_id AS model_id, \
     tr.alignment_origin AS alignment_origin, \
     (SELECT COUNT(*) FROM transcript_segments ts \
        WHERE ts.transcript_revision_id = tr.id) AS segment_count, \
     EXISTS (SELECT 1 FROM transcript_revisions c WHERE c.parent_id = tr.id) AS superseded \
     FROM transcript_revisions tr";

fn row_to_summary(row: &sqlx::sqlite::SqliteRow) -> Result<RecordingSummary, AppError> {
    let id: String = row.get("id");
    let source_kind: Option<String> = row.get("source_kind");
    let import_status: Option<String> = row.get("import_status");
    let tags: Option<String> = row.get("tags");
    let last_error_code: Option<String> = row.get("last_error_code");
    let sha256: String = row.get("sha256");
    let revision_count: i64 = row.get("revision_count");

    Ok(RecordingSummary {
        recording_id: id
            .parse()
            .map_err(|_| internal("stored recording id is not a uuid"))?,
        title: row.get("title"),
        recorded_at: rfc3339_opt("recordings.captured_at", row.get("captured_at"))?,
        imported_at: rfc3339("recordings.created_at", row.get("created_at"))?,
        duration_ms: row
            .get::<Option<i64>, _>("duration_ms")
            .map(|value| as_u64("duration", value))
            .transpose()?,
        source: RecordingSourceRef {
            source_kind: dto_source_kind(source_kind.as_deref()),
            label: source_label(source_kind.as_deref()),
        },
        import_status: dto_import_status(import_status.as_deref()),
        // No engine runs inside this build, so the only honest states are
        // "a transcript exists" and "none".
        transcription_status: if revision_count > 0 {
            TranscriptionStatus::Completed
        } else {
            TranscriptionStatus::None
        },
        processing_engine: None,
        last_error: last_error_code.as_deref().and_then(envelope_for_code),
        tags: tags
            .map(|packed| packed.split(TAG_SEPARATOR).map(str::to_owned).collect())
            .unwrap_or_default(),
        byte_length: as_u64("byte length", row.get("byte_length"))?,
        sha256: sha256
            .parse()
            .map_err(|_| internal("stored digest is not a sha-256"))?,
    })
}

/// Rebuild an envelope from a persisted error code.
///
/// Only the code is stored, and the envelope is built from the code's own
/// canonical message, so nothing about the original failure's internals can
/// reach the UI through this path.
fn envelope_for_code(code: &str) -> Option<ErrorEnvelope> {
    let code = AppErrorCode::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.as_str() == code)?;
    Some(
        AppError::new(code, "restored from a persisted job failure")
            .to_envelope(&mut DiagnosticLog::new(), Timestamp::now()),
    )
}

fn dto_source_kind(stored: Option<&str>) -> SourceKind {
    match stored {
        Some("plaud_cloud_cli") => SourceKind::PlaudCloud,
        Some("plaud_device_ble" | "plaud_device_usb") => SourceKind::PlaudDevice,
        Some("watched_folder") => SourceKind::WatchedFolder,
        // Everything this build creates is a user-selected local file; an
        // unrecognized kind keeps the truthful label below.
        _ => SourceKind::ManualImport,
    }
}

fn source_label(stored: Option<&str>) -> String {
    match stored {
        Some("plaud_cloud_cli") => "PLAUD Cloud",
        Some("plaud_device_ble") => "Device BLE (compressed)",
        Some("plaud_device_usb") => "Device USB (original WAV where available)",
        Some("watched_folder") => "Watched folder",
        Some("local_file") => "Local file",
        _ => "Unknown source",
    }
    .to_owned()
}

fn dto_import_status(stored: Option<&str>) -> ImportStatus {
    match stored {
        Some("imported") => ImportStatus::Imported,
        Some("acquiring") => ImportStatus::Importing,
        Some("failed") => ImportStatus::Failed,
        _ => ImportStatus::Pending,
    }
}

fn row_to_revision_ref(row: &sqlx::sqlite::SqliteRow) -> Result<TranscriptRevisionRef, AppError> {
    let id: String = row.get("id");
    let origin: String = row.get("origin");
    let alignment: String = row.get("alignment_origin");
    Ok(TranscriptRevisionRef {
        revision_id: id
            .parse()
            .map_err(|_| internal("stored revision id is not a uuid"))?,
        // Only a user edit is a `human` revision; a derived alignment or an
        // overlap reconciliation is still machine output.
        kind: if origin == "human_edit" {
            RevisionKind::Human
        } else {
            RevisionKind::Machine
        },
        created_at: rfc3339("transcript_revisions.created_at", row.get("created_at"))?,
        engine_id: row.get("engine"),
        model_id: row.get("model_id"),
        alignment_origin: parse_alignment(&alignment)?,
        segment_count: as_u32("segment count", row.get("segment_count"))?,
        superseded: row.get::<i64, _>("superseded") != 0,
    })
}

fn row_to_segment(
    row: &sqlx::sqlite::SqliteRow,
    engine_id: &str,
) -> Result<TranscriptSegmentDto, AppError> {
    let alignment: String = row.get("alignment_origin");
    Ok(TranscriptSegmentDto {
        index: as_u32("segment index", row.get("seq_index"))?,
        text: row.get("text"),
        start_ms: row
            .get::<Option<i64>, _>("start_ms")
            .map(|value| as_u64("segment start", value))
            .transpose()?,
        end_ms: row
            .get::<Option<i64>, _>("end_ms")
            .map(|value| as_u64("segment end", value))
            .transpose()?,
        alignment_origin: parse_alignment(&alignment)?,
        confidence: row
            .get::<Option<f64>, _>("confidence")
            .map(|value| SegmentConfidence {
                engine_id: engine_id.to_owned(),
                value,
            }),
        speaker_label: row.get("speaker_label"),
        // Annotations are produced by the truthfulness pipeline (spec §8.6A),
        // which the STT EPIC owns; this port stores none and invents none.
        annotations: Vec::new(),
    })
}

fn parse_alignment(value: &str) -> Result<AlignmentOrigin, AppError> {
    match value {
        "native" => Ok(AlignmentOrigin::Native),
        "derived" => Ok(AlignmentOrigin::Derived),
        "none" => Ok(AlignmentOrigin::None),
        _ => Err(internal("stored alignment origin is unknown")),
    }
}

const fn alignment_str(origin: AlignmentOrigin) -> &'static str {
    match origin {
        AlignmentOrigin::Native => "native",
        AlignmentOrigin::Derived => "derived",
        AlignmentOrigin::None => "none",
    }
}

/// A bound value of a dynamically assembled statement.
enum Bind {
    Text(String),
    Int(i64),
}

/* --------------------------------------------------------------- library */

#[async_trait]
impl LibraryPort for LocalBackend {
    async fn list_recordings(
        &self,
        query: ValidatedListRecordings,
    ) -> Result<RecordingPage, AppError> {
        let (where_clause, binds) = build_filter(&query.filter);
        let order = match (query.sort.field, query.sort.direction) {
            (RecordingSortField::RecordedAt, direction) => {
                format!("COALESCE(r.captured_at, r.created_at) {}", dir(direction))
            }
            (RecordingSortField::ImportedAt, direction) => {
                format!("r.created_at {}", dir(direction))
            }
            (RecordingSortField::Title, direction) => {
                format!("r.title COLLATE NOCASE {}", dir(direction))
            }
            (RecordingSortField::DurationMs, direction) => {
                format!("r.duration_ms IS NULL, r.duration_ms {}", dir(direction))
            }
        };

        let sql = format!(
            "{RECORDING_SELECT} {where_clause} ORDER BY {order}, r.id ASC LIMIT ? OFFSET ?"
        );
        // Audited: user values are bound, never interpolated; the only
        // interpolated fragments are constants and `?` placeholders.
        let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql));
        for bind in &binds {
            statement = match bind {
                Bind::Text(value) => statement.bind(value),
                Bind::Int(value) => statement.bind(value),
            };
        }
        let offset = i64::try_from(query.cursor.offset())
            .map_err(|_| invalid_argument("cursor offset is out of range"))?;
        let rows = statement
            .bind(i64::from(query.limit) + 1)
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(|err| storage_error("cannot list recordings", &err))?;

        let has_more = rows.len() > query.limit as usize;
        let items = rows
            .iter()
            .take(query.limit as usize)
            .map(row_to_summary)
            .collect::<Result<Vec<_>, _>>()?;

        let count_sql = format!(
            "SELECT COUNT(*) FROM recordings r \
             JOIN media_assets a ON a.id = r.preferred_media_asset_id {where_clause}"
        );
        let mut count_statement = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_sql));
        for bind in &binds {
            count_statement = match bind {
                Bind::Text(value) => count_statement.bind(value),
                Bind::Int(value) => count_statement.bind(value),
            };
        }
        let total = count_statement
            .fetch_one(self.pool())
            .await
            .map_err(|err| storage_error("cannot count recordings", &err))?;

        Ok(RecordingPage {
            items,
            next_cursor: has_more
                .then(|| PageCursor::at(query.cursor.offset() + u64::from(query.limit)).encode()),
            total: as_u64("recording count", total)?,
        })
    }

    async fn get_recording(&self, recording_id: RecordingId) -> Result<RecordingDetail, AppError> {
        let sql = format!("{RECORDING_SELECT} WHERE r.deleted_at IS NULL AND r.id = ?1");
        // Audited: a constant select plus a constant WHERE clause.
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(recording_id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(|err| storage_error("cannot read a recording", &err))?
            .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such recording"))?;

        let summary = row_to_summary(&row)?;
        let original_file_name: Option<String> = row.get("original_file_name");
        let revision_sql = format!(
            "{REVISION_SELECT} WHERE tr.recording_id = ?1 ORDER BY tr.created_at ASC, tr.id ASC"
        );
        let revision_rows = sqlx::query(sqlx::AssertSqlSafe(revision_sql))
            .bind(recording_id.to_string())
            .fetch_all(self.pool())
            .await
            .map_err(|err| storage_error("cannot read revisions", &err))?;

        Ok(RecordingDetail {
            media: RecordingMediaInfo {
                // No decoder has run in this build, so the asset's codec column
                // is empty rather than guessed from the file name.
                observed_codec: row
                    .get::<Option<String>, _>("codec")
                    .filter(|codec| !codec.is_empty())
                    .unwrap_or_else(|| "unknown".to_owned()),
                sample_rate_hz: row
                    .get::<Option<i64>, _>("sample_rate")
                    .map(|value| as_u32("sample rate", value))
                    .transpose()?,
                channels: row
                    .get::<Option<i64>, _>("channels")
                    .map(|value| as_u32("channels", value))
                    .transpose()?,
                // Falls back to the name the source supplied, which is what the
                // title was created from.
                original_file_name: original_file_name.unwrap_or_else(|| summary.title.clone()),
            },
            revisions: revision_rows
                .iter()
                .map(row_to_revision_ref)
                .collect::<Result<Vec<_>, _>>()?,
            current_revision_id: self.head_revision(recording_id).await?,
            // This port stores no derived AI outputs; the summarization EPIC
            // owns `ai_outputs`, so the list is empty rather than invented.
            outputs: Vec::new(),
            summary,
        })
    }

    async fn resolve_media(&self, recording_id: RecordingId) -> Result<ResolvedMedia, AppError> {
        let row = sqlx::query(
            "SELECT a.relative_path AS relative_path, a.byte_length AS byte_length \
             FROM media_assets a JOIN recordings r ON r.preferred_media_asset_id = a.id \
             WHERE r.id = ?1 AND r.deleted_at IS NULL",
        )
        .bind(recording_id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(|err| storage_error("cannot resolve media", &err))?
        .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such recording"))?;

        let relative: String = row.get("relative_path");
        let path = self.archive().absolute(&relative).map_err(AppError::from)?;
        if !path.is_file() {
            return Err(AppError::new(
                AppErrorCode::AssetNotFound,
                "the archived blob is missing from disk",
            ));
        }
        Ok(ResolvedMedia {
            content_type: crate::media::content_type_for(&relative).to_owned(),
            byte_length: as_u64("byte length", row.get("byte_length"))?,
            path,
        })
    }
}

const fn dir(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Asc => "ASC",
        SortDirection::Desc => "DESC",
    }
}

fn build_filter(filter: &ValidatedRecordingFilter) -> (String, Vec<Bind>) {
    let mut clauses = vec!["r.deleted_at IS NULL".to_owned()];
    let mut binds = Vec::new();

    if let Some(query) = &filter.query {
        clauses.push("r.title LIKE ? ESCAPE '\\'".to_owned());
        binds.push(Bind::Text(format!("%{}%", escape_like(query))));
    }
    if !filter.import_status.is_empty() {
        clauses.push(format!(
            "(SELECT si.import_status FROM source_items si WHERE si.recording_id = r.id \
             ORDER BY si.created_at ASC LIMIT 1) IN ({})",
            placeholders(filter.import_status.len())
        ));
        binds.extend(
            filter
                .import_status
                .iter()
                .map(|status| Bind::Text(stored_import_status(*status))),
        );
    }
    if !filter.transcription_status.is_empty() {
        // Derived from whether a revision exists, which is the only
        // transcription state this port can observe.
        let wants_completed = filter
            .transcription_status
            .contains(&TranscriptionStatus::Completed);
        let wants_none = filter
            .transcription_status
            .contains(&TranscriptionStatus::None);
        let predicate = match (wants_completed, wants_none) {
            (true, true) => "1 = 1",
            (true, false) => {
                "EXISTS (SELECT 1 FROM transcript_revisions tr WHERE tr.recording_id = r.id)"
            }
            (false, true) => {
                "NOT EXISTS (SELECT 1 FROM transcript_revisions tr WHERE tr.recording_id = r.id)"
            }
            (false, false) => "1 = 0",
        };
        clauses.push(predicate.to_owned());
    }
    if !filter.sources.is_empty() {
        clauses.push(format!(
            "(SELECT s.source_kind FROM source_items si JOIN sources s ON s.id = si.source_id \
             WHERE si.recording_id = r.id ORDER BY si.created_at ASC LIMIT 1) IN ({})",
            placeholders(filter.sources.len())
        ));
        binds.extend(
            filter
                .sources
                .iter()
                .map(|kind| Bind::Text(stored_source_kind(*kind))),
        );
    }
    for tag in &filter.tags {
        clauses.push(
            "EXISTS (SELECT 1 FROM recording_tags t WHERE t.recording_id = r.id AND t.tag = ?)"
                .to_owned(),
        );
        binds.push(Bind::Text(tag.clone()));
    }
    if let Some(from) = filter.date_from {
        clauses.push("COALESCE(r.captured_at, r.created_at) >= ?".to_owned());
        binds.push(Bind::Int(from.as_millis()));
    }
    if let Some(to) = filter.date_to {
        clauses.push("COALESCE(r.captured_at, r.created_at) <= ?".to_owned());
        binds.push(Bind::Int(to.as_millis()));
    }

    (format!("WHERE {}", clauses.join(" AND ")), binds)
}

fn stored_import_status(status: ImportStatus) -> String {
    match status {
        ImportStatus::Pending => "discovered",
        ImportStatus::Importing => "acquiring",
        ImportStatus::Imported => "imported",
        ImportStatus::Failed => "failed",
    }
    .to_owned()
}

fn stored_source_kind(kind: SourceKind) -> String {
    match kind {
        SourceKind::ManualImport => "local_file",
        SourceKind::PlaudCloud => "plaud_cloud_cli",
        SourceKind::PlaudDevice => "plaud_device_ble",
        SourceKind::WatchedFolder => "watched_folder",
    }
    .to_owned()
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/* ------------------------------------------------------------- transcript */

#[async_trait]
impl TranscriptPort for LocalBackend {
    async fn create_machine_revision(
        &self,
        spec: MachineRevisionSpec,
    ) -> Result<TranscriptRevisionRef, AppError> {
        let digest = self.media_digest(spec.recording_id).await?;
        let engine = EngineId::new(spec.engine_id.clone());
        let created_at = Timestamp::now();
        let segments: Vec<DomainSegment> = spec
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                let index = u32::try_from(index).unwrap_or(u32::MAX);
                let mut domain = match (segment.start_ms, segment.end_ms) {
                    (Some(start), Some(end)) => {
                        DomainSegment::timed(index, start, end, segment.text.clone())
                    }
                    _ => DomainSegment::text_only(index, segment.text.clone()),
                };
                domain.confidence = segment.confidence.map(|value| {
                    plaudon_domain::transcript::Confidence::new(
                        engine.clone(),
                        plaudon_domain::transcript::ConfidenceScale::VendorDefined,
                        value,
                    )
                });
                domain
            })
            .collect();

        // The domain validator owns the invariants: contiguous indices, no
        // inverted ranges, and no times at all on a revision whose alignment
        // origin is `none` (spec §8.6A).
        let revision = TranscriptRevision::try_new(TranscriptRevisionDraft {
            recording_id: spec.recording_id,
            parent_id: self.head_revision(spec.recording_id).await?,
            origin: RevisionOrigin::Machine,
            input_sha256: digest,
            engine: engine.clone(),
            model: ModelIdentity::new(engine, spec.model_id.clone()),
            locale: None,
            alignment_origin: spec.alignment_origin,
            status: RevisionStatus::Complete,
            normalization_profile: "unspecified".to_owned(),
            chunking_strategy: "unspecified".to_owned(),
            duration_ms: None,
            created_at,
            segments,
        })
        .map_err(|err| invalid_argument(format!("invalid transcript revision: {err}")))?;

        let speakers: Vec<Option<String>> = spec
            .segments
            .iter()
            .map(|segment| segment.speaker_label.clone())
            .collect();
        self.insert_revision(&revision, RevisionOrigin::Machine, &speakers)
            .await?;

        self.events()
            .publish(
                EventDraft::new(EventKind::TranscriptCompleted, created_at)
                    .with_recording(spec.recording_id)
                    .with_hint("revision_id", json!(revision.id.to_string())),
            )
            .await;

        self.revision_ref(revision.id).await
    }

    async fn get_transcript(
        &self,
        query: ValidatedGetTranscript,
    ) -> Result<TranscriptPage, AppError> {
        let revision = self.revision_ref(query.revision_id).await?;
        let segments = self
            .revision_segments(query.revision_id, query.cursor, query.limit)
            .await?;
        let consumed = query
            .cursor
            .saturating_add(u32::try_from(segments.len()).unwrap_or(0));
        let next_cursor = (consumed < revision.segment_count).then_some(consumed);
        Ok(TranscriptPage {
            total_segments: revision.segment_count,
            revision,
            segments,
            next_cursor,
        })
    }

    async fn save_transcript_revision(
        &self,
        request: ValidatedSaveRevision,
    ) -> Result<SaveTranscriptRevisionResponse, AppError> {
        let base = self.revision_ref(request.base_revision_id).await?;
        let owner = self.revision_recording(request.base_revision_id).await?;
        if owner != request.recording_id {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "the base revision belongs to another recording",
            ));
        }

        // Spec §15.2: saving against a base that already has a child is a
        // conflict, never a silent overwrite.
        if base.superseded {
            return Err(AppError::new(
                AppErrorCode::Conflict,
                "the base revision is no longer the head",
            ));
        }

        let mut segments = self
            .revision_segments(request.base_revision_id, 0, u32::MAX)
            .await?;
        for (index, text) in &request.edits {
            let slot = segments
                .iter_mut()
                .find(|segment| segment.index == *index)
                .ok_or_else(|| invalid_argument(format!("segment {index} does not exist")))?;
            slot.text.clone_from(text);
        }

        let digest = self.media_digest(request.recording_id).await?;
        let engine = EngineId::new(base.engine_id.clone().unwrap_or_else(|| "human".to_owned()));
        let created_at = Timestamp::now();
        let domain_segments: Vec<DomainSegment> = segments
            .iter()
            .map(|segment| match (segment.start_ms, segment.end_ms) {
                (Some(start), Some(end)) => {
                    DomainSegment::timed(segment.index, start, end, segment.text.clone())
                }
                _ => DomainSegment::text_only(segment.index, segment.text.clone()),
            })
            .collect();

        let revision = TranscriptRevision::try_new(TranscriptRevisionDraft {
            recording_id: request.recording_id,
            parent_id: Some(request.base_revision_id),
            origin: RevisionOrigin::HumanEdit,
            input_sha256: digest,
            engine: engine.clone(),
            model: ModelIdentity::new(
                engine,
                base.model_id
                    .clone()
                    .unwrap_or_else(|| "human-edit".to_owned()),
            ),
            locale: None,
            alignment_origin: base.alignment_origin,
            status: RevisionStatus::Complete,
            normalization_profile: "unspecified".to_owned(),
            chunking_strategy: "unspecified".to_owned(),
            duration_ms: None,
            created_at,
            segments: domain_segments,
        })
        .map_err(|err| invalid_argument(format!("invalid transcript revision: {err}")))?;

        let speakers: Vec<Option<String>> = segments
            .iter()
            .map(|segment| segment.speaker_label.clone())
            .collect();
        self.insert_revision(&revision, RevisionOrigin::HumanEdit, &speakers)
            .await?;

        self.events()
            .publish(
                EventDraft::new(EventKind::LibraryChanged, created_at)
                    .with_recording(request.recording_id)
                    .with_hint("revision_id", json!(revision.id.to_string())),
            )
            .await;

        Ok(SaveTranscriptRevisionResponse {
            revision_id: revision.id,
            // `ai_outputs` is owned by the summarization EPIC and is empty
            // here, so nothing can have become stale.
            stale_output_ids: Vec::new(),
        })
    }
}

impl LocalBackend {
    /// Insert a validated revision and its segments.
    ///
    /// Existing revisions and their segments are never touched: a new revision
    /// is a new row set whose parent points at the untouched base.
    async fn insert_revision(
        &self,
        revision: &TranscriptRevision,
        origin: RevisionOrigin,
        speakers: &[Option<String>],
    ) -> Result<(), AppError> {
        let speaker_ids = self
            .ensure_speakers(revision.recording_id, speakers)
            .await?;

        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|err| storage_error("cannot begin a revision write", &err))?;

        sqlx::query(
            "INSERT INTO transcript_revisions (id, recording_id, parent_id, origin, \
             input_sha256, engine, model_id, model_version, locale, alignment_origin, status, \
             normalization_profile, chunking_strategy, duration_ms, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, 'complete', ?9, ?10, NULL, ?11)",
        )
        .bind(revision.id.to_string())
        .bind(revision.recording_id.to_string())
        .bind(revision.parent_id.map(|id| id.to_string()))
        .bind(origin_str(origin))
        .bind(revision.input_sha256.to_hex())
        .bind(revision.engine.as_str())
        .bind(&revision.model.model_id)
        .bind(alignment_str(revision.alignment_origin))
        .bind(&revision.normalization_profile)
        .bind(&revision.chunking_strategy)
        .bind(revision.created_at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(|err| storage_error("cannot insert a revision", &err))?;

        for (position, segment) in revision.segments().iter().enumerate() {
            sqlx::query(
                "INSERT INTO transcript_segments (id, transcript_revision_id, seq_index, \
                 start_ms, end_ms, speaker_id, text, confidence, confidence_scale, \
                 alignment_origin) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )
            .bind(uuid::Uuid::now_v7().to_string())
            .bind(revision.id.to_string())
            .bind(i64::from(segment.seq_index))
            .bind(
                segment
                    .start
                    .map(|value| i64::try_from(value.get()).unwrap_or(i64::MAX)),
            )
            .bind(
                segment
                    .end
                    .map(|value| i64::try_from(value.get()).unwrap_or(i64::MAX)),
            )
            .bind(speaker_ids.get(position).cloned().flatten())
            .bind(&segment.text)
            .bind(segment.confidence.as_ref().map(|value| value.value))
            .bind(segment.confidence.as_ref().map(|_| "vendor_defined"))
            .bind(alignment_str(revision.alignment_origin))
            .execute(&mut *tx)
            .await
            .map_err(|err| storage_error("cannot insert a segment", &err))?;
        }

        tx.commit()
            .await
            .map_err(|err| storage_error("cannot commit a revision", &err))
    }

    /// Resolve speaker labels to rows, creating inferred speakers on demand.
    async fn ensure_speakers(
        &self,
        recording_id: RecordingId,
        labels: &[Option<String>],
    ) -> Result<Vec<Option<String>>, AppError> {
        let mut known: BTreeMap<String, String> = BTreeMap::new();
        let rows = sqlx::query("SELECT id, label FROM speakers WHERE recording_id = ?1")
            .bind(recording_id.to_string())
            .fetch_all(self.pool())
            .await
            .map_err(|err| storage_error("cannot read speakers", &err))?;
        for row in rows {
            known.insert(row.get("label"), row.get("id"));
        }

        let mut out = Vec::with_capacity(labels.len());
        for label in labels {
            let Some(label) = label else {
                out.push(None);
                continue;
            };
            if let Some(id) = known.get(label) {
                out.push(Some(id.clone()));
                continue;
            }
            let id = plaudon_domain::ids::SpeakerId::new().to_string();
            sqlx::query(
                "INSERT INTO speakers (id, recording_id, label, name, provenance, \
                 user_confirmed) VALUES (?1, ?2, ?3, NULL, 'inferred', 0)",
            )
            .bind(&id)
            .bind(recording_id.to_string())
            .bind(label)
            .execute(self.pool())
            .await
            .map_err(|err| storage_error("cannot create a speaker", &err))?;
            known.insert(label.clone(), id.clone());
            out.push(Some(id));
        }
        Ok(out)
    }
}

const fn origin_str(origin: RevisionOrigin) -> &'static str {
    match origin {
        RevisionOrigin::Machine => "machine",
        RevisionOrigin::HumanEdit => "human_edit",
        RevisionOrigin::DerivedAlignment => "derived_alignment",
        RevisionOrigin::OverlapReconciliation => "overlap_reconciliation",
    }
}

/* --------------------------------------------------------------- settings */

/// Preferences of a fresh install: nothing enabled, nothing chosen.
#[must_use]
pub fn default_preferences() -> Preferences {
    let mut categories = BTreeMap::new();
    for category in NotificationCategory::ALL {
        categories.insert(*category, false);
    }
    Preferences {
        onboarding_completed: false,
        local_only_mode: false,
        default_stt_engine_id: None,
        default_text_engine_id: None,
        appearance: Appearance::System,
        contrast: Contrast::Standard,
        notifications: NotificationSettings {
            enabled: false,
            categories,
        },
    }
}

#[async_trait]
impl SettingsPort for LocalBackend {
    async fn preferences(&self) -> Result<Preferences, AppError> {
        Ok(self
            .setting::<Preferences>(PREFERENCES_KEY)
            .await?
            .unwrap_or_else(default_preferences))
    }

    async fn set_notification_settings(
        &self,
        settings: NotificationSettings,
    ) -> Result<NotificationSettingsResponse, AppError> {
        let mut preferences = self.preferences().await?;
        preferences.notifications = settings;
        self.put_setting(PREFERENCES_KEY, &preferences).await?;
        let observed = self.apply_notification_settings(&preferences).await?;
        Ok(NotificationSettingsResponse {
            settings: preferences.notifications,
            os_permission: observed,
        })
    }

    async fn set_local_only_mode(&self, enabled: bool) -> Result<LocalOnlyModeResponse, AppError> {
        let mut preferences = self.preferences().await?;
        preferences.local_only_mode = enabled;
        self.put_setting(PREFERENCES_KEY, &preferences).await?;
        Ok(local_only_response(enabled))
    }

    async fn complete_onboarding(
        &self,
        request: crate::dto::CompleteOnboardingRequest,
    ) -> Result<Preferences, AppError> {
        let mut preferences = self.preferences().await?;
        preferences.onboarding_completed = true;
        preferences.default_stt_engine_id = request.default_stt_engine_id;
        preferences.default_text_engine_id = request.default_text_engine_id;
        preferences.local_only_mode = request.local_only_mode;
        preferences.notifications = request.notifications;
        self.put_setting(PREFERENCES_KEY, &preferences).await?;
        self.apply_notification_settings(&preferences).await?;
        Ok(preferences)
    }
}

/// Stable string form of an observed OS permission.
const fn permission_str(permission: OsNotificationPermission) -> &'static str {
    match permission {
        OsNotificationPermission::Granted => "granted",
        OsNotificationPermission::Denied => "denied",
        OsNotificationPermission::NotRequested => "not_requested",
        OsNotificationPermission::Unsupported => "unsupported",
    }
}

/// Describe the effective egress policy.
///
/// This crate performs no network I/O and maintains no host allowlist, so the
/// list is empty in both modes and the description says what is actually
/// enforced rather than implying an allowlist exists.
#[must_use]
pub fn local_only_response(enabled: bool) -> LocalOnlyModeResponse {
    LocalOnlyModeResponse {
        enabled,
        description: if enabled {
            "Local-only Mode is enforced: no cloud engine may be selected and no outbound \
             request is permitted."
                .to_owned()
        } else {
            "Cloud processing is possible, but only for an engine you select and confirm; \
             nothing is sent without that confirmation."
                .to_owned()
        },
        allowed_hosts: Vec::new(),
    }
}


#[async_trait]
impl SearchPort for LocalBackend {
    async fn search(
        &self,
        query: &str,
        page: plaudon_persistence::search::SearchPage,
    ) -> Result<Vec<plaudon_app_core::search::SearchResult>, AppError> {
        let search = plaudon_app_core::search::LibrarySearch::new(self.store.db().clone());
        search.search(query, page).await.map_err(AppError::from)
    }
}

#[async_trait]
impl SpeakersPort for LocalBackend {
    async fn list_speakers(
        &self,
        recording_id: RecordingId,
    ) -> Result<plaudon_app_core::speakers::SpeakerRegistry, AppError> {
        let rows: Vec<_> = sqlx::query(
            "SELECT id, recording_id, cluster_key, original_label, label, name, \
             provenance, user_confirmed, renamed_at, confirmed_at \
             FROM speakers WHERE recording_id = ?1 ORDER BY original_label",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot read speakers", &err))?;

        let mut speakers = Vec::new();
        for row in rows {
            let id: String = row.get("id");
            let cluster_key: Option<String> = row.get("cluster_key");
            let provenance: String = row.get("provenance");
            let renamed_at: Option<String> = row.get("renamed_at");
            let confirmed_at: Option<String> = row.get("confirmed_at");

            speakers.push(plaudon_app_core::speakers::Speaker {
                id: id
                    .parse()
                    .map_err(|_| internal("stored speaker id is not a uuid"))?,
                recording_id,
                cluster_key: cluster_key.map(|k| k.into()),
                original_label: row.get("original_label"),
                label: row.get("label"),
                name: row.get("name"),
                provenance: match provenance.as_str() {
                    "inferred" => plaudon_app_core::speakers::SpeakerProvenance::Inferred,
                    "user_confirmed" => {
                        plaudon_app_core::speakers::SpeakerProvenance::UserConfirmed
                    }
                    _ => return Err(internal("unknown speaker provenance")),
                },
                user_confirmed: row.get("user_confirmed"),
                renamed_at: renamed_at
                    .map(|s| rfc3339_opt("speakers.renamed_at", Some(s)))
                    .transpose()?
                    .flatten(),
                confirmed_at: confirmed_at
                    .map(|s| rfc3339_opt("speakers.confirmed_at", Some(s)))
                    .transpose()?
                    .flatten(),
            });
        }

        Ok(plaudon_app_core::speakers::SpeakerRegistry {
            recording_id,
            speakers,
        })
    }

    async fn rename_speaker(
        &self,
        recording_id: RecordingId,
        speaker_id: SpeakerId,
        name: &str,
        at: Timestamp,
    ) -> Result<plaudon_app_core::speakers::Speaker, AppError> {
        let mut registry = self.list_speakers(recording_id).await?;
        let speaker = registry
            .rename(speaker_id, name, at)
            .map_err(|err| AppError::new(AppErrorCode::InvalidArgument, err.to_string()))?;

        sqlx::query(
            "UPDATE speakers SET label = ?1, name = ?2, provenance = ?3, \
             user_confirmed = ?4, renamed_at = ?5, confirmed_at = NULL \
             WHERE id = ?6 AND recording_id = ?7",
        )
        .bind(&speaker.label)
        .bind(&speaker.name)
        .bind("inferred")
        .bind(false)
        .bind(speaker.renamed_at.map(|t| t.to_rfc3339()))
        .bind(speaker.id.to_string())
        .bind(recording_id.to_string())
        .execute(self.pool())
        .await
        .map_err(|err| storage_error("cannot update speaker", &err))?;

        Ok(speaker.clone())
    }

    async fn confirm_speaker(
        &self,
        recording_id: RecordingId,
        speaker_id: SpeakerId,
        at: Timestamp,
    ) -> Result<plaudon_app_core::speakers::Speaker, AppError> {
        let mut registry = self.list_speakers(recording_id).await?;
        let speaker = registry
            .confirm(speaker_id, at)
            .map_err(|err| AppError::new(AppErrorCode::InvalidArgument, err.to_string()))?;

        sqlx::query(
            "UPDATE speakers SET provenance = ?1, user_confirmed = ?2, confirmed_at = ?3 \
             WHERE id = ?4 AND recording_id = ?5",
        )
        .bind("user_confirmed")
        .bind(true)
        .bind(speaker.confirmed_at.map(|t| t.to_rfc3339()))
        .bind(speaker.id.to_string())
        .bind(recording_id.to_string())
        .execute(self.pool())
        .await
        .map_err(|err| storage_error("cannot confirm speaker", &err))?;

        Ok(speaker.clone())
    }

    async fn reset_speaker(
        &self,
        recording_id: RecordingId,
        speaker_id: SpeakerId,
    ) -> Result<plaudon_app_core::speakers::Speaker, AppError> {
        let mut registry = self.list_speakers(recording_id).await?;
        let speaker = registry
            .reset(speaker_id)
            .map_err(|err| AppError::new(AppErrorCode::InvalidArgument, err.to_string()))?;

        sqlx::query(
            "UPDATE speakers SET label = ?1, name = NULL, provenance = ?2, \
             user_confirmed = ?3, renamed_at = NULL, confirmed_at = NULL \
             WHERE id = ?4 AND recording_id = ?5",
        )
        .bind(&speaker.label)
        .bind("inferred")
        .bind(false)
        .bind(speaker.id.to_string())
        .bind(recording_id.to_string())
        .execute(self.pool())
        .await
        .map_err(|err| storage_error("cannot reset speaker", &err))?;

        Ok(speaker.clone())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Arc;

    use tempfile::TempDir;

    use plaudon_notifications::testing::RecordingNotifier;
    use plaudon_notifications::{Dispatcher, NotificationSettings as CrateSettings, OsPermission};

    use super::LocalBackend;
    use crate::events::RecordingEventSink;
    use crate::secrets::InMemorySecretStore;

    /// A backend over a temporary library, with a recording event sink.
    ///
    /// `secrets` and the two directories are kept alive by the fixture; a
    /// dropped `TempDir` would delete the library under the test.
    #[allow(dead_code)]
    pub(crate) struct Fixture {
        pub backend: LocalBackend,
        pub sink: Arc<RecordingEventSink>,
        pub secrets: Arc<InMemorySecretStore>,
        pub notifier: Arc<RecordingNotifier>,
        pub dir: TempDir,
        pub imports: TempDir,
    }

    impl Fixture {
        /// Write a media file into the approved import directory.
        pub fn source_file(&self, name: &str, bytes: &[u8]) -> String {
            let path = self.imports.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    pub(crate) async fn backend_fixture() -> Fixture {
        fixture_with_notifier(RecordingNotifier::with_permission(
            OsPermission::NotRequested,
        ))
        .await
    }

    /// A fixture whose notifications go to `notifier`.
    ///
    /// The dispatcher uses a short timeout so a hanging backend is contained
    /// inside the test rather than inside the five-second production budget.
    pub(crate) async fn fixture_with_notifier(notifier: Arc<RecordingNotifier>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let imports = tempfile::tempdir().unwrap();
        let sink = Arc::new(RecordingEventSink::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let dispatcher = Arc::new(
            Dispatcher::new(
                Arc::clone(&notifier) as Arc<dyn plaudon_notifications::Notifier>,
                CrateSettings::uniform(false),
            )
            .with_timeout(std::time::Duration::from_millis(100)),
        );
        let backend = LocalBackend::open(
            dir.path(),
            sink.clone(),
            dispatcher,
            secrets.clone(),
            vec![imports.path().to_path_buf()],
        )
        .await
        .unwrap();
        Fixture {
            backend,
            sink,
            secrets,
            notifier,
            dir,
            imports,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::backend_fixture;
    use super::*;
    use crate::dto::{DuplicatePolicy, ImportOutcome, JobState, RecordingSort};
    use crate::ports::{ActivityPort, ImportPort, JobsPort, MachineSegment};
    use crate::validate::ValidatedListJobs;

    fn list_all() -> ValidatedListRecordings {
        ValidatedListRecordings {
            cursor: PageCursor::START,
            limit: 50,
            filter: ValidatedRecordingFilter::default(),
            sort: RecordingSort {
                field: RecordingSortField::ImportedAt,
                direction: SortDirection::Desc,
            },
        }
    }

    fn count_blobs(backend: &LocalBackend) -> usize {
        backend.archive().list_blobs().unwrap().len()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn importing_the_same_bytes_twice_keeps_one_blob_and_reports_a_duplicate() {
        let fixture = backend_fixture().await;
        let first = fixture.source_file("meeting.wav", b"RIFF....WAVEfmt payload");
        let second = fixture.source_file("copy-of-meeting.wav", b"RIFF....WAVEfmt payload");

        let response = fixture
            .backend
            .import_files(vec![first], DuplicatePolicy::Ask)
            .await
            .unwrap();
        assert!(matches!(
            response.results[0],
            ImportOutcome::Accepted { .. }
        ));

        let response = fixture
            .backend
            .import_files(vec![second.clone()], DuplicatePolicy::Ask)
            .await
            .unwrap();
        let ImportOutcome::Duplicate {
            sha256,
            existing_title,
            ..
        } = &response.results[0]
        else {
            panic!(
                "the second import must be reported as a duplicate: {:?}",
                response.results[0]
            );
        };
        assert_eq!(*sha256, Sha256Digest::of(b"RIFF....WAVEfmt payload"));
        assert_eq!(existing_title, "meeting");

        let page = fixture.backend.list_recordings(list_all()).await.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(count_blobs(&fixture.backend), 1);

        // import_anyway creates a second recording over the same blob.
        let response = fixture
            .backend
            .import_files(vec![second], DuplicatePolicy::ImportAnyway)
            .await
            .unwrap();
        assert!(matches!(
            response.results[0],
            ImportOutcome::Accepted { .. }
        ));
        let page = fixture.backend.list_recordings(list_all()).await.unwrap();
        assert_eq!(page.total, 2, "two logical recordings");
        assert_eq!(count_blobs(&fixture.backend), 1, "still one blob");

        // skip returns the existing recording without creating anything.
        let third = fixture.source_file("third.wav", b"RIFF....WAVEfmt payload");
        let response = fixture
            .backend
            .import_files(vec![third], DuplicatePolicy::Skip)
            .await
            .unwrap();
        assert!(matches!(response.results[0], ImportOutcome::Skipped { .. }));
        assert_eq!(
            fixture
                .backend
                .list_recordings(list_all())
                .await
                .unwrap()
                .total,
            2
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rejected_path_fails_its_job_and_persists_the_code() {
        let fixture = backend_fixture().await;
        let outside = tempfile::tempdir().unwrap();
        let escaped = outside.path().join("elsewhere.wav");
        std::fs::write(&escaped, b"bytes").unwrap();

        let response = fixture
            .backend
            .import_files(
                vec![escaped.to_string_lossy().into_owned()],
                DuplicatePolicy::Ask,
            )
            .await
            .unwrap();
        let ImportOutcome::Rejected { error, .. } = &response.results[0] else {
            panic!("a path outside the approved roots must be rejected");
        };
        assert_eq!(error.code, AppErrorCode::ArchivePathRejected);

        let jobs = fixture
            .backend
            .list_jobs(ValidatedListJobs {
                cursor: PageCursor::START,
                limit: 10,
                states: Vec::new(),
                kinds: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(jobs.items.len(), 1);
        assert_eq!(jobs.items[0].state, JobState::FailedTerminal);
        assert_eq!(
            jobs.items[0].error.as_ref().map(|envelope| envelope.code),
            Some(AppErrorCode::ArchivePathRejected)
        );
    }

    async fn imported_recording(fixture: &super::testing::Fixture, name: &str) -> RecordingId {
        let path = fixture.source_file(name, format!("bytes of {name}").as_bytes());
        let response = fixture
            .backend
            .import_files(vec![path], DuplicatePolicy::Ask)
            .await
            .unwrap();
        match &response.results[0] {
            ImportOutcome::Accepted { job_id, .. } => {
                let summary = jobs::load(fixture.backend.pool(), *job_id).await.unwrap();
                summary
                    .recording_id
                    .expect("a committed import links its recording")
            }
            other => panic!("expected an accepted import, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn revisions_are_immutable_and_a_stale_base_conflicts() {
        let fixture = backend_fixture().await;
        let recording_id = imported_recording(&fixture, "talk.wav").await;

        let machine = fixture
            .backend
            .create_machine_revision(MachineRevisionSpec {
                recording_id,
                segments: vec![
                    MachineSegment::timed("hello there", 0, 1000),
                    MachineSegment::timed("second line", 1000, 2000),
                ],
                engine_id: "whisper.cpp".into(),
                model_id: "small".into(),
                alignment_origin: AlignmentOrigin::Native,
            })
            .await
            .unwrap();
        assert_eq!(machine.kind, RevisionKind::Machine);
        assert_eq!(machine.segment_count, 2);
        assert!(!machine.superseded);

        let saved = fixture
            .backend
            .save_transcript_revision(ValidatedSaveRevision {
                recording_id,
                base_revision_id: machine.revision_id,
                edits: vec![(1, "corrected line".to_owned())],
            })
            .await
            .unwrap();
        assert_eq!(saved.stale_output_ids.len(), 0);

        // The old revision is still readable, unchanged.
        let old = fixture
            .backend
            .get_transcript(ValidatedGetTranscript {
                revision_id: machine.revision_id,
                cursor: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(old.segments[1].text, "second line");
        assert!(old.revision.superseded);

        let new = fixture
            .backend
            .get_transcript(ValidatedGetTranscript {
                revision_id: saved.revision_id,
                cursor: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(new.segments[1].text, "corrected line");
        assert_eq!(new.segments[0].text, "hello there");
        assert_eq!(new.revision.kind, RevisionKind::Human);

        // Saving again against the stale base conflicts.
        let err = fixture
            .backend
            .save_transcript_revision(ValidatedSaveRevision {
                recording_id,
                base_revision_id: machine.revision_id,
                edits: vec![(0, "another edit".to_owned())],
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::Conflict);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_text_only_revision_never_gains_times() {
        let fixture = backend_fixture().await;
        let recording_id = imported_recording(&fixture, "notes.m4a").await;

        let revision = fixture
            .backend
            .create_machine_revision(MachineRevisionSpec {
                recording_id,
                segments: vec![MachineSegment::text_only("no timeline at all")],
                engine_id: "whisper.cpp".into(),
                model_id: "small".into(),
                alignment_origin: AlignmentOrigin::None,
            })
            .await
            .unwrap();
        assert_eq!(revision.alignment_origin, AlignmentOrigin::None);

        let page = fixture
            .backend
            .get_transcript(ValidatedGetTranscript {
                revision_id: revision.revision_id,
                cursor: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(page.segments[0].start_ms, None);
        assert_eq!(page.segments[0].end_ms, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn times_on_an_unaligned_revision_are_rejected_by_the_domain() {
        let fixture = backend_fixture().await;
        let recording_id = imported_recording(&fixture, "bad.flac").await;

        let err = fixture
            .backend
            .create_machine_revision(MachineRevisionSpec {
                recording_id,
                segments: vec![MachineSegment::timed("x", 0, 10)],
                engine_id: "whisper.cpp".into(),
                model_id: "small".into(),
                alignment_origin: AlignmentOrigin::None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preferences_round_trip_and_default_to_everything_off() {
        let fixture = backend_fixture().await;
        let defaults = fixture.backend.preferences().await.unwrap();
        assert!(!defaults.onboarding_completed);
        assert!(!defaults.notifications.enabled);
        assert_eq!(defaults.notifications.categories.len(), 4);

        let mut categories = BTreeMap::new();
        for category in NotificationCategory::ALL {
            categories.insert(*category, true);
        }
        let response = fixture
            .backend
            .set_notification_settings(NotificationSettings {
                enabled: true,
                categories,
            })
            .await
            .unwrap();
        assert!(response.settings.enabled);
        assert_eq!(
            response.os_permission,
            OsNotificationPermission::NotRequested
        );
        assert!(
            fixture
                .backend
                .preferences()
                .await
                .unwrap()
                .notifications
                .enabled
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_library_filters_sorts_and_pages() {
        let fixture = backend_fixture().await;
        for (index, name) in ["b.wav", "a.wav", "c.wav"].iter().enumerate() {
            let path = fixture.source_file(name, format!("bytes {index}").as_bytes());
            fixture
                .backend
                .import_files(vec![path], DuplicatePolicy::Ask)
                .await
                .unwrap();
        }

        let sort = RecordingSort {
            field: RecordingSortField::Title,
            direction: SortDirection::Asc,
        };
        let sorted = fixture
            .backend
            .list_recordings(ValidatedListRecordings {
                cursor: PageCursor::START,
                limit: 2,
                filter: ValidatedRecordingFilter::default(),
                sort,
            })
            .await
            .unwrap();
        assert_eq!(sorted.items.len(), 2);
        assert_eq!(sorted.items[0].title, "a");
        assert_eq!(sorted.total, 3);
        let cursor = sorted.next_cursor.clone().unwrap();

        let rest = fixture
            .backend
            .list_recordings(ValidatedListRecordings {
                cursor: PageCursor::decode(&cursor).unwrap(),
                limit: 2,
                filter: ValidatedRecordingFilter::default(),
                sort,
            })
            .await
            .unwrap();
        assert_eq!(rest.items.len(), 1);
        assert_eq!(rest.items[0].title, "c");
        assert!(rest.next_cursor.is_none());

        let filtered = fixture
            .backend
            .list_recordings(ValidatedListRecordings {
                cursor: PageCursor::START,
                limit: 10,
                filter: ValidatedRecordingFilter {
                    query: Some("a".to_owned()),
                    sources: vec![SourceKind::ManualImport],
                    transcription_status: vec![TranscriptionStatus::None],
                    import_status: vec![ImportStatus::Imported],
                    ..ValidatedRecordingFilter::default()
                },
                sort,
            })
            .await
            .unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.items[0].title, "a");

        let detail = fixture
            .backend
            .get_recording(filtered.items[0].recording_id)
            .await
            .unwrap();
        assert_eq!(
            detail.summary.duration_ms, None,
            "no decoder measured a duration"
        );
        assert_eq!(detail.summary.recorded_at, None, "no capture time is known");
        assert_eq!(detail.media.sample_rate_hz, None);
        assert_eq!(detail.media.original_file_name, "a.wav");
        assert_eq!(detail.outputs.len(), 0);
        assert_eq!(detail.current_revision_id, None);
        assert_eq!(detail.summary.source.source_kind, SourceKind::ManualImport);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_records_report_record_not_found() {
        let fixture = backend_fixture().await;
        let err = fixture
            .backend
            .get_recording(RecordingId::new())
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::RecordNotFound);
        let err = fixture
            .backend
            .get_transcript(ValidatedGetTranscript {
                revision_id: TranscriptRevisionId::new(),
                cursor: 0,
                limit: 10,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::RecordNotFound);
    }

    /// Sink that checks, at emission time, that the change the event
    /// describes is already readable from the database.
    #[derive(Debug, Default)]
    struct CommitAssertingSink {
        pool: tokio::sync::Mutex<Option<SqlitePool>>,
        observations: tokio::sync::Mutex<Vec<(EventKind, bool)>>,
    }

    #[async_trait]
    impl EventSink for CommitAssertingSink {
        async fn emit(&self, event: crate::dto::UiEvent) {
            let pool = self.pool.lock().await.clone();
            let Some(pool) = pool else { return };
            let visible = match (event.recording_id, event.job_id) {
                (Some(recording_id), _) => {
                    sqlx::query_scalar::<_, String>("SELECT id FROM recordings WHERE id = ?1")
                        .bind(recording_id.to_string())
                        .fetch_optional(&pool)
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                }
                (None, Some(job_id)) => {
                    sqlx::query_scalar::<_, String>("SELECT id FROM jobs WHERE id = ?1")
                        .bind(job_id.to_string())
                        .fetch_optional(&pool)
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                }
                (None, None) => true,
            };
            self.observations.lock().await.push((event.kind, visible));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_event_is_emitted_before_its_transaction_commits() {
        let dir = tempfile::tempdir().unwrap();
        let imports = tempfile::tempdir().unwrap();
        let sink = Arc::new(CommitAssertingSink::default());
        let backend = LocalBackend::open(
            dir.path(),
            Arc::clone(&sink) as Arc<dyn EventSink>,
            Arc::new(plaudon_notifications::Dispatcher::new(
                plaudon_notifications::testing::RecordingNotifier::granted(),
                plaudon_notifications::NotificationSettings::uniform(false),
            )),
            Arc::new(crate::secrets::InMemorySecretStore::new()),
            vec![imports.path().to_path_buf()],
        )
        .await
        .unwrap();
        *sink.pool.lock().await = Some(backend.pool().clone());

        let path = imports.path().join("committed.wav");
        std::fs::write(&path, b"RIFF committed").unwrap();
        backend
            .import_files(
                vec![path.to_string_lossy().into_owned()],
                crate::dto::DuplicatePolicy::Ask,
            )
            .await
            .unwrap();

        let observations = sink.observations.lock().await.clone();
        assert!(!observations.is_empty(), "the import emitted events");
        for (kind, visible) in observations {
            assert!(
                visible,
                "{} was emitted before its transaction committed",
                kind.as_str()
            );
        }
    }

    /// Enable notifications for the import category and report what the OS
    /// answered.
    async fn enable_import_notifications(
        fixture: &super::testing::Fixture,
    ) -> OsNotificationPermission {
        let mut categories = BTreeMap::new();
        for category in NotificationCategory::ALL {
            categories.insert(*category, *category == NotificationCategory::ImportFinished);
        }
        fixture
            .backend
            .set_notification_settings(NotificationSettings {
                enabled: true,
                categories,
            })
            .await
            .unwrap()
            .os_permission
    }

    async fn import_one(fixture: &super::testing::Fixture, name: &str) {
        let path = fixture.source_file(name, format!("RIFF {name}").as_bytes());
        fixture
            .backend
            .import_files(vec![path], crate::dto::DuplicatePolicy::Ask)
            .await
            .unwrap();
        fixture.backend.notifications().flush().await;
    }

    async fn only_activity_entry(fixture: &super::testing::Fixture) -> crate::dto::ActivityEntry {
        let page = fixture
            .backend
            .list_activity(crate::validate::ValidatedListActivity {
                cursor: PageCursor::START,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        page.items[0].clone()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_granted_notification_is_delivered_and_carries_no_user_content() {
        let fixture = super::testing::fixture_with_notifier(
            plaudon_notifications::testing::RecordingNotifier::granted(),
        )
        .await;
        assert_eq!(
            enable_import_notifications(&fixture).await,
            OsNotificationPermission::Granted
        );

        import_one(&fixture, "quarterly-board-review.wav").await;

        let entry = only_activity_entry(&fixture).await;
        assert!(entry.notification_delivered);
        assert_eq!(entry.notification_suppressed_reason, None);
        assert_eq!(entry.title, "Import finished");

        let deliveries = fixture.notifier.deliveries().await;
        assert_eq!(deliveries.len(), 1);
        let payload = format!("{} {}", deliveries[0].title, deliveries[0].body);
        for forbidden in [
            "quarterly-board-review",
            ".wav",
            "/",
            "added to your library",
        ] {
            assert!(
                !payload.contains(forbidden),
                "payload leaked {forbidden}: {payload}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_denied_permission_still_leaves_the_activity_entry_and_a_completed_job() {
        let fixture = super::testing::fixture_with_notifier(
            plaudon_notifications::testing::RecordingNotifier::with_permission(
                plaudon_notifications::OsPermission::Denied,
            ),
        )
        .await;
        assert_eq!(
            enable_import_notifications(&fixture).await,
            OsNotificationPermission::Denied
        );

        import_one(&fixture, "denied.wav").await;

        let entry = only_activity_entry(&fixture).await;
        assert!(!entry.notification_delivered);
        assert_eq!(
            entry.notification_suppressed_reason.as_deref(),
            Some("suppressed_by_os_permission")
        );
        assert_eq!(fixture.notifier.deliveries().await.len(), 0);

        let jobs = fixture
            .backend
            .list_jobs(ValidatedListJobs {
                cursor: PageCursor::START,
                limit: 10,
                states: Vec::new(),
                kinds: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(jobs.items[0].state, JobState::Completed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_or_hanging_backend_cannot_touch_the_job() {
        for behavior in [
            plaudon_notifications::testing::Behavior::Panic,
            plaudon_notifications::testing::Behavior::Hang(std::time::Duration::from_secs(30)),
        ] {
            let notifier = plaudon_notifications::testing::RecordingNotifier::granted();
            notifier.set_behavior(behavior).await;
            let fixture = super::testing::fixture_with_notifier(notifier).await;
            enable_import_notifications(&fixture).await;

            import_one(&fixture, "contained.wav").await;

            let entry = only_activity_entry(&fixture).await;
            assert!(!entry.notification_delivered);
            assert!(entry.notification_suppressed_reason.is_some());

            let page = fixture.backend.list_recordings(list_all()).await.unwrap();
            assert_eq!(page.total, 1, "the import committed despite the backend");
            let jobs = fixture
                .backend
                .list_jobs(ValidatedListJobs {
                    cursor: PageCursor::START,
                    limit: 10,
                    states: Vec::new(),
                    kinds: Vec::new(),
                })
                .await
                .unwrap();
            assert_eq!(jobs.items[0].state, JobState::Completed);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_opted_out_category_records_activity_without_touching_the_os() {
        let fixture = super::testing::fixture_with_notifier(
            plaudon_notifications::testing::RecordingNotifier::granted(),
        )
        .await;
        // Master switch on, this category off.
        let mut categories = BTreeMap::new();
        for category in NotificationCategory::ALL {
            categories.insert(*category, false);
        }
        fixture
            .backend
            .set_notification_settings(NotificationSettings {
                enabled: true,
                categories,
            })
            .await
            .unwrap();

        import_one(&fixture, "silent.wav").await;

        let entry = only_activity_entry(&fixture).await;
        assert!(!entry.notification_delivered);
        assert_eq!(
            entry.notification_suppressed_reason.as_deref(),
            Some("suppressed_by_preference")
        );
        assert_eq!(fixture.notifier.deliveries().await.len(), 0);
        assert_eq!(
            fixture.notifier.permission_prompts().await,
            0,
            "an opted-out category never prompts the OS"
        );
    }
}
