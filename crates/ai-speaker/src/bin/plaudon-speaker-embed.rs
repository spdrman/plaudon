//! The ADR-0081 §4 measurement bridge: audio in, harness input out.
//!
//! ```text
//! plaudon-speaker-embed build-fixtures --librispeech <dir> --plan <plan.json> --out <dir>
//!                                      [--ffmpeg <path>]
//! plaudon-speaker-embed embed --model <file.onnx> --fixtures <dir> --plan <plan.json>
//!                             --out <eval-embeddings.json> [--stats <file.json>] [--threads <n>]
//! ```
//!
//! `plaudon-eval speaker-match` scores embeddings, never audio (VP-183): it is
//! hermetic by construction and must stay that way. This tool is the other
//! half — the only place where a real checkpoint meets real speech — and it is
//! deliberately a separate binary that exists only under the `onnx` feature.
//!
//! **What it builds.** `build-fixtures` turns LibriSpeech `test-clean` into
//! the fixed-length single-speaker utterances the harness plan describes, in
//! the channel conditions it lists. The mapping from plan speaker label to
//! LibriSpeech speaker id is deterministic (both sorted, then zipped) and is
//! written out with a digest per file, so a second run on another machine
//! produces the same fixtures or says which file differs.
//!
//! **What it never does.** It does not commit anything. The vectors it writes
//! are voice embeddings of real readers; VP-002 and VP-112 make them
//! unloggable and unexportable, and `docs/benchmarks/` carries the *report*
//! (rates, thresholds, counts) that `plaudon-eval` derives from them, never
//! the vectors themselves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_speaker::checkpoint::{ModelArtifact, BUNDLE_TIER, MODEL_SAMPLE_RATE_HZ};
use plaudon_ai_speaker::onnx::{EngineOptions, SpeakerEmbeddingModel};
use plaudon_audio::decode::{decode_to_canonical_pcm, CanonicalPcmProfile, MemoryPcmSink, PcmSink};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::ids::JobId;
use plaudon_eval::speaker::embeddings::{EmbeddingFile, EMBEDDING_FILE_SCHEMA_VERSION};
use plaudon_eval::speaker::fixtures::Condition;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const USAGE: &str = "\
plaudon-speaker-embed build-fixtures --librispeech <dir> --plan <plan.json> --out <dir>
                                     [--ffmpeg <path>]
plaudon-speaker-embed embed --model <file.onnx> --fixtures <dir> --plan <plan.json>
                            --out <eval-embeddings.json> [--stats <file.json>] [--threads <n>]";

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = arguments.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let result = match command {
        "build-fixtures" => build_fixtures(&arguments[1..]),
        "embed" => embed(&arguments[1..]),
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            return;
        }
        other => Err(format!("unknown command `{other}`\n{USAGE}")),
    };
    if let Err(error) = result {
        eprintln!("plaudon-speaker-embed: {error}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------- arguments

fn value<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|index| arguments.get(index + 1))
        .map(String::as_str)
}

fn required<'a>(arguments: &'a [String], name: &str) -> Result<&'a str, String> {
    value(arguments, name).ok_or_else(|| format!("{name} is required\n{USAGE}"))
}

// --------------------------------------------------------------------- plan

/// Exactly the fields of a harness plan this tool needs.
///
/// Deserialised loosely on purpose: the plan is `plaudon-eval`'s document and
/// gains fields there, and a fixture builder that refuses to read a plan it
/// does not fully understand would couple two tools that only share a roster.
#[derive(Debug, Deserialize)]
struct PlanView {
    conditions: Vec<Condition>,
    roster: Vec<String>,
    cohort_speakers: Vec<String>,
    enrol_utterances: u32,
    test_utterances: u32,
    utterance_ms: u32,
}

impl PlanView {
    fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        serde_json::from_str(&text).map_err(|error| format!("parsing {}: {error}", path.display()))
    }

    /// Every (speaker, utterance, condition) the harness will ask for.
    fn segments(&self) -> Vec<SegmentRequest> {
        let mut out = Vec::new();
        for speaker in &self.roster {
            for utterance in 0..self.enrol_utterances {
                out.push(SegmentRequest::new(speaker, utterance, Condition::Clean));
            }
            for utterance in self.enrol_utterances..self.enrol_utterances + self.test_utterances {
                for condition in &self.conditions {
                    out.push(SegmentRequest::new(speaker, utterance, *condition));
                }
            }
        }
        for speaker in &self.cohort_speakers {
            out.push(SegmentRequest::new(speaker, 0, Condition::Clean));
        }
        out.sort_by_key(SegmentRequest::token);
        out.dedup_by(|left, right| left.token() == right.token());
        out
    }

    /// Speakers in the order they are assigned to LibriSpeech readers.
    fn speakers(&self) -> Vec<String> {
        let mut all = self.roster.clone();
        all.extend(self.cohort_speakers.iter().cloned());
        all
    }

    /// Highest utterance index any speaker needs.
    fn utterances_per_speaker(&self) -> u32 {
        self.enrol_utterances + self.test_utterances
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentRequest {
    speaker: String,
    utterance: u32,
    condition: Condition,
}

impl SegmentRequest {
    fn new(speaker: &str, utterance: u32, condition: Condition) -> Self {
        Self {
            speaker: speaker.to_owned(),
            utterance,
            condition,
        }
    }

    /// The key `plaudon-eval` looks a vector up by.
    fn token(&self) -> String {
        format!("{}/{}/{}", self.speaker, self.utterance, self.condition)
    }

    fn relative_path(&self) -> String {
        format!(
            "{}/{}/{}.wav",
            self.speaker,
            self.utterance,
            self.condition.as_str()
        )
    }
}

// --------------------------------------------------------------- fixtures

/// Provenance of one built fixture file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FixtureRecord {
    token: String,
    path: String,
    sha256: String,
    duration_ms: u32,
    sources: Vec<String>,
}

/// The document `build-fixtures` writes beside the audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FixtureManifestOut {
    schema_version: u32,
    corpus: String,
    corpus_licence: String,
    attribution: String,
    sample_rate_hz: u32,
    recipe: BTreeMap<String, String>,
    speaker_assignment: BTreeMap<String, String>,
    fixtures: Vec<FixtureRecord>,
}

fn build_fixtures(arguments: &[String]) -> Result<(), String> {
    let librispeech = PathBuf::from(required(arguments, "--librispeech")?);
    let plan_path = PathBuf::from(required(arguments, "--plan")?);
    let out = PathBuf::from(required(arguments, "--out")?);
    let ffmpeg = value(arguments, "--ffmpeg").unwrap_or("ffmpeg").to_owned();

    let plan = PlanView::read(&plan_path)?;
    let readers = librispeech_readers(&librispeech)?;
    let wanted = plan.speakers();
    if readers.len() < wanted.len() {
        return Err(format!(
            "the plan needs {} speakers, {} has {}",
            wanted.len(),
            librispeech.display(),
            readers.len()
        ));
    }

    let mut assignment = BTreeMap::new();
    for (label, reader) in wanted.iter().zip(readers.iter()) {
        assignment.insert(label.clone(), reader.id.clone());
    }

    let utterance_samples =
        (u64::from(plan.utterance_ms) * u64::from(MODEL_SAMPLE_RATE_HZ) / 1_000) as usize;
    let ctx = JobContext::new(JobId::new());
    let mut records = Vec::new();

    for (label, reader) in wanted.iter().zip(readers.iter()) {
        let needed = if plan.roster.contains(label) {
            plan.utterances_per_speaker()
        } else {
            1
        };
        let (utterances, sources) =
            cut_utterances(reader, needed as usize, utterance_samples, &ctx)
                .map_err(|error| format!("speaker {label} ({}): {error}", reader.id))?;

        for (index, samples) in utterances.iter().enumerate() {
            let utterance = index as u32;
            let is_trial = utterance >= plan.enrol_utterances;
            let conditions: Vec<Condition> = if is_trial {
                plan.conditions.clone()
            } else {
                vec![Condition::Clean]
            };
            for condition in conditions {
                let request = SegmentRequest::new(label, utterance, condition);
                let processed = apply_condition(samples, condition, &out, &ffmpeg)?;
                let path = out.join(request.relative_path());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
                }
                write_wav(&path, &processed)?;
                records.push(FixtureRecord {
                    token: request.token(),
                    path: request.relative_path(),
                    sha256: file_digest(&path)?,
                    duration_ms: plan.utterance_ms,
                    sources: sources[index].clone(),
                });
            }
        }
    }

    let mut recipe = BTreeMap::new();
    recipe.insert(
        "clean".to_owned(),
        "LibriSpeech test-clean FLAC, decoded to 16 kHz mono by plaudon-audio, \
         consecutive utterances of one reader concatenated and trimmed to the plan's \
         utterance length"
            .to_owned(),
    );
    recipe.insert(
        "telephone_bandpass".to_owned(),
        "two-pole RBJ high-pass at 300 Hz followed by a two-pole RBJ low-pass at 3400 Hz, \
         Q = 0.7071, applied in this tool"
            .to_owned(),
    );
    recipe.insert(
        "room_impulse_response".to_owned(),
        "image-source impulse response of a 5.0 x 4.0 x 2.7 m room, wall reflection 0.6, \
         source (1.2, 1.0, 1.5) m, receiver (3.4, 2.6, 1.2) m, 12 image orders, \
         convolved and peak-normalised in this tool"
            .to_owned(),
    );
    recipe.insert(
        "opus_low_bitrate".to_owned(),
        format!(
            "{ffmpeg} -i <wav> -c:a libopus -b:a 12k -ar 16000 -ac 1 <ogg>, decoded back \
                 to 16 kHz mono PCM by the same binary"
        ),
    );

    let manifest = FixtureManifestOut {
        schema_version: 1,
        corpus: "LibriSpeech test-clean".to_owned(),
        corpus_licence: "CC-BY-4.0".to_owned(),
        attribution: "LibriSpeech (Panayotov et al., openslr.org/12), CC-BY-4.0, derived from \
                      public-domain LibriVox recordings; used as test material only"
            .to_owned(),
        sample_rate_hz: MODEL_SAMPLE_RATE_HZ,
        recipe,
        speaker_assignment: assignment,
        fixtures: records,
    };
    let path = out.join("fixtures.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("serialising the manifest: {error}"))?,
    )
    .map_err(|error| format!("writing {}: {error}", path.display()))?;

    println!(
        "built {} fixture file(s) for {} speaker(s) under {}",
        manifest.fixtures.len(),
        manifest.speaker_assignment.len(),
        out.display()
    );
    Ok(())
}

/// One LibriSpeech reader and their files, in corpus order.
#[derive(Debug, Clone)]
struct Reader {
    id: String,
    files: Vec<PathBuf>,
}

fn librispeech_readers(root: &Path) -> Result<Vec<Reader>, String> {
    let base = if root.join("LibriSpeech/test-clean").is_dir() {
        root.join("LibriSpeech/test-clean")
    } else if root.join("test-clean").is_dir() {
        root.join("test-clean")
    } else {
        root.to_path_buf()
    };

    let mut readers = Vec::new();
    let entries =
        std::fs::read_dir(&base).map_err(|error| format!("reading {}: {error}", base.display()))?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if !id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let mut files = Vec::new();
        collect_flac(&entry.path(), &mut files);
        files.sort();
        if !files.is_empty() {
            readers.push(Reader { id, files });
        }
    }
    readers.sort_by(|left, right| {
        left.id
            .parse::<u64>()
            .unwrap_or(u64::MAX)
            .cmp(&right.id.parse::<u64>().unwrap_or(u64::MAX))
    });
    if readers.is_empty() {
        return Err(format!("no LibriSpeech readers under {}", base.display()));
    }
    Ok(readers)
}

fn collect_flac(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_flac(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "flac") {
            out.push(path);
        }
    }
}

/// Fixed-length utterances and, per utterance, the corpus files behind it.
type CutUtterances = (Vec<Vec<f32>>, Vec<Vec<String>>);

/// Cut `count` utterances of exactly `samples` samples from one reader.
///
/// Material is consumed in corpus order and never reused, so an enrolment
/// utterance and a trial utterance are different speech from the same reader —
/// which is the whole point of a verification trial.
fn cut_utterances(
    reader: &Reader,
    count: usize,
    samples: usize,
    ctx: &JobContext,
) -> Result<CutUtterances, String> {
    let mut utterances = Vec::with_capacity(count);
    let mut provenance = Vec::with_capacity(count);
    let mut current: Vec<f32> = Vec::with_capacity(samples);
    let mut sources: Vec<String> = Vec::new();

    for file in &reader.files {
        if utterances.len() == count {
            break;
        }
        let decoded = decode_mono_16k(file, ctx)?;
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut offset = 0usize;
        while offset < decoded.len() && utterances.len() < count {
            let take = (samples - current.len()).min(decoded.len() - offset);
            current.extend_from_slice(&decoded[offset..offset + take]);
            if !sources.contains(&name) {
                sources.push(name.clone());
            }
            offset += take;
            if current.len() == samples {
                utterances.push(std::mem::take(&mut current));
                provenance.push(std::mem::take(&mut sources));
                current = Vec::with_capacity(samples);
            }
        }
    }

    if utterances.len() < count {
        return Err(format!(
            "only {} of {count} utterance(s) could be cut from {} file(s)",
            utterances.len(),
            reader.files.len()
        ));
    }
    Ok((utterances, provenance))
}

fn decode_mono_16k(path: &Path, ctx: &JobContext) -> Result<Vec<f32>, String> {
    let media = validate_media(path, &ValidationRequest::manual_import())
        .map_err(|error| format!("validating {}: {error}", path.display()))?;
    let mut sink = MemoryPcmSink::default();
    decode_to_canonical_pcm(
        &media,
        &CanonicalPcmProfile::whisper_mono_16k(),
        &mut sink as &mut dyn PcmSink,
        ctx,
    )
    .map_err(|error| format!("decoding {}: {error}", path.display()))?;
    Ok(sink
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

// ------------------------------------------------------------- conditions

fn apply_condition(
    samples: &[f32],
    condition: Condition,
    scratch: &Path,
    ffmpeg: &str,
) -> Result<Vec<f32>, String> {
    match condition {
        Condition::Clean => Ok(samples.to_vec()),
        Condition::TelephoneBandpass => Ok(bandpass(samples)),
        Condition::RoomImpulseResponse => Ok(reverberate(samples)),
        Condition::OpusLowBitrate => opus_round_trip(samples, scratch, ffmpeg),
        Condition::NoiseMusan => Err(
            "the MUSAN condition needs the MUSAN corpus, which this tool does not fetch; \
             remove `noise_musan` from the plan's conditions or build it separately"
                .to_owned(),
        ),
    }
}

/// One RBJ biquad.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn high_pass(frequency: f64, rate: f64, q: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * frequency / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: f64::midpoint(1.0, cos) / a0,
            b1: (-(1.0 + cos)) / a0,
            b2: f64::midpoint(1.0, cos) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn low_pass(frequency: f64, rate: f64, q: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * frequency / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: f64::midpoint(1.0, -cos) / a0,
            b1: (1.0 - cos) / a0,
            b2: f64::midpoint(1.0, -cos) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn apply(self, samples: &mut [f32]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for sample in samples.iter_mut() {
            let x0 = f64::from(*sample);
            let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;
            *sample = y0 as f32;
        }
    }
}

fn bandpass(samples: &[f32]) -> Vec<f32> {
    let rate = f64::from(MODEL_SAMPLE_RATE_HZ);
    let q = std::f64::consts::FRAC_1_SQRT_2;
    let mut out = samples.to_vec();
    Biquad::high_pass(300.0, rate, q).apply(&mut out);
    Biquad::low_pass(3_400.0, rate, q).apply(&mut out);
    out
}

/// A deterministic shoebox impulse response, image-source method.
fn room_impulse_response() -> Vec<f32> {
    let rate = f64::from(MODEL_SAMPLE_RATE_HZ);
    let speed = 343.0;
    let room = [5.0f64, 4.0, 2.7];
    let source = [1.2f64, 1.0, 1.5];
    let receiver = [3.4f64, 2.6, 1.2];
    let reflection = 0.6f64;
    let orders = 12i32;

    let mut response = vec![0.0f32; (rate * 0.4) as usize];
    // Image-source method: each axis is mirrored independently, so one image
    // is a choice of mirror per axis plus a translation by whole room widths.
    for mirror in 0..8u8 {
        for nx in -orders..=orders {
            for ny in -orders..=orders {
                for nz in -orders..=orders {
                    let translation = [nx, ny, nz];
                    let mut position = [0.0f64; 3];
                    let mut reflections = 0i32;
                    for axis in 0..3 {
                        let mirrored = mirror & (1 << axis) != 0;
                        let base = if mirrored {
                            -source[axis]
                        } else {
                            source[axis]
                        };
                        position[axis] = base + 2.0 * f64::from(translation[axis]) * room[axis];
                        reflections += translation[axis].abs() + i32::from(mirrored);
                    }
                    let distance = (0..3)
                        .map(|axis| (position[axis] - receiver[axis]).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    let delay = (distance / speed * rate).round() as usize;
                    if delay >= response.len() || distance <= 0.0 {
                        continue;
                    }
                    let gain = reflection.powi(reflections) / distance;
                    response[delay] += gain as f32;
                }
            }
        }
    }
    response
}

fn reverberate(samples: &[f32]) -> Vec<f32> {
    let response = room_impulse_response();
    let mut out = vec![0.0f32; samples.len()];
    for (index, slot) in out.iter_mut().enumerate() {
        let mut sum = 0.0f64;
        let taps = response.len().min(index + 1);
        for tap in 0..taps {
            sum += f64::from(response[tap]) * f64::from(samples[index - tap]);
        }
        *slot = sum as f32;
    }
    let peak = out.iter().fold(0.0f32, |peak, value| peak.max(value.abs()));
    let source_peak = samples
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    if peak > 0.0 {
        let scale = source_peak / peak;
        for value in &mut out {
            *value *= scale;
        }
    }
    out
}

fn opus_round_trip(samples: &[f32], scratch: &Path, ffmpeg: &str) -> Result<Vec<f32>, String> {
    std::fs::create_dir_all(scratch)
        .map_err(|error| format!("creating {}: {error}", scratch.display()))?;
    let wav = scratch.join(".opus-source.wav");
    let ogg = scratch.join(".opus-encoded.ogg");
    let back = scratch.join(".opus-decoded.wav");
    write_wav(&wav, samples)?;

    run(
        ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            &wav.to_string_lossy(),
            "-c:a",
            "libopus",
            "-b:a",
            "12k",
            "-ar",
            "16000",
            "-ac",
            "1",
            &ogg.to_string_lossy(),
        ],
    )?;
    run(
        ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            &ogg.to_string_lossy(),
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            &back.to_string_lossy(),
        ],
    )?;

    let decoded = read_wav(&back)?;
    let _ = std::fs::remove_file(&wav);
    let _ = std::fs::remove_file(&ogg);
    let _ = std::fs::remove_file(&back);

    // Opus adds algorithmic delay; the harness compares fixed-length windows,
    // so the round trip is trimmed back to the source length.
    let mut out = decoded;
    out.truncate(samples.len());
    out.resize(samples.len(), 0.0);
    Ok(out)
}

fn run(program: &str, arguments: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("running {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------- WAV io

fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&MODEL_SAMPLE_RATE_HZ.to_le_bytes());
    bytes.extend_from_slice(&(MODEL_SAMPLE_RATE_HZ * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let clamped = (sample * 32_767.0).clamp(-32_768.0, 32_767.0) as i16;
        bytes.extend_from_slice(&clamped.to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|error| format!("writing {}: {error}", path.display()))
}

fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let body = offset + 8;
        if id == b"data" {
            let end = (body + size).min(bytes.len());
            return Ok(bytes[body..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| f32::from(i16::from_le_bytes(*chunk)) / 32_768.0)
                .collect());
        }
        offset = body + size + (size % 2);
    }
    Err(format!("{} carries no data chunk", path.display()))
}

fn file_digest(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

// ----------------------------------------------------------------- embed

/// The runtime figures ADR-0081 §4 requires, as measured by this run.
#[derive(Debug, Clone, Serialize)]
struct RuntimeStats {
    model_id: String,
    model_version: String,
    model_sha256: String,
    intra_op_threads: usize,
    startup_ms: u64,
    segments: usize,
    audio_seconds: f64,
    embed_seconds: f64,
    pipeline_seconds: f64,
    real_time_factor: f64,
    pipeline_real_time_factor: f64,
    mean_embed_ms: f64,
    max_embed_ms: f64,
}

fn embed(arguments: &[String]) -> Result<(), String> {
    let model_path = PathBuf::from(required(arguments, "--model")?);
    let fixtures = PathBuf::from(required(arguments, "--fixtures")?);
    let plan_path = PathBuf::from(required(arguments, "--plan")?);
    let out = PathBuf::from(required(arguments, "--out")?);
    let stats_path = value(arguments, "--stats").map(PathBuf::from);
    let threads: usize = match value(arguments, "--threads") {
        Some(raw) => raw
            .parse()
            .map_err(|_| format!("--threads expects a number, got `{raw}`"))?,
        None => 1,
    };

    let plan = PlanView::read(&plan_path)?;
    let artifact = artifact_for(&model_path)?;
    let model = SpeakerEmbeddingModel::open(
        &model_path,
        artifact,
        EngineOptions {
            intra_op_threads: threads,
        },
    )
    .map_err(|error| format!("opening {}: {error}", model_path.display()))?;
    let startup_ms = model.startup().as_millis() as u64;

    let ctx = JobContext::new(JobId::new());
    let segments = plan.segments();
    let mut vectors = BTreeMap::new();
    let mut audio_samples = 0usize;
    let mut embed_seconds = 0.0f64;
    let mut max_embed_ms = 0.0f64;
    let pipeline_started = Instant::now();

    for segment in &segments {
        let path = fixtures.join(segment.relative_path());
        let samples = decode_mono_16k(&path, &ctx)?;
        audio_samples += samples.len();

        let started = Instant::now();
        let vector = model
            .embed_samples(&samples)
            .map_err(|error| format!("embedding {}: {error}", segment.token()))?;
        let elapsed = started.elapsed().as_secs_f64();
        embed_seconds += elapsed;
        max_embed_ms = max_embed_ms.max(elapsed * 1_000.0);

        vectors.insert(segment.token(), vector);
    }
    let pipeline_seconds = pipeline_started.elapsed().as_secs_f64();

    let file = EmbeddingFile {
        schema_version: EMBEDDING_FILE_SCHEMA_VERSION,
        model: model.binding().clone(),
        dim: model.dim(),
        vectors,
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    std::fs::write(
        &out,
        serde_json::to_string(&file).map_err(|error| format!("serialising embeddings: {error}"))?,
    )
    .map_err(|error| format!("writing {}: {error}", out.display()))?;

    let audio_seconds = audio_samples as f64 / f64::from(MODEL_SAMPLE_RATE_HZ);
    let stats = RuntimeStats {
        model_id: model.binding().model_id.clone(),
        model_version: model.binding().model_version.clone(),
        model_sha256: model.binding().model_sha256.to_hex(),
        intra_op_threads: threads,
        startup_ms,
        segments: segments.len(),
        audio_seconds,
        embed_seconds,
        pipeline_seconds,
        real_time_factor: embed_seconds / audio_seconds,
        pipeline_real_time_factor: pipeline_seconds / audio_seconds,
        mean_embed_ms: embed_seconds * 1_000.0 / segments.len() as f64,
        max_embed_ms,
    };
    let rendered = serde_json::to_string_pretty(&stats)
        .map_err(|error| format!("serialising the runtime stats: {error}"))?;
    if let Some(path) = stats_path {
        std::fs::write(&path, format!("{rendered}\n"))
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
    }
    println!("{rendered}");
    Ok(())
}

/// Which reviewed artifact a file claims to be, by file name.
///
/// By name and then by digest, never by digest alone: naming the artifact is
/// what makes the failure message say *which* model was expected.
fn artifact_for(path: &Path) -> Result<&'static ModelArtifact, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    BUNDLE_TIER
        .iter()
        .find(|artifact| artifact.file_name == name || artifact.source_file_name == name)
        .ok_or_else(|| {
            format!(
                "`{name}` is not a reviewed artifact; expected one of: {}",
                BUNDLE_TIER
                    .iter()
                    .map(|artifact| artifact.file_name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}
