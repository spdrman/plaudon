//! OpenRouter REST contracts: typed request families, catalog records and
//! error classification.
//!
//! The crate deliberately contains no HTTP client. Spec 16.2A routes every
//! outbound connection through the egress gate in `plaudon-security`, and the
//! A1.2 spike only needs the *contract*: what a model's catalog entry can and
//! cannot tell us, how a request for each family is shaped, and how a
//! provider failure classifies.
//!
//! The finding that drives the design is in [`catalog`]: the public model
//! catalog reports audio as an **input modality** and nothing else. It does
//! not say whether a model implements the dedicated multipart transcription
//! endpoint or only accepts audio parts in a chat request. Therefore every
//! model starts at [`plaudon_ai_contracts::RequestFamily::Unknown`] and can
//! only be promoted by an observed request/response pair.

pub mod allowlist;
pub mod catalog;
pub mod client;
pub mod credential;
pub mod discovery;
pub mod error;
pub mod ledger;
pub mod pricing;
pub mod privacy;
pub mod registry;
pub mod request;
pub mod retry;
pub mod smoke;
pub mod transport;
pub mod wire;

pub use allowlist::{Allowlist, AllowlistEntry, ModelTask};
pub use catalog::{CatalogEntry, ModelProbeRecord, ProbeEvidence, TimestampSupport};
pub use client::{
    CloudDispatchAuthority, CloudDispatcher, DispatchRefused, JobBinding, OpenRouterSpeechToText,
    OpenRouterTextModel,
};
pub use credential::{ApiKey, ApiKeySource, CredentialError, VaultApiKey};
pub use discovery::{CatalogClient, DegradedReason, Discovered};
pub use error::{classify_http_status, OpenRouterError, Retriability};
pub use ledger::{AttemptId, AttemptIntent, AttemptLedger, AttemptUpdate, LedgerUnavailable};
pub use pricing::{CostEstimate, CostOrigin, PricingUnits, SettledCost};
pub use privacy::{PrivacyRefusal, PrivacyResolution, ZERO_RETENTION_CAVEAT};
pub use registry::{ModelCard, ModelRegistry, NotVerifiedReason, Verification};
pub use request::{
    AudioChatRequest, MultipartSttRequest, RequestBuildError, TextGenerationRequest,
};
pub use retry::{RetryDecision, RetryPolicy};
pub use smoke::{CapabilitySmokeTest, SmokeTestOutcome};
pub use transport::{AsyncHttpTransport, TransportFailure};

/// The model catalog path (spec 10.1).
pub const MODELS_PATH: &str = "/models";

/// Per-model route metadata path; `{author}/{slug}` is substituted in.
pub const MODEL_ENDPOINTS_PATH: &str = "/models/{model}/endpoints";

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-ai-openrouter";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;

/// Base URL of the OpenRouter API, for the probe script and documentation.
pub const API_BASE: &str = "https://openrouter.ai/api/v1";

/// The dedicated speech-to-text path (spec 2.2).
pub const TRANSCRIPTIONS_PATH: &str = "/audio/transcriptions";

/// The chat completion path used by the `audio_chat` family.
pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
