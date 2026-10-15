#![allow(clippy::await_holding_lock)] // see `ENGINE_GATE`

//! Local whisper.cpp over a real two-person interview (issue #308).
//!
//! `f_local_stt.rs` proves the engine works on an eleven-second speech
//! excerpt. That says nothing about a real conversation: two voices, turn
//! taking, short interjections, false starts and repeated words. This suite
//! runs the same pinned `ggml-tiny.en` model over a 2.5 minute excerpt of a
//! public-domain NASA interview and scores it against the published
//! transcript.
//!
//! **Hermeticity.** The excerpt and its reference transcript are committed
//! under `tests/fixtures/stt/interview/` (see `PROVENANCE.md` there for the
//! source, licence and how to re-derive the bytes). The model weights come
//! from `scripts/fetch-stt-fixtures.sh`. When either is missing the model
//! tests print `SKIPPED` and say how to fix it, rather than passing silently.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use plaudon_ai_contracts::capabilities::MediaFormat;
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::stt::{SpeechToText, SttInput};
use plaudon_ai_contracts::{AlignmentOrigin, ChunkingStrategy, TranscriptDraft};
use plaudon_ai_whisper::{WhisperConfig, WhisperEngine, WHISPER_SAMPLE_RATE_HZ};
use plaudon_audio::decode::{decode_to_canonical_pcm, CanonicalPcmProfile, MemoryPcmSink};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::ids::JobId;
use sha2::{Digest, Sha256};

const EXCERPT: &str = "interview/hwhap-407-excerpt.mp3";
const REFERENCE: &str = "interview/hwhap-407-excerpt.reference.txt";
const PROVENANCE: &str = "interview/PROVENANCE.md";

/// SHA-256 of the committed excerpt. `PROVENANCE.md` records the same value.
const EXCERPT_SHA256: &str = "PENDING";

/// SHA-256 of the full source file the excerpt was cut from.
const SOURCE_SHA256: &str = "a6b65f84a64161ca52ab5d6146db69c55a4add90bc0b914abfa84de0e61f60bb";

/// The excerpt runs from 130.3 s to 282.7 s of the source, 152.4 s.
const EXCERPT_DURATION_MS: u64 = 152_400;

/// Highest word error rate the tiny English model may score on the excerpt.
///
/// Provisional until the fixture lands and the real rate is measured; see
/// the PR for #308 for the measured figure and the margin applied to it.
const WER_BOUND: f64 = 0.25;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn fixture(relative: &str) -> PathBuf {
    repo_root().join("tests/fixtures/stt").join(relative)
}

/// The seeded model, honouring `PLAUDON_WHISPER_MODEL` like `f_local_stt.rs`.
fn tiny_model() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("PLAUDON_WHISPER_MODEL") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    let path = fixture("models/ggml-tiny.en.bin");
    path.is_file().then_some(path)
}

/// Everything the model tests need, all present.
#[derive(Debug, Clone)]
struct Fixtures {
    model: PathBuf,
    audio: PathBuf,
    reference: PathBuf,
}

/// Either every fixture, or the reason the model tests must skip.
fn fixture_status(
    model: Option<PathBuf>,
    audio: &Path,
    reference: &Path,
) -> Result<Fixtures, String> {
    let Some(model) = model else {
        return Err("SKIPPED: whisper model fixture is absent. Run \
                    `scripts/fetch-stt-fixtures.sh` to seed \
                    tests/fixtures/stt/models/, or set PLAUDON_WHISPER_MODEL."
            .to_string());
    };
    for (label, path) in [
        ("interview audio", audio),
        ("reference transcript", reference),
    ] {
        if !path.is_file() {
            return Err(format!(
                "SKIPPED: {label} fixture is absent at {}. Restore it from git, \
                 or rebuild it with `scripts/make-interview-fixture.sh`.",
                path.display()
            ));
        }
    }
    Ok(Fixtures {
        model,
        audio: audio.to_path_buf(),
        reference: reference.to_path_buf(),
    })
}

macro_rules! fixtures_or_skip {
    () => {
        match fixture_status(tiny_model(), &fixture(EXCERPT), &fixture(REFERENCE)) {
            Ok(fixtures) => fixtures,
            Err(message) => {
                eprintln!("{message}");
                return;
            }
        }
    };
}

/// Serialises every test that loads whisper weights (same rule, and same
/// reason, as `f_local_stt.rs`: one local model workload at a time).
static ENGINE_GATE: Mutex<()> = Mutex::new(());

/// The tiny model's transcript of the excerpt, computed once per process.
static TRANSCRIPT: Mutex<Option<(TranscriptDraft, std::time::Duration)>> = Mutex::new(None);

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    hex::encode(Sha256::digest(&bytes))
}

/// Lowercase words with punctuation dropped and apostrophes folded away, so
/// `It’s` and `its` compare equal and `STS-118` becomes `sts 118`.
fn normalise_words(text: &str) -> Vec<String> {
    text.chars()
        .filter(|c| !matches!(c, '\'' | '\u{2019}' | '\u{2018}'))
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Word error rate: word-level edit distance divided by reference length.
fn word_error_rate(reference: &[String], hypothesis: &[String]) -> f64 {
    assert!(
        !reference.is_empty(),
        "an empty reference has no error rate"
    );
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0; hypothesis.len() + 1];
    for (i, reference_word) in reference.iter().enumerate() {
        current[0] = i + 1;
        for (j, hypothesis_word) in hypothesis.iter().enumerate() {
            let substitution = previous[j] + usize::from(reference_word != hypothesis_word);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    #[allow(clippy::cast_precision_loss)]
    let rate = previous[hypothesis.len()] as f64 / reference.len() as f64;
    rate
}

/// The spoken words of the reference file.
///
/// Lines starting with `#` are notes and `[Speaker Name]` lines mark turns;
/// neither is speech.
fn reference_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('['))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Speaker labels in turn order.
fn reference_turns(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix('[')?.strip_suffix(']').map(str::to_owned))
        .collect()
}

/// Decoded length of the excerpt in 16 kHz samples.
fn canonical_frames(path: &Path) -> u64 {
    let media = validate_media(path, &ValidationRequest::manual_import())
        .unwrap_or_else(|error| panic!("excerpt failed validation: {error}"));
    let mut sink = MemoryPcmSink::default();
    decode_to_canonical_pcm(
        &media,
        &CanonicalPcmProfile::whisper_mono_16k(),
        &mut sink,
        &JobContext::new(JobId::new()),
    )
    .unwrap_or_else(|error| panic!("excerpt failed to decode: {error}"))
    .frames
}

/// Run the real engine over the MP3, through the audio pipeline, once.
async fn transcribe_once(fixtures: &Fixtures) -> (TranscriptDraft, std::time::Duration) {
    let _gate = ENGINE_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut cached = TRANSCRIPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(done) = cached.as_ref() {
        return done.clone();
    }

    let media = validate_media(&fixtures.audio, &ValidationRequest::manual_import())
        .expect("validate the interview excerpt");
    let input = SttInput {
        media_path: fixtures.audio.clone(),
        media_sha256: media.sha256(),
        byte_length: media.profile().byte_length,
        format: MediaFormat::new("mp3", "mp3"),
        duration_ms: media.profile().duration_ms.get().copied(),
        locale_hint: None,
        sample_rate_hz: *media.profile().sample_rate_hz.get().expect("rate"),
        channels: *media.profile().channels.get().expect("channels"),
        source_sample_offset: 0,
        normalization_profile: None,
        chunking_strategy: ChunkingStrategy::SingleChunk,
    };
    let engine = WhisperEngine::new(
        WhisperConfig::new(fixtures.model.clone(), "ggml-tiny.en").forcing_cpu(),
    );
    let started = std::time::Instant::now();
    let draft = engine
        .transcribe(input, JobContext::new(JobId::new()))
        .await
        .expect("the interview must transcribe");
    let done = (draft, started.elapsed());
    *cached = Some(done.clone());
    done
}

#[test]
fn issue_308_interview_excerpt_is_committed_and_matches_its_pinned_digest() {
    let path = fixture(EXCERPT);
    assert!(
        path.is_file(),
        "the interview excerpt is missing at {}",
        path.display()
    );
    assert_eq!(sha256_hex(&path), EXCERPT_SHA256, "excerpt bytes changed");

    let frames = canonical_frames(&path);
    let duration_ms = frames * 1000 / u64::from(WHISPER_SAMPLE_RATE_HZ);
    assert!(
        duration_ms.abs_diff(EXCERPT_DURATION_MS) < 500,
        "excerpt decodes to {duration_ms} ms, expected about {EXCERPT_DURATION_MS} ms"
    );
    assert!(
        (60_000..=300_000).contains(&duration_ms),
        "the fixture is meant to be one to five minutes long"
    );
}

#[test]
fn issue_308_provenance_note_records_source_licence_and_digests() {
    let path = fixture(PROVENANCE);
    let note = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    for required in [
        "https://commons.wikimedia.org/wiki/File:HWHAP_Ep407",
        "https://www.nasa.gov/podcasts/houston-we-have-a-podcast/a-place-in-space-to-call-home-part-1/",
        "public domain",
        SOURCE_SHA256,
        EXCERPT_SHA256,
    ] {
        assert!(
            note.contains(required),
            "PROVENANCE.md does not mention `{required}`"
        );
    }
}

#[test]
fn issue_308_reference_transcript_has_two_speakers_taking_turns() {
    let path = fixture(REFERENCE);
    assert!(
        path.is_file(),
        "reference transcript missing at {}",
        path.display()
    );
    let turns = reference_turns(&path);
    let mut speakers = turns.clone();
    speakers.sort();
    speakers.dedup();
    assert_eq!(speakers, ["Nilufar Ramji", "Tracy Dyson"]);
    assert!(
        turns.windows(2).all(|pair| pair[0] != pair[1]),
        "consecutive turns by the same speaker: {turns:?}"
    );
    assert!(turns.len() >= 6, "expected real turn taking, got {turns:?}");
    assert!(normalise_words(&reference_text(&path)).len() > 300);
}

#[test]
fn issue_308_word_error_rate_counts_edits_over_normalised_words() {
    let reference = normalise_words("It’s the the year of 1997, STS-118.");
    assert_eq!(
        reference,
        ["its", "the", "the", "year", "of", "1997", "sts", "118"]
    );
    assert!(word_error_rate(&reference, &reference).abs() < f64::EPSILON);
    // One deletion, one substitution, one insertion over eight words.
    let hypothesis = normalise_words("Its the year off 1997 STS 118 yes");
    let rate = word_error_rate(&reference, &hypothesis);
    assert!((rate - 3.0 / 8.0).abs() < 1e-9, "rate was {rate}");
}

#[test]
fn issue_308_missing_fixtures_skip_with_a_message_instead_of_passing() {
    let nowhere = PathBuf::from("/nonexistent/plaudon-issue-308");
    let reference = fixture(REFERENCE);

    let no_model = fixture_status(None, &nowhere, &reference).expect_err("must skip");
    assert!(no_model.starts_with("SKIPPED"), "{no_model}");
    assert!(
        no_model.contains("scripts/fetch-stt-fixtures.sh"),
        "{no_model}"
    );

    let no_audio =
        fixture_status(Some(nowhere.clone()), &nowhere, &reference).expect_err("must skip");
    assert!(no_audio.starts_with("SKIPPED"), "{no_audio}");
    assert!(no_audio.contains("interview audio"), "{no_audio}");
    assert!(
        no_audio.contains("scripts/make-interview-fixture.sh"),
        "{no_audio}"
    );
}

#[tokio::test]
async fn issue_308_tiny_model_transcribes_the_interview_within_the_wer_bound() {
    let fixtures = fixtures_or_skip!();
    let (draft, elapsed) = transcribe_once(&fixtures).await;

    let reference = normalise_words(&reference_text(&fixtures.reference));
    let hypothesis = normalise_words(&draft.plain_text());
    let rate = word_error_rate(&reference, &hypothesis);
    eprintln!(
        "issue #308: ggml-tiny.en WER {:.4} over {} reference words ({} hypothesis words), \
         transcription took {elapsed:.2?}",
        rate,
        reference.len(),
        hypothesis.len()
    );
    eprintln!("issue #308 hypothesis: {}", draft.plain_text());
    assert!(
        rate <= WER_BOUND,
        "WER {rate:.4} exceeds the {WER_BOUND} bound for ggml-tiny.en"
    );
}

#[tokio::test]
async fn issue_308_interview_segment_timestamps_are_monotonic_and_inside_the_duration() {
    let fixtures = fixtures_or_skip!();
    let (draft, _) = transcribe_once(&fixtures).await;
    let frames = canonical_frames(&fixtures.audio);

    assert_eq!(draft.alignment_origin, AlignmentOrigin::Native);
    assert!(
        draft.segments.len() > 10,
        "2.5 minutes of conversation should yield many segments, got {}",
        draft.segments.len()
    );

    let mut previous_end = 0u64;
    for (index, segment) in draft.segments.iter().enumerate() {
        let start = segment.start.expect("native alignment carries a start");
        let end = segment.end.expect("native alignment carries an end");
        assert_eq!(start.sample_rate_hz, WHISPER_SAMPLE_RATE_HZ);
        assert_eq!(end.sample_rate_hz, WHISPER_SAMPLE_RATE_HZ);
        assert!(
            start.sample_offset <= end.sample_offset,
            "segment {index} ends before it starts: {segment:?}"
        );
        assert!(
            start.sample_offset >= previous_end,
            "segment {index} starts at {} before the previous one ended at {previous_end}",
            start.sample_offset
        );
        assert!(
            end.sample_offset <= frames,
            "segment {index} ends at {} past the {frames}-sample recording",
            end.sample_offset
        );
        previous_end = end.sample_offset;
    }
}
