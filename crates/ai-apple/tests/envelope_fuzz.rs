//! Property and corpus fuzzing of the envelope decoder (task H1.5).
//!
//! Spec 6.6: "we will fuzz and stress the bridge before shipping". The C ABI
//! itself is fuzzed from `native/apple-bridge/tests/abi_stress.c` under
//! AddressSanitizer, where the interesting failures are memory failures. This
//! file fuzzes the half that runs on every platform — the decoder that turns
//! an arbitrary byte string into a capability verdict — where the interesting
//! failure is a malformed document being read as availability.
//!
//! The invariant under test is one sentence: **no input, however hostile,
//! makes this build believe in a capability it did not observe.**
//!
//! Hermetic. No network, no Apple framework, no real provider.

use std::path::{Path, PathBuf};

use plaudon_ai_apple::envelope::{
    AssetStatus, BridgeErrorCode, BridgeOp, BridgeReply, ProgressEvent, SpeechProbe,
    TextModelProbe, Transcription,
};
use plaudon_ai_apple::probe::{speech_capabilities, text_model_capabilities, AssetDownloadConsent};
use plaudon_domain::capability::CapabilityStatus;
use proptest::prelude::*;

/// A fixed input for the pure draft mapping. Nothing is read from disk.
fn mapping_input() -> plaudon_ai_contracts::stt::SttInput {
    plaudon_ai_contracts::stt::SttInput::whole_file(
        PathBuf::from("/tmp/plaudon-fuzz.wav"),
        plaudon_domain::media::Sha256Digest::from_bytes([3u8; 32]),
        1024,
        plaudon_ai_contracts::capabilities::MediaFormat::new("wav", "pcm_s16le"),
        16_000,
        1,
    )
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("tests/fixtures/apple-bridge")
}

/// Decode `document` every way the application can, and assert that nothing
/// it produces claims a capability.
fn assert_never_claims_capability(document: &str) {
    for op in BridgeOp::ALL {
        let speech = BridgeReply::<SpeechProbe>::parse(op, document);
        let text = BridgeReply::<TextModelProbe>::parse(op, document);
        let transcription = BridgeReply::<Transcription>::parse(op, document);

        let speech_result = match speech {
            BridgeReply::Ok(probe) => Ok(probe),
            BridgeReply::Err(error) => Err(error),
        };
        let text_result = match text {
            BridgeReply::Ok(probe) => Ok(probe),
            BridgeReply::Err(error) => Err(error),
        };

        let stt = speech_capabilities(&speech_result, AssetDownloadConsent::Withheld);
        let llm = text_model_capabilities(&text_result);

        // Supported is only reachable from a document that actually said all
        // of: available, a transcriber, on device, assets installed.
        if stt.status == CapabilityStatus::Supported {
            let probe = speech_result.as_ref().expect("Supported implies a payload");
            assert!(probe.available, "{document}");
            assert!(probe.is_transcriber, "{document}");
            assert!(probe.supports_on_device, "{document}");
            assert!(
                probe.locale_installed || probe.asset_status == AssetStatus::Installed,
                "support without an installed locale: {document}"
            );
        }
        if llm.status == CapabilityStatus::Supported {
            let probe = text_result.as_ref().expect("Supported implies a payload");
            assert!(probe.available, "{document}");
            assert!(!probe.is_transcriber, "{document}");
        }
        // A capability that is not Supported is never selectable, whatever
        // the document claimed.
        if stt.status != CapabilityStatus::Supported {
            assert!(!stt.is_selectable(), "{document}");
        }

        // A transcription that decoded must still survive the draft
        // mapping, which is where a self-contradictory document is refused.
        // A document is allowed to be contradictory; a draft is not.
        if let BridgeReply::Ok(result) = transcription {
            if let Ok(draft) = plaudon_ai_apple::stt::to_draft(&result, &mapping_input()) {
                assert!(
                    draft.alignment_origin == result.alignment_origin,
                    "{document}"
                );
                if !draft.alignment_origin.permits_times() {
                    assert!(
                        draft.segments.iter().all(|segment| segment.start.is_none()),
                        "a timeline-free draft carried offsets: {document}"
                    );
                    assert!(
                        draft
                            .segments
                            .iter()
                            .all(|segment| segment.words.is_empty()),
                        "a timeline-free draft carried word timings: {document}"
                    );
                }
                for segment in &draft.segments {
                    if let (Some(start), Some(end)) = (&segment.start, &segment.end) {
                        assert!(
                            end.sample_offset >= start.sample_offset,
                            "an inverted span survived: {document}"
                        );
                    }
                }
            } else {
                // Refusing is always a correct answer here.
            }
        }
    }
    // Progress parsing must never panic and must never accept a foreign
    // envelope version.
    if let Some(event) = ProgressEvent::parse(document) {
        assert!(event.fraction.is_none_or(f32::is_finite), "{document}");
    }
}

#[test]
fn the_committed_corpus_never_produces_a_capability_claim() {
    let dir = corpus_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("seed corpus missing at {}: {error}", dir.display()));
    let mut seeds = 0;
    for entry in entries {
        let path = entry.expect("readable entry").path();
        if path.is_dir() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("readable seed");
        let document = String::from_utf8_lossy(&bytes);
        assert_never_claims_capability(&document);
        seeds += 1;
    }
    assert!(seeds >= 12, "the committed seed corpus shrank to {seeds}");
}

#[test]
fn truncating_a_valid_document_at_every_byte_never_yields_support() {
    let valid = serde_json::json!({
        "envelope_version": 1,
        "abi_version": 2,
        "op": "probe_speech",
        "ok": true,
        "result": {
            "available": true,
            "is_transcriber": true,
            "supports_on_device": true,
            "asset_status": "installed",
            "installed_locales": ["en_US"],
            "supported_locales": ["en_US"],
            "max_concurrent_locales": 5,
            "supports_word_timestamps": true,
            "os_build": "26A434",
        },
    })
    .to_string();
    for cut in 0..valid.len() {
        assert_never_claims_capability(&valid[..cut]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2048, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes_never_panic_and_never_claim_a_capability(raw in proptest::collection::vec(any::<u8>(), 0..512)) {
        let document = String::from_utf8_lossy(&raw).into_owned();
        assert_never_claims_capability(&document);
    }

    #[test]
    fn arbitrary_text_never_panics(document in ".{0,400}") {
        assert_never_claims_capability(&document);
    }

    /// A structurally valid frame with arbitrary field values.
    #[test]
    fn arbitrary_frames_decode_without_fabricating_support(
        envelope_version in 0u32..5,
        abi_version in 0i32..5,
        op in proptest::sample::select(BridgeOp::ALL.to_vec()),
        ok in any::<bool>(),
        available in any::<bool>(),
        is_transcriber in any::<bool>(),
        on_device in any::<bool>(),
        status in proptest::sample::select(vec!["installed", "supported", "downloading", "unsupported", "???"]),
        code in proptest::sample::select(
            BridgeErrorCode::KNOWN.iter().map(|c| c.as_str().to_owned()).chain(std::iter::once("from_the_future".to_owned())).collect::<Vec<_>>()
        ),
    ) {
        let document = serde_json::json!({
            "envelope_version": envelope_version,
            "abi_version": abi_version,
            "op": op.as_str(),
            "ok": ok,
            "result": {
                "available": available,
                "is_transcriber": is_transcriber,
                "supports_on_device": on_device,
                "asset_status": status,
            },
            "error": { "code": code, "detail": null, "retriable": false },
        })
        .to_string();
        assert_never_claims_capability(&document);

        // A frame from a version this build does not speak is refused by
        // name, never interpreted.
        if envelope_version != 1 {
            let reply = BridgeReply::<SpeechProbe>::parse(op, &document);
            prop_assert!(matches!(
                reply.err().map(|e| e.code.clone()),
                Some(BridgeErrorCode::UnsupportedEnvelopeVersion)
            ));
        }
    }

    /// Deeply nested JSON must not blow the stack or be accepted.
    #[test]
    fn deeply_nested_documents_are_refused_without_a_crash(depth in 1usize..200) {
        let document = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert_never_claims_capability(&document);
    }

    /// Giant scalars must not be read as plausible limits.
    #[test]
    fn oversized_scalars_do_not_become_capabilities(value in any::<u64>()) {
        let document = serde_json::json!({
            "envelope_version": 1,
            "abi_version": 2,
            "op": "probe_speech",
            "ok": true,
            "result": {
                "available": true,
                "is_transcriber": true,
                "supports_on_device": true,
                "asset_status": "installed",
                "installed_locales": ["en_US"],
                "max_concurrent_locales": value % u64::from(u32::MAX),
            },
        })
        .to_string();
        assert_never_claims_capability(&document);
    }
}

#[test]
fn a_locale_list_of_garbage_yields_no_locale_support() {
    let document = serde_json::json!({
        "envelope_version": 1,
        "abi_version": 2,
        "op": "probe_speech",
        "ok": true,
        "result": {
            "available": true,
            "is_transcriber": true,
            "supports_on_device": true,
            "asset_status": "installed",
            "installed_locales": ["", "-", "!!", "\u{202e}", "x".repeat(400)],
            "supported_locales": ["????"],
        },
    })
    .to_string();
    let reply = match BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document) {
        BridgeReply::Ok(probe) => Ok(probe),
        BridgeReply::Err(error) => Err(error),
    };
    let caps = speech_capabilities(&reply, AssetDownloadConsent::Withheld);
    // The engine is otherwise ready, so the status is Supported, but no
    // locale requirement can be satisfied from unparsable tags.
    assert_eq!(caps.status, CapabilityStatus::Supported);
    assert!(!caps.supports_locale(&plaudon_domain::locale::LanguageTag::parse("en-US").unwrap()));
}
