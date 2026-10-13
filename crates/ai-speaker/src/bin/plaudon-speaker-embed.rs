//! The ADR-0081 §4 measurement bridge: audio in, harness input out.
//!
//! ```text
//! plaudon-speaker-embed build-fixtures --librispeech <dir> --plan <plan.json> --out <dir>
//!                                      [--musan <dir>] [--musan-snr-db <db>] [--ffmpeg <path>]
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
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_diarization::contract::SpeakerTurn;
use plaudon_ai_diarization::der::{diarization_error_rate_by_overlap, DerOptions, ReferenceTurn};
use plaudon_ai_diarization::report::{
    open_conditions, ConditionPool, CorpusRecord, DerArtifact, ModelRecord, ParameterRecord,
    PooledDer, ProviderRecord, ReleaseEvidence, RuntimeRecord, SessionMeasurement,
    DIARIZATION_DER_REPORT_SCHEMA_VERSION,
};
use plaudon_ai_speaker::checkpoint::{ModelArtifact, BUNDLE_TIER, MODEL_SAMPLE_RATE_HZ};
use plaudon_ai_speaker::diarize::{
    DiarizationOptions, MemorySamples, OnnxDiarizationProvider, PROVIDER_ID,
};
use plaudon_ai_speaker::onnx::{EngineOptions, SpeakerEmbeddingModel};
use plaudon_ai_speaker::segmentation::SegmentationModel;
use plaudon_audio::decode::{decode_to_canonical_pcm, CanonicalPcmProfile, MemoryPcmSink, PcmSink};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::ids::JobId;
use plaudon_eval::speaker::embeddings::{EmbeddingFile, EMBEDDING_FILE_SCHEMA_VERSION};
use plaudon_eval::speaker::fixtures::{Condition, SessionPlan, SyntheticSession};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const USAGE: &str = "\
plaudon-speaker-embed build-fixtures --librispeech <dir> --plan <plan.json> --out <dir>
                                     [--musan <dir>] [--musan-snr-db <db>] [--ffmpeg <path>]
plaudon-speaker-embed embed --model <file.onnx> --fixtures <dir> --plan <plan.json>
                            --out <eval-embeddings.json> [--stats <file.json>] [--threads <n>]
plaudon-speaker-embed build-session --librispeech <dir> --plan <plan.json>
                                    --sessions <session-plans.json> --out <dir>
                                    [--conditions clean,room_impulse_response]
                                    [--musan <dir>] [--musan-snr-db <db>] [--ffmpeg <path>]
plaudon-speaker-embed diarize --segmentation <file.onnx> --embedding <file.onnx>
                              --sessions <dir> --out <der.json> --peak-rss-bytes <n>
                              [--threads <n>] [--cluster-threshold <f>]
                              [--window-step-frames <n>] [--min-track-ms <n>]
                              [--threshold-provenance <text>] [--commit <sha>]
                              [--measured-on <rfc3339>] [--hypothesis-rttm <dir>]";

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = arguments.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let result = match command {
        "build-fixtures" => build_fixtures(&arguments[1..]),
        "embed" => embed(&arguments[1..]),
        "build-session" => build_session(&arguments[1..]),
        "diarize" => diarize(&arguments[1..]),
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

/// The MUSAN noise file one fixture was mixed with.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NoiseFileRecord {
    path: String,
    sha256: String,
    duration_ms: u32,
}

/// Everything the noise condition needs to be reproducible from the corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NoiseProvenance {
    corpus: String,
    corpus_licence: String,
    attribution: String,
    subset: String,
    snr_db: f64,
    selection: String,
    files: Vec<NoiseFileRecord>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    noise: Option<NoiseProvenance>,
    speaker_assignment: BTreeMap<String, String>,
    fixtures: Vec<FixtureRecord>,
}

fn build_fixtures(arguments: &[String]) -> Result<(), String> {
    let librispeech = PathBuf::from(required(arguments, "--librispeech")?);
    let plan_path = PathBuf::from(required(arguments, "--plan")?);
    let out = PathBuf::from(required(arguments, "--out")?);
    let ffmpeg = value(arguments, "--ffmpeg").unwrap_or("ffmpeg").to_owned();
    let snr_db: f64 = match value(arguments, "--musan-snr-db") {
        Some(raw) => raw
            .parse()
            .map_err(|_| format!("--musan-snr-db expects a number, got `{raw}`"))?,
        None => DEFAULT_MUSAN_SNR_DB,
    };
    let musan = value(arguments, "--musan").map(PathBuf::from);

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
    let mut noise_pool = match (plan.conditions.contains(&Condition::NoiseMusan), &musan) {
        (true, Some(dir)) => Some(MusanPool::open(dir, snr_db)?),
        (true, None) => {
            return Err(
                "the plan asks for `noise_musan`: pass --musan <dir with MUSAN> \
                        (openslr.org/17), or drop the condition from the plan"
                    .to_owned(),
            )
        }
        (false, _) => None,
    };

    for (label, reader) in wanted.iter().zip(readers.iter()) {
        let needed = if plan.roster.contains(label) {
            plan.utterances_per_speaker()
        } else {
            1
        };
        let (utterances, sources) =
            cut_utterances(reader, needed as usize, utterance_samples, false, &ctx)
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
                let mut sources = sources[index].clone();
                let processed = apply_condition(
                    samples,
                    condition,
                    ConditionContext {
                        scratch: &out,
                        ffmpeg: &ffmpeg,
                        noise: noise_pool.as_mut(),
                        token: &request.token(),
                        job: &ctx,
                    },
                    &mut sources,
                )?;
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
                    sources,
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
    if let Some(pool) = &noise_pool {
        recipe.insert("noise_musan".to_owned(), pool.recipe());
    }

    let manifest = FixtureManifestOut {
        schema_version: 1,
        corpus: "LibriSpeech test-clean".to_owned(),
        corpus_licence: "CC-BY-4.0".to_owned(),
        attribution: "LibriSpeech (Panayotov et al., openslr.org/12), CC-BY-4.0, derived from \
                      public-domain LibriVox recordings; used as test material only"
            .to_owned(),
        sample_rate_hz: MODEL_SAMPLE_RATE_HZ,
        recipe,
        noise: noise_pool.as_ref().map(MusanPool::provenance),
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
        collect_extension(&entry.path(), "flac", &mut files);
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

fn collect_extension(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_extension(&path, extension, out);
        } else if path.extension().is_some_and(|ext| ext == extension) {
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
///
/// `trim_silence` removes the pauses inside the corpus material first. The
/// speaker-matching fixtures do not want that — a window of read speech with
/// its natural pauses is what the product embeds — but a diarization session
/// does: its ground truth says "this speaker is active from here to here", and
/// a fifth of a 4-second turn that is actually a pause would be scored as
/// speech the provider missed, which measures the fixture rather than the
/// provider.
fn cut_utterances(
    reader: &Reader,
    count: usize,
    samples: usize,
    trim_silence: bool,
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
        let decoded = if trim_silence {
            speech_only(&decoded)
        } else {
            decoded
        };
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

/// Analysis frame of the silence gate, in samples: 20 ms at 16 kHz.
const SILENCE_FRAME_SAMPLES: usize = 320;

/// Cross-fade applied where two kept runs are spliced, in samples: 5 ms.
const SILENCE_SPLICE_SAMPLES: usize = 80;

/// Drop the pauses from one decoded file, leaving continuous speech.
///
/// An energy gate at a fraction of the file's own loud level, so a quiet
/// reader is not deleted and a noisy one is not kept whole; runs of kept
/// frames are spliced with a short cross-fade, because a hard splice is a
/// click and a click is an onset a segmentation model is entitled to believe.
fn speech_only(samples: &[f32]) -> Vec<f32> {
    let frames: Vec<f32> = samples
        .chunks(SILENCE_FRAME_SAMPLES)
        .map(|frame| {
            let energy: f64 = frame.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
            (energy / frame.len() as f64).sqrt() as f32
        })
        .collect();
    let mut sorted = frames.clone();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted
        .get(sorted.len().saturating_mul(9) / 10)
        .copied()
        .unwrap_or_default();
    let threshold = (loud * 0.1).max(0.002);

    let mut out: Vec<f32> = Vec::with_capacity(samples.len());
    let mut run: Vec<f32> = Vec::new();
    let flush = |run: &mut Vec<f32>, out: &mut Vec<f32>| {
        if run.is_empty() {
            return;
        }
        let fade = SILENCE_SPLICE_SAMPLES.min(run.len()).min(out.len());
        for (index, sample) in run.drain(..).enumerate() {
            if index < fade {
                let weight = (index + 1) as f32 / (fade + 1) as f32;
                let tail = out.len() - fade + index;
                out[tail] = out[tail] * (1.0 - weight) + sample * weight;
            } else {
                out.push(sample);
            }
        }
    };
    for (index, frame) in samples.chunks(SILENCE_FRAME_SAMPLES).enumerate() {
        if frames[index] >= threshold {
            run.extend_from_slice(frame);
        } else {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);
    out
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

/// Everything a condition may need besides the samples themselves.
struct ConditionContext<'a> {
    scratch: &'a Path,
    ffmpeg: &'a str,
    noise: Option<&'a mut MusanPool>,
    token: &'a str,
    job: &'a JobContext,
}

fn apply_condition(
    samples: &[f32],
    condition: Condition,
    cx: ConditionContext<'_>,
    sources: &mut Vec<String>,
) -> Result<Vec<f32>, String> {
    match condition {
        Condition::Clean => Ok(samples.to_vec()),
        Condition::TelephoneBandpass => Ok(bandpass(samples)),
        Condition::RoomImpulseResponse => Ok(reverberate(samples)),
        Condition::OpusLowBitrate => opus_round_trip(samples, cx.scratch, cx.ffmpeg),
        Condition::NoiseMusan => {
            let pool = cx.noise.ok_or_else(|| {
                "the MUSAN condition needs the MUSAN corpus: pass --musan <dir>".to_owned()
            })?;
            let (mixed, source) = pool.mix(cx.token, samples, cx.job)?;
            sources.push(source);
            Ok(mixed)
        }
    }
}

// ------------------------------------------------------------------ MUSAN

/// Signal-to-noise ratio of the `noise_musan` condition, in dB.
///
/// 10 dB is audible noise that still leaves the speech intelligible — the
/// band a laptop microphone in a café or an open-plan office actually sits
/// in. The figure is declared in the fixture manifest, because an SNR that is
/// not stated makes the condition's error rate meaningless.
const DEFAULT_MUSAN_SNR_DB: f64 = 10.0;

/// The MUSAN `noise` subset, and the assignment of its files to fixtures.
///
/// Selection is derived from the fixture token alone — digest in, index out —
/// so the same plan and the same corpus produce the same mixtures on any
/// machine, in any build order, and the manifest can name the file behind
/// every mixed fixture.
struct MusanPool {
    root: PathBuf,
    files: Vec<String>,
    snr_db: f64,
    decoded: BTreeMap<String, Vec<f32>>,
    used: BTreeMap<String, NoiseFileRecord>,
}

impl MusanPool {
    fn open(dir: &Path, snr_db: f64) -> Result<Self, String> {
        if !snr_db.is_finite() {
            return Err(format!("--musan-snr-db {snr_db} is not a number"));
        }
        let root = if dir.join("musan/noise").is_dir() {
            dir.join("musan/noise")
        } else if dir.join("noise").is_dir() {
            dir.join("noise")
        } else {
            dir.to_path_buf()
        };
        let mut absolute = Vec::new();
        collect_extension(&root, "wav", &mut absolute);
        let mut files: Vec<String> = absolute
            .iter()
            .filter_map(|path| path.strip_prefix(&root).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(format!(
                "no MUSAN noise wavs under {}; expected the `noise` subset of \
                 musan.tar.gz (openslr.org/17)",
                root.display()
            ));
        }
        Ok(Self {
            root,
            files,
            snr_db,
            decoded: BTreeMap::new(),
            used: BTreeMap::new(),
        })
    }

    /// Mix one fixture with its noise file, returning the provenance line.
    fn mix(
        &mut self,
        token: &str,
        samples: &[f32],
        ctx: &JobContext,
    ) -> Result<(Vec<f32>, String), String> {
        let digest = Sha256::digest(token.as_bytes());
        let pick = u64::from_be_bytes(digest[0..8].try_into().expect("8 bytes"));
        let seek = u64::from_be_bytes(digest[8..16].try_into().expect("8 bytes"));
        let name = self.files[(pick % self.files.len() as u64) as usize].clone();
        let noise = self.noise_samples(&name, ctx)?;

        let mut window = Vec::with_capacity(samples.len());
        let start = if noise.len() > samples.len() {
            (seek % (noise.len() - samples.len() + 1) as u64) as usize
        } else {
            0
        };
        // Shorter noise files are tiled, which is what MUSAN's own recipe
        // does: the subset holds clips from a few seconds upwards.
        let mut cursor = start;
        while window.len() < samples.len() {
            let take = (samples.len() - window.len()).min(noise.len() - cursor);
            window.extend_from_slice(&noise[cursor..cursor + take]);
            cursor = (cursor + take) % noise.len();
        }

        let speech_rms = rms(samples);
        let noise_rms = rms(&window);
        if noise_rms <= f64::from(f32::EPSILON) {
            return Err(format!("MUSAN file {name} is silent"));
        }
        let gain = speech_rms / noise_rms * 10.0f64.powf(-self.snr_db / 20.0);
        let mut mixed: Vec<f32> = samples
            .iter()
            .zip(window.iter())
            .map(|(speech, noise)| (f64::from(*speech) + gain * f64::from(*noise)) as f32)
            .collect();
        // Digital clipping would be a second, undeclared condition; the sum is
        // scaled back as a whole instead, which leaves the SNR untouched.
        let peak = mixed
            .iter()
            .fold(0.0f32, |peak, value| peak.max(value.abs()));
        if peak > 1.0 {
            for value in &mut mixed {
                *value /= peak;
            }
        }
        Ok((mixed, format!("musan:noise/{name}")))
    }

    fn noise_samples(&mut self, name: &str, ctx: &JobContext) -> Result<&Vec<f32>, String> {
        if !self.decoded.contains_key(name) {
            let path = self.root.join(name);
            let samples = decode_mono_16k(&path, ctx)?;
            if samples.is_empty() {
                return Err(format!("{} decoded to nothing", path.display()));
            }
            self.used.insert(
                name.to_owned(),
                NoiseFileRecord {
                    path: format!("noise/{name}"),
                    sha256: file_digest(&path)?,
                    duration_ms: (samples.len() as u64 * 1_000 / u64::from(MODEL_SAMPLE_RATE_HZ))
                        as u32,
                },
            );
            self.decoded.insert(name.to_owned(), samples);
        }
        Ok(&self.decoded[name])
    }

    fn recipe(&self) -> String {
        format!(
            "the clean fixture summed with one file of the MUSAN `noise` subset at {:.1} dB SNR, \
             the noise scaled by the ratio of the two RMS levels and the sum divided by its peak \
             only if it would clip; the file and the offset come from the sha256 of the fixture \
             token, so the mixture is reproducible from the corpus",
            self.snr_db
        )
    }

    fn provenance(&self) -> NoiseProvenance {
        NoiseProvenance {
            corpus: "MUSAN".to_owned(),
            corpus_licence: "CC-BY-4.0".to_owned(),
            attribution: "MUSAN (Snyder, Chen and Povey, openslr.org/17), CC-BY-4.0; \
                          used as test material only, never redistributed"
                .to_owned(),
            subset: "noise".to_owned(),
            snr_db: self.snr_db,
            selection: "file index = sha256(token)[0..8] mod |files|, start offset = \
                        sha256(token)[8..16] mod (len - window + 1), over the sorted \
                        relative paths of every wav under musan/noise"
                .to_owned(),
            files: self.used.values().cloned().collect(),
        }
    }
}

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|value| f64::from(*value).powi(2)).sum();
    (sum / samples.len() as f64).sqrt()
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

// --------------------------------------------------------------- sessions

/// The document that holds the session timelines a diarization run measures.
///
/// A timeline, not audio: [`SessionPlan::synthesize`] lays the speakers out
/// deterministically, so the RTTM is exact by construction rather than
/// annotated, and the only thing this tool adds is the mix a provider needs as
/// an input.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionPlanFile {
    schema_version: u32,
    /// Why these timelines exist, carried in the document itself.
    notes: String,
    sessions: Vec<SessionPlan>,
}

/// Level each utterance is scaled to before it is mixed.
///
/// Readers differ by more than 10 dB in LibriSpeech, and an unnormalised mix
/// would measure "can the segmentation model hear the quiet one through the
/// loud one" rather than diarization.
const MIX_TARGET_RMS: f32 = 0.08;

/// Fade at each utterance edge, in samples: 10 ms at 16 kHz.
///
/// A hard cut into speech is a click, and a click is an onset the segmentation
/// model is entitled to call speech. The fade is part of the recipe and is
/// recorded with it.
const MIX_FADE_SAMPLES: usize = 160;

/// Peak the finished mix is limited to, so overlapping turns cannot clip.
const MIX_PEAK_CEILING: f32 = 0.99;

/// One built session file and its exact ground truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    session_id: String,
    condition: String,
    path: String,
    rttm_path: String,
    sha256: String,
    duration_ms: u64,
    speakers: Vec<String>,
    /// Reference speech, counting overlap once per simultaneous speaker.
    speech_ms: u64,
    /// Milliseconds during which two speakers are active at once.
    overlap_ms: u64,
    turns: Vec<ReferenceTurn>,
}

/// The document `build-session` writes beside the mixes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionManifestOut {
    schema_version: u32,
    corpus: String,
    corpus_licence: String,
    attribution: String,
    sample_rate_hz: u32,
    notes: String,
    recipe: BTreeMap<String, String>,
    speaker_assignment: BTreeMap<String, String>,
    sessions: Vec<SessionRecord>,
}

fn build_session(arguments: &[String]) -> Result<(), String> {
    let librispeech = PathBuf::from(required(arguments, "--librispeech")?);
    let plan_path = PathBuf::from(required(arguments, "--plan")?);
    let sessions_path = PathBuf::from(required(arguments, "--sessions")?);
    let out = PathBuf::from(required(arguments, "--out")?);
    let ffmpeg = value(arguments, "--ffmpeg").unwrap_or("ffmpeg").to_owned();
    let conditions = conditions_argument(arguments)?;
    let snr_db: f64 = match value(arguments, "--musan-snr-db") {
        Some(raw) => raw
            .parse()
            .map_err(|_| format!("--musan-snr-db expects a number, got `{raw}`"))?,
        None => DEFAULT_MUSAN_SNR_DB,
    };
    let mut noise_pool = match (
        conditions.contains(&Condition::NoiseMusan),
        value(arguments, "--musan"),
    ) {
        (true, Some(dir)) => Some(MusanPool::open(Path::new(dir), snr_db)?),
        (true, None) => {
            return Err(
                "`noise_musan` needs the MUSAN corpus: pass --musan <dir> (openslr.org/17), \
                 or drop the condition"
                    .to_owned(),
            )
        }
        (false, _) => None,
    };

    let plan = PlanView::read(&plan_path)?;
    let text = std::fs::read_to_string(&sessions_path)
        .map_err(|error| format!("reading {}: {error}", sessions_path.display()))?;
    let document: SessionPlanFile = serde_json::from_str(&text)
        .map_err(|error| format!("parsing {}: {error}", sessions_path.display()))?;
    if document.schema_version != 1 {
        return Err(format!(
            "{} declares schema version {}, this tool writes 1",
            sessions_path.display(),
            document.schema_version
        ));
    }

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
    // The same assignment `build-fixtures` makes, so `roster-01` is the same
    // human in the EER artifact and in this one.
    let mut assignment = BTreeMap::new();
    let mut reader_by_label = BTreeMap::new();
    for (label, reader) in wanted.iter().zip(readers.iter()) {
        assignment.insert(label.clone(), reader.id.clone());
        reader_by_label.insert(label.clone(), reader.clone());
    }

    std::fs::create_dir_all(&out)
        .map_err(|error| format!("creating {}: {error}", out.display()))?;
    let ctx = JobContext::new(JobId::new());
    let mut records = Vec::new();

    for session_plan in &document.sessions {
        let session = session_plan.synthesize();
        let utterance_samples =
            (session_plan.utterance_ms * u64::from(MODEL_SAMPLE_RATE_HZ) / 1_000) as usize;

        let mut material: BTreeMap<String, Vec<Vec<f32>>> = BTreeMap::new();
        for label in &session_plan.speakers {
            let reader = reader_by_label
                .get(label)
                .ok_or_else(|| format!("`{label}` is not a speaker of {}", plan_path.display()))?;
            let (utterances, _sources) = cut_utterances(
                reader,
                session_plan.utterances_per_speaker as usize,
                utterance_samples,
                true,
                &ctx,
            )
            .map_err(|error| format!("speaker {label} ({}): {error}", reader.id))?;
            material.insert(label.clone(), utterances);
        }

        let mix = mix_session(&session, session_plan.utterance_ms, &material)?;
        let rttm_name = format!("{}.rttm", session.session_id);
        std::fs::write(out.join(&rttm_name), session.rttm())
            .map_err(|error| format!("writing {rttm_name}: {error}"))?;

        for condition in &conditions {
            let mut sources = Vec::new();
            let processed = apply_condition(
                &mix,
                *condition,
                ConditionContext {
                    scratch: &out,
                    ffmpeg: &ffmpeg,
                    noise: noise_pool.as_mut(),
                    token: &session.session_id,
                    job: &ctx,
                },
                &mut sources,
            )?;
            let name = format!("{}-{}.wav", session.session_id, condition.as_str());
            let path = out.join(&name);
            write_wav(&path, &processed)?;
            records.push(SessionRecord {
                session_id: session.session_id.clone(),
                condition: condition.as_str().to_owned(),
                path: name,
                rttm_path: rttm_name.clone(),
                sha256: file_digest(&path)?,
                duration_ms: processed.len() as u64 * 1_000 / u64::from(MODEL_SAMPLE_RATE_HZ),
                speakers: session_plan.speakers.clone(),
                speech_ms: session.speech_ms(),
                overlap_ms: session.overlap_ms(),
                turns: session.reference_turns(),
            });
        }
    }

    let mut recipe = BTreeMap::new();
    recipe.insert(
        "mix".to_owned(),
        format!(
            "each speaker's utterances are cut from their LibriSpeech reader in corpus order, \
             scaled to {MIX_TARGET_RMS} RMS, faded in and out over {MIX_FADE_SAMPLES} samples \
             and summed into the session timeline at the plan's offsets; the finished mix is \
             scaled down if its peak exceeds {MIX_PEAK_CEILING}. Between turns the mix is \
             digital silence."
        ),
    );
    recipe.insert(
        "ground_truth".to_owned(),
        "plaudon_eval::speaker::fixtures::SessionPlan::synthesize, written as NIST RTTM beside \
         the audio; the timeline is the ground truth, so no annotation is involved"
            .to_owned(),
    );
    recipe.insert(
        "telephone_bandpass".to_owned(),
        "two-pole RBJ high-pass at 300 Hz followed by a two-pole RBJ low-pass at 3400 Hz, \
         Q = 0.7071, applied to the finished mix"
            .to_owned(),
    );
    recipe.insert(
        "room_impulse_response".to_owned(),
        "image-source impulse response of a 5.0 x 4.0 x 2.7 m room, wall reflection 0.6, \
         12 image orders, convolved with the finished mix and peak-normalised"
            .to_owned(),
    );
    recipe.insert(
        "opus_low_bitrate".to_owned(),
        format!(
            "{ffmpeg} -i <wav> -c:a libopus -b:a 12k -ar 16000 -ac 1 <ogg>, decoded back to \
             16 kHz mono PCM by the same binary"
        ),
    );

    let manifest = SessionManifestOut {
        schema_version: 1,
        corpus: "LibriSpeech test-clean".to_owned(),
        corpus_licence: "CC-BY-4.0".to_owned(),
        attribution: "LibriSpeech (Panayotov et al., openslr.org/12), CC-BY-4.0, derived from \
                      public-domain LibriVox recordings; used as test material only"
            .to_owned(),
        sample_rate_hz: MODEL_SAMPLE_RATE_HZ,
        notes: document.notes,
        recipe,
        speaker_assignment: assignment,
        sessions: records,
    };
    let path = out.join("sessions.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("serialising the manifest: {error}"))?,
    )
    .map_err(|error| format!("writing {}: {error}", path.display()))?;

    println!(
        "built {} session file(s) under {}",
        manifest.sessions.len(),
        out.display()
    );
    Ok(())
}

/// Mix one laid-out session into a single multi-speaker waveform.
fn mix_session(
    session: &SyntheticSession,
    utterance_ms: u64,
    material: &BTreeMap<String, Vec<Vec<f32>>>,
) -> Result<Vec<f32>, String> {
    let rate = u64::from(MODEL_SAMPLE_RATE_HZ);
    let total = (session.end_ms() * rate / 1_000) as usize;
    let mut mix = vec![0.0f32; total];

    for turn in &session.turns {
        let utterances = material
            .get(&turn.speaker)
            .ok_or_else(|| format!("no material cut for `{}`", turn.speaker))?;
        let samples = utterances.get(turn.utterance as usize).ok_or_else(|| {
            format!(
                "`{}` needs utterance {} and only {} were cut",
                turn.speaker,
                turn.utterance,
                utterances.len()
            )
        })?;
        if turn.end_ms - turn.start_ms != utterance_ms {
            return Err(format!(
                "turn {}..{} is not the plan's {utterance_ms} ms utterance",
                turn.start_ms, turn.end_ms
            ));
        }

        let gain = {
            let energy: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
            let rms = (energy / samples.len() as f64).sqrt() as f32;
            if rms > f32::EPSILON {
                MIX_TARGET_RMS / rms
            } else {
                0.0
            }
        };
        let offset = (turn.start_ms * rate / 1_000) as usize;
        for (index, sample) in samples.iter().enumerate() {
            let Some(slot) = mix.get_mut(offset + index) else {
                break;
            };
            let fade = fade_gain(index, samples.len());
            *slot += sample * gain * fade;
        }
    }

    let peak = mix.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    if peak > MIX_PEAK_CEILING {
        let scale = MIX_PEAK_CEILING / peak;
        for sample in &mut mix {
            *sample *= scale;
        }
    }
    Ok(mix)
}

/// Raised-cosine fade at both edges of one utterance.
fn fade_gain(index: usize, length: usize) -> f32 {
    let fade = MIX_FADE_SAMPLES.min(length / 2);
    if fade == 0 {
        return 1.0;
    }
    let position = if index < fade {
        index
    } else if index + fade >= length {
        length - 1 - index
    } else {
        return 1.0;
    };
    0.5 - 0.5 * (std::f32::consts::PI * (position as f32 / fade as f32)).cos()
}

/// Conditions named on the command line, `clean` when none are.
fn conditions_argument(arguments: &[String]) -> Result<Vec<Condition>, String> {
    let raw = value(arguments, "--conditions").unwrap_or("clean");
    raw.split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| {
            serde_json::from_str::<Condition>(&format!("\"{token}\""))
                .map_err(|_| format!("`{token}` is not a known condition"))
        })
        .collect()
}

// ------------------------------------------------------------- diarization

fn diarize(arguments: &[String]) -> Result<(), String> {
    let segmentation_path = PathBuf::from(required(arguments, "--segmentation")?);
    let embedding_path = PathBuf::from(required(arguments, "--embedding")?);
    let sessions_dir = PathBuf::from(required(arguments, "--sessions")?);
    let out = PathBuf::from(required(arguments, "--out")?);
    let peak_rss_bytes: u64 = required(arguments, "--peak-rss-bytes")?
        .parse()
        .map_err(|_| "--peak-rss-bytes expects a number of bytes".to_owned())?;
    let threads: usize = match value(arguments, "--threads") {
        Some(raw) => raw
            .parse()
            .map_err(|_| format!("--threads expects a number, got `{raw}`"))?,
        None => 1,
    };
    let mut options = DiarizationOptions::default();
    if let Some(raw) = value(arguments, "--cluster-threshold") {
        options.cluster.threshold = raw
            .parse()
            .map_err(|_| format!("--cluster-threshold expects a number, got `{raw}`"))?;
    }
    if let Some(raw) = value(arguments, "--window-step-frames") {
        options.window_step_frames = raw
            .parse()
            .map_err(|_| format!("--window-step-frames expects a number, got `{raw}`"))?;
    }
    if let Some(raw) = value(arguments, "--min-track-ms") {
        options.min_track_ms = raw
            .parse()
            .map_err(|_| format!("--min-track-ms expects a number, got `{raw}`"))?;
    }
    let threshold_provenance = value(arguments, "--threshold-provenance").unwrap_or(
        "`pyannote/speaker-diarization-3.1`'s published `clustering.threshold` for this exact \
         checkpoint pair, with its `centroid` linkage; not tuned on these sessions",
    );

    let manifest_path = sessions_dir.join("sessions.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("reading {}: {error}", manifest_path.display()))?;
    let manifest: SessionManifestOut = serde_json::from_str(&text)
        .map_err(|error| format!("parsing {}: {error}", manifest_path.display()))?;

    let engine = EngineOptions {
        intra_op_threads: threads,
    };
    let started = Instant::now();
    let segmentation = SegmentationModel::open(&segmentation_path, engine)
        .map_err(|error| format!("opening {}: {error}", segmentation_path.display()))?;
    let embedder =
        SpeakerEmbeddingModel::open(&embedding_path, artifact_for(&embedding_path)?, engine)
            .map_err(|error| format!("opening {}: {error}", embedding_path.display()))?;
    let startup_ms = started.elapsed().as_millis() as u64;

    let scratch = std::env::temp_dir().join("plaudon-diarize-scratch");
    let provider =
        OnnxDiarizationProvider::new(Arc::new(segmentation), Arc::new(embedder), options, scratch);
    let ctx = JobContext::new(JobId::new());

    let mut measurements = Vec::new();
    let mut audio_seconds = 0.0f64;
    let mut run_seconds = 0.0f64;
    for record in &manifest.sessions {
        let path = sessions_dir.join(&record.path);
        let samples = read_wav(&path)?;
        let digest = file_digest(&path)?;
        if digest != record.sha256 {
            return Err(format!(
                "{} is not the file the manifest recorded",
                path.display()
            ));
        }

        let begun = Instant::now();
        let (turns, stats) = provider
            .diarize_samples(&MemorySamples(&samples), None, &ctx)
            .map_err(|error| format!("diarizing {}: {error}", record.path))?;
        let elapsed = begun.elapsed();
        run_seconds += elapsed.as_secs_f64();
        audio_seconds += samples.len() as f64 / f64::from(MODEL_SAMPLE_RATE_HZ);

        if let Some(directory) = value(arguments, "--hypothesis-rttm") {
            write_hypothesis_rttm(Path::new(directory), record, &turns)?;
            let path = Path::new(directory).join(format!(
                "{}-{}-tracks.json",
                record.session_id, record.condition
            ));
            std::fs::write(
                &path,
                serde_json::to_string_pretty(
                    &stats
                        .assignments
                        .iter()
                        .map(|a| {
                            serde_json::json!({
                                "window": a.window,
                                "local_speaker": a.local_speaker,
                                "start_ms": a.start_ms,
                                "end_ms": a.end_ms,
                                "speech_ms": a.speech_ms,
                                "fitted": a.fitted,
                                "cluster": a.cluster,
                            })
                        })
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| format!("serialising tracks: {error}"))?,
            )
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
        }
        let der = diarization_error_rate_by_overlap(&record.turns, &turns, DerOptions::default());
        println!(
            "{} [{}]: DER {:.4} ({} cluster(s) from {} track(s), {} assigned, {} dropped, \
             over {} window(s), overlap DER {:.4}) in {:.1} s",
            record.session_id,
            record.condition,
            der.overall.der,
            stats.clusters,
            stats.tracks,
            stats.tracks_assigned,
            stats.tracks_dropped,
            stats.windows,
            der.overlapping.der,
            elapsed.as_secs_f64()
        );
        measurements.push(SessionMeasurement {
            session_id: record.session_id.clone(),
            condition: record.condition.clone(),
            media_sha256: record.sha256.clone(),
            duration_ms: record.duration_ms,
            reference_speakers: record.speakers.len(),
            reference_speech_ms: der.overall.total_reference_ms,
            overlapping_reference_ms: der.overall.overlapping_reference_ms,
            hypothesis_clusters: der.overall.hypothesis_cluster_count,
            hypothesis_turns: turns.len(),
            elapsed_ms: elapsed.as_millis() as u64,
            der,
        });
    }
    if measurements.is_empty() {
        return Err(format!("{} scored no session", manifest_path.display()));
    }

    let provider_record = ProviderRecord {
        provider_id: PROVIDER_ID.to_owned(),
        provider_version: provider.provider_version(),
        validated: plaudon_ai_diarization::is_validated(PROVIDER_ID),
        models: BUNDLE_TIER
            .iter()
            .map(|artifact| ModelRecord {
                model_id: artifact.model_id.to_owned(),
                version: artifact.version.to_owned(),
                sha256: artifact.sha256_hex.to_owned(),
                licence_spdx: artifact.licence_spdx.to_owned(),
                role: artifact.role.to_owned(),
            })
            .collect(),
        parameters: vec![
            ParameterRecord {
                name: "window_step_frames".to_owned(),
                value: options.window_step_frames.to_string(),
                provenance: "a tenth of the ten-second window, the step pyannote's own \
                             pipeline uses"
                    .to_owned(),
            },
            ParameterRecord {
                name: "min_track_ms".to_owned(),
                value: options.min_track_ms.to_string(),
                provenance: "declared floor: a shorter track is embedded and assigned, but may \
                             not shape where a cluster boundary falls"
                    .to_owned(),
            },
            ParameterRecord {
                name: "min_embed_ms".to_owned(),
                value: options.min_embed_ms.to_string(),
                provenance: "declared floor: below it a vector describes the room, not a voice, \
                             and the track is dropped"
                    .to_owned(),
            },
            ParameterRecord {
                name: "cluster.min_cluster_tracks".to_owned(),
                value: options.cluster.min_cluster_tracks.to_string(),
                provenance: "`pyannote/speaker-diarization-3.1`'s published \
                             `clustering.min_cluster_size`"
                    .to_owned(),
            },
            ParameterRecord {
                name: "embedding_exclude_overlap".to_owned(),
                value: "true".to_owned(),
                provenance: "a speaker is embedded from the frames where they are the only \
                             voice, as `pyannote/speaker-diarization-3.1` does; where a track \
                             has none, the mixture is embedded and the track may not shape the \
                             clustering"
                    .to_owned(),
            },
            ParameterRecord {
                name: "cluster.threshold".to_owned(),
                value: format!("{:.4}", options.cluster.threshold),
                provenance: threshold_provenance.to_owned(),
            },
            ParameterRecord {
                name: "assemble.activity_threshold".to_owned(),
                value: format!("{:.2}", options.assemble.activity_threshold),
                provenance: "majority of the windows covering a frame".to_owned(),
            },
            ParameterRecord {
                name: "speaker_count_hint".to_owned(),
                value: "none".to_owned(),
                provenance: "the speaker count is inferred, never supplied: a hinted run \
                             would measure an easier problem than the product's"
                    .to_owned(),
            },
        ],
    };
    let runtime = RuntimeRecord {
        platform: platform_token(),
        host_cpu: host_cpu(),
        os_build: os_build(),
        intra_op_threads: threads,
        peak_rss_bytes,
        startup_ms,
        real_time_factor: run_seconds / audio_seconds,
    };
    let corpus = CorpusRecord {
        library_id: "librispeech-session-v1".to_owned(),
        corpus: "libri_speech_test_clean_derived".to_owned(),
        licence: manifest.corpus_licence.clone(),
        attribution: manifest.attribution.clone(),
        audio_committed: false,
        recipe: manifest
            .recipe
            .get("mix")
            .cloned()
            .unwrap_or_else(|| "see the session manifest".to_owned()),
    };

    let mut blockers = open_conditions(&provider_record, &runtime, &measurements);
    blockers.push(
        "inference runs in this process; ADR-0015 and VOICE_PROFILES_SPEC §8.1 require a \
         provider that parses untrusted audio to run out of process before it ships"
            .to_owned(),
    );
    blockers.push(
        "the sessions are LibriSpeech-derived synthetic mixes, not meeting-domain audio; \
         AMI and ICSI are the domain gate and are download-on-demand only (VP-182)"
            .to_owned(),
    );
    blockers.push(
        "ADR-0400 conditions 2 and 3 are documented for the weights in \
         docs/licensing/third-party-register.md, but no user-facing model-pack download, \
         licence disclosure or consent flow exists for diarization yet"
            .to_owned(),
    );

    let artifact = DerArtifact {
        schema_version: DIARIZATION_DER_REPORT_SCHEMA_VERSION,
        generator: format!("plaudon-speaker-embed {}", env!("CARGO_PKG_VERSION")),
        measured_on: value(arguments, "--measured-on").map_or_else(now_rfc3339, ToOwned::to_owned),
        commit: value(arguments, "--commit").map_or_else(head_commit, ToOwned::to_owned),
        provider: provider_record,
        runtime,
        corpus,
        by_condition: ConditionPool::of(&measurements),
        overall: PooledDer::of(&measurements),
        sessions: measurements,
        release_evidence: ReleaseEvidence::new(blockers),
        caveats: vec![
            "The audio is a synthetic mix of LibriSpeech read speech: one close-talk channel, \
             digital silence between turns and no conversational dynamics. Meeting audio is \
             harder, so this is an upper bound on the quality of this provider in the field, \
             not a prediction of it."
                .to_owned(),
            "DER is scored with a zero collar and overlapping speech included. Published \
             figures that quote a 250 ms collar, or that exclude overlap, are not comparable \
             to the `overall` figures here; the `single_speaker` half is the closer analogue."
                .to_owned(),
            "The clustering threshold is a declared default, not a value tuned on these \
             sessions. A tuned threshold measured on the same audio it was tuned on would \
             not be a measurement."
                .to_owned(),
            "A cluster is not an identity. These figures say nothing about who anybody is \
             (VOICE_PROFILES_SPEC §17.6)."
                .to_owned(),
            "Accuracy on read English speech by LibriSpeech readers does not transfer to \
             other languages, accents or recording chains (VP-170)."
                .to_owned(),
        ],
    };

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let rendered = serde_json::to_string_pretty(&artifact)
        .map_err(|error| format!("serialising the artifact: {error}"))?;
    std::fs::write(&out, format!("{rendered}\n"))
        .map_err(|error| format!("writing {}: {error}", out.display()))?;
    println!(
        "pooled DER {:.4} over {} session(s); overlap {:.4}, single speaker {:.4}; wrote {}",
        artifact.overall.overall.der,
        artifact.overall.sessions,
        artifact.overall.overlapping.der,
        artifact.overall.single_speaker.der,
        out.display()
    );
    Ok(())
}

/// Write one session's hypothesis as NIST RTTM beside the reference.
///
/// Not part of the artifact: the committed document carries rates and counts,
/// and a turn list is what somebody re-running the measurement needs in order
/// to see *where* the errors are rather than how many there were.
fn write_hypothesis_rttm(
    directory: &Path,
    record: &SessionRecord,
    turns: &[SpeakerTurn],
) -> Result<(), String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("creating {}: {error}", directory.display()))?;
    let mut out = String::new();
    for turn in turns {
        let start = turn.start_ms as f64 / 1_000.0;
        let duration = (turn.end_ms - turn.start_ms) as f64 / 1_000.0;
        writeln!(
            out,
            "SPEAKER {} 1 {start:.3} {duration:.3} <NA> <NA> {} <NA> <NA>",
            record.session_id, turn.cluster
        )
        .expect("formatting into a String cannot fail");
    }
    let path = directory.join(format!(
        "{}-{}-hypothesis.rttm",
        record.session_id, record.condition
    ));
    std::fs::write(&path, out).map_err(|error| format!("writing {}: {error}", path.display()))
}

/// Platform token, as the evidence documents spell it.
fn platform_token() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64".to_owned(),
        ("windows", "x86_64") => "windows-x64".to_owned(),
        (os, arch) => format!("{os}-{arch}"),
    }
}

fn host_cpu() -> String {
    command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
        .unwrap_or_else(|| std::env::consts::ARCH.to_owned())
}

fn os_build() -> String {
    let os = std::env::consts::OS;
    match command_output("sw_vers", &["-productVersion"]) {
        Some(version) => match command_output("sw_vers", &["-buildVersion"]) {
            Some(build) => format!("{os} {version} ({build})"),
            None => format!("{os} {version}"),
        },
        None => os.to_owned(),
    }
}

fn head_commit() -> String {
    command_output("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned())
}

fn now_rfc3339() -> String {
    command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_else(|| "unknown".to_owned())
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim().to_owned();
    (!trimmed.is_empty()).then_some(trimmed)
}
