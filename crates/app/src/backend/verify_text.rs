//! The text-generation capability check (matrix row A4, spec §9.5, §10.1,
//! §10.2, §8.6A).
//!
//! A text model is `Verified` only after one real exchange demonstrated the
//! thing the application actually depends on: that the model accepts a
//! schema-constrained request and returns a body satisfying that exact
//! schema, within a declared output budget. Nothing in a catalogue entry can
//! establish either, so before this module existed no text model could ever
//! be verified and every transcript tool was refused for want of an observed
//! output budget.
//!
//! Two routes reach that observation, and they are deliberately different:
//!
//! * **Cloud.** One real, chargeable `chat/completions` exchange, in the same
//!   order [`crate::backend::cloud`] documents for every dispatch: resolve the
//!   card, refuse a model that cannot answer the check, require the key,
//!   evaluate the budget against the pre-dispatch estimate, run the consent
//!   preflight, and only then open the job row and hand the work to the
//!   client, which commits its ledger row before the first byte moves. A
//!   refusal in the first steps leaves no job row, no ledger row and no
//!   transmitted byte.
//! * **Local.** One on-device generation through the same contract. It costs
//!   nothing, sends nothing and asks for nothing: no key, no budget decision,
//!   no consent grant and no job row, because none of those protect anything
//!   when no byte leaves the machine. The cost it reports is a known zero,
//!   never an invented charge.
//!
//! Both record the same evidence through the same store, so the card reads
//! back the same way for either. An exchange that did not produce
//! schema-valid output is recorded as the negative observation it is: the job
//! fails, the model stays unverified, and the next run has one more fact to
//! work from.

use std::sync::Arc;

use plaudon_ai_contracts::capabilities::{PrivacyClass, RequestFamily, TextModelCapabilities};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{StructuredRequest, TextModel};
use plaudon_ai_openrouter::allowlist::ModelTask;
use plaudon_ai_openrouter::catalog::{ModelProbeRecord, ProbeEvidence, TimestampSupport};
use plaudon_ai_openrouter::client::JobBinding;
use plaudon_ai_openrouter::registry::OPENROUTER_ENGINE_ID;
use plaudon_ai_openrouter::service::today;
use plaudon_ai_openrouter::service::usage::AMOUNT_SCALE;
use plaudon_ai_openrouter::smoke::{
    text_probe_body_is_valid, text_probe_estimate, text_probe_schema, TextCapabilityCheck,
    TEXT_PROBE_OUTPUT_TOKENS, TEXT_PROBE_PROMPT, TEXT_PROBE_SCHEMA_VERSION,
};
use plaudon_app_core::engine_policy::{explain, APPLE_TEXT_ENGINE_ID, LOCAL_LLM_ENGINE_ID};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::JobId;
use plaudon_domain::money::{Currency, Money, MoneyEstimate};
use plaudon_domain::time::Timestamp;
use plaudon_security::consent::ContentType;
use serde_json::json;

use crate::backend::cloud::{card_dto, cost_value, CAPABILITY_CHECK_RECORDING};
use crate::backend::jobs::{self, JobDraft};
use crate::backend::store::storage_error;
use crate::backend::LocalBackend;
use crate::dto::{EventKind, JobKind, JobState, ModelPricing, ModelPrivacy, VerifyModelResponse};
use crate::error::internal;
use crate::events::EventDraft;
use crate::validate::ValidatedVerifyModel;

/// Fixture id stored with an observed text capability-check exchange.
///
/// Distinct from the transcription fixture: the two record different
/// observations about the same model and must stay separately attributable.
const TEXT_FIXTURE_ID: &str = "capability-check-text";

/// Version label of the prompt the check sends.
const PROBE_PROMPT_VERSION: &str = "capability_check_v1";

/// Smallest context window that can hold the check at all.
///
/// The declared output budget plus headroom for the prompt and the schema. A
/// model whose catalogue entry states less than this cannot answer, so the
/// check refuses before the user is charged for proving it.
const PROBE_MIN_CONTEXT_TOKENS: u64 = TEXT_PROBE_OUTPUT_TOKENS + 256;

/// Retention label recorded for an exchange that never left the machine.
const ON_DEVICE_RETENTION: &str = "on_device_no_transmission";

/// Whether a model identifier names an engine that runs on this machine.
fn is_local_engine(model_id: &str) -> bool {
    model_id == APPLE_TEXT_ENGINE_ID || model_id == LOCAL_LLM_ENGINE_ID
}

/// The request every capability check sends, cloud or local.
fn probe_request() -> StructuredRequest {
    StructuredRequest {
        prompt_version: PROBE_PROMPT_VERSION.to_owned(),
        prompt: TEXT_PROBE_PROMPT.to_owned(),
        schema_version: TEXT_PROBE_SCHEMA_VERSION.to_owned(),
        output_schema: text_probe_schema(),
        max_output_tokens: Some(TEXT_PROBE_OUTPUT_TOKENS),
    }
}

/// The record one observed text exchange justifies.
///
/// `provider_route` is what served the exchange: the downstream provider for
/// a cloud route, the engine itself for an on-device one. Everything the
/// exchange could not observe — and a text exchange observes nothing at all
/// about the audio contract — stays unknown.
fn text_record(
    model_id: &str,
    provider_route: &str,
    retention_control: Known<String>,
    schema_valid: bool,
) -> ModelProbeRecord {
    ModelProbeRecord {
        model_id: model_id.to_owned(),
        // Text generation has no audio request family (App. A.3).
        request_family: RequestFamily::Unsupported,
        evidence: ProbeEvidence::ObservedExchange,
        accepted_formats: Known::Unknown,
        max_input_bytes: Known::Unknown,
        max_duration_ms: Known::Unknown,
        timestamps: TimestampSupport::NotObserved,
        diarization: Tri::Unknown,
        language_detection: Tri::Unknown,
        provider_route: Known::Known(provider_route.to_owned()),
        retention_control,
        audio_input_price: Known::Unknown,
        pricing_unit: Known::Unknown,
        audio_tokens_per_second: Known::Unknown,
        structured_output: Tri::from_observation(Some(schema_valid)),
        max_output_tokens: if schema_valid {
            Known::Known(TEXT_PROBE_OUTPUT_TOKENS)
        } else {
            Known::Unknown
        },
        observed_on: today(),
        notes: String::new(),
    }
}

/// A charge of exactly nothing, which is what an on-device run costs.
///
/// Finalized rather than estimated: there is no provider to bill and no
/// figure left to discover (spec §10.4 — a zero must be a known zero).
fn free() -> MoneyEstimate {
    MoneyEstimate::Finalized(Money::new(0, AMOUNT_SCALE, Currency::USD))
}

/// Pricing of an engine that charges nothing.
fn on_device_pricing() -> ModelPricing {
    ModelPricing {
        basis: "none".to_owned(),
        unit_label: "no charge: runs on this device".to_owned(),
        amount_per_unit: None,
    }
}

/// Privacy of an engine nothing is transmitted to.
fn on_device_privacy(engine: &str) -> ModelPrivacy {
    ModelPrivacy {
        relay: engine.to_owned(),
        downstream_provider: None,
        retention: plaudon_security::consent::RetentionPosture::ZeroRetention
            .kind_label()
            .to_owned(),
        retention_days: None,
        zero_retention_available: true,
    }
}

impl LocalBackend {
    /// Check whether a text model really supports the structured output every
    /// transcript tool depends on.
    ///
    /// # Errors
    ///
    /// Every refusal is typed: no stored key, a budget block, Local-only Mode,
    /// a missing consent grant and an unavailable engine all return before
    /// anything is sent. A transmitted request with no answer leaves the job
    /// parked as possibly billed and is never retried automatically.
    pub(crate) async fn verify_text_model(
        &self,
        request: ValidatedVerifyModel,
    ) -> Result<VerifyModelResponse, AppError> {
        debug_assert_eq!(request.task, ModelTask::TextGeneration);
        // The engine seam replaces the model under check, never the policy:
        // whether it is gated is decided by the privacy class it reports.
        if let Some(model) = self.cloud.text_model() {
            let model = Arc::clone(model);
            return self.verify_installed_model(&request, model).await;
        }
        if is_local_engine(&request.model_id) {
            return self.verify_local_text_model(&request).await;
        }
        self.verify_cloud_text_model(&request).await
    }

    /// The on-device engines, each checked by actually running it.
    async fn verify_local_text_model(
        &self,
        request: &ValidatedVerifyModel,
    ) -> Result<VerifyModelResponse, AppError> {
        if request.model_id == APPLE_TEXT_ENGINE_ID {
            let model = Arc::new(plaudon_ai_apple::text::AppleTextModel::new(
                plaudon_ai_apple::host_bridge(),
                crate::backend::capabilities::apple_probe_cache(),
            )) as Arc<dyn TextModel>;
            return self.verify_installed_model(request, model).await;
        }
        // The local runtime and its weights are user-managed artifacts this
        // build has no installer for yet (spec §12), so there is nothing to
        // run and nothing honest to report but that.
        Err(AppError::new(
            AppErrorCode::ModelUnavailable,
            "this build has no installed local text model to check",
        ))
    }

    /// Run the check against a model this process already holds.
    ///
    /// An on-device model is checked for free. A model that reports a cloud
    /// privacy class goes through the key, the budget, the egress gate and
    /// the consent grant first, exactly as a resolved cloud route does.
    async fn verify_installed_model(
        &self,
        request: &ValidatedVerifyModel,
        model: Arc<dyn TextModel>,
    ) -> Result<VerifyModelResponse, AppError> {
        let capabilities = model.capabilities().await;
        if capabilities.status != CapabilityStatus::Supported {
            return Err(AppError::new(
                AppErrorCode::ModelUnavailable,
                explain(&capabilities.engine_id, &capabilities.availability_reason).headline,
            ));
        }
        let engine = capabilities.engine_id.as_str().to_owned();

        if on_device(&capabilities) {
            return self
                .run_on_device_check(request, model.as_ref(), &engine)
                .await;
        }

        // A cloud engine reached through the seam is still a cloud dispatch.
        self.require_key()?;
        self.check_budget(MoneyEstimate::Unknown, request.cost_acknowledged)
            .await?;
        let plan = self
            .authorize_dispatch(
                CAPABILITY_CHECK_RECORDING,
                &request.model_id,
                ContentType::TranscriptText,
            )
            .await?;
        let job_id = self.open_check_job(&request.model_id, &engine).await?;
        let route = plan
            .resolution
            .route
            .downstream_provider
            .clone()
            .unwrap_or_else(|| plan.resolution.route.relay.clone());

        let schema_valid = match model
            .generate_structured(probe_request(), JobContext::new(job_id))
            .await
        {
            Ok(response) => text_probe_body_is_valid(&response.body),
            Err(error) => {
                let error = AppError::from(error);
                self.settle_failed_job(job_id, &error).await;
                return Err(error);
            }
        };
        let record = text_record(&request.model_id, &route, Known::Unknown, schema_valid);
        self.store_text_probe(&record).await?;
        if !schema_valid {
            return Err(self.fail_unstructured(job_id).await);
        }
        self.complete_check_job(job_id).await?;
        self.respond(&request.model_id, None, MoneyEstimate::Unknown, None)
            .await
    }

    /// One free on-device generation, with nothing around it.
    async fn run_on_device_check(
        &self,
        request: &ValidatedVerifyModel,
        model: &dyn TextModel,
        engine: &str,
    ) -> Result<VerifyModelResponse, AppError> {
        // No job row: nothing is billable, nothing can be left in doubt and
        // there is no ledger foreign key to satisfy.
        let response = model
            .generate_structured(probe_request(), JobContext::new(JobId::default()))
            .await
            .map_err(AppError::from)?;
        let schema_valid = text_probe_body_is_valid(&response.body);
        let record = text_record(
            &request.model_id,
            engine,
            Known::Known(ON_DEVICE_RETENTION.to_owned()),
            schema_valid,
        );
        self.store_text_probe(&record).await?;
        if !schema_valid {
            return Err(AppError::new(
                AppErrorCode::SchemaValidationFailed,
                "the model did not return output matching the requested schema",
            ));
        }
        self.respond(&request.model_id, Some(engine), free(), None)
            .await
    }

    /// One real, chargeable exchange with a cloud model.
    async fn verify_cloud_text_model(
        &self,
        request: &ValidatedVerifyModel,
    ) -> Result<VerifyModelResponse, AppError> {
        let card = self
            .card_for(ModelTask::TextGeneration, &request.model_id)
            .await?;
        // Nothing is sent to a model the catalogue already shows cannot hold
        // the check: a request that is certain to be refused still costs.
        if card
            .context_length
            .get()
            .is_some_and(|context| *context < PROBE_MIN_CONTEXT_TOKENS)
        {
            return Err(AppError::new(
                AppErrorCode::ContextTooLarge,
                "this model's context window is too small for the capability check",
            ));
        }
        self.require_key()?;
        let estimate = text_probe_estimate(&card.pricing);
        self.check_budget(estimate.amount, request.cost_acknowledged)
            .await?;
        let plan = self
            .authorize_dispatch(
                CAPABILITY_CHECK_RECORDING,
                &request.model_id,
                ContentType::TranscriptText,
            )
            .await?;

        let job_id = self
            .open_check_job(&request.model_id, OPENROUTER_ENGINE_ID)
            .await?;
        let binding = JobBinding {
            job_id: job_id.to_string(),
            recording_id: CAPABILITY_CHECK_RECORDING.to_owned(),
            consent_id: plan.grant_id.clone(),
            privacy_snapshot: plan.resolution.snapshot(),
            route_label: plan.resolution.route.downstream_provider.clone(),
        };
        let check = TextCapabilityCheck::new(
            request.model_id.clone(),
            card.pricing.clone(),
            self.dispatcher(&plan),
            binding,
            today(),
        )
        .with_now_unix(now_unix());

        let outcome = match check.run(JobContext::new(job_id)).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let error = AppError::from(error);
                self.settle_failed_job(job_id, &error).await;
                return Err(error);
            }
        };
        // Recorded either way: a model observed answering with prose is a
        // fact about that model, and the next check starts from it.
        self.store_text_probe(&outcome.record).await?;
        if !outcome.verified() {
            return Err(self.fail_unstructured(job_id).await);
        }
        self.complete_check_job(job_id).await?;
        self.respond(
            &request.model_id,
            None,
            outcome.cost.amount,
            outcome.provider_request_id,
        )
        .await
    }

    /// The job row one cloud capability check runs under.
    async fn open_check_job(&self, model_id: &str, engine: &str) -> Result<JobId, AppError> {
        let job_id = jobs::create(
            self.pool(),
            &JobDraft {
                kind: JobKind::Summarize,
                label: "Model capability check".to_owned(),
                recording_id: None,
                config_snapshot: json!({
                    "engine": engine,
                    "model_id": model_id,
                    "purpose": "capability_check",
                    "task": ModelTask::TextGeneration.label(),
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
        Ok(job_id)
    }

    async fn complete_check_job(&self, job_id: JobId) -> Result<(), AppError> {
        let mut conn = self
            .pool()
            .acquire()
            .await
            .map_err(|err| storage_error("cannot complete the job", &err))?;
        jobs::complete_in_tx(&mut conn, job_id, None).await?;
        drop(conn);
        self.publish_job(job_id).await;
        Ok(())
    }

    /// End the job of a check whose answer was not structured output.
    ///
    /// Never `Completed`: the exchange happened and was charged, but it
    /// demonstrated the opposite of what the check asks.
    async fn fail_unstructured(&self, job_id: JobId) -> AppError {
        let error = AppError::new(
            AppErrorCode::SchemaValidationFailed,
            "the model did not return output matching the requested schema",
        );
        self.settle_failed_job(job_id, &error).await;
        error
    }

    /// Persist one observed record and announce that the card moved.
    async fn store_text_probe(&self, record: &ModelProbeRecord) -> Result<(), AppError> {
        self.cloud_ai()
            .store()
            .record_probe(
                ModelTask::TextGeneration,
                record,
                TEXT_FIXTURE_ID,
                Timestamp::now(),
            )
            .await
            .map_err(|err| internal(format!("cannot store the probe result: {err}")))?;
        self.events()
            .publish(EventDraft::new(
                EventKind::ModelAvailabilityChanged,
                Timestamp::now(),
            ))
            .await;
        Ok(())
    }

    /// The refreshed card, read back through the same path the picker uses.
    ///
    /// `local_engine` replaces the three fields the OpenRouter card shape
    /// hard-codes — the engine, its relay and its prices — for a model that
    /// has none of them. The wire shape is unchanged.
    async fn respond(
        &self,
        model_id: &str,
        local_engine: Option<&str>,
        cost: MoneyEstimate,
        provider_request_id: Option<String>,
    ) -> Result<VerifyModelResponse, AppError> {
        let card = self.card_for(ModelTask::TextGeneration, model_id).await?;
        let mut model = card_dto(&card);
        if let Some(engine) = local_engine {
            engine.clone_into(&mut model.engine);
            model.pricing = on_device_pricing();
            model.privacy = on_device_privacy(engine);
        }
        Ok(VerifyModelResponse {
            model,
            cost: cost_value(cost),
            provider_request_id,
        })
    }
}

/// Whether the model under check runs on this machine.
fn on_device(capabilities: &TextModelCapabilities) -> bool {
    capabilities.privacy_class.get().copied() == Some(PrivacyClass::OnDevice)
}

/// Seconds since the Unix epoch, for the dispatcher's `Retry-After`
/// arithmetic.
fn now_unix() -> i64 {
    Timestamp::now().as_millis().div_euclid(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use plaudon_ai_openrouter::transport::{AsyncHttpTransport, TransportFailure};
    use plaudon_ai_tools::registry::ToolRegistry;
    use plaudon_security::consent::ConsentPurpose;
    use plaudon_security::outbound::http::{GatedRequest, HttpResponse};

    use crate::backend::testing::{backend_fixture, Fixture};
    use crate::ports::{CloudPort, SettingsPort};
    use crate::secrets::{SecretStore as _, SecretValue};
    use crate::testing_engines::ScriptedTextModel;
    use crate::validate::{ValidatedConsent, ValidatedConsentScope};

    const MODEL: &str = "vendor/text-a";

    /// A transport that answers once and counts everything it was asked to
    /// send. Zero requests is how "nothing left the machine" is asserted.
    #[derive(Debug, Default)]
    struct RecordingTransport {
        reply: Mutex<Option<HttpResponse>>,
        requests: Mutex<usize>,
    }

    impl RecordingTransport {
        fn answering(body: &serde_json::Value) -> Arc<Self> {
            Arc::new(Self {
                reply: Mutex::new(Some(HttpResponse {
                    status: 200,
                    headers: BTreeMap::from([(
                        "x-request-id".to_owned(),
                        "req_capability".to_owned(),
                    )]),
                    body: serde_json::to_vec(body).expect("body"),
                })),
                requests: Mutex::new(0),
            })
        }

        fn silent() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn requests(&self) -> usize {
            *self.requests.lock().expect("lock")
        }
    }

    #[async_trait]
    impl AsyncHttpTransport for RecordingTransport {
        async fn send(&self, _request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
            *self.requests.lock().expect("lock") += 1;
            self.reply
                .lock()
                .expect("lock")
                .take()
                .ok_or(TransportFailure::ConnectFailed {
                    detail: "no reply queued".to_owned(),
                })
        }
    }

    /// A chat completion carrying `content` as the assistant's answer.
    fn chat_reply(content: &str) -> serde_json::Value {
        json!({
            "id": "gen-capability",
            "provider": "OpenAI",
            "choices": [{ "message": { "role": "assistant", "content": content } }],
            "usage": { "prompt_tokens": 24, "completion_tokens": 8, "total_tokens": 32 }
        })
    }

    fn verify(model_id: &str) -> ValidatedVerifyModel {
        ValidatedVerifyModel {
            model_id: model_id.to_owned(),
            task: ModelTask::TextGeneration,
            cost_acknowledged: true,
        }
    }

    async fn grant(fixture: &Fixture, model_id: &str) {
        fixture
            .backend
            .grant_cloud_consent(ValidatedConsent {
                purpose: ConsentPurpose::Summarization,
                content_type: ContentType::TranscriptText,
                scope: ValidatedConsentScope::ModelPolicy,
                model_id: model_id.to_owned(),
                downstream_provider: None,
                require_zero_retention: false,
                disclosure_version: crate::backend::cloud::CURRENT_DISCLOSURE_VERSION.to_owned(),
            })
            .await
            .expect("the grant is stored");
    }

    fn store_key(fixture: &Fixture) {
        fixture
            .secrets
            .set_openrouter_key(&SecretValue::new("sk-or-test-key".to_owned()).expect("a key"))
            .expect("the key is stored");
    }

    async fn job_count(fixture: &Fixture) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs")
            .fetch_one(fixture.backend.pool())
            .await
            .expect("the jobs table is readable")
    }

    async fn job_states(fixture: &Fixture) -> Vec<String> {
        sqlx::query_scalar::<_, String>("SELECT state FROM jobs ORDER BY created_at")
            .fetch_all(fixture.backend.pool())
            .await
            .expect("the jobs table is readable")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_real_exchange_verifies_a_cloud_text_model() {
        let fixture = backend_fixture().await;
        store_key(&fixture);
        grant(&fixture, MODEL).await;
        let transport = RecordingTransport::answering(&chat_reply("{\"ok\":true,\"items\":1}"));
        let backend = fixture
            .backend
            .clone()
            .with_cloud_transport(Arc::clone(&transport) as Arc<dyn AsyncHttpTransport>);

        let response = backend
            .verify_model(verify(MODEL))
            .await
            .expect("the capability check passes");

        assert_eq!(response.model.verification, "verified");
        assert!(response.model.selectable);
        assert_eq!(response.model.engine, OPENROUTER_ENGINE_ID);
        assert_eq!(transport.requests(), 1);
        assert_eq!(job_states(&fixture).await, vec!["completed".to_owned()]);

        // The card now carries what the exchange observed, and that is what
        // a transcript tool reads.
        let card = backend
            .card_for(ModelTask::TextGeneration, MODEL)
            .await
            .expect("the card is readable");
        assert_eq!(
            card.max_output_tokens,
            Known::Known(TEXT_PROBE_OUTPUT_TOKENS)
        );
        assert_eq!(card.supports_structured_output, Tri::Yes);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_verified_text_model_satisfies_every_shipped_tool() {
        let fixture = backend_fixture().await;
        store_key(&fixture);
        grant(&fixture, MODEL).await;
        // The documented retention policy for this exact route; without one
        // the card's privacy class is `Unresolved` and every tool refuses it
        // for that reason instead (spec §10.8).
        fixture
            .backend
            .cloud_ai()
            .store()
            .record_route_policy(
                &plaudon_ai_openrouter::privacy::DocumentedPolicy {
                    model_id: MODEL.to_owned(),
                    downstream_provider: None,
                    content_type: ContentType::TranscriptText,
                    posture: plaudon_security::consent::RetentionPosture::ZeroRetention,
                },
                "test fixture",
                Timestamp::now(),
            )
            .await
            .expect("the route policy is stored");
        let transport = RecordingTransport::answering(&chat_reply("{\"ok\":true,\"items\":1}"));
        let backend = fixture
            .backend
            .clone()
            .with_cloud_transport(transport as Arc<dyn AsyncHttpTransport>);
        backend
            .verify_model(verify(MODEL))
            .await
            .expect("the capability check passes");

        let card = backend
            .card_for(ModelTask::TextGeneration, MODEL)
            .await
            .expect("the card is readable");
        let mut capabilities = crate::backend::tools::cloud_capabilities(&card);
        assert_eq!(
            capabilities.max_output_tokens,
            Known::Known(TEXT_PROBE_OUTPUT_TOKENS)
        );
        assert_eq!(capabilities.supports_structured_output, Tri::Yes);

        // The context window is catalogue data, which a hermetic test has no
        // live catalogue to supply; everything else the gate asks for now
        // comes from the observed exchange.
        capabilities.max_context_tokens = Known::Known(128_000);
        for tool in ToolRegistry::with_defaults().latest_all() {
            tool.ensure_runnable(&capabilities)
                .unwrap_or_else(|_| panic!("{} is still refused", tool.id.as_str()));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unstructured_answer_fails_the_job_and_leaves_the_model_unverified() {
        let fixture = backend_fixture().await;
        store_key(&fixture);
        grant(&fixture, MODEL).await;
        let transport = RecordingTransport::answering(&chat_reply("Sure! Here you go."));
        let backend = fixture
            .backend
            .clone()
            .with_cloud_transport(Arc::clone(&transport) as Arc<dyn AsyncHttpTransport>);

        let error = backend
            .verify_model(verify(MODEL))
            .await
            .expect_err("prose does not verify a structured-output model");
        assert_eq!(error.code(), AppErrorCode::SchemaValidationFailed);
        assert_eq!(transport.requests(), 1);
        let states = job_states(&fixture).await;
        assert_eq!(states.len(), 1);
        assert!(states[0].starts_with("failed"), "{states:?}");

        let card = backend
            .card_for(ModelTask::TextGeneration, MODEL)
            .await
            .expect("the card is readable");
        assert!(!card.verification.is_verified());
        // The negative is recorded, not invented away.
        assert_eq!(card.supports_structured_output, Tri::No);
        assert_eq!(card.max_output_tokens, Known::Unknown);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_a_key_nothing_is_sent_and_no_job_row_exists() {
        let fixture = backend_fixture().await;
        grant(&fixture, MODEL).await;
        let transport = RecordingTransport::silent();
        let backend = fixture
            .backend
            .clone()
            .with_cloud_transport(Arc::clone(&transport) as Arc<dyn AsyncHttpTransport>);

        let error = backend
            .verify_model(verify(MODEL))
            .await
            .expect_err("a check without a key is refused");
        assert_eq!(error.code(), AppErrorCode::AuthenticationRequired);
        assert_eq!(transport.requests(), 0);
        assert_eq!(job_count(&fixture).await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_a_consent_grant_nothing_is_sent_and_no_job_row_exists() {
        let fixture = backend_fixture().await;
        store_key(&fixture);
        let transport = RecordingTransport::silent();
        let backend = fixture
            .backend
            .clone()
            .with_cloud_transport(Arc::clone(&transport) as Arc<dyn AsyncHttpTransport>);

        let error = backend
            .verify_model(verify(MODEL))
            .await
            .expect_err("a check without consent is refused");
        assert_eq!(error.code(), AppErrorCode::ConsentRequired);
        assert_eq!(transport.requests(), 0);
        assert_eq!(job_count(&fixture).await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_only_mode_refuses_the_cloud_check_before_anything_is_sent() {
        let fixture = backend_fixture().await;
        store_key(&fixture);
        grant(&fixture, MODEL).await;
        fixture
            .backend
            .set_local_only_mode(true)
            .await
            .expect("local-only mode is enabled");
        let transport = RecordingTransport::silent();
        let backend = fixture
            .backend
            .clone()
            .with_cloud_transport(Arc::clone(&transport) as Arc<dyn AsyncHttpTransport>);

        let error = backend
            .verify_model(verify(MODEL))
            .await
            .expect_err("local-only mode refuses the dispatch");
        assert_eq!(error.code(), AppErrorCode::LocalOnlyMode);
        assert_eq!(transport.requests(), 0);
        assert_eq!(job_count(&fixture).await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_on_device_check_costs_nothing_and_touches_no_network() {
        let fixture = backend_fixture().await;
        let transport = RecordingTransport::silent();
        let backend = fixture
            .backend
            .clone()
            .with_cloud_transport(Arc::clone(&transport) as Arc<dyn AsyncHttpTransport>)
            .with_text_model(ScriptedTextModel::new(json!({ "ok": true, "items": 1 })).shared());

        let response = backend
            .verify_model(verify(APPLE_TEXT_ENGINE_ID))
            .await
            .expect("the on-device check passes");

        assert_eq!(response.model.verification, "verified");
        assert_eq!(
            response.model.engine,
            crate::testing_engines::SCRIPTED_TEXT_ENGINE
        );
        assert_eq!(response.model.pricing.basis, "none");
        assert!(response.model.pricing.amount_per_unit.is_none());
        assert_eq!(
            response.cost.confidence,
            crate::dto::CostConfidence::Finalized
        );
        let zero = plaudon_ai_openrouter::service::usage::money_to_string(Money::new(
            0,
            AMOUNT_SCALE,
            Currency::USD,
        ));
        assert_eq!(
            response
                .cost
                .amount
                .as_ref()
                .map(|amount| amount.amount.clone()),
            Some(zero)
        );
        assert!(response.provider_request_id.is_none());
        // No key, no grant, no job row, no byte.
        assert_eq!(transport.requests(), 0);
        assert_eq!(job_count(&fixture).await, 0);

        let card = backend
            .card_for(ModelTask::TextGeneration, APPLE_TEXT_ENGINE_ID)
            .await
            .expect("the card is readable");
        assert_eq!(
            card.max_output_tokens,
            Known::Known(TEXT_PROBE_OUTPUT_TOKENS)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_on_device_model_that_cannot_answer_stays_unverified() {
        let fixture = backend_fixture().await;
        let backend = fixture
            .backend
            .clone()
            .with_text_model(ScriptedTextModel::new(json!({ "sorry": "no" })).shared());

        let error = backend
            .verify_model(verify(APPLE_TEXT_ENGINE_ID))
            .await
            .expect_err("a body that misses the schema does not verify");
        assert_eq!(error.code(), AppErrorCode::SchemaValidationFailed);

        let card = backend
            .card_for(ModelTask::TextGeneration, APPLE_TEXT_ENGINE_ID)
            .await
            .expect("the card is readable");
        assert!(!card.verification.is_verified());
        assert_eq!(card.max_output_tokens, Known::Unknown);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_engine_this_build_cannot_run_is_named_rather_than_pretended_at() {
        let fixture = backend_fixture().await;
        let error = fixture
            .backend
            .verify_model(verify(LOCAL_LLM_ENGINE_ID))
            .await
            .expect_err("there is no installed local text model");
        assert_eq!(error.code(), AppErrorCode::ModelUnavailable);
        assert_eq!(job_count(&fixture).await, 0);
    }
}
