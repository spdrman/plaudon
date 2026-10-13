//! P2.5d acceptance: an embedding produced in another process (ADR-0015,
//! ADR-0081 §3, VP-025, issue #283).
//!
//! The tests that need the 26 MB checkpoint say so and skip when it is not
//! installed — `scripts/fetch-speaker-models.sh` is the only sanctioned way to
//! obtain it and this repository does not carry weights. The tests that do not
//! need it are hermetic and always run, because the properties they check —
//! the helper refuses a file that is not the reviewed checkpoint, refuses a
//! window before a model is loaded, and does not implement a decoder's
//! vocabulary — are the ones a regression would quietly break.
//!
//! Isolation assertions are written as *pairs*: denied when the report claims
//! isolation, and reported as reduced when it does not. A test that only
//! asserted the happy path would pass on a machine where no restriction was
//! applied, which is the failure mode §16.6 forbids.

#![cfg(feature = "onnx")]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use plaudon_ai_speaker::checkpoint::{WESPEAKER_EMBEDDING_DIM, WESPEAKER_RESNET34_LM};
use plaudon_ai_speaker::onnx::{EngineOptions, SpeakerEmbeddingModel};
use plaudon_ai_speaker::sandboxed::{HelperError, SandboxOptions, SandboxedModel};
use plaudon_security::sandbox::ipc::{HelperRequest, HelperResponse, Operation};
use plaudon_security::sandbox::{SandboxSpec, SandboxedHelper};

/// Sample rate every window in this file is written at.
const RATE: u32 = 16_000;

/// The helper binary under test, built by Cargo beside this test.
fn helper_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_plaudon-speaker-helper"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate lives two directories below the repository root")
        .to_path_buf()
}

/// The installed checkpoint, or `None` when the model pack is not fetched.
fn checkpoint() -> Option<PathBuf> {
    let path = repo_root()
        .join("tests/fixtures/speaker/models")
        .join(WESPEAKER_RESNET34_LM.file_name);
    path.is_file().then_some(path)
}

/// Three seconds of a deterministic, speech-shaped signal.
///
/// Not speech, and not claimed to be: these tests check that the same samples
/// produce the same vector on both sides of a process boundary, which is a
/// property of the engine and not of the audio.
fn synthetic_window(seconds: u32) -> Vec<f32> {
    let total = (RATE * seconds) as usize;
    (0..total)
        .map(|index| {
            let t = index as f32 / RATE as f32;
            let fundamental = (2.0 * std::f32::consts::PI * 140.0 * t).sin() * 0.35;
            let formant = (2.0 * std::f32::consts::PI * 700.0 * t).sin() * 0.18;
            let shimmer = (2.0 * std::f32::consts::PI * 2_300.0 * t).sin() * 0.06;
            let envelope = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
            (fundamental + formant + shimmer) * envelope
        })
        .collect()
}

fn write_pcm(path: &Path, samples: &[f32]) {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write pcm");
}

fn options() -> SandboxOptions {
    SandboxOptions::for_helper(helper_path())
}

/// The acceptance case: a vector computed in a sandboxed helper process, equal
/// to the one the same checkpoint produces in this process.
#[test]
fn an_embedding_is_produced_inside_the_sandboxed_helper() {
    let Some(model_path) = checkpoint() else {
        eprintln!(
            "skipped: {} is not installed; run scripts/fetch-speaker-models.sh",
            WESPEAKER_RESNET34_LM.file_name
        );
        return;
    };

    let dir = tempfile::tempdir().expect("temp dir");
    let samples = synthetic_window(3);
    let pcm = dir.path().join("window.pcm");
    write_pcm(&pcm, &samples);

    let model = SandboxedModel::open(&model_path, dir.path(), &WESPEAKER_RESNET34_LM, options())
        .expect("the helper loaded the reviewed checkpoint");

    assert_eq!(model.dim(), WESPEAKER_EMBEDDING_DIM);
    assert_eq!(model.binding().model_id, WESPEAKER_RESNET34_LM.model_id);
    assert!(
        model.startup() > Duration::ZERO,
        "the helper reported no time spent verifying 26 MB and creating a session"
    );
    // §16.6: the report is what was applied, and the test prints it rather
    // than assuming it. Both outcomes are legitimate; a *claim* without the
    // primitive would not be.
    eprintln!("helper isolation: {}", model.isolation().summary());

    // Timed, and printed rather than asserted: a wall-clock bound in a test
    // would be a flake on a loaded machine, but the number is what
    // `docs/release/voice-profiles-evidence.md` quotes for the helper round
    // trip, so it is taken where the acceptance case runs.
    let started = std::time::Instant::now();
    let sandboxed = model
        .embed_window(&pcm, 0, samples.len() as u64)
        .expect("the helper embedded the window");
    let first_window = started.elapsed();
    let mut best = first_window;
    for _ in 0..4 {
        let started = std::time::Instant::now();
        model
            .embed_window(&pcm, 0, samples.len() as u64)
            .expect("the helper embedded the window");
        best = best.min(started.elapsed());
    }
    eprintln!(
        "helper startup {:?}; 3 s window round trip: first {first_window:?}, best of five {best:?}",
        model.startup()
    );
    assert_eq!(sandboxed.len(), WESPEAKER_EMBEDDING_DIM);
    assert!(
        sandboxed.iter().all(|value| value.is_finite()),
        "the helper returned a non-finite component"
    );
    let norm: f32 = sandboxed.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!(norm > 0.0, "the helper returned a zero vector");

    // The same samples, through the same checkpoint, in this process. Equal to
    // within f32 rounding: the helper runs the identical code path, so a
    // difference here would mean the window did not survive the boundary.
    let in_process = SpeakerEmbeddingModel::open(
        &model_path,
        &WESPEAKER_RESNET34_LM,
        EngineOptions::default(),
    )
    .expect("open in process")
    .embed_samples(&samples)
    .expect("embed in process");
    assert_eq!(in_process.len(), sandboxed.len());
    let worst = std::iter::zip(&in_process, &sandboxed)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        worst < 1e-6,
        "the sandboxed vector differs from the in-process one by {worst}"
    );
}

/// A second window on the same helper reuses the session rather than paying
/// for a second one.
#[test]
fn a_second_window_reuses_the_helper_process() {
    let Some(model_path) = checkpoint() else {
        eprintln!("skipped: the speaker checkpoint is not installed");
        return;
    };

    let dir = tempfile::tempdir().expect("temp dir");
    let samples = synthetic_window(2);
    let pcm = dir.path().join("window.pcm");
    write_pcm(&pcm, &samples);

    let model = SandboxedModel::open(&model_path, dir.path(), &WESPEAKER_RESNET34_LM, options())
        .expect("load");
    let first = model
        .embed_window(&pcm, 0, samples.len() as u64)
        .expect("first window");
    let second = model
        .embed_window(&pcm, 0, samples.len() as u64)
        .expect("second window");
    assert_eq!(first, second, "the same window produced two vectors");
}

/// A PCM file outside the helper's grants must not be readable.
#[test]
fn a_window_outside_the_granted_directory_is_refused() {
    let Some(model_path) = checkpoint() else {
        eprintln!("skipped: the speaker checkpoint is not installed");
        return;
    };

    let granted = tempfile::tempdir().expect("temp dir");
    let elsewhere = tempfile::tempdir().expect("temp dir");
    let samples = synthetic_window(2);
    let outside = elsewhere.path().join("window.pcm");
    write_pcm(&outside, &samples);

    let model = SandboxedModel::open(
        &model_path,
        granted.path(),
        &WESPEAKER_RESNET34_LM,
        options(),
    )
    .expect("load");
    let filesystem_is_restricted = model.isolation().filesystem.is_isolated();
    let result = model.embed_window(&outside, 0, samples.len() as u64);

    if filesystem_is_restricted {
        let error = result.expect_err("a file outside the grants was readable under a profile");
        assert!(
            matches!(error, HelperError::Refused { operation, .. } if operation == Operation::EmbedWindow),
            "expected a refusal from the helper, got {error}"
        );
    } else {
        // Honest degradation: no filesystem restriction was applied, so the
        // read succeeding is the truthful outcome and the report says why.
        assert!(
            !model.isolation().notes.is_empty(),
            "a reduced-isolation report must name its reason"
        );
    }
}

/// The helper verifies the digest before ONNX Runtime sees the file.
#[test]
fn a_file_that_is_not_the_reviewed_checkpoint_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let impostor = dir.path().join(WESPEAKER_RESNET34_LM.file_name);
    std::fs::write(&impostor, b"ONNX-shaped nothing").expect("write");

    let error = SandboxedModel::open(&impostor, dir.path(), &WESPEAKER_RESNET34_LM, options())
        .expect_err("the helper loaded a file that is not the reviewed checkpoint");
    let HelperError::Refused { operation, detail } = &error else {
        panic!("expected a refusal, got {error}");
    };
    assert_eq!(*operation, Operation::LoadModel);
    assert!(
        detail.contains("wespeaker-resnet34-lm"),
        "the refusal does not name the artifact it rejected: {detail}"
    );
}

/// The helper is not a decoder and does not pretend to be one.
#[test]
fn the_helper_refuses_the_vocabulary_it_does_not_implement() {
    let dir = tempfile::tempdir().expect("temp dir");
    let spec = SandboxSpec::builder()
        .read_path(dir.path())
        .memory_bytes(256 * 1024 * 1024)
        .cpu_seconds(30)
        .wall_clock(Duration::from_secs(30))
        .build()
        .expect("spec");
    let args: [OsString; 0] = [];
    let mut helper = SandboxedHelper::spawn(&spec, &helper_path(), &args).expect("spawn");

    for request in [
        HelperRequest::ReadFile {
            path: dir.path().join("anything").display().to_string(),
        },
        HelperRequest::ConnectTcp {
            host: "198.51.100.7".to_owned(),
            port: 443,
        },
        HelperRequest::SpawnChild,
        HelperRequest::AllocateBytes {
            bytes: 1 << 30,
            graceful: true,
        },
    ] {
        let operation = request.operation();
        let response = helper.request(request).expect("the helper answered");
        assert_eq!(
            response,
            HelperResponse::Unsupported { operation },
            "the inference helper implemented an operation it has no business implementing"
        );
    }

    // And a window before a model is loaded is a refusal, not a vector.
    let response = helper
        .request(HelperRequest::EmbedWindow {
            pcm_path: dir.path().join("window.pcm").display().to_string(),
            start_frame: 0,
            frames: 16_000,
        })
        .expect("the helper answered");
    assert!(
        matches!(
            response,
            HelperResponse::Refused {
                operation: Operation::EmbedWindow,
                ..
            }
        ),
        "a window embedded without a loaded model: {response:?}"
    );
}
