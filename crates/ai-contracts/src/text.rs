//! The text-model contract (spec 6.5, 9.5).

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode};
use serde::{Deserialize, Serialize};

use crate::capabilities::TextModelCapabilities;
use crate::job::JobContext;

/// A structured generation request.
///
/// The output schema is mandatory: spec 9.5 requires structured output
/// contracts with validation and bounded repair rather than free-form text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredRequest {
    /// Version of the prompt template that produced `prompt`.
    pub prompt_version: String,
    /// Fully rendered prompt.
    pub prompt: String,
    /// Version of `output_schema`.
    pub schema_version: String,
    /// JSON Schema the response must satisfy.
    pub output_schema: serde_json::Value,
    /// Maximum output tokens, when the caller wants to bound cost.
    pub max_output_tokens: Option<u64>,
}

/// A validated structured response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredResponse {
    /// Response body, already validated against `output_schema`.
    pub body: serde_json::Value,
    /// Number of bounded repair attempts that were needed.
    pub repair_attempts: u8,
    /// Provider-reported usage, verbatim, when available.
    pub usage: Option<serde_json::Value>,
}

/// Failures a text model may report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextModelError {
    /// The run was cancelled.
    #[error("generation was cancelled")]
    Cancelled,
    /// The model or its assets are unavailable.
    #[error("the text model is unavailable")]
    Unavailable,
    /// The prompt exceeds the model's context window.
    #[error("the prompt exceeds the model context window")]
    ContextTooLarge,
    /// Consent has not been granted for this model's privacy class.
    #[error("consent is required before this model may run")]
    ConsentRequired,
    /// The route's privacy policy could not be resolved.
    #[error("the privacy route for this model could not be resolved")]
    PolicyUnresolved,
    /// The provider or a platform guardrail refused the request.
    ///
    /// Spec 11.4: a guardrail refusal is an error, never a successful result.
    #[error("the provider refused the request")]
    ProviderRejected,
    /// Bytes were transmitted but no result arrived.
    #[error("the request was transmitted but the outcome is unknown")]
    OutcomeUnknown,
    /// Output failed schema validation even after bounded repair.
    #[error("the model output did not satisfy the required schema")]
    SchemaValidationFailed,
    /// The model failed in a way this build does not model.
    #[error("the text model failed")]
    EngineFailure,
}

impl From<TextModelError> for AppError {
    fn from(error: TextModelError) -> Self {
        let code = match error {
            TextModelError::Cancelled => AppErrorCode::Canceled,
            TextModelError::Unavailable => AppErrorCode::ModelUnavailable,
            TextModelError::ContextTooLarge => AppErrorCode::ContextTooLarge,
            TextModelError::ConsentRequired => AppErrorCode::ConsentRequired,
            TextModelError::PolicyUnresolved => AppErrorCode::PolicyUnresolved,
            TextModelError::ProviderRejected => AppErrorCode::ProviderRejected,
            TextModelError::OutcomeUnknown => AppErrorCode::OutcomeUnknown,
            TextModelError::SchemaValidationFailed => AppErrorCode::SchemaValidationFailed,
            TextModelError::EngineFailure => AppErrorCode::InternalError,
        };
        Self::new(code, error.to_string())
    }
}

/// A structured text generation model (spec 6.5).
#[async_trait]
pub trait TextModel: Send + Sync {
    /// Runtime-probed capabilities.
    async fn capabilities(&self) -> TextModelCapabilities;

    /// Generate a schema-validated response.
    async fn generate_structured(
        &self,
        request: StructuredRequest,
        ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guardrail_refusal_is_an_error_not_a_success() {
        let error = AppError::from(TextModelError::ProviderRejected);
        assert_eq!(error.code(), AppErrorCode::ProviderRejected);
    }

    #[test]
    fn every_text_model_error_maps_to_a_distinct_stable_code() {
        let errors = [
            TextModelError::Cancelled,
            TextModelError::Unavailable,
            TextModelError::ContextTooLarge,
            TextModelError::ConsentRequired,
            TextModelError::PolicyUnresolved,
            TextModelError::ProviderRejected,
            TextModelError::OutcomeUnknown,
            TextModelError::SchemaValidationFailed,
            TextModelError::EngineFailure,
        ];
        let codes: std::collections::BTreeSet<_> = errors
            .iter()
            .cloned()
            .map(|e| AppError::from(e).code())
            .collect();
        assert_eq!(codes.len(), errors.len());
    }
}
