//! The allowlisted projection every export renders from (spec 13.6, 22(9)).
//!
//! Exports and archives serialize **this** structure, never a database row.
//! Each field is named explicitly here, so a column added to SQLite later is
//! excluded until somebody adds it on purpose. `tests/integration` holds the
//! coverage test that fails when a new column appears without a decision.
//!
//! Nothing in this module reads a credential table. `device_credential_refs`,
//! `sources.sync_cursor`, `sources.capabilities_json`, `inference_attempts`
//! and the job tables are deliberately absent: they hold vault handles, route
//! snapshots and transient state that spec 13.6 excludes from archives.

use plaudon_domain::ids::{MediaAssetId, RecordingId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use plaudon_persistence::{Database, PersistenceError};

use crate::error::{ExportError, ExportResult};

/// Schema version of the recording export document.
pub const EXPORT_SCHEMA_VERSION: &str = "recording-export/1";

/// Sentence every metadata export carries, so a JSON file is never mistaken
/// for a backup (spec 13.6, ADR-012).
pub const NOT_A_BACKUP_NOTICE: &str = "This export contains metadata and text only. It is not a backup: it does not include your audio and cannot be restored into a library. Use Create backup (.recording-archive) for a restorable copy.";

/// Sentence every export carries about anchors (spec 9.6A).
pub const ANCHOR_NOTICE: &str = "A source anchor proves that a passage exists in the cited transcript revision. It does not prove the claim is correct; items marked 'unverified' or 'review required' still need a human.";

/// Stamp identifying the writer of an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStamp {
    /// Application name.
    pub name: String,
    /// Application version.
    pub version: String,
}

impl Default for AppStamp {
    fn default() -> Self {
        Self {
            name: "plaudon".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// Facts about the logical meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingFacts {
    /// Durable recording identity.
    pub id: RecordingId,
    /// User-editable title.
    pub title: String,
    /// Capture instant, when known.
    pub captured_at: Option<Timestamp>,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Row creation instant.
    pub created_at: Timestamp,
    /// Local deletion instant, when the recording is tombstoned.
    pub deleted_at: Option<Timestamp>,
    /// User-editable tags, sorted.
    pub tags: Vec<String>,
    /// User-editable free-form annotation.
    pub annotation: Option<String>,
    /// Preferred media candidate, when one is set.
    pub preferred_media_asset_id: Option<MediaAssetId>,
}

/// Facts about one acquired media candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFacts {
    /// Asset identity.
    pub id: MediaAssetId,
    /// Digest of the exact stored bytes.
    pub sha256: String,
    /// Length of the exact stored bytes.
    pub byte_length: u64,
    /// Content-addressed file name inside an archive (`<sha256>.<ext>`).
    pub archive_file_name: String,
    /// Reported container/codec.
    pub codec: Option<String>,
    /// Sample rate, when probed.
    pub sample_rate: Option<u32>,
    /// Channel count, when probed.
    pub channels: Option<u16>,
    /// Duration in milliseconds, when probed.
    pub duration_ms: Option<u64>,
    /// Fidelity class as stored.
    pub quality_class: String,
    /// Honest, source-relative fidelity label (spec 7.9, 23).
    pub quality_label: String,
    /// Integrity verification state.
    pub verification_state: String,
    /// When the recording acquired this candidate.
    pub obtained_at: Option<Timestamp>,
}

/// Facts about a source association.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFacts {
    /// Source kind (`local_file`, `plaud_cloud_cli`, …).
    pub source_kind: String,
    /// Account or device scope the external id belongs to.
    pub account_scope: String,
    /// Human-readable source name.
    pub display_name: String,
    /// Stable external identifier, when the source exposes one.
    pub external_id: Option<String>,
    /// Import status of the association.
    pub import_status: String,
    /// Remote lifecycle state as last observed.
    pub remote_state: String,
    /// Remote version hint, when exposed.
    pub remote_version: Option<String>,
    /// When the item was last seen at the source.
    pub last_seen_at: Option<Timestamp>,
}

/// Facts about a speaker label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerFacts {
    /// Speaker row identity.
    pub id: String,
    /// Stable cluster label, always present (`Speaker 1`).
    pub label: String,
    /// User-entered name. Present only when the user confirmed it.
    pub name: Option<String>,
    /// Provenance as recorded.
    pub provenance: String,
    /// Whether a human confirmed the identity.
    pub user_confirmed: bool,
    /// Label to render, with its confirmation qualifier.
    pub display: String,
}

/// One word alignment inside a segment (spec 8.5, migration 0200).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordFacts {
    /// Zero-based position inside the segment.
    pub seq_index: u32,
    /// Start offset in milliseconds.
    pub start_ms: u64,
    /// End offset in milliseconds.
    pub end_ms: u64,
    /// The word as normalized by the engine.
    pub text: String,
    /// Engine-scoped confidence, when reported.
    pub confidence: Option<f64>,
    /// Scale the confidence is expressed on.
    pub confidence_scale: Option<String>,
}

/// A non-destructive quality flag on a revision (spec 8.6A, migration 0200).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptAnnotationFacts {
    /// Segment the flag applies to.
    pub segment_index: u32,
    /// Flag kind.
    pub kind: String,
    /// Why the flag was raised.
    pub reason: String,
    /// Start offset, when the flag carries one.
    pub start_ms: Option<u64>,
    /// End offset, when the flag carries one.
    pub end_ms: Option<u64>,
}

/// One transcript segment, as exported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentFacts {
    /// Segment row identity, referenced by evidence anchors.
    pub id: String,
    /// Zero-based position in the revision.
    pub seq_index: u32,
    /// Start offset in milliseconds, when real times exist.
    pub start_ms: Option<u64>,
    /// End offset in milliseconds, when real times exist.
    pub end_ms: Option<u64>,
    /// Speaker attribution, when diarization ran.
    pub speaker_id: Option<String>,
    /// Normalized text.
    pub text: String,
    /// Engine-scoped confidence value, when reported.
    pub confidence: Option<f64>,
    /// Scale the confidence is expressed on.
    pub confidence_scale: Option<String>,
    /// Word alignments, when the engine reported them.
    pub words: Vec<WordFacts>,
}

/// One transcript revision, as exported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptFacts {
    /// Revision identity.
    pub id: TranscriptRevisionId,
    /// Parent revision, when this is a correction or reconciliation.
    pub parent_id: Option<TranscriptRevisionId>,
    /// How the revision came to exist.
    pub origin: String,
    /// Digest of the exact transcribed media bytes.
    pub input_sha256: String,
    /// Producing engine.
    pub engine: String,
    /// Producing model.
    pub model_id: String,
    /// Model version, when reported.
    pub model_version: Option<String>,
    /// Locale, when known.
    pub locale: Option<String>,
    /// Declared alignment origin (`native`, `derived`, `none`).
    pub alignment_origin: String,
    /// Honest alignment label shown next to times (spec 8.6A).
    pub alignment_label: String,
    /// Lifecycle status.
    pub status: String,
    /// Text normalization profile.
    pub normalization_profile: String,
    /// Chunking strategy.
    pub chunking_strategy: String,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Creation instant.
    pub created_at: Timestamp,
    /// Compute backend the engine ran on, when recorded.
    pub engine_backend: Option<String>,
    /// Canonical engine parameters, when recorded.
    pub engine_params: Option<serde_json::Value>,
    /// Non-destructive quality flags, ordered by segment.
    pub annotations: Vec<TranscriptAnnotationFacts>,
    /// Ordered segments.
    pub segments: Vec<SegmentFacts>,
}

impl TranscriptFacts {
    /// Parsed alignment origin.
    #[must_use]
    pub fn alignment(&self) -> AlignmentOrigin {
        match self.alignment_origin.as_str() {
            "native" => AlignmentOrigin::Native,
            "derived" => AlignmentOrigin::Derived,
            _ => AlignmentOrigin::None,
        }
    }

    /// Plain transcript text, one segment per line.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            out.push_str(&segment.text);
            out.push('\n');
        }
        out
    }
}

/// One evidence anchor attached to a generated output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceFacts {
    /// JSON Pointer into the output body the anchor supports.
    pub item_path: String,
    /// Segment the anchor resolves to, when the cited span exists.
    pub segment_id: Option<String>,
    /// Position of that segment in its revision, when resolvable.
    pub segment_index: Option<u32>,
    /// Start offset of that segment, when real times exist.
    pub start_ms: Option<u64>,
    /// The exact quoted passage.
    pub quote: String,
    /// `source_linked`, `human_confirmed` or `unverified`.
    pub verification_status: String,
}

/// One generated output, as exported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputFacts {
    /// Output identity.
    pub id: String,
    /// Output type (`meeting_summary`, `action_items`, …).
    pub output_type: String,
    /// Exact transcript revision the output was generated from.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Template version used.
    pub template_version: String,
    /// Model that produced it.
    pub model_id: String,
    /// Prompt version used.
    pub prompt_version: String,
    /// Output schema version.
    pub schema_version: String,
    /// Lifecycle status, including `stale`.
    pub status: String,
    /// Creation instant.
    pub created_at: Timestamp,
    /// The structured body, verbatim.
    pub body: serde_json::Value,
    /// Anchors, in stable order.
    pub evidence: Vec<EvidenceFacts>,
}

/// Notices every export carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportNotices {
    /// Metadata export is not a backup.
    pub not_a_backup: String,
    /// What a source anchor does and does not prove.
    pub anchors: String,
}

impl Default for ExportNotices {
    fn default() -> Self {
        Self {
            not_a_backup: NOT_A_BACKUP_NOTICE.to_owned(),
            anchors: ANCHOR_NOTICE.to_owned(),
        }
    }
}

/// Everything an export of one recording may contain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingExport {
    /// Versioned schema identifier.
    pub schema_version: String,
    /// Writer stamp.
    pub app: AppStamp,
    /// When the export was produced.
    pub exported_at: Timestamp,
    /// The logical meeting.
    pub recording: RecordingFacts,
    /// Media candidates, oldest first.
    pub media: Vec<MediaFacts>,
    /// Source associations, in creation order.
    pub sources: Vec<SourceFacts>,
    /// Speaker labels for this recording.
    pub speakers: Vec<SpeakerFacts>,
    /// Transcript revisions, oldest first.
    pub transcripts: Vec<TranscriptFacts>,
    /// Generated outputs, in stable order.
    pub outputs: Vec<OutputFacts>,
    /// Revision the text renderings used.
    pub selected_transcript_id: Option<TranscriptRevisionId>,
    /// Honest notices.
    pub notices: ExportNotices,
}

impl RecordingExport {
    /// The revision the text renderings should use.
    #[must_use]
    pub fn selected_transcript(&self) -> Option<&TranscriptFacts> {
        let selected = self.selected_transcript_id?;
        self.transcripts.iter().find(|t| t.id == selected)
    }

    /// Outputs generated from a given revision.
    #[must_use]
    pub fn outputs_for(&self, revision: TranscriptRevisionId) -> Vec<&OutputFacts> {
        self.outputs
            .iter()
            .filter(|output| output.transcript_revision_id == revision)
            .collect()
    }

    /// Render a speaker reference for display.
    #[must_use]
    pub fn speaker_display(&self, speaker_id: &str) -> Option<&str> {
        self.speakers
            .iter()
            .find(|speaker| speaker.id == speaker_id)
            .map(|speaker| speaker.display.as_str())
    }
}

/// Honest, source-relative fidelity label (spec 23 reference notes).
#[must_use]
pub fn quality_label(quality_class: &str) -> &'static str {
    match quality_class {
        "original_device_recording" => "Original device recording",
        "compressed_device_transfer" => {
            "Compressed device transfer (.ASR sidecar; not the original device WAV)"
        }
        "vendor_cloud_export" => "Vendor cloud export",
        "user_supplied_file" => "User-supplied file",
        "local_derivative" => "Locally produced derivative",
        _ => "Fidelity not established",
    }
}

/// Honest alignment label (spec 8.6A).
#[must_use]
pub fn alignment_label(alignment_origin: &str) -> &'static str {
    match alignment_origin {
        "native" => "native",
        "derived" => "derived (approximate)",
        _ => "unavailable",
    }
}

/// Speaker display text that never presents an inferred cluster as identity.
#[must_use]
pub fn speaker_display(label: &str, name: Option<&str>, user_confirmed: bool) -> String {
    match (name, user_confirmed) {
        (Some(name), true) if !name.is_empty() => format!("{name} (confirmed)"),
        _ => format!("{label} (inferred cluster)"),
    }
}

/// Reads the allowlisted projection out of a library.
#[derive(Debug, Clone)]
pub struct BundleLoader {
    db: Database,
}

impl BundleLoader {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Load the export projection for one recording.
    ///
    /// `selected_transcript` defaults to the newest revision.
    pub async fn load(
        &self,
        recording_id: RecordingId,
        selected_transcript: Option<TranscriptRevisionId>,
        exported_at: Timestamp,
    ) -> ExportResult<RecordingExport> {
        let recording = self.recording_facts(recording_id).await?;
        let media = self.media_facts(recording_id).await?;
        let sources = self.source_facts(recording_id).await?;
        let speakers = self.speaker_facts(recording_id).await?;
        let transcripts = self.transcript_facts(recording_id).await?;
        let outputs = self.output_facts(recording_id, &transcripts).await?;

        let selected = selected_transcript.or_else(|| transcripts.last().map(|t| t.id));
        if let Some(selected) = selected {
            if !transcripts.iter().any(|t| t.id == selected) {
                return Err(ExportError::RecordNotFound);
            }
        }

        Ok(RecordingExport {
            schema_version: EXPORT_SCHEMA_VERSION.to_owned(),
            app: AppStamp::default(),
            exported_at,
            recording,
            media,
            sources,
            speakers,
            transcripts,
            outputs,
            selected_transcript_id: selected,
            notices: ExportNotices::default(),
        })
    }

    /// Recording identities in the library, oldest first, excluding tombstoned
    /// recordings unless `include_deleted` is set.
    pub async fn recording_ids(&self, include_deleted: bool) -> ExportResult<Vec<RecordingId>> {
        let sql = if include_deleted {
            "SELECT id FROM recordings ORDER BY created_at, id"
        } else {
            "SELECT id FROM recordings WHERE deleted_at IS NULL ORDER BY created_at, id"
        };
        let rows = sqlx::query(sql)
            .fetch_all(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;
        rows.iter()
            .map(|row| {
                row.get::<String, _>("id")
                    .parse::<RecordingId>()
                    .map_err(|_| ExportError::IntegrityFailed)
            })
            .collect()
    }

    async fn recording_facts(&self, id: RecordingId) -> ExportResult<RecordingFacts> {
        let row = sqlx::query(
            "SELECT id, title, captured_at, duration_ms, preferred_media_asset_id, annotation,
                    created_at, deleted_at
             FROM recordings WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await
        .map_err(PersistenceError::from)?
        .ok_or(ExportError::RecordNotFound)?;

        let tags =
            sqlx::query("SELECT tag FROM recording_tags WHERE recording_id = ?1 ORDER BY tag")
                .bind(id.to_string())
                .fetch_all(self.db.pool())
                .await
                .map_err(PersistenceError::from)?
                .iter()
                .map(|row| row.get::<String, _>("tag"))
                .collect();

        Ok(RecordingFacts {
            id,
            title: row.get("title"),
            captured_at: row
                .get::<Option<i64>, _>("captured_at")
                .map(Timestamp::from_millis),
            duration_ms: row
                .get::<Option<i64>, _>("duration_ms")
                .and_then(|v| u64::try_from(v).ok()),
            created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
            deleted_at: row
                .get::<Option<i64>, _>("deleted_at")
                .map(Timestamp::from_millis),
            tags,
            annotation: row.get("annotation"),
            preferred_media_asset_id: row
                .get::<Option<String>, _>("preferred_media_asset_id")
                .and_then(|raw| raw.parse().ok()),
        })
    }

    async fn media_facts(&self, id: RecordingId) -> ExportResult<Vec<MediaFacts>> {
        let rows = sqlx::query(
            "SELECT a.id, a.sha256, a.byte_length, a.relative_path, a.mime, a.codec,
                    a.sample_rate, a.channels, a.duration_ms, a.quality_class,
                    a.verification_state, rm.obtained_at
             FROM recording_media rm
             JOIN media_assets a ON a.id = rm.media_asset_id
             WHERE rm.recording_id = ?1
             ORDER BY rm.obtained_at, a.id",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        rows.iter()
            .map(|row| {
                let sha: String = row.get("sha256");
                let relative: String = row.get("relative_path");
                let extension = relative
                    .rsplit('.')
                    .next()
                    .filter(|ext| !ext.is_empty() && *ext != relative)
                    .unwrap_or("bin")
                    .to_owned();
                let quality_class: String = row.get("quality_class");
                Ok(MediaFacts {
                    id: row
                        .get::<String, _>("id")
                        .parse()
                        .map_err(|_| ExportError::IntegrityFailed)?,
                    archive_file_name: format!("{sha}.{extension}"),
                    sha256: sha,
                    byte_length: u64::try_from(row.get::<i64, _>("byte_length")).unwrap_or(0),
                    codec: row.get("codec"),
                    sample_rate: row
                        .get::<Option<i64>, _>("sample_rate")
                        .and_then(|v| u32::try_from(v).ok()),
                    channels: row
                        .get::<Option<i64>, _>("channels")
                        .and_then(|v| u16::try_from(v).ok()),
                    duration_ms: row
                        .get::<Option<i64>, _>("duration_ms")
                        .and_then(|v| u64::try_from(v).ok()),
                    quality_label: quality_label(&quality_class).to_owned(),
                    quality_class,
                    verification_state: row.get("verification_state"),
                    obtained_at: row
                        .get::<Option<i64>, _>("obtained_at")
                        .map(Timestamp::from_millis),
                })
            })
            .collect()
    }

    async fn source_facts(&self, id: RecordingId) -> ExportResult<Vec<SourceFacts>> {
        let rows = sqlx::query(
            "SELECT s.source_kind, s.account_scope, s.display_name, i.external_id,
                    i.import_status, i.remote_state, i.remote_version, i.last_seen_at
             FROM source_items i
             JOIN sources s ON s.id = i.source_id
             WHERE i.recording_id = ?1
             ORDER BY i.created_at, i.id",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        Ok(rows
            .iter()
            .map(|row| SourceFacts {
                source_kind: row.get("source_kind"),
                account_scope: row.get("account_scope"),
                display_name: row.get("display_name"),
                external_id: row.get("external_id"),
                import_status: row.get("import_status"),
                remote_state: row.get("remote_state"),
                remote_version: row.get("remote_version"),
                last_seen_at: row
                    .get::<Option<i64>, _>("last_seen_at")
                    .map(Timestamp::from_millis),
            })
            .collect())
    }

    async fn speaker_facts(&self, id: RecordingId) -> ExportResult<Vec<SpeakerFacts>> {
        let rows = sqlx::query(
            "SELECT id, label, name, provenance, user_confirmed
             FROM speakers WHERE recording_id = ?1 ORDER BY label, id",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        Ok(rows
            .iter()
            .map(|row| {
                let label: String = row.get("label");
                let name: Option<String> = row.get("name");
                let confirmed = row.get::<i64, _>("user_confirmed") == 1;
                SpeakerFacts {
                    id: row.get("id"),
                    display: speaker_display(&label, name.as_deref(), confirmed),
                    label,
                    // An unconfirmed name is a guess; it never leaves as identity.
                    name: if confirmed { name } else { None },
                    provenance: row.get("provenance"),
                    user_confirmed: confirmed,
                }
            })
            .collect())
    }

    async fn transcript_facts(&self, id: RecordingId) -> ExportResult<Vec<TranscriptFacts>> {
        let rows = sqlx::query(
            "SELECT id, parent_id, origin, input_sha256, engine, model_id, model_version,
                    locale, alignment_origin, status, normalization_profile, chunking_strategy,
                    duration_ms, created_at, engine_backend, engine_params_json
             FROM transcript_revisions WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let revision_id: TranscriptRevisionId = row
                .get::<String, _>("id")
                .parse()
                .map_err(|_| ExportError::IntegrityFailed)?;
            let alignment_origin: String = row.get("alignment_origin");
            out.push(TranscriptFacts {
                id: revision_id,
                parent_id: row
                    .get::<Option<String>, _>("parent_id")
                    .and_then(|raw| raw.parse().ok()),
                origin: row.get("origin"),
                input_sha256: row.get("input_sha256"),
                engine: row.get("engine"),
                model_id: row.get("model_id"),
                model_version: row.get("model_version"),
                locale: row.get("locale"),
                alignment_label: alignment_label(&alignment_origin).to_owned(),
                alignment_origin,
                status: row.get("status"),
                normalization_profile: row.get("normalization_profile"),
                chunking_strategy: row.get("chunking_strategy"),
                duration_ms: row
                    .get::<Option<i64>, _>("duration_ms")
                    .and_then(|v| u64::try_from(v).ok()),
                created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
                engine_backend: row.get("engine_backend"),
                engine_params: row
                    .get::<Option<String>, _>("engine_params_json")
                    .and_then(|raw| serde_json::from_str(&raw).ok()),
                annotations: self.annotation_facts(revision_id).await?,
                segments: self.segment_facts(revision_id).await?,
            });
        }
        Ok(out)
    }

    async fn annotation_facts(
        &self,
        id: TranscriptRevisionId,
    ) -> ExportResult<Vec<TranscriptAnnotationFacts>> {
        let rows = sqlx::query(
            "SELECT segment_index, kind, reason, start_ms, end_ms
             FROM transcript_annotations WHERE transcript_revision_id = ?1
             ORDER BY segment_index, kind",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        Ok(rows
            .iter()
            .map(|row| TranscriptAnnotationFacts {
                segment_index: u32::try_from(row.get::<i64, _>("segment_index")).unwrap_or(0),
                kind: row.get("kind"),
                reason: row.get("reason"),
                start_ms: row
                    .get::<Option<i64>, _>("start_ms")
                    .and_then(|v| u64::try_from(v).ok()),
                end_ms: row
                    .get::<Option<i64>, _>("end_ms")
                    .and_then(|v| u64::try_from(v).ok()),
            })
            .collect())
    }

    async fn segment_facts(&self, id: TranscriptRevisionId) -> ExportResult<Vec<SegmentFacts>> {
        let rows = sqlx::query(
            "SELECT id, seq_index, start_ms, end_ms, speaker_id, text, confidence, confidence_scale
             FROM transcript_segments WHERE transcript_revision_id = ?1 ORDER BY seq_index",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let segment_id: String = row.get("id");
            out.push(SegmentFacts {
                seq_index: u32::try_from(row.get::<i64, _>("seq_index")).unwrap_or(0),
                start_ms: row
                    .get::<Option<i64>, _>("start_ms")
                    .and_then(|v| u64::try_from(v).ok()),
                end_ms: row
                    .get::<Option<i64>, _>("end_ms")
                    .and_then(|v| u64::try_from(v).ok()),
                speaker_id: row.get("speaker_id"),
                text: row.get("text"),
                confidence: row.get("confidence"),
                confidence_scale: row.get("confidence_scale"),
                words: self.word_facts(&segment_id).await?,
                id: segment_id,
            });
        }
        Ok(out)
    }

    async fn word_facts(&self, segment_id: &str) -> ExportResult<Vec<WordFacts>> {
        let rows = sqlx::query(
            "SELECT seq_index, start_ms, end_ms, text, confidence, confidence_scale
             FROM transcript_words WHERE segment_id = ?1 ORDER BY seq_index",
        )
        .bind(segment_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        Ok(rows
            .iter()
            .map(|row| WordFacts {
                seq_index: u32::try_from(row.get::<i64, _>("seq_index")).unwrap_or(0),
                start_ms: u64::try_from(row.get::<i64, _>("start_ms")).unwrap_or(0),
                end_ms: u64::try_from(row.get::<i64, _>("end_ms")).unwrap_or(0),
                text: row.get("text"),
                confidence: row.get("confidence"),
                confidence_scale: row.get("confidence_scale"),
            })
            .collect())
    }

    async fn output_facts(
        &self,
        id: RecordingId,
        transcripts: &[TranscriptFacts],
    ) -> ExportResult<Vec<OutputFacts>> {
        let rows = sqlx::query(
            "SELECT id, output_type, transcript_revision_id, template_version, model_id,
                    prompt_version, schema_version, body_json, status, created_at
             FROM ai_outputs WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let output_id: String = row.get("id");
            let revision: TranscriptRevisionId = row
                .get::<String, _>("transcript_revision_id")
                .parse()
                .map_err(|_| ExportError::IntegrityFailed)?;
            let body_json: String = row.get("body_json");
            let evidence_rows = sqlx::query(
                "SELECT e.item_path AS item_path, e.cited_revision_id AS cited_revision_id,
                        e.seq_index AS seq_index, e.quote AS quote, c.state AS verification_status
                 FROM output_evidence e
                 JOIN output_claims c ON c.id = e.claim_id
                 WHERE e.output_id = ?1
                 ORDER BY e.item_path, e.claim_id, e.position",
            )
            .bind(&output_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;

            let evidence = evidence_rows
                .iter()
                .map(|evidence| {
                    let cited_revision: String = evidence.get("cited_revision_id");
                    let seq_index =
                        u32::try_from(evidence.get::<i64, _>("seq_index")).unwrap_or(u32::MAX);
                    let located = transcripts
                        .iter()
                        .filter(|t| t.id.to_string() == cited_revision)
                        .flat_map(|t| t.segments.iter())
                        .find(|segment| segment.seq_index == seq_index);
                    EvidenceFacts {
                        item_path: evidence.get("item_path"),
                        segment_index: located.map(|segment| segment.seq_index),
                        start_ms: located.and_then(|segment| segment.start_ms),
                        segment_id: located.map(|segment| segment.id.clone()),
                        quote: evidence.get("quote"),
                        verification_status: evidence.get("verification_status"),
                    }
                })
                .collect();

            out.push(OutputFacts {
                id: output_id,
                output_type: row.get("output_type"),
                transcript_revision_id: revision,
                template_version: row.get("template_version"),
                model_id: row.get("model_id"),
                prompt_version: row.get("prompt_version"),
                schema_version: row.get("schema_version"),
                status: row.get("status"),
                created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
                body: serde_json::from_str(&body_json)
                    .unwrap_or(serde_json::Value::String(body_json)),
                evidence,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconfirmed_name_is_never_rendered_as_identity() {
        assert_eq!(
            speaker_display("Speaker 1", Some("Dana"), false),
            "Speaker 1 (inferred cluster)"
        );
        assert_eq!(
            speaker_display("Speaker 1", Some("Dana"), true),
            "Dana (confirmed)"
        );
        assert_eq!(
            speaker_display("Speaker 2", None, false),
            "Speaker 2 (inferred cluster)"
        );
    }

    #[test]
    fn a_compressed_transfer_is_never_labelled_an_original_wav() {
        let label = quality_label("compressed_device_transfer");
        assert!(label.contains("not the original device WAV"));
        assert_eq!(quality_label("something_new"), "Fidelity not established");
    }

    #[test]
    fn alignment_labels_are_the_three_honest_values() {
        assert_eq!(alignment_label("native"), "native");
        assert_eq!(alignment_label("derived"), "derived (approximate)");
        assert_eq!(alignment_label("none"), "unavailable");
        assert_eq!(alignment_label("invented"), "unavailable");
    }

    #[test]
    fn the_not_a_backup_notice_names_the_archive_workflow() {
        assert!(NOT_A_BACKUP_NOTICE.contains("not a backup"));
        assert!(NOT_A_BACKUP_NOTICE.contains(".recording-archive"));
    }
}
