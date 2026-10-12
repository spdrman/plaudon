//! Apple native AI bridge, end to end across crates (spec §8.4, §9.4,
//! §11.3–§11.6, §18.2, §18.7(10), §22(4)).
//!
//! These tests prove the claims EPIC H makes in aggregate: the adapter in
//! `plaudon-ai-apple`, the engine policy and chunk planner in
//! `plaudon-app-core`, and the whisper.cpp fallback agree with one another,
//! and an unavailable Apple engine never turns into a cloud request.
//!
//! **Hermeticity.** Nothing here links an Apple framework (the `apple-bridge`
//! feature is off), reaches the network or talks to a provider. Every
//! scenario is built from [`FakeBridge`] scripted documents, so the same suite
//! runs on Windows and in CI. What a real Mac answers is the job of the
//! opt-in hardware suite; what the application does with each answer is
//! proven here.
//!
//! **Determinism.** Probes run with a generous budget (`PROBE_TIMEOUT`)
//! rather than the product's 100 ms, because the budget is a wall-clock
//! ceiling and a loaded CI host must not turn a scripted answer into an
//! `EngineTimeout`. The one test that asserts the product budget measures a
//! cache read, which performs no bridge call at all.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_ai_apple::bridge::{failure_document, success_document};
use plaudon_ai_apple::probe::AppleAvailability;
use plaudon_ai_apple::{
    probe_once, AppleBridge, AppleCapabilities, AppleSpeechToText, AppleTextModel,
    AssetDownloadConsent, BridgeOp, CacheOutcome, CompatibilityMatrix, EvidenceLevel,
    EvidenceRecord, FakeBridge, MacClass, ProbeCache, APPLE_FOUNDATION_ENGINE, APPLE_SPEECH_ENGINE,
    PROBE_BUDGET,
};
use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, MediaFormat, PrivacyClass, RequestFamily, SttCapabilities,
    TextModelCapabilities,
};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::policy::SttRequirements;
use plaudon_ai_contracts::stt::{SpeechToText, SttError, SttInput};
use plaudon_ai_contracts::text::{StructuredRequest, TextModel, TextModelError};
use plaudon_ai_contracts::{AlignmentOrigin, Known, Tri};
use plaudon_ai_whisper::WHISPER_ENGINE_ID as WHISPER_CRATE_ENGINE_ID;
use plaudon_app_core::chunking::{
    plan_chunks, ChunkError, ChunkPolicy, ContextBudget, ContextOrigin, Coverage, SegmentExtent,
    UnknownContext,
};
use plaudon_app_core::engine_policy::{
    degraded_to_local_fallback, explain, select_text_model, CorrectiveAction, TextEngineCandidate,
    TextRejection, TextRequirements, APPLE_TEXT_ENGINE_ID, OPENROUTER_ENGINE_ID,
};
use plaudon_app_core::stt::selection::{
    select_engine, CloudPolicy, EngineCandidate, EngineDecision, Platform, ProbeOutcome,
    SelectionRejection, SelectionRequest, APPLE_ENGINE_ID, WHISPER_ENGINE_ID,
};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::ids::JobId;
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};

/// Probe ceiling for scenarios that are not about latency. See the module
/// documentation.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Scripted platform answers
// ---------------------------------------------------------------------------

fn speech_probe(asset_status: &str, installed: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "available": true,
        "supported_locales": ["en_US", "ja_JP"],
        "installed_locales": installed,
        "resolved_locale": "en_US",
        "asset_status": asset_status,
        "supports_on_device": true,
        "is_transcriber": true,
        "max_concurrent_locales": 2,
        "reserved_locales": [],
        "accepted_formats": [],
        "supports_word_timestamps": true,
        "os_build": "26A434",
        "os_version": "27.0.1",
        "hardware": "Apple M4 Pro",
    })
}

/// The system language model as a ready Mac reports it: available, guided
/// output supported, and no context window published (spec §11.4).
fn text_probe() -> serde_json::Value {
    serde_json::json!({
        "available": true,
        "is_transcriber": false,
        "supported_languages": ["en-Latn-US"],
        "supports_structured_output": true,
        "max_context_tokens": null,
        "os_build": "26A434",
        "os_version": "27.0.1",
        "hardware": "Apple M4 Pro",
    })
}

/// A bridge whose probes both report a ready Mac. Unscripted operations are
/// refused, so a scenario that forgets to script a call fails loudly.
fn ready_bridge() -> FakeBridge {
    FakeBridge::new().with_default(|op| match op {
        BridgeOp::ProbeSpeech => success_document(op, speech_probe("installed", &["en_US"])),
        BridgeOp::ProbeFoundationModel => success_document(op, text_probe()),
        _ => failure_document(op, "engine_failure", Some("unscripted")),
    })
}

/// A bridge whose every operation is refused with the same platform verdict.
fn refusing_bridge(code: &'static str, detail: &'static str) -> FakeBridge {
    FakeBridge::new().with_default(move |op| failure_document(op, code, Some(detail)))
}

fn transcription() -> serde_json::Value {
    serde_json::json!({
        "text": "hello world",
        "segments": [
            {
                "seq_index": 0,
                "start_ms": 0,
                "end_ms": 900,
                "text": "hello",
                "confidence": 0.9,
                "words": [{ "text": "hello", "start_ms": 0, "end_ms": 400 }],
            },
            {
                "seq_index": 1,
                "start_ms": 960,
                "end_ms": 1500,
                "text": " world",
                "words": [{ "text": "world", "start_ms": 960, "end_ms": 1400 }],
            },
        ],
        "alignment_origin": "native",
        "locale": "en_US",
        "duration_ms": 1500,
        "final": true,
    })
}

// ---------------------------------------------------------------------------
// Fixtures shared across scenarios
// ---------------------------------------------------------------------------

fn handle(bridge: &Arc<FakeBridge>) -> Arc<dyn AppleBridge> {
    bridge.clone()
}

fn cache(bridge: &Arc<dyn AppleBridge>) -> Arc<ProbeCache> {
    Arc::new(ProbeCache::new(bridge.clone()).with_budget(PROBE_TIMEOUT))
}

fn speech_engine(bridge: &Arc<dyn AppleBridge>) -> AppleSpeechToText {
    AppleSpeechToText::new(bridge.clone(), cache(bridge))
}

fn text_model(bridge: &Arc<dyn AppleBridge>) -> AppleTextModel {
    AppleTextModel::new(bridge.clone(), cache(bridge))
}

fn availability(bridge: &FakeBridge) -> AppleAvailability {
    probe_once(bridge, Some("en-US"), PROBE_TIMEOUT)
}

fn en_us() -> LanguageTag {
    LanguageTag::parse("en-US").expect("literal tag")
}

fn stt_input(path: &Path, source_sample_offset: u64) -> SttInput {
    let mut input = SttInput::whole_file(
        path.to_path_buf(),
        Sha256Digest::from_bytes([7u8; 32]),
        1024,
        MediaFormat::new("wav", "pcm_s16le"),
        16_000,
        1,
    );
    input.duration_ms = Some(1500);
    input.locale_hint = Some(en_us());
    input.source_sample_offset = source_sample_offset;
    input
}

fn media_file(bytes: &[u8]) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("temp media file");
    std::fs::write(file.path(), bytes).expect("seed media bytes");
    file
}

/// A local or cloud engine as the policy sees it. Whisper.cpp and OpenRouter
/// are declared by hand: this suite proves how Apple's payloads interact with
/// them, not how they are built.
fn stt_caps(engine: &str, offline: bool) -> SttCapabilities {
    let id = EngineId::new(engine);
    let mut caps = SttCapabilities::unknown(id.clone(), ModelIdentity::new(id, "fixture"));
    caps.status = CapabilityStatus::Supported;
    caps.availability_reason = AvailabilityReason::Available;
    caps.offline = Tri::from_observation(Some(offline));
    caps.privacy_class = Known::Known(if offline {
        PrivacyClass::OnDevice
    } else {
        PrivacyClass::CloudRetaining
    });
    caps.request_family = if offline {
        RequestFamily::Unsupported
    } else {
        RequestFamily::MultipartStt
    };
    caps.can_cancel = Tri::Yes;
    caps.supports_segments = Tri::Yes;
    caps.locales = Known::Known(vec![en_us()]);
    caps
}

fn text_caps(engine: &str, offline: bool) -> TextModelCapabilities {
    let id = EngineId::new(engine);
    let mut caps = TextModelCapabilities::unknown(id.clone(), ModelIdentity::new(id, "fixture"));
    caps.status = CapabilityStatus::Supported;
    caps.availability_reason = AvailabilityReason::Available;
    caps.offline = Tri::from_observation(Some(offline));
    caps.privacy_class = Known::Known(if offline {
        PrivacyClass::OnDevice
    } else {
        PrivacyClass::CloudZeroRetention
    });
    caps.supports_structured_output = Tri::Yes;
    caps.can_cancel = Tri::Yes;
    caps
}

/// The Apple candidate exactly as the composition root builds it: the probe
/// outcome is the runtime answer, never inferred from the OS version (§8.4).
fn apple_candidate(caps: SttCapabilities) -> EngineCandidate {
    let probe = if caps.is_selectable() {
        ProbeOutcome::Succeeded
    } else {
        ProbeOutcome::Failed(format!("{:?}", caps.availability_reason))
    };
    EngineCandidate {
        capabilities: caps,
        probe,
    }
}

fn mac_request(cloud_policy: CloudPolicy) -> SelectionRequest {
    SelectionRequest {
        platform: Platform::MacOs,
        requirements: SttRequirements {
            locale: Some(en_us()),
            ..SttRequirements::default()
        },
        cloud_policy,
    }
}

/// Decide with Apple, whisper.cpp and an OpenRouter candidate all offered.
fn decide(apple: SttCapabilities, cloud_policy: CloudPolicy) -> EngineDecision {
    select_engine(
        &mac_request(cloud_policy),
        &[
            apple_candidate(apple),
            EngineCandidate::probed(stt_caps(WHISPER_ENGINE_ID, true)),
            EngineCandidate::probed(stt_caps(OPENROUTER_ENGINE_ID, false)),
        ],
    )
}

/// The assertions shared by every "Apple cannot run here" scenario: Apple is
/// not selectable, whisper.cpp runs, and cloud consent changes nothing
/// (spec §11.6, §22(4), E2E §18.7(10)).
fn assert_falls_back_to_local_whisper(apple: &SttCapabilities) {
    assert!(
        !apple.is_selectable(),
        "an unavailable probe must not yield a selectable payload: {:?}",
        apple.availability_reason
    );

    let decision = decide(apple.clone(), CloudPolicy::ConsentGranted);
    assert_eq!(
        decision.chosen.as_ref().map(EngineId::as_str),
        Some(WHISPER_ENGINE_ID),
        "{:?}",
        decision.explain()
    );
    assert!(degraded_to_local_fallback(&decision));
    assert!(
        decision.rejection_for(APPLE_ENGINE_ID).is_some(),
        "Apple must carry a recorded rejection, not silently vanish"
    );
    assert!(
        matches!(
            decision.rejection_for(OPENROUTER_ENGINE_ID),
            Some(SelectionRejection::NotPreferred)
        ),
        "the cloud engine is eligible under consent but must lose to local: {:?}",
        decision.explain()
    );
}

// ---------------------------------------------------------------------------
// 1. A ready Mac
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_ready_mac_transcribes_natively_and_the_policy_prefers_apple_over_whisper() {
    let bridge = Arc::new(ready_bridge().reply(
        BridgeOp::TranscribeFile,
        success_document(BridgeOp::TranscribeFile, transcription()),
    ));
    let engine = speech_engine(&handle(&bridge));
    let file = media_file(b"stand-in for a recording");

    // One second into the source: a chunk after the first must not restart the
    // timeline at zero (§8.2).
    let offset = 16_000;
    let draft = engine
        .transcribe(
            stt_input(file.path(), offset),
            JobContext::new(JobId::new()),
        )
        .await
        .expect("a ready Mac transcribes");

    assert_eq!(draft.alignment_origin, AlignmentOrigin::Native);
    assert!(draft.alignment_origin.permits_seek());
    assert_eq!(draft.engine_id.as_str(), APPLE_SPEECH_ENGINE);
    assert_eq!(draft.segments.len(), 2);

    // 960 ms at 16 kHz is 15 360 samples, placed after the chunk offset.
    let second_start = draft.segments[1].start.expect("native offsets");
    assert_eq!(second_start.sample_offset, offset + 15_360);
    assert_eq!(second_start.sample_rate_hz, 16_000);

    let word = &draft.segments[0].words[0];
    assert_eq!(word.text, "hello");
    assert_eq!(word.start.sample_offset, offset);
    assert_eq!(word.end.sample_offset, offset + 6_400);

    let caps = engine.capabilities().await;
    assert!(caps.is_selectable(), "{:?}", caps.availability_reason);
    let decision = decide(caps, CloudPolicy::LocalOnly);
    assert_eq!(
        decision.chosen.as_ref().map(EngineId::as_str),
        Some(APPLE_ENGINE_ID),
        "{:?}",
        decision.explain()
    );
    assert_eq!(
        decision.rejection_for(WHISPER_ENGINE_ID),
        Some(&SelectionRejection::NotPreferred),
        "whisper.cpp was eligible and outranked, not broken"
    );
    assert!(!degraded_to_local_fallback(&decision));
}

// ---------------------------------------------------------------------------
// 2. Apple Intelligence disabled
// ---------------------------------------------------------------------------

#[test]
fn disabling_apple_intelligence_falls_back_to_local_whisper_and_never_the_cloud() {
    let bridge = refusing_bridge("disabled_by_user", "apple_intelligence_not_enabled");
    let snapshot = availability(&bridge);
    let caps = AppleCapabilities::from_availability(&snapshot, AssetDownloadConsent::Granted);

    assert_falls_back_to_local_whisper(&caps.speech);
    assert_eq!(MacClass::classify(&snapshot), MacClass::AppleSiliconUnready);

    // The user is told the concrete runtime cause and offered the local
    // engine, not shown a generic failure (§5.4, §22(4)).
    let explanation = explain(
        &EngineId::new(APPLE_SPEECH_ENGINE),
        &caps.speech.availability_reason,
    );
    assert_eq!(explanation.code, "disabled_by_user");
    assert!(explanation.headline.contains("Apple Intelligence"));
    assert!(explanation
        .actions
        .contains(&CorrectiveAction::EnableAppleIntelligence));
    assert!(explanation
        .actions
        .contains(&CorrectiveAction::UseLocalWhisper));

    // Only the two probes ever reached the bridge.
    assert!(bridge.calls().iter().all(|call| matches!(
        call.op,
        BridgeOp::ProbeSpeech | BridgeOp::ProbeFoundationModel
    )));
}

// ---------------------------------------------------------------------------
// 3. Intel or otherwise unsupported
// ---------------------------------------------------------------------------

#[test]
fn an_intel_mac_is_classified_unsupported_and_falls_back_to_local_whisper() {
    let bridge = refusing_bridge("hardware_unsupported", "device_not_eligible");
    let snapshot = availability(&bridge);
    let caps = AppleCapabilities::from_availability(&snapshot, AssetDownloadConsent::Granted);

    assert_eq!(MacClass::classify(&snapshot), MacClass::IntelOrUnsupported);
    assert!(!caps.text_model.is_selectable());
    assert_falls_back_to_local_whisper(&caps.speech);

    let explanation = explain(
        &EngineId::new(APPLE_SPEECH_ENGINE),
        &caps.speech.availability_reason,
    );
    assert_eq!(explanation.code, "hardware_unsupported");
    assert!(
        explanation.headline.contains("device_not_eligible"),
        "the sentence must cite the platform's own evidence: {}",
        explanation.headline
    );
    assert!(explanation
        .actions
        .contains(&CorrectiveAction::UseLocalWhisper));
}

// ---------------------------------------------------------------------------
// 4. Local-only mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_only_mode_dispatches_nothing_and_selects_no_cloud_engine() {
    let bridge = Arc::new(refusing_bridge(
        "disabled_by_user",
        "apple_intelligence_not_enabled",
    ));
    let handle = handle(&bridge);
    let engine = speech_engine(&handle);
    let model = text_model(&handle);

    let speech = engine.capabilities().await;
    let text = model.capabilities().await;
    assert!(!speech.is_selectable());
    assert!(!text.is_selectable());

    // Transcription and generation both refuse before any work is dispatched.
    let file = media_file(b"recording");
    assert_eq!(
        engine
            .transcribe(stt_input(file.path(), 0), JobContext::new(JobId::new()))
            .await,
        Err(SttError::Unavailable)
    );
    let request = StructuredRequest {
        prompt_version: "meeting_summary@1".into(),
        prompt: "TRANSCRIPT: ...".into(),
        schema_version: "meeting_summary@1".into(),
        output_schema: serde_json::json!({ "type": "object" }),
        max_output_tokens: Some(256),
    };
    assert_eq!(
        model
            .generate_structured(request, JobContext::new(JobId::new()))
            .await
            .expect_err("an unavailable model refuses"),
        TextModelError::Unavailable
    );

    // Speech: with no local engine on offer nothing is chosen at all, and the
    // cloud candidate is refused for the mode, not merely outranked.
    let stt_decision = select_engine(
        &mac_request(CloudPolicy::LocalOnly),
        &[
            apple_candidate(speech.clone()),
            EngineCandidate::probed(stt_caps(OPENROUTER_ENGINE_ID, false)),
        ],
    );
    assert_eq!(stt_decision.chosen, None, "{:?}", stt_decision.explain());
    assert_eq!(
        stt_decision.rejection_for(OPENROUTER_ENGINE_ID),
        Some(&SelectionRejection::LocalOnlyModeBlocksCloud)
    );
    let with_whisper = decide(speech, CloudPolicy::LocalOnly);
    assert_eq!(
        with_whisper.chosen.as_ref().map(EngineId::as_str),
        Some(WHISPER_ENGINE_ID)
    );

    // Text: even a cloud model the user picked for this job is refused.
    let text_decision = select_text_model(
        &[
            TextEngineCandidate::offered(text),
            TextEngineCandidate::chosen(text_caps(OPENROUTER_ENGINE_ID, false)),
        ],
        &TextRequirements::default(),
        CloudPolicy::LocalOnly,
    );
    assert_eq!(text_decision.chosen, None, "{:?}", text_decision.explain());
    assert_eq!(
        text_decision.rejection_for(OPENROUTER_ENGINE_ID),
        Some(&TextRejection::LocalOnlyModeBlocksCloud)
    );

    // E2E §18.7(10): no operation other than a probe crossed the bridge.
    let calls = bridge.calls();
    assert!(!calls.is_empty(), "the probes must have run");
    assert!(
        calls.iter().all(|call| matches!(
            call.op,
            BridgeOp::ProbeSpeech | BridgeOp::ProbeFoundationModel
        )),
        "unexpected dispatch: {:?}",
        calls.iter().map(|call| call.op).collect::<Vec<_>>()
    );
    assert_eq!(bridge.live_token_count(), 0, "no handle was left behind");
}

// ---------------------------------------------------------------------------
// 5. Asset consent gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_uninstalled_locale_is_never_downloaded_without_consent_and_unlocks_after_install() {
    // Three speech probes, in the order the adapter takes them: the refused
    // transcription (locale-keyed cache entry), the pre-install capability
    // read, and the re-probe that an installation forces (§11.6).
    let bridge = Arc::new(
        FakeBridge::new()
            .reply(
                BridgeOp::ProbeSpeech,
                success_document(BridgeOp::ProbeSpeech, speech_probe("supported", &[])),
            )
            .reply(
                BridgeOp::ProbeSpeech,
                success_document(BridgeOp::ProbeSpeech, speech_probe("supported", &[])),
            )
            .reply(
                BridgeOp::InstallSpeechAssets,
                success_document(
                    BridgeOp::InstallSpeechAssets,
                    serde_json::json!({ "asset_status": "installed", "locale": "en_US" }),
                ),
            )
            .reply(
                BridgeOp::ProbeSpeech,
                success_document(BridgeOp::ProbeSpeech, speech_probe("installed", &["en_US"])),
            )
            .with_default(|op| failure_document(op, "engine_failure", Some("unscripted"))),
    );
    let engine = speech_engine(&handle(&bridge));
    let file = media_file(b"original recording bytes");
    let before = std::fs::read(file.path()).expect("read media");

    // Consent withheld: the engine fails closed instead of starting a
    // download the user never approved (§12).
    assert_eq!(
        engine
            .transcribe(stt_input(file.path(), 0), JobContext::new(JobId::new()))
            .await,
        Err(SttError::Unavailable)
    );
    assert!(
        bridge.calls_for(BridgeOp::TranscribeFile).is_empty(),
        "no transcription may be dispatched while the assets are absent"
    );
    assert_eq!(bridge.calls_for(BridgeOp::InstallSpeechAssets).len(), 0);
    assert!(!engine.capabilities().await.is_selectable());
    assert_eq!(std::fs::read(file.path()).expect("read media"), before);

    // The user approves the plan, the install succeeds, and the verdict flips.
    let plan = engine.asset_plan(&en_us()).await;
    assert!(plan.requires_download());
    let installed = engine
        .install_assets(&en_us(), JobContext::new(JobId::new()))
        .await
        .expect("install succeeds");
    assert_eq!(
        installed.asset_status,
        plaudon_ai_apple::envelope::AssetStatus::Installed
    );
    let caps = engine.capabilities().await;
    assert!(
        caps.is_selectable(),
        "the cache must be re-probed after an install: {:?}",
        caps.availability_reason
    );
    assert_eq!(std::fs::read(file.path()).expect("read media"), before);
}

// ---------------------------------------------------------------------------
// 6. Guardrail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_guardrail_refusal_is_an_error_and_never_a_summary() {
    let bridge = Arc::new(ready_bridge().reply(
        BridgeOp::GenerateText,
        failure_document(
            BridgeOp::GenerateText,
            "guardrail_violation",
            Some("language_model_guardrail"),
        ),
    ));
    let model = text_model(&handle(&bridge));
    let request = StructuredRequest {
        prompt_version: "meeting_summary@1".into(),
        prompt: "TRANSCRIPT: ...".into(),
        schema_version: "meeting_summary@1".into(),
        output_schema: serde_json::json!({
            "type": "object",
            "required": ["summary"],
            "properties": { "summary": { "type": "string" } },
        }),
        max_output_tokens: Some(256),
    };

    let outcome = model
        .generate_structured(request, JobContext::new(JobId::new()))
        .await;

    assert_eq!(
        outcome.expect_err("a guardrail is not a result"),
        TextModelError::ProviderRejected
    );
    // A refusal is final: it is not retried through schema repair, which
    // would only re-ask the model for the content it declined (§11.4).
    assert_eq!(bridge.calls_for(BridgeOp::GenerateText).len(), 1);
}

// ---------------------------------------------------------------------------
// 7. Context overflow drives re-chunking
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_context_overflow_error_shrinks_the_budget_and_replans_with_full_coverage() {
    let bridge = Arc::new(ready_bridge().reply(
        BridgeOp::GenerateText,
        failure_document(
            BridgeOp::GenerateText,
            "context_size_exceeded",
            Some("1000"),
        ),
    ));
    let model = text_model(&handle(&bridge));

    // The framework publishes no window up front (§11.4), so the payload says
    // Unknown and the planner must label whatever it assumes instead.
    let caps = model.capabilities().await;
    assert!(caps.is_selectable());
    assert!(matches!(caps.max_context_tokens, Known::Unknown));

    let fail_closed = ChunkPolicy {
        unknown_context: UnknownContext::FailClosed,
        reserved_output_tokens: 100,
        prompt_overhead_tokens: 50,
        overlap_segments: 1,
    };
    assert_eq!(
        ContextBudget::from_capabilities(&caps, &fail_closed),
        Err(ChunkError::ContextLengthUnverified)
    );

    let conservative = ChunkPolicy {
        unknown_context: UnknownContext::Conservative { tokens: 4096 },
        ..fail_closed
    };
    let budget =
        ContextBudget::from_capabilities(&caps, &conservative).expect("a labelled assumption");
    assert!(matches!(budget.origin, ContextOrigin::Conservative { .. }));

    // Forty 100-character segments: every one costs the same, so chunk counts
    // follow the allowance directly.
    let segments: Vec<SegmentExtent> = (0..40)
        .map(|seq_index| SegmentExtent {
            seq_index,
            chars: 100,
        })
        .collect();
    let first_plan = plan_chunks(&segments, &budget).expect("plan under the assumed window");

    // The model is the authority on its own limit and reveals it only here.
    let request = StructuredRequest {
        prompt_version: "meeting_summary@1".into(),
        prompt: "TRANSCRIPT: ...".into(),
        schema_version: "meeting_summary@1".into(),
        output_schema: serde_json::json!({ "type": "object" }),
        max_output_tokens: Some(256),
    };
    assert_eq!(
        model
            .generate_structured(request, JobContext::new(JobId::new()))
            .await
            .expect_err("the prompt overflowed"),
        TextModelError::ContextTooLarge
    );
    let observed = model
        .observed_context_window()
        .expect("the overflow revealed the window");
    assert_eq!(observed, 1000);

    let replanned = plan_chunks(&segments, &budget.shrunk(observed)).expect("re-plan");
    assert!(
        replanned.chunks.len() > first_plan.chunks.len(),
        "{} chunks should exceed {}",
        replanned.chunks.len(),
        first_plan.chunks.len()
    );
    assert_eq!(replanned.budget.origin, ContextOrigin::Verified);
    assert_eq!(replanned.coverage, Coverage::Full);
    assert!(replanned.is_complete());
}

// ---------------------------------------------------------------------------
// 8. Compatibility gate
// ---------------------------------------------------------------------------

/// A synthetic record. These exist to exercise the gate's logic; they are not
/// claims about any real machine (§22(17)).
fn record(class: MacClass, level: EvidenceLevel) -> EvidenceRecord {
    EvidenceRecord {
        class,
        test_run_id: format!("synthetic-{}", class.as_str()),
        observed_at: "2026-10-11T20:00:00Z".to_owned(),
        os_build: "26A434".to_owned(),
        os_version: "27.0.1".to_owned(),
        hardware: "Synthetic".to_owned(),
        apple_intelligence_ready: class == MacClass::AppleSiliconReady,
        locale: "en-US".to_owned(),
        asset_status: "installed".to_owned(),
        speech_transcribed: class == MacClass::AppleSiliconReady,
        text_generated: class == MacClass::AppleSiliconReady,
        fell_back_to_whisper: class != MacClass::AppleSiliconReady,
        network_requests: 0,
        evidence_level: level,
        comparison: None,
        known_limitations: Vec::new(),
    }
}

#[test]
fn the_compatibility_gate_fails_when_a_mac_class_has_no_evidence() {
    let mut records: Vec<EvidenceRecord> = MacClass::ALL
        .iter()
        .map(|class| record(*class, EvidenceLevel::RealHardware))
        .collect();
    let complete = CompatibilityMatrix::from_records(records.clone());
    assert_eq!(complete.gate_failures(), Vec::<String>::new());

    records.retain(|record| record.class != MacClass::IntelOrUnsupported);
    let missing = CompatibilityMatrix::from_records(records);
    let failures = missing.gate_failures();
    assert!(failures
        .iter()
        .any(|failure| failure.starts_with(MacClass::IntelOrUnsupported.as_str())));
    assert!(
        failures
            .iter()
            .all(|failure| failure.starts_with(MacClass::IntelOrUnsupported.as_str())),
        "only the dropped class may fail the gate: {failures:?}"
    );
}

#[test]
fn simulated_evidence_never_labels_a_ready_mac_supported() {
    let matrix = CompatibilityMatrix::from_records(
        MacClass::ALL
            .iter()
            .map(|class| record(*class, EvidenceLevel::Simulated))
            .collect(),
    );
    let ready = matrix
        .cell(MacClass::AppleSiliconReady)
        .expect("a cell per class");
    assert_ne!(ready.status, CapabilityStatus::Supported);
    assert_eq!(ready.status, CapabilityStatus::Experimental);
    assert_eq!(ready.evidence_level, EvidenceLevel::Simulated);
}

// ---------------------------------------------------------------------------
// 9. Engine identifiers agree across crates
// ---------------------------------------------------------------------------

/// The three crates each name an engine, and the policy matches on the string.
/// A drift between them would make selection silently reject a real engine,
/// so this is the one place the constants are compared.
#[test]
fn engine_identifiers_agree_across_crates() {
    assert_eq!(APPLE_SPEECH_ENGINE, APPLE_ENGINE_ID);
    assert_eq!(APPLE_FOUNDATION_ENGINE, APPLE_TEXT_ENGINE_ID);
    assert_eq!(WHISPER_ENGINE_ID, WHISPER_CRATE_ENGINE_ID);
    assert_eq!(APPLE_SPEECH_ENGINE, "apple-speech");
    assert_eq!(APPLE_FOUNDATION_ENGINE, "apple-foundation");
    assert_eq!(WHISPER_ENGINE_ID, "whisper.cpp");
}

// ---------------------------------------------------------------------------
// 10. Probe latency budget
// ---------------------------------------------------------------------------

#[test]
fn a_cached_probe_read_fits_the_latency_budget_and_does_not_call_the_bridge() {
    let bridge = Arc::new(ready_bridge());
    let probes = ProbeCache::new(handle(&bridge)).with_budget(PROBE_TIMEOUT);

    let (_, first) = probes.availability(Some("en-US"));
    assert_eq!(first, CacheOutcome::Probed);
    let calls_after_first = bridge.calls().len();
    assert_eq!(calls_after_first, 2, "one speech probe and one text probe");

    let started = Instant::now();
    let (snapshot, second) = probes.availability(Some("en-US"));
    let elapsed = started.elapsed();

    assert_eq!(second, CacheOutcome::Reused);
    assert!(
        elapsed < PROBE_BUDGET,
        "a cache read took {elapsed:?}, budget {PROBE_BUDGET:?}"
    );
    assert_eq!(bridge.calls().len(), calls_after_first);
    assert!(snapshot.speech.is_ok());
    assert_eq!(PROBE_BUDGET, Duration::from_millis(100));
}
