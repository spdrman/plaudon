//! `plaudon-speaker-helper` — the process speaker embeddings are computed in
//! (ADR-0015, ADR-0081 §3, VP-025).
//!
//! The application never parses a model file and never runs ONNX Runtime. This
//! binary does both, under `plaudon_security::sandbox`: a `(deny default)`
//! Seatbelt profile on macOS or a job object on Windows, a memory cap, a CPU
//! cap, a wall-clock deadline, no network and no further `exec`. It is granted
//! exactly two paths — the checkpoint it loads and the canonical PCM
//! derivative it reads windows from — and it answers on stdout in the typed,
//! length-prefixed [`plaudon_security::sandbox::ipc`] protocol.
//!
//! Three properties are load-bearing:
//!
//! * **No work happens before the first frame arrives.** On Windows the parent
//!   assigns the process to its job object just after creation rather than
//!   creating it suspended, so a helper that did anything at start-up would do
//!   it uncapped (`sandbox::windows`). Even the `Hello` answer is computed
//!   after the request is read.
//! * **The digest is verified before the file is parsed.** `LoadModel` names
//!   the artifact record by id, and
//!   [`SpeakerEmbeddingModel::open`][open] refuses a file that is not the
//!   reviewed one *before* ONNX Runtime sees a byte of it. A helper that
//!   loaded whatever path it was handed would move the parsing risk into the
//!   sandbox without reducing it.
//! * **No vector is ever written to stderr or to a log.** Stderr is a
//!   diagnostic channel the parent captures; an embedding leaves this process
//!   through exactly one route, the `Embedded` response, and the type that
//!   carries it redacts itself in `Debug` (VP-002, VP-112).
//!
//! A failure is a *response*, not a crash: a missing file, a refused path or a
//! model that will not load comes back as `Refused` with the operating-system
//! error where there is one, so the parent can tell "the sandbox denied this"
//! from "the helper fell over". The exceptions are the ones the sandbox owns —
//! an allocation past the memory cap or a run past the CPU budget kills the
//! process, and the parent classifies the death (§14.7).
//!
//! [open]: plaudon_ai_speaker::onnx::SpeakerEmbeddingModel::open

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use plaudon_ai_speaker::checkpoint::{ModelArtifact, BUNDLE_TIER};
use plaudon_ai_speaker::onnx::{EngineOptions, SpeakerEmbeddingModel};
use plaudon_security::sandbox::ipc::{
    self, EmbeddingVector, FrameError, HelperRequest, HelperResponse, Operation, Refusal,
    RefusalKind,
};

/// Largest window the helper will read into memory: two minutes of 16 kHz mono
/// `f32`, 7.7 MB. Windows in this pipeline are seconds long (VP-024); a larger
/// request is a bug or a hostile parent, and either way it is refused rather
/// than allocated.
const MAX_WINDOW_SAMPLES: u64 = 120 * 16_000;

/// Bytes of PCM read per chunk while assembling a window.
const PCM_CHUNK_BYTES: usize = 64 * 1024;

fn main() {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    // The loaded model, if `LoadModel` has succeeded. One model per helper
    // process: ADR-0015 wants one process per unit of work, and a helper that
    // accumulated sessions would be the long-lived daemon §6.2 rules out.
    let mut model: Option<SpeakerEmbeddingModel> = None;

    loop {
        let request = match ipc::read_message::<_, HelperRequest>(&mut reader) {
            Ok(request) => request,
            // The parent closed the pipe: exit cleanly.
            Err(FrameError::Closed) => break,
            Err(err) => {
                let _ = ipc::write_frame(
                    &mut writer,
                    &HelperResponse::Refused {
                        operation: Operation::Hello,
                        refusal: Refusal {
                            kind: RefusalKind::Other,
                            os_error: None,
                            detail: err.to_string(),
                        },
                    },
                );
                break;
            }
        };

        if matches!(request, HelperRequest::Shutdown) {
            let _ = ipc::write_frame(
                &mut writer,
                &HelperResponse::Echo {
                    payload: "shutdown".to_owned(),
                },
            );
            break;
        }

        let response = handle(request, &mut model);
        if ipc::write_frame(&mut writer, &response).is_err() {
            break;
        }
    }
}

fn handle(request: HelperRequest, model: &mut Option<SpeakerEmbeddingModel>) -> HelperResponse {
    let operation = request.operation();
    match request {
        HelperRequest::Hello => HelperResponse::Hello {
            limits: ipc::current_limits(),
            pid: std::process::id(),
        },
        HelperRequest::LoadModel { path, model_id } => load_model(&path, &model_id, model),
        HelperRequest::EmbedWindow {
            pcm_path,
            start_frame,
            frames,
        } => embed_window(&pcm_path, start_frame, frames, model.as_ref()),
        // Everything else is a decoder's or a probe's vocabulary. This helper
        // reads one PCM file by range and runs one model; it does not echo,
        // write files, open sockets, spawn children, allocate on demand or
        // sleep, and it says so instead of appearing to comply.
        HelperRequest::Echo { .. }
        | HelperRequest::ReadFile { .. }
        | HelperRequest::WriteFile { .. }
        | HelperRequest::ConnectTcp { .. }
        | HelperRequest::SpawnChild
        | HelperRequest::AllocateBytes { .. }
        | HelperRequest::BurnCpu { .. }
        | HelperRequest::SleepMs { .. }
        // Handled by the caller before dispatch.
        | HelperRequest::Shutdown => HelperResponse::Unsupported { operation },
    }
}

/// Verify a checkpoint against its record and open a session over it.
fn load_model(
    path: &str,
    model_id: &str,
    model: &mut Option<SpeakerEmbeddingModel>,
) -> HelperResponse {
    if model.is_some() {
        return refusal(
            Operation::LoadModel,
            RefusalKind::Other,
            "this helper already holds a session; a second model needs a second process",
        );
    }
    let Some(artifact) = artifact_for(model_id) else {
        return refusal(
            Operation::LoadModel,
            RefusalKind::NotFound,
            "no compiled artifact record carries that model id",
        );
    };

    let started = Instant::now();
    match SpeakerEmbeddingModel::open(Path::new(path), artifact, EngineOptions::default()) {
        Ok(opened) => {
            let dim = u32::try_from(opened.dim()).unwrap_or(u32::MAX);
            let response = HelperResponse::ModelLoaded {
                model_id: artifact.model_id.to_owned(),
                version: artifact.version.to_owned(),
                sha256: artifact.sha256_hex.to_owned(),
                dim,
                startup_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            };
            *model = Some(opened);
            response
        }
        // The error text names the artifact, the sizes and the runtime's
        // complaint; it never carries audio or a vector.
        Err(error) => refusal(Operation::LoadModel, RefusalKind::Other, &error.to_string()),
    }
}

/// Read one window of canonical PCM and embed it.
fn embed_window(
    pcm_path: &str,
    start_frame: u64,
    frames: u64,
    model: Option<&SpeakerEmbeddingModel>,
) -> HelperResponse {
    let Some(model) = model else {
        return refusal(
            Operation::EmbedWindow,
            RefusalKind::Other,
            "no model is loaded in this helper",
        );
    };
    if frames == 0 || frames > MAX_WINDOW_SAMPLES {
        return refusal(
            Operation::EmbedWindow,
            RefusalKind::ResourceExhausted,
            "the requested window is empty or longer than this helper will buffer",
        );
    }

    let samples = match read_window(pcm_path, start_frame, frames) {
        Ok(samples) => samples,
        Err(error) => {
            return HelperResponse::Refused {
                operation: Operation::EmbedWindow,
                refusal: Refusal::from_io(&error),
            }
        }
    };

    match model.embed_samples(&samples) {
        Ok(values) => HelperResponse::Embedded {
            vector: EmbeddingVector(values),
        },
        Err(error) => refusal(
            Operation::EmbedWindow,
            RefusalKind::Other,
            &error.to_string(),
        ),
    }
}

/// Read `frames` `f32` samples starting at `start_frame`.
///
/// Reads by seeking rather than by loading the file: the derivative of a long
/// recording is hundreds of megabytes and §8.1 forbids making peak memory a
/// function of total duration.
fn read_window(pcm_path: &str, start_frame: u64, frames: u64) -> std::io::Result<Vec<f32>> {
    let mut file = File::open(pcm_path)?;
    file.seek(SeekFrom::Start(start_frame.saturating_mul(4)))?;

    // `frames` is bounded by `MAX_WINDOW_SAMPLES` above, so this reservation
    // is bounded too.
    let wanted = usize::try_from(frames).unwrap_or(usize::MAX);
    let mut samples = Vec::with_capacity(wanted);
    let mut buffer = vec![0u8; PCM_CHUNK_BYTES];
    while samples.len() < wanted {
        let remaining = (wanted - samples.len()) * 4;
        let slice = &mut buffer[..remaining.min(PCM_CHUNK_BYTES)];
        let read = file.read(slice)?;
        if read == 0 {
            break;
        }
        for chunk in slice[..read].as_chunks::<4>().0 {
            samples.push(f32::from_le_bytes(*chunk));
        }
    }
    Ok(samples)
}

fn artifact_for(model_id: &str) -> Option<&'static ModelArtifact> {
    BUNDLE_TIER
        .iter()
        .find(|artifact| artifact.model_id == model_id)
}

fn refusal(operation: Operation, kind: RefusalKind, detail: &str) -> HelperResponse {
    HelperResponse::Refused {
        operation,
        refusal: Refusal {
            kind,
            os_error: None,
            detail: detail.to_owned(),
        },
    }
}
