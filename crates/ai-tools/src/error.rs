//! Failures a transcript tool can report (spec 15.4).
//!
//! Every variant maps onto an existing stable [`AppErrorCode`]. The taxonomy is
//! a closed set, so this crate maps into it rather than extending it.

use plaudon_domain::error::{AppError, AppErrorCode};

use crate::anchor::AnchorRejection;

/// Why a tool run could not produce a usable result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// No tool with that identity and version is registered.
    #[error("no such tool")]
    UnknownTool,
    /// The selected model does not meet the tool's required capabilities.
    #[error("the selected model does not satisfy this tool's requirements")]
    CapabilityUnmet,
    /// The tool's model policy forbids this route.
    #[error("this model route is not permitted for this tool")]
    PolicyForbidden,
    /// Cloud consent is required and absent.
    #[error("consent is required before this tool may run")]
    ConsentRequired,
    /// Local-only Mode forbids the dispatch this tool needs.
    #[error("local-only mode refused this tool's network dispatch")]
    LocalOnlyMode,
    /// The model output did not satisfy the tool's output schema.
    #[error("the model output did not satisfy the tool's output schema")]
    SchemaValidationFailed,
    /// Every generated item failed anchor verification.
    #[error("generated output cited {0} unverifiable anchors and was rejected")]
    UngroundedOutput(usize),
    /// An anchor was refused during verification.
    #[error("an anchor was refused: {0:?}")]
    AnchorRejected(AnchorRejection),
    /// The run was cancelled.
    #[error("the tool run was cancelled")]
    Canceled,
    /// The input exceeds what the selected model accepts.
    #[error("the transcript exceeds the selected model's context")]
    ContextTooLarge,
    /// The engine failed in a way the adapter reported as unavailable.
    #[error("the selected model is unavailable")]
    ModelUnavailable,
    /// Bytes were transmitted but no result arrived.
    #[error("the request was transmitted but the outcome is unknown")]
    OutcomeUnknown,
    /// The provider or a platform guardrail refused the request.
    #[error("the provider refused the request")]
    ProviderRejected,
    /// A caller supplied an argument the contract forbids.
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
    /// A user-supplied tool configuration could not be loaded.
    #[error("the tool configuration could not be loaded: {0}")]
    ConfigurationInvalid(String),
}

impl From<ToolError> for AppError {
    fn from(error: ToolError) -> Self {
        let code = match error {
            ToolError::UnknownTool => AppErrorCode::RecordNotFound,
            ToolError::CapabilityUnmet | ToolError::UngroundedOutput(_) => {
                AppErrorCode::CapabilityUnverified
            }
            ToolError::PolicyForbidden => AppErrorCode::PolicyUnresolved,
            ToolError::ConsentRequired => AppErrorCode::ConsentRequired,
            ToolError::LocalOnlyMode => AppErrorCode::LocalOnlyMode,
            ToolError::SchemaValidationFailed => AppErrorCode::SchemaValidationFailed,
            ToolError::AnchorRejected(_)
            | ToolError::InvalidArgument(_)
            | ToolError::ConfigurationInvalid(_) => AppErrorCode::InvalidArgument,
            ToolError::Canceled => AppErrorCode::Canceled,
            ToolError::ContextTooLarge => AppErrorCode::ContextTooLarge,
            ToolError::ModelUnavailable => AppErrorCode::ModelUnavailable,
            ToolError::OutcomeUnknown => AppErrorCode::OutcomeUnknown,
            ToolError::ProviderRejected => AppErrorCode::ProviderRejected,
        };
        Self::new(code, error.to_string())
    }
}

impl From<plaudon_ai_contracts::text::TextModelError> for ToolError {
    fn from(error: plaudon_ai_contracts::text::TextModelError) -> Self {
        use plaudon_ai_contracts::text::TextModelError as E;
        match error {
            E::Cancelled => Self::Canceled,
            E::Unavailable => Self::ModelUnavailable,
            E::ContextTooLarge => Self::ContextTooLarge,
            E::ConsentRequired => Self::ConsentRequired,
            E::PolicyUnresolved => Self::PolicyForbidden,
            E::ProviderRejected => Self::ProviderRejected,
            E::OutcomeUnknown => Self::OutcomeUnknown,
            E::SchemaValidationFailed => Self::SchemaValidationFailed,
            E::EngineFailure => Self::ModelUnavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ungrounded_output_is_an_error_not_a_quiet_success() {
        let error = AppError::from(ToolError::UngroundedOutput(3));
        assert_eq!(error.code(), AppErrorCode::CapabilityUnverified);
    }

    #[test]
    fn a_lost_response_never_maps_to_a_retryable_code() {
        let error = AppError::from(ToolError::OutcomeUnknown);
        assert_eq!(error.code(), AppErrorCode::OutcomeUnknown);
        assert!(!error.code().is_retryable());
    }

    #[test]
    fn consent_and_local_only_refusals_stay_distinguishable() {
        assert_eq!(
            AppError::from(ToolError::ConsentRequired).code(),
            AppErrorCode::ConsentRequired
        );
        assert_eq!(
            AppError::from(ToolError::LocalOnlyMode).code(),
            AppErrorCode::LocalOnlyMode
        );
    }
}
