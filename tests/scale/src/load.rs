//! Loading the synthetic dataset through the real persistence layer.
//!
//! Every row is written with the shipping code: [`MigrationRunner`] applies
//! the committed SQL migrations, [`MediaArchive`] stores and re-hashes the
//! media bytes at their content-addressed path, and the repositories in
//! [`plaudon_persistence::repo`] write `recordings`, `media_assets`,
//! `recording_media`, `sources` and `source_items`.
//!
//! Transcript revisions, segments and speakers have no repository yet, so they
//! are written with statements against the same migrated tables — the schema,
//! the constraints and the indexes are the real ones either way. Nothing here
//! creates a table.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant};

use plaudon_domain::ids::{ImportIntentId, RecordingId, SourceId, SourceItemId, TranscriptRevisionId};
use plaudon_domain::recording::{Recording, RecordingMedia};
use plaudon_domain::media::MediaAsset;
use plaudon_domain::source::{
    ExternalRecordingId, ImportStatus, RemoteState, SourceAccountScope, SourceAssociation,
    SourceIdentityKey, SourceKind,
};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::{self, MediaArchive};
use plaudon_persistence::error::{PersistenceError, PersistenceResult};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::{MediaAssetRepository, RecordingRepository, SourceRepository};
use plaudon_persistence::Database;
use plaudon_domain::media::VerificationState;
use serde::Serialize;

use crate::{DatasetSpec, SyntheticLibrary, SyntheticRecording};

/// Segments written per transaction when loading a transcript.
const SEGMENT_BATCH: u32 = 2_000;

/// What a load actually produced, for the published result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LoadStats {
    /// Rows in `recordings`.
    pub recordings: u64,
    /// Rows in `media_assets`.
    pub media_assets: u64,
    /// Rows in `transcript_revisions`.
    pub transcript_revisions: u64,
    /// Rows in `transcript_segments`.
    pub transcript_segments: u64,
    /// Rows in `speakers`.
    pub speakers: u64,
    /// Rows in `source_items`.
    pub source_items: u64,
    /// Bytes stored in the media archive.
    pub archive_bytes: u64,
    /// Size of the SQLite database file after loading.
    pub database_bytes: u64,
    /// Wall time spent loading the whole dataset.
    pub load_wall_ms: u64,
    /// Wall time spent inside the media archive (stage, hash, commit).
    pub archive_wall_ms: u64,
}

/// The marathon transcript of spec 18.6A's UI row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marathon {
    /// Recording holding the long transcript.
    pub recording_id: RecordingId,
    /// Its single transcript revision.
    pub revision_id: TranscriptRevisionId,
    /// Segments in that revision.
    pub segment_count: u32,
}

/// A loaded library: real database, real archive, known contents.
#[derive(Debug)]
pub struct ScaleFixture {
    /// The migrated database.
    pub db: Database,
    /// The content-addressed media archive.
    pub archive: MediaArchive,
    /// The generator that produced the contents.
    pub library: SyntheticLibrary,
    /// What was written.
    pub stats: LoadStats,
    /// The 100k-segment transcript.
    pub marathon: Marathon,
}

impl ScaleFixture {
    /// The dataset spec behind this fixture.
    #[must_use]
    pub const fn spec(&self) -> &DatasetSpec {
        self.library.spec()
    }
}

/// Build the dataset under `root`, returning the live database and archive.
///
/// `root` must be a directory the caller owns for the lifetime of the fixture;
/// the suite uses a `tempfile::TempDir` so a run leaves nothing behind.
pub async fn build(root: &Path, spec: DatasetSpec) -> PersistenceResult<ScaleFixture> {
    let started = Instant::now();
    let library = SyntheticLibrary::new(spec);

    let db = Database::open(root.join("library.sqlite")).await?;
    MigrationRunner::new().run(&db).await?;
    let archive = MediaArchive::open(root.join("archive"))?;

    let media_repo = MediaAssetRepository::new(db.clone());
    let recording_repo = RecordingRepository::new(db.clone());
    let source_repo = SourceRepository::new(db.clone());

    let mut sources: BTreeMap<(SourceKind, String), SourceId> = BTreeMap::new();
    let mut archive_time = Duration::ZERO;
    let mut archive_bytes = 0u64;
    let mut speakers = 0u64;
    let mut segments = 0u64;

    for index in 0..=spec.marathon_index() {
        let recording = library.recording(index);

        let archive_started = Instant::now();
        let relative = store_blob(&archive, &recording)?;
        archive_time += archive_started.elapsed();
        archive_bytes += recording.media.identity.byte_length;

        let asset = MediaAsset {
            id: recording.media.id,
            identity: recording.media.identity.clone(),
            relative_path: relative,
            quality_class: recording.media.quality_class,
            verification_state: VerificationState::Verified,
            verified_at: Some(recording.created_at),
        };
        media_repo.insert(&asset, recording.created_at).await?;

        let mut row = Recording::new(recording.id, recording.title.clone(), recording.created_at);
        row.captured_at = Some(recording.captured_at);
        row.duration_ms = Some(recording.duration_ms);
        row.annotation = (index % 7 == 0).then(|| format!("Imported for scale tier {}", spec.tier));
        row.tags = recording.tags.iter().cloned().collect();
        recording_repo.insert(&row).await?;
        recording_repo
            .attach_media(
                recording.id,
                &RecordingMedia {
                    media_asset_id: recording.media.id,
                    quality_class: recording.media.quality_class,
                    obtained_at: recording.created_at,
                },
            )
            .await?;

        let scope = SourceAccountScope::new(recording.account_scope.clone());
        let source_id = match sources.get(&(recording.source_kind, recording.account_scope.clone()))
        {
            Some(id) => *id,
            None => {
                let id = source_repo
                    .ensure_source(
                        recording.source_kind,
                        &scope,
                        recording.source_kind.provenance_label(),
                        recording.created_at,
                    )
                    .await?;
                sources.insert((recording.source_kind, recording.account_scope.clone()), id);
                id
            }
        };
        source_repo
            .insert_item(
                &SourceAssociation {
                    id: SourceItemId::new(),
                    source_id,
                    identity: SourceIdentityKey::new(
                        recording.source_kind,
                        scope,
                        Some(ExternalRecordingId::new(recording.external_id.clone())),
                    ),
                    recording_id: recording.id,
                    import_status: ImportStatus::Imported,
                    remote_state: RemoteState::Present,
                    remote_version: Some(format!("v{}", index % 3 + 1)),
                    last_seen_at: Some(recording.created_at),
                    tombstone_reason: None,
                },
                recording.created_at,
            )
            .await?;

        speakers += insert_speakers(&db, &recording).await?;
        insert_revision(&db, &recording).await?;
        segments += insert_segments(&db, &library, &recording).await?;
    }

    let marathon_recording = library.recording(spec.marathon_index());
    let database_bytes = std::fs::metadata(db.path()).map(|meta| meta.len()).unwrap_or(0);

    let stats = LoadStats {
        recordings: spec.total_recordings(),
        media_assets: spec.total_recordings(),
        transcript_revisions: spec.total_recordings(),
        transcript_segments: segments,
        speakers,
        source_items: spec.total_recordings(),
        archive_bytes,
        database_bytes,
        load_wall_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        archive_wall_ms: u64::try_from(archive_time.as_millis()).unwrap_or(u64::MAX),
    };

    Ok(ScaleFixture {
        db,
        archive,
        library,
        stats,
        marathon: Marathon {
            recording_id: marathon_recording.id,
            revision_id: marathon_recording.revision_id,
            segment_count: marathon_recording.segment_count,
        },
    })
}

/// Stage, hash and commit one blob through the real archive.
fn store_blob(archive: &MediaArchive, recording: &SyntheticRecording) -> PersistenceResult<String> {
    let staging = archive.staging_path(ImportIntentId::new());
    let mut bytes = Cursor::new(&recording.media.bytes);
    let identity = archive.stage_from_reader(
        &staging,
        &mut bytes,
        &recording.media.identity.observed_codec,
    )?;
    if identity != recording.media.identity {
        // The generator computed the digest itself; a mismatch would mean the
        // dataset and the archive disagree about what was stored.
        return Err(PersistenceError::IntegrityFailed);
    }
    let relative = archive::relative_path(&identity, recording.media.extension);
    archive.commit_staged(&staging, &relative)?;
    Ok(relative)
}

async fn insert_speakers(db: &Database, recording: &SyntheticRecording) -> PersistenceResult<u64> {
    if recording.speakers.is_empty() {
        return Ok(0);
    }
    let mut tx = db.pool().begin().await?;
    for (slot, (id, label)) in recording.speakers.iter().enumerate() {
        sqlx::query(
            "INSERT INTO speakers (id, recording_id, label, name, provenance, user_confirmed)
             VALUES (?1, ?2, ?3, ?4, 'inferred', 0)",
        )
        .bind(id.to_string())
        .bind(recording.id.to_string())
        .bind(format!("S{slot}"))
        .bind(label)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(recording.speakers.len() as u64)
}

async fn insert_revision(db: &Database, recording: &SyntheticRecording) -> PersistenceResult<()> {
    sqlx::query(
        "INSERT INTO transcript_revisions
             (id, recording_id, origin, input_sha256, engine, model_id, model_version, locale,
              alignment_origin, status, normalization_profile, chunking_strategy, duration_ms,
              created_at)
         VALUES (?1, ?2, 'machine', ?3, ?4, ?5, 'scale-fixture', ?6, 'native', 'complete',
                 'nfc', 'fixed-window', ?7, ?8)",
    )
    .bind(recording.revision_id.to_string())
    .bind(recording.id.to_string())
    .bind(recording.media.identity.sha256.to_hex())
    .bind(recording.engine)
    .bind(recording.model_id)
    .bind(recording.locale.as_str())
    .bind(i64::try_from(recording.duration_ms).unwrap_or(i64::MAX))
    .bind(recording.created_at.as_millis())
    .execute(db.pool())
    .await?;
    Ok(())
}

async fn insert_segments(
    db: &Database,
    library: &SyntheticLibrary,
    recording: &SyntheticRecording,
) -> PersistenceResult<u64> {
    let mut written = 0u64;
    let mut from = 0u32;
    while from < recording.segment_count {
        let batch = library.segments_of(recording, from, SEGMENT_BATCH);
        if batch.is_empty() {
            break;
        }
        let mut tx = db.pool().begin().await?;
        for segment in &batch {
            let speaker_id = segment
                .speaker_slot
                .and_then(|slot| recording.speakers.get(usize::from(slot)))
                .map(|(id, _)| id.to_string());
            sqlx::query(
                "INSERT INTO transcript_segments
                     (id, transcript_revision_id, seq_index, start_ms, end_ms, speaker_id, text,
                      confidence, confidence_scale, alignment_origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'probability', 'native')",
            )
            .bind(segment.id.to_string())
            .bind(recording.revision_id.to_string())
            .bind(i64::from(segment.seq_index))
            .bind(i64::try_from(segment.start_ms).unwrap_or(i64::MAX))
            .bind(i64::try_from(segment.end_ms).unwrap_or(i64::MAX))
            .bind(speaker_id)
            .bind(&segment.text)
            .bind(segment.confidence)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        written += batch.len() as u64;
        from += SEGMENT_BATCH;
    }
    Ok(written)
}
