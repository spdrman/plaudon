//! The biometric tier of the voice-profiles feature: enrolments, encrypted
//! exemplars, match suggestions and the consent journal
//! (`VOICE_PROFILES_SPEC` VP-070..VP-080, VP-130, VP-216; ADR-0080, ADR-0082).
//!
//! Three properties of this module are load-bearing.
//!
//! **No plaintext embedding ever reaches it.** [`ExemplarInsert`] carries
//! ciphertext and a nonce, nothing else; the vector exists in plaintext only in
//! `plaudon_app_core::voice_profiles`, in process memory, behind the OS secret
//! store. A repository that cannot see a vector cannot log, export or leak one.
//!
//! **Nothing here assigns anybody.** Matching writes rows into
//! `speaker_suggestions` with a score and a band; turning a suggestion into a
//! label is [`crate::people::PeopleRepository::assign_speaker`], which only
//! ever runs from an explicit user decision (ADR-0082).
//!
//! **The journal outlives its subject.** `voice_profile_events` has no foreign
//! key to `people`, so deleting a person — the thing a user is most likely to
//! want evidence of afterwards — keeps the record of what was deleted and when.

use plaudon_domain::ids::{PersonId, RecordingId, SpeakerSuggestionId, VoiceExemplarId};
use plaudon_domain::time::Timestamp;
use sqlx::{Row, Sqlite, Transaction};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// Longest retention the law of the strictest jurisdiction we target allows
/// (Texas CUBI); the user may shorten it, never lengthen it (VP-130).
pub const MAX_RETENTION_MONTHS: u8 = 12;

/// Kinds of journalled event (`voice_profile_events.kind`).
///
/// The set is closed and mirrors the schema's `CHECK`, so a kind that the
/// database would reject cannot be constructed in Rust either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceProfileEventKind {
    /// A roster entry was created.
    PersonCreated,
    /// A roster entry's display name changed.
    PersonRenamed,
    /// Two roster entries were merged into one.
    PersonMerged,
    /// A roster entry was deleted.
    PersonDeleted,
    /// A person's voice enrolment was created after the consent gate.
    Enrolled,
    /// An exemplar was added to an existing enrolment.
    ExemplarAdded,
    /// An enrolment was split by exemplar provenance.
    EnrollmentSplit,
    /// Retention elapsed and the exemplars were destroyed.
    EnrollmentExpired,
    /// The user deleted the enrolment, keeping the roster entry.
    EnrollmentDeleted,
    /// The user accepted a suggestion.
    SuggestionConfirmed,
    /// The user rejected a suggestion.
    SuggestionRejected,
    /// The user rejected a suggestion and named somebody else.
    SuggestionCorrected,
    /// A speaker row was linked to a roster person.
    AssignmentSet,
    /// A speaker row's roster link was removed.
    AssignmentCleared,
}

impl VoiceProfileEventKind {
    /// Stored string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonCreated => "person_created",
            Self::PersonRenamed => "person_renamed",
            Self::PersonMerged => "person_merged",
            Self::PersonDeleted => "person_deleted",
            Self::Enrolled => "enrolled",
            Self::ExemplarAdded => "exemplar_added",
            Self::EnrollmentSplit => "enrollment_split",
            Self::EnrollmentExpired => "enrollment_expired",
            Self::EnrollmentDeleted => "enrollment_deleted",
            Self::SuggestionConfirmed => "suggestion_confirmed",
            Self::SuggestionRejected => "suggestion_rejected",
            Self::SuggestionCorrected => "suggestion_corrected",
            Self::AssignmentSet => "assignment_set",
            Self::AssignmentCleared => "assignment_cleared",
        }
    }
}

/// One journalled event as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredVoiceProfileEvent {
    /// Row identity.
    pub id: String,
    /// Person the event is about. Not a foreign key: it survives deletion.
    pub person_id: PersonId,
    /// What happened.
    pub kind: String,
    /// Counts and versions, as a JSON object. Never an embedding.
    pub detail_json: String,
    /// When it happened.
    pub created_at: Timestamp,
}

/// Append one event inside an open transaction.
///
/// `detail` must be a JSON **object**; the schema rejects anything else, which
/// keeps the journal machine-readable for the §12.3 consent export.
pub(crate) async fn journal(
    tx: &mut Transaction<'_, Sqlite>,
    person_id: PersonId,
    kind: VoiceProfileEventKind,
    detail: &str,
    at: Timestamp,
) -> PersistenceResult<()> {
    sqlx::query(
        "INSERT INTO voice_profile_events (id, person_id, kind, detail_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(person_id.to_string())
    .bind(kind.as_str())
    .bind(detail)
    .bind(at.as_millis())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// What the user attested to when an enrolment was created (VP-120..VP-123).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentDraft {
    /// Version of the disclosure text shown at the gate.
    pub consent_version: String,
    /// Optional free-text basis the user recorded for themselves.
    pub consent_note: Option<String>,
    /// Retention in months, 1..=12.
    pub retention_months: u8,
}

/// A person's enrolment as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEnrollment {
    /// The person this enrolment belongs to.
    pub person_id: PersonId,
    /// Disclosure version recorded at the gate.
    pub consent_version: String,
    /// Free-text basis, if the user recorded one.
    pub consent_note: Option<String>,
    /// Retention in months.
    pub retention_months: u8,
    /// Last time a match used this enrolment; drives the expiry clock.
    pub last_matched_at: Option<Timestamp>,
    /// `active` or `expired`.
    pub status: String,
    /// When the enrolment was created.
    pub created_at: Timestamp,
}

/// One encrypted exemplar to store.
///
/// There is no plaintext-vector constructor on purpose: sealing happens in
/// `plaudon_app_core::voice_profiles::EmbeddingCipher`, so this crate never
/// holds biometric material in a form it could accidentally persist or print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExemplarInsert {
    /// Enrolled person the exemplar belongs to.
    pub person_id: PersonId,
    /// Recording it was cut from, if that recording still exists.
    pub recording_id: Option<RecordingId>,
    /// Diarization run it was cut from.
    pub diarization_run_id: Option<Uuid>,
    /// Cluster within that run.
    pub cluster_key: Option<String>,
    /// Start of the source span, in milliseconds.
    pub source_start_ms: Option<u64>,
    /// End of the source span, in milliseconds.
    pub source_end_ms: Option<u64>,
    /// Net speech used, in milliseconds. The schema rejects under one second.
    pub speech_ms: u64,
    /// Embedding model identity.
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
    /// Embedding model digest, 64 hex characters.
    pub model_sha256: String,
    /// Dimensionality of the sealed vector.
    pub embedding_dim: u32,
    /// XChaCha20-Poly1305 ciphertext of the vector.
    pub embedding_cipher: Vec<u8>,
    /// The 24-byte nonce it was sealed with.
    pub embedding_nonce: Vec<u8>,
}

/// An exemplar as stored. The vector stays sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredExemplar {
    /// Row identity.
    pub id: VoiceExemplarId,
    /// Enrolled person.
    pub person_id: PersonId,
    /// Source recording, or `None` once it was deleted.
    pub recording_id: Option<RecordingId>,
    /// Source diarization run, or `None` once it was deleted.
    pub diarization_run_id: Option<Uuid>,
    /// Cluster within that run.
    pub cluster_key: Option<String>,
    /// Start of the source span.
    pub source_start_ms: Option<u64>,
    /// End of the source span.
    pub source_end_ms: Option<u64>,
    /// Net speech used.
    pub speech_ms: u64,
    /// Embedding model identity.
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
    /// Embedding model digest.
    pub model_sha256: String,
    /// Dimensionality of the sealed vector.
    pub embedding_dim: u32,
    /// Ciphertext.
    pub embedding_cipher: Vec<u8>,
    /// Nonce.
    pub embedding_nonce: Vec<u8>,
    /// When the exemplar was stored.
    pub created_at: Timestamp,
}

/// One suggestion produced by a match job.
#[derive(Debug, Clone, PartialEq)]
pub struct SuggestionInsert {
    /// Recording the cluster belongs to.
    pub recording_id: RecordingId,
    /// Diarization run the cluster belongs to.
    pub diarization_run_id: Uuid,
    /// Cluster within that run.
    pub cluster_key: String,
    /// Candidate person.
    pub person_id: PersonId,
    /// Similarity score.
    pub score: f64,
    /// `suggest` or `weak` (VP-061).
    pub band: String,
    /// Embedding model identity the score came from.
    pub model_id: String,
    /// Embedding model version the score came from.
    pub model_version: String,
}

/// A suggestion as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSuggestion {
    /// Row identity.
    pub id: SpeakerSuggestionId,
    /// Recording the cluster belongs to.
    pub recording_id: RecordingId,
    /// Diarization run the cluster belongs to.
    pub diarization_run_id: Uuid,
    /// Cluster within that run.
    pub cluster_key: String,
    /// Candidate person.
    pub person_id: PersonId,
    /// Similarity score.
    pub score: f64,
    /// How the score was computed.
    pub score_kind: String,
    /// `suggest` or `weak`.
    pub band: String,
    /// `pending`, `confirmed`, `rejected`, `corrected` or `expired`.
    pub state: String,
    /// Who the cluster actually was, when the user corrected the suggestion.
    pub corrected_person_id: Option<PersonId>,
    /// When the user settled it.
    pub decided_at: Option<Timestamp>,
    /// Embedding model identity.
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
    /// When the suggestion was produced.
    pub created_at: Timestamp,
}

/// What deleting an enrolment destroyed. Shown in the disclosure and
/// journalled, so the count the user saw is the count that happened (VP-080).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EnrollmentDeletion {
    /// Exemplar rows destroyed.
    pub exemplars: u32,
    /// Suggestion rows destroyed.
    pub suggestions: u32,
}

/// Reads and writes the biometric tier.
#[derive(Debug, Clone)]
pub struct VoiceEnrollmentRepository {
    db: Database,
}

impl VoiceEnrollmentRepository {
    /// Open the repository.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Create a person's enrolment, recording the consent state it was created
    /// under, and journal it.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Conflict`] when the person does not exist or is
    /// already enrolled, [`PersistenceError::Invalid`] for a retention outside
    /// 1..=12 months.
    pub async fn enroll(
        &self,
        person_id: PersonId,
        draft: &EnrollmentDraft,
        at: Timestamp,
    ) -> PersistenceResult<StoredEnrollment> {
        validate_retention(draft.retention_months)?;
        let mut tx = self.db.begin_write().await?;
        sqlx::query(
            "INSERT INTO person_voice_enrollments
                 (person_id, consent_version, consent_note, retention_months, last_matched_at,
                  status, created_at)
             VALUES (?1, ?2, ?3, ?4, NULL, 'active', ?5)",
        )
        .bind(person_id.to_string())
        .bind(&draft.consent_version)
        .bind(draft.consent_note.as_deref())
        .bind(i64::from(draft.retention_months))
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await?;
        journal(
            &mut tx,
            person_id,
            VoiceProfileEventKind::Enrolled,
            &serde_json::json!({
                "consent_version": draft.consent_version,
                "retention_months": draft.retention_months,
            })
            .to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(StoredEnrollment {
            person_id,
            consent_version: draft.consent_version.clone(),
            consent_note: draft.consent_note.clone(),
            retention_months: draft.retention_months,
            last_matched_at: None,
            status: "active".to_owned(),
            created_at: at,
        })
    }

    /// The enrolment of a person, when there is one.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn enrollment(
        &self,
        person_id: PersonId,
    ) -> PersistenceResult<Option<StoredEnrollment>> {
        let row = sqlx::query(
            "SELECT person_id, consent_version, consent_note, retention_months, last_matched_at,
                    status, created_at
             FROM person_voice_enrollments WHERE person_id = ?1",
        )
        .bind(person_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        row.map(|row| decode_enrollment(&row)).transpose()
    }

    /// Shorten the retention of an enrolment.
    ///
    /// Lengthening is refused rather than clamped: VP-130 allows the user to
    /// choose a shorter clock, never a longer one, and silently ignoring the
    /// request would leave the UI showing a value the database does not hold.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] when there is no enrolment,
    /// [`PersistenceError::Invalid`] when the new value is out of range or not
    /// shorter than the current one.
    pub async fn shorten_retention(
        &self,
        person_id: PersonId,
        months: u8,
    ) -> PersistenceResult<()> {
        validate_retention(months)?;
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT retention_months FROM person_voice_enrollments WHERE person_id = ?1",
        )
        .bind(person_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        let current = current.ok_or(PersistenceError::RecordNotFound)?;
        if i64::from(months) > current {
            return Err(PersistenceError::Invalid(format!(
                "retention may be shortened, never lengthened: {current} -> {months} months"
            )));
        }
        sqlx::query(
            "UPDATE person_voice_enrollments SET retention_months = ?2 WHERE person_id = ?1",
        )
        .bind(person_id.to_string())
        .bind(i64::from(months))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Record that a match used this enrolment, resetting the expiry clock.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] when there is no enrolment.
    pub async fn mark_matched(&self, person_id: PersonId, at: Timestamp) -> PersistenceResult<()> {
        let affected = sqlx::query(
            "UPDATE person_voice_enrollments SET last_matched_at = ?2 WHERE person_id = ?1",
        )
        .bind(person_id.to_string())
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        Ok(())
    }

    /// Store one sealed exemplar and journal the addition.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Conflict`] when the person is not enrolled, and
    /// [`PersistenceError::Invalid`] when the ciphertext, nonce or model
    /// binding is malformed.
    pub async fn add_exemplar(
        &self,
        exemplar: &ExemplarInsert,
        at: Timestamp,
    ) -> PersistenceResult<VoiceExemplarId> {
        validate_exemplar(exemplar)?;
        let id = VoiceExemplarId::new();
        let mut tx = self.db.begin_write().await?;
        sqlx::query(
            "INSERT INTO voice_exemplars
                 (id, person_id, recording_id, diarization_run_id, cluster_key, source_start_ms,
                  source_end_ms, speech_ms, model_id, model_version, model_sha256, embedding_dim,
                  embedding_cipher, embedding_nonce, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .bind(id.to_string())
        .bind(exemplar.person_id.to_string())
        .bind(exemplar.recording_id.map(|id| id.to_string()))
        .bind(exemplar.diarization_run_id.map(|id| id.to_string()))
        .bind(exemplar.cluster_key.as_deref())
        .bind(exemplar.source_start_ms.map(as_i64))
        .bind(exemplar.source_end_ms.map(as_i64))
        .bind(as_i64(exemplar.speech_ms))
        .bind(&exemplar.model_id)
        .bind(&exemplar.model_version)
        .bind(&exemplar.model_sha256)
        .bind(i64::from(exemplar.embedding_dim))
        .bind(exemplar.embedding_cipher.as_slice())
        .bind(exemplar.embedding_nonce.as_slice())
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await?;
        journal(
            &mut tx,
            exemplar.person_id,
            VoiceProfileEventKind::ExemplarAdded,
            &serde_json::json!({
                "model_id": exemplar.model_id,
                "model_version": exemplar.model_version,
                "speech_ms": exemplar.speech_ms,
            })
            .to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Every sealed exemplar of a person, oldest first.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn exemplars(&self, person_id: PersonId) -> PersistenceResult<Vec<StoredExemplar>> {
        let rows = sqlx::query(
            "SELECT id, person_id, recording_id, diarization_run_id, cluster_key, source_start_ms,
                    source_end_ms, speech_ms, model_id, model_version, model_sha256, embedding_dim,
                    embedding_cipher, embedding_nonce, created_at
             FROM voice_exemplars WHERE person_id = ?1 ORDER BY created_at, id",
        )
        .bind(person_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(decode_exemplar).collect()
    }

    /// Delete a person's enrolment: every exemplar and suggestion goes, the
    /// roster entry stays (VP-080).
    ///
    /// Exemplars cascade through the foreign key. Suggestions reference
    /// `people`, not the enrolment, so they are deleted here, in the same
    /// transaction — a suggestion is a score produced by a voiceprint, and a
    /// user who deleted the voiceprint did not consent to keeping its output.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] when there is no enrolment.
    pub async fn delete_enrollment(
        &self,
        person_id: PersonId,
        at: Timestamp,
    ) -> PersistenceResult<EnrollmentDeletion> {
        let mut tx = self.db.begin_write().await?;
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT person_id FROM person_voice_enrollments WHERE person_id = ?1",
        )
        .bind(person_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        if exists.is_none() {
            return Err(PersistenceError::RecordNotFound);
        }

        let counts = tier_counts(&mut tx, person_id).await?;
        sqlx::query("DELETE FROM speaker_suggestions WHERE person_id = ?1")
            .bind(person_id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM person_voice_enrollments WHERE person_id = ?1")
            .bind(person_id.to_string())
            .execute(&mut *tx)
            .await?;
        journal(
            &mut tx,
            person_id,
            VoiceProfileEventKind::EnrollmentDeleted,
            &serde_json::json!({
                "exemplars_destroyed": counts.exemplars,
                "suggestions_destroyed": counts.suggestions,
            })
            .to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(counts)
    }

    /// Expire an enrolment: destroy its exemplar ciphertexts, keep the row as a
    /// tombstone so the UI can explain why matching stopped (VP-130).
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] when there is no enrolment.
    pub async fn expire(
        &self,
        person_id: PersonId,
        at: Timestamp,
    ) -> PersistenceResult<EnrollmentDeletion> {
        let mut tx = self.db.begin_write().await?;
        let affected = sqlx::query(
            "UPDATE person_voice_enrollments SET status = 'expired' WHERE person_id = ?1",
        )
        .bind(person_id.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        let counts = tier_counts(&mut tx, person_id).await?;
        sqlx::query("DELETE FROM voice_exemplars WHERE person_id = ?1")
            .bind(person_id.to_string())
            .execute(&mut *tx)
            .await?;
        journal(
            &mut tx,
            person_id,
            VoiceProfileEventKind::EnrollmentExpired,
            &serde_json::json!({ "exemplars_destroyed": counts.exemplars }).to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(EnrollmentDeletion {
            exemplars: counts.exemplars,
            suggestions: 0,
        })
    }

    /// Record match output. Re-running the same match is idempotent: the
    /// `(run, cluster, person)` unique key makes a repeat insert update the
    /// score of a still-pending row and leave a settled one alone, so a
    /// resumed job never resurrects a decision the user already made (VP-102).
    ///
    /// # Errors
    ///
    /// Propagates persistence failures; the schema rejects an unknown band.
    pub async fn record_suggestions(
        &self,
        suggestions: &[SuggestionInsert],
        at: Timestamp,
    ) -> PersistenceResult<u32> {
        let mut tx = self.db.begin_write().await?;
        let mut written = 0u32;
        for suggestion in suggestions {
            let affected = sqlx::query(
                "INSERT INTO speaker_suggestions
                     (id, recording_id, diarization_run_id, cluster_key, person_id, score,
                      score_kind, band, state, corrected_person_id, decided_at, model_id,
                      model_version, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'cosine_similarity_asnorm', ?7, 'pending', NULL,
                         NULL, ?8, ?9, ?10)
                 ON CONFLICT (diarization_run_id, cluster_key, person_id) DO UPDATE SET
                     score = excluded.score,
                     band = excluded.band,
                     model_id = excluded.model_id,
                     model_version = excluded.model_version,
                     created_at = excluded.created_at
                 WHERE speaker_suggestions.state = 'pending'",
            )
            .bind(SpeakerSuggestionId::new().to_string())
            .bind(suggestion.recording_id.to_string())
            .bind(suggestion.diarization_run_id.to_string())
            .bind(&suggestion.cluster_key)
            .bind(suggestion.person_id.to_string())
            .bind(suggestion.score)
            .bind(&suggestion.band)
            .bind(&suggestion.model_id)
            .bind(&suggestion.model_version)
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await?
            .rows_affected();
            written += u32::try_from(affected).unwrap_or(u32::MAX);
        }
        tx.commit().await?;
        Ok(written)
    }

    /// Pending suggestions for a recording, strongest score first.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn pending_for_recording(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<StoredSuggestion>> {
        let rows = sqlx::query(
            "SELECT id, recording_id, diarization_run_id, cluster_key, person_id, score, score_kind,
                    band, state, corrected_person_id, decided_at, model_id, model_version,
                    created_at
             FROM speaker_suggestions
             WHERE recording_id = ?1 AND state = 'pending'
             ORDER BY score DESC, id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(decode_suggestion).collect()
    }

    /// Settle a suggestion the user accepted.
    ///
    /// Confirming records the decision only. Linking the speaker row to the
    /// person is a separate, explicit step
    /// ([`crate::people::PeopleRepository::assign_speaker`]): no score, at any
    /// value, writes a label by itself (ADR-0082).
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] for an unknown or already settled
    /// row.
    pub async fn confirm_suggestion(
        &self,
        id: SpeakerSuggestionId,
        at: Timestamp,
    ) -> PersistenceResult<StoredSuggestion> {
        self.settle(
            id,
            "confirmed",
            None,
            VoiceProfileEventKind::SuggestionConfirmed,
            at,
        )
        .await
    }

    /// Settle a suggestion the user rejected. The score is kept as negative
    /// evidence for per-enrolment calibration; the rejected cluster's
    /// embedding is never stored (VP-216).
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] for an unknown or already settled
    /// row.
    pub async fn reject_suggestion(
        &self,
        id: SpeakerSuggestionId,
        at: Timestamp,
    ) -> PersistenceResult<StoredSuggestion> {
        self.settle(
            id,
            "rejected",
            None,
            VoiceProfileEventKind::SuggestionRejected,
            at,
        )
        .await
    }

    /// Settle a suggestion the user rejected and replaced with somebody else.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] for an unknown or already settled
    /// row, [`PersistenceError::Invalid`] when the correction names the person
    /// who was suggested.
    pub async fn correct_suggestion(
        &self,
        id: SpeakerSuggestionId,
        corrected_person_id: PersonId,
        at: Timestamp,
    ) -> PersistenceResult<StoredSuggestion> {
        self.settle(
            id,
            "corrected",
            Some(corrected_person_id),
            VoiceProfileEventKind::SuggestionCorrected,
            at,
        )
        .await
    }

    async fn settle(
        &self,
        id: SpeakerSuggestionId,
        state: &str,
        corrected_person_id: Option<PersonId>,
        kind: VoiceProfileEventKind,
        at: Timestamp,
    ) -> PersistenceResult<StoredSuggestion> {
        let mut tx = self.db.begin_write().await?;
        let row = sqlx::query(
            "SELECT id, recording_id, diarization_run_id, cluster_key, person_id, score, score_kind,
                    band, state, corrected_person_id, decided_at, model_id, model_version,
                    created_at
             FROM speaker_suggestions WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let existing = row.as_ref().ok_or(PersistenceError::RecordNotFound)?;
        let current = decode_suggestion(existing)?;
        if current.state != "pending" {
            return Err(PersistenceError::Conflict);
        }
        if corrected_person_id == Some(current.person_id) {
            return Err(PersistenceError::Invalid(
                "a correction must name somebody other than the suggested person".to_owned(),
            ));
        }

        sqlx::query(
            "UPDATE speaker_suggestions
             SET state = ?2, corrected_person_id = ?3, decided_at = ?4
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(state)
        .bind(corrected_person_id.map(|id| id.to_string()))
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await?;
        journal(
            &mut tx,
            current.person_id,
            kind,
            &serde_json::json!({
                "score": current.score,
                "band": current.band,
                "cluster_key": current.cluster_key,
                "corrected_person_id": corrected_person_id.map(|id| id.to_string()),
            })
            .to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(StoredSuggestion {
            state: state.to_owned(),
            corrected_person_id,
            decided_at: Some(at),
            ..current
        })
    }
}

/// Count what the biometric tier currently holds for one person.
pub(crate) async fn tier_counts(
    tx: &mut Transaction<'_, Sqlite>,
    person_id: PersonId,
) -> PersistenceResult<EnrollmentDeletion> {
    let exemplars: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM voice_exemplars WHERE person_id = ?1")
            .bind(person_id.to_string())
            .fetch_one(&mut **tx)
            .await?;
    let suggestions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM speaker_suggestions WHERE person_id = ?1")
            .bind(person_id.to_string())
            .fetch_one(&mut **tx)
            .await?;
    Ok(EnrollmentDeletion {
        exemplars: u32::try_from(exemplars).unwrap_or(u32::MAX),
        suggestions: u32::try_from(suggestions).unwrap_or(u32::MAX),
    })
}

fn validate_retention(months: u8) -> PersistenceResult<()> {
    if months == 0 || months > MAX_RETENTION_MONTHS {
        return Err(PersistenceError::Invalid(format!(
            "retention must be 1..={MAX_RETENTION_MONTHS} months, got {months}"
        )));
    }
    Ok(())
}

fn validate_exemplar(exemplar: &ExemplarInsert) -> PersistenceResult<()> {
    if exemplar.embedding_nonce.len() != 24 {
        return Err(PersistenceError::Invalid(
            "an XChaCha20-Poly1305 nonce is 24 bytes".to_owned(),
        ));
    }
    if exemplar.embedding_cipher.len() < 16 {
        return Err(PersistenceError::Invalid(
            "ciphertext is shorter than one authentication tag".to_owned(),
        ));
    }
    if exemplar.model_sha256.len() != 64
        || !exemplar
            .model_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(PersistenceError::Invalid(
            "model_sha256 must be 64 lowercase hex characters".to_owned(),
        ));
    }
    if exemplar.embedding_dim == 0 {
        return Err(PersistenceError::Invalid(
            "an embedding has at least one dimension".to_owned(),
        ));
    }
    Ok(())
}

fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn parse_person(raw: &str) -> PersistenceResult<PersonId> {
    raw.parse().map_err(|_| PersistenceError::IntegrityFailed)
}

fn decode_enrollment(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<StoredEnrollment> {
    let person_id: String = row.try_get("person_id")?;
    let retention: i64 = row.try_get("retention_months")?;
    Ok(StoredEnrollment {
        person_id: parse_person(&person_id)?,
        consent_version: row.try_get("consent_version")?,
        consent_note: row.try_get("consent_note")?,
        retention_months: u8::try_from(retention).map_err(|_| PersistenceError::IntegrityFailed)?,
        last_matched_at: row
            .try_get::<Option<i64>, _>("last_matched_at")?
            .map(Timestamp::from_millis),
        status: row.try_get("status")?,
        created_at: Timestamp::from_millis(row.try_get::<i64, _>("created_at")?),
    })
}

fn decode_exemplar(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<StoredExemplar> {
    let id: String = row.try_get("id")?;
    let person_id: String = row.try_get("person_id")?;
    let recording_id: Option<String> = row.try_get("recording_id")?;
    let run_id: Option<String> = row.try_get("diarization_run_id")?;
    let dim: i64 = row.try_get("embedding_dim")?;
    Ok(StoredExemplar {
        id: id.parse().map_err(|_| PersistenceError::IntegrityFailed)?,
        person_id: parse_person(&person_id)?,
        recording_id: recording_id
            .map(|raw| raw.parse())
            .transpose()
            .map_err(|_| PersistenceError::IntegrityFailed)?,
        diarization_run_id: run_id
            .map(|raw| raw.parse())
            .transpose()
            .map_err(|_| PersistenceError::IntegrityFailed)?,
        cluster_key: row.try_get("cluster_key")?,
        source_start_ms: row
            .try_get::<Option<i64>, _>("source_start_ms")?
            .map(i64::unsigned_abs),
        source_end_ms: row
            .try_get::<Option<i64>, _>("source_end_ms")?
            .map(i64::unsigned_abs),
        speech_ms: row.try_get::<i64, _>("speech_ms")?.unsigned_abs(),
        model_id: row.try_get("model_id")?,
        model_version: row.try_get("model_version")?,
        model_sha256: row.try_get("model_sha256")?,
        embedding_dim: u32::try_from(dim).map_err(|_| PersistenceError::IntegrityFailed)?,
        embedding_cipher: row.try_get("embedding_cipher")?,
        embedding_nonce: row.try_get("embedding_nonce")?,
        created_at: Timestamp::from_millis(row.try_get::<i64, _>("created_at")?),
    })
}

fn decode_suggestion(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<StoredSuggestion> {
    let id: String = row.try_get("id")?;
    let recording_id: String = row.try_get("recording_id")?;
    let run_id: String = row.try_get("diarization_run_id")?;
    let person_id: String = row.try_get("person_id")?;
    let corrected: Option<String> = row.try_get("corrected_person_id")?;
    Ok(StoredSuggestion {
        id: id.parse().map_err(|_| PersistenceError::IntegrityFailed)?,
        recording_id: recording_id
            .parse()
            .map_err(|_| PersistenceError::IntegrityFailed)?,
        diarization_run_id: run_id
            .parse()
            .map_err(|_| PersistenceError::IntegrityFailed)?,
        cluster_key: row.try_get("cluster_key")?,
        person_id: parse_person(&person_id)?,
        score: row.try_get("score")?,
        score_kind: row.try_get("score_kind")?,
        band: row.try_get("band")?,
        state: row.try_get("state")?,
        corrected_person_id: corrected.map(|raw| parse_person(&raw)).transpose()?,
        decided_at: row
            .try_get::<Option<i64>, _>("decided_at")?
            .map(Timestamp::from_millis),
        model_id: row.try_get("model_id")?,
        model_version: row.try_get("model_version")?,
        created_at: Timestamp::from_millis(row.try_get::<i64, _>("created_at")?),
    })
}
