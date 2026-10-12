//! The biometric tier: the consent gate, enrolment, suggestions and erasure
//! (`VOICE_PROFILES_SPEC` §8, §12.2, VP-003, VP-011, VP-085, VP-094, VP-095,
//! VP-120..VP-123, VP-215, VP-216).
//!
//! # Where the gate is
//!
//! Every method here that could lead to an embedding starts by reading the
//! stored gate and asking
//! [`authorise_biometric_work`](plaudon_app_core::voice_profiles::authorise_biometric_work)
//! for a [`ConsentProof`](plaudon_app_core::voice_profiles::ConsentProof).
//! The enrolment service's embedding functions *take* that proof, so a closed
//! gate is not a check somebody remembered to write — it is the reason the
//! call does not type-check without one. The roster module beside this one
//! never obtains a proof, which is VP-011's structural claim, and the runtime
//! proof is a scripted embedder that panics if it is ever called while the
//! gate is closed (§17.1).
//!
//! # What a response may carry
//!
//! Scores, bands, person ids and counts. Never a vector, never key material:
//! `plaudon_security::response::assert_no_secret_in_response` would refuse an
//! embedding-shaped field, and no DTO here has one (VP-085).

use async_trait::async_trait;
use plaudon_app_core::voice_profiles::{
    authorise_biometric_work, cluster_clean_speech, gate_state, ConsentError, ConsentGateState,
    ConsentProof, DisclosureReason, EnrolError, EnrolmentSource, VoiceEnrolmentService,
    VoiceProfileEraser, ACTIVATION_DISCLOSURE, ENROL_SPEECH_MS, EXEMPLAR_SPEECH_MS,
};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, PersonId, RecordingId, SpeakerSuggestionId};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::people::PersonDraft;
use plaudon_persistence::voice_profiles::{StoredSuggestion, VoiceProfileGate};
use serde_json::json;

use crate::backend::people::people_error;
use crate::backend::processing::speaker_match::an_embedder_is_installed;
use crate::backend::LocalBackend;
use crate::dto::{
    ActivationDisclosureDto, ClusterSpeechDto, ConsentGateDto, DeleteVoiceEnrollmentResponse,
    EventKind, ForgetAllVoiceProfilesResponse, ListSpeakerSuggestionsResponse,
    ListVoiceExemplarsResponse, PersonRefDto, SpeakerMatchJobDto, SpeakerMatchRefusalDto,
    SpeakerSuggestionDecisionDto, SpeakerSuggestionDto, SplitVoiceEnrollmentResponse,
    StartSpeakerMatchingResponse, VoiceEnrollmentDto, VoiceExemplarDto, VoiceProfileSettingsDto,
};
use crate::events::EventDraft;
use crate::ports::{EnrolmentRequest, SplitRequest, SplitTarget, VoiceProfilesPort};

/// Default page size of the review queue (VP-093).
///
/// The queue virtualises, so the first page is what a frame needs; a caller
/// that wants more asks for more.
pub const SUGGESTION_PAGE_DEFAULT: u32 = 200;

/// Largest page the queue will return.
pub const SUGGESTION_PAGE_MAX: u32 = 1_000;

/// Milliseconds in the thirty-day month the retention clock counts in.
const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// Map a consent refusal onto the application's taxonomy.
///
/// `CapabilityUnverified` rather than `PermissionDenied`: the condition is
/// something the *user* can change in settings, and the message says which
/// one it is.
fn consent_error(error: &ConsentError) -> AppError {
    let detail = match error {
        ConsentError::MatchingOff => {
            "voice matching is off; the people list works without it".to_owned()
        }
        ConsentError::DisclosureRequired(DisclosureReason::NeverAccepted) => {
            "the voice-matching disclosure has not been accepted".to_owned()
        }
        ConsentError::DisclosureRequired(DisclosureReason::VersionChanged { .. }) => {
            "the voice-matching disclosure changed and has to be accepted again".to_owned()
        }
        ConsentError::DisclosureRequired(DisclosureReason::RestoredLibrary) => {
            "restored voice profiles need the consent gate in this library".to_owned()
        }
        ConsentError::NotAttested => {
            "an enrolment needs your attestation that this person has agreed".to_owned()
        }
        other => other.to_string(),
    };
    AppError::new(AppErrorCode::CapabilityUnverified, detail)
}

/// Map an enrolment failure onto the application's taxonomy.
fn enrol_error(error: EnrolError) -> AppError {
    match error {
        EnrolError::Consent(inner) => consent_error(&inner),
        EnrolError::NotEnoughSpeech(inner) => {
            AppError::new(AppErrorCode::InvalidArgument, inner.to_string())
        }
        EnrolError::UnknownCluster { .. } | EnrolError::UnknownPerson => {
            AppError::new(AppErrorCode::RecordNotFound, error.to_string())
        }
        EnrolError::UnknownExemplar => AppError::new(
            AppErrorCode::RecordNotFound,
            "no such exemplar in this enrolment",
        ),
        EnrolError::AlreadyEnrolled | EnrolError::NotEnrolled => {
            AppError::new(AppErrorCode::Conflict, error.to_string())
        }
        EnrolError::InvalidSplit(detail) => AppError::new(AppErrorCode::InvalidArgument, detail),
        EnrolError::Embed(inner) => AppError::new(
            AppErrorCode::ModelUnavailable,
            format!("the speaker-embedding model could not run: {inner}"),
        ),
        EnrolError::Store(inner) => AppError::from(inner),
        EnrolError::Persistence(inner) => people_error(inner),
        EnrolError::CorruptReconciliation(detail) => AppError::new(
            AppErrorCode::InternalError,
            format!("the stored speaker reconciliation is unusable: {detail}"),
        ),
    }
}

impl LocalBackend {
    /// The gate as stored, which is the one source of truth for §12.2.
    pub(crate) async fn voice_profile_gate(&self) -> Result<VoiceProfileGate, AppError> {
        self.voice_gate().load().await.map_err(people_error)
    }

    /// Authorise biometric work, or refuse with the condition the user can
    /// act on.
    async fn authorise(&self) -> Result<ConsentProof, AppError> {
        let gate = self.voice_profile_gate().await?;
        authorise_biometric_work(&gate).map_err(|error| consent_error(&error))
    }

    /// The enrolment service, or the honest refusal that this build has no
    /// speaker-embedding model.
    fn enrolment_service(&self) -> Result<VoiceEnrolmentService, AppError> {
        let embedder = self.processing().voice_embedder().ok_or_else(|| {
            AppError::new(
                AppErrorCode::ModelUnavailable,
                "this build has no speaker-embedding model, so nobody can be enrolled",
            )
        })?;
        Ok(VoiceEnrolmentService::new(
            self.voices(),
            self.speaker_rows(),
            self.embedding_cipher(),
            embedder,
        ))
    }

    /// Resolve the (recording, cluster) pair an enrolment reads speech from.
    async fn enrolment_source(
        &self,
        recording_id: RecordingId,
        cluster_key: &str,
    ) -> Result<EnrolmentSource, AppError> {
        let run = self
            .speaker_rows()
            .latest_run(recording_id)
            .await
            .map_err(people_error)?
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::RecordNotFound,
                    "this recording has no diarization run to take speech from",
                )
            })?;
        let media = self.media_facts(recording_id).await?;
        let revision = self.domain_revision(recording_id, None).await?;
        Ok(EnrolmentSource {
            recording_id,
            diarization_run_id: run.id,
            revision,
            cluster_key: cluster_key.to_owned(),
            media_path: media.path,
        })
    }

    /// The enrolment as the wire describes it, after a store.
    async fn enrollment_dto(
        &self,
        outcome: &plaudon_app_core::voice_profiles::ExemplarOutcome,
    ) -> Result<VoiceEnrollmentDto, AppError> {
        let enrolment = self
            .voices()
            .enrollment(outcome.person_id)
            .await
            .map_err(people_error)?
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::InternalError,
                    "the enrolment vanished between the write and the read",
                )
            })?;
        let last_use = enrolment
            .last_matched_at
            .unwrap_or(enrolment.created_at)
            .as_millis();
        Ok(VoiceEnrollmentDto {
            person_id: outcome.person_id,
            exemplar_id: outcome.exemplar_id,
            exemplars: outcome.exemplars,
            speech_ms: outcome.speech_ms,
            total_speech_ms: outcome.total_speech_ms,
            source_recordings: outcome.source_recordings,
            model_id: outcome.model_id.clone(),
            model_version: outcome.model_version.clone(),
            retention_months: enrolment.retention_months,
            expires_at: Some(Timestamp::from_millis(
                last_use + i64::from(enrolment.retention_months) * MONTH_MS,
            )),
        })
    }

    /// One suggestion as the wire describes it: a score and a band, with the
    /// person it is about named so the panel can offer Confirm / Not them.
    async fn suggestion_dto(
        &self,
        row: &StoredSuggestion,
    ) -> Result<Option<SpeakerSuggestionDto>, AppError> {
        let Some(person) = self
            .people()
            .get(row.person_id)
            .await
            .map_err(people_error)?
        else {
            // A suggestion whose person is gone is not shown: the row cascades
            // with the person, so this is only reachable mid-deletion.
            return Ok(None);
        };
        let speaker = self
            .speaker_row_for_cluster(row.recording_id, &row.cluster_key)
            .await?;
        let title: Option<String> =
            sqlx::query_scalar("SELECT title FROM recordings WHERE id = ?1 AND deleted_at IS NULL")
                .bind(row.recording_id.to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(|err| {
                    crate::backend::store::storage_error("cannot read a recording", &err)
                })?;

        Ok(Some(SpeakerSuggestionDto {
            suggestion_id: row.id,
            recording_id: row.recording_id,
            recording_title: title,
            diarization_run_id: row.diarization_run_id.to_string(),
            cluster_key: row.cluster_key.clone(),
            speaker_id: speaker.as_ref().map(|speaker| speaker.id),
            speaker_label: speaker.as_ref().map(|speaker| speaker.label.clone()),
            person: PersonRefDto {
                person_id: person.id,
                display_name: person.display_name,
            },
            score: row.score,
            score_kind: row.score_kind.clone(),
            band: row.band.clone(),
            state: row.state.clone(),
            created_at: row.created_at,
        }))
    }

    /// Assign the person a settled suggestion names, through the ordinary
    /// assignment path (VP-072, VP-216).
    async fn assign_from_suggestion(
        &self,
        row: &StoredSuggestion,
        person_id: PersonId,
        overwrite: bool,
    ) -> Result<
        (
            bool,
            Option<crate::dto::SpeakerDto>,
            Option<PersonRefDto>,
            Vec<u32>,
            u32,
        ),
        AppError,
    > {
        let Some(speaker) = self
            .speaker_row_for_cluster(row.recording_id, &row.cluster_key)
            .await?
        else {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "this recording has no speaker row for that cluster",
            ));
        };
        let applied = self
            .apply_cluster_assignment(
                row.recording_id,
                &speaker,
                person_id,
                overwrite,
                Timestamp::now(),
            )
            .await?;
        Ok((
            applied.applied,
            Some(applied.speaker),
            applied.person,
            applied.assigned_segments,
            applied.segment_exceptions,
        ))
    }

    /// Announce that a recording's suggestions changed. Ids and counts only
    /// (VP-086).
    pub(crate) async fn publish_suggestions_ready(
        &self,
        recording_id: RecordingId,
        pending_count: u32,
    ) {
        self.events()
            .publish(
                EventDraft::new(EventKind::VoiceProfileSuggestionsReady, Timestamp::now())
                    .with_recording(recording_id)
                    .with_hint("pending_count", json!(pending_count)),
            )
            .await;
    }

    /// The settings block's counts (VP-090).
    async fn settings_dto(&self) -> Result<VoiceProfileSettingsDto, AppError> {
        let gate = self.voice_profile_gate().await?;
        let state = gate_state(&gate);
        let voices = self.voices();
        let totals = voices.tier_totals().await.map_err(people_error)?;
        let pending: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM speaker_suggestions WHERE state = 'pending'")
                .fetch_one(self.pool())
                .await
                .map_err(|err| {
                    crate::backend::store::storage_error("cannot count suggestions", &err)
                })?;
        let reason = match &state {
            ConsentGateState::DisclosureRequired { reason } => Some(
                match reason {
                    DisclosureReason::NeverAccepted => "never_accepted",
                    DisclosureReason::VersionChanged { .. } => "version_changed",
                    DisclosureReason::RestoredLibrary => "restored_library",
                }
                .to_owned(),
            ),
            _ => None,
        };

        Ok(VoiceProfileSettingsDto {
            enabled: gate.enabled,
            consent: ConsentGateDto {
                state: state.as_str().to_owned(),
                reason,
                accepted_version: gate.consent_version.clone(),
                accepted_at: gate.consent_recorded_at.map(Timestamp::from_millis),
                required_version: ACTIVATION_DISCLOSURE.version.to_owned(),
            },
            disclosure: ActivationDisclosureDto {
                version: ACTIVATION_DISCLOSURE.version.to_owned(),
                title: ACTIVATION_DISCLOSURE.title.to_owned(),
                body: ACTIVATION_DISCLOSURE
                    .body
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
                two_tier_sentence: ACTIVATION_DISCLOSURE.two_tier_sentence.to_owned(),
                responsibility_sentence: ACTIVATION_DISCLOSURE.responsibility_sentence.to_owned(),
                retention_default_months: ACTIVATION_DISCLOSURE.retention_default_months,
            },
            enrolled_people: totals.enrollments,
            exemplars: totals.exemplars,
            pending_suggestions: u32::try_from(pending).unwrap_or(u32::MAX),
            roster_people: totals.people_kept,
            key_available: self.embedding_cipher().check_available().is_ok(),
            embedder_available: an_embedder_is_installed(self),
            enrol_speech_ms: ENROL_SPEECH_MS,
            exemplar_speech_ms: EXEMPLAR_SPEECH_MS,
        })
    }
}

#[async_trait]
impl VoiceProfilesPort for LocalBackend {
    async fn voice_profile_settings(&self) -> Result<VoiceProfileSettingsDto, AppError> {
        self.settings_dto().await
    }

    async fn set_voice_profiles_enabled(
        &self,
        enabled: bool,
        accepted_disclosure_version: Option<&str>,
    ) -> Result<VoiceProfileSettingsDto, AppError> {
        let now = Timestamp::now();
        if enabled {
            // Turning the feature on *is* passing the gate: an activation with
            // no acceptance, or with a stale one, is refused rather than
            // recorded (VP-120, VP-121).
            let accepted = accepted_disclosure_version.unwrap_or_default();
            if accepted != ACTIVATION_DISCLOSURE.version {
                return Err(AppError::new(
                    AppErrorCode::InvalidArgument,
                    format!(
                        "voice matching turns on by accepting disclosure {}",
                        ACTIVATION_DISCLOSURE.version
                    ),
                ));
            }
            self.voice_gate()
                .activate(ACTIVATION_DISCLOSURE.version, now)
                .await
                .map_err(people_error)?;
        } else {
            self.voice_gate().disable(now).await.map_err(people_error)?;
        }
        self.settings_dto().await
    }

    async fn enroll_person_voice(
        &self,
        request: &EnrolmentRequest,
    ) -> Result<VoiceEnrollmentDto, AppError> {
        let proof = self.authorise().await?;
        let attestation = request.attestation.as_ref().ok_or_else(|| {
            AppError::new(
                AppErrorCode::InvalidArgument,
                "an enrolment needs the per-enrolment attestation",
            )
        })?;
        let service = self.enrolment_service()?;
        let source = self
            .enrolment_source(request.recording_id, &request.cluster_key)
            .await?;
        let ctx = plaudon_ai_contracts::job::JobContext::new(JobId::new());
        let outcome = service
            .enroll(
                &proof,
                request.person_id,
                attestation,
                &source,
                &ctx,
                Timestamp::now(),
            )
            .await
            .map_err(enrol_error)?;
        self.publish_person_changed(request.person_id).await;
        self.enrollment_dto(&outcome).await
    }

    async fn add_voice_exemplar(
        &self,
        request: &EnrolmentRequest,
    ) -> Result<VoiceEnrollmentDto, AppError> {
        let proof = self.authorise().await?;
        let service = self.enrolment_service()?;
        let source = self
            .enrolment_source(request.recording_id, &request.cluster_key)
            .await?;
        let ctx = plaudon_ai_contracts::job::JobContext::new(JobId::new());
        let outcome = service
            .add_exemplar(&proof, request.person_id, &source, &ctx, Timestamp::now())
            .await
            .map_err(enrol_error)?;
        self.publish_person_changed(request.person_id).await;
        self.enrollment_dto(&outcome).await
    }

    async fn split_voice_enrollment(
        &self,
        request: &SplitRequest,
    ) -> Result<SplitVoiceEnrollmentResponse, AppError> {
        let proof = self.authorise().await?;
        let service = self.enrolment_service()?;
        let now = Timestamp::now();
        let target = match &request.target {
            SplitTarget::Existing(person_id) => *person_id,
            SplitTarget::New(display_name) => {
                self.people()
                    .create(&PersonDraft::named(display_name.clone()), now)
                    .await
                    .map_err(people_error)?
                    .id
            }
        };
        let outcome = service
            .split(
                &proof,
                request.person_id,
                target,
                &request.exemplar_ids,
                &request.attestation,
                now,
            )
            .await
            .map_err(enrol_error)?;
        self.publish_person_changed(request.person_id).await;
        self.publish_person_changed(target).await;
        Ok(SplitVoiceEnrollmentResponse {
            source_person_id: outcome.source_person_id,
            target_person_id: outcome.target_person_id,
            exemplars_moved: outcome.exemplars_moved,
            exemplars_remaining: outcome.exemplars_remaining,
            target: self.person_dto_by_id(target).await?,
        })
    }

    async fn list_voice_exemplars(
        &self,
        person_id: PersonId,
    ) -> Result<ListVoiceExemplarsResponse, AppError> {
        // Not gated, for the same reason deletion is not (VP-123): seeing
        // what a stored profile is made of is how a user repairs or destroys
        // it, and a stale disclosure must not stand between them and that.
        // Nothing here reaches the embedder, and no response field carries a
        // vector (VP-085).
        let voices = self.voices();
        if voices
            .enrollment(person_id)
            .await
            .map_err(people_error)?
            .is_none()
        {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "this person has no voice enrolment",
            ));
        }
        let rows = voices.exemplars(person_id).await.map_err(people_error)?;
        let mut recordings: Vec<RecordingId> =
            rows.iter().filter_map(|row| row.recording_id).collect();
        recordings.sort_unstable();
        recordings.dedup();

        let mut titles: Vec<(RecordingId, Option<String>)> = Vec::with_capacity(recordings.len());
        for recording_id in &recordings {
            let title: Option<String> = sqlx::query_scalar(
                "SELECT title FROM recordings WHERE id = ?1 AND deleted_at IS NULL",
            )
            .bind(recording_id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(|err| crate::backend::store::storage_error("cannot read a recording", &err))?
            .flatten();
            titles.push((*recording_id, title));
        }

        let exemplars: Vec<VoiceExemplarDto> = rows
            .iter()
            .map(|row| VoiceExemplarDto {
                exemplar_id: row.id,
                recording_id: row.recording_id,
                recording_title: row.recording_id.and_then(|id| {
                    titles
                        .iter()
                        .find(|(held, _)| *held == id)
                        .and_then(|(_, title)| title.clone())
                }),
                cluster_key: row.cluster_key.clone(),
                source_start_ms: row.source_start_ms,
                source_end_ms: row.source_end_ms,
                speech_ms: u32::try_from(row.speech_ms).unwrap_or(u32::MAX),
                model_id: row.model_id.clone(),
                model_version: row.model_version.clone(),
                created_at: row.created_at,
            })
            .collect();

        Ok(ListVoiceExemplarsResponse {
            person_id,
            total_speech_ms: exemplars
                .iter()
                .map(|exemplar| exemplar.speech_ms)
                .fold(0u32, u32::saturating_add),
            source_recordings: u32::try_from(recordings.len()).unwrap_or(u32::MAX),
            exemplars,
        })
    }

    async fn cluster_speech(
        &self,
        recording_id: RecordingId,
        cluster_key: &str,
    ) -> Result<ClusterSpeechDto, AppError> {
        // Ungated on purpose: the meter is what the user looks at *while*
        // deciding whether to enrol, so it has to answer before the gate does
        // (VP-094). It reads the stored reconciliation — no audio is opened
        // and no model is called, which is why no `ConsentProof` exists in
        // this path at all.
        let run = self
            .speaker_rows()
            .latest_run(recording_id)
            .await
            .map_err(people_error)?
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::RecordNotFound,
                    "this recording has no diarization run to measure speech in",
                )
            })?;
        let revision = self.domain_revision(recording_id, None).await?;
        let speech = cluster_clean_speech(&self.speaker_rows(), &revision, run.id, cluster_key)
            .await
            .map_err(enrol_error)?;
        Ok(ClusterSpeechDto {
            recording_id,
            cluster_key: speech.cluster_key,
            clean_speech_ms: speech.clean_speech_ms,
            spans: u32::try_from(speech.spans.len()).unwrap_or(u32::MAX),
            first_span_start_ms: speech.spans.first().map(|(start, _)| *start),
            last_span_end_ms: speech.spans.last().map(|(_, end)| *end),
            enrol_speech_ms: ENROL_SPEECH_MS,
            exemplar_speech_ms: EXEMPLAR_SPEECH_MS,
            meets_enrol_minimum: speech.clean_speech_ms >= ENROL_SPEECH_MS,
            meets_exemplar_minimum: speech.clean_speech_ms >= EXEMPLAR_SPEECH_MS,
        })
    }

    async fn delete_voice_enrollment(
        &self,
        person_id: PersonId,
        apply: bool,
        reason: Option<&str>,
    ) -> Result<DeleteVoiceEnrollmentResponse, AppError> {
        // Deleting a voiceprint is never gated: a user who turned the feature
        // off, or whose disclosure went stale, must still be able to destroy
        // what was collected (VP-123). Only *creating* biometric data needs
        // the gate.
        let voices = self.voices();
        if voices
            .enrollment(person_id)
            .await
            .map_err(people_error)?
            .is_none()
        {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "this person has no voice enrolment",
            ));
        }
        if !apply {
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
            return Ok(DeleteVoiceEnrollmentResponse {
                applied: false,
                exemplars: u32::try_from(exemplars).unwrap_or(u32::MAX),
                suggestions: u32::try_from(suggestions).unwrap_or(u32::MAX),
                person_kept: true,
                person: self.person_dto_by_id(person_id).await?,
            });
        }

        let outcome = voices
            .delete_enrollment(person_id, Timestamp::now())
            .await
            .map_err(people_error)?;
        if let Some(reason) = reason {
            tracing::info!(
                reason_length = reason.len(),
                "a voice enrolment was deleted with a recorded reason"
            );
        }
        self.publish_person_changed(person_id).await;
        Ok(DeleteVoiceEnrollmentResponse {
            applied: true,
            exemplars: outcome.exemplars,
            suggestions: outcome.suggestions,
            person_kept: true,
            person: self.person_dto_by_id(person_id).await?,
        })
    }

    async fn forget_all_voice_profiles(
        &self,
        apply: bool,
    ) -> Result<ForgetAllVoiceProfilesResponse, AppError> {
        let eraser = VoiceProfileEraser::new(self.store().db().clone(), self.embedding_cipher());
        if !apply {
            let disclosure = eraser.disclose().await.map_err(AppError::from)?;
            return Ok(ForgetAllVoiceProfilesResponse {
                applied: false,
                enrollments: disclosure.enrollments,
                exemplars: disclosure.exemplars,
                suggestions: disclosure.suggestions,
                people_kept: disclosure.people_kept,
                key_present: disclosure.key_present,
                key_destroyed: false,
                feature_disabled: false,
                limits: disclosure.limits,
            });
        }
        let receipt = eraser
            .forget_all(Timestamp::now())
            .await
            .map_err(AppError::from)?;
        Ok(ForgetAllVoiceProfilesResponse {
            applied: true,
            enrollments: receipt.enrollments_destroyed,
            exemplars: receipt.exemplars_destroyed,
            suggestions: receipt.suggestions_destroyed,
            people_kept: receipt.people_kept,
            key_present: false,
            key_destroyed: receipt.key_destroyed,
            feature_disabled: receipt.feature_disabled,
            limits: plaudon_app_core::voice_profiles::FORGET_ALL_LIMITS
                .iter()
                .map(|line| (*line).to_owned())
                .collect(),
        })
    }

    async fn list_speaker_suggestions(
        &self,
        recording_id: Option<RecordingId>,
        limit: u32,
    ) -> Result<ListSpeakerSuggestionsResponse, AppError> {
        let voices = self.voices();
        let rows = match recording_id {
            Some(recording_id) => voices
                .pending_for_recording(recording_id)
                .await
                .map_err(people_error)?,
            None => voices
                .pending_queue(limit.clamp(1, SUGGESTION_PAGE_MAX))
                .await
                .map_err(people_error)?,
        };
        let mut suggestions = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(dto) = self.suggestion_dto(row).await? {
                suggestions.push(dto);
            }
        }
        let pending_count = u32::try_from(suggestions.len()).unwrap_or(u32::MAX);
        Ok(ListSpeakerSuggestionsResponse {
            suggestions,
            pending_count,
        })
    }

    async fn confirm_speaker_suggestion(
        &self,
        suggestion_id: SpeakerSuggestionId,
        overwrite_segment_exceptions: bool,
    ) -> Result<SpeakerSuggestionDecisionDto, AppError> {
        let voices = self.voices();
        let row = voices
            .suggestion(suggestion_id)
            .await
            .map_err(people_error)?
            .ok_or_else(|| {
                AppError::new(AppErrorCode::RecordNotFound, "no such speaker suggestion")
            })?;
        if row.state != "pending" {
            return Err(AppError::new(
                AppErrorCode::Conflict,
                "this suggestion has already been settled",
            ));
        }

        // The assignment is attempted first: with per-segment exceptions and
        // no overwrite the answer is a disclosure, and settling the suggestion
        // then would record a decision the user has not made yet (VP-211).
        let (applied, speaker, person, assigned_segments, segment_exceptions) = self
            .assign_from_suggestion(&row, row.person_id, overwrite_segment_exceptions)
            .await?;
        if !applied {
            return Ok(SpeakerSuggestionDecisionDto {
                suggestion_id,
                state: row.state,
                applied: false,
                speaker,
                person: None,
                assigned_segments,
                segment_exceptions,
            });
        }

        let settled = voices
            .confirm_suggestion(suggestion_id, Timestamp::now())
            .await
            .map_err(people_error)?;
        Ok(SpeakerSuggestionDecisionDto {
            suggestion_id,
            state: settled.state,
            applied: true,
            speaker,
            person,
            assigned_segments,
            segment_exceptions,
        })
    }

    async fn reject_speaker_suggestion(
        &self,
        suggestion_id: SpeakerSuggestionId,
        corrected_person_id: Option<PersonId>,
        overwrite_segment_exceptions: bool,
    ) -> Result<SpeakerSuggestionDecisionDto, AppError> {
        let voices = self.voices();
        let row = voices
            .suggestion(suggestion_id)
            .await
            .map_err(people_error)?
            .ok_or_else(|| {
                AppError::new(AppErrorCode::RecordNotFound, "no such speaker suggestion")
            })?;
        if row.state != "pending" {
            return Err(AppError::new(
                AppErrorCode::Conflict,
                "this suggestion has already been settled",
            ));
        }

        let Some(corrected) = corrected_person_id else {
            // A plain rejection renames nothing, ever. What it records is the
            // score as negative evidence for this enrolment's calibration
            // (VP-216, VP-063) — never the rejected cluster's embedding, which
            // would be a voiceprint of whoever actually spoke.
            let settled = voices
                .reject_suggestion(suggestion_id, Timestamp::now())
                .await
                .map_err(people_error)?;
            return Ok(SpeakerSuggestionDecisionDto {
                suggestion_id,
                state: settled.state,
                applied: false,
                speaker: None,
                person: None,
                assigned_segments: Vec::new(),
                segment_exceptions: 0,
            });
        };

        if corrected == row.person_id {
            return Err(AppError::new(
                AppErrorCode::InvalidArgument,
                "a correction has to name somebody other than the suggested person",
            ));
        }
        let (applied, speaker, person, assigned_segments, segment_exceptions) = self
            .assign_from_suggestion(&row, corrected, overwrite_segment_exceptions)
            .await?;
        if !applied {
            return Ok(SpeakerSuggestionDecisionDto {
                suggestion_id,
                state: row.state,
                applied: false,
                speaker,
                person: None,
                assigned_segments,
                segment_exceptions,
            });
        }
        let settled = voices
            .correct_suggestion(suggestion_id, corrected, Timestamp::now())
            .await
            .map_err(people_error)?;
        Ok(SpeakerSuggestionDecisionDto {
            suggestion_id,
            state: settled.state,
            applied: true,
            speaker,
            person,
            assigned_segments,
            segment_exceptions,
        })
    }

    async fn start_speaker_matching(
        &self,
        recording_ids: &[RecordingId],
    ) -> Result<StartSpeakerMatchingResponse, AppError> {
        let mut jobs = Vec::new();
        let mut refusals = Vec::new();
        for recording_id in recording_ids {
            match self.enqueue_speaker_match(*recording_id).await {
                Ok(job_id) => jobs.push(SpeakerMatchJobDto {
                    recording_id: *recording_id,
                    job_id,
                }),
                // One recording that cannot be matched — no transcript yet, no
                // media on disk — does not cancel a bulk selection: the queue
                // reports it and the rest run (VP-093). The reason is the
                // code's canonical sentence, not the error's detail, which
                // §15.4 keeps out of anything a user reads.
                Err(error) => refusals.push(SpeakerMatchRefusalDto {
                    recording_id: *recording_id,
                    reason: error.code().canonical_message().to_owned(),
                }),
            }
        }
        Ok(StartSpeakerMatchingResponse { jobs, refusals })
    }
}
