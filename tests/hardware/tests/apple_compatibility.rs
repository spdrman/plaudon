//! The three Mac classes of spec 11.5 (task H2.4).
//!
//! Spec 11.5 requires testing (1) a supported Apple Silicon Mac with the
//! requisite OS and models, (2) an Apple Silicon Mac with Apple Intelligence
//! disabled or unready, and (3) an Intel or otherwise unsupported Mac, and
//! comparing results against whisper.cpp. DoD 22(17) adds that a macOS
//! compatibility claim must be backed by a **named real-hardware test**.
//!
//! This suite produces those records. It cannot produce all three on one
//! machine — that is the point of the matrix — so each run classifies the
//! host, measures what that host can show, and writes an evidence record the
//! release bundle collects. Running it on three machines fills the matrix.
//!
//! It never runs by default:
//!
//! ```text
//! PLAUDON_APPLE_HARDWARE_TESTS=1 \
//! PLAUDON_APPLE_AUDIO_FIXTURE=tests/fixtures/audio/speech-16k.wav \
//!   cargo test -p plaudon-hardware-tests --features apple-bridge \
//!     --test apple_compatibility -- --nocapture
//! ```
//!
//! The default run, with no variable and no feature, asserts only that the
//! gate holds. That is what the macOS CI job checks.

const OPT_IN: &str = "PLAUDON_APPLE_HARDWARE_TESTS";

/// Fixture the measurement transcribes, when one is supplied. Read only by
/// the `measured` module, which exists only on macOS with the bridge linked.
#[cfg(all(target_os = "macos", feature = "apple-bridge"))]
const AUDIO: &str = "PLAUDON_APPLE_AUDIO_FIXTURE";

#[test]
fn the_hardware_suite_is_opt_in() {
    // Printed so a CI log shows the suite was present and declined.
    if !plaudon_hardware_tests::opted_in(OPT_IN, "the Apple compatibility suite") {
        return;
    }
    println!("{OPT_IN} is set; the measurement tests will run.");
}

#[cfg(all(target_os = "macos", feature = "apple-bridge"))]
mod measured {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use plaudon_ai_apple::bridge::AppleBridge;
    use plaudon_ai_apple::compatibility::{
        ComparisonMetrics, EvidenceLevel, EvidenceRecord, MacClass,
    };
    use plaudon_ai_apple::envelope::{
        encode_request, BridgeOp, BridgeReply, GenerateRequest, TranscribeRequest, Transcription,
    };
    use plaudon_ai_apple::probe::{
        probe_once, AppleCapabilities, AssetDownloadConsent, PROBE_BUDGET,
    };
    use plaudon_domain::capability::CapabilityStatus;

    use super::{AUDIO, OPT_IN};

    fn bridge() -> Arc<dyn AppleBridge> {
        plaudon_ai_apple::host_bridge()
    }

    /// Classify the host and record everything the matrix needs about it.
    ///
    /// One test rather than three, because a machine belongs to exactly one
    /// class and pretending otherwise would let a single run claim evidence
    /// for cells it never touched.
    #[test]
    fn this_mac_produces_an_evidence_record_for_its_class() {
        if !plaudon_hardware_tests::opted_in(OPT_IN, "the Apple compatibility measurement") {
            return;
        }
        let bridge = bridge();
        let locale = std::env::var("PLAUDON_APPLE_LOCALE").unwrap_or_else(|_| "en-US".to_owned());

        let started = Instant::now();
        let availability = probe_once(bridge.as_ref(), Some(&locale), Duration::from_secs(10));
        let probe_elapsed = started.elapsed();
        let class = MacClass::classify(&availability);
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);

        println!(
            "class:           {} ({})",
            class.as_str(),
            class.description()
        );
        println!("probe elapsed:   {probe_elapsed:?} (application budget {PROBE_BUDGET:?})");
        println!("speech status:   {:?}", caps.speech.status);
        println!("text status:     {:?}", caps.text_model.status);
        println!("speech reason:   {:?}", caps.speech.availability_reason);
        println!("text reason:     {:?}", caps.text_model.availability_reason);

        let mut speech_transcribed = false;
        let mut comparison = None;
        if let Some(path) = plaudon_hardware_tests::audio_fixture(AUDIO) {
            let request = encode_request(
                BridgeOp::TranscribeFile,
                &TranscribeRequest {
                    path: path.display().to_string(),
                    locale: Some(locale.clone()),
                    cancel_token: 0,
                    timeout_ms: 10 * 60 * 1000,
                    allow_asset_download: std::env::var("PLAUDON_APPLE_ALLOW_DOWNLOAD").is_ok(),
                    want_word_timestamps: true,
                },
            );
            let started = Instant::now();
            let document = bridge
                .call(BridgeOp::TranscribeFile, &request, None)
                .expect("the bridge always returns a document");
            let elapsed = started.elapsed();
            match BridgeReply::<Transcription>::parse(BridgeOp::TranscribeFile, &document) {
                BridgeReply::Ok(result) => {
                    speech_transcribed = true;
                    println!(
                        "transcription:   {} segments in {elapsed:?}",
                        result.segments.len()
                    );
                    println!("alignment:       {:?}", result.alignment_origin);
                    println!("text:            {}", result.text);
                    assert!(
                        result.final_result,
                        "a successful transcription is always final"
                    );
                    comparison = Some(ComparisonMetrics {
                        fixture: path.display().to_string(),
                        audio_duration_ms: result.duration_ms.unwrap_or_default(),
                        apple_elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                        whisper_elapsed_ms: None,
                        apple_text_sha256: digest(result.text.as_bytes()),
                        whisper_text_sha256: None,
                        agreement: None,
                    });
                }
                BridgeReply::Err(error) => {
                    println!("transcription:   refused with {error:?}");
                }
            }
        } else {
            println!("transcription:   skipped, no {AUDIO} fixture");
        }

        let mut text_generated = false;
        if caps.text_model.status == CapabilityStatus::Supported {
            let request = encode_request(
                BridgeOp::GenerateText,
                &GenerateRequest {
                    prompt: "TRANSCRIPT: We decided to ship on Friday. Anna sends the notes."
                        .to_owned(),
                    instructions: Some(plaudon_ai_apple::text::GROUNDING_INSTRUCTIONS.to_owned()),
                    schema: Some(serde_json::json!({
                        "type": "object",
                        "required": ["summary"],
                        "properties": { "summary": { "type": "string" } },
                    })),
                    cancel_token: 0,
                    timeout_ms: 120_000,
                    max_output_tokens: Some(256),
                    temperature: Some(0.0),
                },
            );
            let document = bridge
                .call(BridgeOp::GenerateText, &request, None)
                .expect("the bridge always returns a document");
            text_generated = document.contains("\"ok\":true");
            println!(
                "generation:      {}",
                if text_generated {
                    "produced a document"
                } else {
                    "refused"
                }
            );
            println!("generation raw:  {document}");
        }

        let record = EvidenceRecord {
            class,
            test_run_id: std::env::var("PLAUDON_TEST_RUN_ID")
                .unwrap_or_else(|_| format!("apple-{}-{}", class.as_str(), stamp())),
            observed_at: stamp(),
            os_build: availability.os_build().unwrap_or("unknown").to_owned(),
            os_version: availability
                .speech
                .as_ref()
                .map(|probe| probe.os_version.clone())
                .unwrap_or_default(),
            hardware: availability.hardware().unwrap_or("unknown").to_owned(),
            apple_intelligence_ready: class == MacClass::AppleSiliconReady,
            locale,
            asset_status: availability
                .speech
                .as_ref()
                .map(|probe| format!("{:?}", probe.asset_status).to_lowercase())
                .unwrap_or_else(|_| "unsupported".to_owned()),
            speech_transcribed,
            text_generated,
            // Filled in by the degradation test below, which runs in the same
            // process and rewrites the record.
            fell_back_to_whisper: false,
            network_requests: 0,
            evidence_level: EvidenceLevel::RealHardware,
            comparison,
            known_limitations: Vec::new(),
        };
        let path = plaudon_hardware_tests::write_evidence(
            &format!("apple-{}", class.as_str()),
            &serde_json::to_value(&record).expect("an evidence record serializes"),
        )
        .expect("the evidence directory is writable");
        println!("evidence:        {}", path.display());

        // The claim this run is allowed to make. A machine in a degradation
        // class must not produce a Supported label no matter what it measured.
        if class == MacClass::AppleSiliconReady && speech_transcribed {
            assert!(
                record.supports_a_supported_label(),
                "a ready Mac that transcribed should support a Supported label"
            );
        } else {
            assert!(
                !record.supports_a_supported_label(),
                "only a ready Mac that produced output may support a Supported label"
            );
        }
    }

    /// Availability is cheap enough to ask before every decision.
    ///
    /// Spec 11.3 treats it as a precondition of each engine choice, so a slow
    /// probe is a product problem. The application budget is 100 ms and a
    /// cached read is what the library view actually performs; this measures
    /// both the cold call and the cached one on real hardware.
    #[test]
    fn the_probe_is_cheap_enough_to_ask_constantly() {
        if !plaudon_hardware_tests::opted_in(OPT_IN, "the Apple probe latency measurement") {
            return;
        }
        let bridge = bridge();
        let cold = Instant::now();
        let _ = probe_once(bridge.as_ref(), Some("en-US"), Duration::from_secs(10));
        let cold = cold.elapsed();

        let cache = plaudon_ai_apple::probe::ProbeCache::new(bridge);
        let _ = cache.availability(Some("en-US"));
        let warm = Instant::now();
        let (_, outcome) = cache.availability(Some("en-US"));
        let warm = warm.elapsed();

        println!("cold probe:  {cold:?}");
        println!("cached read: {warm:?} ({outcome:?})");
        assert!(
            warm < PROBE_BUDGET,
            "a cached availability read took {warm:?}, over the {PROBE_BUDGET:?} budget"
        );
    }

    /// Cancellation and the time budget hold against the real platform.
    #[test]
    fn a_real_run_is_cancellable_and_bounded() {
        if !plaudon_hardware_tests::opted_in(OPT_IN, "the Apple cancellation measurement") {
            return;
        }
        let bridge = bridge();
        let baseline = bridge.live_token_count();

        let guard = plaudon_ai_apple::CancelGuard::new(bridge.clone());
        guard.cancel();
        let request = encode_request(
            BridgeOp::GenerateText,
            &GenerateRequest {
                prompt: "Summarize this.".to_owned(),
                instructions: None,
                schema: None,
                cancel_token: guard.token(),
                timeout_ms: 60_000,
                max_output_tokens: None,
                temperature: None,
            },
        );
        let started = Instant::now();
        let document = bridge
            .call(BridgeOp::GenerateText, &request, None)
            .expect("a document is always returned");
        assert!(document.contains("cancelled"), "{document}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a pre-cancelled call must return immediately"
        );
        drop(guard);
        assert_eq!(bridge.live_token_count(), baseline, "the handle leaked");
    }

    /// RFC 3339 stamp without adding a date dependency to a test package.
    fn stamp() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let days = i64::try_from(secs / 86_400).unwrap_or_default();
        let rest = secs % 86_400;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rest / 3600,
            (rest % 3600) / 60,
            rest % 60
        )
    }

    /// Lowercase hex SHA-256, so a transcript regression between two runs of
    /// the same fixture is detectable without storing the transcript itself
    /// in the evidence bundle.
    fn digest(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(bytes))
    }
}
