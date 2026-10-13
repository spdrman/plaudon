//! Cloud AI over OpenRouter BYOK: the [`CloudPort`] implementation
//! (spec §10, ADR-0022).
//!
//! The order of operations in [`LocalBackend::start_processing`] and
//! [`LocalBackend::verify_model`] is the privacy contract of this module:
//!
//! 1. validate and resolve the model (an unverified model is refused here);
//! 2. evaluate the local budget against the pre-dispatch estimate;
//! 3. run the consent preflight (egress gate, scoped grant, retention posture);
//! 4. only then open a job row and hand the work to the OpenRouter client,
//!    which commits its ledger row *before* the first byte is transmitted.
//!
//! A refusal in steps 1-3 therefore leaves no job row and no ledger row, and
//! no content has left the machine.
//!
//! No value that reaches a response or an error envelope is built from a
//! secret, a URL, a path or transcript text: responses carry model ids, enum
//! labels and decimal amounts, and every error goes through
//! [`crate::error::envelope`], which renders only the canonical message.

use std::fmt::Display;
use std::sync::Arc;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{MediaFormat, PricingBasis, RequestFamily};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::stt::{SttInput, TranscriptDraft};
use plaudon_ai_openrouter::allowlist::ModelTask;
use plaudon_ai_openrouter::client::{CloudDispatcher, JobBinding, OpenRouterSpeechToText};
use plaudon_ai_openrouter::credential::{ApiKey, ApiKeySource, CredentialError};
use plaudon_ai_openrouter::discovery::{CatalogClient, DegradedReason};
use plaudon_ai_openrouter::pricing::{estimate, CostEstimate, EstimateInput, PricingUnits};
use plaudon_ai_openrouter::privacy::{self, PrivacyResolution};
use plaudon_ai_openrouter::registry::{
    CatalogSource, ModelCard as DomainCard, Verification, OPENROUTER_ENGINE_ID,
};
use plaudon_ai_openrouter::service::ledger::AttemptRow;
use plaudon_ai_openrouter::service::usage::{money_to_string, AMOUNT_SCALE};
use plaudon_ai_openrouter::service::{
    evaluate_budget, summarize, today, BudgetDecision, CatalogView, CloudAi,
    CloudBudget as DomainBudget, ConsentAuthority, CostTotals, DispatchContext, ModelChoice, Stage,
    BUDGET_ENFORCEMENT_NOTE,
};
use plaudon_ai_openrouter::smoke::CapabilitySmokeTest;
use plaudon_ai_openrouter::transport::{AsyncHttpTransport, ReqwestTransport};
use plaudon_domain::capability::{DispatchState, Tri};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::money::{Currency, Money, MoneyEstimate};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_security::consent::{
    ConsentGrant, ContentType, GrantScope, ProviderRoute, RetentionPosture, RELAY_OPENROUTER,
};
use plaudon_security::outbound::http::GatedHttpClientFactory;
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
use serde_json::json;
use sqlx::{Row, SqlitePool};

use crate::backend::consent::ConsentStore;
use crate::backend::jobs::{self, JobDraft};
use crate::backend::store::{rfc3339, rfc3339_opt, storage_error};
use crate::backend::LocalBackend;
use crate::dto::{
    CloudBudget, ConsentSummary, CostConfidence, CostValue, EventKind, GetUsageResponse,
    GrantCloudConsentResponse, JobKind, JobState, ListModelsResponse, ModelCard, ModelFeatures,
    ModelPricing, ModelPrivacy, MoneyAmount, SetModelPreferenceResponse, StartProcessingResponse,
    TriState, UsageAttempt, UsageByModel, UsageTotals, VerifyModelResponse,
};
use crate::error::internal;
use crate::events::EventDraft;
use crate::ports::{CloudPort, SettingsPort};
use crate::secrets::SecretStore;
use crate::validate::{
    ValidatedBudget, ValidatedConsent, ValidatedConsentScope, ValidatedListModels,
    ValidatedPreference, ValidatedPreferenceScope, ValidatedStartProcessing, ValidatedUsageRange,
    ValidatedVerifyModel,
};

/// Version of the privacy statement the app currently shows. A grant recorded
/// against another version no longer covers a dispatch (spec §4.4 #5).
pub const CURRENT_DISCLOSURE_VERSION: &str = "2026-02-01";

/// Synthetic recording id of a capability check, which sends a bundled clip
/// rather than any recording (so only model-policy grants cover it).
pub const CAPABILITY_CHECK_RECORDING: &str = "capability-check";

/// Fixture id stored with an observed capability-check exchange.
const CAPABILITY_FIXTURE_ID: &str = "capability-check";

/// Sample rate assumed when the archive recorded none. Offsets are converted
/// back to milliseconds with the same rate, so the value cancels out.
const FALLBACK_SAMPLE_RATE_HZ: u32 = 16_000;

/// Everything the cloud commands need beyond the library itself.
#[derive(Clone)]
pub struct CloudRuntime {
    ai: CloudAi,
    transport: Arc<dyn AsyncHttpTransport>,
    consents: ConsentStore,
    text_model: Option<Arc<dyn plaudon_ai_contracts::text::TextModel>>,
}

/// Written by hand because a [`TextModel`](plaudon_ai_contracts::text::TextModel)
/// is an object-safe trait without a `Debug` bound. Nothing about the model is
/// printed beyond whether one was installed.
impl std::fmt::Debug for CloudRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudRuntime")
            .field("ai", &self.ai)
            .field("transport", &self.transport)
            .field("consents", &self.consents)
            .field("text_model_installed", &self.text_model.is_some())
            .finish()
    }
}

impl CloudRuntime {
    /// Open the cloud state over the library database, with the production
    /// HTTPS transport.
    pub fn open(db: &Database, pool: SqlitePool) -> Result<Self, AppError> {
        let ai = CloudAi::open(db.clone())
            .map_err(|err| internal(format!("the model allowlist is invalid: {err}")))?;
        let transport = ReqwestTransport::new()
            .map_err(|err| internal(format!("cannot build the HTTPS transport: {err}")))?;
        Ok(Self {
            ai,
            transport: Arc::new(transport),
            consents: ConsentStore::new(pool),
            text_model: None,
        })
    }

    /// Replace the transport. Tests use this to run without a network.
    #[must_use]
    pub fn with_transport(mut self, transport: Arc<dyn AsyncHttpTransport>) -> Self {
        self.transport = transport;
        self
    }

    /// Install the text model every tool run uses, bypassing route
    /// resolution. Tests use this to run the whole tool path without a
    /// network; production never sets it, so `None` is the only value a
    /// shipped build ever holds.
    #[must_use]
    pub fn with_text_model(
        mut self,
        model: Arc<dyn plaudon_ai_contracts::text::TextModel>,
    ) -> Self {
        self.text_model = Some(model);
        self
    }

    /// The installed text model, when a test installed one.
    #[must_use]
    pub(super) fn text_model(&self) -> Option<&Arc<dyn plaudon_ai_contracts::text::TextModel>> {
        self.text_model.as_ref()
    }
}

/// The OS-keychain key as an [`ApiKeySource`]. Reads happen per request; the
/// key is never cached or copied anywhere else.
#[derive(Debug, Clone)]
struct DesktopApiKey(Arc<dyn SecretStore>);

impl ApiKeySource for DesktopApiKey {
    fn api_key(&self) -> Result<ApiKey, CredentialError> {
        match self.0.load_openrouter_key() {
            Ok(Some(value)) => Ok(ApiKey::new(value.expose().to_owned())),
            Ok(None) => Err(CredentialError::Missing),
            Err(_) => Err(CredentialError::StoreUnavailable {
                detail: "the credential store is unavailable".to_owned(),
            }),
        }
    }
}

/* ------------------------------------------------------------ pure mapping */

fn store_error(context: &str, error: impl Display) -> AppError {
    internal(format!("{context}: {error}"))
}

/// Wire form of an amount.
#[must_use]
pub fn money_amount(money: Money) -> MoneyAmount {
    MoneyAmount {
        amount: money_to_string(money),
        currency: money.currency.as_str().to_owned(),
    }
}

/// Wire form of a cost estimate or settlement.
#[must_use]
pub fn cost_value(estimate: MoneyEstimate) -> CostValue {
    match estimate {
        MoneyEstimate::Unknown => CostValue::unknown(),
        MoneyEstimate::Estimated(money) => CostValue {
            confidence: CostConfidence::Estimated,
            amount: Some(money_amount(money)),
        },
        MoneyEstimate::Finalized(money) => CostValue {
            confidence: CostConfidence::Finalized,
            amount: Some(money_amount(money)),
        },
    }
}

/// What one attempt cost: the settled amount, else its estimate when the
/// request may have been billed, else unknown. Never zero.
fn attempt_estimate(row: &AttemptRow) -> MoneyEstimate {
    match row.settled_cost() {
        Some(cost) if cost.amount.amount().is_some() => cost.amount,
        _ if row.possibly_billed() => row.estimate(),
        _ => MoneyEstimate::Unknown,
    }
}

/// Cost of a job from its ledger attempts: finalized only when every attempt
/// is finalized, estimated when any amount is an estimate or any attempt is
/// unknown beside a known one, unknown when nothing is known.
fn job_cost(rows: &[AttemptRow]) -> CostValue {
    let mut totals = CostTotals::default();
    let mut any_unknown = false;
    for row in rows {
        match attempt_estimate(row) {
            MoneyEstimate::Unknown => {
                any_unknown = true;
                totals.add(None);
            }
            MoneyEstimate::Estimated(money) => {
                totals.estimated = Some(sum(totals.estimated, money));
                totals.attempts += 1;
            }
            MoneyEstimate::Finalized(money) => {
                totals.finalized = Some(sum(totals.finalized, money));
                totals.attempts += 1;
            }
        }
    }
    let Some(committed) = totals.committed() else {
        return CostValue::unknown();
    };
    let confidence = if totals.estimated.is_some() || any_unknown {
        CostConfidence::Estimated
    } else {
        CostConfidence::Finalized
    };
    CostValue {
        confidence,
        amount: Some(money_amount(committed)),
    }
}

fn sum(current: Option<Money>, addend: Money) -> Money {
    current
        .and_then(|existing| existing.checked_add(addend))
        .unwrap_or(addend)
}

/// Cost of one job, read from the ledger. Local jobs have no attempts and are
/// therefore `unknown`.
pub async fn load_job_cost(pool: &SqlitePool, job_id: &str) -> Result<CostValue, AppError> {
    let rows = attempts_for_job(pool, job_id).await?;
    Ok(job_cost(&rows))
}

async fn attempts_for_job(pool: &SqlitePool, job_id: &str) -> Result<Vec<AttemptRow>, AppError> {
    sqlx::query_as::<_, AttemptRow>(
        "SELECT a.id, a.job_id, a.provider, a.model_id, a.request_family, a.dispatch_state, \
                a.content_sha256, a.route, a.privacy_policy_snapshot, a.consent_id, \
                a.estimated_units_json, a.final_cost_json, a.provider_request_id, \
                a.created_at, a.dispatched_at, a.responded_at \
         FROM inference_attempts a WHERE a.job_id = ?1 ORDER BY a.created_at ASC, a.id ASC",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await
    .map_err(|err| storage_error("cannot read the job's attempts", &err))
}

const fn tri(value: Tri) -> TriState {
    match value {
        Tri::Yes => TriState::Yes,
        Tri::No => TriState::No,
        Tri::Unknown => TriState::Unknown,
    }
}

const fn family_label(family: RequestFamily) -> &'static str {
    match family {
        RequestFamily::MultipartStt => "multipart_stt",
        RequestFamily::AudioChat => "audio_chat",
        RequestFamily::Unsupported => "unsupported",
        RequestFamily::Unknown => "unknown",
    }
}

fn family_from_stored(raw: &str) -> &'static str {
    match raw {
        "multipart_stt" => "multipart_stt",
        "audio_chat" => "audio_chat",
        "unsupported" => "unsupported",
        _ => "unknown",
    }
}

const fn dispatch_label(state: DispatchState) -> &'static str {
    match state {
        DispatchState::NotSent => "not_sent",
        DispatchState::Sending => "sending",
        DispatchState::AcceptedByProvider => "accepted_by_provider",
        DispatchState::OutcomeUnknown => "outcome_unknown",
        DispatchState::Completed => "completed",
        DispatchState::FailedBeforeSend => "failed_before_send",
    }
}

fn first_known_price(units: &PricingUnits) -> Option<Money> {
    // Same precedence as `PricingUnits::unit_label`, so the label and the
    // amount always describe the same unit.
    [
        &units.audio_input_per_token,
        &units.prompt_per_token,
        &units.per_request,
    ]
    .into_iter()
    .find_map(|price| price.get().copied())
}

fn pricing_dto(units: &PricingUnits) -> ModelPricing {
    let amount_per_unit = first_known_price(units).map(money_amount);
    let basis = match units.basis().get() {
        Some(PricingBasis::PerToken) => "per_token",
        Some(PricingBasis::PerRequest) => "per_request",
        Some(PricingBasis::PerAudioSecond) => "per_audio_second",
        Some(PricingBasis::None) => "none",
        Some(PricingBasis::Other(_)) | None => "other",
    };
    ModelPricing {
        basis: basis.to_owned(),
        unit_label: units
            .unit_label()
            .unwrap_or("pricing not published")
            .to_owned(),
        amount_per_unit,
    }
}

fn privacy_dto(resolution: Option<&PrivacyResolution>, model_id: &str) -> ModelPrivacy {
    let Some(resolution) = resolution else {
        return ModelPrivacy {
            relay: RELAY_OPENROUTER.to_owned(),
            downstream_provider: None,
            retention: RetentionPosture::Unknown.kind_label().to_owned(),
            retention_days: None,
            zero_retention_available: false,
        };
    };
    debug_assert_eq!(resolution.route.model_id, model_id);
    ModelPrivacy {
        relay: resolution.route.relay.clone(),
        downstream_provider: resolution.route.downstream_provider.clone(),
        retention: resolution.posture.kind_label().to_owned(),
        retention_days: match resolution.posture {
            RetentionPosture::LimitedRetention { days } => Some(days),
            _ => None,
        },
        zero_retention_available: resolution.posture == RetentionPosture::ZeroRetention,
    }
}

/// Wire form of a model card.
#[must_use]
pub fn card_dto(card: &DomainCard) -> ModelCard {
    let (verification, reason, fixture_id, validated_at) = match &card.verification {
        Verification::Verified {
            fixture_id,
            validated_at,
        } => (
            "verified",
            None,
            Some(fixture_id.clone()),
            Some(instant_of_date(validated_at)),
        ),
        Verification::NotVerified { reason } => {
            ("not_verified", Some(reason.label().to_owned()), None, None)
        }
    };
    ModelCard {
        model_id: card.model_id.clone(),
        display_name: card.display_name.clone(),
        engine: OPENROUTER_ENGINE_ID.to_owned(),
        request_family: family_label(card.request_family).to_owned(),
        verification: verification.to_owned(),
        verification_reason: reason,
        fixture_id,
        validated_at,
        features: ModelFeatures {
            segments: tri(card.features.segments),
            word_timestamps: tri(card.features.word_timestamps),
            diarization: tri(card.features.diarization),
            language_detection: tri(card.features.language_detection),
        },
        locales: card
            .locales
            .get()
            .map(|tags| tags.iter().map(ToString::to_string).collect()),
        max_input_bytes: card.max_input_bytes.get().copied(),
        max_duration_ms: card.max_duration_ms.get().copied(),
        pricing: pricing_dto(&card.pricing),
        privacy: privacy_dto(card.privacy.as_ref(), &card.model_id),
        selectable: card.is_selectable(),
    }
}

/// A `YYYY-MM-DD` observation date as an RFC 3339 instant at midnight UTC.
fn instant_of_date(date: &str) -> String {
    if date.len() == 10 && date.as_bytes()[4] == b'-' {
        format!("{date}T00:00:00Z")
    } else {
        date.to_owned()
    }
}

const fn degraded_label(reason: DegradedReason) -> &'static str {
    match reason {
        DegradedReason::LocalOnlyMode => "local_only_mode",
        DegradedReason::Offline => "offline",
        DegradedReason::CatalogError | DegradedReason::MalformedCatalog => "catalog_error",
    }
}

/// The task a stage's cloud models are listed under. Diarization has no
/// OpenRouter model at all.
const fn task_for_stage(stage: Stage) -> Option<ModelTask> {
    match stage {
        Stage::Transcribe => Some(ModelTask::Transcription),
        Stage::Diarize => None,
        _ => Some(ModelTask::TextGeneration),
    }
}

const fn job_kind_for_stage(stage: Stage) -> JobKind {
    match stage {
        Stage::Transcribe | Stage::Diarize | Stage::Normalize => JobKind::Transcribe,
        _ => JobKind::Summarize,
    }
}

fn consent_summary(grant: &ConsentGrant) -> Result<ConsentSummary, AppError> {
    let route = grant.routes.iter().next();
    let recording_id = match &grant.scope {
        GrantScope::Recording { recording_id } => Some(recording_id.clone()),
        GrantScope::AskEveryTime | GrantScope::ModelPolicy { .. } => None,
    };
    let at = |unix: i64| rfc3339("consent timestamp", unix.saturating_mul(1000));
    Ok(ConsentSummary {
        consent_id: grant.id.clone(),
        purpose: grant.purpose.label().to_owned(),
        content_type: grant
            .content_types
            .iter()
            .next()
            .map_or("metadata_only", |content| content.label())
            .to_owned(),
        scope_kind: grant.scope.kind_label().to_owned(),
        recording_id,
        model_id: route
            .map(|route| route.model_id.clone())
            .unwrap_or_default(),
        downstream_provider: route.and_then(|route| route.downstream_provider.clone()),
        required_retention: grant
            .required_retention
            .map(|posture| posture.kind_label().to_owned()),
        disclosure_version: grant.disclosure_version.clone(),
        granted_at: at(grant.granted_at_unix)?,
        expires_at: grant.expires_at_unix.map(at).transpose()?,
        revoked_at: grant.revoked_at_unix.map(at).transpose()?,
    })
}

fn totals_dto(totals: &CostTotals) -> UsageTotals {
    UsageTotals {
        finalized: totals.finalized.map(money_amount),
        estimated: totals.estimated.map(money_amount),
        unknown_count: totals.unknown_count,
    }
}

fn attempt_dto(row: &AttemptRow) -> Result<UsageAttempt, AppError> {
    Ok(UsageAttempt {
        attempt_id: row.id.clone(),
        job_id: row.job_id.clone(),
        model_id: row.model_id.clone(),
        request_family: family_from_stored(&row.request_family).to_owned(),
        dispatch_state: dispatch_label(row.state()).to_owned(),
        cost: cost_value(attempt_estimate(row)),
        provider_request_id: row.provider_request_id.clone(),
        created_at: rfc3339("attempt created_at", row.created_at)?,
        responded_at: rfc3339_opt("attempt responded_at", row.responded_at)?,
        possibly_billed: row.possibly_billed(),
    })
}

/* ------------------------------------------------------------ backend glue */

/// Everything `authorize` established about one dispatch.
///
/// Visible to the sibling backend modules that dispatch through the same
/// gate: a transcript tool run authorizes exactly the way a transcription
/// does (`backend::tools`), so it carries the same plan rather than growing a
/// second one.
#[derive(Debug, Clone)]
pub(super) struct DispatchPlan {
    policy: Arc<OutboundPolicy>,
    authority: Arc<ConsentAuthority>,
    pub(super) grant_id: String,
    pub(super) resolution: PrivacyResolution,
}

/// Media facts of the recording to transcribe.
///
/// Visible to the sibling backend modules because a local engine reads the
/// same archived blob a cloud one uploads: the transcribe executor builds
/// its `SttInput` from exactly these facts.
#[derive(Debug, Clone)]
pub(super) struct MediaFacts {
    pub(super) path: std::path::PathBuf,
    pub(super) sha256: Sha256Digest,
    pub(super) byte_length: u64,
    pub(super) duration_ms: Option<u64>,
    pub(super) sample_rate_hz: u32,
    pub(super) channels: u16,
    pub(super) format: MediaFormat,
}

/// A cloud transcription that passed every pre-dispatch check.
#[derive(Debug, Clone)]
pub struct PreparedTranscription {
    card: DomainCard,
    media: MediaFacts,
    estimate: CostEstimate,
    plan: DispatchPlan,
}

fn now_unix() -> i64 {
    Timestamp::now().as_millis().div_euclid(1000)
}

impl LocalBackend {
    /// The cloud state (catalogue, ledger, selection store). Exposed so
    /// integration tests can seed capability evidence and inspect the ledger.
    #[must_use]
    pub fn cloud_ai(&self) -> &CloudAi {
        &self.cloud.ai
    }

    fn api_key_source(&self) -> DesktopApiKey {
        DesktopApiKey(Arc::clone(self.secrets()))
    }

    /// The effective egress policy: Local-only Mode denies everything, and
    /// otherwise a network grant covers the pinned vendor hosts. A grant here
    /// only opens the gate; the per-dispatch consent check still has to pass.
    fn outbound_policy(local_only: bool) -> OutboundPolicy {
        if local_only {
            OutboundPolicy::local_only()
        } else {
            OutboundPolicy::network_permitted(NetworkGrant::new(
                ApprovalSource::UserSettingsConfirmation,
                now_unix(),
                CURRENT_DISCLOSURE_VERSION,
            ))
        }
    }

    async fn catalog_view(
        &self,
        task: ModelTask,
        client: Option<&CatalogClient>,
    ) -> Result<CatalogView, AppError> {
        self.cloud
            .ai
            .catalog(task, client, DegradedReason::Offline)
            .await
            .map_err(|err| store_error("cannot build the model catalogue", err))
    }

    pub(super) async fn card_for(
        &self,
        task: ModelTask,
        model_id: &str,
    ) -> Result<DomainCard, AppError> {
        let view = self.catalog_view(task, None).await?;
        Ok(view
            .cards
            .into_iter()
            .find(|card| card.model_id == model_id)
            .unwrap_or_else(|| DomainCard::custom(model_id, task)))
    }

    async fn budget_dto(&self) -> Result<CloudBudget, AppError> {
        let budget = self
            .cloud
            .ai
            .store()
            .budget()
            .await
            .map_err(|err| store_error("cannot read the budget", err))?;
        let totals = self
            .cloud
            .ai
            .month_to_date(Timestamp::now())
            .await
            .map_err(|err| store_error("cannot read month-to-date spend", err.detail))?;
        let month_to_date = match totals.committed() {
            Some(committed) => CostValue {
                confidence: if totals.estimated.is_some() || totals.has_unknown() {
                    CostConfidence::Estimated
                } else {
                    CostConfidence::Finalized
                },
                amount: Some(money_amount(committed)),
            },
            // Nothing recorded this month is a known zero; attempts with no
            // derivable cost are not.
            None if totals.attempts == 0 => CostValue {
                confidence: CostConfidence::Finalized,
                amount: Some(money_amount(Money::new(0, AMOUNT_SCALE, Currency::USD))),
            },
            None => CostValue::unknown(),
        };
        Ok(CloudBudget {
            per_job_limit: budget.per_job_limit.map(money_amount),
            monthly_limit: budget.monthly_limit.map(money_amount),
            month_to_date,
            blocks_dispatch: budget.is_enforcing(),
            enforcement_note: BUDGET_ENFORCEMENT_NOTE.to_owned(),
        })
    }

    /// Evaluate the local budget against an estimate. Runs before consent and
    /// before any job or ledger row exists.
    pub(super) async fn check_budget(
        &self,
        estimate: MoneyEstimate,
        cost_acknowledged: bool,
    ) -> Result<(), AppError> {
        let budget = self
            .cloud
            .ai
            .store()
            .budget()
            .await
            .map_err(|err| store_error("cannot read the budget", err))?;
        let month_to_date = self
            .cloud
            .ai
            .month_to_date(Timestamp::now())
            .await
            .map_err(|err| store_error("cannot read month-to-date spend", err.detail))?;
        match evaluate_budget(&budget, estimate, &month_to_date) {
            BudgetDecision::Allow => Ok(()),
            BudgetDecision::AcknowledgementRequired { .. } if cost_acknowledged => Ok(()),
            BudgetDecision::AcknowledgementRequired { .. } => Err(AppError::new(
                AppErrorCode::CostAcknowledgementRequired,
                "the cost of this request cannot be calculated and a limit is set",
            )),
            BudgetDecision::Block { limit, .. } => Err(AppError::new(
                AppErrorCode::BudgetExceeded,
                format!("the {limit:?} budget would be exceeded"),
            )),
        }
    }

    /// The egress gate, a covering grant and the retention posture, for one
    /// dispatch. Nothing is sent and nothing is recorded.
    pub(super) async fn authorize_dispatch(
        &self,
        recording_id: &str,
        model_id: &str,
        content_type: ContentType,
    ) -> Result<DispatchPlan, AppError> {
        let preferences = self.preferences().await?;
        let policy = Arc::new(Self::outbound_policy(preferences.local_only_mode));
        let grants = self.cloud.consents.load_grants().await?;
        let documented_policies = self
            .cloud
            .ai
            .store()
            .route_policies()
            .await
            .map_err(|err| store_error("cannot read route policies", err))?;
        let resolution = privacy::resolve(model_id, content_type, None, &documented_policies);
        let now = now_unix();
        let required_retention = grants
            .iter()
            .filter(|grant| grant.is_active(now) && grant.covers_route(&resolution.route))
            .find_map(|grant| grant.required_retention);
        let authority = Arc::new(ConsentAuthority::new(
            Arc::clone(&policy),
            grants,
            DispatchContext {
                recording_id: recording_id.to_owned(),
                disclosure_version: CURRENT_DISCLOSURE_VERSION.to_owned(),
                required_retention,
                documented_policies,
                endpoints: None,
            },
            now,
        ));
        let approval = plaudon_ai_openrouter::client::CloudDispatchAuthority::authorize(
            authority.as_ref(),
            content_type,
            model_id,
        )
        .await
        .map_err(|refusal| {
            use plaudon_ai_openrouter::client::DispatchRefused;
            match refusal {
                DispatchRefused::Egress if preferences.local_only_mode => AppError::new(
                    AppErrorCode::LocalOnlyMode,
                    "local-only mode refused the dispatch",
                ),
                DispatchRefused::Egress => AppError::new(
                    AppErrorCode::EgressDenied,
                    "the egress policy refused the dispatch",
                ),
                DispatchRefused::ConsentRequired => AppError::new(
                    AppErrorCode::ConsentRequired,
                    "no active consent grant covers this dispatch",
                ),
                DispatchRefused::PrivacyUnresolved => AppError::new(
                    AppErrorCode::PolicyUnresolved,
                    "the privacy route could not be resolved",
                ),
            }
        })?;
        Ok(DispatchPlan {
            policy,
            authority,
            grant_id: approval.grant_id,
            resolution,
        })
    }

    pub(super) fn dispatcher(&self, plan: &DispatchPlan) -> CloudDispatcher {
        CloudDispatcher::new(
            GatedHttpClientFactory::new(Arc::clone(&plan.policy)),
            Arc::clone(&self.cloud.transport),
            Arc::new(self.api_key_source()),
            self.cloud.ai.ledger_handle(),
            Arc::clone(&plan.authority) as Arc<_>,
        )
    }

    pub(super) fn require_key(&self) -> Result<(), AppError> {
        match self.secrets().load_openrouter_key()? {
            Some(_) => Ok(()),
            None => Err(AppError::new(
                AppErrorCode::AuthenticationRequired,
                "no OpenRouter key is stored",
            )),
        }
    }

    pub(super) async fn media_facts(
        &self,
        recording_id: RecordingId,
    ) -> Result<MediaFacts, AppError> {
        let row = sqlx::query(
            "SELECT a.relative_path AS relative_path, a.byte_length AS byte_length, \
                    a.sha256 AS sha256, a.codec AS codec, a.sample_rate AS sample_rate, \
                    a.channels AS channels, a.duration_ms AS duration_ms \
             FROM media_assets a JOIN recordings r ON r.preferred_media_asset_id = a.id \
             WHERE r.id = ?1 AND r.deleted_at IS NULL",
        )
        .bind(recording_id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(|err| storage_error("cannot read the recording's media", &err))?
        .ok_or_else(|| AppError::new(AppErrorCode::RecordNotFound, "no such recording"))?;

        let relative: String = row.get("relative_path");
        let path = self.archive().absolute(&relative).map_err(AppError::from)?;
        if !path.is_file() {
            return Err(AppError::new(
                AppErrorCode::AssetNotFound,
                "the archived blob is missing from disk",
            ));
        }
        let container = std::path::Path::new(&relative)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let codec: Option<String> = row.get("codec");
        // An archive that observed no codec says so with `unknown`; the
        // container's customary codec stands in for matching against the
        // endpoint's documented formats. The provider still decodes the bytes.
        let codec = codec.filter(|codec| !codec.is_empty() && codec != "unknown");
        let codec = codec.unwrap_or_else(|| {
            match container.as_str() {
                "wav" => "pcm_s16le",
                "m4a" | "mp4" => "aac",
                "ogg" | "opus" => "opus",
                other => other,
            }
            .to_owned()
        });
        let sha256: String = row.get("sha256");
        let non_negative = |value: Option<i64>| value.and_then(|v| u64::try_from(v).ok());
        Ok(MediaFacts {
            path,
            sha256: sha256
                .parse()
                .map_err(|_| internal("stored media digest is not a sha-256"))?,
            byte_length: non_negative(Some(row.get("byte_length"))).unwrap_or(0),
            duration_ms: non_negative(row.get("duration_ms")),
            sample_rate_hz: non_negative(row.get("sample_rate"))
                .and_then(|rate| u32::try_from(rate).ok())
                .unwrap_or(FALLBACK_SAMPLE_RATE_HZ),
            channels: non_negative(row.get("channels"))
                .and_then(|count| u16::try_from(count).ok())
                .unwrap_or(1),
            format: MediaFormat::new(container, codec),
        })
    }

    /// Whether an earlier attempt for this recording and model may have been
    /// billed and was never followed by a completed one (spec §10.7).
    async fn has_unacknowledged_unknown_outcome(
        &self,
        recording_id: RecordingId,
        model_id: &str,
    ) -> Result<bool, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inference_attempts a JOIN jobs j ON j.id = a.job_id \
             WHERE a.dispatch_state = 'outcome_unknown' AND a.model_id = ?2 \
               AND json_extract(j.config_snapshot_json, '$.recording_id') = ?1 \
               AND a.created_at > COALESCE((SELECT MAX(a2.created_at) \
                    FROM inference_attempts a2 JOIN jobs j2 ON j2.id = a2.job_id \
                    WHERE a2.dispatch_state = 'completed' AND a2.model_id = ?2 \
                      AND json_extract(j2.config_snapshot_json, '$.recording_id') = ?1), 0)",
        )
        .bind(recording_id.to_string())
        .bind(model_id)
        .fetch_one(self.pool())
        .await
        .map_err(|err| storage_error("cannot read prior attempts", &err))?;
        Ok(count > 0)
    }

    /// Every check that must pass before a cloud transcription is sent.
    ///
    /// Run twice: once by `start_processing`, so a refusal leaves no job row
    /// at all, and once by the executor at dispatch time, so a key removed, a
    /// budget lowered or a consent revoked while the job waited still stops
    /// it before a byte leaves.
    pub(super) async fn prepare_cloud_transcription(
        &self,
        recording_id: RecordingId,
        model_id: &str,
        cost_acknowledged: bool,
    ) -> Result<PreparedTranscription, AppError> {
        let media = self.media_facts(recording_id).await?;
        let card = self.card_for(ModelTask::Transcription, model_id).await?;
        if !card.is_selectable() {
            return Err(AppError::new(
                AppErrorCode::CapabilityUnverified,
                "the selected model has not passed the capability check",
            ));
        }
        if matches!(card.max_input_bytes.get(), Some(limit) if media.byte_length > *limit)
            || matches!(
                (card.max_duration_ms.get(), media.duration_ms),
                (Some(limit), Some(duration)) if duration > *limit
            )
        {
            return Err(AppError::new(
                AppErrorCode::ContextTooLarge,
                "the recording exceeds the model's observed limits",
            ));
        }
        self.require_key()?;

        let estimate = estimate(
            &card.pricing,
            EstimateInput::Audio {
                duration_ms: media.duration_ms,
            },
        );
        self.check_budget(estimate.amount, cost_acknowledged)
            .await?;
        if !cost_acknowledged
            && self
                .has_unacknowledged_unknown_outcome(recording_id, model_id)
                .await?
        {
            return Err(AppError::new(
                AppErrorCode::CostAcknowledgementRequired,
                "an earlier attempt may already have been billed",
            ));
        }

        let plan = self
            .authorize_dispatch(&recording_id.to_string(), model_id, ContentType::AudioBytes)
            .await?;
        Ok(PreparedTranscription {
            card,
            media,
            estimate,
            plan,
        })
    }

    pub(super) async fn publish_job(&self, job_id: JobId) {
        if let Ok(summary) = jobs::load(self.pool(), job_id).await {
            jobs::publish_update(self.events(), &summary).await;
        }
    }

    /// Send one prepared transcription and return what came back.
    ///
    /// State, progress and the commit belong to the worker that called this
    /// (`backend::processing`); the only thing that happens here is the
    /// dispatch itself, which is the one step that must not be repeated
    /// without a user's acknowledgement.
    pub(super) async fn dispatch_cloud_transcription(
        &self,
        recording_id: RecordingId,
        prepared: &PreparedTranscription,
        ctx: &JobContext,
    ) -> Result<TranscriptDraft, AppError> {
        let binding = JobBinding {
            job_id: ctx.job_id.to_string(),
            recording_id: recording_id.to_string(),
            consent_id: prepared.plan.grant_id.clone(),
            privacy_snapshot: prepared.plan.resolution.snapshot(),
            route_label: prepared.plan.resolution.route.downstream_provider.clone(),
        };
        let client = OpenRouterSpeechToText::new(
            prepared.card.clone(),
            self.dispatcher(&prepared.plan),
            binding,
        )
        .with_now_unix(now_unix());
        let media = &prepared.media;
        let mut input = SttInput::whole_file(
            media.path.clone(),
            media.sha256,
            media.byte_length,
            media.format.clone(),
            media.sample_rate_hz,
            media.channels,
        );
        input.duration_ms = media.duration_ms;

        let outcome = client
            .transcribe_with_cost(input, ctx.clone())
            .await
            .map_err(AppError::from)?;
        Ok(outcome.draft)
    }

    /// Record how a job ended when its dispatch did not succeed. An outcome
    /// that is unknown parks the job as possibly billed; it is never retried
    /// without an explicit cost acknowledgement.
    pub(super) async fn settle_failed_job(&self, job_id: JobId, error: &AppError) {
        let settled = if error.code() == AppErrorCode::OutcomeUnknown {
            self.park_outcome_unknown(job_id).await
        } else {
            jobs::fail(self.pool(), job_id, error).await.map(|_| ())
        };
        if let Err(storage) = settled {
            tracing::warn!(
                code = storage.code().as_str(),
                "cannot persist the end state of a cloud job"
            );
        }
        self.publish_job(job_id).await;
    }

    async fn park_outcome_unknown(&self, job_id: JobId) -> Result<(), AppError> {
        jobs::transition(
            self.pool(),
            job_id,
            JobState::CloudOutcomeUnknown,
            Some("outcome unknown"),
            None,
        )
        .await?;
        sqlx::query("UPDATE jobs SET last_error_code = ?2 WHERE id = ?1")
            .bind(job_id.to_string())
            .bind(AppErrorCode::OutcomeUnknown.as_str())
            .execute(self.pool())
            .await
            .map_err(|err| storage_error("cannot record the unknown outcome", &err))?;
        Ok(())
    }

    /// Mark attempts a crash left in flight as unknown and park their jobs
    /// as possibly billed (spec §14.3).
    pub(crate) async fn reconcile_cloud_attempts(&self) -> Result<usize, AppError> {
        let moved = self
            .cloud
            .ai
            .ledger()
            .reconcile_in_flight(Timestamp::now())
            .await
            .map_err(|err| store_error("cannot reconcile the ledger", err.detail))?;
        for attempt_id in &moved {
            sqlx::query(
                "UPDATE jobs SET state = 'cloud_outcome_unknown', last_error_code = ?2 \
                 WHERE id = (SELECT job_id FROM inference_attempts WHERE id = ?1)",
            )
            .bind(attempt_id)
            .bind(AppErrorCode::OutcomeUnknown.as_str())
            .execute(self.pool())
            .await
            .map_err(|err| storage_error("cannot park a possibly-billed job", &err))?;
        }
        Ok(moved.len())
    }

    /// Queue one stage for the worker pool, local or cloud.
    ///
    /// The job is keyed on the audio digest, the stage, the engine and the
    /// model, so asking twice for work already pending reuses it instead of
    /// running it twice (§14.2).
    async fn queue_stage(
        &self,
        request: &ValidatedStartProcessing,
        choice: &ModelChoice,
        estimated_cost: CostValue,
        cloud: bool,
    ) -> Result<StartProcessingResponse, AppError> {
        let media = self.media_facts(request.recording_id).await?;
        let stage = crate::backend::processing::StageRequest {
            recording_id: request.recording_id,
            kind: job_kind_for_stage(request.stage),
            engine: choice.engine.clone(),
            model_id: choice.model_id.clone(),
            stage: request.stage.as_str().to_owned(),
            label: format!("Process ({})", request.stage.as_str()),
            audio_sha256: media.sha256.to_hex(),
            cloud,
            cost_acknowledged: request.cost_acknowledged,
            transcript_revision: None,
            model_version: None,
            normalization: std::collections::BTreeMap::new(),
        };
        let job_id = self.enqueue_stage(&stage).await?;
        Ok(StartProcessingResponse {
            job_id: job_id.to_string(),
            estimated_cost,
            dispatch: "queued".to_owned(),
            blocked_reason: None,
        })
    }
}

#[async_trait]
impl CloudPort for LocalBackend {
    async fn list_models(
        &self,
        query: ValidatedListModels,
    ) -> Result<ListModelsResponse, AppError> {
        let default_model_id = self
            .cloud
            .ai
            .store()
            .task_default(query.stage)
            .await
            .map_err(|err| store_error("cannot read the stage default", err))?
            .filter(ModelChoice::is_cloud)
            .and_then(|choice| choice.model_id);
        let engine_matches = query
            .engine
            .as_deref()
            .is_none_or(|engine| engine == OPENROUTER_ENGINE_ID);
        let Some(task) = task_for_stage(query.stage).filter(|_| engine_matches) else {
            return Ok(ListModelsResponse {
                models: Vec::new(),
                catalog_source: "pinned_allowlist".to_owned(),
                catalog_fetched_at: None,
                degraded_reason: None,
                default_model_id,
            });
        };

        let preferences = self.preferences().await?;
        let key = self.api_key_source();
        let (client, forced_reason) = if preferences.local_only_mode {
            (None, Some("local_only_mode"))
        } else if !key.is_present() {
            (None, Some("no_key"))
        } else {
            let policy = Arc::new(Self::outbound_policy(false));
            (
                Some(CatalogClient::new(
                    GatedHttpClientFactory::new(policy),
                    Arc::clone(&self.cloud.transport),
                    Arc::new(key),
                )),
                None,
            )
        };
        let view = self.catalog_view(task, client.as_ref()).await?;
        let degraded_reason = forced_reason.map(str::to_owned).or_else(|| {
            view.degraded
                .map(|reason| degraded_label(reason).to_owned())
        });
        Ok(ListModelsResponse {
            models: view.cards.iter().map(card_dto).collect(),
            catalog_source: match view.source {
                CatalogSource::Live => "live",
                CatalogSource::PinnedAllowlist => "pinned_allowlist",
            }
            .to_owned(),
            catalog_fetched_at: view.fetched_at.map(|date| instant_of_date(&date)),
            degraded_reason,
            default_model_id,
        })
    }

    async fn get_usage(&self, range: ValidatedUsageRange) -> Result<GetUsageResponse, AppError> {
        let rows = self
            .cloud
            .ai
            .ledger()
            .attempts_between(range.from, range.to)
            .await
            .map_err(|err| store_error("cannot read the ledger", err.detail))?;
        let summary = summarize(&rows);
        Ok(GetUsageResponse {
            from: range
                .from
                .to_rfc3339()
                .ok_or_else(|| internal("range start is not representable"))?,
            to: range
                .to
                .to_rfc3339()
                .ok_or_else(|| internal("range end is not representable"))?,
            totals: totals_dto(&summary.totals),
            by_model: summary
                .by_model
                .iter()
                .map(|model| UsageByModel {
                    model_id: model.model_id.clone(),
                    attempts: model.totals.attempts,
                    finalized: model.totals.finalized.map(money_amount),
                    estimated: model.totals.estimated.map(money_amount),
                    unknown_count: model.totals.unknown_count,
                })
                .collect(),
            attempts: rows
                .iter()
                .map(attempt_dto)
                .collect::<Result<Vec<_>, _>>()?,
            budget: self.budget_dto().await?,
        })
    }

    async fn set_cloud_budget(&self, budget: ValidatedBudget) -> Result<CloudBudget, AppError> {
        self.cloud
            .ai
            .store()
            .set_budget(&DomainBudget {
                per_job_limit: budget.per_job_limit,
                monthly_limit: budget.monthly_limit,
            })
            .await
            .map_err(|err| store_error("cannot store the budget", err))?;
        self.budget_dto().await
    }

    async fn grant_cloud_consent(
        &self,
        request: ValidatedConsent,
    ) -> Result<GrantCloudConsentResponse, AppError> {
        let mut route = ProviderRoute::new(RELAY_OPENROUTER, request.model_id.clone());
        if let Some(provider) = &request.downstream_provider {
            route = route.with_downstream_provider(provider.clone());
        }
        let id = format!("consent-{}", uuid::Uuid::new_v4());
        let granted_at_unix = now_unix();
        let scope = match &request.scope {
            ValidatedConsentScope::AskEveryTime => GrantScope::AskEveryTime,
            ValidatedConsentScope::Recording(recording_id) => GrantScope::Recording {
                recording_id: recording_id.to_string(),
            },
            ValidatedConsentScope::ModelPolicy => GrantScope::ModelPolicy {
                route: route.clone(),
            },
        };
        let mut grant = ConsentGrant::new(
            id,
            scope,
            request.purpose,
            request.disclosure_version.clone(),
            granted_at_unix,
        )
        .with_content_type(request.content_type)
        .with_route(route);
        if request.require_zero_retention {
            grant = grant.with_required_retention(RetentionPosture::ZeroRetention);
        }
        // `AskEveryTime` is stored as a preference and never authorizes: the
        // preflight's scope check refuses it by construction.
        self.cloud.consents.upsert_grant(&grant).await?;
        Ok(GrantCloudConsentResponse {
            consent_id: grant.id.clone(),
            scope_kind: grant.scope.kind_label().to_owned(),
            granted_at: rfc3339("consent granted_at", granted_at_unix.saturating_mul(1000))?,
            expires_at: None,
        })
    }

    async fn revoke_cloud_consent(&self, consent_id: String) -> Result<(), AppError> {
        self.cloud
            .consents
            .mark_revoked(&consent_id, now_unix())
            .await
    }

    async fn set_model_preference(
        &self,
        preference: ValidatedPreference,
    ) -> Result<SetModelPreferenceResponse, AppError> {
        let store = self.cloud.ai.store();
        let now = Timestamp::now();
        if preference.engine == OPENROUTER_ENGINE_ID
            && preference.model_id.is_some()
            && self.preferences().await?.local_only_mode
        {
            return Err(AppError::new(
                AppErrorCode::LocalOnlyMode,
                "local-only mode forbids selecting a cloud model",
            ));
        }
        let choice = preference
            .model_id
            .clone()
            .map(|model| ModelChoice::new(preference.engine.clone(), Some(model)));
        let (scope_kind, recording_id) = match (preference.scope, &choice) {
            (ValidatedPreferenceScope::Default, Some(choice)) => {
                store
                    .set_task_default(preference.stage, choice, now)
                    .await
                    .map_err(|err| store_error("cannot store the default model", err))?;
                ("default", None)
            }
            // `validate::preference` refuses this; kept explicit so a default
            // can never be cleared by a recording-shaped request.
            (ValidatedPreferenceScope::Default, None) => {
                return Err(crate::error::invalid_argument(
                    "a default preference must name a model",
                ))
            }
            (ValidatedPreferenceScope::Recording(recording), choice) => {
                self.recording_row(recording).await?;
                let key = recording.to_string();
                match choice {
                    Some(choice) => store
                        .set_recording_override(&key, preference.stage, choice, now)
                        .await
                        .map_err(|err| store_error("cannot store the override", err))?,
                    None => store
                        .clear_recording_override(&key, preference.stage)
                        .await
                        .map_err(|err| store_error("cannot clear the override", err))?,
                }
                ("recording", Some(key))
            }
        };

        let effective = self
            .cloud
            .ai
            .effective_choice(
                recording_id.as_deref().unwrap_or_default(),
                preference.stage,
                None,
            )
            .await
            .map_err(|err| store_error("cannot resolve the effective model", err))?;
        let effective_model = match (effective, task_for_stage(preference.stage)) {
            (Some(effective), Some(task)) if effective.choice.is_cloud() => {
                match effective.choice.model_id {
                    Some(model_id) => Some(card_dto(&self.card_for(task, &model_id).await?)),
                    None => None,
                }
            }
            _ => None,
        };
        Ok(SetModelPreferenceResponse {
            stage: preference.stage.as_str().to_owned(),
            scope_kind: scope_kind.to_owned(),
            recording_id,
            engine: preference.engine,
            model_id: preference.model_id,
            effective_model,
        })
    }

    async fn start_processing(
        &self,
        request: ValidatedStartProcessing,
    ) -> Result<StartProcessingResponse, AppError> {
        self.recording_row(request.recording_id).await?;
        let effective = self
            .cloud
            .ai
            .effective_choice(
                &request.recording_id.to_string(),
                request.stage,
                request.model_override.as_ref(),
            )
            .await
            .map_err(|err| store_error("cannot resolve the model", err))?
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::ModelUnavailable,
                    "no model is configured for this stage",
                )
            })?;

        // A local engine never touches the network from here.
        if !effective.choice.is_cloud() {
            return self
                .queue_stage(&request, &effective.choice, CostValue::unknown(), false)
                .await;
        }
        let model_id = effective.choice.model_id.clone().ok_or_else(|| {
            AppError::new(
                AppErrorCode::CapabilityUnverified,
                "a cloud engine needs a model id",
            )
        })?;
        if request.stage != Stage::Transcribe {
            return Err(AppError::new(
                AppErrorCode::CapabilityUnverified,
                "this build only runs the transcribe stage in the cloud",
            ));
        }

        // Every pre-dispatch check runs *before* the job row exists, so a
        // refusal leaves no job, no ledger row and nothing transmitted. The
        // executor runs them again at dispatch time, because the answer can
        // change while a job waits in the queue.
        let prepared = self
            .prepare_cloud_transcription(request.recording_id, &model_id, request.cost_acknowledged)
            .await?;
        let estimated_cost = cost_value(prepared.estimate.amount);
        self.queue_stage(&request, &effective.choice, estimated_cost, true)
            .await
    }

    async fn verify_model(
        &self,
        request: ValidatedVerifyModel,
    ) -> Result<VerifyModelResponse, AppError> {
        if request.task == ModelTask::TextGeneration {
            return self.verify_text_model(request).await;
        }
        let card = self.card_for(request.task, &request.model_id).await?;
        let candidate = self
            .cloud
            .ai
            .allowlist()
            .entry(&request.model_id, request.task)
            .map(|entry| entry.candidate_family);
        let family = if card.request_family.has_builder() {
            card.request_family
        } else if card.request_family == RequestFamily::Unsupported {
            RequestFamily::Unsupported
        } else {
            candidate.unwrap_or(RequestFamily::Unknown)
        };
        // Nothing is sent for a model without a request builder.
        if !family.has_builder() {
            return Err(AppError::new(
                AppErrorCode::CapabilityUnverified,
                "no request contract is known for this model",
            ));
        }
        self.require_key()?;
        // The bundled clip has no price we can derive, so the estimate is
        // unknown and a configured limit needs an acknowledgement.
        self.check_budget(MoneyEstimate::Unknown, request.cost_acknowledged)
            .await?;
        let plan = self
            .authorize_dispatch(
                CAPABILITY_CHECK_RECORDING,
                &request.model_id,
                ContentType::AudioBytes,
            )
            .await?;

        let job_id = jobs::create(
            self.pool(),
            &JobDraft {
                kind: JobKind::Transcribe,
                label: "Model capability check".to_owned(),
                recording_id: None,
                config_snapshot: json!({
                    "engine": OPENROUTER_ENGINE_ID,
                    "model_id": request.model_id,
                    "purpose": "capability_check",
                }),
            },
        )
        .await?;
        jobs::transition(
            self.pool(),
            job_id,
            JobState::CloudSending,
            Some("sending"),
            None,
        )
        .await?;
        let binding = JobBinding {
            job_id: job_id.to_string(),
            recording_id: CAPABILITY_CHECK_RECORDING.to_owned(),
            consent_id: plan.grant_id.clone(),
            privacy_snapshot: plan.resolution.snapshot(),
            route_label: plan.resolution.route.downstream_provider.clone(),
        };
        let smoke = CapabilitySmokeTest::new(
            request.model_id.clone(),
            family,
            card.pricing.clone(),
            self.dispatcher(&plan),
            binding,
            today(),
        )
        .with_now_unix(now_unix());

        let outcome = match smoke.run(JobContext::new(job_id)).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let error = AppError::from(error);
                self.settle_failed_job(job_id, &error).await;
                return Err(error);
            }
        };
        self.cloud
            .ai
            .store()
            .record_probe(
                request.task,
                &outcome.record,
                CAPABILITY_FIXTURE_ID,
                Timestamp::now(),
            )
            .await
            .map_err(|err| store_error("cannot store the probe result", err))?;

        let mut conn = self
            .pool()
            .acquire()
            .await
            .map_err(|err| storage_error("cannot complete the job", &err))?;
        jobs::complete_in_tx(&mut conn, job_id, None).await?;
        drop(conn);
        self.publish_job(job_id).await;
        self.events()
            .publish(EventDraft::new(
                EventKind::ModelAvailabilityChanged,
                Timestamp::now(),
            ))
            .await;

        let refreshed = self.card_for(request.task, &request.model_id).await?;
        Ok(VerifyModelResponse {
            model: card_dto(&refreshed),
            cost: cost_value(outcome.cost.amount),
            provider_request_id: outcome.provider_request_id,
        })
    }

    async fn cloud_consents(&self) -> Result<Vec<ConsentSummary>, AppError> {
        let mut grants = self.cloud.consents.load_grants().await?;
        grants.sort_by(|a, b| {
            b.granted_at_unix
                .cmp(&a.granted_at_unix)
                .then_with(|| a.id.cmp(&b.id))
        });
        grants.iter().map(consent_summary).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_domain::capability::Known;

    fn usd(units: i128) -> Money {
        Money::new(units, AMOUNT_SCALE, Currency::USD)
    }

    #[test]
    fn an_unknown_cost_serializes_with_a_null_amount() {
        let value = cost_value(MoneyEstimate::Unknown);
        assert_eq!(value.confidence, CostConfidence::Unknown);
        assert!(value.amount.is_none());
    }

    #[test]
    fn an_estimate_is_never_reported_as_finalized() {
        assert_eq!(
            cost_value(MoneyEstimate::Estimated(usd(5))).confidence,
            CostConfidence::Estimated
        );
        assert_eq!(
            cost_value(MoneyEstimate::Finalized(usd(5))).confidence,
            CostConfidence::Finalized
        );
    }

    #[test]
    fn a_date_becomes_an_instant_and_an_instant_is_left_alone() {
        assert_eq!(instant_of_date("2026-10-11"), "2026-10-11T00:00:00Z");
        assert_eq!(
            instant_of_date("2026-10-11T01:02:03Z"),
            "2026-10-11T01:02:03Z"
        );
    }

    #[test]
    fn unknown_pricing_has_no_amount_and_no_invented_unit() {
        let dto = pricing_dto(&PricingUnits::unknown());
        assert_eq!(dto.basis, "other");
        assert!(dto.amount_per_unit.is_none());
        assert_eq!(dto.unit_label, "pricing not published");
    }

    #[test]
    fn a_catalog_price_carries_its_unit_label() {
        let mut units = PricingUnits::unknown();
        units.audio_input_per_token = Known::Known(usd(2));
        let dto = pricing_dto(&units);
        assert_eq!(dto.basis, "per_token");
        assert_eq!(dto.unit_label, "USD per audio input token");
        assert_eq!(dto.amount_per_unit.unwrap().currency, "USD");
    }

    async fn row_count(fixture: &crate::backend::testing::Fixture, table: &'static str) -> i64 {
        // Audited: `table` is a literal from this module.
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(fixture.backend.pool())
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_known_estimate_over_a_limit_is_refused_and_leaves_no_row() {
        use crate::backend::testing::backend_fixture;

        let fixture = backend_fixture().await;
        fixture
            .backend
            .set_cloud_budget(ValidatedBudget {
                per_job_limit: Some(usd(10_000_000_000)),
                monthly_limit: None,
            })
            .await
            .unwrap();

        let over = fixture
            .backend
            .check_budget(MoneyEstimate::Estimated(usd(20_000_000_000)), true)
            .await
            .unwrap_err();
        assert_eq!(over.code(), AppErrorCode::BudgetExceeded);
        let under = fixture
            .backend
            .check_budget(MoneyEstimate::Estimated(usd(5_000_000_000)), false)
            .await;
        assert!(under.is_ok());
        let unknown = fixture
            .backend
            .check_budget(MoneyEstimate::Unknown, false)
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), AppErrorCode::CostAcknowledgementRequired);
        assert!(fixture
            .backend
            .check_budget(MoneyEstimate::Unknown, true)
            .await
            .is_ok());

        assert_eq!(row_count(&fixture, "inference_attempts").await, 0);
        assert_eq!(row_count(&fixture, "jobs").await, 0);
    }
}
