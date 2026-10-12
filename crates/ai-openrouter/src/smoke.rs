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

use plaudon_ai_contracts::capabilities::{MediaFormat, RequestFamily};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::stt::SttError;
use plaudon_domain::capability::{Known, Tri};
use plaudon_domain::media::Sha256Digest;
use plaudon_security::consent::ContentType;

use crate::catalog::{ModelProbeRecord, ProbeEvidence, TimestampSupport};
use crate::client::{CloudDispatcher, JobBinding};
use crate::ledger::AttemptIntent;
use crate::pricing::{CostEstimate, EstimateGap, PricingUnits, SettledCost};
use crate::wire::{multipart_boundary, ChatCompletionResponse, TranscriptionResponse};
use crate::{AudioChatRequest, MultipartSttRequest};

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
}
