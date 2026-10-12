//! The capability smoke test that turns a pinned candidate into a `Verified`
//! model (spec 10.1, 10.2, 8.6A).
//!
//! `Verified` means "passing our actual capability smoke test on that API
//! contract". This module is that test. It sends **one** short bundled clip to
//! one model, with a consent grant and a ledger row exactly like any other
//! dispatch, and records what the exchange actually demonstrated:
//!
//! * which request family answered;
//! * which response fields were present (segments, words, speaker labels,
//!   detected language);
//! * which provider route served it;
//! * what it cost, when the provider reported a cost.
//!
//! Everything it cannot observe stays unknown. A smoke test never promotes a
//! model on the strength of a catalogue entry, and a failed smoke test leaves
//! the model exactly as unverified as it was.
//!
//! [`TextCapabilityCheck`] is the same thing for a text model. Text
//! generation has no request family to establish (App. A.3), so what one
//! exchange has to demonstrate instead is that the provider accepted a
//! schema-constrained request and returned a body that satisfies that exact
//! schema, which route served it, and the output budget it answered within.
//! A body that is not schema-valid is recorded as the negative observation it
//! is; it never verifies the model.

use plaudon_ai_contracts::capabilities::{MediaFormat, RequestFamily};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::stt::SttError;
use plaudon_ai_contracts::text::TextModelError;
use plaudon_domain::capability::{Known, Tri};
use plaudon_domain::media::Sha256Digest;
use plaudon_security::consent::ContentType;

use crate::catalog::{ModelProbeRecord, ProbeEvidence, TimestampSupport};
use crate::client::{CloudDispatcher, JobBinding};
use crate::ledger::AttemptIntent;
use crate::pricing::{CostEstimate, EstimateGap, PricingUnits, SettledCost};
use crate::wire::{multipart_boundary, ChatCompletionResponse, TranscriptionResponse};
use crate::{AudioChatRequest, MultipartSttRequest, TextGenerationRequest};

/// The clip the smoke test uploads.
///
/// One second of 16 kHz mono PCM. It is deliberately tiny: the test exists to
/// observe a wire contract, not to measure transcription quality, and a user
/// pays for every byte of it.
pub const SMOKE_CLIP: &[u8] = include_bytes!("../fixtures/capability-check.wav");

/// Format of [`SMOKE_CLIP`].
#[must_use]
pub fn smoke_clip_format() -> MediaFormat {
    MediaFormat::new("wav", "pcm_s16le")
}

/// What one smoke test established.
#[derive(Debug, Clone)]
pub struct SmokeTestOutcome {
    /// The record to merge into the capability registry.
    pub record: ModelProbeRecord,
    /// What the test itself cost.
    pub cost: SettledCost,
    /// Vendor request id, when returned.
    pub provider_request_id: Option<String>,
}

/// Runs one capability smoke test against one candidate model.
#[derive(Debug, Clone)]
pub struct CapabilitySmokeTest {
    model_id: String,
    candidate_family: RequestFamily,
    pricing: PricingUnits,
    dispatcher: CloudDispatcher,
    binding: JobBinding,
    observed_on: String,
    now_unix: i64,
}

impl CapabilitySmokeTest {
    /// Prepare a smoke test for `model_id` using the family our fixtures cover
    /// for it.
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        candidate_family: RequestFamily,
        pricing: PricingUnits,
        dispatcher: CloudDispatcher,
        binding: JobBinding,
        observed_on: impl Into<String>,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            candidate_family,
            pricing,
            dispatcher,
            binding,
            observed_on: observed_on.into(),
            now_unix: 0,
        }
    }

    /// Set the clock reading used for `Retry-After` HTTP-date arithmetic.
    #[must_use]
    pub fn with_now_unix(mut self, now_unix: i64) -> Self {
        self.now_unix = now_unix;
        self
    }

    /// Run the test.
    ///
    /// # Errors
    ///
    /// Returns the typed [`SttError`] for every refusal and failure. A family
    /// with no request builder is refused before anything is sent, so a model
    /// the catalogue merely called "audio capable" never causes an upload.
    pub async fn run(&self, ctx: JobContext) -> Result<SmokeTestOutcome, SttError> {
        if !self.candidate_family.has_builder() {
            return Err(SttError::Unavailable);
        }
        let format = smoke_clip_format();
        let accepted = [format.clone()];

        let body = match self.candidate_family {
            RequestFamily::MultipartStt => MultipartSttRequest::build(
                RequestFamily::MultipartStt,
                &self.model_id,
                &format,
                &accepted,
                None,
                SMOKE_CLIP.to_vec(),
                None,
            )
            .map_err(|_| SttError::UnsupportedFormat)?
            .encode(&multipart_boundary("capability")),
            _ => AudioChatRequest::build(
                RequestFamily::AudioChat,
                &self.model_id,
                "Transcribe the attached audio verbatim. Return only the transcript text.",
                &format,
                &accepted,
                None,
                SMOKE_CLIP.to_vec(),
            )
            .map_err(|_| SttError::UnsupportedFormat)?
            .encode(),
        };

        let path = if self.candidate_family == RequestFamily::MultipartStt {
            crate::TRANSCRIPTIONS_PATH
        } else {
            crate::CHAT_COMPLETIONS_PATH
        };

        let intent = AttemptIntent {
            job_id: self.binding.job_id.clone(),
            content_sha256: Sha256Digest::of(SMOKE_CLIP).to_hex(),
            request_family: self.candidate_family,
            relay: plaudon_security::consent::RELAY_OPENROUTER.to_owned(),
            model_id: self.model_id.clone(),
            route_label: self.binding.route_label.clone(),
            privacy_snapshot: self.binding.privacy_snapshot.clone(),
            consent_id: self.binding.consent_id.clone(),
            estimate: CostEstimate::unknown(EstimateGap::AudioTokenRateUnobserved),
        };

        let success = self
            .dispatcher
            .dispatch(
                path,
                ContentType::AudioBytes,
                &body,
                &intent,
                &ctx,
                self.now_unix,
            )
            .await
            .map_err(SttError::from)?;

        let mut record = ModelProbeRecord {
            model_id: self.model_id.clone(),
            request_family: self.candidate_family,
            evidence: ProbeEvidence::ObservedExchange,
            accepted_formats: Known::Known(vec![format.container.clone()]),
            // The clip proves only that *this* size was accepted; the real
            // ceiling is still unknown and is never guessed upward.
            max_input_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            timestamps: TimestampSupport::ObservedNone,
            diarization: Tri::Unknown,
            language_detection: Tri::Unknown,
            provider_route: Known::Unknown,
            retention_control: Known::Unknown,
            audio_input_price: self.pricing.audio_input_per_token.clone(),
            pricing_unit: match self.pricing.unit_label() {
                Some(label) => Known::Known(label.to_owned()),
                None => Known::Unknown,
            },
            observed_on: self.observed_on.clone(),
            audio_tokens_per_second: Known::Unknown,
            // An audio exchange demonstrates nothing about structured text
            // output or a text output budget.
            structured_output: Tri::Unknown,
            max_output_tokens: Known::Unknown,
            notes: String::new(),
        };

        let (usage, provider) = if self.candidate_family == RequestFamily::MultipartStt {
            let decoded: TranscriptionResponse =
                serde_json::from_slice(&success.body).map_err(|_| SttError::MalformedOutput)?;
            record.timestamps = if decoded.has_word_offsets() {
                TimestampSupport::ObservedWords
            } else if decoded.has_native_offsets() {
                TimestampSupport::ObservedSegments
            } else {
                TimestampSupport::ObservedNone
            };
            record.diarization = Tri::from_observation(Some(decoded.has_speaker_labels()));
            record.language_detection = Tri::from_observation(Some(decoded.language.is_some()));
            (decoded.usage.clone(), None)
        } else {
            let decoded: ChatCompletionResponse =
                serde_json::from_slice(&success.body).map_err(|_| SttError::MalformedOutput)?;
            if decoded.first_content().is_none() {
                return Err(SttError::MalformedOutput);
            }
            // A chat model returns prose, so it demonstrates no timing,
            // no diarization and no language field at all.
            record.timestamps = TimestampSupport::ObservedNone;
            record.diarization = Tri::No;
            record.language_detection = Tri::No;
            (decoded.usage.clone(), decoded.provider.clone())
        };

        // A reported token count plus a reported audio duration is the only
        // thing that makes a future audio estimate calculable (spec 10.4).
        if let Some(rate) = usage
            .as_ref()
            .and_then(crate::pricing::observed_audio_token_rate)
        {
            record.audio_tokens_per_second = Known::Known(rate);
        }

        // The route is what the consent grant was taken against; the response
        // may also name the downstream provider, which is stronger evidence.
        record.provider_route = match provider.or_else(|| self.binding.route_label.clone()) {
            Some(route) => Known::Known(route),
            None => Known::Unknown,
        };

        let cost = self
            .dispatcher
            .settle_attempt(
                &success.attempt_id,
                success.provider_request_id.clone(),
                &self.pricing,
                usage.as_ref(),
            )
            .await;

        Ok(SmokeTestOutcome {
            record,
            cost,
            provider_request_id: success.provider_request_id,
        })
    }
}

// ---------------------------------------------------------------------------
// Text generation
// ---------------------------------------------------------------------------

/// Output budget the text capability check declares.
///
/// A transcript tool refuses a model whose observed output budget is below
/// `plaudon_ai_tools::tool::RequiredCapabilities::min_output_tokens` (1 024 in
/// the shipped tools), so a check that asked for less would pass and still
/// leave every tool refused. The figure is what the request declares and what
/// a successful exchange therefore demonstrates — never a ceiling read off a
/// catalogue.
pub const TEXT_PROBE_OUTPUT_TOKENS: u64 = 1_024;

/// Name the structured-output schema is sent under.
pub const TEXT_PROBE_SCHEMA_VERSION: &str = "capability_check_v1";

/// Instruction channel of the check. Fixed text, no user content (§9.6).
pub const TEXT_PROBE_INSTRUCTION: &str = concat!(
    "You answer with one JSON object and nothing else. ",
    "The object must satisfy the supplied JSON schema exactly."
);

/// The one short prompt the check sends. Deliberately tiny: the exchange
/// exists to observe a wire contract, and the user pays for every token.
pub const TEXT_PROBE_PROMPT: &str =
    "Reply with {\"ok\": true} and the number of items you were asked for, which is 1.";

/// The schema the check asks the provider to enforce.
///
/// Small enough to cost nothing and strict enough that prose cannot satisfy
/// it: `additionalProperties: false` plus two required fields means a model
/// that ignored the schema fails validation rather than passing by accident.
#[must_use]
pub fn text_probe_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok", "items"],
        "properties": {
            "ok": { "type": "boolean" },
            "items": { "type": "integer", "minimum": 0 }
        }
    })
}

/// Whether a document satisfies [`text_probe_schema`].
///
/// Validated locally in every case: a provider's claim that it enforced the
/// schema is not evidence that it did (spec 9.5).
#[must_use]
pub fn text_probe_body_is_valid(body: &serde_json::Value) -> bool {
    jsonschema::validator_for(&text_probe_schema())
        .is_ok_and(|validator| validator.is_valid(body))
}

/// The pre-dispatch estimate of one text capability check.
///
/// Exposed so the caller's budget decision is taken against the same prompt
/// and the same output budget the check actually sends.
#[must_use]
pub fn text_probe_estimate(units: &PricingUnits) -> CostEstimate {
    crate::pricing::estimate(
        units,
        crate::pricing::EstimateInput::Text {
            prompt_tokens: crate::client::approximate_tokens(TEXT_PROBE_PROMPT),
            max_output_tokens: TEXT_PROBE_OUTPUT_TOKENS,
        },
    )
}

/// What one text capability check established.
#[derive(Debug, Clone)]
pub struct TextCheckOutcome {
    /// The record to merge into the capability registry. It is evidence
    /// either way: a provider that answered with prose is recorded as having
    /// been observed not to produce schema-valid output.
    pub record: ModelProbeRecord,
    /// What the exchange cost. Charged whether or not the body validated.
    pub cost: SettledCost,
    /// Vendor request id, when returned.
    pub provider_request_id: Option<String>,
}

impl TextCheckOutcome {
    /// Whether the exchange demonstrated structured output.
    #[must_use]
    pub const fn verified(&self) -> bool {
        self.record.structured_output.satisfies()
    }
}

/// Runs one text-generation capability check against one model.
///
/// The text counterpart of [`CapabilitySmokeTest`]: one real chargeable
/// `chat/completions` exchange, through the same dispatcher, the same consent
/// approval and the same ledger row, recording what it demonstrated —
/// that the provider accepted a schema-constrained request, that the body it
/// returned satisfies that schema, which route served it, and the output
/// budget the exchange answered within.
#[derive(Debug, Clone)]
pub struct TextCapabilityCheck {
    model_id: String,
    pricing: PricingUnits,
    dispatcher: CloudDispatcher,
    binding: JobBinding,
    observed_on: String,
    now_unix: i64,
}

impl TextCapabilityCheck {
    /// Prepare a check for `model_id`.
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        pricing: PricingUnits,
        dispatcher: CloudDispatcher,
        binding: JobBinding,
        observed_on: impl Into<String>,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            pricing,
            dispatcher,
            binding,
            observed_on: observed_on.into(),
            now_unix: 0,
        }
    }

    /// Set the clock reading used for `Retry-After` HTTP-date arithmetic.
    #[must_use]
    pub fn with_now_unix(mut self, now_unix: i64) -> Self {
        self.now_unix = now_unix;
        self
    }

    /// Run the check.
    ///
    /// An exchange that completed is returned as `Ok` even when the body did
    /// not satisfy the schema: that is an observation about the model, and
    /// the caller records it and refuses the verification. Only a failure to
    /// obtain a response at all is an error.
    ///
    /// # Errors
    ///
    /// Returns the typed [`TextModelError`] for every refusal and transport
    /// failure, including [`TextModelError::OutcomeUnknown`] when bytes were
    /// transmitted and no answer arrived.
    pub async fn run(&self, ctx: JobContext) -> Result<TextCheckOutcome, TextModelError> {
        let body = TextGenerationRequest::build(
            &self.model_id,
            TEXT_PROBE_INSTRUCTION,
            TEXT_PROBE_PROMPT,
            TEXT_PROBE_SCHEMA_VERSION,
            text_probe_schema(),
            // Schema enforcement is the thing under test, so it is requested
            // rather than withheld until it has been observed.
            true,
            u32::try_from(TEXT_PROBE_OUTPUT_TOKENS).ok(),
        )
        .map_err(|_| TextModelError::EngineFailure)?
        .encode();

        let intent = AttemptIntent {
            job_id: self.binding.job_id.clone(),
            content_sha256: Sha256Digest::of(TEXT_PROBE_PROMPT.as_bytes()).to_hex(),
            // Text generation has no audio request family (App. A.3).
            request_family: RequestFamily::Unsupported,
            relay: plaudon_security::consent::RELAY_OPENROUTER.to_owned(),
            model_id: self.model_id.clone(),
            route_label: self.binding.route_label.clone(),
            privacy_snapshot: self.binding.privacy_snapshot.clone(),
            consent_id: self.binding.consent_id.clone(),
            estimate: text_probe_estimate(&self.pricing),
        };

        let success = self
            .dispatcher
            .dispatch(
                crate::CHAT_COMPLETIONS_PATH,
                ContentType::TranscriptText,
                &body,
                &intent,
                &ctx,
                self.now_unix,
            )
            .await
            .map_err(TextModelError::from)?;

        let Ok(decoded) = serde_json::from_slice::<ChatCompletionResponse>(&success.body) else {
            // The envelope itself was unreadable, so nothing was observed
            // about the model. The row is still settled: the provider
            // answered, and an unsettled row would reconcile to "outcome
            // unknown" and park a job that is not in doubt.
            self.dispatcher
                .settle_attempt(
                    &success.attempt_id,
                    success.provider_request_id.clone(),
                    &self.pricing,
                    None,
                )
                .await;
            return Err(TextModelError::EngineFailure);
        };

        let schema_valid = decoded
            .first_content()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw.trim()).ok())
            .is_some_and(|document| text_probe_body_is_valid(&document));

        let cost = self
            .dispatcher
            .settle_attempt(
                &success.attempt_id,
                success.provider_request_id.clone(),
                &self.pricing,
                decoded.usage.as_ref(),
            )
            .await;

        let record = ModelProbeRecord {
            model_id: self.model_id.clone(),
            request_family: RequestFamily::Unsupported,
            evidence: ProbeEvidence::ObservedExchange,
            // Everything below belongs to the audio contract and this
            // exchange carried no audio at all.
            accepted_formats: Known::Unknown,
            max_input_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            timestamps: TimestampSupport::NotObserved,
            diarization: Tri::Unknown,
            language_detection: Tri::Unknown,
            provider_route: match decoded
                .provider
                .clone()
                .or_else(|| self.binding.route_label.clone())
            {
                Some(route) => Known::Known(route),
                None => Known::Unknown,
            },
            retention_control: Known::Unknown,
            audio_input_price: Known::Unknown,
            pricing_unit: Known::Unknown,
            audio_tokens_per_second: Known::Unknown,
            structured_output: Tri::from_observation(Some(schema_valid)),
            // The budget is only demonstrated by an exchange that answered
            // within it; a refusal to produce the schema demonstrates nothing.
            max_output_tokens: if schema_valid {
                Known::Known(TEXT_PROBE_OUTPUT_TOKENS)
            } else {
                Known::Unknown
            },
            observed_on: self.observed_on.clone(),
            notes: String::new(),
        };

        Ok(TextCheckOutcome {
            record,
            cost,
            provider_request_id: success.provider_request_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_clip_is_a_small_valid_wav() {
        assert_eq!(&SMOKE_CLIP[0..4], b"RIFF");
        assert_eq!(&SMOKE_CLIP[8..12], b"WAVE");
        assert!(
            SMOKE_CLIP.len() < 64 * 1024,
            "the user pays for every byte of the capability check"
        );
    }

    #[test]
    fn a_family_without_a_builder_cannot_be_smoke_tested() {
        // Guarded before any request is constructed; see `run`.
        for family in [RequestFamily::Unknown, RequestFamily::Unsupported] {
            assert!(!family.has_builder(), "{family:?}");
        }
    }

    // -----------------------------------------------------------------
    // Text capability check
    // -----------------------------------------------------------------

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use plaudon_domain::ids::JobId;
    use plaudon_security::consent::{
        dispatch_preflight, ConsentGrant, ConsentPurpose, DispatchApproval, DispatchRequest,
        ProviderRoute, RELAY_OPENROUTER,
    };
    use plaudon_security::outbound::http::{GatedHttpClientFactory, GatedRequest, HttpResponse};
    use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};

    use crate::client::DispatchRefused;
    use crate::credential::StaticApiKey;
    use crate::ledger::RecordingLedger;
    use crate::transport::{AsyncHttpTransport, TransportFailure};

    const MODEL: &str = "vendor/text-a";
    const NOW: i64 = 1_793_000_000;
    const DISCLOSURE: &str = "2026-10-11";

    /// A transport that replays one response and records every request.
    #[derive(Debug)]
    struct ScriptedTransport {
        reply: Mutex<Vec<Result<HttpResponse, TransportFailure>>>,
        sent: Mutex<usize>,
    }

    impl ScriptedTransport {
        fn new(reply: Result<HttpResponse, TransportFailure>) -> Arc<Self> {
            Arc::new(Self {
                reply: Mutex::new(vec![reply]),
                sent: Mutex::new(0),
            })
        }

        fn json(body: &serde_json::Value) -> Arc<Self> {
            Self::new(Ok(HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: serde_json::to_vec(body).expect("fixture body"),
            }))
        }

        fn attempts(&self) -> usize {
            *self.sent.lock().expect("transport lock")
        }
    }

    #[async_trait]
    impl AsyncHttpTransport for ScriptedTransport {
        async fn send(&self, _request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
            *self.sent.lock().expect("transport lock") += 1;
            let mut queue = self.reply.lock().expect("transport lock");
            if queue.is_empty() {
                return Err(TransportFailure::ConnectFailed {
                    detail: "queue exhausted".to_owned(),
                });
            }
            queue.remove(0)
        }
    }

    /// The real consent preflight over one grant.
    #[derive(Debug)]
    struct Authority {
        policy: Arc<OutboundPolicy>,
        grants: Vec<ConsentGrant>,
    }

    impl Authority {
        fn allowing() -> Arc<Self> {
            Arc::new(Self {
                policy: Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
                    ApprovalSource::TestHarness,
                    NOW - 120,
                    DISCLOSURE,
                ))),
                grants: vec![ConsentGrant::for_model_policy(
                    "consent-1",
                    ProviderRoute::new(RELAY_OPENROUTER, MODEL),
                    ConsentPurpose::Summarization,
                    DISCLOSURE,
                    NOW - 60,
                )
                .with_content_type(ContentType::TranscriptText)],
            })
        }
    }

    #[async_trait]
    impl crate::client::CloudDispatchAuthority for Authority {
        async fn authorize(
            &self,
            content_type: ContentType,
            model_id: &str,
        ) -> Result<DispatchApproval, DispatchRefused> {
            let request = DispatchRequest::new(
                ConsentPurpose::Summarization,
                content_type,
                ProviderRoute::new(RELAY_OPENROUTER, model_id),
                "capability-check",
                DISCLOSURE,
            );
            dispatch_preflight(&self.policy, &self.grants, &request, NOW).map_err(|error| {
                use plaudon_security::consent::ConsentError;
                match error {
                    ConsentError::Egress(_) => DispatchRefused::Egress,
                    ConsentError::PrivacyPostureUnresolved { .. } => {
                        DispatchRefused::PrivacyUnresolved
                    }
                    _ => DispatchRefused::ConsentRequired,
                }
            })
        }
    }

    fn check(transport: Arc<ScriptedTransport>, ledger: Arc<RecordingLedger>) -> TextCapabilityCheck
    {
        let authority = Authority::allowing();
        let dispatcher = CloudDispatcher::new(
            GatedHttpClientFactory::new(Arc::clone(&authority.policy)),
            transport,
            Arc::new(StaticApiKey::new("sk-or-v1-unit-test")),
            ledger,
            authority,
        );
        TextCapabilityCheck::new(
            MODEL,
            PricingUnits::unknown(),
            dispatcher,
            JobBinding {
                job_id: JobId::new().to_string(),
                recording_id: "capability-check".to_owned(),
                consent_id: "consent-1".to_owned(),
                privacy_snapshot: "{}".to_owned(),
                route_label: Some("OpenAI".to_owned()),
            },
            "2026-10-12",
        )
        .with_now_unix(NOW)
    }

    fn chat_reply(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "gen-1",
            "provider": "OpenAI",
            "choices": [{ "message": { "role": "assistant", "content": content } }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 7, "total_tokens": 27 }
        })
    }

    #[test]
    fn the_declared_budget_satisfies_what_a_transcript_tool_requires() {
        // The shipped tools require 1 024 output tokens; a check that asked
        // for less would verify a model every tool still refuses.
        const { assert!(TEXT_PROBE_OUTPUT_TOKENS >= 1_024) };
        assert!(TEXT_PROBE_PROMPT.len() < 512);
    }

    #[test]
    fn the_probe_schema_rejects_prose_and_accepts_the_asked_for_document() {
        assert!(text_probe_body_is_valid(
            &serde_json::json!({ "ok": true, "items": 1 })
        ));
        assert!(!text_probe_body_is_valid(&serde_json::json!(
            "I cannot help with that."
        )));
        assert!(!text_probe_body_is_valid(
            &serde_json::json!({ "ok": true, "items": 1, "extra": 2 })
        ));
        assert!(!text_probe_body_is_valid(&serde_json::json!({ "ok": true })));
    }

    #[tokio::test]
    async fn a_schema_valid_answer_is_recorded_with_its_route_and_output_budget() {
        let transport = ScriptedTransport::json(&chat_reply("{\"ok\":true,\"items\":1}"));
        let ledger = Arc::new(RecordingLedger::new());
        let outcome = check(Arc::clone(&transport), Arc::clone(&ledger))
            .run(JobContext::new(JobId::new()))
            .await
            .expect("the exchange completed");

        assert!(outcome.verified());
        assert_eq!(outcome.record.structured_output, Tri::Yes);
        assert_eq!(
            outcome.record.max_output_tokens,
            Known::Known(TEXT_PROBE_OUTPUT_TOKENS)
        );
        assert_eq!(
            outcome.record.provider_route,
            Known::Known("OpenAI".to_owned())
        );
        assert_eq!(outcome.record.evidence, ProbeEvidence::ObservedExchange);
        // The audio contract is untouched by a text exchange.
        assert_eq!(outcome.record.request_family, RequestFamily::Unsupported);
        assert_eq!(outcome.record.timestamps, TimestampSupport::NotObserved);
        assert_eq!(transport.attempts(), 1);
        // The row was opened before the send and settled after it.
        assert_eq!(ledger.intents().len(), 1);
        assert!(ledger
            .states()
            .contains(&plaudon_domain::capability::DispatchState::Completed));
    }

    #[tokio::test]
    async fn prose_is_recorded_as_an_observed_negative_and_never_verifies() {
        let transport = ScriptedTransport::json(&chat_reply("Sure! Here is your answer."));
        let outcome = check(transport, Arc::new(RecordingLedger::new()))
            .run(JobContext::new(JobId::new()))
            .await
            .expect("the exchange completed");

        assert!(!outcome.verified());
        assert_eq!(outcome.record.structured_output, Tri::No);
        assert_eq!(outcome.record.max_output_tokens, Known::Unknown);
    }

    #[tokio::test]
    async fn a_document_that_misses_the_schema_does_not_verify() {
        let transport = ScriptedTransport::json(&chat_reply("{\"ok\":\"yes\"}"));
        let outcome = check(transport, Arc::new(RecordingLedger::new()))
            .run(JobContext::new(JobId::new()))
            .await
            .expect("the exchange completed");
        assert_eq!(outcome.record.structured_output, Tri::No);
    }

    #[tokio::test]
    async fn a_transmission_with_no_answer_is_an_unknown_outcome() {
        let transport = ScriptedTransport::new(Err(TransportFailure::TimedOutAfterSend));
        let error = check(transport, Arc::new(RecordingLedger::new()))
            .run(JobContext::new(JobId::new()))
            .await
            .expect_err("a transmitted request with no answer is not a result");
        assert_eq!(error, TextModelError::OutcomeUnknown);
    }

    #[tokio::test]
    async fn a_cancelled_check_sends_nothing() {
        let transport = ScriptedTransport::json(&chat_reply("{\"ok\":true,\"items\":1}"));
        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();
        let error = check(Arc::clone(&transport), Arc::new(RecordingLedger::new()))
            .run(ctx)
            .await
            .expect_err("a cancelled check does not dispatch");
        assert_eq!(error, TextModelError::Unavailable);
        assert_eq!(transport.attempts(), 0);
    }
}
