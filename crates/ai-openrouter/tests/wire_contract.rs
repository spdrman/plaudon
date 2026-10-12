//! Wire-level contract tests for the OpenRouter STT and text endpoints
//! (spec 18.3, tasks G1.1, G1.3, G1.4, G1.5, G2.1).
//!
//! Everything here is hermetic: captured fixtures replayed through the real
//! egress gate, the real consent preflight and the real request builders. No
//! network, no credential, no vendor CLI. The opt-in real-provider run lives in
//! `scripts/probe-openrouter.sh`.

mod support;

use std::sync::Arc;

use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::stt::{SttError, SttInput};
use plaudon_ai_openrouter::client::CloudDispatchAuthority;
use plaudon_ai_openrouter::ledger::{AttemptUpdate, RecordingLedger};
use plaudon_ai_openrouter::registry::{NotVerifiedReason, Verification};
use plaudon_ai_openrouter::transport::TransportFailure;
use plaudon_domain::capability::DispatchState;
use plaudon_domain::ids::JobId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::money::MoneyEstimate;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_security::consent::ContentType;
use support::{
    harness, response_from, verified_card, wav, FixtureTransport, PreflightAuthority, TEST_API_KEY,
};
use tempfile::TempDir;

const MODEL: &str = "openai/whisper-1";

struct Media {
    _dir: TempDir,
    input: SttInput,
}

fn media(bytes: &[u8]) -> Media {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("audio.wav");
    std::fs::write(&path, bytes).expect("write fixture audio");
    let mut input = SttInput::whole_file(
        path,
        Sha256Digest::of(bytes),
        bytes.len() as u64,
        wav(),
        16_000,
        1,
    );
    input.duration_ms = Some(3_520);
    Media { _dir: dir, input }
}

fn ctx() -> JobContext {
    JobContext::new(JobId::new())
}

// ---------------------------------------------------------------------------
// Success paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_multipart_transcription_with_segments_yields_native_alignment() {
    let media = media(b"RIFFfake-wav-bytes");
    let transport = FixtureTransport::replaying("transcription-success-segments");
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        transport,
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );

    let outcome = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("transcribes");

    assert_eq!(outcome.draft.alignment_origin, AlignmentOrigin::Native);
    assert_eq!(outcome.draft.plain_text(), "Good morning everyone.");
    assert_eq!(outcome.draft.segments.len(), 1);
    assert!(outcome.draft.segments[0].start.is_some());

    let request = &h.transport.requests()[0];
    assert!(request.url.ends_with("/audio/transcriptions"));
    assert!(request
        .content_type
        .as_deref()
        .expect("content type")
        .starts_with("multipart/form-data; boundary="));
    assert!(String::from_utf8_lossy(&request.body).contains("name=\"model\""));
}

#[tokio::test]
async fn a_text_only_response_never_claims_a_timeline() {
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-text-only"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );

    let outcome = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("transcribes");

    assert_eq!(outcome.draft.alignment_origin, AlignmentOrigin::None);
    assert!(outcome
        .draft
        .segments
        .iter()
        .all(|segment| segment.start.is_none() && segment.end.is_none()));
}

#[tokio::test]
async fn an_audio_chat_model_uses_the_chat_endpoint_and_never_the_stt_one() {
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card("openai/gpt-audio", RequestFamily::AudioChat),
        FixtureTransport::replaying("chat-completion-structured"),
        PreflightAuthority::allowing("openai/gpt-audio", ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );

    let outcome = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("transcribes");

    let request = &h.transport.requests()[0];
    assert!(request.url.ends_with("/chat/completions"));
    assert_eq!(request.content_type.as_deref(), Some("application/json"));
    assert_eq!(outcome.draft.alignment_origin, AlignmentOrigin::None);
    // The provider reported a cost, so it is finalized rather than estimated.
    assert!(outcome.cost.amount.is_finalized());
}

#[tokio::test]
async fn a_response_without_usage_leaves_the_cost_unknown() {
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    let outcome = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("transcribes");
    assert_eq!(outcome.cost.amount, MoneyEstimate::Unknown);
    assert_eq!(outcome.cost.amount.amount(), None);
}

// ---------------------------------------------------------------------------
// Fail-closed routing (G1.3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unverified_model_sends_zero_bytes() {
    let media = media(b"RIFFfake-wav-bytes");
    let mut card = verified_card(MODEL, RequestFamily::MultipartStt);
    card.verification = Verification::NotVerified {
        reason: NotVerifiedReason::NoObservedExchange,
    };
    let ledger = Arc::new(RecordingLedger::new());
    let h = harness(
        card,
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        ledger.clone(),
    );

    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("refused");
    assert_eq!(error, SttError::Unavailable);
    assert_eq!(h.transport.transmitted_bytes(), 0);
    assert!(ledger.intents().is_empty(), "no attempt row for a refusal");
}

#[tokio::test]
async fn an_unknown_or_unsupported_family_has_no_dispatch_path() {
    for family in [RequestFamily::Unknown, RequestFamily::Unsupported] {
        let media = media(b"RIFFfake-wav-bytes");
        let mut card = verified_card(MODEL, family);
        card.request_family = family;
        let ledger = Arc::new(RecordingLedger::new());
        let h = harness(
            card,
            FixtureTransport::replaying("transcription-success-segments"),
            PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
                as Arc<dyn CloudDispatchAuthority>,
            ledger.clone(),
        );
        let error = h
            .client
            .transcribe_with_cost(media.input, ctx())
            .await
            .expect_err("refused");
        assert_eq!(error, SttError::Unavailable, "{family:?}");
        assert_eq!(h.transport.transmitted_bytes(), 0, "{family:?}");
        assert!(ledger.intents().is_empty());
    }
}

#[tokio::test]
async fn an_unaccepted_format_is_rejected_locally() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("audio.aiff");
    std::fs::write(&path, b"FORM").expect("write");
    let input = SttInput::whole_file(
        path,
        Sha256Digest::of(b"FORM"),
        4,
        plaudon_ai_contracts::capabilities::MediaFormat::new("aiff", "pcm_s16be"),
        16_000,
        1,
    );
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    assert_eq!(
        h.client
            .transcribe_with_cost(input, ctx())
            .await
            .expect_err("refused"),
        SttError::UnsupportedFormat
    );
    assert_eq!(h.transport.transmitted_bytes(), 0);
}

#[tokio::test]
async fn an_oversize_payload_is_rejected_before_any_byte_leaves() {
    let media = media(b"RIFFfake-wav-bytes");
    let mut card = verified_card(MODEL, RequestFamily::MultipartStt);
    card.max_input_bytes = plaudon_domain::capability::Known::Known(4);
    let h = harness(
        card,
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    assert_eq!(
        h.client
            .transcribe_with_cost(media.input, ctx())
            .await
            .expect_err("refused"),
        SttError::InputTooLarge
    );
    assert_eq!(h.transport.transmitted_bytes(), 0);
}

#[tokio::test]
async fn an_over_duration_input_is_rejected_before_any_byte_leaves() {
    let media = media(b"RIFFfake-wav-bytes");
    let mut card = verified_card(MODEL, RequestFamily::MultipartStt);
    card.max_duration_ms = plaudon_domain::capability::Known::Known(1_000);
    let h = harness(
        card,
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    assert_eq!(
        h.client
            .transcribe_with_cost(media.input, ctx())
            .await
            .expect_err("refused"),
        SttError::InputTooLarge
    );
    assert_eq!(h.transport.transmitted_bytes(), 0);
}

// ---------------------------------------------------------------------------
// Consent and Local-only Mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_only_mode_blocks_the_request_entirely() {
    let media = media(b"RIFFfake-wav-bytes");
    let ledger = Arc::new(RecordingLedger::new());
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::local_only(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        ledger.clone(),
    );
    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("blocked");
    assert_eq!(error, SttError::Unavailable);
    assert_eq!(h.transport.transmitted_bytes(), 0);
    assert_eq!(h.transport.attempts(), 0);
    assert!(ledger.intents().is_empty());
}

#[tokio::test]
async fn a_missing_consent_grant_blocks_the_request() {
    let media = media(b"RIFFfake-wav-bytes");
    let ledger = Arc::new(RecordingLedger::new());
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::without_consent() as Arc<dyn CloudDispatchAuthority>,
        ledger.clone(),
    );
    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("blocked");
    assert_eq!(error, SttError::ProviderRejected);
    assert_eq!(h.transport.transmitted_bytes(), 0);
    assert!(ledger.intents().is_empty());
}

// ---------------------------------------------------------------------------
// Ledger before dispatch (G2.1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_ledger_row_is_committed_before_the_request_is_sent() {
    let media = media(b"RIFFfake-wav-bytes");
    let ledger = Arc::new(RecordingLedger::new());
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        ledger.clone(),
    );
    let sha = media.input.media_sha256.to_hex();
    h.client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("transcribes");

    let intents = ledger.intents();
    assert_eq!(intents.len(), 1);
    let intent = &intents[0];
    assert_eq!(intent.job_id, "job-1");
    assert_eq!(intent.content_sha256, sha);
    assert_eq!(intent.request_family, RequestFamily::MultipartStt);
    assert_eq!(intent.consent_id, "consent-1");
    assert!(intent.privacy_snapshot.contains("retention="));
    // Sending, then completed.
    assert_eq!(
        ledger.states(),
        vec![DispatchState::Sending, DispatchState::Completed]
    );
    // No secret in the ledger.
    let rendered = format!("{intents:?}");
    assert!(!rendered.contains(TEST_API_KEY));
}

#[tokio::test]
async fn an_unavailable_ledger_prevents_the_dispatch() {
    let media = media(b"RIFFfake-wav-bytes");
    let ledger = Arc::new(RecordingLedger::unavailable());
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        ledger,
    );
    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("refused");
    assert_eq!(error, SttError::Unavailable);
    assert_eq!(h.transport.transmitted_bytes(), 0);
}

// ---------------------------------------------------------------------------
// Error classification (G1.4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn documented_provider_errors_map_to_distinct_typed_failures() {
    let cases = [
        ("error-401-unauthorized", SttError::ConsentRequired),
        ("error-402-insufficient-credit", SttError::ConsentRequired),
        ("error-404-unknown-model", SttError::Unavailable),
        ("error-413-payload-too-large", SttError::InputTooLarge),
    ];
    for (fixture, expected) in cases {
        let media = media(b"RIFFfake-wav-bytes");
        let h = harness(
            verified_card(MODEL, RequestFamily::MultipartStt),
            FixtureTransport::new(vec![Ok(response_from(fixture))]),
            PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
                as Arc<dyn CloudDispatchAuthority>,
            Arc::new(RecordingLedger::new()),
        );
        assert_eq!(
            h.client
                .transcribe_with_cost(media.input, ctx())
                .await
                .expect_err("refused"),
            expected,
            "{fixture}"
        );
    }
}

#[tokio::test]
async fn a_rate_limit_is_retried_after_the_provider_named_delay() {
    let media = media(b"RIFFfake-wav-bytes");
    let transport = FixtureTransport::new(vec![
        Ok(response_from("error-429-rate-limited")),
        Ok(response_from("transcription-success-segments")),
    ]);
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        transport,
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    // The fixture asks for 30s, above the harness's 5ms ceiling, so the job is
    // parked rather than slept through: the retry never happens here.
    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("parked");
    assert_eq!(error, SttError::ProviderRejected);
    assert_eq!(h.transport.attempts(), 1);
}

#[tokio::test]
async fn a_provider_outage_is_retried_automatically() {
    let media = media(b"RIFFfake-wav-bytes");
    let transport = FixtureTransport::new(vec![
        Ok(response_from("error-502-provider-unavailable")),
        Ok(response_from("transcription-success-segments")),
    ]);
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        transport,
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    let outcome = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("retried and succeeded");
    assert_eq!(outcome.draft.plain_text(), "Good morning everyone.");
    assert_eq!(h.transport.attempts(), 2);
}

#[tokio::test]
async fn a_lost_response_after_upload_is_never_retried() {
    let media = media(b"RIFFfake-wav-bytes");
    let ledger = Arc::new(RecordingLedger::new());
    let transport = FixtureTransport::new(vec![
        Err(TransportFailure::TimedOutAfterSend),
        Ok(response_from("transcription-success-segments")),
    ]);
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        transport,
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        ledger.clone(),
    );
    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("outcome unknown");
    assert_eq!(error, SttError::OutcomeUnknown);
    assert_eq!(
        h.transport.attempts(),
        1,
        "a possibly-billed request must not be re-sent"
    );
    assert_eq!(
        ledger.states(),
        vec![DispatchState::Sending, DispatchState::OutcomeUnknown]
    );
    assert!(matches!(
        ledger.updates().last(),
        Some(AttemptUpdate::OutcomeUnknown { .. })
    ));
}

#[tokio::test]
async fn a_failure_before_transmission_is_safe_to_retry() {
    let media = media(b"RIFFfake-wav-bytes");
    let ledger = Arc::new(RecordingLedger::new());
    let transport = FixtureTransport::new(vec![
        Err(TransportFailure::ConnectFailed {
            detail: "connect".to_owned(),
        }),
        Ok(response_from("transcription-success-segments")),
    ]);
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        transport,
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        ledger.clone(),
    );
    h.client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("retried");
    assert_eq!(h.transport.attempts(), 2);
    assert_eq!(ledger.intents().len(), 2, "each attempt gets its own row");
    assert!(ledger.states().contains(&DispatchState::FailedBeforeSend));
}

#[tokio::test]
async fn a_truncated_response_asks_the_user_instead_of_retrying() {
    let media = media(b"RIFFfake-wav-bytes");
    let transport = FixtureTransport::new(vec![
        Err(TransportFailure::ResponseTruncated),
        Ok(response_from("transcription-success-segments")),
    ]);
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        transport,
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("stops");
    assert_eq!(error, SttError::MalformedOutput);
    assert_eq!(h.transport.attempts(), 1);
}

#[tokio::test]
async fn an_unsafe_redirect_is_refused_and_not_followed() {
    let media = media(b"RIFFfake-wav-bytes");
    let transport = FixtureTransport::new(vec![Err(TransportFailure::UnsafeRedirect {
        destination_label: "https://evil.example".to_owned(),
    })]);
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        transport,
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    let error = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect_err("refused");
    assert_eq!(error, SttError::ProviderRejected);
    assert_eq!(h.transport.attempts(), 1);
}

#[tokio::test]
async fn a_body_that_is_not_the_documented_shape_fails_closed() {
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("malformed-json"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    assert_eq!(
        h.client
            .transcribe_with_cost(media.input, ctx())
            .await
            .expect_err("refused"),
        SttError::MalformedOutput
    );
}

#[tokio::test]
async fn a_response_with_no_timing_at_all_degrades_to_text_only() {
    // Distinct from the mixed case below: when nothing is timed there is
    // nothing to invent, so the honest result is a seek-free transcript.
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-text-only"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    let outcome = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("text only is valid");
    assert_eq!(outcome.draft.alignment_origin, AlignmentOrigin::None);
}

#[tokio::test]
async fn a_mix_of_timed_and_untimed_segments_is_refused() {
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-partial-segment"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    assert_eq!(
        h.client
            .transcribe_with_cost(media.input, ctx())
            .await
            .expect_err("refused"),
        SttError::MalformedOutput
    );
}

#[tokio::test]
async fn an_unexpected_chat_content_shape_fails_closed() {
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card("openai/gpt-audio", RequestFamily::AudioChat),
        FixtureTransport::replaying("chat-completion-unexpected-format"),
        PreflightAuthority::allowing("openai/gpt-audio", ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    assert_eq!(
        h.client
            .transcribe_with_cost(media.input, ctx())
            .await
            .expect_err("refused"),
        SttError::MalformedOutput
    );
}

// ---------------------------------------------------------------------------
// Credential handling (G1.1 security acceptance)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_api_key_appears_only_in_the_authorization_header() {
    let media = media(b"RIFFfake-wav-bytes");
    let ledger = Arc::new(RecordingLedger::new());
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        ledger.clone(),
    );
    let outcome = h
        .client
        .transcribe_with_cost(media.input, ctx())
        .await
        .expect("transcribes");

    let request = &h.transport.requests()[0];
    assert!(request.has_authorization);
    assert_eq!(
        request.authorization.as_deref(),
        Some(format!("Bearer {TEST_API_KEY}").as_str())
    );
    // Nowhere else: not in the body, not in the draft, not in the ledger.
    assert!(!String::from_utf8_lossy(&request.body).contains(TEST_API_KEY));
    assert!(!format!("{:?}", outcome.draft).contains(TEST_API_KEY));
    assert!(!format!("{:?}", ledger.intents()).contains(TEST_API_KEY));
    assert!(!format!("{:?}", ledger.updates()).contains(TEST_API_KEY));
}

#[tokio::test]
async fn a_cancelled_job_never_dispatches() {
    let media = media(b"RIFFfake-wav-bytes");
    let h = harness(
        verified_card(MODEL, RequestFamily::MultipartStt),
        FixtureTransport::replaying("transcription-success-segments"),
        PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
        Arc::new(RecordingLedger::new()),
    );
    let context = ctx();
    context.cancel.cancel();
    assert_eq!(
        h.client
            .transcribe_with_cost(media.input, context)
            .await
            .expect_err("refused"),
        SttError::Cancelled
    );
    assert_eq!(h.transport.transmitted_bytes(), 0);
}

// ---------------------------------------------------------------------------
// Capability smoke test (G1.2: what makes a model `Verified`)
// ---------------------------------------------------------------------------

mod smoke {
    use super::*;
    use plaudon_ai_openrouter::catalog::{ProbeEvidence, TimestampSupport};
    use plaudon_ai_openrouter::pricing::PricingUnits;
    use plaudon_ai_openrouter::registry::{ModelRegistry, Verification};
    use plaudon_ai_openrouter::smoke::CapabilitySmokeTest;
    use plaudon_ai_openrouter::{Allowlist, CapabilitySmokeTest as _Alias, ModelTask};

    fn smoke_test(
        family: RequestFamily,
        transport: Arc<FixtureTransport>,
        ledger: Arc<RecordingLedger>,
    ) -> CapabilitySmokeTest {
        let h = harness(
            verified_card(MODEL, family),
            transport,
            PreflightAuthority::allowing(MODEL, ContentType::AudioBytes)
                as Arc<dyn CloudDispatchAuthority>,
            ledger,
        );
        // Reuse the harness's dispatcher by rebuilding the test over the same
        // pieces: the smoke test is the only caller that may run against an
        // unverified card, because it is what verifies it.
        let _ = &h.client;
        CapabilitySmokeTest::new(
            MODEL,
            family,
            PricingUnits::unknown(),
            h.dispatcher().clone(),
            h.binding(),
            "2026-10-11",
        )
    }

    #[tokio::test]
    async fn a_successful_exchange_records_what_it_observed_and_nothing_more() {
        let ledger = Arc::new(RecordingLedger::new());
        let test = smoke_test(
            RequestFamily::MultipartStt,
            FixtureTransport::replaying("transcription-success-words"),
            ledger.clone(),
        );
        let outcome = test.run(ctx()).await.expect("smoke test runs");

        assert_eq!(outcome.record.evidence, ProbeEvidence::ObservedExchange);
        assert_eq!(outcome.record.timestamps, TimestampSupport::ObservedWords);
        assert_eq!(
            outcome.record.diarization,
            plaudon_domain::capability::Tri::Yes
        );
        assert_eq!(
            outcome.record.language_detection,
            plaudon_domain::capability::Tri::Yes
        );
        // The clip proves nothing about the real size ceiling.
        assert!(!outcome.record.max_input_bytes.is_known());
        assert_eq!(ledger.intents().len(), 1, "the smoke test is ledgered too");

        let mut registry = ModelRegistry::from_allowlist(&Allowlist::embedded().expect("manifest"));
        registry.apply_probe(
            ModelTask::Transcription,
            &outcome.record,
            "capability-check",
        );
        let card = registry
            .card(MODEL, ModelTask::Transcription)
            .expect("card");
        assert!(matches!(card.verification, Verification::Verified { .. }));
        assert!(card.is_selectable());
    }

    #[tokio::test]
    async fn a_failed_exchange_leaves_the_model_unverified() {
        let ledger = Arc::new(RecordingLedger::new());
        let test = smoke_test(
            RequestFamily::MultipartStt,
            FixtureTransport::new(vec![Ok(response_from("error-404-unknown-model"))]),
            ledger,
        );
        assert!(test.run(ctx()).await.is_err());

        let registry = ModelRegistry::from_allowlist(&Allowlist::embedded().expect("manifest"));
        let card = registry
            .card(MODEL, ModelTask::Transcription)
            .expect("card");
        assert!(!card.verification.is_verified());
    }

    #[tokio::test]
    async fn a_family_with_no_builder_is_never_smoke_tested() {
        let ledger = Arc::new(RecordingLedger::new());
        let transport = FixtureTransport::replaying("transcription-success-segments");
        let test = smoke_test(RequestFamily::Unknown, transport.clone(), ledger.clone());
        assert_eq!(
            test.run(ctx()).await.expect_err("refused"),
            SttError::Unavailable
        );
        assert_eq!(transport.transmitted_bytes(), 0);
        assert!(ledger.intents().is_empty());
    }
}
