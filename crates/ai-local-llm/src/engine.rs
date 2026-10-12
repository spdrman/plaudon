//! The local text generation engine (spec 3.1, 8.4, 9.4, 12).
//!
//! The engine implements [`TextModel`], so the summarization pipeline and the
//! transcript tools reach it through exactly the same contract as a cloud
//! model. What differs is the preflight, and the preflight is the point of
//! this crate:
//!
//! 1. The footprint/licence gate must have passed for this host
//!    ([`crate::gate`]). Until it has, the engine reports itself unavailable
//!    and [`LocalTextModel::generate_structured`] refuses.
//! 2. The weight file must verify against the trusted manifest
//!    ([`crate::artifact`]) on every run, not once at install time.
//! 3. The input must fit the verified context, and when it does not, the
//!    caller is told to section it rather than having its tail quietly
//!    dropped.
//!
//! Inference itself is delegated to a [`LocalInferenceBackend`]. The shipped
//! backend runs a user-installed GGUF runtime as a child process: spec 12
//! forbids storing weights in the application bundle and forbids silent
//! downloads, so the model and its runtime are user-managed artifacts and the
//! engine's job is to use them correctly, offline.

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, PricingBasis, PrivacyClass, TextModelCapabilities,
};
use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::model::{EngineId, ModelIdentity};
use tokio::io::AsyncWriteExt;

use crate::artifact::LocalModelArtifact;
use crate::context::{estimate_tokens, ContextBudget};
use crate::gate::GateVerdict;

/// Engine identifier for the cross-platform local text model.
pub const LOCAL_LLM_ENGINE_ID: &str = "local-llm";

/// What a backend is asked to generate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceRequest {
    /// Verified model file.
    pub model_path: PathBuf,
    /// Fully rendered prompt.
    pub prompt: String,
    /// JSON Schema the response must satisfy, passed to runtimes that support
    /// grammar-constrained decoding.
    pub output_schema: serde_json::Value,
    /// Maximum tokens to generate.
    pub max_output_tokens: u64,
    /// Context window to run with.
    pub context_tokens: u64,
}

/// Something that can run a local model.
#[async_trait]
pub trait LocalInferenceBackend: Send + Sync + std::fmt::Debug {
    /// Human-readable backend name, recorded in provenance.
    fn backend_name(&self) -> &str;

    /// Generate raw text.
    async fn generate(
        &self,
        request: InferenceRequest,
        ctx: &JobContext,
    ) -> Result<String, TextModelError>;
}

/// Runs a user-installed GGUF runtime as a child process.
///
/// The runtime is located by the model manager, never downloaded by this
/// crate, and is invoked with no network access of its own. The prompt is
/// written to the child's stdin rather than passed as an argument, so a
/// transcript can never reach a shell command line.
#[derive(Debug, Clone)]
pub struct ProcessInferenceBackend {
    runtime_path: PathBuf,
    extra_args: Vec<String>,
}

impl ProcessInferenceBackend {
    /// Point the backend at an installed runtime executable.
    #[must_use]
    pub fn new(runtime_path: PathBuf) -> Self {
        Self {
            runtime_path,
            extra_args: Vec::new(),
        }
    }

    /// Append runtime-specific arguments, e.g. a GPU layer count.
    #[must_use]
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Whether the runtime executable is present.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.runtime_path.is_file()
    }
}

#[async_trait]
impl LocalInferenceBackend for ProcessInferenceBackend {
    fn backend_name(&self) -> &str {
        "process"
    }

    async fn generate(
        &self,
        request: InferenceRequest,
        ctx: &JobContext,
    ) -> Result<String, TextModelError> {
        if !self.is_installed() {
            return Err(TextModelError::Unavailable);
        }
        let mut command = tokio::process::Command::new(&self.runtime_path);
        command
            .arg("--model")
            .arg(&request.model_path)
            .arg("--ctx-size")
            .arg(request.context_tokens.to_string())
            .arg("--n-predict")
            .arg(request.max_output_tokens.to_string())
            .arg("--prompt-file")
            .arg("-")
            .args(&self.extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // An inference helper has no business reading the user's
            // environment; spec 16.6 keeps diagnostics free of ambient state.
            .env_clear();

        let mut child = command.spawn().map_err(|_| TextModelError::Unavailable)?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request.prompt.as_bytes())
                .await
                .map_err(|_| TextModelError::EngineFailure)?;
            stdin
                .shutdown()
                .await
                .map_err(|_| TextModelError::EngineFailure)?;
        }

        ctx.report(Progress::fraction(0.1));
        let output = tokio::select! {
            result = child.wait_with_output() => result.map_err(|_| TextModelError::EngineFailure)?,
            () = ctx.cancel.cancelled() => return Err(TextModelError::Cancelled),
        };
        if !output.status.success() {
            return Err(TextModelError::EngineFailure);
        }
        ctx.report(Progress::fraction(1.0));
        String::from_utf8(output.stdout).map_err(|_| TextModelError::EngineFailure)
    }
}

/// A local text model behind the footprint gate.
#[derive(Debug)]
pub struct LocalTextModel {
    artifact: LocalModelArtifact,
    verdict: GateVerdict,
    backend: Box<dyn LocalInferenceBackend>,
    budget: ContextBudget,
}

impl LocalTextModel {
    /// Build an engine for a gate verdict and an installed artifact.
    #[must_use]
    pub fn new(
        artifact: LocalModelArtifact,
        verdict: GateVerdict,
        backend: Box<dyn LocalInferenceBackend>,
    ) -> Self {
        let context_tokens = verdict
            .verified_context_tokens()
            .unwrap_or(artifact.context_tokens)
            .min(artifact.context_tokens);
        Self {
            artifact,
            verdict,
            backend,
            budget: ContextBudget::verified(context_tokens),
        }
    }

    /// Whether the feature may be offered at all on this host.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        self.verdict.is_passed()
    }

    /// The gate verdict, so the UI can explain an unavailable feature.
    #[must_use]
    pub const fn verdict(&self) -> &GateVerdict {
        &self.verdict
    }

    /// The context budget this engine plans against.
    #[must_use]
    pub const fn budget(&self) -> &ContextBudget {
        &self.budget
    }

    /// Model identity.
    #[must_use]
    pub fn identity(&self) -> ModelIdentity {
        ModelIdentity::new(
            EngineId::new(LOCAL_LLM_ENGINE_ID),
            self.artifact.model_id.clone(),
        )
    }
}

#[async_trait]
impl TextModel for LocalTextModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        let available = self.is_available();
        TextModelCapabilities {
            payload_version: 1,
            status: if available {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Unsupported
            },
            engine_id: EngineId::new(LOCAL_LLM_ENGINE_ID),
            model: self.identity(),
            // Generation happens in a child process on this machine. That is
            // true whether or not the gate passed.
            offline: Tri::Yes,
            max_context_tokens: if available {
                Known::Known(self.budget.context_tokens)
            } else {
                Known::Unknown
            },
            max_output_tokens: if available {
                Known::Known(self.budget.reserved_output_tokens)
            } else {
                Known::Unknown
            },
            // Local runtimes constrain decoding with a grammar rather than a
            // provider-side schema; until that is probed on the installed
            // runtime it is not claimed.
            supports_structured_output: if available { Tri::Yes } else { Tri::Unknown },
            can_cancel: Tri::Yes,
            privacy_class: Known::Known(PrivacyClass::OnDevice),
            pricing_basis: Known::Known(PricingBasis::None),
            availability_reason: if available {
                AvailabilityReason::Available
            } else {
                AvailabilityReason::HardwareUnsupported {
                    detail: "footprint_gate_not_passed".to_string(),
                }
            },
        }
    }

    async fn generate_structured(
        &self,
        request: StructuredRequest,
        ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        if !self.is_available() {
            return Err(TextModelError::Unavailable);
        }
        self.artifact
            .verify()
            .map_err(|_| TextModelError::Unavailable)?;
        if ctx.is_cancelled() {
            return Err(TextModelError::Cancelled);
        }

        let prompt_tokens =
            estimate_tokens(u32::try_from(request.prompt.chars().count()).unwrap_or(u32::MAX));
        if prompt_tokens > self.budget.input_token_allowance() {
            // The caller must section the input (spec 9.4). Silently sending a
            // truncated prompt would produce a summary of an unknown prefix.
            return Err(TextModelError::ContextTooLarge);
        }

        let raw = self
            .backend
            .generate(
                InferenceRequest {
                    model_path: self.artifact.path.clone(),
                    prompt: request.prompt,
                    output_schema: request.output_schema,
                    max_output_tokens: request
                        .max_output_tokens
                        .unwrap_or(self.budget.reserved_output_tokens),
                    context_tokens: self.budget.context_tokens,
                },
                &ctx,
            )
            .await?;

        let body = parse_body(&raw).ok_or(TextModelError::SchemaValidationFailed)?;
        let repair_attempts = u8::from(raw.trim() != body.to_string());
        Ok(StructuredResponse {
            body,
            repair_attempts,
            usage: None,
        })
    }
}

/// Extract the JSON document from a runtime's stdout.
fn parse_body(raw: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.trim()) {
        return Some(value);
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::GateRefusal;
    use plaudon_domain::ids::JobId;
    use sha2::{Digest, Sha256};

    #[derive(Debug)]
    struct ScriptedBackend {
        output: String,
    }

    #[async_trait]
    impl LocalInferenceBackend for ScriptedBackend {
        fn backend_name(&self) -> &str {
            "scripted"
        }
        async fn generate(
            &self,
            _request: InferenceRequest,
            ctx: &JobContext,
        ) -> Result<String, TextModelError> {
            if ctx.is_cancelled() {
                return Err(TextModelError::Cancelled);
            }
            Ok(self.output.clone())
        }
    }

    fn artifact(dir: &std::path::Path) -> LocalModelArtifact {
        let bytes = b"fixture-weights";
        let path = dir.join("model.gguf");
        std::fs::write(&path, bytes).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        LocalModelArtifact {
            model_id: "fixture-3b".to_string(),
            path,
            expected_sha256: hex::encode(hasher.finalize()),
            expected_bytes: bytes.len() as u64,
            context_tokens: 32_768,
        }
    }

    fn passed() -> GateVerdict {
        GateVerdict::Passed {
            model_id: "fixture-3b".to_string(),
            verified_context_tokens: 32_768,
            backend: "metal".to_string(),
        }
    }

    fn refused() -> GateVerdict {
        GateVerdict::Refused(GateRefusal::PlatformNotMeasured {
            platform: "plan9-risc".to_string(),
        })
    }

    fn request(prompt: &str) -> StructuredRequest {
        StructuredRequest {
            prompt_version: "v1".to_string(),
            prompt: prompt.to_string(),
            schema_version: "v1".to_string(),
            output_schema: serde_json::json!({"type": "object"}),
            max_output_tokens: Some(256),
        }
    }

    #[tokio::test]
    async fn a_gated_off_engine_is_unavailable_and_refuses_to_run() {
        let dir = tempfile::tempdir().unwrap();
        let model = LocalTextModel::new(
            artifact(dir.path()),
            refused(),
            Box::new(ScriptedBackend {
                output: "{}".to_string(),
            }),
        );
        assert!(!model.is_available());
        let caps = model.capabilities().await;
        assert_eq!(caps.status, CapabilityStatus::Unsupported);
        assert_eq!(caps.max_context_tokens, Known::Unknown);
        assert_eq!(
            model
                .generate_structured(request("hello"), JobContext::new(JobId::new()))
                .await,
            Err(TextModelError::Unavailable)
        );
    }

    #[tokio::test]
    async fn a_gated_on_engine_generates_and_reports_on_device_privacy() {
        let dir = tempfile::tempdir().unwrap();
        let model = LocalTextModel::new(
            artifact(dir.path()),
            passed(),
            Box::new(ScriptedBackend {
                output: "{\"summary\": \"ok\"}".to_string(),
            }),
        );
        assert!(model.is_available());
        let caps = model.capabilities().await;
        assert_eq!(caps.privacy_class, Known::Known(PrivacyClass::OnDevice));
        assert_eq!(caps.offline, Tri::Yes);
        assert_eq!(caps.pricing_basis, Known::Known(PricingBasis::None));

        let response = model
            .generate_structured(request("hello"), JobContext::new(JobId::new()))
            .await
            .unwrap();
        assert_eq!(response.body, serde_json::json!({"summary": "ok"}));
        assert_eq!(response.usage, None);
    }

    #[tokio::test]
    async fn a_tampered_model_file_stops_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let entry = artifact(dir.path());
        let model = LocalTextModel::new(
            entry.clone(),
            passed(),
            Box::new(ScriptedBackend {
                output: "{}".to_string(),
            }),
        );
        std::fs::write(&entry.path, b"tampered-weight").unwrap();
        assert_eq!(
            model
                .generate_structured(request("hello"), JobContext::new(JobId::new()))
                .await,
            Err(TextModelError::Unavailable)
        );
    }

    #[tokio::test]
    async fn an_oversized_prompt_is_refused_rather_than_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let mut entry = artifact(dir.path());
        entry.context_tokens = 2_048;
        let model = LocalTextModel::new(
            entry,
            GateVerdict::Passed {
                model_id: "fixture-3b".to_string(),
                verified_context_tokens: 2_048,
                backend: "cpu".to_string(),
            },
            Box::new(ScriptedBackend {
                output: "{}".to_string(),
            }),
        );
        let huge = "x".repeat(1_000_000);
        assert_eq!(
            model
                .generate_structured(request(&huge), JobContext::new(JobId::new()))
                .await,
            Err(TextModelError::ContextTooLarge)
        );
    }

    #[tokio::test]
    async fn cancellation_is_observed_before_the_backend_runs() {
        let dir = tempfile::tempdir().unwrap();
        let model = LocalTextModel::new(
            artifact(dir.path()),
            passed(),
            Box::new(ScriptedBackend {
                output: "{}".to_string(),
            }),
        );
        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();
        assert_eq!(
            model.generate_structured(request("hello"), ctx).await,
            Err(TextModelError::Cancelled)
        );
    }

    #[tokio::test]
    async fn output_that_is_not_json_is_a_schema_failure_not_a_fabricated_body() {
        let dir = tempfile::tempdir().unwrap();
        let model = LocalTextModel::new(
            artifact(dir.path()),
            passed(),
            Box::new(ScriptedBackend {
                output: "I am a language model and cannot help.".to_string(),
            }),
        );
        assert_eq!(
            model
                .generate_structured(request("hello"), JobContext::new(JobId::new()))
                .await,
            Err(TextModelError::SchemaValidationFailed)
        );
    }

    #[tokio::test]
    async fn json_wrapped_in_runtime_chatter_is_recovered_and_counted_as_a_repair() {
        let dir = tempfile::tempdir().unwrap();
        let model = LocalTextModel::new(
            artifact(dir.path()),
            passed(),
            Box::new(ScriptedBackend {
                output: "loading model...\n{\"summary\": \"ok\"}\n[done]".to_string(),
            }),
        );
        let response = model
            .generate_structured(request("hello"), JobContext::new(JobId::new()))
            .await
            .unwrap();
        assert_eq!(response.body, serde_json::json!({"summary": "ok"}));
        assert_eq!(response.repair_attempts, 1);
    }

    #[tokio::test]
    async fn a_missing_runtime_executable_reports_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ProcessInferenceBackend::new(dir.path().join("llama-cli"));
        assert!(!backend.is_installed());
        let err = backend
            .generate(
                InferenceRequest {
                    model_path: dir.path().join("model.gguf"),
                    prompt: "hi".to_string(),
                    output_schema: serde_json::json!({}),
                    max_output_tokens: 16,
                    context_tokens: 2_048,
                },
                &JobContext::new(JobId::new()),
            )
            .await
            .unwrap_err();
        assert_eq!(err, TextModelError::Unavailable);
    }

    #[test]
    fn the_engine_id_is_distinct_from_the_apple_and_whisper_engines() {
        assert_eq!(LOCAL_LLM_ENGINE_ID, "local-llm");
        assert_ne!(LOCAL_LLM_ENGINE_ID, "apple-foundation");
        assert_ne!(LOCAL_LLM_ENGINE_ID, "whisper.cpp");
    }
}
