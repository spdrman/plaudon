//! `plaud-apple-probe`: the opt-in Apple availability probe and evidence
//! recorder (tasks H1.3, H2.4).
//!
//! macOS-only and never built by default. It prints the raw bridge documents,
//! the capability each one maps to, and — with `--json` — an evidence record
//! that can be pasted into the compatibility register. It transcribes nothing
//! unless given an audio file, and downloads nothing unless told to.
//!
//! ```text
//! scripts/probe-apple.sh [audio-file] [locale]
//! cargo run -p plaudon-ai-apple --features apple-bridge --bin plaud-apple-probe -- \
//!     --json --locale en-US --audio tests/fixtures/audio/speech.wav
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_ai_apple::bridge::AppleBridge;
use plaudon_ai_apple::compatibility::{ComparisonMetrics, EvidenceLevel, EvidenceRecord, MacClass};
use plaudon_ai_apple::envelope::{
    encode_request, BridgeOp, BridgeReply, GenerateRequest, TranscribeRequest, Transcription,
};
use plaudon_ai_apple::ffi::LinkedBridge;
use plaudon_ai_apple::probe::{probe_once, AppleCapabilities, AssetDownloadConsent, PROBE_BUDGET};

#[derive(Debug, Default)]
struct Options {
    audio: Option<PathBuf>,
    locale: Option<String>,
    json: bool,
    allow_download: bool,
    generate: bool,
    run_id: Option<String>,
}

fn parse_args() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => options.json = true,
            "--allow-download" => options.allow_download = true,
            "--generate" => options.generate = true,
            "--audio" => options.audio = args.next().map(PathBuf::from),
            "--locale" => options.locale = args.next(),
            "--run-id" => options.run_id = args.next(),
            // Positional form kept for `scripts/probe-apple.sh [audio] [locale]`.
            other if options.audio.is_none() && !other.starts_with("--") => {
                options.audio = Some(PathBuf::from(other));
            }
            other if options.locale.is_none() && !other.starts_with("--") => {
                options.locale = Some(other.to_owned());
            }
            other => eprintln!("ignoring unrecognized argument: {other}"),
        }
    }
    options
}

fn main() -> std::process::ExitCode {
    let options = parse_args();
    let bridge: Arc<dyn AppleBridge> = Arc::new(LinkedBridge::new());

    match bridge.abi_version() {
        Ok(version) if version == plaudon_ai_apple::EXPECTED_ABI_VERSION => {}
        Ok(version) => {
            eprintln!(
                "bridge ABI {version} does not match the expected {}",
                plaudon_ai_apple::EXPECTED_ABI_VERSION
            );
            return std::process::ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("bridge ABI check failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }

    // Timed with a generous budget so the real cost is observable; the
    // application itself uses `PROBE_BUDGET`.
    let started = Instant::now();
    let availability = probe_once(
        bridge.as_ref(),
        options.locale.as_deref(),
        Duration::from_secs(10),
    );
    let probe_elapsed = started.elapsed();
    let class = MacClass::classify(&availability);
    let caps = AppleCapabilities::from_availability(
        &availability,
        if options.allow_download {
            AssetDownloadConsent::Granted
        } else {
            AssetDownloadConsent::Withheld
        },
    );

    if !options.json {
        println!("== host ==");
        println!("class:   {} ({})", class.as_str(), class.description());
        println!("elapsed: {probe_elapsed:?} (budget {PROBE_BUDGET:?})");
        println!();
        println!("== probeSpeech ==");
        println!("{:#?}", availability.speech);
        println!("{:#?}", caps.speech);
        println!();
        println!("== probeFoundationModel ==");
        println!("{:#?}", availability.text_model);
        println!("{:#?}", caps.text_model);
    }

    let mut speech_transcribed = false;
    let mut comparison = None;
    if let Some(path) = &options.audio {
        let request = encode_request(
            BridgeOp::TranscribeFile,
            &TranscribeRequest {
                path: path.display().to_string(),
                locale: options.locale.clone(),
                cancel_token: 0,
                timeout_ms: 10 * 60 * 1000,
                allow_asset_download: options.allow_download,
                want_word_timestamps: true,
            },
        );
        let started = Instant::now();
        let document = bridge.call(BridgeOp::TranscribeFile, &request, None);
        let elapsed = started.elapsed();
        match document {
            Ok(document) => {
                match BridgeReply::<Transcription>::parse(BridgeOp::TranscribeFile, &document) {
                    BridgeReply::Ok(result) => {
                        speech_transcribed = true;
                        if !options.json {
                            println!();
                            println!("== transcribeFile ==");
                            println!("elapsed:          {elapsed:?}");
                            println!("alignment_origin: {:?}", result.alignment_origin);
                            println!("segments:         {}", result.segments.len());
                            println!("characters:       {}", result.text.chars().count());
                            println!("text:             {}", result.text);
                        }
                        comparison = Some(ComparisonMetrics {
                            fixture: path.display().to_string(),
                            audio_duration_ms: result.duration_ms.unwrap_or(0),
                            apple_elapsed_ms: u64::try_from(elapsed.as_millis())
                                .unwrap_or(u64::MAX),
                            whisper_elapsed_ms: None,
                            apple_text_sha256: sha256_hex(result.text.as_bytes()),
                            whisper_text_sha256: None,
                            agreement: None,
                        });
                    }
                    BridgeReply::Err(error) => {
                        if !options.json {
                            println!();
                            println!("== transcribeFile ==");
                            println!("error: {error:?}");
                        }
                    }
                }
            }
            Err(error) => eprintln!("transcribe call failed: {error}"),
        }
    }

    let mut text_generated = false;
    if options.generate {
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
        match bridge.call(BridgeOp::GenerateText, &request, None) {
            Ok(document) => {
                text_generated = document.contains("\"ok\":true");
                if !options.json {
                    println!();
                    println!("== generateText ==");
                    println!("{document}");
                }
            }
            Err(error) => eprintln!("generate call failed: {error}"),
        }
    }

    if options.json {
        let record = EvidenceRecord {
            class,
            test_run_id: options
                .run_id
                .unwrap_or_else(|| format!("manual-{}", now_rfc3339())),
            observed_at: now_rfc3339(),
            os_build: availability.os_build().unwrap_or("unknown").to_owned(),
            os_version: availability
                .speech
                .as_ref()
                .map(|p| p.os_version.clone())
                .unwrap_or_default(),
            hardware: availability.hardware().unwrap_or("unknown").to_owned(),
            apple_intelligence_ready: availability
                .text_model
                .as_ref()
                .is_ok_and(|probe| probe.available),
            locale: options.locale.unwrap_or_else(|| "system".to_owned()),
            asset_status: availability.speech.as_ref().map_or_else(
                |_| "unsupported".to_owned(),
                |probe| format!("{:?}", probe.asset_status).to_lowercase(),
            ),
            speech_transcribed,
            text_generated,
            // The probe binary exercises Apple only; the whisper.cpp leg of a
            // degradation record comes from the hardware suite.
            fell_back_to_whisper: false,
            network_requests: 0,
            evidence_level: EvidenceLevel::RealHardware,
            comparison,
            known_limitations: Vec::new(),
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&record).unwrap_or_else(|_| "{}".to_owned())
        );
    } else {
        println!();
        println!("Record the result in docs/spikes/A1.3-apple-availability.md and, if a");
        println!("transcription succeeded, in compatibility.yaml with a test_run_id.");
        println!("Re-run with --json to emit an evidence record for the release bundle.");
    }

    std::process::ExitCode::SUCCESS
}

/// Minimal RFC 3339 stamp without pulling a date library into a diagnostic.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let days = secs / 86_400;
    let rest = secs % 86_400;
    // Days since 1970-01-01, converted with the civil-from-days algorithm.
    let z = days as i64 + 719_468;
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

/// SHA-256 as lowercase hex, implemented locally so the diagnostic binary
/// does not add a dependency to the crate's release graph.
fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut message = bytes.to_vec();
    let bit_length = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (index, word) in w.iter_mut().take(16).enumerate() {
            let at = index * 4;
            *word = u32::from_be_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for index in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (slot, value) in h.iter_mut().zip(v) {
            *slot = slot.wrapping_add(value);
        }
    }
    h.iter().fold(String::with_capacity(64), |mut hex, word| {
        let _ = write!(hex, "{word:08x}");
        hex
    })
}
