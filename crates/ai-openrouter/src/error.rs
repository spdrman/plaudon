//! Provider error classification (spec 10.6, 10.7).
//!
//! The single rule: once bytes have been transmitted, an unknown outcome is
//! never retried automatically, because the provider may already have billed
//! the request.

use plaudon_ai_contracts::stt::SttError;
use plaudon_domain::capability::DispatchState;
use plaudon_domain::error::{AppError, AppErrorCode};
use serde::{Deserialize, Serialize};

/// Whether an attempt may be retried, and by whom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retriability {
    /// Safe for the engine to retry without asking.
    AutomaticRetry,
    /// May be retried only after the user acknowledges a possible second charge.
    RetryNeedsCostAcknowledgement,
    /// Must not be retried.
    Terminal,
}

/// A classified OpenRouter failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenRouterError {
    /// The API key is missing, malformed or rejected.
    #[error("the provider rejected the credential")]
    Unauthorized,
    /// The account has no credit.
    #[error("the provider reports insufficient credit")]
    InsufficientCredit,
    /// The provider asked us to slow down.
    #[error("the provider rate limited the request")]
    RateLimited {
        /// Seconds the provider asked us to wait, when it said.
        retry_after_seconds: Option<u32>,
    },
    /// The model identifier is not known to the provider.
    #[error("the provider does not know this model")]
    UnknownModel,
    /// The provider refused the request on content or policy grounds.
    #[error("the provider refused the request")]
    Rejected,
    /// The payload exceeded a provider-side limit.
    #[error("the provider rejected the payload size")]
    PayloadTooLarge,
    /// A provider-side error that may clear on its own.
    #[error("the provider reported a transient failure")]
    ProviderUnavailable,
    /// The response body was not the documented shape.
    #[error("the provider returned output we could not interpret")]
    MalformedResponse,
    /// The response was truncated mid-stream.
    #[error("the provider response was truncated")]
    TruncatedResponse,
    /// Bytes were transmitted and no usable response arrived.
    #[error("the request was transmitted but the outcome is unknown")]
    OutcomeUnknown,
    /// The request never left the machine.
    #[error("the request failed before any byte was transmitted")]
    FailedBeforeSend,
}

impl OpenRouterError {
    /// How this failure may be retried.
    #[must_use]
    pub const fn retriability(&self) -> Retriability {
        match self {
            Self::RateLimited { .. } | Self::ProviderUnavailable | Self::FailedBeforeSend => {
                Retriability::AutomaticRetry
            }
            // A truncated or malformed response means the provider did work,
            // so a retry can be billed twice.
            Self::MalformedResponse | Self::TruncatedResponse => {
                Retriability::RetryNeedsCostAcknowledgement
            }
            Self::Unauthorized
            | Self::InsufficientCredit
            | Self::UnknownModel
            | Self::Rejected
            | Self::PayloadTooLarge
            | Self::OutcomeUnknown => Retriability::Terminal,
        }
    }

    /// The dispatch state this failure leaves the attempt in.
    #[must_use]
    pub const fn dispatch_state(&self) -> DispatchState {
        match self {
            Self::Unauthorized
            | Self::InsufficientCredit
            | Self::UnknownModel
            | Self::PayloadTooLarge
            | Self::FailedBeforeSend => DispatchState::FailedBeforeSend,
            Self::RateLimited { .. } | Self::ProviderUnavailable | Self::Rejected => {
                DispatchState::AcceptedByProvider
            }
            Self::MalformedResponse | Self::TruncatedResponse | Self::OutcomeUnknown => {
                DispatchState::OutcomeUnknown
            }
        }
    }

    /// The stable application code.
    #[must_use]
    pub const fn code(&self) -> AppErrorCode {
        match self {
            Self::Unauthorized => AppErrorCode::AuthenticationRequired,
            Self::InsufficientCredit => AppErrorCode::BudgetExceeded,
            Self::RateLimited { .. } => AppErrorCode::RemoteRateLimited,
            Self::UnknownModel => AppErrorCode::ModelUnavailable,
            Self::Rejected => AppErrorCode::ProviderRejected,
            Self::PayloadTooLarge => AppErrorCode::ContextTooLarge,
            Self::ProviderUnavailable | Self::FailedBeforeSend => AppErrorCode::SourceUnavailable,
            Self::MalformedResponse | Self::TruncatedResponse => AppErrorCode::MalformedOutput,
            Self::OutcomeUnknown => AppErrorCode::OutcomeUnknown,
        }
    }
}

impl From<OpenRouterError> for AppError {
    fn from(error: OpenRouterError) -> Self {
        let code = error.code();
        Self::new(code, error.to_string())
    }
}

impl From<OpenRouterError> for SttError {
    fn from(error: OpenRouterError) -> Self {
        match error {
            OpenRouterError::Unauthorized | OpenRouterError::InsufficientCredit => {
                Self::ConsentRequired
            }
            OpenRouterError::UnknownModel
            | OpenRouterError::ProviderUnavailable
            | OpenRouterError::FailedBeforeSend => Self::Unavailable,
            OpenRouterError::RateLimited { .. } | OpenRouterError::Rejected => {
                Self::ProviderRejected
            }
            OpenRouterError::PayloadTooLarge => Self::InputTooLarge,
            OpenRouterError::MalformedResponse | OpenRouterError::TruncatedResponse => {
                Self::MalformedOutput
            }
            OpenRouterError::OutcomeUnknown => Self::OutcomeUnknown,
        }
    }
}

/// Classify an HTTP status into a typed failure.
///
/// `bytes_transmitted` distinguishes a request that never left the machine
/// from one the provider may have started billing.
#[must_use]
pub fn classify_http_status(
    status: u16,
    retry_after_seconds: Option<u32>,
    bytes_transmitted: bool,
) -> OpenRouterError {
    match status {
        401 | 403 => OpenRouterError::Unauthorized,
        402 => OpenRouterError::InsufficientCredit,
        404 => OpenRouterError::UnknownModel,
        413 => OpenRouterError::PayloadTooLarge,
        429 => OpenRouterError::RateLimited {
            retry_after_seconds,
        },
        400 | 422 => OpenRouterError::Rejected,
        502..=504 => OpenRouterError::ProviderUnavailable,
        // Everything else, including an unrecognized status, is judged by
        // whether anything actually left the machine.
        _ => {
            if bytes_transmitted {
                OpenRouterError::OutcomeUnknown
            } else {
                OpenRouterError::FailedBeforeSend
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_statuses_classify_as_expected() {
        assert_eq!(
            classify_http_status(401, None, false),
            OpenRouterError::Unauthorized
        );
        assert_eq!(
            classify_http_status(402, None, false),
            OpenRouterError::InsufficientCredit
        );
        assert_eq!(
            classify_http_status(404, None, false),
            OpenRouterError::UnknownModel
        );
        assert_eq!(
            classify_http_status(413, None, true),
            OpenRouterError::PayloadTooLarge
        );
        assert_eq!(
            classify_http_status(429, Some(30), true),
            OpenRouterError::RateLimited {
                retry_after_seconds: Some(30)
            }
        );
        assert_eq!(
            classify_http_status(503, None, true),
            OpenRouterError::ProviderUnavailable
        );
    }

    #[test]
    fn an_unrecognized_status_after_transmission_is_an_unknown_outcome() {
        assert_eq!(
            classify_http_status(599, None, true),
            OpenRouterError::OutcomeUnknown
        );
        assert_eq!(
            classify_http_status(599, None, false),
            OpenRouterError::FailedBeforeSend
        );
    }

    #[test]
    fn nothing_that_may_have_been_billed_retries_automatically() {
        for error in [
            OpenRouterError::OutcomeUnknown,
            OpenRouterError::MalformedResponse,
            OpenRouterError::TruncatedResponse,
        ] {
            assert_ne!(
                error.retriability(),
                Retriability::AutomaticRetry,
                "{error:?} may have been billed"
            );
            assert!(!error.dispatch_state().auto_retry_safe());
        }
    }

    #[test]
    fn only_pre_dispatch_failures_are_automatically_retriable() {
        assert_eq!(
            OpenRouterError::FailedBeforeSend.retriability(),
            Retriability::AutomaticRetry
        );
        assert!(OpenRouterError::FailedBeforeSend
            .dispatch_state()
            .auto_retry_safe());
        assert_eq!(
            OpenRouterError::RateLimited {
                retry_after_seconds: None
            }
            .retriability(),
            Retriability::AutomaticRetry
        );
    }

    #[test]
    fn insufficient_credit_is_terminal_and_reported_as_a_budget_problem() {
        let error = OpenRouterError::InsufficientCredit;
        assert_eq!(error.retriability(), Retriability::Terminal);
        assert_eq!(error.code(), AppErrorCode::BudgetExceeded);
        assert!(!AppError::from(error).is_retryable());
    }

    #[test]
    fn an_unknown_outcome_reaches_the_stt_contract_as_an_unknown_outcome() {
        assert_eq!(
            SttError::from(OpenRouterError::OutcomeUnknown),
            SttError::OutcomeUnknown
        );
        assert_eq!(
            SttError::from(OpenRouterError::PayloadTooLarge),
            SttError::InputTooLarge
        );
    }
}
