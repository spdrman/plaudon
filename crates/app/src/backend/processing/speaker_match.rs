//! The speaker-matching executor: diarize → reconcile → embed → match →
//! suggest, resumably (`VOICE_PROFILES_SPEC` §4, §10, VP-100..VP-105;
//! ADR-0080, ADR-0082).
//!
//! The pipeline itself lives in
//! [`plaudon_app_core::voice_profiles::pipeline`], where it has no worker, no
//! preferences and no engine registry. This module is the application half:
//! it reads the consent state the job is planned against, resolves the media
//! and the transcript revision the stages work on, hands the pipeline the
//! job's cancellation token and progress sink, and turns an outcome or a
//! refusal into the job's terminal state.
//!
//! Three rules shape it.
//!
//! * **Off unless the gate says otherwise (VP-003).** The toggle lives in
//!   `app_preferences` under [`VOICE_MATCHING_PREFERENCE_KEY`] and is absent —
//!   therefore off — in a library nobody turned it on in.
//!   [`LocalBackend::enqueue_speaker_match`] refuses to queue work the gate
//!   would not let run, and the executor re-reads the gate at run time, because
//!   a user may have turned the feature off or deleted their profiles while the
//!   job sat in the queue. In that state the plan simply has no embed, match or
//!   suggest stage (VP-024).
//! * **No engine, no job (§14.4).** This build ships no speaker-embedding
//!   model: the ONNX tier is a later phase, so [`an_embedder_is_installed`]
//!   answers `false` on a host and preflight parks the job in
//!   `waiting_for_model`, a state the user can act on, rather than failing it.
//!   Tests substitute a scripted embedder through the same seam the speech
//!   engines use.
//! * **A CPU slot, never beside local speech (VP-104).** The job takes
//!   [`ResourceClass::Cpu`], whose limit is one, so a match job and a local
//!   transcription of the same recording cannot run at the same time; they
//!   queue behind each other like any two CPU stages.

use std::collections::BTreeMap;
use std::sync::Arc;

use plaudon_ai_diarization::contract::DiarizationProvider;
use plaudon_ai_diarization::provider::RunPolicy;
use plaudon_app_core::voice_profiles::embedder::SpeakerEmbedder;
use plaudon_app_core::voice_profiles::pipeline::{
    profile_set_digest, GateRefusal, MatchGate, SpeakerMatchError, SpeakerMatchOutcome,
    SpeakerMatchPipeline, SpeakerMatchPlan, SpeakerMatchRequest,
};
use plaudon_app_core::voice_profiles::store::EmbeddingCipher;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_jobs::store::JobOutput;
use plaudon_persistence::voice_profiles::VoiceEnrollmentRepository;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::backend::LocalBackend;
use crate::dto::{JobKind, OutputLink, OutputLinkKind};

use super::{Completion, RunContext, StageRequest};

/// `app_preferences` key holding the runtime voice-matching state (§12.2).
///
/// An absent row is the default posture: matching off, no disclosure accepted.
pub const VOICE_MATCHING_PREFERENCE_KEY: &str = "voice.matching";

/// The runtime half of the §12.2 gate, as stored.
///
/// The build feature and this toggle are two independent switches (VP-191);
/// this is the second one. The disclosure *text* and the command that records
/// an acceptance belong to the roster surface; what is persisted here is which
/// version the user accepted, so a newer disclosure re-gates (VP-121).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceMatchingSettings {
    /// Whether the user turned biometric matching on. Off by default (VP-003).
    #[serde(default)]
    pub enabled: bool,
    /// Version of the activation disclosure the user accepted, if any.
    #[serde(default)]
    pub accepted_disclosure_version: Option<String>,
}

/// Which voice engines a worker may run.
#[derive(Clone, Default)]
pub(super) enum VoiceSeam {
    /// The host: no speaker-embedding model ships in this build yet, so the
    /// executor reports the engine as unavailable rather than pretending a
    /// model could run (VP-042, §14.4).
    #[default]
    Host,
    /// The provider and embedder a test installed instead.
    Substituted {
        diarizer: Arc<dyn DiarizationProvider>,
        embedder: Arc<dyn SpeakerEmbedder>,
    },
}

impl VoiceSeam {
    pub(super) fn substituted(
        diarizer: Arc<dyn DiarizationProvider>,
        embedder: Arc<dyn SpeakerEmbedder>,
    ) -> Self {
        Self::Substituted { diarizer, embedder }
    }
}

impl std::fmt::Debug for VoiceSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host => f.write_str("host"),
            Self::Substituted { .. } => f.write_str("substituted"),
        }
    }
}

/// Whether a speaker-embedding model could run at all on this machine.
///
/// Preflight asks before a job is admitted: an absent model parks the job in
/// `waiting_for_model`, which the user can act on, rather than failing it
/// (§14.4).
pub(super) fn an_embedder_is_installed(backend: &LocalBackend) -> bool {
    matches!(backend.processing().voice, VoiceSeam::Substituted { .. })
}

/// Run one speaker-matching job.
///
/// # Errors
///
/// [`AppErrorCode::ModelUnavailable`] when no embedder is installed,
/// [`AppErrorCode::Canceled`] when the user cancelled, and otherwise whatever
/// the stage that could not finish reported. Every committed stage stays
/// committed, so a retry resumes instead of restarting (VP-102).
pub async fn match_speakers(run: &RunContext) -> Result<Completion, AppError> {
    let backend = run.backend();
    let recording_id = run.recording()?;
    let pipeline = backend.speaker_match_pipeline()?;

    // The gate is read again here, not trusted from enqueue time: a user may
    // have turned matching off, withdrawn consent or deleted every profile
    // while this job waited in the queue (VP-003, VP-024).
    let gate = backend.voice_match_gate().await?;
    let plan = SpeakerMatchPlan::for_gate(&gate);
    if let Some(refusal) = plan.refusal() {
        tracing::info!(
            reason = refusal.as_str(),
            "speaker matching runs without its biometric stages"
        );
    }

    let media = backend.media_facts(recording_id).await?;
    let media_asset_id = preferred_media_asset_id(backend, recording_id).await?;
    let revision = backend.domain_revision(recording_id, None).await?;

    let request = SpeakerMatchRequest {
        job_id: run.job_id,
        recording_id,
        media_asset_id,
        media_path: media.path.clone(),
        audio_sha256: media.sha256.to_hex(),
        duration_ms: media.duration_ms,
        revision,
        gate,
        // Local-only providers need no cloud grant; the allowlist is what
        // decides, and `run_guarded` applies it.
        policy: RunPolicy::default_posture(),
        speaker_count_hint: None,
    };

    run.stage("diarizing").await;
    let outcome = match pipeline.run(&request, &run.ctx).await {
        Ok(outcome) => outcome,
        Err(error) => {
            if matches!(error, SpeakerMatchError::Cancelled) {
                // A cancelled job is terminal and is never resumed, so the
                // biometric scratch it parked goes with it (§12.4).
                if let Err(cleanup) = pipeline.discard_scratch(run.job_id).await {
                    tracing::warn!(
                        error = %cleanup,
                        "a cancelled match job's scratch could not be discarded"
                    );
                }
            }
            return Err(map_error(&error));
        }
    };

    run.stage(outcome_label(&outcome)).await;
    Ok(Completion {
        outputs: vec![JobOutput::new(
            "diarization_run",
            outcome.diarization_run_id.clone(),
        )],
        link: Some(OutputLink {
            kind: OutputLinkKind::Recording,
            id: recording_id.to_string(),
        }),
    })
}

/// The step label a finished run leaves behind, which is what the job list
/// shows. Counts only: never a person's name and never a score (VP-112).
fn outcome_label(outcome: &SpeakerMatchOutcome) -> &'static str {
    match (&outcome.refusal, outcome.suggestions_written) {
        (Some(_), _) => "matching off",
        (None, 0) => "no match",
        (None, _) => "suggestions ready",
    }
}

/// Map a pipeline failure onto the application's error envelope.
///
/// Neither a profile name nor a score reaches the message: an error envelope is
/// a diagnostics surface, and §15.4 keeps both out of it (VP-112).
fn map_error(error: &SpeakerMatchError) -> AppError {
    match error {
        SpeakerMatchError::Cancelled => {
            AppError::new(AppErrorCode::Canceled, "speaker matching was cancelled")
        }
        SpeakerMatchError::Diarization(inner) => AppError::from(inner.clone()),
        SpeakerMatchError::Store(inner) if inner.is_store_locked() => AppError::new(
            AppErrorCode::SecretStoreLocked,
            "the voice-profile key store is locked, so matching cannot run",
        ),
        SpeakerMatchError::Embed(_) => AppError::new(
            AppErrorCode::ModelUnavailable,
            "the speaker-embedding model could not produce an embedding",
        ),
        SpeakerMatchError::Persistence(_) | SpeakerMatchError::Jobs(_) => AppError::new(
            AppErrorCode::StorageUnavailable,
            "the library could not record this match job's progress",
        ),
        other => AppError::new(AppErrorCode::InternalError, other.to_string()),
    }
}

/// The recording's preferred media asset, which the diarization run records.
async fn preferred_media_asset_id(
    backend: &LocalBackend,
    recording_id: RecordingId,
) -> Result<String, AppError> {
    let row = sqlx::query(
        "SELECT preferred_media_asset_id FROM recordings WHERE id = ?1 AND deleted_at IS NULL",
    )
    .bind(recording_id.to_string())
    .fetch_optional(backend.pool())
    .await
    .map_err(|error| {
        AppError::new(
            AppErrorCode::StorageUnavailable,
            format!("cannot read the recording's media: {error}"),
        )
    })?
    .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such recording"))?;
    row.try_get::<Option<String>, _>("preferred_media_asset_id")
        .ok()
        .flatten()
        .ok_or_else(|| {
            AppError::new(
                AppErrorCode::AssetNotFound,
                "this recording has no preferred media asset to match against",
            )
        })
}

impl LocalBackend {
    /// The runtime voice-matching state, defaulting to off (VP-003).
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn voice_matching_settings(&self) -> Result<VoiceMatchingSettings, AppError> {
        let stored: Option<String> =
            sqlx::query_scalar("SELECT value_json FROM app_preferences WHERE key = ?1")
                .bind(VOICE_MATCHING_PREFERENCE_KEY)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| {
                    AppError::new(
                        AppErrorCode::StorageUnavailable,
                        format!("cannot read the voice-matching preference: {error}"),
                    )
                })?;
        Ok(stored
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default())
    }

    /// Persist the runtime voice-matching state.
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn set_voice_matching_settings(
        &self,
        settings: &VoiceMatchingSettings,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(settings).map_err(|error| {
            AppError::new(
                AppErrorCode::InternalError,
                format!("the voice-matching preference is not serializable: {error}"),
            )
        })?;
        sqlx::query(
            "INSERT INTO app_preferences (key, version, value_json) VALUES (?1, 1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        )
        .bind(VOICE_MATCHING_PREFERENCE_KEY)
        .bind(json)
        .execute(self.pool())
        .await
        .map_err(|error| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot store the voice-matching preference: {error}"),
            )
        })?;
        Ok(())
    }

    /// The consent state a match job is planned against (§12.2).
    ///
    /// # Errors
    ///
    /// Propagates storage failures. A locked credential store is *not* an
    /// error here: it closes the gate, exactly as ADR-0014 requires, and the
    /// name-only roster keeps working beside it.
    pub async fn voice_match_gate(&self) -> Result<MatchGate, AppError> {
        let settings = self.voice_matching_settings().await?;
        let enrolments = VoiceEnrollmentRepository::new(self.store().db().clone())
            .active_enrollments()
            .await
            .map_err(|error| {
                AppError::new(
                    AppErrorCode::StorageUnavailable,
                    format!("cannot read the enrolled profiles: {error}"),
                )
            })?;
        // `check_available` answers whether the *vault* answers; a library that
        // has never sealed anything has no key yet, and that is not a refusal.
        let key_available = EmbeddingCipher::new(Arc::clone(self.voice_secrets()))
            .check_available()
            .is_ok();
        Ok(MatchGate {
            matching_enabled: settings.enabled,
            accepted_disclosure_version: settings.accepted_disclosure_version,
            enrolled_profiles: enrolments.len(),
            key_available,
        })
    }

    /// The pipeline this build runs, or the honest refusal that it has no
    /// embedder.
    ///
    /// # Errors
    ///
    /// [`AppErrorCode::ModelUnavailable`] when no speaker-embedding model is
    /// installed in this build.
    pub fn speaker_match_pipeline(&self) -> Result<SpeakerMatchPipeline, AppError> {
        let VoiceSeam::Substituted { diarizer, embedder } = &self.processing().voice else {
            return Err(AppError::new(
                AppErrorCode::ModelUnavailable,
                "this build has no speaker-embedding model, so matching cannot run",
            ));
        };
        Ok(SpeakerMatchPipeline::new(
            self.processing().engine().store().clone(),
            self.store().db().clone(),
            EmbeddingCipher::new(Arc::clone(self.voice_secrets())),
            Arc::clone(diarizer),
            Arc::clone(embedder),
        ))
    }

    /// Queue a speaker-matching job for one recording.
    ///
    /// Refuses when the gate is closed: a dormant pipeline is not work, and
    /// queueing it would put a job in the list that can only ever report "the
    /// feature is off" (VP-003). The key covers the audio digest, the
    /// transcript revision, the diarization provider, the embedding checkpoint,
    /// the threshold-set version and the enrolled-profile-set digest, so asking
    /// twice deduplicates and enrolling a profile makes a fresh key (VP-101).
    ///
    /// # Errors
    ///
    /// [`AppErrorCode::CapabilityUnverified`] when the gate is closed,
    /// [`AppErrorCode::ModelUnavailable`] when no embedder is installed, and
    /// storage failures from the queue.
    pub async fn enqueue_speaker_match(
        &self,
        recording_id: RecordingId,
    ) -> Result<JobId, AppError> {
        let gate = self.voice_match_gate().await?;
        if let Err(refusal) = gate.decide() {
            return Err(gate_refusal_error(&refusal));
        }
        let VoiceSeam::Substituted { diarizer, embedder } = &self.processing().voice else {
            return Err(AppError::new(
                AppErrorCode::ModelUnavailable,
                "this build has no speaker-embedding model, so matching cannot run",
            ));
        };

        let media = self.media_facts(recording_id).await?;
        let revision = self.head_revision(recording_id).await?.ok_or_else(|| {
            AppError::new(
                AppErrorCode::RecordNotFound,
                "this recording has no transcript to reconcile speakers against",
            )
        })?;
        let provider_id = diarizer.capabilities().await.provider_id;
        let checkpoint = embedder.checkpoint();

        let mut normalization = BTreeMap::new();
        normalization.insert("diarization_provider".to_owned(), provider_id.clone());
        normalization.insert(
            "threshold_set".to_owned(),
            checkpoint.threshold_set_version.clone(),
        );
        normalization.insert(
            "enrolled_profiles".to_owned(),
            self.enrolled_profile_digest().await?,
        );

        let request = StageRequest {
            recording_id,
            kind: JobKind::SpeakerMatch,
            engine: provider_id,
            model_id: Some(checkpoint.binding.model_id.clone()),
            model_version: Some(checkpoint.binding.model_version.clone()),
            stage: "speaker_match".to_owned(),
            label: "Match speakers".to_owned(),
            audio_sha256: media.sha256.to_hex(),
            cloud: false,
            cost_acknowledged: false,
            transcript_revision: Some(revision.to_string()),
            normalization,
        };
        self.enqueue_stage(&request).await
    }

    /// Digest of the enrolled profile set, for the job key (VP-101).
    ///
    /// # Errors
    ///
    /// Propagates storage failures.
    pub async fn enrolled_profile_digest(&self) -> Result<String, AppError> {
        let voices = VoiceEnrollmentRepository::new(self.store().db().clone());
        let storage = |error: plaudon_persistence::PersistenceError| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot read the enrolled profiles: {error}"),
            )
        };
        let mut profiles = Vec::new();
        for enrolment in voices.active_enrollments().await.map_err(storage)? {
            let exemplars = voices
                .exemplars(enrolment.person_id)
                .await
                .map_err(storage)?
                .into_iter()
                .map(|exemplar| exemplar.id.to_string())
                .collect();
            profiles.push((enrolment.person_id.to_string(), exemplars));
        }
        Ok(profile_set_digest(&profiles))
    }
}

/// The error a closed gate produces, naming the condition the user can act on.
fn gate_refusal_error(refusal: &GateRefusal) -> AppError {
    let detail = match refusal {
        GateRefusal::MatchingOff => "voice matching is off",
        GateRefusal::DisclosureNotAccepted => {
            "voice matching has not been activated: the disclosure has not been accepted"
        }
        GateRefusal::DisclosureOutdated { .. } => {
            "the voice-matching disclosure changed and has to be accepted again"
        }
        GateRefusal::NoEnrolledProfiles => "nobody is enrolled, so there is nothing to match",
        GateRefusal::KeyUnavailable => {
            "the voice-profile key store is locked, so matching cannot run"
        }
    };
    AppError::new(AppErrorCode::CapabilityUnverified, detail)
}
