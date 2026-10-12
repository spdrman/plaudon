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

pub mod catalog;
pub mod error;
pub mod request;

pub use catalog::{CatalogEntry, ModelProbeRecord, ProbeEvidence, TimestampSupport};
pub use error::{classify_http_status, OpenRouterError, Retriability};
pub use request::{AudioChatRequest, MultipartSttRequest, RequestBuildError};

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
