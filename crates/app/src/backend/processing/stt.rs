//! The transcribe executor: choose an engine, run it, commit the revision.
//!
//! Engine choice is `plaudon_app_core::engine_policy`'s, not this module's.
//! Every candidate is *probed* — `SpeechToText::capabilities()` is a runtime
//! answer, never a compiled-in assumption — and the policy ranks Apple Speech
//! above local whisper.cpp above anything else, refusing a candidate whose
//! probe failed or whose capabilities do not cover the media.
//!
//! A cloud engine is never a candidate here. OpenRouter transcription runs
//! only when the user's model preference named it *and* the key, the budget
//! and the consent grant were all satisfied before the job row was opened
//! (`LocalBackend::prepare_cloud_transcription`), so a local job can never
//! silently become a billable one.

use std::path::PathBuf;
use std::sync::Arc;

use plaudon_ai_contracts::policy::SttRequirements;
use plaudon_ai_contracts::stt::{SpeechToText, SttInput, TranscriptDraft};
use plaudon_ai_contracts::transcript::SourceTime;
use plaudon_ai_openrouter::registry::OPENROUTER_ENGINE_ID;
use plaudon_app_core::engine_policy::candidate_from_probe;
use plaudon_app_core::stt::selection::CloudPolicy;
use plaudon_app_core::stt::{select_engine, EngineCandidate, Platform, SelectionRequest};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::RecordingId;
use plaudon_jobs::store::JobOutput;

use crate::backend::LocalBackend;
use crate::dto::{OutputLink, OutputLinkKind};
use crate::ports::{MachineRevisionSpec, MachineSegment, TranscriptPort};

use super::{Completion, RunContext};

/// Directory under the library's model root holding whisper weights.
const WHISPER_MODEL_DIR: &str = "whisper.cpp";

/// Which speech engines a worker may run.
#[derive(Clone, Default)]
pub(super) enum EngineSeam {
    /// Probe the host: Apple Speech, plus whisper.cpp for every installed
    /// model when this build carries the engine.
    #[default]
    Host,
    /// The engines a test installed instead.
    Substituted(Vec<Arc<dyn SpeechToText>>),
}

impl EngineSeam {
    pub(super) fn substituted(engines: Vec<Arc<dyn SpeechToText>>) -> Self {
        Self::Substituted(engines)
    }
}

impl std::fmt::Debug for EngineSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host => f.write_str("host"),
            Self::Substituted(engines) => write!(f, "substituted({})", engines.len()),
        }
    }
}

/// Whether a local speech engine could run at all on this machine.
///
/// Preflight asks before a job is admitted: an absent model parks the job in
/// `waiting_for_model`, which is a state the user can act on, rather than
/// failing it (§14.4).
pub(crate) fn a_local_engine_is_installed(backend: &LocalBackend) -> bool {
    match &backend.processing().seam {
        EngineSeam::Substituted(engines) => !engines.is_empty(),
        EngineSeam::Host => {
            apple_speech_available() || !installed_whisper_models(backend).is_empty()
        }
    }
}

/// Run one transcribe job.
pub async fn transcribe(run: &RunContext) -> Result<Completion, AppError> {
    let recording_id = run.recording()?;
    if run.text("engine") == Some(OPENROUTER_ENGINE_ID) {
        return cloud(run, recording_id).await;
    }
    local(run, recording_id).await
}

/// Transcribe on this machine.
async fn local(run: &RunContext, recording_id: RecordingId) -> Result<Completion, AppError> {
    let backend = run.backend();
    let media = backend.media_facts(recording_id).await?;
    let engines = host_engines(backend);
    if engines.is_empty() || !a_local_engine_is_installed(backend) {
        // Issue #305: say that no speech model is installed, and offer the
        // install, rather than blaming a model the user never selected.
        return Err(AppError::new(
            AppErrorCode::NoSpeechModel,
            "no local speech engine has a model installed",
        ));
    }

    let preference = run.text("engine").filter(|engine| !engine.is_empty());
    let mut probed: Vec<(EngineCandidate, Arc<dyn SpeechToText>)> = Vec::new();
    for engine in engines {
        let capabilities = engine.capabilities().await;
        if preference.is_some_and(|wanted| capabilities.engine_id.as_str() != wanted) {
            continue;
        }
        probed.push((candidate_from_probe(capabilities), engine));
    }
    if probed.is_empty() {
        return Err(AppError::new(
            AppErrorCode::ModelUnavailable,
            "the engine this recording is configured for is not installed",
        ));
    }

    let request = SelectionRequest {
        platform: Platform::current(),
        requirements: SttRequirements {
            format: Some(media.format.clone()),
            byte_length: Some(media.byte_length),
            duration_ms: media.duration_ms,
            locale: None,
            // The worker only ever selects among local engines; the policy
            // enforces that rather than this module trusting its own list.
            require_offline: true,
            require_segments: false,
            require_word_timestamps: false,
            require_cancellation: true,
        },
        cloud_policy: CloudPolicy::LocalOnly,
    };
    let candidates: Vec<EngineCandidate> = probed
        .iter()
        .map(|(candidate, _)| candidate.clone())
        .collect();
    let decision = select_engine(&request, &candidates);
    let Some(chosen) = decision.chosen.clone() else {
        // The policy's own words, which name the engine and the observed
        // reason; this module composes none of its own.
        return Err(AppError::new(
            AppErrorCode::ModelUnavailable,
            decision
                .explain()
                .first()
                .cloned()
                .unwrap_or_else(|| "no speech engine can run this recording".to_owned()),
        ));
    };
    let (candidate, engine) = probed
        .into_iter()
        .find(|(candidate, _)| candidate.engine_id() == &chosen)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCode::InternalError,
                "the selected engine disappeared between probe and run",
            )
        })?;

    run.stage("transcribing").await;
    let mut input = SttInput::whole_file(
        media.path.clone(),
        media.sha256,
        media.byte_length,
        media.format.clone(),
        media.sample_rate_hz,
        media.channels,
    );
    input.duration_ms = media.duration_ms;

    let draft = engine
        .transcribe(input, run.ctx.clone())
        .await
        .map_err(AppError::from)?;
    let model_id = candidate.capabilities.model.model_id.clone();
    commit(run, recording_id, chosen.as_str(), &model_id, draft).await
}

/// Transcribe through the cloud engine the job was opened for.
async fn cloud(run: &RunContext, recording_id: RecordingId) -> Result<Completion, AppError> {
    let backend = run.backend();
    let model_id = run.text("model_id").ok_or_else(|| {
        AppError::new(
            AppErrorCode::CapabilityUnverified,
            "a cloud engine needs a model id",
        )
    })?;
    // Re-checked at dispatch time, not only when the job was queued: a key
    // removed, a budget lowered or a consent revoked while the job waited
    // must stop it here, before a byte leaves.
    let prepared = backend
        .prepare_cloud_transcription(recording_id, model_id, run.flag("cost_acknowledged"))
        .await?;
    run.enter_cloud_sending().await?;
    let draft = backend
        .dispatch_cloud_transcription(recording_id, &prepared, &run.ctx)
        .await?;
    let model_id = model_id.to_owned();
    commit(run, recording_id, OPENROUTER_ENGINE_ID, &model_id, draft).await
}

/// Write the machine revision and describe what the job produced.
async fn commit(
    run: &RunContext,
    recording_id: RecordingId,
    engine_id: &str,
    model_id: &str,
    draft: TranscriptDraft,
) -> Result<Completion, AppError> {
    let revision = run
        .backend()
        .create_machine_revision(MachineRevisionSpec {
            recording_id,
            segments: machine_segments(&draft),
            engine_id: engine_id.to_owned(),
            model_id: model_id.to_owned(),
            alignment_origin: draft.alignment_origin,
        })
        .await?;
    Ok(Completion {
        outputs: vec![JobOutput::new(
            "transcript_revision",
            revision.revision_id.to_string(),
        )],
        link: Some(OutputLink {
            kind: OutputLinkKind::Recording,
            id: recording_id.to_string(),
        }),
    })
}

/// The draft's segments in the wire shape the revision writer takes.
pub(super) fn machine_segments(draft: &TranscriptDraft) -> Vec<MachineSegment> {
    let to_ms = |time: SourceTime| {
        time.sample_offset.saturating_mul(1000) / u64::from(time.sample_rate_hz.max(1))
    };
    draft
        .segments
        .iter()
        .map(|segment| MachineSegment {
            text: segment.text.clone(),
            start_ms: segment.start.map(to_ms),
            end_ms: segment.end.map(to_ms),
            confidence: None,
            speaker_label: None,
        })
        .collect()
}

/* ------------------------------------------------------------- the hosts */

/// Every local engine this build can actually run.
fn host_engines(backend: &LocalBackend) -> Vec<Arc<dyn SpeechToText>> {
    match &backend.processing().seam {
        EngineSeam::Substituted(engines) => engines.clone(),
        EngineSeam::Host => {
            let mut engines: Vec<Arc<dyn SpeechToText>> =
                vec![Arc::new(plaudon_ai_apple::stt::AppleSpeechToText::new(
                    plaudon_ai_apple::host_bridge(),
                    crate::backend::capabilities::apple_probe_cache(),
                ))];
            engines.extend(whisper_engines(backend));
            engines
        }
    }
}

/// Whether the Apple speech engine reports itself usable right now.
fn apple_speech_available() -> bool {
    let (availability, _) = crate::backend::capabilities::apple_probe_cache().availability(None);
    plaudon_ai_apple::probe::AppleCapabilities::from_availability(
        &availability,
        plaudon_ai_apple::probe::AssetDownloadConsent::Withheld,
    )
    .speech
    .availability_reason
    .is_available()
}

/// Installed whisper weights: one directory per model id, the weight file
/// inside it.
fn installed_whisper_models(backend: &LocalBackend) -> Vec<(String, PathBuf)> {
    let root = backend.store().models_dir().join(WHISPER_MODEL_DIR);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut models: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let model_id = entry.file_name().to_string_lossy().into_owned();
            let weights = std::fs::read_dir(entry.path())
                .ok()?
                .flatten()
                .map(|file| file.path())
                .find(|path| {
                    path.is_file()
                        && path
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("bin"))
                })?;
            Some((model_id, weights))
        })
        .collect();
    models.sort_by(|left, right| left.0.cmp(&right.0));
    models
}

#[cfg(feature = "local-whisper")]
fn whisper_engines(backend: &LocalBackend) -> Vec<Arc<dyn SpeechToText>> {
    installed_whisper_models(backend)
        .into_iter()
        .map(|(model_id, path)| {
            Arc::new(plaudon_ai_whisper::engine::WhisperEngine::new(
                plaudon_ai_whisper::engine::WhisperConfig::new(path, model_id),
            )) as Arc<dyn SpeechToText>
        })
        .collect()
}

#[cfg(not(feature = "local-whisper"))]
fn whisper_engines(_backend: &LocalBackend) -> Vec<Arc<dyn SpeechToText>> {
    // This build did not link whisper.cpp. Offering the engine here would
    // make the engine inventory claim something the binary cannot do.
    Vec::new()
}
