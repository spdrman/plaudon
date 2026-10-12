//! The summarize/extract executor: the `plaudon_app_core` pipeline over a
//! committed transcript revision, with its output and every stage's
//! provenance written in one transaction.
//!
//! Route choice follows the same rule as a transcript tool run
//! (`backend::tools::resolve_text_model`), because it is the same decision:
//!
//! * a model installed by a test wins, so the whole path runs with no
//!   network;
//! * Apple Foundation Models is the default when the probe reports it
//!   available — on device, free, no consent needed;
//! * OpenRouter runs only when the user's preference names it, and then only
//!   after the key, the budget, the egress gate and a covering consent grant
//!   have all passed;
//! * the cross-platform local LLM is offered only when its footprint gate
//!   passes. In this build the gate artifact is not shipped (ADR-0401), so
//!   the route reports the gate's own refusal rather than pretending the
//!   model exists.

use std::sync::Arc;

use plaudon_ai_contracts::capabilities::{PrivacyClass, TextModelCapabilities};
use plaudon_ai_contracts::text::TextModel;
use plaudon_ai_contracts::validation::{RepairPolicy, SpeakerDirectory};
use plaudon_ai_openrouter::client::{JobBinding, OpenRouterTextModel};
use plaudon_ai_openrouter::pricing::{estimate, EstimateInput};
use plaudon_ai_openrouter::registry::OPENROUTER_ENGINE_ID;
use plaudon_app_core::chunking::ChunkPolicy;
use plaudon_app_core::engine_policy::{LOCAL_LLM_ENGINE_ID, OPENROUTER_ENGINE_ID as POLICY_CLOUD};
use plaudon_app_core::pipeline::{
    PipelineError, PipelineRequest, SummarizationPipeline, SystemClock,
};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::RecordingId;
use plaudon_jobs::store::JobOutput;
use plaudon_persistence::intelligence::OutputRepository;
use plaudon_security::consent::ContentType;

use crate::backend::LocalBackend;
use crate::dto::{OutputLink, OutputLinkKind};

use super::{Completion, RunContext};

/// Template used when the request names none.
const DEFAULT_TEMPLATE_ID: &str = "general_meeting";

/// `ai_outputs.output_type` of a template-driven summary (spec §13.2).
const OUTPUT_TYPE: &str = "meeting_summary";

/// Run one summarize job.
pub async fn summarize(run: &RunContext) -> Result<Completion, AppError> {
    let backend = run.backend();
    let recording_id = run.recording()?;

    run.stage("reading the transcript").await;
    let revision = backend.domain_revision(recording_id, None).await?;
    let media = backend.media_facts(recording_id).await?;

    let registry = backend.template_registry()?;
    let template_id = run.text("template_id").unwrap_or(DEFAULT_TEMPLATE_ID);
    let template = registry
        .get(template_id)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCode::RecordNotFound,
                "the template this job names no longer exists",
            )
        })?
        .clone();
    let extraction_schema = registry
        .extraction_schema(&template)
        .map_err(|err| AppError::new(AppErrorCode::InvalidArgument, err.to_string()))?;

    let route = resolve_model(run, recording_id).await?;
    run.stage("summarizing").await;

    let request = PipelineRequest {
        recording_id,
        revision,
        template,
        extraction_schema,
        summary_schema: registry.summary_schema(),
        capabilities: route.capabilities,
        chunk_policy: ChunkPolicy::DEFAULT,
        repair_policy: RepairPolicy::DEFAULT,
        speakers: SpeakerDirectory::unknown(),
        reusable_sections: Vec::new(),
        privacy_policy_snapshot: route.privacy_snapshot,
        audio_sha256: media.sha256,
    };

    let outcome = SummarizationPipeline::new(route.model, Arc::new(SystemClock))
        .run(request, &run.ctx)
        .await
        .map_err(pipeline_error)?;

    run.stage("committing").await;
    let output = outcome.to_generated_output(OUTPUT_TYPE);
    let output_id = output.id;
    OutputRepository::new(backend.store().db().clone())
        .save(&output)
        .await
        .map_err(AppError::from)?;

    Ok(Completion {
        outputs: vec![JobOutput::new("ai_output", output_id.to_string())],
        link: Some(OutputLink {
            kind: OutputLinkKind::Recording,
            id: recording_id.to_string(),
        }),
    })
}

/// The model the stage runs on, with the capabilities the pipeline plans
/// against.
struct Route {
    model: Arc<dyn TextModel>,
    capabilities: TextModelCapabilities,
    privacy_snapshot: Option<String>,
}

async fn resolve_model(run: &RunContext, recording_id: RecordingId) -> Result<Route, AppError> {
    let backend = run.backend();

    // The test seam replaces the model, never the policy: a scripted model
    // that reports a cloud route still passes the key, the budget, the egress
    // gate and the consent check below.
    if let Some(model) = backend.installed_text_model() {
        let capabilities = model.capabilities().await;
        let requires_consent = capabilities
            .privacy_class
            .get()
            .copied()
            .is_some_and(PrivacyClass::requires_cloud_consent);
        if !requires_consent {
            return Ok(Route {
                model,
                capabilities,
                privacy_snapshot: Some("on-device".to_owned()),
            });
        }
        let model_id = capabilities.model.model_id.clone();
        let plan = backend
            .authorize_dispatch(&recording_id.to_string(), &model_id, ContentType::TranscriptText)
            .await?;
        return Ok(Route {
            model,
            capabilities,
            privacy_snapshot: Some(plan.resolution.snapshot()),
        });
    }

    let engine = run
        .text("engine")
        .filter(|engine| !engine.is_empty())
        .unwrap_or(plaudon_ai_apple::APPLE_FOUNDATION_ENGINE)
        .to_owned();

    if engine == plaudon_ai_apple::APPLE_FOUNDATION_ENGINE {
        return apple_route();
    }
    if engine == LOCAL_LLM_ENGINE_ID {
        return Err(local_llm_refusal(backend));
    }
    if engine != OPENROUTER_ENGINE_ID && engine != POLICY_CLOUD {
        return Err(AppError::new(
            AppErrorCode::ModelUnavailable,
            format!("this build cannot summarize on the {engine} engine"),
        ));
    }
    cloud_route(run, recording_id).await
}

/// The on-device language model, as the probe currently reports it.
fn apple_route() -> Result<Route, AppError> {
    let cache = crate::backend::capabilities::apple_probe_cache();
    let (availability, _) = cache.availability(None);
    let capabilities = plaudon_ai_apple::probe::text_model_capabilities(&availability.text_model);
    if !capabilities.is_selectable() {
        return Err(AppError::new(
            AppErrorCode::ModelUnavailable,
            plaudon_app_core::engine_policy::explain(
                &capabilities.engine_id,
                &capabilities.availability_reason,
            )
            .headline,
        ));
    }
    let model = plaudon_ai_apple::text::AppleTextModel::new(plaudon_ai_apple::host_bridge(), cache);
    Ok(Route {
        model: Arc::new(model) as Arc<dyn TextModel>,
        capabilities,
        privacy_snapshot: Some("on-device".to_owned()),
    })
}

/// Why the cross-platform local model cannot be offered on this host.
fn local_llm_refusal(backend: &LocalBackend) -> AppError {
    let verdict = plaudon_ai_local_llm::gate::evaluate_at(
        &plaudon_ai_local_llm::gate::artifact_path(backend.store().root()),
        &plaudon_ai_local_llm::gate::current_platform(),
        plaudon_ai_local_llm::gate::FootprintThresholds::default(),
    );
    let message = match verdict {
        plaudon_ai_local_llm::gate::GateVerdict::Refused(refusal) => refusal.explain(),
        plaudon_ai_local_llm::gate::GateVerdict::Passed { .. } => {
            "the local language model passed its gate but no model file is installed".to_owned()
        }
    };
    AppError::new(AppErrorCode::ModelUnavailable, message)
}

/// The cloud route: key, budget, egress gate and consent, in that order.
async fn cloud_route(run: &RunContext, recording_id: RecordingId) -> Result<Route, AppError> {
    let backend = run.backend();
    let model_id = run.text("model_id").ok_or_else(|| {
        AppError::new(
            AppErrorCode::CapabilityUnverified,
            "a cloud engine needs a model id",
        )
    })?;
    let card = backend
        .card_for(plaudon_ai_openrouter::allowlist::ModelTask::TextGeneration, model_id)
        .await?;
    let capabilities = crate::backend::tools::cloud_capabilities(&card);
    if !capabilities.is_selectable() {
        return Err(AppError::new(
            AppErrorCode::CapabilityUnverified,
            "the selected model has not passed the capability check",
        ));
    }
    backend.require_key()?;
    // The prompt is assembled inside the pipeline, so the only honest
    // pre-dispatch figure is the reserved output budget; an unknown price
    // makes the estimate unknown, never zero.
    let predicted = estimate(
        &card.pricing,
        EstimateInput::Text {
            prompt_tokens: 0,
            max_output_tokens: ChunkPolicy::DEFAULT.reserved_output_tokens,
        },
    );
    backend
        .check_budget(predicted.amount, run.flag("cost_acknowledged"))
        .await?;
    let plan = backend
        .authorize_dispatch(&recording_id.to_string(), model_id, ContentType::TranscriptText)
        .await?;

    let binding = JobBinding {
        job_id: run.job_id.to_string(),
        recording_id: recording_id.to_string(),
        consent_id: plan.grant_id.clone(),
        privacy_snapshot: plan.resolution.snapshot(),
        route_label: plan.resolution.route.downstream_provider.clone(),
    };
    let model = OpenRouterTextModel::new(card, backend.dispatcher(&plan), binding)
        .with_structured_output(capabilities.supports_structured_output)
        .with_now_unix(plaudon_domain::time::Timestamp::now().as_millis().div_euclid(1000));
    Ok(Route {
        model: Arc::new(model) as Arc<dyn TextModel>,
        capabilities,
        privacy_snapshot: Some(plan.resolution.snapshot()),
    })
}

/// The error envelope a pipeline failure becomes.
///
/// Every arm keeps the pipeline's own words; nothing here invents a cause,
/// and a model error keeps its own code so a cancellation stays a
/// cancellation and an unknown outcome stays unknown.
fn pipeline_error(error: PipelineError) -> AppError {
    match error {
        PipelineError::Model(model) => AppError::from(model),
        PipelineError::Cancelled => AppError::new(AppErrorCode::Canceled, error_text(&error)),
        PipelineError::CapabilityUnverified | PipelineError::RequestFamilyUnknown => {
            AppError::new(AppErrorCode::CapabilityUnverified, error_text(&error))
        }
        PipelineError::Chunking(_) | PipelineError::CoverageIncomplete { .. } => {
            AppError::new(AppErrorCode::ContextTooLarge, error_text(&error))
        }
        PipelineError::SummarySchema { .. }
        | PipelineError::ExtractionRejected { .. }
        | PipelineError::Validator(_) => {
            AppError::new(AppErrorCode::SchemaValidationFailed, error_text(&error))
        }
        PipelineError::UnsafeResponse(_) => {
            AppError::new(AppErrorCode::ProviderRejected, error_text(&error))
        }
    }
}

/// The pipeline's message, which names no path, prompt or transcript text.
fn error_text(error: &PipelineError) -> String {
    error.to_string()
}
