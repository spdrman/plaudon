//! The roster half of the voice-profiles feature: the master speaker list and
//! assignment (`VOICE_PROFILES_SPEC` §9A, VP-010, VP-011, VP-072, VP-080,
//! VP-210..VP-215, §12.2A).
//!
//! Nothing in this module reads the consent gate, asks the OS credential store
//! for a key, or can reach a speaker embedder: a roster entry is a typed name,
//! which is ordinary personal data and no statute's biometric definition
//! reaches it. That is why the people commands are a separate module from the
//! biometric ones — with matching off, or in a build with no embedding model
//! at all, every method here still works.
//!
//! Two places where the roster and the biometric tier do touch, both
//! deliberate:
//!
//! * **Deleting a person** destroys their enrolment, because the user asked
//!   for the colleague to be gone, not merely unenrolled. The disclosure
//!   counts both (VP-080).
//! * **Merging two people** moves one's exemplars onto the other, which means
//!   re-sealing each ciphertext under the survivor's AEAD binding
//!   ([`plaudon_app_core::voice_profiles::reseal_exemplars`]). That needs the
//!   data key, so a merge of an *enrolled* duplicate refuses while the
//!   credential store is locked rather than rewriting a column and leaving
//!   voiceprints nothing can decrypt. A merge of two name-only entries — the
//!   manual-only library of VP-011 — never touches a key at all.

use std::collections::BTreeMap;

use async_trait::async_trait;
use plaudon_app_core::voice_profiles::{reseal_exemplars, EmbeddingCipher};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerId};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::people::{PeopleRepository, PersonDraft, PersonStatus, StoredPerson};
use plaudon_persistence::speakers::{SpeakerRepository, StoredSpeaker};
use plaudon_persistence::voice_profiles::{VoiceEnrollmentRepository, VoiceProfileGateRepository};
use serde_json::json;

use crate::backend::LocalBackend;
use crate::dto::{
    AssignClusterPersonResponse, DeletePersonResponse, EventKind, ListPeopleResponse,
    MergeCandidateGroupDto, MergePeopleResponse, PersonDto, PersonRefDto, PersonVoiceDto,
    SpeakerDto,
};
use crate::events::EventDraft;
use crate::ports::{ClusterAssignment, PeoplePort};

/// Milliseconds in a month, for the §12.4 expiry countdown.
///
/// A thirty-day month: the retention clock is a bound, not a calendar
/// appointment, and VP-130 reads "12 months" as an outer limit the product
/// must not exceed — so the shorter arithmetic month is the safe rounding.
const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// Map a roster failure onto the application's error taxonomy.
pub(crate) fn people_error(error: plaudon_persistence::PersistenceError) -> AppError {
    use plaudon_persistence::PersistenceError as Error;
    match error {
        Error::RecordNotFound => AppError::new(AppErrorCode::RecordNotFound, "no such person"),
        Error::Conflict => AppError::new(
            AppErrorCode::Conflict,
            "that roster entry changed while this edit was in flight",
        ),
        Error::Invalid(detail) => AppError::new(AppErrorCode::InvalidArgument, detail),
        other => AppError::new(
            AppErrorCode::StorageUnavailable,
            format!("the people list is unavailable: {other}"),
        ),
    }
}

/// The name a dedupe group is keyed by: case-folded, whitespace-collapsed
/// (VP-214).
fn normalise_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// One speaker row as the wire describes it.
pub(crate) fn speaker_dto(row: &StoredSpeaker) -> SpeakerDto {
    SpeakerDto {
        speaker_id: row.id,
        recording_id: row.recording_id,
        label: row.label.clone(),
        name: row.name.clone(),
        confirmed: row.user_confirmed,
        original_label: row.original_label.clone(),
        renamed_at: row.renamed_at,
        confirmed_at: row.confirmed_at,
    }
}

impl LocalBackend {
    /// The roster repository.
    pub(crate) fn people(&self) -> PeopleRepository {
        PeopleRepository::new(self.store().db().clone())
    }

    /// The biometric-tier repository.
    pub(crate) fn voices(&self) -> VoiceEnrollmentRepository {
        VoiceEnrollmentRepository::new(self.store().db().clone())
    }

    /// The diarization and speaker-row repository.
    pub(crate) fn speaker_rows(&self) -> SpeakerRepository {
        SpeakerRepository::new(self.store().db().clone())
    }

    /// The consent gate as stored (§12.2).
    pub(crate) fn voice_gate(&self) -> VoiceProfileGateRepository {
        VoiceProfileGateRepository::new(self.store().db().clone())
    }

    /// The exemplar cipher over this library's key scope.
    pub(crate) fn embedding_cipher(&self) -> EmbeddingCipher {
        EmbeddingCipher::new(std::sync::Arc::clone(self.voice_secrets()))
    }

    /// One roster entry with the counts the People screen shows (VP-091).
    pub(crate) async fn person_dto(&self, person: &StoredPerson) -> Result<PersonDto, AppError> {
        let people = self.people();
        let voices = self.voices();
        let recordings = people
            .linked_recordings(person.id)
            .await
            .map_err(people_error)?;
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM speaker_suggestions WHERE person_id = ?1 AND state = 'pending'",
        )
        .bind(person.id.to_string())
        .fetch_one(self.pool())
        .await
        .map_err(|err| crate::backend::store::storage_error("cannot count suggestions", &err))?;

        let voice = match voices.enrollment(person.id).await.map_err(people_error)? {
            None => None,
            Some(enrolment) => {
                let exemplars = voices.exemplars(person.id).await.map_err(people_error)?;
                let mut recordings_seen: Vec<RecordingId> = exemplars
                    .iter()
                    .filter_map(|row| row.recording_id)
                    .collect();
                recordings_seen.sort_unstable();
                recordings_seen.dedup();
                // The clock runs from the last thing that used the enrolment,
                // which is what the retention sweep also measures (VP-130).
                let last_use = enrolment
                    .last_matched_at
                    .unwrap_or(enrolment.created_at)
                    .as_millis()
                    .max(
                        exemplars
                            .iter()
                            .map(|row| row.created_at.as_millis())
                            .max()
                            .unwrap_or(i64::MIN),
                    );
                Some(PersonVoiceDto {
                    status: enrolment.status.clone(),
                    exemplars: u32::try_from(exemplars.len()).unwrap_or(u32::MAX),
                    source_recordings: u32::try_from(recordings_seen.len()).unwrap_or(u32::MAX),
                    total_speech_ms: exemplars
                        .iter()
                        .map(|row| u32::try_from(row.speech_ms).unwrap_or(u32::MAX))
                        .fold(0u32, u32::saturating_add),
                    retention_months: enrolment.retention_months,
                    consent_version: enrolment.consent_version.clone(),
                    enrolled_at: enrolment.created_at,
                    last_matched_at: enrolment.last_matched_at,
                    expires_at: Some(Timestamp::from_millis(
                        last_use + i64::from(enrolment.retention_months) * MONTH_MS,
                    )),
                })
            }
        };

        Ok(PersonDto {
            person_id: person.id,
            display_name: person.display_name.clone(),
            aliases: person.aliases.clone(),
            email: person.email.clone(),
            organisation: person.organisation.clone(),
            status: match person.status {
                PersonStatus::Active => "active".to_owned(),
                PersonStatus::Merged => "merged".to_owned(),
            },
            created_at: person.created_at,
            updated_at: person.updated_at,
            recordings,
            pending_suggestions: u32::try_from(pending).unwrap_or(u32::MAX),
            voice,
        })
    }

    /// One roster entry by id.
    pub(crate) async fn person_dto_by_id(
        &self,
        person_id: PersonId,
    ) -> Result<PersonDto, AppError> {
        let person = self
            .people()
            .get(person_id)
            .await
            .map_err(people_error)?
            .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such person"))?;
        self.person_dto(&person).await
    }

    /// The speaker row of a recording, by id.
    pub(crate) async fn speaker_row(
        &self,
        recording_id: RecordingId,
        speaker_id: SpeakerId,
    ) -> Result<StoredSpeaker, AppError> {
        self.speaker_rows()
            .list_for_recording(recording_id)
            .await
            .map_err(people_error)?
            .into_iter()
            .find(|row| row.id == speaker_id)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::RecordNotFound,
                    "no such speaker in this recording",
                )
            })
    }

    /// The speaker row standing for one diarization cluster of a recording.
    pub(crate) async fn speaker_row_for_cluster(
        &self,
        recording_id: RecordingId,
        cluster_key: &str,
    ) -> Result<Option<StoredSpeaker>, AppError> {
        Ok(self
            .speaker_rows()
            .list_for_recording(recording_id)
            .await
            .map_err(people_error)?
            .into_iter()
            .find(|row| row.cluster_key.as_deref() == Some(cluster_key)))
    }

    /// Write one cluster's assignment: the roster link plus the cluster's
    /// segments, in one action (VP-032, VP-211, VP-212).
    ///
    /// With per-segment exceptions present and `overwrite` unset nothing is
    /// written and the count is reported, which is the "3 segments were set
    /// individually — keep or overwrite?" disclosure.
    pub(crate) async fn apply_cluster_assignment(
        &self,
        recording_id: RecordingId,
        speaker: &StoredSpeaker,
        person_id: PersonId,
        overwrite: bool,
        now: Timestamp,
    ) -> Result<AssignClusterPersonResponse, AppError> {
        let speakers = self.speaker_rows();
        let revision = self.head_revision(recording_id).await?;
        let run = speakers
            .latest_run(recording_id)
            .await
            .map_err(people_error)?;
        let cluster = speaker.cluster_key.clone();

        let mut exceptions = 0u32;
        if let (Some(revision_id), Some(run), Some(cluster_key)) =
            (revision, run.as_ref(), cluster.as_deref())
        {
            exceptions = speakers
                .cluster_segment_exceptions(revision_id, run.id, cluster_key, speaker.id)
                .await
                .map_err(people_error)?;
        }
        if exceptions > 0 && !overwrite {
            return Ok(AssignClusterPersonResponse {
                applied: false,
                speaker: speaker_dto(speaker),
                person: None,
                segments_assigned: 0,
                segment_exceptions: exceptions,
            });
        }

        self.people()
            .assign_speaker(speaker.id, person_id, now)
            .await
            .map_err(|error| match error {
                plaudon_persistence::PersistenceError::Conflict => AppError::new(
                    AppErrorCode::InvalidArgument,
                    "that person is not an active roster entry",
                ),
                other => people_error(other),
            })?;

        let mut segments_assigned = 0u32;
        if let (Some(revision_id), Some(run), Some(cluster_key)) =
            (revision, run.as_ref(), cluster.as_deref())
        {
            segments_assigned = speakers
                .assign_cluster_segments(revision_id, run.id, cluster_key, speaker.id)
                .await
                .map_err(people_error)?;
        }

        let refreshed = self.speaker_row(recording_id, speaker.id).await?;
        let person = self
            .people()
            .get(person_id)
            .await
            .map_err(people_error)?
            .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such person"))?;
        self.publish_person_changed(person_id).await;

        Ok(AssignClusterPersonResponse {
            applied: true,
            speaker: speaker_dto(&refreshed),
            person: Some(PersonRefDto {
                person_id: person.id,
                display_name: person.display_name,
            }),
            segments_assigned,
            segment_exceptions: exceptions,
        })
    }

    /// Announce that the roster changed. Ids only (VP-086, ADR-0021).
    pub(crate) async fn publish_person_changed(&self, person_id: PersonId) {
        self.events()
            .publish(
                EventDraft::new(EventKind::PersonChanged, Timestamp::now())
                    .with_hint("person_id", json!(person_id.to_string())),
            )
            .await;
    }
}

#[async_trait]
impl PeoplePort for LocalBackend {
    async fn list_people(&self, query: Option<&str>) -> Result<ListPeopleResponse, AppError> {
        let needle = query.map(|raw| raw.trim().to_lowercase());
        let mut people = Vec::new();
        for person in self.people().list_active().await.map_err(people_error)? {
            if let Some(needle) = needle.as_deref() {
                if !needle.is_empty() {
                    let matches = person.display_name.to_lowercase().contains(needle)
                        || person
                            .aliases
                            .iter()
                            .any(|alias| alias.to_lowercase().contains(needle))
                        || person
                            .email
                            .as_deref()
                            .is_some_and(|email| email.to_lowercase().contains(needle))
                        || person
                            .organisation
                            .as_deref()
                            .is_some_and(|org| org.to_lowercase().contains(needle));
                    if !matches {
                        continue;
                    }
                }
            }
            people.push(self.person_dto(&person).await?);
        }

        // Dedupe candidates come from the *whole* roster, not from the filtered
        // view: a search for "pri" must not invent or hide a duplicate pair
        // (VP-214). They are candidates only; nothing here merges anything.
        let mut groups: BTreeMap<String, Vec<PersonRefDto>> = BTreeMap::new();
        for person in self.people().list_active().await.map_err(people_error)? {
            let reference = PersonRefDto {
                person_id: person.id,
                display_name: person.display_name.clone(),
            };
            let mut keys = vec![normalise_name(&person.display_name)];
            keys.extend(person.aliases.iter().map(|alias| normalise_name(alias)));
            keys.retain(|key| !key.is_empty());
            keys.sort();
            keys.dedup();
            for key in keys {
                let entry = groups.entry(key).or_default();
                if !entry.iter().any(|existing| existing.person_id == person.id) {
                    entry.push(reference.clone());
                }
            }
        }
        let merge_candidates = groups
            .into_iter()
            .filter(|(_, members)| members.len() > 1)
            .map(|(normalised_name, people)| MergeCandidateGroupDto {
                normalised_name,
                people,
            })
            .collect();

        Ok(ListPeopleResponse {
            people,
            merge_candidates,
        })
    }

    async fn create_person(&self, draft: &PersonDraft) -> Result<PersonDto, AppError> {
        let person = self
            .people()
            .create(draft, Timestamp::now())
            .await
            .map_err(people_error)?;
        self.publish_person_changed(person.id).await;
        self.person_dto(&person).await
    }

    async fn update_person(
        &self,
        person_id: PersonId,
        draft: &PersonDraft,
    ) -> Result<PersonDto, AppError> {
        let person = self
            .people()
            .update(person_id, draft, Timestamp::now())
            .await
            .map_err(people_error)?;
        self.publish_person_changed(person.id).await;
        self.person_dto(&person).await
    }

    async fn merge_people(
        &self,
        loser: PersonId,
        survivor: PersonId,
        apply: bool,
    ) -> Result<MergePeopleResponse, AppError> {
        let people = self.people();
        let preview = people
            .merge_preview(loser, survivor)
            .await
            .map_err(people_error)?;
        if !apply {
            return Ok(MergePeopleResponse {
                applied: false,
                speaker_links: preview.speaker_links,
                recordings: preview.recordings,
                exemplars: preview.exemplars,
                pending_suggestions: preview.pending_suggestions,
                enrollment_moved: preview.enrollment_moved,
                survivor: None,
            });
        }

        // Exemplars move only by being re-sealed under the survivor's binding;
        // with nobody enrolled this is an empty slice and no key is touched.
        let moving = self.voices().exemplars(loser).await.map_err(people_error)?;
        let resealed = if moving.is_empty() {
            Vec::new()
        } else {
            reseal_exemplars(&self.embedding_cipher(), &moving, survivor).map_err(AppError::from)?
        };

        let outcome = people
            .merge(loser, survivor, &resealed, Timestamp::now())
            .await
            .map_err(people_error)?;
        self.publish_person_changed(survivor).await;
        self.publish_person_changed(loser).await;

        Ok(MergePeopleResponse {
            applied: true,
            speaker_links: outcome.speaker_links,
            recordings: outcome.recordings,
            exemplars: outcome.exemplars,
            pending_suggestions: outcome.pending_suggestions,
            enrollment_moved: outcome.enrollment_moved,
            survivor: Some(self.person_dto_by_id(survivor).await?),
        })
    }

    async fn delete_person(
        &self,
        person_id: PersonId,
        apply: bool,
    ) -> Result<DeletePersonResponse, AppError> {
        let people = self.people();
        if people.get(person_id).await.map_err(people_error)?.is_none() {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "no such person",
            ));
        }
        if !apply {
            let recordings = people
                .linked_recordings(person_id)
                .await
                .map_err(people_error)?;
            let links: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM speakers WHERE person_id = ?1")
                    .bind(person_id.to_string())
                    .fetch_one(self.pool())
                    .await
                    .map_err(|err| {
                        crate::backend::store::storage_error("cannot count assignments", &err)
                    })?;
            let voices = self.voices();
            let exemplars = voices
                .exemplars(person_id)
                .await
                .map_err(people_error)?
                .len();
            let suggestions: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM speaker_suggestions WHERE person_id = ?1")
                    .bind(person_id.to_string())
                    .fetch_one(self.pool())
                    .await
                    .map_err(|err| {
                        crate::backend::store::storage_error("cannot count suggestions", &err)
                    })?;
            return Ok(DeletePersonResponse {
                applied: false,
                severed_speaker_links: u32::try_from(links).unwrap_or(u32::MAX),
                recordings,
                exemplars: u32::try_from(exemplars).unwrap_or(u32::MAX),
                suggestions: u32::try_from(suggestions).unwrap_or(u32::MAX),
            });
        }

        let outcome = people
            .delete(person_id, Timestamp::now())
            .await
            .map_err(people_error)?;
        self.publish_person_changed(person_id).await;
        Ok(DeletePersonResponse {
            applied: true,
            severed_speaker_links: outcome.severed_speaker_links,
            recordings: outcome.recordings,
            exemplars: outcome.exemplars,
            suggestions: outcome.suggestions,
        })
    }

    async fn assign_cluster_person(
        &self,
        assignment: &ClusterAssignment,
    ) -> Result<AssignClusterPersonResponse, AppError> {
        let speaker = self
            .speaker_row(assignment.recording_id, assignment.speaker_id)
            .await?;
        let Some(person_id) = assignment.person_id else {
            // "Unknown/none" (VP-210): the link goes and the row returns to
            // its machine label. The transcript keeps its segment links, which
            // point at this row and now read as the neutral cluster again.
            let previous = speaker.cluster_key.clone();
            let speakers = self.speaker_rows();
            match self
                .people()
                .clear_assignment(speaker.id, Timestamp::now())
                .await
            {
                Ok(()) | Err(plaudon_persistence::PersistenceError::RecordNotFound) => {}
                Err(other) => return Err(people_error(other)),
            }
            speakers.reset(speaker.id).await.map_err(people_error)?;
            let refreshed = self
                .speaker_row(assignment.recording_id, assignment.speaker_id)
                .await?;
            debug_assert_eq!(refreshed.cluster_key, previous);
            return Ok(AssignClusterPersonResponse {
                applied: true,
                speaker: speaker_dto(&refreshed),
                person: None,
                segments_assigned: 0,
                segment_exceptions: 0,
            });
        };

        self.apply_cluster_assignment(
            assignment.recording_id,
            &speaker,
            person_id,
            assignment.overwrite_segment_exceptions,
            Timestamp::now(),
        )
        .await
    }
}
