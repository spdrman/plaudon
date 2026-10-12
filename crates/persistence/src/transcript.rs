//! Transcript revision persistence (spec §8.5, §8.6 — task F2.4).
//!
//! A revision is immutable. Re-transcribing with another model writes a new
//! row that references the same recording and input digest, so prior
//! revisions — including human edits — stay retrievable and nothing is
//! overwritten (§8.6). Everything an engine recorded about *how* a result was
//! produced travels with it: parameters, chunking strategy, normalization
//! profile and the compute backend that actually ran, so a CPU fallback is
//! visible in the data rather than inferred from a log.
//!
//! Two invariants are enforced below the application, by the schema:
//!
//! * a segment of a revision whose alignment origin is `none` carries no
//!   times at all (§8.6A), and
//! * deleting a recording's revisions removes their segments, words and
//!   annotations with them, leaving no dangling rows (E2E #16).

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::transcript::AnnotationKind;
use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{AlignmentOrigin, RevisionOrigin, RevisionStatus};
use sqlx::Row;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// Raw provider response retained only when the user enables detailed
/// provenance. The repository stores ciphertext; it never encrypts, and it
/// never accepts plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedProvenance {
    /// Ciphertext of the redacted provider response.
    pub ciphertext: Vec<u8>,
    /// Nonce used by the AEAD.
    pub nonce: Vec<u8>,
    /// Scheme label, e.g. `xchacha20poly1305`.
    pub scheme: String,
}

/// What the caller is saving, beyond the draft itself.
#[derive(Debug, Clone, PartialEq)]
pub struct RevisionContext {
    /// Recording the revision belongs to.
    pub recording_id: RecordingId,
    /// Previous revision this one supersedes or corrects, when any.
    pub parent_id: Option<TranscriptRevisionId>,
    /// How this revision came to exist.
    pub origin: RevisionOrigin,
    /// Quality status.
    pub status: RevisionStatus,
    /// When it was produced.
    pub created_at: Timestamp,
    /// Off by default (§8.5): detailed provenance is opt-in.
    pub provenance: Option<EncryptedProvenance>,
}

impl RevisionContext {
    /// A machine revision with minimal provenance, which is the default.
    #[must_use]
    pub fn machine(recording_id: RecordingId, created_at: Timestamp) -> Self {
        Self {
            recording_id,
            parent_id: None,
            origin: RevisionOrigin::Machine,
            status: RevisionStatus::Complete,
            created_at,
            provenance: None,
        }
    }
}

/// A persisted revision's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRevision {
    /// Identity of the stored revision.
    pub id: TranscriptRevisionId,
    /// Recording it belongs to.
    pub recording_id: RecordingId,
    /// Revision it was derived from.
    pub parent_id: Option<TranscriptRevisionId>,
    /// Digest of the source bytes that were transcribed.
    pub input_sha256: Sha256Digest,
    /// Engine identifier.
    pub engine: String,
    /// Model identifier.
    pub model_id: String,
    /// Model version, when the engine reported one.
    pub model_version: Option<String>,
    /// Locale, when known.
    pub locale: Option<String>,
    /// Alignment origin of every segment in the revision.
    pub alignment_origin: AlignmentOrigin,
    /// Canonicalisation label.
    pub normalization_profile: String,
    /// Chunking strategy label.
    pub chunking_strategy: String,
    /// Compute backend that actually ran (F2.5).
    pub engine_backend: Option<String>,
    /// Canonical JSON of the engine parameters.
    pub engine_params_json: Option<String>,
    /// Duration the engine observed.
    pub duration_ms: Option<u64>,
    /// Whether detailed provenance was retained.
    pub has_provenance: bool,
}

/// A persisted segment.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSegment {
    /// Position in the revision.
    pub seq_index: u32,
    /// Start offset in milliseconds, absent for a text-only revision.
    pub start_ms: Option<u64>,
    /// End offset in milliseconds, absent for a text-only revision.
    pub end_ms: Option<u64>,
    /// Segment text exactly as the engine produced it.
    pub text: String,
    /// Engine-scoped confidence value.
    pub confidence: Option<f64>,
    /// Scale the confidence is expressed on; never compared across engines.
    pub confidence_scale: Option<String>,
    /// Word alignment, when the engine reported it.
    pub words: Vec<StoredWord>,
}

/// A persisted word alignment.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredWord {
    /// Position within the segment.
    pub seq_index: u32,
    /// Start offset in milliseconds.
    pub start_ms: u64,
    /// End offset in milliseconds.
    pub end_ms: u64,
    /// The word.
    pub text: String,
    /// Engine-scoped confidence.
    pub confidence: Option<f64>,
}

/// A persisted non-destructive annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAnnotation {
    /// Segment the annotation refers to.
    pub segment_index: u32,
    /// What was observed.
    pub kind: AnnotationKind,
    /// Human-readable, non-secret explanation.
    pub reason: String,
    /// Start of the annotated region.
    pub start_ms: Option<u64>,
    /// End of the annotated region.
    pub end_ms: Option<u64>,
}

/// Reads and writes transcript revisions.
#[derive(Debug, Clone)]
pub struct TranscriptRepository {
    database: Database,
}

fn alignment_label(origin: AlignmentOrigin) -> &'static str {
    match origin {
        AlignmentOrigin::Native => "native",
        AlignmentOrigin::Derived => "derived",
        AlignmentOrigin::None => "none",
    }
}

fn parse_alignment(value: &str) -> PersistenceResult<AlignmentOrigin> {
    match value {
        "native" => Ok(AlignmentOrigin::Native),
        "derived" => Ok(AlignmentOrigin::Derived),
        "none" => Ok(AlignmentOrigin::None),
        other => Err(PersistenceError::Migration(format!(
            "unrecognised alignment origin `{other}` in the database"
        ))),
    }
}

const fn origin_label(origin: RevisionOrigin) -> &'static str {
    match origin {
        RevisionOrigin::Machine => "machine",
        RevisionOrigin::HumanEdit => "human_edit",
        RevisionOrigin::DerivedAlignment => "derived_alignment",
        RevisionOrigin::OverlapReconciliation => "overlap_reconciliation",
    }
}

const fn status_label(status: RevisionStatus) -> &'static str {
    match status {
        RevisionStatus::Complete => "complete",
        RevisionStatus::NeedsReview => "needs_review",
        RevisionStatus::Partial => "partial",
    }
}

fn storage(error: sqlx::Error) -> PersistenceError {
    PersistenceError::StorageUnavailable(Box::new(error))
}

impl TranscriptRepository {
    /// Build a repository over an open database.
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }

    /// Persist a draft as a new immutable revision.
    ///
    /// Writes the revision, its segments, their word alignments and the
    /// annotations in one transaction, so a crash can never leave a revision
    /// without its segments.
    pub async fn save_draft(
        &self,
        draft: &TranscriptDraft,
        context: &RevisionContext,
    ) -> PersistenceResult<TranscriptRevisionId> {
        let revision_id = TranscriptRevisionId::new();
        let params_json = serde_json::to_string(&draft.engine_params)
            .map_err(|error| PersistenceError::Migration(error.to_string()))?;
        let normalization = draft
            .normalization_profile
            .as_ref()
            .map_or_else(|| "none".to_string(), super::transcript::profile_label);

        let mut transaction = self.database.begin_write().await?;

        sqlx::query(
            "INSERT INTO transcript_revisions (
                 id, recording_id, parent_id, origin, input_sha256, engine, model_id,
                 model_version, locale, alignment_origin, status, normalization_profile,
                 chunking_strategy, duration_ms, created_at, engine_backend,
                 engine_params_json, raw_provenance_cipher, raw_provenance_nonce,
                 raw_provenance_scheme)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20)",
        )
        .bind(revision_id.to_string())
        .bind(context.recording_id.to_string())
        .bind(context.parent_id.map(|id| id.to_string()))
        .bind(origin_label(context.origin))
        .bind(draft.input_sha256.to_hex())
        .bind(draft.engine_id.as_str())
        .bind(draft.model.model_id.as_str())
        .bind(draft.model.model_version.as_deref())
        .bind(
            draft
                .locale
                .as_ref()
                .map(plaudon_domain::locale::LanguageTag::as_str),
        )
        .bind(alignment_label(draft.alignment_origin))
        .bind(status_label(context.status))
        .bind(normalization)
        .bind(draft.chunking_strategy.label())
        .bind(
            draft
                .duration_ms
                .map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
        )
        .bind(context.created_at.as_millis())
        .bind(draft.backend.as_deref())
        .bind(params_json)
        .bind(context.provenance.as_ref().map(|p| p.ciphertext.clone()))
        .bind(context.provenance.as_ref().map(|p| p.nonce.clone()))
        .bind(context.provenance.as_ref().map(|p| p.scheme.clone()))
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;

        for segment in &draft.segments {
            let segment_id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO transcript_segments (
                     id, transcript_revision_id, seq_index, start_ms, end_ms, speaker_id,
                     text, confidence, confidence_scale, alignment_origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9)",
            )
            .bind(&segment_id)
            .bind(revision_id.to_string())
            .bind(i64::from(segment.seq_index))
            .bind(
                segment
                    .start
                    .map(|time| i64::try_from(time.as_millis()).unwrap_or(i64::MAX)),
            )
            .bind(
                segment
                    .end
                    .map(|time| i64::try_from(time.as_millis()).unwrap_or(i64::MAX)),
            )
            .bind(&segment.text)
            .bind(segment.confidence.as_ref().map(|c| c.value))
            .bind(
                segment
                    .confidence
                    .as_ref()
                    .map(|c| confidence_scale_label(c)),
            )
            .bind(alignment_label(draft.alignment_origin))
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;

            for (index, word) in segment.words.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO transcript_words (
                         id, segment_id, seq_index, start_ms, end_ms, text, confidence,
                         confidence_scale)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .bind(Uuid::new_v4().to_string())
                .bind(&segment_id)
                .bind(i64::try_from(index).unwrap_or(i64::MAX))
                .bind(i64::try_from(word.start.as_millis()).unwrap_or(i64::MAX))
                .bind(i64::try_from(word.end.as_millis()).unwrap_or(i64::MAX))
                .bind(&word.text)
                .bind(word.confidence.as_ref().map(|c| c.value))
                .bind(word.confidence.as_ref().map(|c| confidence_scale_label(c)))
                .execute(&mut *transaction)
                .await
                .map_err(storage)?;
            }
        }

        for annotation in &draft.annotations {
            sqlx::query(
                "INSERT INTO transcript_annotations (
                     id, transcript_revision_id, segment_index, kind, reason, start_ms, end_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(revision_id.to_string())
            .bind(i64::try_from(annotation.segment_index).unwrap_or(i64::MAX))
            .bind(annotation.kind.as_str())
            .bind(&annotation.reason)
            .bind(
                annotation
                    .start
                    .map(|t| i64::try_from(t.as_millis()).unwrap_or(i64::MAX)),
            )
            .bind(
                annotation
                    .end
                    .map(|t| i64::try_from(t.as_millis()).unwrap_or(i64::MAX)),
            )
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
        }

        transaction.commit().await.map_err(storage)?;
        Ok(revision_id)
    }

    /// Load a revision's metadata.
    pub async fn revision(&self, id: TranscriptRevisionId) -> PersistenceResult<StoredRevision> {
        let row = sqlx::query(
            "SELECT id, recording_id, parent_id, input_sha256, engine, model_id, model_version,
                    locale, alignment_origin, normalization_profile, chunking_strategy,
                    duration_ms, engine_backend, engine_params_json,
                    raw_provenance_cipher IS NOT NULL AS has_provenance
             FROM transcript_revisions WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.database.pool())
        .await
        .map_err(storage)?
        .ok_or(PersistenceError::RecordNotFound)?;

        let parse_id = |value: String| -> PersistenceResult<TranscriptRevisionId> {
            value
                .parse()
                .map_err(|_| PersistenceError::Migration("unparsable revision id".into()))
        };
        let recording_id: RecordingId = row
            .get::<String, _>("recording_id")
            .parse()
            .map_err(|_| PersistenceError::Migration("unparsable recording id".into()))?;
        let parent_id = match row.get::<Option<String>, _>("parent_id") {
            Some(value) => Some(parse_id(value)?),
            None => None,
        };
        let input_sha256: Sha256Digest = row
            .get::<String, _>("input_sha256")
            .parse()
            .map_err(|_| PersistenceError::IntegrityFailed)?;

        Ok(StoredRevision {
            id,
            recording_id,
            parent_id,
            input_sha256,
            engine: row.get("engine"),
            model_id: row.get("model_id"),
            model_version: row.get("model_version"),
            locale: row.get("locale"),
            alignment_origin: parse_alignment(&row.get::<String, _>("alignment_origin"))?,
            normalization_profile: row.get("normalization_profile"),
            chunking_strategy: row.get("chunking_strategy"),
            engine_backend: row.get("engine_backend"),
            engine_params_json: row.get("engine_params_json"),
            duration_ms: row
                .get::<Option<i64>, _>("duration_ms")
                .map(|value| u64::try_from(value).unwrap_or(0)),
            has_provenance: row.get::<i64, _>("has_provenance") != 0,
        })
    }

    /// Load a revision's segments, in order, with their word alignments.
    pub async fn segments(
        &self,
        id: TranscriptRevisionId,
    ) -> PersistenceResult<Vec<StoredSegment>> {
        let rows = sqlx::query(
            "SELECT id, seq_index, start_ms, end_ms, text, confidence, confidence_scale
             FROM transcript_segments
             WHERE transcript_revision_id = ?1
             ORDER BY seq_index",
        )
        .bind(id.to_string())
        .fetch_all(self.database.pool())
        .await
        .map_err(storage)?;

        let mut segments = Vec::with_capacity(rows.len());
        for row in rows {
            let segment_id: String = row.get("id");
            let words = sqlx::query(
                "SELECT seq_index, start_ms, end_ms, text, confidence
                 FROM transcript_words WHERE segment_id = ?1 ORDER BY seq_index",
            )
            .bind(&segment_id)
            .fetch_all(self.database.pool())
            .await
            .map_err(storage)?
            .into_iter()
            .map(|word| StoredWord {
                seq_index: u32::try_from(word.get::<i64, _>("seq_index")).unwrap_or(0),
                start_ms: u64::try_from(word.get::<i64, _>("start_ms")).unwrap_or(0),
                end_ms: u64::try_from(word.get::<i64, _>("end_ms")).unwrap_or(0),
                text: word.get("text"),
                confidence: word.get("confidence"),
            })
            .collect();

            segments.push(StoredSegment {
                seq_index: u32::try_from(row.get::<i64, _>("seq_index")).unwrap_or(0),
                start_ms: row
                    .get::<Option<i64>, _>("start_ms")
                    .map(|value| u64::try_from(value).unwrap_or(0)),
                end_ms: row
                    .get::<Option<i64>, _>("end_ms")
                    .map(|value| u64::try_from(value).unwrap_or(0)),
                text: row.get("text"),
                confidence: row.get("confidence"),
                confidence_scale: row.get("confidence_scale"),
                words,
            });
        }
        Ok(segments)
    }

    /// Load a revision's annotations.
    pub async fn annotations(
        &self,
        id: TranscriptRevisionId,
    ) -> PersistenceResult<Vec<StoredAnnotation>> {
        let rows = sqlx::query(
            "SELECT segment_index, kind, reason, start_ms, end_ms
             FROM transcript_annotations
             WHERE transcript_revision_id = ?1
             ORDER BY segment_index, kind",
        )
        .bind(id.to_string())
        .fetch_all(self.database.pool())
        .await
        .map_err(storage)?;

        rows.into_iter()
            .map(|row| {
                let kind_text: String = row.get("kind");
                let kind = AnnotationKind::parse(&kind_text).ok_or_else(|| {
                    PersistenceError::Migration(format!("unrecognised annotation `{kind_text}`"))
                })?;
                Ok(StoredAnnotation {
                    segment_index: u32::try_from(row.get::<i64, _>("segment_index")).unwrap_or(0),
                    kind,
                    reason: row.get("reason"),
                    start_ms: row
                        .get::<Option<i64>, _>("start_ms")
                        .map(|value| u64::try_from(value).unwrap_or(0)),
                    end_ms: row
                        .get::<Option<i64>, _>("end_ms")
                        .map(|value| u64::try_from(value).unwrap_or(0)),
                })
            })
            .collect()
    }

    /// Every revision for a recording, oldest first.
    pub async fn revisions_for(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<TranscriptRevisionId>> {
        let rows = sqlx::query(
            "SELECT id FROM transcript_revisions WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.database.pool())
        .await
        .map_err(storage)?;
        rows.into_iter()
            .map(|row| {
                row.get::<String, _>("id")
                    .parse()
                    .map_err(|_| PersistenceError::Migration("unparsable revision id".into()))
            })
            .collect()
    }

    /// Delete every revision of a recording, with its segments, words and
    /// annotations. Leaves no dangling rows (E2E #16).
    pub async fn delete_revisions_for(&self, recording_id: RecordingId) -> PersistenceResult<u64> {
        let ids = self.revisions_for(recording_id).await?;
        let mut transaction = self.database.begin_write().await?;
        // Children first: `transcript_revisions.parent_id` is ON DELETE
        // RESTRICT, so the lineage is removed newest-first.
        for id in ids.iter().rev() {
            sqlx::query("DELETE FROM transcript_annotations WHERE transcript_revision_id = ?1")
                .bind(id.to_string())
                .execute(&mut *transaction)
                .await
                .map_err(storage)?;
            sqlx::query(
                "DELETE FROM transcript_words WHERE segment_id IN
                 (SELECT id FROM transcript_segments WHERE transcript_revision_id = ?1)",
            )
            .bind(id.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            sqlx::query("DELETE FROM transcript_segments WHERE transcript_revision_id = ?1")
                .bind(id.to_string())
                .execute(&mut *transaction)
                .await
                .map_err(storage)?;
            sqlx::query("DELETE FROM transcript_revisions WHERE id = ?1")
                .bind(id.to_string())
                .execute(&mut *transaction)
                .await
                .map_err(storage)?;
        }
        transaction.commit().await.map_err(storage)?;
        Ok(ids.len() as u64)
    }
}

fn confidence_scale_label(confidence: &plaudon_domain::transcript::Confidence) -> &'static str {
    use plaudon_domain::transcript::ConfidenceScale;
    match confidence.scale {
        ConfidenceScale::Probability => "probability",
        ConfidenceScale::AverageLogProbability => "average_log_probability",
        ConfidenceScale::VendorDefined => "vendor_defined",
    }
}

pub(crate) fn profile_label(
    profile: &plaudon_ai_contracts::transcript::NormalizationProfile,
) -> String {
    profile.label()
}

/// Segment helper used by callers that want the draft shape back.
#[must_use]
pub fn segment_text(segments: &[TranscriptSegmentDraft]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
