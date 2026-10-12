//! Persistence for diarization runs, speaker turns and speaker labelling
//! (spec 8.7, 13.2, 13.3).
//!
//! Two things are deliberate here.
//!
//! **Raw turns are kept.** `speaker_turns` stores what the provider said,
//! verbatim; `segment_speaker_assignments` stores what reconciliation
//! concluded. Keeping both means a reconciliation can be recomputed after a
//! transcript correction without re-running diarization, and an ambiguous span
//! stays visible as ambiguous.
//!
//! **Labelling never rewrites transcript text.** A segment points at a speaker
//! row; the row carries the label. A rename therefore changes one row, applies
//! to every segment of that cluster at once, and is reversible because
//! `original_label` is never overwritten (spec 8.7, \u00a74.5).

use plaudon_domain::ids::{RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// A diarization run as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiarizationRunInsert {
    /// Run identity.
    pub id: Uuid,
    /// Recording that was diarized.
    pub recording_id: RecordingId,
    /// Media asset that was diarized.
    pub media_asset_id: String,
    /// Provider identifier, which must be in the validated allowlist.
    pub provider_id: String,
    /// Provider version, when known.
    pub provider_version: Option<String>,
    /// Where processing happened.
    pub privacy_class: String,
    /// Terminal status: `succeeded`, `failed` or `canceled`.
    pub status: String,
    /// SHA-256 of the exact media bytes.
    pub input_sha256: String,
    /// Clusters the provider reported.
    pub detected_cluster_count: Option<u32>,
    /// When the run started.
    pub created_at: Timestamp,
    /// When the run finished.
    pub finished_at: Option<Timestamp>,
}

/// One raw provider turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerTurnInsert {
    /// Position in the provider's output.
    pub seq_index: u32,
    /// Opaque cluster identifier.
    pub cluster_key: String,
    /// Inclusive start.
    pub start_ms: u64,
    /// Exclusive end.
    pub end_ms: u64,
    /// Provider confidence, never compared across providers.
    pub confidence: Option<f64>,
    /// Scale the confidence is expressed on.
    pub confidence_scale: Option<String>,
}

/// A reconciled per-segment assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentInsert {
    /// Segment position in the revision.
    pub seq_index: u32,
    /// `single`, `overlapping` or `unassigned`.
    pub assignment: String,
    /// The one cluster, for `single`.
    pub cluster_key: Option<String>,
    /// JSON array of clusters, for `overlapping`.
    pub overlapping_keys_json: Option<String>,
    /// Milliseconds of the segment covered by the attributed cluster(s).
    pub covered_ms: u64,
}

/// A speaker row as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSpeaker {
    /// Row identity.
    pub id: SpeakerId,
    /// Recording the row belongs to.
    pub recording_id: RecordingId,
    /// Diarization cluster, when the row came from a run.
    pub cluster_key: Option<String>,
    /// The machine-assigned label the row started with.
    pub original_label: String,
    /// Current label.
    pub label: String,
    /// User-entered name.
    pub name: Option<String>,
    /// `inferred` or `user_confirmed`.
    pub provenance: String,
    /// Whether a human confirmed the name as this speaker's identity.
    pub user_confirmed: bool,
    /// When the row was renamed.
    pub renamed_at: Option<Timestamp>,
    /// When the name was confirmed.
    pub confirmed_at: Option<Timestamp>,
}

/// Reads and writes diarization output and speaker labels.
#[derive(Debug, Clone)]
pub struct SpeakerRepository {
    db: Database,
}

impl SpeakerRepository {
    /// Open the repository.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Record a diarization run with its raw turns, in one transaction.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures; the schema rejects inverted or
    /// unnamed turns.
    pub async fn record_run(
        &self,
        run: &DiarizationRunInsert,
        turns: &[SpeakerTurnInsert],
    ) -> PersistenceResult<()> {
        let mut tx = self.db.begin_write().await?;
        sqlx::query(
            "INSERT INTO diarization_runs (id, recording_id, media_asset_id, provider_id,
                 provider_version, privacy_class, status, input_sha256, detected_cluster_count,
                 created_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(run.id.to_string())
        .bind(run.recording_id.to_string())
        .bind(&run.media_asset_id)
        .bind(&run.provider_id)
        .bind(run.provider_version.as_deref())
        .bind(&run.privacy_class)
        .bind(&run.status)
        .bind(&run.input_sha256)
        .bind(run.detected_cluster_count.map(i64::from))
        .bind(run.created_at.as_millis())
        .bind(run.finished_at.map(Timestamp::as_millis))
        .execute(&mut *tx)
        .await?;

        for turn in turns {
            sqlx::query(
                "INSERT INTO speaker_turns (id, diarization_run_id, seq_index, cluster_key,
                     start_ms, end_ms, confidence, confidence_scale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(run.id.to_string())
            .bind(i64::from(turn.seq_index))
            .bind(&turn.cluster_key)
            .bind(i64::try_from(turn.start_ms).unwrap_or(i64::MAX))
            .bind(i64::try_from(turn.end_ms).unwrap_or(i64::MAX))
            .bind(turn.confidence)
            .bind(turn.confidence_scale.as_deref())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Record a reconciliation, create the speaker rows it implies, and point
    /// every unambiguously attributed segment at its speaker row.
    ///
    /// Overlapping and unassigned segments deliberately keep
    /// `transcript_segments.speaker_id` NULL: a single-valued column cannot
    /// express two simultaneous speakers, and guessing one would assert
    /// something the audio does not support (spec 8.7).
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn apply_reconciliation(
        &self,
        run_id: Uuid,
        recording_id: RecordingId,
        revision_id: TranscriptRevisionId,
        cluster_order: &[String],
        assignments: &[AssignmentInsert],
    ) -> PersistenceResult<Vec<StoredSpeaker>> {
        let mut tx = self.db.begin_write().await?;

        for (index, cluster_key) in cluster_order.iter().enumerate() {
            let label = format!("Speaker {}", index + 1);
            sqlx::query(
                "INSERT INTO speakers (id, recording_id, label, name, provenance, user_confirmed,
                     cluster_key, original_label)
                 VALUES (?1, ?2, ?3, NULL, 'inferred', 0, ?4, ?3)
                 ON CONFLICT(recording_id, cluster_key) WHERE cluster_key IS NOT NULL DO NOTHING",
            )
            .bind(SpeakerId::new().to_string())
            .bind(recording_id.to_string())
            .bind(&label)
            .bind(cluster_key)
            .execute(&mut *tx)
            .await?;
        }

        for assignment in assignments {
            sqlx::query(
                "INSERT INTO segment_speaker_assignments (transcript_revision_id, seq_index,
                     diarization_run_id, assignment, cluster_key, overlapping_keys_json,
                     covered_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(transcript_revision_id, seq_index, diarization_run_id)
                 DO UPDATE SET assignment = excluded.assignment,
                               cluster_key = excluded.cluster_key,
                               overlapping_keys_json = excluded.overlapping_keys_json,
                               covered_ms = excluded.covered_ms",
            )
            .bind(revision_id.to_string())
            .bind(i64::from(assignment.seq_index))
            .bind(run_id.to_string())
            .bind(&assignment.assignment)
            .bind(assignment.cluster_key.as_deref())
            .bind(assignment.overlapping_keys_json.as_deref())
            .bind(i64::try_from(assignment.covered_ms).unwrap_or(i64::MAX))
            .execute(&mut *tx)
            .await?;

            let speaker_id: Option<String> = match (
                assignment.assignment.as_str(),
                assignment.cluster_key.as_deref(),
            ) {
                ("single", Some(key)) => {
                    sqlx::query_scalar(
                        "SELECT id FROM speakers WHERE recording_id = ?1 AND cluster_key = ?2",
                    )
                    .bind(recording_id.to_string())
                    .bind(key)
                    .fetch_optional(&mut *tx)
                    .await?
                }
                _ => None,
            };

            sqlx::query(
                "UPDATE transcript_segments SET speaker_id = ?1
                 WHERE transcript_revision_id = ?2 AND seq_index = ?3",
            )
            .bind(speaker_id)
            .bind(revision_id.to_string())
            .bind(i64::from(assignment.seq_index))
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        self.list_for_recording(recording_id).await
    }

    /// Speaker rows for a recording, in label order.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn list_for_recording(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<StoredSpeaker>> {
        let rows = sqlx::query(
            "SELECT id, cluster_key, original_label, label, name, provenance, user_confirmed,
                    renamed_at, confirmed_at
             FROM speakers WHERE recording_id = ?1
             ORDER BY COALESCE(original_label, label), id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                let original: Option<String> = row.try_get("original_label")?;
                let label: String = row.try_get("label")?;
                Ok(StoredSpeaker {
                    id: id.parse().map_err(|_| PersistenceError::IntegrityFailed)?,
                    recording_id,
                    cluster_key: row.try_get("cluster_key")?,
                    original_label: original.unwrap_or_else(|| label.clone()),
                    label,
                    name: row.try_get("name")?,
                    provenance: row.try_get("provenance")?,
                    user_confirmed: row.try_get::<i64, _>("user_confirmed")? != 0,
                    renamed_at: row
                        .try_get::<Option<i64>, _>("renamed_at")?
                        .map(Timestamp::from_millis),
                    confirmed_at: row
                        .try_get::<Option<i64>, _>("confirmed_at")?
                        .map(Timestamp::from_millis),
                })
            })
            .collect()
    }

    /// Rename a speaker. The name is recorded; identity is not claimed until
    /// [`SpeakerRepository::confirm`] is called.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::RecordNotFound`] for an unknown row.
    pub async fn rename(&self, id: SpeakerId, name: &str, at: Timestamp) -> PersistenceResult<()> {
        let affected = sqlx::query(
            "UPDATE speakers
             SET name = ?2, label = ?2, provenance = 'inferred', user_confirmed = 0,
                 renamed_at = ?3, confirmed_at = NULL
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(name)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        Ok(())
    }

    /// Confirm that the typed name identifies this speaker.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Conflict`] when the row has no name: there
    /// is nothing to confirm, and the schema's own check would reject it.
    pub async fn confirm(&self, id: SpeakerId, at: Timestamp) -> PersistenceResult<()> {
        let name: Option<Option<String>> =
            sqlx::query_scalar("SELECT name FROM speakers WHERE id = ?1")
                .bind(id.to_string())
                .fetch_optional(self.db.pool())
                .await?;
        match name {
            None => return Err(PersistenceError::RecordNotFound),
            Some(None) => return Err(PersistenceError::Conflict),
            Some(Some(_)) => {}
        }
        sqlx::query(
            "UPDATE speakers
             SET provenance = 'user_confirmed', user_confirmed = 1, confirmed_at = ?2
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Withdraw a confirmation, keeping the typed name.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::RecordNotFound`] for an unknown row.
    pub async fn unconfirm(&self, id: SpeakerId) -> PersistenceResult<()> {
        let affected = sqlx::query(
            "UPDATE speakers
             SET provenance = 'inferred', user_confirmed = 0, confirmed_at = NULL
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        Ok(())
    }

    /// Restore the machine-assigned label, discarding the name.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::RecordNotFound`] for an unknown row.
    pub async fn reset(&self, id: SpeakerId) -> PersistenceResult<()> {
        let affected = sqlx::query(
            "UPDATE speakers
             SET label = COALESCE(original_label, label), name = NULL, provenance = 'inferred',
                 user_confirmed = 0, renamed_at = NULL, confirmed_at = NULL
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        Ok(())
    }
}
