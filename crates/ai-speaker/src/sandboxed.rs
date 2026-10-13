//! Inference out of process (ADR-0015, ADR-0081 §3, VP-025).
//!
//! [`SandboxedModel`] is what [`crate::onnx::OnnxSpeakerEmbedder`] runs its
//! windows through. It owns a `plaudon_security::sandbox` helper process,
//! hands it the checkpoint path and a window of the decoded PCM derivative,
//! and gets a vector back. ONNX Runtime, and the parsing of the model file, do
//! not happen in this process at all.
//!
//! # What the helper is granted
//!
//! Two paths, read-only: the checkpoint and the directory holding the
//! canonical PCM derivative. No write path, no network (not expressible in the
//! spec API), no further `exec`, a memory cap, a CPU budget and a wall-clock
//! deadline. The environment is scrubbed by the sandbox, so the helper
//! inherits no credential and no loader override.
//!
//! # Lifetime: one process per recording, not per window and not per session
//!
//! ADR-0015 rejects a long-lived helper daemon, and a helper per *window* is
//! not affordable: opening the session costs a digest verification over 26 MB
//! and roughly half a second (`docs/hardware/macos-apple-silicon.md`), which
//! would dominate a two-second window. The unit of work is therefore the
//! embedder: one helper, spawned when the model is opened, killed when the
//! embedder is dropped.
//!
//! A helper that dies — a crash on a hostile file, the memory cap, the CPU
//! budget, the wall-clock deadline — fails the window that was in flight and
//! is **not** retried: re-feeding the same window to a fresh process is how a
//! crash loop is built, and §14.7 wants a failed job, not a quiet retry. The
//! *next* window spawns a fresh helper, so a long recording that outran one
//! process's budget continues rather than failing wholesale, and the
//! re-opened session is checked to be the same checkpoint before it is used.
//!
//! # What crosses back
//!
//! A vector, and the facts needed to bind it: model id, version, digest, and
//! the dimensionality. The parent validates all four against the record it
//! asked for before a single component is used (ADR-0015 §5: the helper's
//! output is untrusted too), and no component is ever logged (VP-002).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use plaudon_app_core::voice_profiles::embedder::EmbedError;
use plaudon_app_core::voice_profiles::matching::ModelBinding;
use plaudon_domain::media::Sha256Digest;
use plaudon_security::sandbox::ipc::{HelperRequest, HelperResponse, Operation, Refusal};
use plaudon_security::sandbox::{
    HelperHandle, IsolationReport, SandboxError, SandboxSpec, SandboxedHelper, SpecError,
};

use crate::checkpoint::ModelArtifact;

/// File name of the helper binary, without a platform extension.
pub const HELPER_BINARY: &str = "plaudon-speaker-helper";

/// Default memory cap for an inference helper.
///
/// ONNX Runtime holds the 26 MB graph, its arena and one window's activations;
/// the measured peak resident set on macOS arm64 is 420 MB
/// (`docs/hardware/macos-apple-silicon.md`). One gigabyte leaves headroom for
/// a larger checkpoint without leaving the cap meaningless.
pub const DEFAULT_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;

/// Default CPU budget for one helper process, in seconds.
pub const DEFAULT_CPU_SECONDS: u64 = 1800;

/// Default wall-clock lifetime for one helper process.
pub const DEFAULT_WALL_CLOCK: Duration = Duration::from_secs(3600);

/// Why the out-of-process engine could not answer.
///
/// No variant carries a vector component, a sample or a file's contents: these
/// strings reach logs, and a log line may never carry biometric data (VP-002,
/// VP-112).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelperError {
    /// The helper binary is not where it should be.
    #[error("the inference helper is not installed at {path}")]
    NotInstalled {
        /// Path that was tried.
        path: String,
    },

    /// The restriction set could not be built (a grant that does not resolve).
    #[error("the inference helper's sandbox could not be described: {0}")]
    Spec(#[from] SpecError),

    /// The helper could not be spawned, died, was killed or timed out.
    #[error(transparent)]
    Sandbox(#[from] SandboxError),

    /// The helper attempted the operation and refused it.
    #[error("the inference helper refused {operation:?}: {detail}")]
    Refused {
        /// Operation that was refused.
        operation: Operation,
        /// The helper's reason, which carries no audio and no vector.
        detail: String,
    },

    /// The helper answered a different message than the one the protocol
    /// requires here.
    #[error("the inference helper answered {found} to a {expected} request")]
    Protocol {
        /// Discriminant the helper sent.
        found: &'static str,
        /// What the parent asked for.
        expected: &'static str,
    },

    /// The helper loaded a different checkpoint than the one asked for.
    #[error("the inference helper loaded {found}, not {expected}")]
    WrongCheckpoint {
        /// What the helper reported.
        found: String,
        /// What the parent required.
        expected: String,
    },

    /// The vector is not the shape this checkpoint produces.
    #[error("the inference helper returned {found} component(s), expected {expected}")]
    WrongDimension {
        /// Components the helper sent.
        found: usize,
        /// Components the checkpoint declares.
        expected: usize,
    },
}

impl From<HelperError> for EmbedError {
    fn from(error: HelperError) -> Self {
        match error {
            // The engine is not installed or cannot be described: the feature
            // is unavailable, which is a different user-facing state from "it
            // tried and failed".
            HelperError::NotInstalled { .. } | HelperError::Spec(_) => Self::Unavailable,
            // A refusal to read the window is the media being unreadable from
            // the helper's position; everything else is an engine failure.
            HelperError::Refused {
                operation: Operation::EmbedWindow,
                ..
            } => Self::Undecodable,
            HelperError::WrongCheckpoint { .. } | HelperError::WrongDimension { .. } => {
                Self::Malformed("the helper's answer did not match the checkpoint".to_owned())
            }
            HelperError::Sandbox(_)
            | HelperError::Refused { .. }
            | HelperError::Protocol { .. } => Self::EngineFailure,
        }
    }
}

/// Caps and paths one helper process runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxOptions {
    /// Absolute path of the helper binary.
    pub helper_path: PathBuf,
    /// Memory cap in bytes.
    pub memory_bytes: u64,
    /// CPU budget in seconds.
    pub cpu_seconds: u64,
    /// Wall-clock lifetime of one helper process.
    pub wall_clock: Duration,
}

impl SandboxOptions {
    /// Defaults for a helper beside the running executable.
    ///
    /// # Errors
    ///
    /// [`HelperError::NotInstalled`] when no helper binary sits beside the
    /// current executable, which is where a packaged build puts it.
    pub fn beside_current_exe() -> Result<Self, HelperError> {
        let path = default_helper_path()?;
        Ok(Self::for_helper(path))
    }

    /// Defaults for an explicitly located helper binary.
    #[must_use]
    pub fn for_helper(helper_path: PathBuf) -> Self {
        Self {
            helper_path,
            memory_bytes: DEFAULT_MEMORY_BYTES,
            cpu_seconds: DEFAULT_CPU_SECONDS,
            wall_clock: DEFAULT_WALL_CLOCK,
        }
    }
}

/// Where a packaged build keeps the helper: beside the executable.
///
/// # Errors
///
/// [`HelperError::NotInstalled`] when the path cannot be determined or holds
/// no file.
pub fn default_helper_path() -> Result<PathBuf, HelperError> {
    let exe = std::env::current_exe().map_err(|error| HelperError::NotInstalled {
        path: format!("<the running executable's directory: {:?}>", error.kind()),
    })?;
    let dir = exe.parent().ok_or_else(|| HelperError::NotInstalled {
        path: exe.display().to_string(),
    })?;
    let candidate = dir.join(if cfg!(windows) {
        concat!("plaudon-speaker-helper", ".exe")
    } else {
        HELPER_BINARY
    });
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(HelperError::NotInstalled {
            path: candidate.display().to_string(),
        })
    }
}

/// A digest-verified checkpoint, loaded inside a sandboxed helper process.
#[derive(Debug)]
pub struct SandboxedModel {
    artifact: &'static ModelArtifact,
    model_path: PathBuf,
    /// Directory the canonical PCM derivatives live in. Granted to the helper
    /// as a whole because the derivative is created *after* the helper starts
    /// and a grant cannot be added to a running process.
    pcm_dir: PathBuf,
    options: SandboxOptions,
    binding: ModelBinding,
    dim: usize,
    startup: Duration,
    isolation: IsolationReport,
    /// The live helper. `None` after a failure, which makes the next window
    /// spawn a fresh process rather than talk to a dead one.
    helper: Mutex<Option<HelperHandle>>,
}

impl SandboxedModel {
    /// Spawn a helper, load `artifact` from `model_path` inside it, and keep
    /// the process for the windows that follow.
    ///
    /// `pcm_dir` is the directory the caller will write canonical PCM
    /// derivatives into; it is granted read-only to every helper this model
    /// spawns and must exist before this call.
    ///
    /// # Errors
    ///
    /// [`HelperError`] when the helper cannot start, cannot be granted the
    /// paths, refuses the model, or answers with a checkpoint or a
    /// dimensionality that is not the one that was asked for.
    pub fn open(
        model_path: &Path,
        pcm_dir: &Path,
        artifact: &'static ModelArtifact,
        options: SandboxOptions,
    ) -> Result<Self, HelperError> {
        if !options.helper_path.is_file() {
            return Err(HelperError::NotInstalled {
                path: options.helper_path.display().to_string(),
            });
        }

        let mut helper = spawn_helper(model_path, pcm_dir, &options)?;
        let loaded = request(
            &mut helper,
            HelperRequest::LoadModel {
                path: model_path.display().to_string(),
                model_id: artifact.model_id.to_owned(),
            },
        )?;
        let (binding, dim, startup) = match loaded {
            HelperResponse::ModelLoaded {
                model_id,
                version,
                sha256,
                dim,
                startup_ms,
            } => {
                let binding = verify_identity(artifact, &model_id, &version, &sha256)?;
                (binding, dim as usize, Duration::from_millis(startup_ms))
            }
            other => return Err(unexpected(&other, "ModelLoaded")),
        };

        let isolation = helper.isolation;
        Ok(Self {
            artifact,
            model_path: model_path.to_path_buf(),
            pcm_dir: pcm_dir.to_path_buf(),
            options,
            binding,
            dim,
            startup,
            isolation,
            helper: Mutex::new(Some(helper)),
        })
    }

    /// The artifact record the checkpoint was verified against.
    #[must_use]
    pub const fn artifact(&self) -> &'static ModelArtifact {
        self.artifact
    }

    /// Model id, version and digest, as the matching core binds them.
    #[must_use]
    pub const fn binding(&self) -> &ModelBinding {
        &self.binding
    }

    /// Dimensionality of every vector this checkpoint produces.
    #[must_use]
    pub const fn dim(&self) -> usize {
        self.dim
    }

    /// Wall time the helper spent verifying the digest and creating the
    /// session, as the helper measured it.
    #[must_use]
    pub const fn startup(&self) -> Duration {
        self.startup
    }

    /// The directory every helper of this model may read PCM derivatives from.
    ///
    /// A caller that writes a window's audio anywhere else is writing it
    /// somewhere the helper cannot read, so the path is taken from the model
    /// rather than agreed separately.
    #[must_use]
    pub fn scratch_dir(&self) -> &Path {
        &self.pcm_dir
    }

    /// What the operating system actually applied to the helper process.
    ///
    /// Exposed because §16.6 forbids claiming an isolation that was not
    /// applied: a caller that reports "inference is sandboxed" must be able to
    /// read what that meant on this machine.
    #[must_use]
    pub const fn isolation(&self) -> &IsolationReport {
        &self.isolation
    }

    /// Embed `frames` samples of `pcm_path` starting at `start_frame`.
    ///
    /// # Errors
    ///
    /// [`HelperError`] when the helper dies, refuses, or answers with a vector
    /// that is not the checkpoint's shape.
    pub fn embed_window(
        &self,
        pcm_path: &Path,
        start_frame: u64,
        frames: u64,
    ) -> Result<Vec<f32>, HelperError> {
        let mut guard = self
            .helper
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // A handle that has recorded a failure is permanently failed; drop it
        // and start a fresh process for this window.
        if guard.as_ref().is_some_and(HelperHandle::is_failed) {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(self.reopen()?);
        }
        let helper = guard.as_mut().expect("a helper was just placed here");

        let response = request(
            helper,
            HelperRequest::EmbedWindow {
                pcm_path: pcm_path.display().to_string(),
                start_frame,
                frames,
            },
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                // The helper is gone or untrustworthy; the next window gets a
                // new process, and this window stays failed.
                *guard = None;
                return Err(error);
            }
        };

        match response {
            HelperResponse::Embedded { vector } => {
                if vector.len() != self.dim {
                    return Err(HelperError::WrongDimension {
                        found: vector.len(),
                        expected: self.dim,
                    });
                }
                Ok(vector.0)
            }
            other => Err(unexpected(&other, "Embedded")),
        }
    }

    /// Start a fresh helper and reload the same checkpoint into it.
    fn reopen(&self) -> Result<HelperHandle, HelperError> {
        let mut helper = spawn_helper(&self.model_path, &self.pcm_dir, &self.options)?;
        match request(
            &mut helper,
            HelperRequest::LoadModel {
                path: self.model_path.display().to_string(),
                model_id: self.artifact.model_id.to_owned(),
            },
        )? {
            HelperResponse::ModelLoaded {
                model_id,
                version,
                sha256,
                dim,
                ..
            } => {
                let binding = verify_identity(self.artifact, &model_id, &version, &sha256)?;
                if binding != self.binding || dim as usize != self.dim {
                    return Err(HelperError::WrongCheckpoint {
                        found: binding.to_string(),
                        expected: self.binding.to_string(),
                    });
                }
                Ok(helper)
            }
            other => Err(unexpected(&other, "ModelLoaded")),
        }
    }
}

impl Drop for SandboxedModel {
    fn drop(&mut self) {
        // `HelperHandle`'s own `Drop` kills the process; taking it here makes
        // the ordering explicit rather than incidental.
        if let Ok(mut guard) = self.helper.lock() {
            if let Some(mut helper) = guard.take() {
                helper.kill();
            }
        }
    }
}

/// Spawn one helper under the restriction set.
///
/// **No writable path is granted.** The helper reads a checkpoint and a PCM
/// derivative and answers on a pipe; it has no reason to create a file, and
/// the engine is pinned to the CPU execution provider partly so that stays
/// true (`crate::onnx::EngineOptions`). A grant that exists "in case the
/// runtime needs somewhere to write" is a grant nobody is checking.
fn spawn_helper(
    model_path: &Path,
    pcm_dir: &Path,
    options: &SandboxOptions,
) -> Result<HelperHandle, HelperError> {
    let spec = SandboxSpec::builder()
        .read_path(model_path)
        .read_path(pcm_dir)
        .memory_bytes(options.memory_bytes)
        .cpu_seconds(options.cpu_seconds)
        .wall_clock(options.wall_clock)
        .build()?;
    let args: [OsString; 0] = [];
    Ok(SandboxedHelper::spawn(&spec, &options.helper_path, &args)?)
}

/// Send one request and turn a refusal into an error.
fn request(
    helper: &mut HelperHandle,
    request: HelperRequest,
) -> Result<HelperResponse, HelperError> {
    match helper.request(request)? {
        HelperResponse::Refused { operation, refusal } => Err(HelperError::Refused {
            operation,
            detail: describe(&refusal),
        }),
        HelperResponse::Unsupported { operation } => Err(HelperError::Refused {
            operation,
            detail: "the helper does not implement this operation".to_owned(),
        }),
        other => Ok(other),
    }
}

/// Check the helper loaded the checkpoint the parent asked for.
///
/// The helper's answer is untrusted: it is a different process, and a
/// compromised one would happily report the identity we wanted to hear. This
/// comparison is against the compiled record, not against the helper's claim,
/// so the only thing the helper can do is fail the check.
fn verify_identity(
    artifact: &'static ModelArtifact,
    model_id: &str,
    version: &str,
    sha256: &str,
) -> Result<ModelBinding, HelperError> {
    if model_id != artifact.model_id
        || version != artifact.version
        || !artifact.matches_digest(sha256)
    {
        return Err(HelperError::WrongCheckpoint {
            found: format!("{model_id}/{version}"),
            expected: format!("{}/{}", artifact.model_id, artifact.version),
        });
    }
    let digest: Sha256Digest =
        artifact
            .sha256_hex
            .parse()
            .map_err(|_| HelperError::WrongCheckpoint {
                found: "a digest that is not 32 hexadecimal bytes".to_owned(),
                expected: artifact.sha256_hex.to_owned(),
            })?;
    Ok(ModelBinding::new(
        artifact.model_id,
        artifact.version,
        digest,
    ))
}

/// Render a refusal without copying anything the helper read.
fn describe(refusal: &Refusal) -> String {
    match refusal.os_error {
        Some(code) => format!("{:?} (os error {code}): {}", refusal.kind, refusal.detail),
        None => format!("{:?}: {}", refusal.kind, refusal.detail),
    }
}

fn unexpected(response: &HelperResponse, expected: &'static str) -> HelperError {
    HelperError::Protocol {
        found: discriminant_of(response),
        expected,
    }
}

/// The name of a response's variant, and nothing it carries.
fn discriminant_of(response: &HelperResponse) -> &'static str {
    match response {
        HelperResponse::Hello { .. } => "Hello",
        HelperResponse::Echo { .. } => "Echo",
        HelperResponse::FileRead { .. } => "FileRead",
        HelperResponse::FileWritten { .. } => "FileWritten",
        HelperResponse::TcpConnected { .. } => "TcpConnected",
        HelperResponse::ChildSpawned { .. } => "ChildSpawned",
        HelperResponse::Allocated { .. } => "Allocated",
        HelperResponse::CpuBurned { .. } => "CpuBurned",
        HelperResponse::Slept { .. } => "Slept",
        HelperResponse::ModelLoaded { .. } => "ModelLoaded",
        HelperResponse::Embedded { .. } => "Embedded",
        HelperResponse::Refused { .. } => "Refused",
        HelperResponse::Unsupported { .. } => "Unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::WESPEAKER_RESNET34_LM;

    #[test]
    fn a_missing_helper_binary_is_unavailable_not_a_crash() {
        let dir = tempfile::tempdir().expect("temp dir");
        let options = SandboxOptions::for_helper(dir.path().join("plaudon-speaker-helper"));
        let error = SandboxedModel::open(
            &dir.path().join("model.onnx"),
            dir.path(),
            &WESPEAKER_RESNET34_LM,
            options,
        )
        .expect_err("refused");
        assert!(matches!(error, HelperError::NotInstalled { .. }));
        assert_eq!(EmbedError::from(error), EmbedError::Unavailable);
    }

    #[test]
    fn a_helper_that_loads_a_different_checkpoint_is_rejected() {
        let error = verify_identity(
            &WESPEAKER_RESNET34_LM,
            "some-other-model",
            WESPEAKER_RESNET34_LM.version,
            WESPEAKER_RESNET34_LM.sha256_hex,
        )
        .expect_err("refused");
        assert!(matches!(error, HelperError::WrongCheckpoint { .. }));
        assert_eq!(
            EmbedError::from(error),
            EmbedError::Malformed("the helper's answer did not match the checkpoint".to_owned())
        );
    }

    #[test]
    fn a_helper_that_reports_a_digest_we_did_not_review_is_rejected() {
        let error = verify_identity(
            &WESPEAKER_RESNET34_LM,
            WESPEAKER_RESNET34_LM.model_id,
            WESPEAKER_RESNET34_LM.version,
            &"0".repeat(64),
        )
        .expect_err("refused");
        assert!(matches!(error, HelperError::WrongCheckpoint { .. }));
    }

    #[test]
    fn no_error_variant_can_carry_a_vector() {
        // Same guard as `onnx::tests::no_error_variant_can_carry_a_vector`,
        // one process boundary further out: every variant renders from
        // strings and counts, so a component cannot be smuggled into a log
        // line through an error (VP-002, VP-112).
        let rendered = HelperError::WrongDimension {
            found: 192,
            expected: 256,
        }
        .to_string();
        assert_eq!(
            rendered,
            "the inference helper returned 192 component(s), expected 256"
        );
    }
}
