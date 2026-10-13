//! Reading the archive documents out of a live library (spec 13.6).
//!
//! Every query here names its columns. That is the allowlist: a column added
//! to SQLite later does not appear in an archive until it is added to a query
//! *and* to the coverage map in `tests/integration`, which fails until the
//! decision is recorded. No query in this module touches
//! `device_credential_refs`, `inference_attempts`, `consent_grants`,
//! `app_preferences`, `jobs`, `job_steps`, `import_journal` or the event
//! tables; those hold credential handles, routing snapshots and transient
//! state that spec 13.6 excludes.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use plaudon_persistence::voice_profiles::MAX_RETENTION_MONTHS;
use plaudon_persistence::{Database, PersistenceError};
use sqlx::Row;

use super::documents::{
    AnnotationDocument, ClaimRow, EvidenceRow, MediaAssetRow, MediaRevisionRow, OutputDocument,
    OutputRow, PersonDocument, RecordDocument, RecordingMediaRow, RecordingRow, SegmentRow,
    SourceDocument, SourceItemRow, SpeakerRow, ToolDocument, TranscriptAnnotationRow,
    TranscriptDocument, TranscriptRow, WordRow, ANNOTATION_SCHEMA, OUTPUT_SCHEMA, PERSON_SCHEMA,
    RECORD_SCHEMA, SOURCE_SCHEMA, TOOL_SCHEMA, TRANSCRIPT_SCHEMA,
};
use super::voiceprofiles::{EnrollmentRow, ExemplarRow, VoiceProfileDocument, VOICEPROFILE_SCHEMA};
use crate::error::{ExportError, ExportResult};

/// Reads archive documents from a library database.
#[derive(Debug, Clone)]
pub struct LibrarySnapshot {
    db: Database,
}

impl LibrarySnapshot {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Recording identities, oldest first.
    ///
    /// Tombstoned recordings are not archived: a deletion the user performed
    /// must not come back through a backup taken afterwards (spec 7.10).
    pub async fn recording_ids(&self) -> ExportResult<Vec<String>> {
        let rows = sqlx::query(
            "SELECT id FROM recordings WHERE deleted_at IS NULL ORDER BY created_at, id",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;
        Ok(rows.iter().map(|row| row.get::<String, _>("id")).collect())
    }

    /// Structural facts for one recording.
    pub async fn record_document(&self, recording_id: &str) -> ExportResult<RecordDocument> {
        let row = sqlx::query(
            "SELECT id, captured_at, duration_ms, preferred_media_asset_id, created_at, deleted_at
             FROM recordings WHERE id = ?1",
        )
        .bind(recording_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(PersistenceError::from)?
        .ok_or(ExportError::RecordNotFound)?;

        let recording = RecordingRow {
            id: row.get("id"),
            captured_at: row.get("captured_at"),
            duration_ms: row
                .get::<Option<i64>, _>("duration_ms")
                .and_then(|v| u64::try_from(v).ok()),
            preferred_media_asset_id: row.get("preferred_media_asset_id"),
            created_at: row.get("created_at"),
            deleted_at: row.get("deleted_at"),
        };

        let media_rows = sqlx::query(
            "SELECT a.id, a.sha256, a.byte_length, a.relative_path, a.mime, a.codec,
                    a.sample_rate, a.channels, a.duration_ms, a.quality_class, a.created_at,
                    rm.quality_class AS link_quality, rm.obtained_at
             FROM recording_media rm
             JOIN media_assets a ON a.id = rm.media_asset_id
             WHERE rm.recording_id = ?1
             ORDER BY rm.obtained_at, a.id",
        )
        .bind(recording_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut media_assets = Vec::with_capacity(media_rows.len());
        let mut recording_media = Vec::with_capacity(media_rows.len());
        for row in &media_rows {
            let relative: String = row.get("relative_path");
            media_assets.push(MediaAssetRow {
                id: row.get("id"),
                sha256: row.get("sha256"),
                byte_length: u64::try_from(row.get::<i64, _>("byte_length")).unwrap_or(0),
                extension: extension_of(&relative),
                mime: row.get("mime"),
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
                quality_class: row.get("quality_class"),
                created_at: row.get("created_at"),
            });
            recording_media.push(RecordingMediaRow {
                media_asset_id: row.get("id"),
                quality_class: row.get("link_quality"),
                obtained_at: row.get("obtained_at"),
            });
        }

        let revision_rows = sqlx::query(
            "SELECT id, media_asset_id, source_item_id, source_version, predecessor_id, obtained_at
             FROM media_revisions WHERE recording_id = ?1 ORDER BY obtained_at, id",
        )
        .bind(recording_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;
        let media_revisions = revision_rows
            .iter()
            .map(|row| MediaRevisionRow {
                id: row.get("id"),
                media_asset_id: row.get("media_asset_id"),
                source_item_id: row.get("source_item_id"),
                source_version: row.get("source_version"),
                predecessor_id: row.get("predecessor_id"),
                obtained_at: row.get("obtained_at"),
            })
            .collect();

        let item_rows = sqlx::query(
            "SELECT id, source_id, external_id, import_status, last_seen_at, remote_version,
                    remote_state, tombstone_reason, created_at
             FROM source_items WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;
        let source_items = item_rows
            .iter()
            .map(|row| SourceItemRow {
                id: row.get("id"),
                source_id: row.get("source_id"),
                external_id: row.get("external_id"),
                import_status: row.get("import_status"),
                last_seen_at: row.get("last_seen_at"),
                remote_version: row.get("remote_version"),
                remote_state: row.get("remote_state"),
                tombstone_reason: row.get("tombstone_reason"),
                created_at: row.get("created_at"),
            })
            .collect();

        Ok(RecordDocument {
            schema_version: RECORD_SCHEMA.to_owned(),
            recording,
            media_assets,
            recording_media,
            media_revisions,
            source_items,
        })
    }

    /// Everything the user typed for one recording.
    pub async fn annotation_document(
        &self,
        recording_id: &str,
    ) -> ExportResult<AnnotationDocument> {
        let row = sqlx::query("SELECT title, annotation FROM recordings WHERE id = ?1")
            .bind(recording_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(PersistenceError::from)?
            .ok_or(ExportError::RecordNotFound)?;

        let tags =
            sqlx::query("SELECT tag FROM recording_tags WHERE recording_id = ?1 ORDER BY tag")
                .bind(recording_id)
                .fetch_all(self.db.pool())
                .await
                .map_err(PersistenceError::from)?
                .iter()
                .map(|row| row.get::<String, _>("tag"))
                .collect();

        let speakers = sqlx::query(
            "SELECT id, label, name, provenance, user_confirmed, person_id
             FROM speakers WHERE recording_id = ?1 ORDER BY label, id",
        )
        .bind(recording_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?
        .iter()
        .map(|row| SpeakerRow {
            id: row.get("id"),
            label: row.get("label"),
            name: row.get("name"),
            provenance: row.get("provenance"),
            user_confirmed: row.get::<i64, _>("user_confirmed") == 1,
            person_id: row.get("person_id"),
        })
        .collect();

        Ok(AnnotationDocument {
            schema_version: ANNOTATION_SCHEMA.to_owned(),
            recording_id: recording_id.to_owned(),
            title: row.get("title"),
            annotation: row.get("annotation"),
            tags,
            speakers,
        })
    }

    /// Transcript revisions for one recording, oldest first.
    pub async fn transcript_documents(
        &self,
        recording_id: &str,
    ) -> ExportResult<Vec<TranscriptDocument>> {
        let rows = sqlx::query(
            "SELECT id, parent_id, origin, input_sha256, engine, model_id, model_version, locale,
                    alignment_origin, status, normalization_profile, chunking_strategy,
                    duration_ms, created_at, engine_backend, engine_params_json, source_vendor
             FROM transcript_revisions WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let revision_id: String = row.get("id");
            let revision = TranscriptRow {
                id: revision_id.clone(),
                parent_id: row.get("parent_id"),
                origin: row.get("origin"),
                input_sha256: row.get("input_sha256"),
                engine: row.get("engine"),
                model_id: row.get("model_id"),
                model_version: row.get("model_version"),
                locale: row.get("locale"),
                alignment_origin: row.get("alignment_origin"),
                status: row.get("status"),
                normalization_profile: row.get("normalization_profile"),
                chunking_strategy: row.get("chunking_strategy"),
                duration_ms: row
                    .get::<Option<i64>, _>("duration_ms")
                    .and_then(|v| u64::try_from(v).ok()),
                created_at: row.get("created_at"),
                engine_backend: row.get("engine_backend"),
                engine_params_json: row.get("engine_params_json"),
                source_vendor: row.get("source_vendor"),
            };

            let segment_rows = sqlx::query(
                "SELECT id, seq_index, start_ms, end_ms, speaker_id, text, confidence,
                        confidence_scale, alignment_origin
                 FROM transcript_segments WHERE transcript_revision_id = ?1 ORDER BY seq_index",
            )
            .bind(&revision_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;

            let mut segments = Vec::with_capacity(segment_rows.len());
            for segment in &segment_rows {
                let segment_id: String = segment.get("id");
                let word_rows = sqlx::query(
                    "SELECT id, seq_index, start_ms, end_ms, text, confidence, confidence_scale
                     FROM transcript_words WHERE segment_id = ?1 ORDER BY seq_index",
                )
                .bind(&segment_id)
                .fetch_all(self.db.pool())
                .await
                .map_err(PersistenceError::from)?;

                segments.push(SegmentRow {
                    seq_index: u32::try_from(segment.get::<i64, _>("seq_index")).unwrap_or(0),
                    start_ms: segment
                        .get::<Option<i64>, _>("start_ms")
                        .and_then(|v| u64::try_from(v).ok()),
                    end_ms: segment
                        .get::<Option<i64>, _>("end_ms")
                        .and_then(|v| u64::try_from(v).ok()),
                    speaker_id: segment.get("speaker_id"),
                    text: segment.get("text"),
                    confidence: segment.get("confidence"),
                    confidence_scale: segment.get("confidence_scale"),
                    alignment_origin: segment.get("alignment_origin"),
                    words: word_rows
                        .iter()
                        .map(|word| WordRow {
                            id: word.get("id"),
                            seq_index: u32::try_from(word.get::<i64, _>("seq_index")).unwrap_or(0),
                            start_ms: u64::try_from(word.get::<i64, _>("start_ms")).unwrap_or(0),
                            end_ms: u64::try_from(word.get::<i64, _>("end_ms")).unwrap_or(0),
                            text: word.get("text"),
                            confidence: word.get("confidence"),
                            confidence_scale: word.get("confidence_scale"),
                        })
                        .collect(),
                    id: segment_id,
                });
            }

            let annotation_rows = sqlx::query(
                "SELECT id, segment_index, kind, reason, start_ms, end_ms
                 FROM transcript_annotations WHERE transcript_revision_id = ?1
                 ORDER BY segment_index, id",
            )
            .bind(&revision_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;

            out.push(TranscriptDocument {
                schema_version: TRANSCRIPT_SCHEMA.to_owned(),
                recording_id: recording_id.to_owned(),
                revision,
                segments,
                annotations: annotation_rows
                    .iter()
                    .map(|row| TranscriptAnnotationRow {
                        id: row.get("id"),
                        segment_index: u32::try_from(row.get::<i64, _>("segment_index"))
                            .unwrap_or(0),
                        kind: row.get("kind"),
                        reason: row.get("reason"),
                        start_ms: row
                            .get::<Option<i64>, _>("start_ms")
                            .and_then(|v| u64::try_from(v).ok()),
                        end_ms: row
                            .get::<Option<i64>, _>("end_ms")
                            .and_then(|v| u64::try_from(v).ok()),
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// Generated outputs for one recording.
    pub async fn output_documents(&self, recording_id: &str) -> ExportResult<Vec<OutputDocument>> {
        let rows = sqlx::query(
            "SELECT id, output_type, transcript_revision_id, template_version, model_id,
                    prompt_version, schema_version, body_json, status, created_at
             FROM ai_outputs WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let output_id: String = row.get("id");
            let claim_rows = sqlx::query(
                "SELECT id, item_path, kind, category, claim_text, state, entailment,
                        review_reasons_json, reviewer, reviewed_at, review_accepted, review_note
                 FROM output_claims WHERE output_id = ?1 ORDER BY item_path, id",
            )
            .bind(&output_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;
            let evidence_rows = sqlx::query(
                "SELECT id, claim_id, position, item_path, cited_revision_id, seq_index,
                        char_start, char_end, quote, verification_status, rejection_code,
                        matched_text, alignment_origin, seek_ms
                 FROM output_evidence WHERE output_id = ?1 ORDER BY claim_id, position",
            )
            .bind(&output_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;

            out.push(OutputDocument {
                schema_version: OUTPUT_SCHEMA.to_owned(),
                recording_id: recording_id.to_owned(),
                output: OutputRow {
                    id: output_id,
                    output_type: row.get("output_type"),
                    transcript_revision_id: row.get("transcript_revision_id"),
                    template_version: row.get("template_version"),
                    model_id: row.get("model_id"),
                    prompt_version: row.get("prompt_version"),
                    schema_version: row.get("schema_version"),
                    body_json: row.get("body_json"),
                    status: row.get("status"),
                    created_at: row.get("created_at"),
                },
                evidence: evidence_rows
                    .iter()
                    .map(|evidence| EvidenceRow {
                        id: evidence.get("id"),
                        claim_id: evidence.get("claim_id"),
                        position: evidence.get("position"),
                        item_path: evidence.get("item_path"),
                        cited_revision_id: evidence.get("cited_revision_id"),
                        seq_index: evidence.get("seq_index"),
                        char_start: evidence.get("char_start"),
                        char_end: evidence.get("char_end"),
                        quote: evidence.get("quote"),
                        verification_status: evidence.get("verification_status"),
                        rejection_code: evidence.get("rejection_code"),
                        matched_text: evidence.get("matched_text"),
                        alignment_origin: evidence.get("alignment_origin"),
                        seek_ms: evidence.get("seek_ms"),
                    })
                    .collect(),
                claims: claim_rows
                    .iter()
                    .map(|claim| ClaimRow {
                        id: claim.get("id"),
                        item_path: claim.get("item_path"),
                        kind: claim.get("kind"),
                        category: claim.get("category"),
                        claim_text: claim.get("claim_text"),
                        state: claim.get("state"),
                        entailment: claim.get("entailment"),
                        review_reasons_json: claim.get("review_reasons_json"),
                        reviewer: claim.get("reviewer"),
                        reviewed_at: claim.get("reviewed_at"),
                        review_accepted: claim.get("review_accepted"),
                        review_note: claim.get("review_note"),
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// Configured sources, without cursors, capability snapshots or handles.
    pub async fn source_documents(&self) -> ExportResult<Vec<SourceDocument>> {
        let rows = sqlx::query(
            "SELECT id, source_kind, account_scope, display_name, status, sync_policy, created_at
             FROM sources ORDER BY created_at, id",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;
        Ok(rows
            .iter()
            .map(|row| SourceDocument {
                schema_version: SOURCE_SCHEMA.to_owned(),
                id: row.get("id"),
                source_kind: row.get("source_kind"),
                account_scope: row.get("account_scope"),
                display_name: row.get("display_name"),
                status: row.get("status"),
                sync_policy: row.get("sync_policy"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    /// Tool definitions, when the capability that owns the table has shipped.
    pub async fn tool_documents(&self) -> ExportResult<Vec<ToolDocument>> {
        if !self.table_exists("transcript_tools").await? {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT tool_id, version, display_name, kind, prompt_version, output_schema_version,
                    definition_sha256, origin, optional, created_at
             FROM transcript_tools ORDER BY tool_id, version",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;
        Ok(rows
            .iter()
            .map(|row| ToolDocument {
                schema_version: TOOL_SCHEMA.to_owned(),
                id: row.get("tool_id"),
                version: row.get("version"),
                definition_sha256: row.get("definition_sha256"),
                origin: row.get("origin"),
                optional: row.get("optional"),
                created_at: row.get("created_at"),
                display_name: row.get("display_name"),
                kind: row.get("kind"),
                prompt_version: row.get("prompt_version"),
                output_schema_version: row.get("output_schema_version"),
            })
            .collect())
    }

    /// The people roster: names, aliases and plain contacts (VP-077).
    ///
    /// Ordinary library data, carried by every portable backup. Nothing here is
    /// derived from anybody's voice — the enrolment that may hang off the same
    /// person is [`Self::voice_profile_documents`], a separate namespace with a
    /// separate rule.
    pub async fn people_documents(&self) -> ExportResult<Vec<PersonDocument>> {
        if !self.table_exists("people").await? {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT id, display_name, aliases_json, email, organisation, status, created_at,
                    updated_at
             FROM people ORDER BY created_at, id",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;
        rows.iter()
            .map(|row| {
                let aliases_json: String = row.get("aliases_json");
                let aliases: Vec<String> =
                    serde_json::from_str(&aliases_json).map_err(ExportError::Serialization)?;
                Ok(PersonDocument {
                    schema_version: PERSON_SCHEMA.to_owned(),
                    id: row.get("id"),
                    display_name: row.get("display_name"),
                    aliases,
                    email: row.get("email"),
                    organisation: row.get("organisation"),
                    status: row.get("status"),
                    created_at: row.get("created_at"),
                    updated_at: row.get("updated_at"),
                })
            })
            .collect()
    }

    /// Enrolments and their sealed exemplars, one document per person (VP-078).
    ///
    /// Reading this is not the same as exporting it: the caller still needs a
    /// [`VoiceProfilePermit`](super::voiceprofiles::VoiceProfilePermit), which
    /// only an encrypted archive can produce. The query exists here, beside the
    /// other document queries, because the column allowlist discipline is the
    /// same — `speaker_suggestions` is absent on purpose (a pending decision
    /// about a diarization run, and runs are not archived).
    pub async fn voice_profile_documents(&self) -> ExportResult<Vec<VoiceProfileDocument>> {
        if !self.table_exists("person_voice_enrollments").await? {
            return Ok(Vec::new());
        }
        let enrollments = sqlx::query(
            "SELECT person_id, consent_version, consent_note, retention_months, last_matched_at,
                    status, expired_at, created_at
             FROM person_voice_enrollments ORDER BY created_at, person_id",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(PersistenceError::from)?;

        let mut documents = Vec::with_capacity(enrollments.len());
        for row in &enrollments {
            let person_id: String = row.get("person_id");
            let retention: i64 = row.get("retention_months");
            let exemplar_rows = sqlx::query(
                "SELECT id, recording_id, cluster_key, source_start_ms, source_end_ms, speech_ms,
                        model_id, model_version, model_sha256, embedding_dim, embedding_cipher,
                        embedding_nonce, created_at
                 FROM voice_exemplars WHERE person_id = ?1 ORDER BY created_at, id",
            )
            .bind(&person_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;

            let exemplars = exemplar_rows
                .iter()
                .map(|row| ExemplarRow {
                    id: row.get("id"),
                    recording_id: row.get("recording_id"),
                    cluster_key: row.get("cluster_key"),
                    source_start_ms: row
                        .get::<Option<i64>, _>("source_start_ms")
                        .and_then(|value| u64::try_from(value).ok()),
                    source_end_ms: row
                        .get::<Option<i64>, _>("source_end_ms")
                        .and_then(|value| u64::try_from(value).ok()),
                    speech_ms: u64::try_from(row.get::<i64, _>("speech_ms")).unwrap_or(0),
                    model_id: row.get("model_id"),
                    model_version: row.get("model_version"),
                    model_sha256: row.get("model_sha256"),
                    embedding_dim: u32::try_from(row.get::<i64, _>("embedding_dim")).unwrap_or(0),
                    embedding_cipher_b64: BASE64.encode(row.get::<Vec<u8>, _>("embedding_cipher")),
                    embedding_nonce_b64: BASE64.encode(row.get::<Vec<u8>, _>("embedding_nonce")),
                    created_at: row.get("created_at"),
                })
                .collect();

            documents.push(VoiceProfileDocument {
                schema_version: VOICEPROFILE_SCHEMA.to_owned(),
                person_id,
                enrollment: EnrollmentRow {
                    consent_version: row.get("consent_version"),
                    consent_note: row.get("consent_note"),
                    retention_months: u8::try_from(retention).unwrap_or(MAX_RETENTION_MONTHS),
                    last_matched_at: row.get("last_matched_at"),
                    status: row.get("status"),
                    expired_at: row.get("expired_at"),
                    created_at: row.get("created_at"),
                },
                exemplars,
            });
        }
        Ok(documents)
    }

    /// Archive-relative library path of a media asset.
    pub async fn media_relative_path(&self, media_asset_id: &str) -> ExportResult<String> {
        let row = sqlx::query("SELECT relative_path FROM media_assets WHERE id = ?1")
            .bind(media_asset_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(PersistenceError::from)?
            .ok_or(ExportError::RecordNotFound)?;
        Ok(row.get("relative_path"))
    }

    async fn table_exists(&self, name: &str) -> ExportResult<bool> {
        let row = sqlx::query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(name)
            .fetch_optional(self.db.pool())
            .await
            .map_err(PersistenceError::from)?;
        Ok(row.is_some())
    }
}

/// The extension of an archive-relative media path, defaulting to `bin`.
#[must_use]
pub fn extension_of(relative_path: &str) -> String {
    relative_path
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 8
                && extension.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .unwrap_or("bin")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_sanitized_to_a_short_alphanumeric_suffix() {
        assert_eq!(extension_of("media/sha256/ab/abc.wav"), "wav");
        assert_eq!(extension_of("media/sha256/ab/abc"), "bin");
        assert_eq!(extension_of("media/sha256/ab/abc."), "bin");
        assert_eq!(extension_of("media/sha256/ab/abc.WAV"), "wav");
        assert_eq!(extension_of("x.verylongextension"), "bin");
        assert_eq!(extension_of("x.wa v"), "bin");
    }
}
