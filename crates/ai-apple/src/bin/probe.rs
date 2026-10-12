//! `plaud-apple-probe`: the opt-in A1.3 availability probe.
//!
//! macOS-only and never built by default. Prints the raw bridge documents and
//! the capability each one maps to, so a compatibility record can be written
//! from observation rather than from an OS version.
//!
//!   scripts/probe-apple.sh [audio-file] [locale]

use plaudon_ai_apple::ffi::{self, CancelToken};
use plaudon_ai_apple::probe::{speech_availability, text_model_availability, BridgeDocument};

fn main() -> std::process::ExitCode {
    if let Err(error) = ffi::check_abi() {
        eprintln!("bridge ABI check failed: {error}");
        return std::process::ExitCode::FAILURE;
    }

    let audio = std::env::args().nth(1);
    let locale = std::env::args().nth(2);

    let speech = match ffi::probe_speech(locale.as_deref()) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("probe_speech failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("== probeSpeech ==");
    println!("{speech:#?}");
    println!("{:#?}", speech_availability(&speech));

    let text_model = match ffi::probe_foundation_model() {
        Ok(document) => document,
        Err(error) => {
            eprintln!("probe_foundation_model failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!();
    println!("== probeFoundationModel ==");
    println!("{text_model:#?}");
    println!("{:#?}", text_model_availability(&text_model));

    if let Some(path) = audio {
        println!();
        println!("== transcribeFile ==");
        let cancel = CancelToken::new();
        let budget = std::time::Duration::from_secs(
            std::env::var("PLAUDON_APPLE_TIMEOUT_SECS")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(120),
        );
        match ffi::transcribe_file(&path, locale.as_deref(), &cancel, budget) {
            Ok(BridgeDocument::Ok(transcription)) => {
                println!("alignment_origin: {:?}", transcription.alignment_origin);
                println!("segments: {}", transcription.segments.len());
                println!("characters: {}", transcription.text.chars().count());
            }
            Ok(BridgeDocument::Err(error)) => println!("error: {error:?}"),
            Err(error) => println!("call failed: {error}"),
        }
    }

    std::process::ExitCode::SUCCESS
}
