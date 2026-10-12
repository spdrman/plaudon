//! Restoring a `.recording-archive` into a library (spec 13.6, 13.8, 16.5).
//!
//! Four stages, in this order, with the library untouched until the last one:
//!
//! 1. **Inspect.** Preamble, manifest, entry policy. No filesystem access
//!    under the library, no decryption of payloads beyond the manifest.
//! 2. **Stage.** Every payload is extracted into a private `0700` directory on
//!    the library's own volume and verified against the manifest digest. A
//!    failure here deletes the staging tree and ends the restore.
//! 3. **Commit media.** Verified blobs are renamed into the content-addressed
//!    archive. A blob already present is reused, not rewritten.
//! 4. **Integrate.** One SQLite transaction inserts the records. Re-running
//!    the same restore is a no-op, which is what makes an interrupted restore
//!    resumable rather than half-applied.
//!
//! Interruption between 3 and 4 leaves content-addressed blobs with no rows
//! referencing them. They are inert, they are what the next attempt reuses,
//! and the library the user sees is unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plaudon_domain::ids::ImportIntentId;
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use plaudon_persistence::archive::{relative_path, MediaArchive};
use plaudon_persistence::{Database, PersistenceError};
use plaudon_security::fsguard::PrivateTempDir;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::documents::{
    AnnotationDocument, OutputDocument, RecordDocument, SourceDocument, ToolDocument,
    TranscriptDocument,
};
use super::manifest::{Manifest, ManifestEntry};
use super::reader::{self, EntrySink, InspectionReport};
use crate::error::{ArchiveRejection, ExportError, ExportResult};

/// Staging-directory prefix for restores.
pub const STAGING_PREFIX: &str = "plaudon-restore";

/// Where restored records should land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// Refuse unless the target library has no recordings.
    NewLibrary,
    /// Add to an existing library, reporting conflicts.
    Merge,
}

/// What to do when a record already exists in the target library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Keep the library's version and report the conflict.
    Skip,
    /// Abort the whole restore without changing the library.
    Fail,
}

/// Options for a restore run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOptions {
    /// New library or merge.
    pub mode: RestoreMode,
    /// What to do about existing records.
    pub on_conflict: ConflictPolicy,
    /// Whether a recording the user deleted locally may be restored.
    ///
    /// Default `false`: spec 7.10 says a deletion must survive, and a restore
    /// is the explicit action that can override it — but only when the user
    /// asked for exactly that.
    pub restore_tombstoned: bool,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            mode: RestoreMode::Merge,
            on_conflict: ConflictPolicy::Skip,
            restore_tombstoned: false,
        }
    }
}

/// One record the restore did not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreConflict {
    /// Archive entry path.
    pub entry_path: String,
    /// Why it was skipped.
    pub reason: String,
}

/// Outcome of a restore.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RestoreReport {
    /// Recordings inserted.
    pub recordings_restored: u64,
    /// Transcript revisions inserted.
    pub transcripts_restored: u64,
    /// Generated outputs inserted.
    pub outputs_restored: u64,
    /// Sources inserted.
    pub sources_restored: u64,
    /// Tool definitions inserted.
    pub tools_restored: u64,
    /// Media blobs committed into the archive.
    pub media_committed: u64,
    /// Media blobs already present and reused.
    pub media_reused: u64,
    /// Records that were not applied.
    pub conflicts: Vec<RestoreConflict>,
}

/// Dry-run result: what an archive contains and what would conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePreview {
    /// Inspection of the archive itself.
    pub inspection: InspectionReport,
    /// Records that already exist in the target library.
    pub conflicts: Vec<RestoreConflict>,
    /// Whether the target library currently has no recordings.
    pub target_is_empty: bool,
}

/// Extracts verified payloads into a private staging directory.
struct StagingSink {
    directory: PathBuf,
    current: Option<(PathBuf, std::fs::File)>,
    staged: BTreeMap<String, PathBuf>,
    next: u64,
}

impl StagingSink {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            current: None,
            staged: BTreeMap::new(),
            next: 0,
        }
    }
}

impl EntrySink for StagingSink {
    fn begin(&mut self, _entry: &ManifestEntry) -> ExportResult<()> {
        let path = self.directory.join(format!("entry-{:08}", self.next));
        self.next += 1;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        self.current = Some((path, file));
        Ok(())
    }

    fn chunk(&mut self, bytes: &[u8]) -> ExportResult<()> {
        use std::io::Write;
        let (_, file) = self
            .current
            .as_mut()
            .ok_or_else(|| ExportError::IntegrityFailed)?;
        file.write_all(bytes)?;
        Ok(())
    }

    fn commit(&mut self, entry: &ManifestEntry) -> ExportResult<()> {
        let (path, file) = self
            .current
            .take()
            .ok_or_else(|| ExportError::IntegrityFailed)?;
        file.sync_all()?;
        drop(file);
        self.staged.insert(entry.path.clone(), path);
        Ok(())
    }
}

/// Restores archives into a library.
#[derive(Debug, Clone)]
pub struct Restorer {
    db: Database,
    media: MediaArchive,
    library_root: PathBuf,
}

impl Restorer {
    /// Wrap a library.
    #[must_use]
    pub fn new(db: Database, media: MediaArchive, library_root: impl Into<PathBuf>) -> Self {
        Self {
            db,
            media,
            library_root: library_root.into(),
        }
    }

    /// Inspect an archive and report conflicts without writing anything.
    pub async fn dry_run(
        &self,
        archive_path: &Path,
        password: Option<&str>,
    ) -> ExportResult<RestorePreview> {
        let probe = self.library_root.join("temp").join("restore-dry-run");
        let inspection = reader::inspect(archive_path, password, &probe)?;
        let mut conflicts = Vec::new();
        for entry in &inspection.manifest.entries {
            if let Some(id) = entry.path.strip_prefix("records/") {
                let recording_id = id.trim_end_matches(".json");
                if let Some(reason) = self.existing_reason(recording_id).await? {
                    conflicts.push(RestoreConflict {
                        entry_path: entry.path.clone(),
                        reason,
                    });
                }
            }
        }
        Ok(RestorePreview {
            inspection,
            conflicts,
            target_is_empty: self.recording_count().await? == 0,
        })
    }

    /// Restore an archive.
    pub async fn restore(
        &self,
        archive_path: &Path,
        password: Option<&str>,
        options: &RestoreOptions,
    ) -> ExportResult<RestoreReport> {
        if options.mode == RestoreMode::NewLibrary && self.recording_count().await? != 0 {
            return Err(ExportError::TargetLibraryNotEmpty);
        }

        let staging_base = self.library_root.join("temp");
        std::fs::create_dir_all(&staging_base)?;
        let staging = PrivateTempDir::create(Some(&staging_base), STAGING_PREFIX)
            .map_err(|error| ExportError::DestinationRejected(error.reason().to_string()))?;

        let mut sink = StagingSink::new(staging.path().to_path_buf());
        let (manifest, _) = reader::stream(archive_path, password, staging.path(), &mut sink)?;

        let report = self
            .integrate(&manifest, &sink.staged, options)
            .await;
        // The staging directory is removed when `staging` drops, whether the
        // integration succeeded or not.
        drop(staging);
        report
    }

    async fn integrate(
        &self,
        manifest: &Manifest,
        staged: &BTreeMap<String, PathBuf>,
        options: &RestoreOptions,
    ) -> ExportResult<RestoreReport> {
        let mut report = RestoreReport::default();

        // Parse every document before touching the library, so a malformed
        // document fails the restore rather than half-applying it.
        let mut records: Vec<RecordDocument> = Vec::new();
        let mut annotations: Vec<AnnotationDocument> = Vec::new();
        let mut transcripts: Vec<TranscriptDocument> = Vec::new();
        let mut outputs: Vec<OutputDocument> = Vec::new();
        let mut sources: Vec<SourceDocument> = Vec::new();
        let mut tools: Vec<ToolDocument> = Vec::new();
        let mut media: Vec<(&ManifestEntry, &PathBuf)> = Vec::new();

        for entry in &manifest.entries {
            let path = staged.get(&entry.path).ok_or_else(|| {
                ExportError::ArchiveRejected(ArchiveRejection::Malformed(format!(
                    "entry {} was not staged",
                    entry.path
                )))
            })?;
            if entry.path.starts_with("media/") {
                media.push((entry, path));
                continue;
            }
            let bytes = std::fs::read(path)?;
            let malformed = |what: &str| {
                ExportError::ArchiveRejected(ArchiveRejection::Malformed(format!(
                    "{} document {} is not valid",
                    what, entry.path
                )))
            };
            if entry.path.starts_with("records/") {
                records.push(serde_json::from_slice(&bytes).map_err(|_| malformed("record"))?);
            } else if entry.path.starts_with("annotations/") {
                annotations
                    .push(serde_json::from_slice(&bytes).map_err(|_| malformed("annotation"))?);
            } else if entry.path.starts_with("transcripts/") {
                transcripts
                    .push(serde_json::from_slice(&bytes).map_err(|_| malformed("transcript"))?);
            } else if entry.path.starts_with("outputs/") {
                outputs.push(serde_json::from_slice(&bytes).map_err(|_| malformed("output"))?);
            } else if entry.path.starts_with("sources/") {
                sources.push(serde_json::from_slice(&bytes).map_err(|_| malformed("source"))?);
            } else if entry.path.starts_with("tools/") {
                tools.push(serde_json::from_slice(&bytes).map_err(|_| malformed("tool"))?);
            }
        }

        // Decide what is skipped before anything is written.
        let mut skipped_recordings: BTreeMap<String, String> = BTreeMap::new();
        for record in &records {
            let id = record.recording.id.clone();
            if let Some(reason) = self.existing_reason(&id).await? {
                if reason == "deleted locally" && options.restore_tombstoned {
                    continue;
                }
                if options.on_conflict == ConflictPolicy::Fail {
                    return Err(ExportError::RestoreConflict(format!(
                        "recording {id} {reason}"
                    )));
                }
                report.conflicts.push(RestoreConflict {
                    entry_path: format!("records/{id}.json"),
                    reason: reason.clone(),
                });
                skipped_recordings.insert(id, reason);
            }
        }

        // Commit media only after every document parsed and every conflict is
        // known. A blob already in the archive is reused as-is.
        let mut committed_paths: BTreeMap<String, String> = BTreeMap::new();
        for (entry, staged_path) in &media {
            let digest: Sha256Digest = entry
                .sha256
                .parse()
                .map_err(|_| ExportError::IntegrityFailed)?;
            let extension = entry
                .path
                .rsplit_once('.')
                .map_or("bin", |(_, extension)| extension);
            let identity = MediaIdentity::new(digest, entry.byte_length, String::new());
            let relative = relative_path(&identity, extension);
            if self.media.exists(&relative)? {
                self.media.verify(&relative, &identity)?;
                report.media_reused += 1;
            } else {
                let intent = ImportIntentId::new();
                let staging_path = self.media.staging_path(intent);
                if let Some(parent) = staging_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(staged_path, &staging_path)?;
                self.media.commit_staged(&staging_path, &relative)?;
                report.media_committed += 1;
            }
            committed_paths.insert(entry.sha256.clone(), relative);
        }

        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(PersistenceError::from)?;

        for source in &sources {
            let affected = sqlx::query(
                "INSERT OR IGNORE INTO sources
                     (id, source_kind, account_scope, display_name, status, capabilities_json,
                      last_verified_at, sync_cursor, sync_policy, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, '{}', NULL, NULL, ?6, ?7)",
            )
            .bind(&source.id)
            .bind(&source.source_kind)
            .bind(&source.account_scope)
            .bind(&source.display_name)
            .bind(&source.status)
            .bind(&source.sync_policy)
            .bind(source.created_at)
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?
            .rows_affected();
            report.sources_restored += affected;
        }

        for record in &records {
            if skipped_recordings.contains_key(&record.recording.id) {
                continue;
            }
            let title = annotations
                .iter()
                .find(|annotation| annotation.recording_id == record.recording.id)
                .map_or("Restored recording", |annotation| annotation.title.as_str());

            for asset in &record.media_assets {
                let Some(relative) = committed_paths.get(&asset.sha256) else {
                    return Err(ExportError::ArchiveRejected(ArchiveRejection::Malformed(
                        format!(
                            "recording {} references media {} that the archive does not carry",
                            record.recording.id, asset.sha256
                        ),
                    )));
                };
                sqlx::query(
                    "INSERT OR IGNORE INTO media_assets
                         (id, sha256, byte_length, relative_path, mime, codec, sample_rate,
                          channels, duration_ms, quality_class, verification_state, verified_at,
                          created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'verified', ?11, ?11)",
                )
                .bind(&asset.id)
                .bind(&asset.sha256)
                .bind(i64::try_from(asset.byte_length).unwrap_or(i64::MAX))
                .bind(relative)
                .bind(asset.mime.as_deref())
                .bind(asset.codec.as_deref())
                .bind(asset.sample_rate.map(i64::from))
                .bind(asset.channels.map(i64::from))
                .bind(asset.duration_ms.map(|v| i64::try_from(v).unwrap_or(i64::MAX)))
                .bind(&asset.quality_class)
                .bind(asset.created_at)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }

            let affected = sqlx::query(
                "INSERT OR IGNORE INTO recordings
                     (id, title, captured_at, duration_ms, preferred_media_asset_id, annotation,
                      created_at, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            )
            .bind(&record.recording.id)
            .bind(title)
            .bind(record.recording.captured_at)
            .bind(
                record
                    .recording
                    .duration_ms
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
            )
            .bind(record.recording.preferred_media_asset_id.as_deref())
            .bind(
                annotations
                    .iter()
                    .find(|annotation| annotation.recording_id == record.recording.id)
                    .and_then(|annotation| annotation.annotation.as_deref()),
            )
            .bind(record.recording.created_at)
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?
            .rows_affected();
            report.recordings_restored += affected;

            for link in &record.recording_media {
                sqlx::query(
                    "INSERT OR IGNORE INTO recording_media
                         (recording_id, media_asset_id, quality_class, obtained_at)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(&record.recording.id)
                .bind(&link.media_asset_id)
                .bind(&link.quality_class)
                .bind(link.obtained_at)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }

            for revision in &record.media_revisions {
                sqlx::query(
                    "INSERT OR IGNORE INTO media_revisions
                         (id, recording_id, media_asset_id, source_item_id, source_version,
                          predecessor_id, obtained_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .bind(&revision.id)
                .bind(&record.recording.id)
                .bind(&revision.media_asset_id)
                .bind(revision.source_item_id.as_deref())
                .bind(revision.source_version.as_deref())
                .bind(revision.predecessor_id.as_deref())
                .bind(revision.obtained_at)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }

            for item in &record.source_items {
                // A source that is not in the archive would violate the
                // foreign key; skip rather than fabricate a source row.
                let known = sources.iter().any(|source| source.id == item.source_id)
                    || sqlx::query("SELECT 1 FROM sources WHERE id = ?1")
                        .bind(&item.source_id)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(PersistenceError::from)?
                        .is_some();
                if !known {
                    report.conflicts.push(RestoreConflict {
                        entry_path: format!("records/{}.json", record.recording.id),
                        reason: format!("source {} is not part of the archive", item.source_id),
                    });
                    continue;
                }
                sqlx::query(
                    "INSERT OR IGNORE INTO source_items
                         (id, source_id, external_id, recording_id, import_status, last_seen_at,
                          remote_version, remote_state, tombstone_reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .bind(&item.id)
                .bind(&item.source_id)
                .bind(item.external_id.as_deref())
                .bind(&record.recording.id)
                .bind(&item.import_status)
                .bind(item.last_seen_at)
                .bind(item.remote_version.as_deref())
                .bind(&item.remote_state)
                .bind(item.tombstone_reason.as_deref())
                .bind(item.created_at)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }
        }

        for annotation in &annotations {
            if skipped_recordings.contains_key(&annotation.recording_id) {
                continue;
            }
            for tag in &annotation.tags {
                sqlx::query(
                    "INSERT OR IGNORE INTO recording_tags (recording_id, tag) VALUES (?1, ?2)",
                )
                .bind(&annotation.recording_id)
                .bind(tag)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }
            for speaker in &annotation.speakers {
                sqlx::query(
                    "INSERT OR IGNORE INTO speakers
                         (id, recording_id, label, name, provenance, user_confirmed)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .bind(&speaker.id)
                .bind(&annotation.recording_id)
                .bind(&speaker.label)
                .bind(speaker.name.as_deref())
                .bind(&speaker.provenance)
                .bind(i64::from(speaker.user_confirmed))
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }
        }

        for document in &transcripts {
            if skipped_recordings.contains_key(&document.recording_id) {
                continue;
            }
            let revision = &document.revision;
            let affected = sqlx::query(
                "INSERT OR IGNORE INTO transcript_revisions
                     (id, recording_id, parent_id, origin, input_sha256, engine, model_id,
                      model_version, locale, alignment_origin, status, normalization_profile,
                      chunking_strategy, duration_ms, created_at, engine_backend,
                      engine_params_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            )
            .bind(&revision.id)
            .bind(&document.recording_id)
            .bind(revision.parent_id.as_deref())
            .bind(&revision.origin)
            .bind(&revision.input_sha256)
            .bind(&revision.engine)
            .bind(&revision.model_id)
            .bind(revision.model_version.as_deref())
            .bind(revision.locale.as_deref())
            .bind(&revision.alignment_origin)
            .bind(&revision.status)
            .bind(&revision.normalization_profile)
            .bind(&revision.chunking_strategy)
            .bind(
                revision
                    .duration_ms
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
            )
            .bind(revision.created_at)
            .bind(revision.engine_backend.as_deref())
            .bind(revision.engine_params_json.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?
            .rows_affected();
            report.transcripts_restored += affected;

            for segment in &document.segments {
                sqlx::query(
                    "INSERT OR IGNORE INTO transcript_segments
                         (id, transcript_revision_id, seq_index, start_ms, end_ms, speaker_id,
                          text, confidence, confidence_scale, alignment_origin)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .bind(&segment.id)
                .bind(&revision.id)
                .bind(i64::from(segment.seq_index))
                .bind(segment.start_ms.map(|v| i64::try_from(v).unwrap_or(i64::MAX)))
                .bind(segment.end_ms.map(|v| i64::try_from(v).unwrap_or(i64::MAX)))
                .bind(segment.speaker_id.as_deref())
                .bind(&segment.text)
                .bind(segment.confidence)
                .bind(segment.confidence_scale.as_deref())
                .bind(&segment.alignment_origin)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;

                for word in &segment.words {
                    sqlx::query(
                        "INSERT OR IGNORE INTO transcript_words
                             (id, segment_id, seq_index, start_ms, end_ms, text, confidence,
                              confidence_scale)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    )
                    .bind(&word.id)
                    .bind(&segment.id)
                    .bind(i64::from(word.seq_index))
                    .bind(i64::try_from(word.start_ms).unwrap_or(i64::MAX))
                    .bind(i64::try_from(word.end_ms).unwrap_or(i64::MAX))
                    .bind(&word.text)
                    .bind(word.confidence)
                    .bind(word.confidence_scale.as_deref())
                    .execute(&mut *tx)
                    .await
                    .map_err(PersistenceError::from)?;
                }
            }

            for annotation in &document.annotations {
                sqlx::query(
                    "INSERT OR IGNORE INTO transcript_annotations
                         (id, transcript_revision_id, segment_index, kind, reason, start_ms, end_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .bind(&annotation.id)
                .bind(&revision.id)
                .bind(i64::from(annotation.segment_index))
                .bind(&annotation.kind)
                .bind(&annotation.reason)
                .bind(
                    annotation
                        .start_ms
                        .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
                )
                .bind(annotation.end_ms.map(|v| i64::try_from(v).unwrap_or(i64::MAX)))
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }
        }

        for document in &outputs {
            if skipped_recordings.contains_key(&document.recording_id) {
                continue;
            }
            let output = &document.output;
            let affected = sqlx::query(
                "INSERT OR IGNORE INTO ai_outputs
                     (id, recording_id, output_type, transcript_revision_id, template_version,
                      model_id, prompt_version, schema_version, body_json, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .bind(&output.id)
            .bind(&document.recording_id)
            .bind(&output.output_type)
            .bind(&output.transcript_revision_id)
            .bind(&output.template_version)
            .bind(&output.model_id)
            .bind(&output.prompt_version)
            .bind(&output.schema_version)
            .bind(&output.body_json)
            .bind(&output.status)
            .bind(output.created_at)
            .execute(&mut *tx)
            .await
            .map_err(PersistenceError::from)?
            .rows_affected();
            report.outputs_restored += affected;

            for evidence in &document.evidence {
                sqlx::query(
                    "INSERT OR IGNORE INTO output_evidence
                         (id, output_id, item_path, segment_id, quote, verification_status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .bind(&evidence.id)
                .bind(&output.id)
                .bind(&evidence.item_path)
                .bind(&evidence.segment_id)
                .bind(&evidence.quote)
                .bind(&evidence.verification_status)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?;
            }
        }

        if !tools.is_empty() && table_exists(&mut tx, "transcript_tools").await? {
            for tool in &tools {
                let affected = sqlx::query(
                    "INSERT OR IGNORE INTO transcript_tools
                         (id, version, display_name, kind, prompt_version, output_schema_version)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .bind(&tool.id)
                .bind(&tool.version)
                .bind(&tool.display_name)
                .bind(&tool.kind)
                .bind(&tool.prompt_version)
                .bind(&tool.output_schema_version)
                .execute(&mut *tx)
                .await
                .map_err(PersistenceError::from)?
                .rows_affected();
                report.tools_restored += affected;
            }
        } else {
            for tool in &tools {
                report.conflicts.push(RestoreConflict {
                    entry_path: format!("tools/{}.json", tool.id),
                    reason: "this build has no transcript tool table".to_owned(),
                });
            }
        }

        tx.commit().await.map_err(PersistenceError::from)?;
        Ok(report)
    }

    async fn recording_count(&self) -> ExportResult<i64> {
        let row = sqlx::query("SELECT COUNT(*) FROM recordings")
            .fetch_one(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;
        Ok(row.get::<i64, _>(0))
    }

    /// Why a recording cannot simply be inserted, if it cannot.
    async fn existing_reason(&self, recording_id: &str) -> ExportResult<Option<String>> {
        let row = sqlx::query("SELECT deleted_at FROM recordings WHERE id = ?1")
            .bind(recording_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;
        if let Some(row) = row {
            return Ok(Some(
                if row.get::<Option<i64>, _>("deleted_at").is_some() {
                    "deleted locally".to_owned()
                } else {
                    "already in this library".to_owned()
                },
            ));
        }
        let tombstoned = sqlx::query(
            "SELECT 1 FROM tombstones WHERE entity_type = 'recording' AND entity_id = ?1",
        )
        .bind(recording_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;
        Ok(tombstoned.map(|_| "deleted locally".to_owned()))
    }
}

async fn table_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    name: &str,
) -> ExportResult<bool> {
    let row = sqlx::query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
        .bind(name)
        .fetch_optional(&mut **tx)
        .await
        .map_err(PersistenceError::from)?;
    Ok(row.is_some())
}
