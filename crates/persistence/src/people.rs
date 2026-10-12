//! The master speaker list — the people roster (`VOICE_PROFILES_SPEC` VP-010,
//! VP-011, VP-072, VP-213, §12.2A).
//!
//! This module is the *roster tier*, and it is deliberately free of biometric
//! machinery: no key, no secret store, no embedding, no score. A library can
//! name its recurring speakers, assign them to recordings and keep doing so
//! forever with voice matching switched off — which is the whole point of
//! §12.2A, because a typed name is ordinary personal data and no statute's
//! biometric definition reaches it. Nothing in this file may grow a dependency
//! on [`crate::voice_profiles`]'s exemplars; the one place the two tiers touch
//! is [`PeopleRepository::delete`], which has to *count and destroy* biometric
//! rows when the person they belong to goes away.
//!
//! Two invariants are expressed in SQL rather than in prose:
//!
//! * `speakers.name` is a **text snapshot** beside `speakers.person_id`.
//!   Deleting a person severs the link (`ON DELETE SET NULL`) and leaves the
//!   name the recording already displayed, so a roster edit can never make a
//!   transcript lose its labels (VP-213d, VP-080).
//! * A merged-away person stays as a `status = 'merged'` tombstone. The journal
//!   refers to people by id and must stay resolvable afterwards (VP-213c).

use plaudon_domain::ids::{PersonId, RecordingId, SpeakerId};
use plaudon_domain::time::Timestamp;
use sqlx::{Row, Sqlite, Transaction};

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};
use crate::voice_profiles::{journal, tier_counts, StoredVoiceProfileEvent, VoiceProfileEventKind};

/// Longest display name the schema accepts.
pub const MAX_DISPLAY_NAME_CHARS: usize = 120;

/// Lifecycle of a roster entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonStatus {
    /// A normal, pickable roster entry.
    Active,
    /// Merged into another entry; kept so journalled ids stay resolvable.
    Merged,
}

impl PersonStatus {
    /// Stored string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Merged => "merged",
        }
    }

    /// Decode a stored string; anything unknown is treated as merged, i.e. not
    /// offered to the user, because showing an unrecognised row in a picker is
    /// worse than hiding it.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => Self::Active,
            _ => Self::Merged,
        }
    }
}

/// The editable fields of a roster entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PersonDraft {
    /// Name shown everywhere the person appears.
    pub display_name: String,
    /// Alternate spellings, used for search and merge-candidate detection.
    pub aliases: Vec<String>,
    /// Optional contact address. Ordinary personal data, never biometric.
    pub email: Option<String>,
    /// Optional organisation.
    pub organisation: Option<String>,
}

impl PersonDraft {
    /// A draft with just a name.
    #[must_use]
    pub fn named(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            ..Self::default()
        }
    }
}

/// A roster entry as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPerson {
    /// Row identity.
    pub id: PersonId,
    /// Name shown everywhere the person appears.
    pub display_name: String,
    /// Alternate spellings.
    pub aliases: Vec<String>,
    /// Optional contact address.
    pub email: Option<String>,
    /// Optional organisation.
    pub organisation: Option<String>,
    /// When the entry was created.
    pub created_at: Timestamp,
    /// When it was last edited.
    pub updated_at: Timestamp,
    /// Active, or a merge tombstone.
    pub status: PersonStatus,
}

/// What a merge moved, counted before it happened so the disclosure the user
/// confirmed is the disclosure that was executed (VP-213c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeOutcome {
    /// Speaker rows repointed at the surviving person.
    pub speaker_links: u32,
    /// Distinct recordings those rows belong to.
    pub recordings: u32,
    /// Exemplars moved to the surviving enrolment.
    pub exemplars: u32,
    /// Pending suggestions repointed.
    pub pending_suggestions: u32,
    /// Whether the loser's enrolment became the survivor's.
    pub enrollment_moved: bool,
}

/// What deleting a person destroyed or severed (VP-080).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PersonDeletion {
    /// Speaker links severed; the name snapshots stay.
    pub severed_speaker_links: u32,
    /// Distinct recordings affected.
    pub recordings: u32,
    /// Exemplars destroyed with the enrolment.
    pub exemplars: u32,
    /// Suggestions destroyed.
    pub suggestions: u32,
}

/// Reads and writes the roster, and the links from speaker rows to it.
#[derive(Debug, Clone)]
pub struct PeopleRepository {
    db: Database,
}

impl PeopleRepository {
    /// Open the repository.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Create a roster entry and journal it.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Invalid`] for an empty or over-long name.
    pub async fn create(
        &self,
        draft: &PersonDraft,
        at: Timestamp,
    ) -> PersistenceResult<StoredPerson> {
        validate_draft(draft)?;
        let id = PersonId::new();
        let aliases_json = encode_aliases(&draft.aliases)?;
        let mut tx = self.db.begin_write().await?;
        sqlx::query(
            "INSERT INTO people (id, display_name, aliases_json, email, organisation, created_at,
                 updated_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 'active')",
        )
        .bind(id.to_string())
        .bind(&draft.display_name)
        .bind(&aliases_json)
        .bind(draft.email.as_deref())
        .bind(draft.organisation.as_deref())
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await?;
        journal(
            &mut tx,
            id,
            VoiceProfileEventKind::PersonCreated,
            &serde_json::json!({ "display_name": draft.display_name }).to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(StoredPerson {
            id,
            display_name: draft.display_name.clone(),
            aliases: draft.aliases.clone(),
            email: draft.email.clone(),
            organisation: draft.organisation.clone(),
            created_at: at,
            updated_at: at,
            status: PersonStatus::Active,
        })
    }

    /// One roster entry, including merge tombstones.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn get(&self, id: PersonId) -> PersistenceResult<Option<StoredPerson>> {
        let row = sqlx::query(
            "SELECT id, display_name, aliases_json, email, organisation, created_at, updated_at,
                    status
             FROM people WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        row.map(|row| decode_person(&row)).transpose()
    }

    /// Every pickable roster entry, by name.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn list_active(&self) -> PersistenceResult<Vec<StoredPerson>> {
        let rows = sqlx::query(
            "SELECT id, display_name, aliases_json, email, organisation, created_at, updated_at,
                    status
             FROM people WHERE status = 'active'
             ORDER BY display_name COLLATE NOCASE, id",
        )
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(decode_person).collect()
    }

    /// Edit a roster entry.
    ///
    /// A rename is journalled with both names, because VP-213a makes the blast
    /// radius of a rename ("the name shown in 14 recordings") part of what the
    /// user is told, and that sentence is only auditable if the old name
    /// survives somewhere.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] for an unknown row,
    /// [`PersistenceError::Invalid`] for an empty or over-long name.
    pub async fn update(
        &self,
        id: PersonId,
        draft: &PersonDraft,
        at: Timestamp,
    ) -> PersistenceResult<StoredPerson> {
        validate_draft(draft)?;
        let aliases_json = encode_aliases(&draft.aliases)?;
        let mut tx = self.db.begin_write().await?;
        let previous: Option<String> =
            sqlx::query_scalar("SELECT display_name FROM people WHERE id = ?1")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        let previous = previous.ok_or(PersistenceError::RecordNotFound)?;
        sqlx::query(
            "UPDATE people
             SET display_name = ?2, aliases_json = ?3, email = ?4, organisation = ?5,
                 updated_at = ?6
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(&draft.display_name)
        .bind(&aliases_json)
        .bind(draft.email.as_deref())
        .bind(draft.organisation.as_deref())
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await?;
        if previous != draft.display_name {
            let recordings = linked_recording_count(&mut tx, id).await?;
            journal(
                &mut tx,
                id,
                VoiceProfileEventKind::PersonRenamed,
                &serde_json::json!({
                    "from": previous,
                    "to": draft.display_name,
                    "recordings_showing_the_name": recordings,
                })
                .to_string(),
                at,
            )
            .await?;
        }
        tx.commit().await?;
        self.get(id).await?.ok_or(PersistenceError::RecordNotFound)
    }

    /// How many recordings currently show this person, i.e. the blast radius of
    /// a rename (VP-213a).
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn linked_recordings(&self, id: PersonId) -> PersistenceResult<u32> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT recording_id) FROM speakers WHERE person_id = ?1",
        )
        .bind(id.to_string())
        .fetch_one(self.db.pool())
        .await?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Link a speaker row to a roster person and snapshot the name onto the row.
    ///
    /// This is the one assignment primitive. A confirmed suggestion and a
    /// manual pick both land here, which is what VP-215 means by "no forked
    /// manual implementation": with matching off, this path still works, and
    /// with matching on, nothing else writes a label.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] for an unknown speaker row,
    /// [`PersistenceError::Conflict`] for an unknown person.
    pub async fn assign_speaker(
        &self,
        speaker_id: SpeakerId,
        person_id: PersonId,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        let mut tx = self.db.begin_write().await?;
        let name: Option<String> = sqlx::query_scalar(
            "SELECT display_name FROM people WHERE id = ?1 AND status = 'active'",
        )
        .bind(person_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let name = name.ok_or(PersistenceError::Conflict)?;
        let recording_id: Option<String> =
            sqlx::query_scalar("SELECT recording_id FROM speakers WHERE id = ?1")
                .bind(speaker_id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        let recording_id = recording_id.ok_or(PersistenceError::RecordNotFound)?;
        sqlx::query(
            "UPDATE speakers
             SET person_id = ?2, name = ?3, label = ?3, provenance = 'user_confirmed',
                 user_confirmed = 1
             WHERE id = ?1",
        )
        .bind(speaker_id.to_string())
        .bind(person_id.to_string())
        .bind(&name)
        .execute(&mut *tx)
        .await?;
        journal(
            &mut tx,
            person_id,
            VoiceProfileEventKind::AssignmentSet,
            &serde_json::json!({
                "recording_id": recording_id,
                "speaker_id": speaker_id.to_string(),
            })
            .to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove a speaker row's roster link, keeping the name snapshot the
    /// recording already displayed.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] when the row has no link.
    pub async fn clear_assignment(
        &self,
        speaker_id: SpeakerId,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        let mut tx = self.db.begin_write().await?;
        let person: Option<Option<String>> =
            sqlx::query_scalar("SELECT person_id FROM speakers WHERE id = ?1")
                .bind(speaker_id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        let person = match person {
            None => return Err(PersistenceError::RecordNotFound),
            Some(None) => return Err(PersistenceError::RecordNotFound),
            Some(Some(raw)) => parse_person(&raw)?,
        };
        sqlx::query("UPDATE speakers SET person_id = NULL WHERE id = ?1")
            .bind(speaker_id.to_string())
            .execute(&mut *tx)
            .await?;
        journal(
            &mut tx,
            person,
            VoiceProfileEventKind::AssignmentCleared,
            &serde_json::json!({ "speaker_id": speaker_id.to_string() }).to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Speaker rows in a recording that are linked to a roster person.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn assignments_for_recording(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<(SpeakerId, PersonId)>> {
        let rows = sqlx::query(
            "SELECT id, person_id FROM speakers
             WHERE recording_id = ?1 AND person_id IS NOT NULL ORDER BY id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let speaker: String = row.try_get("id")?;
                let person: String = row.try_get("person_id")?;
                Ok((
                    speaker
                        .parse()
                        .map_err(|_| PersistenceError::IntegrityFailed)?,
                    parse_person(&person)?,
                ))
            })
            .collect()
    }

    /// What a merge of `loser` into `survivor` would move, without moving it.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn merge_preview(
        &self,
        loser: PersonId,
        survivor: PersonId,
    ) -> PersistenceResult<MergeOutcome> {
        let mut tx = self.db.begin_write().await?;
        let outcome = merge_counts(&mut tx, loser, survivor).await?;
        tx.rollback().await?;
        Ok(outcome)
    }

    /// Merge `loser` into `survivor`: repoint assignments, exemplars and
    /// suggestions, keep the loser as a tombstone, journal the counts.
    ///
    /// The enrolment follows the person: if only the loser was enrolled, its
    /// row and consent state move to the survivor, because the two rows always
    /// described the same human being — that is what "merge" asserts. If both
    /// were enrolled the survivor's consent record wins, keeping the *shorter*
    /// retention of the two, since VP-130 allows shortening and never
    /// lengthening.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] when either person is unknown,
    /// [`PersistenceError::Invalid`] when both ids are the same or the
    /// survivor is itself a tombstone.
    pub async fn merge(
        &self,
        loser: PersonId,
        survivor: PersonId,
        at: Timestamp,
    ) -> PersistenceResult<MergeOutcome> {
        if loser == survivor {
            return Err(PersistenceError::Invalid(
                "a person cannot be merged into themselves".to_owned(),
            ));
        }
        let mut tx = self.db.begin_write().await?;
        let survivor_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM people WHERE id = ?1")
                .bind(survivor.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        match survivor_status.as_deref() {
            None => return Err(PersistenceError::RecordNotFound),
            Some("active") => {}
            Some(_) => {
                return Err(PersistenceError::Invalid(
                    "the surviving person must be an active roster entry".to_owned(),
                ))
            }
        }
        let loser_exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM people WHERE id = ?1")
                .bind(loser.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        if loser_exists.is_none() {
            return Err(PersistenceError::RecordNotFound);
        }

        let outcome = merge_counts(&mut tx, loser, survivor).await?;
        let survivor_name: String =
            sqlx::query_scalar("SELECT display_name FROM people WHERE id = ?1")
                .bind(survivor.to_string())
                .fetch_one(&mut *tx)
                .await?;

        // The enrolment first: exemplars reference it, so it has to exist
        // before they can be repointed.
        let survivor_enrolled: Option<i64> = sqlx::query_scalar(
            "SELECT retention_months FROM person_voice_enrollments WHERE person_id = ?1",
        )
        .bind(survivor.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let loser_enrolment = sqlx::query(
            "SELECT consent_version, consent_note, retention_months, last_matched_at, status,
                    created_at
             FROM person_voice_enrollments WHERE person_id = ?1",
        )
        .bind(loser.to_string())
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(row) = loser_enrolment {
            let retention: i64 = row.try_get("retention_months")?;
            let last_matched: Option<i64> = row.try_get("last_matched_at")?;
            match survivor_enrolled {
                None => {
                    sqlx::query(
                        "INSERT INTO person_voice_enrollments
                             (person_id, consent_version, consent_note, retention_months,
                              last_matched_at, status, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    )
                    .bind(survivor.to_string())
                    .bind(row.try_get::<String, _>("consent_version")?)
                    .bind(row.try_get::<Option<String>, _>("consent_note")?)
                    .bind(retention)
                    .bind(last_matched)
                    .bind(row.try_get::<String, _>("status")?)
                    .bind(row.try_get::<i64, _>("created_at")?)
                    .execute(&mut *tx)
                    .await?;
                }
                Some(existing) => {
                    sqlx::query(
                        "UPDATE person_voice_enrollments
                         SET retention_months = MIN(?2, ?3),
                             last_matched_at = MAX(COALESCE(last_matched_at, 0), COALESCE(?4, 0))
                         WHERE person_id = ?1",
                    )
                    .bind(survivor.to_string())
                    .bind(existing)
                    .bind(retention)
                    .bind(last_matched)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            sqlx::query("UPDATE voice_exemplars SET person_id = ?2 WHERE person_id = ?1")
                .bind(loser.to_string())
                .bind(survivor.to_string())
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM person_voice_enrollments WHERE person_id = ?1")
                .bind(loser.to_string())
                .execute(&mut *tx)
                .await?;
        }

        // A suggestion that would collide with one the survivor already has for
        // the same (run, cluster) is redundant evidence about one person, and
        // the unique key forbids keeping both.
        sqlx::query(
            "DELETE FROM speaker_suggestions AS loser_row
             WHERE loser_row.person_id = ?1
               AND EXISTS (SELECT 1 FROM speaker_suggestions AS kept
                           WHERE kept.person_id = ?2
                             AND kept.diarization_run_id = loser_row.diarization_run_id
                             AND kept.cluster_key = loser_row.cluster_key)",
        )
        .bind(loser.to_string())
        .bind(survivor.to_string())
        .execute(&mut *tx)
        .await?;
        // A suggestion of the loser that was corrected *to* the survivor (or
        // the reverse) becomes a self-correction, which the schema forbids and
        // which carries no evidence once the two are one person.
        sqlx::query(
            "DELETE FROM speaker_suggestions
             WHERE (person_id = ?1 AND corrected_person_id = ?2)
                OR (person_id = ?2 AND corrected_person_id = ?1)",
        )
        .bind(loser.to_string())
        .bind(survivor.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE speaker_suggestions SET person_id = ?2 WHERE person_id = ?1")
            .bind(loser.to_string())
            .bind(survivor.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE speaker_suggestions SET corrected_person_id = ?2 WHERE corrected_person_id = ?1",
        )
        .bind(loser.to_string())
        .bind(survivor.to_string())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE speakers SET person_id = ?2, name = ?3, label = ?3 WHERE person_id = ?1",
        )
        .bind(loser.to_string())
        .bind(survivor.to_string())
        .bind(&survivor_name)
        .execute(&mut *tx)
        .await?;

        sqlx::query("UPDATE people SET status = 'merged', updated_at = ?2 WHERE id = ?1")
            .bind(loser.to_string())
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await?;

        let detail = serde_json::json!({
            "merged_person_id": loser.to_string(),
            "surviving_person_id": survivor.to_string(),
            "recordings_affected": outcome.recordings,
            "speaker_links_repointed": outcome.speaker_links,
            "exemplars_moved": outcome.exemplars,
            "pending_suggestions_repointed": outcome.pending_suggestions,
            "enrollment_moved": outcome.enrollment_moved,
        })
        .to_string();
        journal(
            &mut tx,
            survivor,
            VoiceProfileEventKind::PersonMerged,
            &detail,
            at,
        )
        .await?;
        journal(
            &mut tx,
            loser,
            VoiceProfileEventKind::PersonMerged,
            &detail,
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// Delete a roster entry.
    ///
    /// The enrolment and its exemplars and suggestions go with it; every
    /// `speakers` link is severed and every `speakers.name` snapshot stays, so
    /// the recordings keep displaying what they displayed before (VP-080,
    /// VP-213d). The journal keeps the counts, and keeps them after the row it
    /// describes is gone.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::RecordNotFound`] for an unknown row.
    pub async fn delete(&self, id: PersonId, at: Timestamp) -> PersistenceResult<PersonDeletion> {
        let mut tx = self.db.begin_write().await?;
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM people WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(PersistenceError::RecordNotFound);
        }
        let biometric = tier_counts(&mut tx, id).await?;
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM speakers WHERE person_id = ?1")
            .bind(id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        let recordings = linked_recording_count(&mut tx, id).await?;

        sqlx::query("DELETE FROM people WHERE id = ?1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;

        let outcome = PersonDeletion {
            severed_speaker_links: u32::try_from(links).unwrap_or(u32::MAX),
            recordings,
            exemplars: biometric.exemplars,
            suggestions: biometric.suggestions,
        };
        journal(
            &mut tx,
            id,
            VoiceProfileEventKind::PersonDeleted,
            &serde_json::json!({
                "speaker_links_severed": outcome.severed_speaker_links,
                "recordings_keeping_a_name_snapshot": outcome.recordings,
                "exemplars_destroyed": outcome.exemplars,
                "suggestions_destroyed": outcome.suggestions,
            })
            .to_string(),
            at,
        )
        .await?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// The journal of one person, oldest first. Survives the person's deletion,
    /// which is what makes it usable as the §12.3 consent record.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn events(&self, id: PersonId) -> PersistenceResult<Vec<StoredVoiceProfileEvent>> {
        let rows = sqlx::query(
            "SELECT id, person_id, kind, detail_json, created_at
             FROM voice_profile_events WHERE person_id = ?1 ORDER BY created_at, id",
        )
        .bind(id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let person_id: String = row.try_get("person_id")?;
                Ok(StoredVoiceProfileEvent {
                    id: row.try_get("id")?,
                    person_id: parse_person(&person_id)?,
                    kind: row.try_get("kind")?,
                    detail_json: row.try_get("detail_json")?,
                    created_at: Timestamp::from_millis(row.try_get::<i64, _>("created_at")?),
                })
            })
            .collect()
    }
}

async fn merge_counts(
    tx: &mut Transaction<'_, Sqlite>,
    loser: PersonId,
    survivor: PersonId,
) -> PersistenceResult<MergeOutcome> {
    let speaker_links: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM speakers WHERE person_id = ?1")
            .bind(loser.to_string())
            .fetch_one(&mut **tx)
            .await?;
    let recordings = linked_recording_count(tx, loser).await?;
    let exemplars: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM voice_exemplars WHERE person_id = ?1")
            .bind(loser.to_string())
            .fetch_one(&mut **tx)
            .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM speaker_suggestions WHERE person_id = ?1 AND state = 'pending'",
    )
    .bind(loser.to_string())
    .fetch_one(&mut **tx)
    .await?;
    let loser_enrolled: Option<String> =
        sqlx::query_scalar("SELECT person_id FROM person_voice_enrollments WHERE person_id = ?1")
            .bind(loser.to_string())
            .fetch_optional(&mut **tx)
            .await?;
    let survivor_enrolled: Option<String> =
        sqlx::query_scalar("SELECT person_id FROM person_voice_enrollments WHERE person_id = ?1")
            .bind(survivor.to_string())
            .fetch_optional(&mut **tx)
            .await?;
    Ok(MergeOutcome {
        speaker_links: u32::try_from(speaker_links).unwrap_or(u32::MAX),
        recordings,
        exemplars: u32::try_from(exemplars).unwrap_or(u32::MAX),
        pending_suggestions: u32::try_from(pending).unwrap_or(u32::MAX),
        enrollment_moved: loser_enrolled.is_some() && survivor_enrolled.is_none(),
    })
}

async fn linked_recording_count(
    tx: &mut Transaction<'_, Sqlite>,
    person_id: PersonId,
) -> PersistenceResult<u32> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT recording_id) FROM speakers WHERE person_id = ?1",
    )
    .bind(person_id.to_string())
    .fetch_one(&mut **tx)
    .await?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

fn validate_draft(draft: &PersonDraft) -> PersistenceResult<()> {
    let name = draft.display_name.trim();
    if name.is_empty() {
        return Err(PersistenceError::Invalid(
            "a roster entry needs a display name".to_owned(),
        ));
    }
    if draft.display_name.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(PersistenceError::Invalid(format!(
            "a display name is at most {MAX_DISPLAY_NAME_CHARS} characters"
        )));
    }
    if draft.aliases.iter().any(|alias| alias.trim().is_empty()) {
        return Err(PersistenceError::Invalid(
            "an alias cannot be blank".to_owned(),
        ));
    }
    Ok(())
}

fn encode_aliases(aliases: &[String]) -> PersistenceResult<String> {
    serde_json::to_string(aliases)
        .map_err(|error| PersistenceError::Invalid(format!("aliases are not encodable: {error}")))
}

fn decode_aliases(raw: &str) -> PersistenceResult<Vec<String>> {
    serde_json::from_str(raw).map_err(|_| PersistenceError::IntegrityFailed)
}

fn parse_person(raw: &str) -> PersistenceResult<PersonId> {
    raw.parse().map_err(|_| PersistenceError::IntegrityFailed)
}

fn decode_person(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<StoredPerson> {
    let id: String = row.try_get("id")?;
    let aliases: String = row.try_get("aliases_json")?;
    let status: String = row.try_get("status")?;
    Ok(StoredPerson {
        id: parse_person(&id)?,
        display_name: row.try_get("display_name")?,
        aliases: decode_aliases(&aliases)?,
        email: row.try_get("email")?,
        organisation: row.try_get("organisation")?,
        created_at: Timestamp::from_millis(row.try_get::<i64, _>("created_at")?),
        updated_at: Timestamp::from_millis(row.try_get::<i64, _>("updated_at")?),
        status: PersonStatus::parse(&status),
    })
}
