//! Backend implementation of [`crate::ports::ToolsPort`] (spec §9.7, §9.8,
//! matrix row A11).
//!
//! The port owns three decisions the command layer must not make, and this
//! module is where each one is made exactly once:
//!
//! * **Which text model answers.** [`LocalBackend::describe_text_route`]
//!   resolves the configured choice for [`Stage::ToolRun`] and falls back to
//!   the Apple on-device model when the probe says it is available. An engine
//!   this build cannot run is named and refused rather than pretended at.
//! * **Whether that route may be used right now.** A cloud run goes through
//!   the same gate a cloud transcription does — a stored key, the budget, the
//!   egress policy and a covering consent grant — *before* a job row exists
//!   and long before a byte moves. Listing tools runs none of it: a list is
//!   not a dispatch.
//! * **Whether an answer survived anchor verification.** Every anchor that
//!   leaves this module has been re-resolved against the revision it names, so
//!   a stale or fabricated citation arrives at the interface visibly refused
//!   instead of silently rendered.
//!
//! Nothing here renders prose for the user beyond naming an observed cause:
//! the study artifacts themselves are rendered by
//! [`crate::tool_presentation`].

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, PricingBasis, PrivacyClass, RequestFamily, TextModelCapabilities,
};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{StructuredRequest, TextModel};
use plaudon_ai_openrouter::allowlist::ModelTask;
use plaudon_ai_openrouter::client::{approximate_tokens, JobBinding, OpenRouterTextModel};
use plaudon_ai_openrouter::pricing::{estimate, EstimateInput};
use plaudon_ai_openrouter::registry::{ModelCard as DomainCard, OPENROUTER_ENGINE_ID};
use plaudon_ai_openrouter::service::{ModelChoice, Stage};
use plaudon_ai_tools::anchor::{verify_anchor, AnchorVerification, SourceAnchor};
use plaudon_ai_tools::error::ToolError;
use plaudon_ai_tools::prompt::{build_grounded_prompt, FencedPassage};
use plaudon_ai_tools::registry::ToolRegistry;
use plaudon_ai_tools::run::{ConsentPolicySnapshot, ToolRunDraft, ToolRunRecord, ToolRunStatus};
use plaudon_ai_tools::study::{self, study_artifacts_from_value};
use plaudon_ai_tools::tool::{CapabilityGap, ToolId, ToolOrigin, TranscriptToolDefinition};
use plaudon_ai_tools::validate::OutputSchema;
use plaudon_app_core::transcript_tools::{
    ConversationError, ConversationService, OutputProvenance, StudyOutputWriter,
};
use plaudon_domain::capability::{CapabilityStatus, Known};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::money::MoneyEstimate;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};
use plaudon_persistence::tools::{StoredToolDefinition, ToolRepository, ToolRunInsert};
use plaudon_security::consent::ContentType;
use serde_json::json;
use sqlx::Row;

use crate::backend::jobs::{self, JobDraft};
use crate::backend::store::{as_u32, as_u64, rfc3339, rfc3339_opt, storage_error};
use crate::backend::{alignment_str, LocalBackend};
use crate::dto::{
    AnchorDto, AskRecordingResponse, ConsentPolicyDto, ConversationDto, ConversationTurnDto,
    DefinitionDto, FlashcardDto, GenerateStudyArtifactsResponse, JobKind, JobState,
    ListConversationsResponse, ListStudyArtifactsResponse, ListToolRunsResponse,
    ListTranscriptToolsResponse, QuizItemDto, RejectedAnchorDto, RejectedStudyItemDto,
    StudyArtifactSetDto, StudyArtifactsDto, TextRouteDto, ToolRunDto, TranscriptToolDto,
};
use crate::error::{internal, invalid_argument};
use crate::ports::{SettingsPort, ToolsPort};
use crate::validate::{ValidatedAskRecording, ValidatedGenerateStudyArtifacts};

/// Directory under the library root holding user tool overrides.
///
/// Spec §9.3 lets a user copy and customise a tool without editing source
/// code; a malformed file here is reported, never skipped, because an edit
/// that silently did nothing is worse than an edit that was refused.
const TOOL_OVERRIDE_DIR: &str = "tools";

/// Most transcript segments one study run may dispatch.
///
/// A study tool reads the whole revision rather than a retrieved slice, so the
/// bound is the only thing standing between a six-hour recording and a request
/// no model will accept. Exceeding it is [`AppErrorCode::ContextTooLarge`]:
/// quietly sending the first part would produce study material that claims to
/// cover a recording it never read.
const MAX_STUDY_SEGMENTS: usize = 400;

/// Most transcript characters one study run may dispatch.
const MAX_STUDY_PROMPT_CHARS: usize = 120_000;

/* ------------------------------------------------------------ text routes */

/// A text route as it would be used, without anything having been dispatched.
#[derive(Debug, Clone)]
struct RouteDescription {
    /// Engine identifier.
    engine: String,
    /// Model identifier, when the engine names one.
    model_id: Option<String>,
    /// Whether using it sends content off the device.
    is_cloud: bool,
    /// Capabilities as currently observed, used for tool offerability.
    capabilities: TextModelCapabilities,
    /// Why the route cannot run right now; `None` when it can.
    unavailable_reason: Option<String>,
}

impl RouteDescription {
    /// Whether a run on this route could start right now.
    fn available(&self) -> bool {
        self.unavailable_reason.is_none()
    }

    /// The wire form.
    fn to_dto(&self) -> TextRouteDto {
        TextRouteDto {
            engine: self.engine.clone(),
            model_id: self.model_id.clone(),
            is_cloud: self.is_cloud,
            available: self.available(),
            unavailable_reason: self.unavailable_reason.clone(),
        }
    }
}

/// A route that passed every pre-dispatch check, bound to one run.
struct ResolvedRoute {
    /// The model to dispatch to.
    model: Arc<dyn TextModel>,
    /// Its capabilities, recorded with the run.
    capabilities: TextModelCapabilities,
    /// The posture the run is authorized under.
    consent_policy: ConsentPolicySnapshot,
    /// Identity of the grant that authorized it, exactly as `consent_grants`
    /// stores it. Kept as text because a grant id is not a bare UUID.
    consent_id: Option<String>,
    /// The job row the ledger's foreign key resolves against, for a cloud run.
    job_id: Option<JobId>,
}

/// Seconds since the Unix epoch, for the dispatcher's `Retry-After` arithmetic.
fn now_unix() -> i64 {
    Timestamp::now().as_millis().div_euclid(1000)
}

/// Stable wire string of a privacy class.
const fn privacy_label(class: PrivacyClass) -> &'static str {
    match class {
        PrivacyClass::OnDevice => "on_device",
        PrivacyClass::CloudZeroRetention => "cloud_zero_retention",
        PrivacyClass::CloudRetaining => "cloud_retaining",
        PrivacyClass::Unresolved => "unresolved",
    }
}

/// Where a definition came from, in the vocabulary the DTO publishes.
const fn origin_label(origin: ToolOrigin) -> &'static str {
    match origin {
        ToolOrigin::Builtin => "built_in",
        ToolOrigin::User => "user_edit",
    }
}

/// One sentence naming what a model is missing for a tool.
fn gap_text(gap: &CapabilityGap) -> String {
    match gap {
        CapabilityGap::ModelNotVerified { status } => format!(
            "the model has not been validated for this configuration ({})",
            match status {
                CapabilityStatus::Supported => "supported",
                CapabilityStatus::Experimental => "experimental",
                CapabilityStatus::Unsupported => "unsupported",
                CapabilityStatus::Unknown => "unknown",
            }
        ),
        CapabilityGap::StructuredOutputUnavailable => {
            "the model cannot be constrained to the tool's output schema".to_owned()
        }
        CapabilityGap::NotCancellable => "the engine cannot cancel a run in flight".to_owned(),
        CapabilityGap::ContextTooSmall { required, observed } => match observed {
            Some(observed) => format!(
                "the model's context of {observed} tokens is below the {required} this tool needs"
            ),
            None => format!(
                "the model's context length is unknown and this tool needs {required} tokens"
            ),
        },
        CapabilityGap::OutputBudgetTooSmall { required, observed } => match observed {
            Some(observed) => format!(
                "the model's output budget of {observed} tokens is below the {required} this tool \
                 needs"
            ),
            None => format!(
                "the model's output budget is unknown and this tool needs {required} tokens"
            ),
        },
        CapabilityGap::PrivacyClassNotAllowed { observed } => match observed {
            Some(class) => format!(
                "this tool does not accept a {} route",
                privacy_label(*class)
            ),
            None => "the route's privacy posture could not be resolved".to_owned(),
        },
    }
}

/// Capabilities of a cloud text model, derived from the catalogue card alone.
///
/// This is the same mapping [`OpenRouterTextModel::capabilities`] performs,
/// computed without a dispatcher so that listing tools cannot open a job row
/// or touch the network. Anything the catalogue does not state stays
/// `Unknown`, and `Unknown` satisfies no requirement (spec §6.6).
pub(super) fn cloud_capabilities(card: &DomainCard) -> TextModelCapabilities {
    let engine = EngineId::new(OPENROUTER_ENGINE_ID);
    let mut capabilities = TextModelCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, card.model_id.clone()),
    );
    capabilities.status = if card.verification.is_verified() {
        CapabilityStatus::Supported
    } else {
        CapabilityStatus::Unknown
    };
    capabilities.offline = plaudon_domain::capability::Tri::No;
    capabilities.can_cancel = plaudon_domain::capability::Tri::Yes;
    capabilities.max_context_tokens = card.context_length.clone();
    // The catalogue publishes no output ceiling and no schema-enforcement
    // flag; both come from the capability check's observed exchange, and
    // stay Unknown until one has run (spec 6.6, 8.6A).
    capabilities.max_output_tokens = card.max_output_tokens.clone();
    capabilities.supports_structured_output = card.supports_structured_output;
    capabilities.privacy_class = match card.privacy.as_ref().map(|privacy| privacy.posture) {
        Some(plaudon_security::consent::RetentionPosture::ZeroRetention) => {
            Known::Known(PrivacyClass::CloudZeroRetention)
        }
        Some(plaudon_security::consent::RetentionPosture::Unknown) | None => {
            Known::Known(PrivacyClass::Unresolved)
        }
        Some(_) => Known::Known(PrivacyClass::CloudRetaining),
    };
    capabilities.pricing_basis = match card.pricing.basis() {
        Known::Known(PricingBasis::None) | Known::Unknown => Known::Unknown,
        Known::Known(basis) => Known::Known(basis),
    };
    capabilities.availability_reason = if card.verification.is_verified() {
        AvailabilityReason::Available
    } else {
        AvailabilityReason::NotProbed
    };
    capabilities
}

/// The posture a run on `capabilities` happens under.
fn posture_for(
    capabilities: &TextModelCapabilities,
    local_only_mode: bool,
    consent_id: Option<uuid::Uuid>,
) -> ConsentPolicySnapshot {
    let privacy_class = capabilities
        .privacy_class
        .get()
        .copied()
        .unwrap_or(PrivacyClass::Unresolved);
    ConsentPolicySnapshot {
        privacy_class,
        required_cloud_consent: privacy_class.requires_cloud_consent(),
        consent_id,
        local_only_mode,
    }
}

/* --------------------------------------------------------- anchor mapping */

/// Re-resolve one anchor against the revision it names.
///
/// Verification is never inherited from the row that stored it: a transcript
/// edit after the citation was written is exactly the case this has to catch
/// (spec §9.6A).
fn anchor_dto(anchor: &SourceAnchor, revision: &TranscriptRevision) -> AnchorDto {
    match verify_anchor(anchor, revision) {
        AnchorVerification::Verified {
            seek_ms,
            alignment_origin,
        } => AnchorDto {
            seq_index: anchor.seq_index,
            char_start: anchor.char_start,
            char_end: anchor.char_end,
            excerpt: anchor.excerpt.clone(),
            verified: true,
            rejection_code: None,
            matched_text: span_text(revision, anchor),
            alignment_origin: alignment_str(alignment_origin).to_owned(),
            seek_ms,
        },
        AnchorVerification::Rejected(reason) => AnchorDto {
            seq_index: anchor.seq_index,
            char_start: anchor.char_start,
            char_end: anchor.char_end,
            excerpt: anchor.excerpt.clone(),
            verified: false,
            rejection_code: Some(reason.as_str().to_owned()),
            matched_text: None,
            alignment_origin: "none".to_owned(),
            seek_ms: None,
        },
    }
}

/// The text the revision actually holds at an anchor's span.
fn span_text(revision: &TranscriptRevision, anchor: &SourceAnchor) -> Option<String> {
    let segment = revision
        .segments()
        .iter()
        .find(|segment| segment.seq_index == anchor.seq_index)?;
    let chars: Vec<char> = segment.text.chars().collect();
    let start = anchor.char_start as usize;
    let end = anchor.char_end as usize;
    if start >= end || end > chars.len() {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// Map a refused anchor to the wire form, discriminator included.
fn rejected_anchor_dto(
    anchor: &SourceAnchor,
    rejection: plaudon_ai_tools::anchor::AnchorRejection,
) -> RejectedAnchorDto {
    RejectedAnchorDto {
        seq_index: anchor.seq_index,
        char_start: anchor.char_start,
        char_end: anchor.char_end,
        excerpt: anchor.excerpt.clone(),
        rejection_code: rejection.as_str().to_owned(),
    }
}

/// The definition row a run's foreign key resolves against.
fn definition_row(tool: &TranscriptToolDefinition, now: Timestamp) -> StoredToolDefinition {
    StoredToolDefinition {
        tool_id: tool.id.to_string(),
        version: tool.version,
        display_name: tool.display_name.clone(),
        kind: tool.kind.as_str().to_owned(),
        prompt_version: tool.prompt_version.clone(),
        output_schema_version: tool.output_schema_version.clone(),
        definition_sha256: tool.definition_sha256(),
        origin: tool.origin.as_str().to_owned(),
        optional: tool.optional,
        created_at: now,
    }
}

/// Flatten the two failure kinds a recorded conversation can have.
fn conversation_error(error: ConversationError) -> AppError {
    match error {
        ConversationError::Tool(error) => AppError::from(error),
        ConversationError::Persistence(error) => AppError::from(error),
    }
}

/// Position of the correct answer among a quiz item's options.
///
/// The stored artifact keeps the answer and its distractors apart; the wire
/// form presents one option list and an index. Putting the answer first every
/// time would make the key guessable without reading the recording, so the
/// position is derived from the question itself: stable across renders of the
/// same item, different between items, and never random.
fn answer_position(question: &str, option_count: usize) -> usize {
    if option_count <= 1 {
        return 0;
    }
    let spread: usize = question.bytes().map(usize::from).sum();
    spread % option_count
}

/// Build the wire form of one quiz item from its stored answer and
/// distractors.
fn quiz_item_dto(
    question: String,
    answer: String,
    distractors: Vec<String>,
    anchors: Vec<AnchorDto>,
) -> QuizItemDto {
    let position = answer_position(&question, distractors.len() + 1);
    let mut options = distractors;
    options.insert(position, answer);
    QuizItemDto {
        question,
        options,
        answer_index: u32::try_from(position).unwrap_or(0),
        anchors,
    }
}

/* ----------------------------------------------------------- backend glue */

impl LocalBackend {
    /// Install the text model every tool run uses, instead of resolving a
    /// route.
    ///
    /// The counterpart of
    /// [`with_cloud_transport`](LocalBackend::with_cloud_transport): a test
    /// drives the whole ask and study path against a scripted model with no
    /// network, no key and no catalogue. Production never calls it.
    #[must_use]
    pub fn with_text_model(mut self, model: Arc<dyn TextModel>) -> Self {
        self.cloud = self.cloud.clone().with_text_model(model);
        self
    }

    /// The shipped tools plus the user's overrides.
    ///
    /// A malformed override is an error the user sees: `load_overrides_from`
    /// refuses the file rather than leaving the user convinced an edit took
    /// effect.
    fn tool_registry(&self) -> Result<ToolRegistry, AppError> {
        let mut registry = ToolRegistry::with_defaults();
        registry
            .load_overrides_from(&self.store().root().join(TOOL_OVERRIDE_DIR))
            .map_err(AppError::from)?;
        Ok(registry)
    }

    /// One tool at its highest version.
    fn latest_tool(
        registry: &ToolRegistry,
        tool_id: &str,
    ) -> Result<TranscriptToolDefinition, AppError> {
        registry
            .latest(&ToolId::new(tool_id))
            .cloned()
            .ok_or_else(|| AppError::from(ToolError::UnknownTool))
    }

    /// Which text model would answer, without dispatching anything.
    ///
    /// Nothing here opens a job row, reads the key or runs the consent
    /// preflight: a list of tools is not a dispatch, and treating it as one
    /// would make opening a panel a billable event.
    async fn describe_text_route(
        &self,
        recording_id: Option<RecordingId>,
    ) -> Result<Option<RouteDescription>, AppError> {
        if let Some(model) = self.cloud.text_model() {
            let capabilities = model.capabilities().await;
            return Ok(Some(RouteDescription {
                engine: capabilities.engine_id.as_str().to_owned(),
                model_id: Some(capabilities.model.model_id.clone()),
                is_cloud: !matches!(
                    capabilities.privacy_class,
                    Known::Known(PrivacyClass::OnDevice)
                ),
                unavailable_reason: (capabilities.status != CapabilityStatus::Supported)
                    .then(|| "the installed text model reports itself unavailable".to_owned()),
                capabilities,
            }));
        }

        let scope = recording_id.map(|id| id.to_string()).unwrap_or_default();
        let effective = self
            .cloud_ai()
            .effective_choice(&scope, Stage::ToolRun, None)
            .await
            .map_err(|err| internal(format!("cannot resolve the tool-run model: {err}")))?;

        match effective.map(|effective| effective.choice) {
            Some(choice) if choice.engine == OPENROUTER_ENGINE_ID => {
                let Some(model_id) = choice.model_id.clone() else {
                    return Ok(Some(RouteDescription {
                        engine: choice.engine,
                        model_id: None,
                        is_cloud: true,
                        capabilities: TextModelCapabilities::unknown(
                            EngineId::new(OPENROUTER_ENGINE_ID),
                            ModelIdentity::new(EngineId::new(OPENROUTER_ENGINE_ID), String::new()),
                        ),
                        unavailable_reason: Some(
                            "the cloud engine is selected without a model".to_owned(),
                        ),
                    }));
                };
                let card = self.card_for(ModelTask::TextGeneration, &model_id).await?;
                let capabilities = cloud_capabilities(&card);
                let unavailable_reason = if !self
                    .secrets()
                    .openrouter_key_present()
                    .unwrap_or(false)
                {
                    Some("no OpenRouter key is stored".to_owned())
                } else if !card.verification.is_verified() {
                    Some("this model has not passed a capability check on this machine".to_owned())
                } else {
                    None
                };
                Ok(Some(RouteDescription {
                    engine: choice.engine,
                    model_id: Some(model_id),
                    is_cloud: true,
                    capabilities,
                    unavailable_reason,
                }))
            }
            Some(choice) if choice.engine == plaudon_ai_apple::APPLE_FOUNDATION_ENGINE => {
                Ok(Some(self.apple_route().await))
            }
            Some(choice) => Ok(Some(RouteDescription {
                engine: choice.engine.clone(),
                model_id: choice.model_id.clone(),
                is_cloud: false,
                capabilities: TextModelCapabilities::unknown(
                    EngineId::new(choice.engine.clone()),
                    ModelIdentity::new(
                        EngineId::new(choice.engine.clone()),
                        choice.model_id.unwrap_or_default(),
                    ),
                ),
                unavailable_reason: Some(format!(
                    "this build cannot run transcript tools on the {} engine",
                    choice.engine
                )),
            })),
            // Nothing configured: the on-device model is the only route that
            // needs no configuration at all, so it is the fallback.
            None => {
                let apple = self.apple_route().await;
                Ok(apple.available().then_some(apple))
            }
        }
    }

    /// The Apple on-device route as the probe and the capability evidence
    /// currently report it.
    ///
    /// The Foundation Models framework publishes no output ceiling, so the
    /// probe alone can never satisfy a tool that needs one. The only thing
    /// that can establish it is an on-device generation that answered within
    /// a declared budget, which `verify_model` records exactly like a cloud
    /// probe; that evidence is read back here. Nothing else is taken from
    /// the stored card: availability is a live fact, not a remembered one.
    async fn apple_route(&self) -> RouteDescription {
        let cache = crate::backend::capabilities::apple_probe_cache();
        let (availability, _) = cache.availability(None);
        let mut capabilities =
            plaudon_ai_apple::probe::text_model_capabilities(&availability.text_model);
        if !capabilities.max_output_tokens.is_known() {
            if let Ok(card) = self
                .card_for(
                    ModelTask::TextGeneration,
                    plaudon_ai_apple::APPLE_FOUNDATION_ENGINE,
                )
                .await
            {
                if card.verification.is_verified() {
                    capabilities.max_output_tokens = card.max_output_tokens.clone();
                }
            }
        }
        let unavailable_reason = (capabilities.status != CapabilityStatus::Supported).then(|| {
            plaudon_app_core::engine_policy::explain(
                &capabilities.engine_id,
                &capabilities.availability_reason,
            )
            .headline
        });
        RouteDescription {
            engine: plaudon_ai_apple::APPLE_FOUNDATION_ENGINE.to_owned(),
            model_id: Some(capabilities.model.model_id.clone()),
            is_cloud: false,
            capabilities,
            unavailable_reason,
        }
    }

    /// Everything that must pass before one tool run may dispatch.
    ///
    /// The order is the contract: the capability check first (so an impossible
    /// run never opens a job row), then — for the cloud only — the key, the
    /// budget, the egress gate and the consent grant, and only then the job
    /// row the ledger's foreign key resolves against.
    async fn resolve_text_model(
        &self,
        recording_id: RecordingId,
        tool: &TranscriptToolDefinition,
        model_override: Option<&ModelChoice>,
        cost_acknowledged: bool,
        prompt_for_estimate: Option<&str>,
    ) -> Result<ResolvedRoute, AppError> {
        let local_only = self.preferences().await?.local_only_mode;

        if let Some(model) = self.cloud.text_model() {
            let capabilities = model.capabilities().await;
            tool.ensure_runnable(&capabilities)
                .map_err(AppError::from)?;
            let model = Arc::clone(model);
            // The seam replaces the model, never the policy. A scripted model
            // that reports a cloud route still passes the key, the budget, the
            // egress gate and the consent check, because that is the part a
            // test must be able to exercise.
            if !capabilities
                .privacy_class
                .get()
                .copied()
                .is_some_and(PrivacyClass::requires_cloud_consent)
            {
                return Ok(ResolvedRoute {
                    consent_policy: posture_for(&capabilities, local_only, None),
                    model,
                    capabilities,
                    consent_id: None,
                    job_id: None,
                });
            }
            let model_id = capabilities.model.model_id.clone();
            let (plan, job_id) = self
                .cloud_preflight(
                    recording_id,
                    tool,
                    &model_id,
                    None,
                    prompt_for_estimate,
                    cost_acknowledged,
                )
                .await?;
            return Ok(ResolvedRoute {
                consent_policy: posture_for(&capabilities, local_only, None),
                consent_id: Some(plan.grant_id),
                model,
                capabilities,
                job_id: Some(job_id),
            });
        }

        let effective = self
            .cloud_ai()
            .effective_choice(&recording_id.to_string(), Stage::ToolRun, model_override)
            .await
            .map_err(|err| internal(format!("cannot resolve the tool-run model: {err}")))?
            .map(|effective| effective.choice);

        let choice = match effective {
            Some(choice) => choice,
            None => ModelChoice::new(plaudon_ai_apple::APPLE_FOUNDATION_ENGINE, None),
        };

        if choice.engine == plaudon_ai_apple::APPLE_FOUNDATION_ENGINE {
            return self.apple_model(tool, local_only).await;
        }
        if choice.engine != OPENROUTER_ENGINE_ID {
            return Err(AppError::new(
                AppErrorCode::ModelUnavailable,
                format!(
                    "this build cannot run transcript tools on the {} engine",
                    choice.engine
                ),
            ));
        }

        let model_id = choice.model_id.clone().ok_or_else(|| {
            AppError::new(
                AppErrorCode::CapabilityUnverified,
                "a cloud engine needs a model id",
            )
        })?;
        let card = self.card_for(ModelTask::TextGeneration, &model_id).await?;
        let capabilities = cloud_capabilities(&card);
        tool.ensure_runnable(&capabilities)
            .map_err(AppError::from)?;

        let (plan, job_id) = self
            .cloud_preflight(
                recording_id,
                tool,
                &model_id,
                Some(&card.pricing),
                prompt_for_estimate,
                cost_acknowledged,
            )
            .await?;

        let binding = JobBinding {
            job_id: job_id.to_string(),
            recording_id: recording_id.to_string(),
            consent_id: plan.grant_id.clone(),
            privacy_snapshot: plan.resolution.snapshot(),
            route_label: plan.resolution.route.downstream_provider.clone(),
        };
        let model = OpenRouterTextModel::new(card, self.dispatcher(&plan), binding)
            .with_now_unix(now_unix());
        Ok(ResolvedRoute {
            model: Arc::new(model) as Arc<dyn TextModel>,
            consent_policy: posture_for(&capabilities, local_only, None),
            consent_id: Some(plan.grant_id),
            capabilities,
            job_id: Some(job_id),
        })
    }

    /// The key, the budget, the egress gate, the consent grant and the job
    /// row — in that order — for one cloud tool run.
    ///
    /// Nothing has been transmitted when this returns: the plan is the
    /// permission to transmit, and the job row exists so the attempt ledger's
    /// foreign key resolves when the dispatcher writes its row.
    async fn cloud_preflight(
        &self,
        recording_id: RecordingId,
        tool: &TranscriptToolDefinition,
        model_id: &str,
        pricing: Option<&plaudon_ai_openrouter::pricing::PricingUnits>,
        prompt_for_estimate: Option<&str>,
        cost_acknowledged: bool,
    ) -> Result<(crate::backend::cloud::DispatchPlan, JobId), AppError> {
        self.require_key()?;
        // The dispatched prompt is the only thing a text charge is derived
        // from. A tool whose prompt is assembled downstream (retrieval happens
        // inside `ask_recording`) therefore has no estimate at all, and
        // `Unknown` is the honest value: §10.4 makes an unknown charge the
        // user's decision, never a zero.
        let estimated = match (pricing, prompt_for_estimate) {
            (Some(pricing), Some(prompt)) => {
                estimate(
                    pricing,
                    EstimateInput::Text {
                        prompt_tokens: approximate_tokens(prompt),
                        max_output_tokens: tool.required_capabilities.min_output_tokens,
                    },
                )
                .amount
            }
            _ => MoneyEstimate::Unknown,
        };
        self.check_budget(estimated, cost_acknowledged).await?;

        let plan = self
            .authorize_dispatch(
                &recording_id.to_string(),
                model_id,
                ContentType::TranscriptText,
            )
            .await?;

        let job_id = jobs::create(
            self.pool(),
            &JobDraft {
                kind: JobKind::Summarize,
                label: format!("{} (cloud tool run)", tool.display_name),
                recording_id: Some(recording_id),
                config_snapshot: json!({
                    "engine": OPENROUTER_ENGINE_ID,
                    "model_id": model_id,
                    "stage": Stage::ToolRun.as_str(),
                    "tool_id": tool.id.as_str(),
                    "tool_version": tool.version,
                }),
            },
        )
        .await?;
        jobs::transition(
            self.pool(),
            job_id,
            JobState::CloudReady,
            Some("ready to send"),
            None,
        )
        .await?;
        Ok((plan, job_id))
    }

    /// The Apple on-device model, which needs no consent, no job row and no
    /// budget because nothing leaves the device.
    async fn apple_model(
        &self,
        tool: &TranscriptToolDefinition,
        local_only: bool,
    ) -> Result<ResolvedRoute, AppError> {
        let route = self.apple_route().await;
        if let Some(reason) = route.unavailable_reason {
            return Err(AppError::new(AppErrorCode::ModelUnavailable, reason));
        }
        tool.ensure_runnable(&route.capabilities)
            .map_err(AppError::from)?;
        let model = plaudon_ai_apple::text::AppleTextModel::new(
            plaudon_ai_apple::host_bridge(),
            crate::backend::capabilities::apple_probe_cache(),
        );
        Ok(ResolvedRoute {
            model: Arc::new(model) as Arc<dyn TextModel>,
            consent_policy: ConsentPolicySnapshot::on_device(local_only),
            consent_id: None,
            capabilities: route.capabilities,
            job_id: None,
        })
    }

    /// Close the job row a cloud tool run opened.
    async fn settle_tool_job(&self, job_id: Option<JobId>, outcome: Result<(), &AppError>) {
        let Some(job_id) = job_id else {
            return;
        };
        let result = match outcome {
            Ok(()) => {
                jobs::transition(self.pool(), job_id, JobState::Completed, Some("done"), None).await
            }
            Err(error) => jobs::fail(self.pool(), job_id, error).await.map(|_| ()),
        };
        if let Err(error) = result {
            tracing::warn!(
                code = error.code().as_str(),
                "a tool run's job row was not settled"
            );
        }
    }

    /// Rebuild the domain revision a tool reads, carrying its stored identity.
    ///
    /// The requested revision must belong to the requested recording: a
    /// citation that resolved against someone else's transcript would verify
    /// and still be wrong, so a foreign revision is
    /// [`AppErrorCode::RecordNotFound`] rather than a successful answer.
    pub(super) async fn domain_revision(
        &self,
        recording_id: RecordingId,
        revision_id: Option<TranscriptRevisionId>,
    ) -> Result<TranscriptRevision, AppError> {
        let revision_id = match revision_id {
            Some(id) => id,
            None => self.head_revision(recording_id).await?.ok_or_else(|| {
                AppError::new(
                    AppErrorCode::RecordNotFound,
                    "this recording has no transcript revision",
                )
            })?,
        };
        self.revision_by_id(recording_id, revision_id).await
    }

    /// Rebuild one revision by identity, checking it belongs to `recording_id`.
    async fn revision_by_id(
        &self,
        recording_id: RecordingId,
        revision_id: TranscriptRevisionId,
    ) -> Result<TranscriptRevision, AppError> {
        let row = sqlx::query(
            "SELECT recording_id, parent_id, origin, input_sha256, engine, model_id, \
             alignment_origin, normalization_profile, chunking_strategy, duration_ms, created_at \
             FROM transcript_revisions WHERE id = ?1",
        )
        .bind(revision_id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(|err| storage_error("cannot read a transcript revision", &err))?
        .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such revision"))?;

        let owner: String = row.try_get("recording_id").map_err(row_error)?;
        if owner != recording_id.to_string() {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "that revision belongs to another recording",
            ));
        }

        let alignment = parse_alignment(
            &row.try_get::<String, _>("alignment_origin")
                .map_err(row_error)?,
        );
        let digest: String = row.try_get("input_sha256").map_err(row_error)?;
        let parent: Option<String> = row.try_get("parent_id").map_err(row_error)?;
        let engine: String = row.try_get("engine").map_err(row_error)?;
        let model_id: String = row.try_get("model_id").map_err(row_error)?;
        let origin = parse_origin(&row.try_get::<String, _>("origin").map_err(row_error)?);

        let segments = self.domain_segments(revision_id, alignment).await?;
        let draft = TranscriptRevisionDraft {
            recording_id,
            parent_id: parent.and_then(|value| value.parse().ok()),
            origin,
            input_sha256: digest.parse().unwrap_or_else(|_| Sha256Digest::of(b"")),
            engine: EngineId::new(engine.clone()),
            model: ModelIdentity::new(EngineId::new(engine), model_id),
            locale: None,
            alignment_origin: alignment,
            status: RevisionStatus::Complete,
            normalization_profile: row.try_get("normalization_profile").map_err(row_error)?,
            chunking_strategy: row.try_get("chunking_strategy").map_err(row_error)?,
            duration_ms: row
                .try_get::<Option<i64>, _>("duration_ms")
                .map_err(row_error)?
                .and_then(|value| u64::try_from(value).ok()),
            created_at: Timestamp::from_millis(row.try_get("created_at").map_err(row_error)?),
            segments: segments.clone(),
        };

        // A revision whose stored times do not satisfy the domain invariant is
        // read without times rather than refused: the text is still citable,
        // and a citation into it simply offers no seek control (spec §8.6A).
        let mut revision = match TranscriptRevision::try_new(draft.clone()) {
            Ok(revision) => revision,
            Err(_) => TranscriptRevision::try_new(TranscriptRevisionDraft {
                alignment_origin: AlignmentOrigin::None,
                segments: segments
                    .into_iter()
                    .map(|segment| TranscriptSegment::text_only(segment.seq_index, segment.text))
                    .collect(),
                ..draft
            })
            .map_err(|err| internal(format!("a stored revision is unreadable: {err}")))?,
        };
        revision.id = revision_id;
        Ok(revision)
    }

    /// Segments of one revision as domain values.
    async fn domain_segments(
        &self,
        revision_id: TranscriptRevisionId,
        alignment: AlignmentOrigin,
    ) -> Result<Vec<TranscriptSegment>, AppError> {
        let rows = sqlx::query(
            "SELECT seq_index, start_ms, end_ms, text FROM transcript_segments \
             WHERE transcript_revision_id = ?1 ORDER BY seq_index ASC",
        )
        .bind(revision_id.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot read transcript segments", &err))?;

        rows.iter()
            .map(|row| {
                let seq_index = as_u32("seq_index", row.try_get("seq_index").map_err(row_error)?)?;
                let text: String = row.try_get("text").map_err(row_error)?;
                let start: Option<i64> = row.try_get("start_ms").map_err(row_error)?;
                let end: Option<i64> = row.try_get("end_ms").map_err(row_error)?;
                Ok(match (alignment.permits_times(), start, end) {
                    (true, Some(start), Some(end)) => TranscriptSegment::timed(
                        seq_index,
                        as_u64("start_ms", start)?,
                        as_u64("end_ms", end)?,
                        text,
                    ),
                    _ => TranscriptSegment::text_only(seq_index, text),
                })
            })
            .collect()
    }

    /// Revisions a listing cites, loaded once each.
    async fn revision_cache(
        &self,
        recording_id: RecordingId,
        ids: &[TranscriptRevisionId],
    ) -> BTreeMap<TranscriptRevisionId, TranscriptRevision> {
        let mut cache = BTreeMap::new();
        for id in ids {
            if cache.contains_key(id) {
                continue;
            }
            if let Ok(revision) = self.revision_by_id(recording_id, *id).await {
                cache.insert(*id, revision);
            }
        }
        cache
    }

    /// Version of the definition that produced a stored output.
    ///
    /// Resolved from the definition rows by the exact prompt and schema
    /// versions the output recorded. An output whose definition is no longer
    /// registered reports version `0`, which is outside the range a real
    /// definition can have (`>= 1`), rather than naming a version that may
    /// have been produced by different instructions.
    async fn stored_tool_version(
        &self,
        tool_id: &str,
        prompt_version: &str,
        schema_version: &str,
    ) -> u32 {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM transcript_tools WHERE tool_id = ?1 AND prompt_version = ?2 \
             AND output_schema_version = ?3 ORDER BY version DESC LIMIT 1",
        )
        .bind(tool_id)
        .bind(prompt_version)
        .bind(schema_version)
        .fetch_optional(self.pool())
        .await
        .ok()
        .flatten();
        found
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0)
    }
}

/// Map a column read failure that the schema should have made impossible.
fn row_error(err: sqlx::Error) -> AppError {
    storage_error("cannot read a stored column", &err)
}

/// Parse a stored alignment origin, defaulting to "no times at all".
fn parse_alignment(value: &str) -> AlignmentOrigin {
    match value {
        "native" => AlignmentOrigin::Native,
        "derived" => AlignmentOrigin::Derived,
        _ => AlignmentOrigin::None,
    }
}

/// Parse a stored revision origin.
fn parse_origin(value: &str) -> RevisionOrigin {
    match value {
        "human_edit" => RevisionOrigin::HumanEdit,
        "derived_alignment" => RevisionOrigin::DerivedAlignment,
        "overlap_reconciliation" => RevisionOrigin::OverlapReconciliation,
        _ => RevisionOrigin::Machine,
    }
}

/* ----------------------------------------------------------------- port */

#[async_trait]
impl ToolsPort for LocalBackend {
    async fn list_transcript_tools(
        &self,
        recording_id: Option<RecordingId>,
    ) -> Result<ListTranscriptToolsResponse, AppError> {
        let registry = self.tool_registry()?;
        let route = self.describe_text_route(recording_id).await?;

        let tools = registry
            .latest_all()
            .into_iter()
            .map(|tool| {
                let (offerable, unavailable_reason) = match route.as_ref() {
                    None => (
                        false,
                        Some("no text model is configured for transcript tools".to_owned()),
                    ),
                    Some(route) if !route.available() => (false, route.unavailable_reason.clone()),
                    Some(route) => match tool.check_capabilities(&route.capabilities) {
                        Ok(()) => (true, None),
                        Err(gaps) => (
                            false,
                            Some(gaps.iter().map(gap_text).collect::<Vec<_>>().join("; ")),
                        ),
                    },
                };
                TranscriptToolDto {
                    tool_id: tool.id.to_string(),
                    version: tool.version,
                    kind: tool.kind.as_str().to_owned(),
                    display_name: tool.display_name.clone(),
                    instruction: tool.instruction.clone(),
                    prompt_version: tool.prompt_version.clone(),
                    output_schema_version: tool.output_schema_version.clone(),
                    origin: origin_label(tool.origin).to_owned(),
                    optional: tool.optional,
                    offerable,
                    unavailable_reason,
                }
            })
            .collect();

        Ok(ListTranscriptToolsResponse {
            tools,
            text_model: route.as_ref().map(RouteDescription::to_dto),
        })
    }

    async fn ask_recording(
        &self,
        request: ValidatedAskRecording,
    ) -> Result<AskRecordingResponse, AppError> {
        let registry = self.tool_registry()?;
        let tool = Self::latest_tool(&registry, "ask_recording")?;
        let revision = self
            .domain_revision(request.recording_id, request.revision_id)
            .await?;
        let route = self
            .resolve_text_model(
                request.recording_id,
                &tool,
                request.model_override.as_ref(),
                request.cost_acknowledged,
                None,
            )
            .await?;

        let now = Timestamp::now();
        let context = JobContext::new(route.job_id.unwrap_or_default());
        let answered = ConversationService::new(self.store().db().clone())
            // Text generation has no audio request family (App. A.3), which is
            // what the attempt ledger records for the same dispatch.
            .with_request_family(RequestFamily::Unsupported)
            .with_consent_policy(route.consent_policy.clone())
            .with_consent_id(route.consent_id.clone())
            .ask(
                route.model.as_ref(),
                &route.capabilities,
                &tool,
                &definition_row(&tool, now),
                request.recording_id,
                &revision,
                &request.question,
                context,
                now,
            )
            .await
            .map_err(conversation_error);

        let answered = match answered {
            Ok(answered) => {
                self.settle_tool_job(route.job_id, Ok(())).await;
                answered
            }
            Err(error) => {
                self.settle_tool_job(route.job_id, Err(&error)).await;
                return Err(error);
            }
        };

        Ok(AskRecordingResponse {
            conversation_id: answered.conversation_id.to_string(),
            transcript_revision_id: revision.id.to_string(),
            outcome: answered.answer.outcome.as_str().to_owned(),
            text: answered.answer.text.clone(),
            anchors: answered
                .answer
                .anchors
                .iter()
                .map(|anchor| anchor_dto(anchor, &revision))
                .collect(),
            rejected_anchors: answered
                .answer
                .rejected_anchors
                .iter()
                .map(|(anchor, rejection)| rejected_anchor_dto(anchor, *rejection))
                .collect(),
            retrieved_segments: answered.answer.retrieved.clone(),
            dispatched: answered.answer.dispatched,
            tool_run_id: answered.tool_run_id.map(|id| id.to_string()),
            tool_id: tool.id.to_string(),
            tool_version: tool.version,
        })
    }

    async fn list_conversations(
        &self,
        recording_id: RecordingId,
    ) -> Result<ListConversationsResponse, AppError> {
        let rows = sqlx::query(
            "SELECT id, transcript_revision_id, created_at FROM recording_conversations \
             WHERE recording_id = ?1 ORDER BY created_at DESC, id DESC",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot read conversations", &err))?;

        let mut headers = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.try_get("id").map_err(row_error)?;
            let revision: String = row.try_get("transcript_revision_id").map_err(row_error)?;
            let created_at: i64 = row.try_get("created_at").map_err(row_error)?;
            let id: uuid::Uuid = id
                .parse()
                .map_err(|_| internal("a stored conversation id is not a uuid"))?;
            let revision: TranscriptRevisionId = revision
                .parse()
                .map_err(|_| internal("a stored revision id is not a uuid"))?;
            headers.push((id, revision, created_at));
        }

        let head = self.head_revision(recording_id).await?;
        let revisions = self
            .revision_cache(
                recording_id,
                &headers
                    .iter()
                    .map(|(_, revision, _)| *revision)
                    .collect::<Vec<_>>(),
            )
            .await;
        let repository = ToolRepository::new(self.store().db().clone());

        let mut conversations = Vec::with_capacity(headers.len());
        for (id, revision_id, created_at) in headers {
            let messages = repository.messages(id).await.map_err(AppError::from)?;
            let revision = revisions.get(&revision_id);
            let mut turns = Vec::with_capacity(messages.len());
            for message in messages {
                let anchors: Vec<SourceAnchor> =
                    serde_json::from_str(&message.source_anchors_json).unwrap_or_default();
                turns.push(ConversationTurnDto {
                    message_id: message.id.to_string(),
                    seq_index: message.seq_index,
                    role: message.role,
                    text: message.text,
                    outcome: message.outcome,
                    anchors: revision.map_or_else(Vec::new, |revision| {
                        anchors
                            .iter()
                            .map(|anchor| anchor_dto(anchor, revision))
                            .collect()
                    }),
                    tool_run_id: message.tool_run_id.map(|id| id.to_string()),
                    created_at: rfc3339("created_at", message.created_at.as_millis())?,
                });
            }
            conversations.push(ConversationDto {
                conversation_id: id.to_string(),
                recording_id: recording_id.to_string(),
                transcript_revision_id: revision_id.to_string(),
                revision_is_current: head == Some(revision_id),
                created_at: rfc3339("created_at", created_at)?,
                turns,
            });
        }
        Ok(ListConversationsResponse { conversations })
    }

    async fn generate_study_artifacts(
        &self,
        request: ValidatedGenerateStudyArtifacts,
    ) -> Result<GenerateStudyArtifactsResponse, AppError> {
        let registry = self.tool_registry()?;
        let tool = Self::latest_tool(&registry, &request.tool_id)?;
        if study::output_type_for(tool.kind).is_none() {
            return Err(invalid_argument(
                "that tool does not produce study artifacts",
            ));
        }
        let revision = self
            .domain_revision(request.recording_id, request.revision_id)
            .await?;

        let segments = revision.segments();
        let characters: usize = segments
            .iter()
            .map(|segment| segment.text.chars().count())
            .sum();
        if segments.len() > MAX_STUDY_SEGMENTS || characters > MAX_STUDY_PROMPT_CHARS {
            return Err(AppError::new(
                AppErrorCode::ContextTooLarge,
                "this transcript is longer than one study run may send",
            ));
        }
        let passages: Vec<FencedPassage> = segments
            .iter()
            .map(|segment| FencedPassage {
                seq_index: segment.seq_index,
                text: segment.text.clone(),
            })
            .collect();
        let prompt = build_grounded_prompt(&tool.instruction, None, &passages);

        let route = self
            .resolve_text_model(
                request.recording_id,
                &tool,
                request.model_override.as_ref(),
                request.cost_acknowledged,
                Some(&prompt.text),
            )
            .await?;

        let outcome = self
            .execute_study_run(&tool, &revision, &prompt.text, &route)
            .await;
        match &outcome {
            Ok(_) => self.settle_tool_job(route.job_id, Ok(())).await,
            Err(error) => self.settle_tool_job(route.job_id, Err(error)).await,
        }
        outcome
    }

    async fn list_study_artifacts(
        &self,
        recording_id: RecordingId,
    ) -> Result<ListStudyArtifactsResponse, AppError> {
        let rows = sqlx::query(
            "SELECT id, output_type, transcript_revision_id, prompt_version, schema_version, \
             body_json, created_at FROM ai_outputs \
             WHERE recording_id = ?1 AND output_type IN ('flashcards','quiz','definitions') \
             ORDER BY created_at DESC, id DESC",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot read study outputs", &err))?;

        let head = self.head_revision(recording_id).await?;
        let mut ids = Vec::with_capacity(rows.len());
        for row in &rows {
            let raw: String = row.try_get("transcript_revision_id").map_err(row_error)?;
            if let Ok(id) = raw.parse::<TranscriptRevisionId>() {
                ids.push(id);
            }
        }
        let revisions = self.revision_cache(recording_id, &ids).await;

        let mut sets = Vec::with_capacity(rows.len());
        for row in &rows {
            let output_id: String = row.try_get("id").map_err(row_error)?;
            let output_type: String = row.try_get("output_type").map_err(row_error)?;
            let revision_raw: String = row.try_get("transcript_revision_id").map_err(row_error)?;
            let prompt_version: String = row.try_get("prompt_version").map_err(row_error)?;
            let schema_version: String = row.try_get("schema_version").map_err(row_error)?;
            let body: String = row.try_get("body_json").map_err(row_error)?;
            let created_at: i64 = row.try_get("created_at").map_err(row_error)?;

            let revision_id: TranscriptRevisionId = revision_raw
                .parse()
                .map_err(|_| internal("a stored revision id is not a uuid"))?;
            let evidence = self.evidence_for(&output_id, revision_id).await?;
            let body: serde_json::Value = serde_json::from_str(&body)
                .map_err(|_| internal("a stored study output is not json"))?;
            let artifacts = revisions
                .get(&revision_id)
                .map_or_else(StudyArtifactsDto::default, |revision| {
                    artifacts_from_body(&body, &evidence, revision)
                });

            sets.push(StudyArtifactSetDto {
                output_id,
                tool_id: output_type.clone(),
                tool_version: self
                    .stored_tool_version(&output_type, &prompt_version, &schema_version)
                    .await,
                transcript_revision_id: revision_id.to_string(),
                revision_is_current: head == Some(revision_id),
                created_at: rfc3339("created_at", created_at)?,
                artifacts,
            });
        }
        Ok(ListStudyArtifactsResponse { sets })
    }

    async fn list_tool_runs(
        &self,
        recording_id: RecordingId,
    ) -> Result<ListToolRunsResponse, AppError> {
        let repository = ToolRepository::new(self.store().db().clone());
        let stored = repository
            .runs_for(recording_id)
            .await
            .map_err(AppError::from)?;

        let rows = sqlx::query(
            "SELECT id, model_version, consent_policy_json FROM tool_runs WHERE recording_id = ?1",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot read tool runs", &err))?;
        let mut extra: BTreeMap<String, (Option<String>, String)> = BTreeMap::new();
        for row in &rows {
            let id: String = row.try_get("id").map_err(row_error)?;
            let model_version: Option<String> = row.try_get("model_version").map_err(row_error)?;
            let policy: String = row.try_get("consent_policy_json").map_err(row_error)?;
            extra.insert(id, (model_version, policy));
        }

        let mut runs = Vec::with_capacity(stored.len());
        for run in stored {
            let key = run.id.to_string();
            let (model_version, policy) = extra
                .get(&key)
                .cloned()
                .unwrap_or_else(|| (None, "{}".to_owned()));
            runs.push(ToolRunDto {
                run_id: key,
                recording_id: run.recording_id.to_string(),
                transcript_revision_id: run.transcript_revision_id.to_string(),
                tool_id: run.tool_id,
                tool_version: run.tool_version,
                engine: run.engine,
                model_id: run.model_id,
                model_version,
                request_family: run.request_family,
                prompt_version: run.prompt_version,
                schema_version: run.schema_version,
                input_sha256: run.input_sha256,
                output_sha256: run.output_sha256,
                status: run.status,
                consent_policy: consent_policy_dto(&policy, run.consent_id.clone()),
                cost: crate::backend::cloud::cost_value(run.cost),
                created_at: rfc3339("created_at", run.created_at.as_millis())?,
                finished_at: rfc3339_opt("finished_at", run.finished_at.map(Timestamp::as_millis))?,
            });
        }
        Ok(ListToolRunsResponse { runs })
    }
}

impl LocalBackend {
    /// Dispatch one study run, validate it, store it and record it.
    ///
    /// Split out so the job row a cloud run opened is settled on every path
    /// out of [`ToolsPort::generate_study_artifacts`], including a refusal.
    async fn execute_study_run(
        &self,
        tool: &TranscriptToolDefinition,
        revision: &TranscriptRevision,
        prompt: &str,
        route: &ResolvedRoute,
    ) -> Result<GenerateStudyArtifactsResponse, AppError> {
        let now = Timestamp::now();
        let context = JobContext::new(route.job_id.unwrap_or_default());
        let response = route
            .model
            .generate_structured(
                StructuredRequest {
                    prompt_version: tool.prompt_version.clone(),
                    prompt: prompt.to_owned(),
                    schema_version: tool.output_schema_version.clone(),
                    output_schema: tool.output_schema.clone(),
                    max_output_tokens: Some(tool.required_capabilities.min_output_tokens),
                },
                context,
            )
            .await
            .map_err(|err| AppError::from(ToolError::from(err)))?;

        let run = ToolRunRecord::open(
            tool,
            ToolRunDraft {
                recording_id: revision.recording_id,
                transcript_revision_id: revision.id,
                model: route.capabilities.model.clone(),
                request_family: RequestFamily::Unsupported,
                prompt_text: prompt.to_owned(),
                consent_policy: route.consent_policy.clone(),
                estimated_cost: MoneyEstimate::Unknown,
                created_at: now,
            },
        );
        let repository = ToolRepository::new(self.store().db().clone());
        repository
            .register(&definition_row(tool, now))
            .await
            .map_err(AppError::from)?;

        let validated =
            OutputSchema::compile(tool.output_schema_version.clone(), &tool.output_schema)
                .and_then(|schema| schema.check(&response.body))
                .and_then(|()| study_artifacts_from_value(&response.body, revision));
        let validation = match validated {
            Ok(validation) => validation,
            Err(error) => {
                // The run happened and produced nothing usable; recording that
                // is the difference between "refused" and "never ran".
                self.record_tool_run(
                    &repository,
                    tool,
                    revision,
                    route,
                    &run.clone()
                        .closed(ToolRunStatus::Rejected, MoneyEstimate::Unknown, now),
                )
                .await?;
                return Err(AppError::from(error));
            }
        };

        let stored = if validation.accepted.is_empty() {
            plaudon_app_core::transcript_tools::StoredStudyOutput {
                output_ids: Vec::new(),
                evidence_rows: 0,
            }
        } else {
            StudyOutputWriter::new(self.store().db().clone())
                .persist(
                    &validation.accepted,
                    &OutputProvenance {
                        recording_id: revision.recording_id,
                        transcript_revision_id: revision.id,
                        model_id: route.capabilities.model.model_id.clone(),
                        prompt_version: tool.prompt_version.clone(),
                        schema_version: tool.output_schema_version.clone(),
                        created_at: now,
                    },
                )
                .await
                .map_err(AppError::from)?
        };

        let closed = if validation.accepted.is_empty() {
            run.clone()
                .closed(ToolRunStatus::Rejected, MoneyEstimate::Unknown, now)
        } else {
            run.clone()
                .succeeded(&response.body, MoneyEstimate::Unknown, now)
        };
        self.record_tool_run(&repository, tool, revision, route, &closed)
            .await?;

        let artifacts = StudyArtifactsDto {
            flashcards: validation
                .accepted
                .flashcards
                .iter()
                .map(|card| FlashcardDto {
                    front: card.front.clone(),
                    back: card.back.clone(),
                    anchors: card
                        .anchors
                        .iter()
                        .map(|anchor| anchor_dto(anchor, revision))
                        .collect(),
                })
                .collect(),
            quiz: validation
                .accepted
                .quiz
                .iter()
                .map(|item| {
                    quiz_item_dto(
                        item.question.clone(),
                        item.answer.clone(),
                        item.distractors.clone(),
                        item.anchors
                            .iter()
                            .map(|anchor| anchor_dto(anchor, revision))
                            .collect(),
                    )
                })
                .collect(),
            definitions: validation
                .accepted
                .definitions
                .iter()
                .map(|item| DefinitionDto {
                    term: item.term.clone(),
                    definition: item.definition.clone(),
                    anchors: item
                        .anchors
                        .iter()
                        .map(|anchor| anchor_dto(anchor, revision))
                        .collect(),
                })
                .collect(),
        };

        Ok(GenerateStudyArtifactsResponse {
            tool_id: tool.id.to_string(),
            tool_version: tool.version,
            transcript_revision_id: revision.id.to_string(),
            artifacts,
            rejected: validation
                .rejected
                .iter()
                .map(|item| RejectedStudyItemDto {
                    path: item.item_path.clone(),
                    reason: rejection_reason_code(&item.reason).to_owned(),
                    detail: rejection_detail(&item.reason),
                })
                .collect(),
            output_ids: stored.output_ids.iter().map(ToString::to_string).collect(),
            tool_run_id: Some(closed.id.to_string()),
            dispatched: route.consent_policy.privacy_class != PrivacyClass::OnDevice,
        })
    }

    /// Store one finished run so `list_tool_runs` shows it.
    async fn record_tool_run(
        &self,
        repository: &ToolRepository,
        tool: &TranscriptToolDefinition,
        revision: &TranscriptRevision,
        route: &ResolvedRoute,
        run: &ToolRunRecord,
    ) -> Result<(), AppError> {
        repository
            .record_run(&ToolRunInsert {
                id: run.id,
                recording_id: revision.recording_id,
                transcript_revision_id: revision.id,
                tool_id: tool.id.to_string(),
                tool_version: tool.version,
                engine: route.capabilities.engine_id.as_str().to_owned(),
                model_id: route.capabilities.model.model_id.clone(),
                model_version: route.capabilities.model.model_version.clone(),
                request_family: "unsupported".to_owned(),
                prompt_version: tool.prompt_version.clone(),
                schema_version: tool.output_schema_version.clone(),
                input_sha256: run.input_sha256.clone(),
                output_sha256: run.output_sha256.clone(),
                status: run.status.as_str().to_owned(),
                consent_id: route.consent_id.clone(),
                consent_policy_json: serde_json::to_string(&run.consent_policy)
                    .unwrap_or_else(|_| "{}".to_owned()),
                cost: run.actual_cost,
                created_at: run.created_at,
                finished_at: run.finished_at,
            })
            .await
            .map_err(AppError::from)
    }

    /// Stored citations of one output, bound to the revision they name.
    async fn evidence_for(
        &self,
        output_id: &str,
        revision_id: TranscriptRevisionId,
    ) -> Result<BTreeMap<String, Vec<SourceAnchor>>, AppError> {
        let rows = sqlx::query(
            "SELECT item_path, cited_revision_id, seq_index, char_start, char_end, quote \
             FROM output_evidence WHERE output_id = ?1 ORDER BY item_path ASC, position ASC",
        )
        .bind(output_id)
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot read output evidence", &err))?;

        let mut grouped: BTreeMap<String, Vec<SourceAnchor>> = BTreeMap::new();
        for row in &rows {
            let item_path: String = row.try_get("item_path").map_err(row_error)?;
            let cited: String = row.try_get("cited_revision_id").map_err(row_error)?;
            let anchor = SourceAnchor::new(
                cited.parse().unwrap_or(revision_id),
                as_u32("seq_index", row.try_get("seq_index").map_err(row_error)?)?,
                as_u32("char_start", row.try_get("char_start").map_err(row_error)?)?,
                as_u32("char_end", row.try_get("char_end").map_err(row_error)?)?,
                row.try_get::<String, _>("quote").map_err(row_error)?,
            );
            grouped.entry(item_path).or_default().push(anchor);
        }
        Ok(grouped)
    }
}

/// Stable discriminator of a refused study item.
const fn rejection_reason_code(reason: &plaudon_ai_tools::study::RejectionReason) -> &'static str {
    use plaudon_ai_tools::study::RejectionReason as Reason;
    match reason {
        Reason::Uncited => "uncited",
        Reason::AllAnchorsRejected { .. } => "all_anchors_rejected",
        Reason::EmptyField { .. } => "empty_field",
    }
}

/// What was wrong with a refused study item, in the user's terms.
fn rejection_detail(reason: &plaudon_ai_tools::study::RejectionReason) -> String {
    use plaudon_ai_tools::study::RejectionReason as Reason;
    match reason {
        Reason::Uncited => "the item carried no citation at all".to_owned(),
        Reason::AllAnchorsRejected { first } => format!(
            "every citation the item carried failed verification ({})",
            first.as_str()
        ),
        Reason::EmptyField { field } => format!("the {field} field was blank"),
    }
}

/// The consent posture a stored run recorded.
///
/// A row whose posture cannot be read reports an unresolved route with no
/// egress rather than asserting the run stayed on the device.
fn consent_policy_dto(policy_json: &str, consent_id: Option<String>) -> ConsentPolicyDto {
    match serde_json::from_str::<ConsentPolicySnapshot>(policy_json) {
        Ok(snapshot) => ConsentPolicyDto {
            egress_allowed: snapshot.privacy_class.requires_cloud_consent()
                && !snapshot.local_only_mode,
            privacy_class: privacy_label(snapshot.privacy_class).to_owned(),
            consent_id: snapshot.consent_id.map(|id| id.to_string()).or(consent_id),
        },
        Err(_) => ConsentPolicyDto {
            egress_allowed: false,
            privacy_class: privacy_label(PrivacyClass::Unresolved).to_owned(),
            consent_id,
        },
    }
}

/// Rebuild a stored study set from its body and its evidence rows.
///
/// The body holds the text of each item; the evidence rows hold the citations.
/// Every citation is re-verified against the revision it names, so an item
/// whose transcript has since changed arrives visibly unverified.
fn artifacts_from_body(
    body: &serde_json::Value,
    evidence: &BTreeMap<String, Vec<SourceAnchor>>,
    revision: &TranscriptRevision,
) -> StudyArtifactsDto {
    let anchors_for = |path: &str| -> Vec<AnchorDto> {
        evidence
            .get(path)
            .map(|anchors| {
                anchors
                    .iter()
                    .map(|anchor| anchor_dto(anchor, revision))
                    .collect()
            })
            .unwrap_or_default()
    };

    let flashcards = body
        .get("flashcards")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    Some(FlashcardDto {
                        front: item.get("front")?.as_str()?.to_owned(),
                        back: item.get("back")?.as_str()?.to_owned(),
                        anchors: anchors_for(&format!("/flashcards/{index}")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let quiz = body
        .get("quiz")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let distractors = item
                        .get("distractors")
                        .and_then(serde_json::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(quiz_item_dto(
                        item.get("question")?.as_str()?.to_owned(),
                        item.get("answer")?.as_str()?.to_owned(),
                        distractors,
                        anchors_for(&format!("/quiz/{index}")),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let definitions = body
        .get("definitions")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    Some(DefinitionDto {
                        term: item.get("term")?.as_str()?.to_owned(),
                        definition: item.get("definition")?.as_str()?.to_owned(),
                        anchors: anchors_for(&format!("/definitions/{index}")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    StudyArtifactsDto {
        flashcards,
        quiz,
        definitions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use parking_lot::Mutex;
    use plaudon_ai_contracts::capabilities::PricingBasis;
    use plaudon_ai_contracts::text::{StructuredResponse, TextModelError};
    use plaudon_domain::capability::Tri;

    use crate::backend::testing::{backend_fixture, Fixture};
    use crate::dto::DuplicatePolicy;
    use crate::ports::{ImportPort, MachineRevisionSpec, MachineSegment, TranscriptPort};

    const SEGMENTS: [&str; 2] = [
        "We will ship the migration on Friday.",
        "Idempotency means the same request can be retried without duplicating work.",
    ];

    /// Model identity the cloud-flavoured scripted model reports.
    const CLOUD_MODEL: &str = "vendor/model-a";

    /// A text model that answers with one scripted document and remembers
    /// every prompt it was given.
    ///
    /// It reports its own privacy class, which is what decides whether the
    /// backend treats it as an on-device route or puts it through the whole
    /// cloud gate. The seam replaces the model; it never replaces the policy.
    #[derive(Debug)]
    struct ScriptedModel {
        body: serde_json::Value,
        engine: &'static str,
        model_id: &'static str,
        privacy: PrivacyClass,
        prompts: Mutex<Vec<String>>,
    }

    impl ScriptedModel {
        /// A model that runs on this machine.
        fn new(body: serde_json::Value) -> Arc<Self> {
            Arc::new(Self {
                body,
                engine: "test-text-engine",
                model_id: "scripted",
                privacy: PrivacyClass::OnDevice,
                prompts: Mutex::new(Vec::new()),
            })
        }

        /// A model that claims a provider route, so every cloud check applies.
        fn cloud(body: serde_json::Value) -> Arc<Self> {
            Arc::new(Self {
                body,
                engine: OPENROUTER_ENGINE_ID,
                model_id: CLOUD_MODEL,
                privacy: PrivacyClass::CloudZeroRetention,
                prompts: Mutex::new(Vec::new()),
            })
        }

        fn prompt_count(&self) -> usize {
            self.prompts.lock().len()
        }
    }

    #[async_trait]
    impl TextModel for ScriptedModel {
        async fn capabilities(&self) -> TextModelCapabilities {
            let engine = EngineId::new(self.engine);
            TextModelCapabilities {
                payload_version: TextModelCapabilities::SUPPORTED_PAYLOAD_VERSION,
                status: CapabilityStatus::Supported,
                engine_id: engine.clone(),
                model: ModelIdentity::new(engine, self.model_id),
                offline: if self.privacy == PrivacyClass::OnDevice {
                    Tri::Yes
                } else {
                    Tri::No
                },
                max_context_tokens: Known::Known(128_000),
                max_output_tokens: Known::Known(8_192),
                supports_structured_output: Tri::Yes,
                can_cancel: Tri::Yes,
                privacy_class: Known::Known(self.privacy),
                pricing_basis: Known::Known(PricingBasis::None),
                availability_reason: AvailabilityReason::Available,
            }
        }

        async fn generate_structured(
            &self,
            request: StructuredRequest,
            _ctx: JobContext,
        ) -> Result<StructuredResponse, TextModelError> {
            self.prompts.lock().push(request.prompt);
            Ok(StructuredResponse {
                body: self.body.clone(),
                repair_attempts: 0,
                usage: None,
            })
        }
    }

    /// A library holding one recording with one two-segment revision.
    async fn library() -> (Fixture, RecordingId, TranscriptRevisionId) {
        let fixture = backend_fixture().await;
        let path = fixture.source_file("talk.wav", b"bytes of talk.wav");
        let response = fixture
            .backend
            .import_files(vec![path], DuplicatePolicy::Ask)
            .await
            .expect("the import runs");
        let crate::dto::ImportOutcome::Accepted { job_id, .. } = &response.results[0] else {
            panic!("the import was not accepted");
        };
        let recording_id = crate::backend::jobs::load(fixture.backend.pool(), *job_id)
            .await
            .expect("the job exists")
            .recording_id
            .expect("a committed import links its recording");

        let revision = fixture
            .backend
            .create_machine_revision(MachineRevisionSpec {
                recording_id,
                segments: vec![
                    MachineSegment::timed(SEGMENTS[0], 0, 1_000),
                    MachineSegment::timed(SEGMENTS[1], 1_000, 2_000),
                ],
                engine_id: "whisper.cpp".into(),
                model_id: "small".into(),
                alignment_origin: AlignmentOrigin::Native,
            })
            .await
            .expect("the revision is stored");
        (fixture, recording_id, revision.revision_id)
    }

    /// An anchor into `SEGMENTS[segment]` covering `[start, end)`.
    fn anchor(segment: usize, start: usize, end: usize) -> serde_json::Value {
        let text: String = SEGMENTS[segment]
            .chars()
            .skip(start)
            .take(end - start)
            .collect();
        json!({
            "seq_index": segment,
            "char_start": start,
            "char_end": end,
            "excerpt": text,
        })
    }

    fn ask(recording_id: RecordingId, question: &str) -> ValidatedAskRecording {
        ValidatedAskRecording {
            recording_id,
            question: question.to_owned(),
            revision_id: None,
            model_override: None,
            cost_acknowledged: false,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answered_question_is_stored_as_a_conversation_and_a_run() {
        let (fixture, recording_id, revision_id) = library().await;
        let model = ScriptedModel::new(json!({
            "outcome": "answered",
            "answer": "On Friday.",
            "anchors": [anchor(0, 0, 7)]
        }));
        let backend = fixture
            .backend
            .clone()
            .with_text_model(Arc::clone(&model) as Arc<dyn TextModel>);

        let answer = backend
            .ask_recording(ask(recording_id, "when does the migration ship?"))
            .await
            .expect("the question is answered");

        assert_eq!(answer.outcome, "answered");
        assert_eq!(answer.text, "On Friday.");
        assert_eq!(answer.transcript_revision_id, revision_id.to_string());
        assert_eq!(answer.anchors.len(), 1);
        assert!(answer.anchors[0].verified);
        assert_eq!(answer.anchors[0].matched_text.as_deref(), Some("We will"));
        assert_eq!(answer.anchors[0].alignment_origin, "native");
        assert_eq!(answer.anchors[0].seek_ms, Some(0));
        assert!(answer.dispatched);
        assert!(answer.tool_run_id.is_some());
        assert_eq!(model.prompt_count(), 1);

        let conversations = backend
            .list_conversations(recording_id)
            .await
            .expect("conversations list")
            .conversations;
        assert_eq!(conversations.len(), 1);
        assert!(conversations[0].revision_is_current);
        assert_eq!(conversations[0].turns.len(), 2);
        assert_eq!(conversations[0].turns[0].role, "user");
        assert_eq!(
            conversations[0].turns[1].outcome.as_deref(),
            Some("answered")
        );
        assert!(conversations[0].turns[1].anchors[0].verified);

        let runs = backend
            .list_tool_runs(recording_id)
            .await
            .expect("runs list")
            .runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "succeeded");
        assert_eq!(runs[0].tool_id, "ask_recording");
        assert_eq!(runs[0].transcript_revision_id, revision_id.to_string());
        // Nothing left the device, so nothing claims egress and no grant is
        // named.
        assert_eq!(runs[0].consent_policy.privacy_class, "on_device");
        assert!(!runs[0].consent_policy.egress_allowed);
        assert_eq!(runs[0].consent_policy.consent_id, None);
        assert_eq!(runs[0].cost.confidence, crate::dto::CostConfidence::Unknown);
        assert_eq!(runs[0].cost.amount, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_question_the_recording_cannot_speak_to_dispatches_nothing() {
        let (fixture, recording_id, _) = library().await;
        let model = ScriptedModel::new(json!({
            "outcome": "answered", "answer": "unused", "anchors": []
        }));
        let backend = fixture
            .backend
            .clone()
            .with_text_model(Arc::clone(&model) as Arc<dyn TextModel>);

        let answer = backend
            .ask_recording(ask(recording_id, "zarquon telemetry spike"))
            .await
            .expect("a refusal is a successful answer");

        assert_eq!(answer.outcome, "not_supported_by_recording");
        assert!(!answer.dispatched);
        assert_eq!(answer.tool_run_id, None);
        assert!(answer.anchors.is_empty());
        assert_eq!(answer.retrieved_segments, Vec::<u32>::new());
        assert_eq!(model.prompt_count(), 0, "nothing may be dispatched");
        assert!(backend
            .list_tool_runs(recording_id)
            .await
            .expect("runs list")
            .runs
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answer_whose_anchors_do_not_verify_becomes_insufficient_evidence() {
        let (fixture, recording_id, _) = library().await;
        let model = ScriptedModel::new(json!({
            "outcome": "answered",
            "answer": "The budget is four million dollars.",
            "anchors": [{
                "seq_index": 0, "char_start": 0, "char_end": 7, "excerpt": "Invented"
            }]
        }));
        let backend = fixture
            .backend
            .clone()
            .with_text_model(Arc::clone(&model) as Arc<dyn TextModel>);

        let answer = backend
            .ask_recording(ask(recording_id, "when does the migration ship?"))
            .await
            .expect("an ungrounded answer is still an answer");

        assert_eq!(answer.outcome, "insufficient_evidence");
        assert!(!answer.text.contains("four million"));
        assert!(answer.anchors.is_empty());
        assert_eq!(answer.rejected_anchors.len(), 1);
        assert_eq!(
            answer.rejected_anchors[0].rejection_code,
            "excerpt_mismatch"
        );
        assert_eq!(answer.rejected_anchors[0].excerpt, "Invented");

        let runs = backend
            .list_tool_runs(recording_id)
            .await
            .expect("runs list")
            .runs;
        assert_eq!(runs[0].status, "rejected");
        assert_eq!(runs[0].output_sha256, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_study_run_stores_its_artifacts_and_lists_them_back() {
        let (fixture, recording_id, revision_id) = library().await;
        let model = ScriptedModel::new(json!({
            "flashcards": [
                {
                    "front": "What does idempotency mean?",
                    "back": "Retrying does not duplicate work.",
                    "anchors": [anchor(1, 0, 11)]
                },
                {
                    "front": "Fabricated card",
                    "back": "Nothing in the transcript says this.",
                    "anchors": [{
                        "seq_index": 0,
                        "char_start": 0,
                        "char_end": 7,
                        "excerpt": "Invented"
                    }]
                }
            ]
        }));
        let backend = fixture
            .backend
            .clone()
            .with_text_model(Arc::clone(&model) as Arc<dyn TextModel>);

        let run = backend
            .generate_study_artifacts(ValidatedGenerateStudyArtifacts {
                recording_id,
                tool_id: "flashcards".to_owned(),
                revision_id: None,
                model_override: None,
                cost_acknowledged: false,
            })
            .await
            .expect("the study run completes");

        assert_eq!(run.tool_id, "flashcards");
        assert_eq!(run.transcript_revision_id, revision_id.to_string());
        assert_eq!(run.artifacts.flashcards.len(), 1);
        assert!(run.artifacts.flashcards[0].anchors[0].verified);
        assert_eq!(run.rejected.len(), 1);
        assert_eq!(run.rejected[0].path, "/flashcards/1");
        assert_eq!(run.rejected[0].reason, "all_anchors_rejected");
        assert!(run.rejected[0].detail.contains("excerpt_mismatch"));
        assert_eq!(run.output_ids.len(), 1);
        assert!(run.tool_run_id.is_some());
        assert!(!run.dispatched, "an on-device run sends nothing");

        let sets = backend
            .list_study_artifacts(recording_id)
            .await
            .expect("stored sets")
            .sets;
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].tool_id, "flashcards");
        assert_eq!(sets[0].tool_version, 1);
        assert!(sets[0].revision_is_current);
        assert_eq!(sets[0].artifacts.flashcards.len(), 1);
        assert_eq!(
            sets[0].artifacts.flashcards[0].front,
            "What does idempotency mean?"
        );
        let stored_anchor = &sets[0].artifacts.flashcards[0].anchors[0];
        assert!(stored_anchor.verified);
        assert_eq!(stored_anchor.seek_ms, Some(1_000));

        let runs = backend
            .list_tool_runs(recording_id)
            .await
            .expect("runs list")
            .runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "succeeded");
        assert_eq!(runs[0].tool_id, "flashcards");
        assert!(runs[0].output_sha256.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tool_that_produces_no_study_artifacts_is_refused() {
        let (fixture, recording_id, _) = library().await;
        let backend = fixture
            .backend
            .clone()
            .with_text_model(ScriptedModel::new(json!({})) as Arc<dyn TextModel>);

        let error = backend
            .generate_study_artifacts(ValidatedGenerateStudyArtifacts {
                recording_id,
                tool_id: "meeting_summary".to_owned(),
                revision_id: None,
                model_override: None,
                cost_acknowledged: false,
            })
            .await
            .expect_err("a meeting tool is not a study tool");
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);

        let unknown = backend
            .generate_study_artifacts(ValidatedGenerateStudyArtifacts {
                recording_id,
                tool_id: "no_such_tool".to_owned(),
                revision_id: None,
                model_override: None,
                cost_acknowledged: false,
            })
            .await
            .expect_err("an unknown tool is refused");
        assert_eq!(unknown.code(), AppErrorCode::RecordNotFound);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_revision_of_another_recording_is_not_found() {
        let (fixture, recording_id, _) = library().await;
        let other = library().await;
        let backend = fixture.backend.clone().with_text_model(ScriptedModel::new(
            json!({"outcome": "answered", "answer": "x", "anchors": []}),
        ) as Arc<dyn TextModel>);

        let error = backend
            .ask_recording(ValidatedAskRecording {
                recording_id,
                question: "when does the migration ship?".to_owned(),
                revision_id: Some(other.2),
                model_override: None,
                cost_acknowledged: false,
            })
            .await
            .expect_err("a foreign revision is refused");
        assert_eq!(error.code(), AppErrorCode::RecordNotFound);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listing_tools_reports_the_route_and_offers_every_tool_it_satisfies() {
        let (fixture, recording_id, _) = library().await;
        let backend = fixture
            .backend
            .clone()
            .with_text_model(ScriptedModel::new(json!({"flashcards": []})) as Arc<dyn TextModel>);

        let listed = backend
            .list_transcript_tools(Some(recording_id))
            .await
            .expect("the tools list");
        assert_eq!(listed.tools.len(), 8);
        let route = listed.text_model.expect("a route resolves");
        assert_eq!(route.engine, "test-text-engine");
        assert!(!route.is_cloud);
        assert!(route.available);
        assert_eq!(route.unavailable_reason, None);
        for tool in &listed.tools {
            assert!(tool.offerable, "{} is not offerable", tool.tool_id);
            assert_eq!(tool.unavailable_reason, None);
            assert_eq!(tool.origin, "built_in");
        }
        let study: Vec<&str> = listed
            .tools
            .iter()
            .filter(|tool| tool.optional)
            .map(|tool| tool.tool_id.as_str())
            .collect();
        assert_eq!(study, vec!["definitions", "flashcards", "quiz"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_malformed_tool_override_is_reported_rather_than_skipped() {
        let (fixture, _, _) = library().await;
        let dir = fixture.backend.store().root().join(TOOL_OVERRIDE_DIR);
        std::fs::create_dir_all(&dir).expect("the override directory is created");
        std::fs::write(dir.join("broken.json"), b"{ not json").expect("the file is written");

        let error = fixture
            .backend
            .list_transcript_tools(None)
            .await
            .expect_err("a malformed override is an error the user sees");
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_user_override_appends_a_version_rather_than_replacing_one() {
        let (fixture, recording_id, _) = library().await;
        let dir = fixture.backend.store().root().join(TOOL_OVERRIDE_DIR);
        std::fs::create_dir_all(&dir).expect("the override directory is created");
        std::fs::write(
            dir.join("flashcards.json"),
            serde_json::to_vec(&json!({
                "tool_id": "flashcards",
                "instruction": "Produce cards for a language class.",
                "prompt_version": "flashcards/2"
            }))
            .expect("the override serializes"),
        )
        .expect("the file is written");

        let backend = fixture
            .backend
            .clone()
            .with_text_model(ScriptedModel::new(json!({"flashcards": []})) as Arc<dyn TextModel>);
        let listed = backend
            .list_transcript_tools(Some(recording_id))
            .await
            .expect("the tools list");
        let flashcards = listed
            .tools
            .iter()
            .find(|tool| tool.tool_id == "flashcards")
            .expect("the edited tool is listed");
        assert_eq!(flashcards.version, 2);
        assert_eq!(flashcards.prompt_version, "flashcards/2");
        assert_eq!(flashcards.origin, "user_edit");
    }

    #[test]
    fn a_quiz_answer_key_is_stable_and_not_always_the_first_option() {
        let first = quiz_item_dto(
            "When does the migration ship?".to_owned(),
            "Friday".to_owned(),
            vec!["Monday".to_owned(), "Tuesday".to_owned()],
            Vec::new(),
        );
        let again = quiz_item_dto(
            "When does the migration ship?".to_owned(),
            "Friday".to_owned(),
            vec!["Monday".to_owned(), "Tuesday".to_owned()],
            Vec::new(),
        );
        assert_eq!(first, again, "the same item must render the same key");
        assert_eq!(
            first.options[first.answer_index as usize], "Friday",
            "the key must address the answer"
        );
        assert_eq!(first.options.len(), 3);
    }

    #[test]
    fn a_run_whose_posture_cannot_be_read_never_claims_it_stayed_on_the_device() {
        let policy = consent_policy_dto("not json at all", Some("grant-1".to_owned()));
        assert_eq!(policy.privacy_class, "unresolved");
        assert!(!policy.egress_allowed);
        assert_eq!(policy.consent_id.as_deref(), Some("grant-1"));
    }

    /// Jobs other than the import that created the fixture's recording.
    async fn cloud_job_count(fixture: &Fixture) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE job_type <> 'import'")
            .fetch_one(fixture.backend.pool())
            .await
            .expect("the jobs table is readable")
    }

    async fn run_count(fixture: &Fixture) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tool_runs")
            .fetch_one(fixture.backend.pool())
            .await
            .expect("the runs table is readable")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cloud_route_asks_for_a_key_then_consent_and_only_then_dispatches() {
        use crate::ports::CloudPort;
        use crate::secrets::{SecretStore as _, SecretValue};
        use crate::validate::{ValidatedConsent, ValidatedConsentScope};
        use plaudon_security::consent::ConsentPurpose;

        let (fixture, recording_id, _) = library().await;
        let model = ScriptedModel::cloud(json!({
            "outcome": "answered",
            "answer": "On Friday.",
            "anchors": [anchor(0, 0, 7)]
        }));
        let backend = fixture
            .backend
            .clone()
            .with_text_model(Arc::clone(&model) as Arc<dyn TextModel>);

        // No key: refused before the consent check, before any job row.
        let error = backend
            .ask_recording(ask(recording_id, "when does the migration ship?"))
            .await
            .expect_err("a cloud route without a key is refused");
        assert_eq!(error.code(), AppErrorCode::AuthenticationRequired);
        assert_eq!(model.prompt_count(), 0);
        assert_eq!(cloud_job_count(&fixture).await, 0);
        assert_eq!(run_count(&fixture).await, 0);

        fixture
            .secrets
            .set_openrouter_key(&SecretValue::new("sk-or-test-key".to_owned()).expect("a key"))
            .expect("the key is stored");

        // Key but no grant: refused by the consent preflight, still nothing
        // sent and still no job row.
        let error = backend
            .ask_recording(ask(recording_id, "when does the migration ship?"))
            .await
            .expect_err("a cloud route without consent is refused");
        assert_eq!(error.code(), AppErrorCode::ConsentRequired);
        assert_eq!(model.prompt_count(), 0);
        assert_eq!(cloud_job_count(&fixture).await, 0);
        assert_eq!(run_count(&fixture).await, 0);

        let consent_id = fixture
            .backend
            .grant_cloud_consent(ValidatedConsent {
                purpose: ConsentPurpose::Summarization,
                content_type: ContentType::TranscriptText,
                scope: ValidatedConsentScope::ModelPolicy,
                model_id: CLOUD_MODEL.to_owned(),
                downstream_provider: None,
                require_zero_retention: false,
                disclosure_version: crate::backend::cloud::CURRENT_DISCLOSURE_VERSION.to_owned(),
            })
            .await
            .expect("the grant is stored")
            .consent_id;

        let answer = backend
            .ask_recording(ask(recording_id, "when does the migration ship?"))
            .await
            .expect("the authorized question is answered");
        assert_eq!(answer.outcome, "answered");
        assert_eq!(model.prompt_count(), 1);
        assert_eq!(cloud_job_count(&fixture).await, 1, "one job row per run");

        let runs = backend
            .list_tool_runs(recording_id)
            .await
            .expect("runs list")
            .runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].engine, OPENROUTER_ENGINE_ID);
        assert_eq!(runs[0].model_id, CLOUD_MODEL);
        assert_eq!(runs[0].consent_policy.privacy_class, "cloud_zero_retention");
        assert!(runs[0].consent_policy.egress_allowed);
        assert_eq!(
            runs[0].consent_policy.consent_id.as_deref(),
            Some(consent_id.as_str())
        );
        // Text generation has no audio request family (App. A.3).
        assert_eq!(runs[0].request_family, "unsupported");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_only_mode_refuses_a_cloud_tool_run() {
        use crate::secrets::{SecretStore as _, SecretValue};

        let (fixture, recording_id, _) = library().await;
        fixture
            .backend
            .set_local_only_mode(true)
            .await
            .expect("local-only mode is enabled");
        fixture
            .secrets
            .set_openrouter_key(&SecretValue::new("sk-or-test-key".to_owned()).expect("a key"))
            .expect("the key is stored");

        let model = ScriptedModel::cloud(json!({
            "outcome": "answered", "answer": "On Friday.", "anchors": [anchor(0, 0, 7)]
        }));
        let backend = fixture
            .backend
            .clone()
            .with_text_model(Arc::clone(&model) as Arc<dyn TextModel>);

        let error = backend
            .ask_recording(ask(recording_id, "when does the migration ship?"))
            .await
            .expect_err("local-only mode refuses the dispatch");
        assert_eq!(error.code(), AppErrorCode::LocalOnlyMode);
        assert_eq!(model.prompt_count(), 0);
        assert_eq!(cloud_job_count(&fixture).await, 0);
    }
}
