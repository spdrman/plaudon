//! The Rust half of the bridge envelope (spec 6.6, 11.2).
//!
//! Every document that crosses the C ABI is described here and in
//! `schemas/apple-bridge-envelope.v1.schema.json`; the header
//! `native/apple-bridge/include/plaudon_apple_bridge.h` states the ownership
//! and threading rules that make the exchange safe.
//!
//! This module is pure. It compiles and is tested on Windows, in CI, with no
//! Apple framework anywhere in the build, which is what makes the part of the
//! bridge that can be *wrong* testable without a Mac.
//!
//! Two decoding rules are load-bearing and are enforced by the types rather
//! than by review:
//!
//! * A document this build cannot fully understand is an error, never a
//!   success. An unparsed reply must never read as availability.
//! * An unrecognized error code decodes to
//!   [`BridgeErrorCode::Unrecognized`] and is treated exactly like
//!   `engine_failure`. A newer library can never talk an older caller into
//!   believing in a capability it cannot verify.

use serde::{Deserialize, Serialize};

/// Envelope contract version this build speaks.
pub const ENVELOPE_VERSION: u32 = 1;

/// ABI version this build requires from the Swift library.
pub const EXPECTED_ABI_VERSION: i32 = 2;

/// The operations the bridge implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeOp {
    /// Runtime availability of `SpeechTranscriber`.
    ProbeSpeech,
    /// Runtime availability of `SystemLanguageModel`.
    ProbeFoundationModel,
    /// Consent-gated download of speech assets for a locale.
    InstallSpeechAssets,
    /// File transcription through `SpeechAnalyzer`.
    TranscribeFile,
    /// On-device text generation.
    GenerateText,
}

impl BridgeOp {
    /// Wire name, matching the `op` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProbeSpeech => "probe_speech",
            Self::ProbeFoundationModel => "probe_foundation_model",
            Self::InstallSpeechAssets => "install_speech_assets",
            Self::TranscribeFile => "transcribe_file",
            Self::GenerateText => "generate_text",
        }
    }

    /// Every operation, for exhaustiveness tests.
    pub const ALL: [Self; 5] = [
        Self::ProbeSpeech,
        Self::ProbeFoundationModel,
        Self::InstallSpeechAssets,
        Self::TranscribeFile,
        Self::GenerateText,
    ];
}

/// The closed error vocabulary of the bridge.
///
/// The variant order matches the header's section 5 listing; the stability
/// test in `tests/abi_snapshot.rs` fails if the two diverge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BridgeErrorCode {
    /// Malformed envelope, or an operation the library does not implement.
    InvalidRequest,
    /// `envelope_version` outside the supported range.
    UnsupportedEnvelopeVersion,
    /// A field is present but unusable.
    InvalidArgument,
    /// Framework absent, or the OS predates the API.
    PlatformUnsupported,
    /// This Mac is not eligible for the feature.
    HardwareUnsupported,
    /// Apple Intelligence is switched off.
    DisabledByUser,
    /// The OS withheld an authorization.
    PermissionDenied,
    /// The locale is outside the supported set.
    LocaleUnsupported,
    /// Assets are absent and download was not authorized.
    ModelAssetMissing,
    /// Assets are downloading or being verified.
    ModelAssetNotReady,
    /// An authorized installation failed.
    AssetDownloadFailed,
    /// The platform's concurrent-use limit is reached.
    ConcurrencyLimit,
    /// The path does not exist or cannot be opened.
    InputUnreadable,
    /// The container or codec could not be decoded.
    UnsupportedMedia,
    /// Cooperative cancellation.
    Cancelled,
    /// The wall-clock budget elapsed.
    EngineTimeout,
    /// An Apple safety guardrail blocked the request.
    GuardrailViolation,
    /// The model declined to answer.
    Refusal,
    /// The prompt exceeded the model's context.
    ContextSizeExceeded,
    /// The platform throttled the request.
    RateLimited,
    /// The model does not support the language.
    UnsupportedLanguage,
    /// The guided-output schema cannot be expressed.
    SchemaUnsupported,
    /// The platform produced only volatile output.
    OutputNotFinal,
    /// Anything else.
    EngineFailure,
    /// A code this build does not know, carried verbatim for diagnostics.
    ///
    /// Behaves exactly like [`BridgeErrorCode::EngineFailure`] everywhere a
    /// decision is made.
    Unrecognized(String),
}

impl BridgeErrorCode {
    /// Every known code, in header order.
    pub const KNOWN: [Self; 24] = [
        Self::InvalidRequest,
        Self::UnsupportedEnvelopeVersion,
        Self::InvalidArgument,
        Self::PlatformUnsupported,
        Self::HardwareUnsupported,
        Self::DisabledByUser,
        Self::PermissionDenied,
        Self::LocaleUnsupported,
        Self::ModelAssetMissing,
        Self::ModelAssetNotReady,
        Self::AssetDownloadFailed,
        Self::ConcurrencyLimit,
        Self::InputUnreadable,
        Self::UnsupportedMedia,
        Self::Cancelled,
        Self::EngineTimeout,
        Self::GuardrailViolation,
        Self::Refusal,
        Self::ContextSizeExceeded,
        Self::RateLimited,
        Self::UnsupportedLanguage,
        Self::SchemaUnsupported,
        Self::OutputNotFinal,
        Self::EngineFailure,
    ];

    /// Wire name of a known code; `None` for an unrecognized one.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedEnvelopeVersion => "unsupported_envelope_version",
            Self::InvalidArgument => "invalid_argument",
            Self::PlatformUnsupported => "platform_unsupported",
            Self::HardwareUnsupported => "hardware_unsupported",
            Self::DisabledByUser => "disabled_by_user",
            Self::PermissionDenied => "permission_denied",
            Self::LocaleUnsupported => "locale_unsupported",
            Self::ModelAssetMissing => "model_asset_missing",
            Self::ModelAssetNotReady => "model_asset_not_ready",
            Self::AssetDownloadFailed => "asset_download_failed",
            Self::ConcurrencyLimit => "concurrency_limit",
            Self::InputUnreadable => "input_unreadable",
            Self::UnsupportedMedia => "unsupported_media",
            Self::Cancelled => "cancelled",
            Self::EngineTimeout => "engine_timeout",
            Self::GuardrailViolation => "guardrail_violation",
            Self::Refusal => "refusal",
            Self::ContextSizeExceeded => "context_size_exceeded",
            Self::RateLimited => "rate_limited",
            Self::UnsupportedLanguage => "unsupported_language",
            Self::SchemaUnsupported => "schema_unsupported",
            Self::OutputNotFinal => "output_not_final",
            Self::EngineFailure => "engine_failure",
            Self::Unrecognized(raw) => raw,
        }
    }

    /// Decode a wire name. Anything unknown is carried, never discarded.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        Self::KNOWN
            .iter()
            .find(|code| code.as_str() == raw)
            .cloned()
            .unwrap_or_else(|| Self::Unrecognized(raw.to_owned()))
    }

    /// Whether this code says the capability itself is absent, as opposed to
    /// this one call having failed.
    ///
    /// Used by the probe cache: a call-scoped failure must not poison the
    /// cached availability verdict.
    #[must_use]
    pub const fn is_capability_verdict(&self) -> bool {
        matches!(
            self,
            Self::PlatformUnsupported
                | Self::HardwareUnsupported
                | Self::DisabledByUser
                | Self::PermissionDenied
                | Self::LocaleUnsupported
                | Self::ModelAssetMissing
                | Self::ModelAssetNotReady
                | Self::UnsupportedLanguage
        )
    }
}

impl Serialize for BridgeErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BridgeErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::parse(&raw))
    }
}

/// A failure the bridge reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeError {
    /// Machine-readable reason.
    pub code: BridgeErrorCode,
    /// Short discriminator. Never user-facing prose, never a platform
    /// exception message (spec 6.6).
    #[serde(default)]
    pub detail: Option<String>,
    /// Whether the library believes an identical retry could succeed.
    #[serde(default)]
    pub retriable: bool,
}

impl BridgeError {
    /// Construct a failure, for tests and for locally generated refusals.
    #[must_use]
    pub fn new(code: BridgeErrorCode, detail: impl Into<Option<String>>) -> Self {
        let retriable = matches!(
            code,
            BridgeErrorCode::ModelAssetNotReady
                | BridgeErrorCode::AssetDownloadFailed
                | BridgeErrorCode::ConcurrencyLimit
                | BridgeErrorCode::EngineTimeout
                | BridgeErrorCode::RateLimited
                | BridgeErrorCode::EngineFailure
        );
        Self {
            code,
            detail: detail.into(),
            retriable,
        }
    }
}

/// Either half of a bridge reply.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeReply<T> {
    /// The call succeeded.
    Ok(T),
    /// The call failed.
    Err(BridgeError),
}

impl<T> BridgeReply<T> {
    /// The success payload, if any.
    #[must_use]
    pub const fn ok(&self) -> Option<&T> {
        match self {
            Self::Ok(value) => Some(value),
            Self::Err(_) => None,
        }
    }

    /// The failure, if any.
    #[must_use]
    pub const fn err(&self) -> Option<&BridgeError> {
        match self {
            Self::Ok(_) => None,
            Self::Err(error) => Some(error),
        }
    }
}

/// Response frame shared by every operation.
#[derive(Debug, Clone, Deserialize)]
struct ResponseFrame {
    envelope_version: u32,
    abi_version: i32,
    op: String,
    ok: bool,
    #[serde(default)]
    error: Option<BridgeError>,
    #[serde(default)]
    result: Option<serde_json::Value>,
}

impl<T: serde::de::DeserializeOwned> BridgeReply<T> {
    /// Parse a response document produced for `op`.
    ///
    /// A frame that does not validate is a failure, with a detail that says
    /// which rule it broke. The alternative — treating an unreadable document
    /// as partially usable — is how a product ends up reporting availability
    /// it never observed.
    #[must_use]
    pub fn parse(op: BridgeOp, document: &str) -> Self {
        let frame: ResponseFrame = match serde_json::from_str(document) {
            Ok(frame) => frame,
            Err(_) => {
                return Self::malformed("unparseable_document");
            }
        };
        if frame.envelope_version != ENVELOPE_VERSION {
            return Self::Err(BridgeError::new(
                BridgeErrorCode::UnsupportedEnvelopeVersion,
                Some(frame.envelope_version.to_string()),
            ));
        }
        if frame.abi_version != EXPECTED_ABI_VERSION {
            return Self::malformed("abi_version_mismatch");
        }
        if frame.op != op.as_str() {
            return Self::malformed("op_mismatch");
        }
        if frame.ok {
            let Some(value) = frame.result else {
                return Self::malformed("missing_result");
            };
            return match serde_json::from_value(value) {
                Ok(parsed) => Self::Ok(parsed),
                Err(_) => Self::malformed("unexpected_result_shape"),
            };
        }
        match frame.error {
            Some(error) => Self::Err(error),
            None => Self::malformed("missing_error"),
        }
    }

    fn malformed(detail: &str) -> Self {
        Self::Err(BridgeError::new(
            BridgeErrorCode::EngineFailure,
            Some(detail.to_owned()),
        ))
    }
}

/// Installation state of the speech assets for a locale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    /// Assets exist for this locale but are not installed.
    Supported,
    /// Installation is in progress.
    Downloading,
    /// Ready to use.
    Installed,
    /// The platform cannot serve this locale at all. Also the decoding of any
    /// status string this build does not recognize.
    #[default]
    #[serde(other)]
    Unsupported,
}

impl AssetStatus {
    /// Whether transcription can start without downloading anything.
    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Installed)
    }
}

/// An audio format the engine accepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptedFormat {
    /// Sample rate in hertz.
    pub sample_rate_hz: f64,
    /// Channel count.
    pub channels: u32,
}

/// Result of `probe_speech`.
///
/// The booleans are independent platform observations, not a state machine:
/// a host can be available without on-device support, report a transcriber
/// without an installed locale, and so on. An enum would make the impossible
/// combinations the representable ones.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeechProbe {
    /// Whether the framework itself is usable on this host.
    pub available: bool,
    /// Locales the platform can serve, installed or not.
    #[serde(default)]
    pub supported_locales: Vec<String>,
    /// Locales whose assets are already on disk.
    #[serde(default)]
    pub installed_locales: Vec<String>,
    /// Locale the request resolved to, when one was requested.
    #[serde(default)]
    pub resolved_locale: Option<String>,
    /// Asset state for the resolved locale, as `AssetInventory` reports it.
    ///
    /// Corroboration, not the gate: see [`SpeechProbe::locale_installed`].
    #[serde(default)]
    pub asset_status: AssetStatus,
    /// Whether the resolved locale appears in the platform's installed set.
    ///
    /// This is the readiness gate. Measured on macOS 27.0.1 (26A434):
    /// `AssetInventory.status(forModules:)` reported `supported` for a locale
    /// that was present in `installedLocales` and that transcribed
    /// successfully, so gating on the status would refuse work the machine
    /// can do. Recorded in `docs/spikes/A1.3-apple-availability.md`.
    #[serde(default)]
    pub locale_installed: bool,
    /// Whether recognition runs without leaving the machine.
    #[serde(default)]
    pub supports_on_device: bool,
    /// Always true; asserted so a text model can never be mistaken for one.
    #[serde(default)]
    pub is_transcriber: bool,
    /// Platform limit on simultaneously reserved locales.
    #[serde(default)]
    pub max_concurrent_locales: u32,
    /// Locales currently reserved by this host.
    #[serde(default)]
    pub reserved_locales: Vec<String>,
    /// Formats the engine declares.
    #[serde(default)]
    pub accepted_formats: Vec<AcceptedFormat>,
    /// Whether word-level timings are available.
    #[serde(default)]
    pub supports_word_timestamps: bool,
    /// OS build string, recorded with compatibility evidence.
    #[serde(default)]
    pub os_build: String,
    /// OS marketing version.
    #[serde(default)]
    pub os_version: String,
    /// CPU brand string, which decides Apple Intelligence eligibility.
    #[serde(default)]
    pub hardware: String,
}

/// Result of `probe_foundation_model`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextModelProbe {
    /// Whether the system language model is ready.
    pub available: bool,
    /// Always false (spec 2.3): this model never transcribes.
    #[serde(default)]
    pub is_transcriber: bool,
    /// Languages the model declares.
    #[serde(default)]
    pub supported_languages: Vec<String>,
    /// Whether the requested locale is covered, when one was requested.
    #[serde(default)]
    pub supports_requested_locale: Option<bool>,
    /// Whether guided structured output is available.
    #[serde(default)]
    pub supports_structured_output: bool,
    /// Context window, when the platform reports one. It currently does not.
    #[serde(default)]
    pub max_context_tokens: Option<u64>,
    /// Model version, when the platform reports one.
    #[serde(default)]
    pub model_version: Option<String>,
    /// OS build string.
    #[serde(default)]
    pub os_build: String,
    /// OS marketing version.
    #[serde(default)]
    pub os_version: String,
    /// CPU brand string.
    #[serde(default)]
    pub hardware: String,
}

/// One word with engine-reported timings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordTiming {
    /// The word, trimmed.
    pub text: String,
    /// Start offset in milliseconds.
    pub start_ms: u64,
    /// End offset in milliseconds.
    pub end_ms: u64,
}

/// One finalized transcript segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionSegment {
    /// Zero-based position.
    pub seq_index: u32,
    /// Start offset, when the engine reported one.
    #[serde(default)]
    pub start_ms: Option<u64>,
    /// End offset, when the engine reported one.
    #[serde(default)]
    pub end_ms: Option<u64>,
    /// Segment text, verbatim.
    pub text: String,
    /// Engine-scoped confidence, when reported.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Word timings, when requested and reported.
    #[serde(default)]
    pub words: Vec<WordTiming>,
}

/// Result of `transcribe_file`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcription {
    /// Full text.
    pub text: String,
    /// Ordered segments.
    #[serde(default)]
    pub segments: Vec<TranscriptionSegment>,
    /// Where the offsets came from (spec 8.6A).
    pub alignment_origin: plaudon_domain::transcript::AlignmentOrigin,
    /// Locale the run used.
    pub locale: String,
    /// Media duration as the engine measured it.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Always true; a volatile result is an `output_not_final` error instead.
    ///
    /// Named `final` on the wire, where it reads naturally, and renamed here
    /// because `final` is a reserved word in Rust.
    #[serde(default, rename = "final")]
    pub final_result: bool,
}

/// Result of `generate_text`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Generation {
    /// Generated text, or the JSON document when guided generation was used.
    pub text: String,
    /// Whether the platform generated under a guided schema.
    #[serde(default)]
    pub structured: bool,
    /// Model version, when reported.
    #[serde(default)]
    pub model_version: Option<String>,
    /// Context window the platform revealed, when it did.
    #[serde(default)]
    pub context_tokens: Option<u64>,
    /// Input tokens, when reported.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Output tokens, when reported.
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

/// Result of `install_speech_assets`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetInstall {
    /// State after the attempt.
    pub asset_status: AssetStatus,
    /// Locale the assets belong to.
    pub locale: String,
    /// Installed size, when the platform reports one.
    #[serde(default)]
    pub installed_bytes: Option<u64>,
}

/// Phase a progress document belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressPhase {
    /// Downloading speech assets.
    AssetDownload,
    /// Transcribing audio.
    Transcribe,
    /// Generating text.
    Generate,
    /// A phase this build does not model.
    #[default]
    #[serde(other)]
    Unknown,
}

/// A progress notification from the bridge.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProgressEvent {
    /// Operation reporting progress.
    #[serde(default)]
    pub op: Option<String>,
    /// Phase within the operation.
    #[serde(default)]
    pub phase: ProgressPhase,
    /// Completed fraction, when the platform can estimate one.
    #[serde(default)]
    pub fraction: Option<f32>,
    /// Milliseconds of audio processed.
    #[serde(default)]
    pub processed_ms: Option<u64>,
    /// Completed download units.
    #[serde(default)]
    pub completed_units: Option<u64>,
    /// Total download units.
    #[serde(default)]
    pub total_units: Option<u64>,
}

impl ProgressEvent {
    /// Parse a progress document, rejecting anything this build cannot read.
    ///
    /// A progress update is advisory, so an undecodable one is dropped rather
    /// than escalated: spec 6.5 says a dropped update always beats a stalled
    /// transcription.
    #[must_use]
    pub fn parse(document: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(document).ok()?;
        let version = value.get("envelope_version")?.as_u64()?;
        if u32::try_from(version).ok()? != ENVELOPE_VERSION {
            return None;
        }
        serde_json::from_value(value).ok()
    }
}

// MARK: - Requests

/// Request for either probe.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProbeRequest {
    /// Locale to resolve, or `None` for the system locale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

/// Request for `install_speech_assets`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InstallAssetsRequest {
    /// Locale whose assets to install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Cancellation handle, or 0.
    pub cancel_token: u64,
    /// Wall-clock budget; 0 selects the library default.
    pub timeout_ms: u64,
}

/// Request for `transcribe_file`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscribeRequest {
    /// Absolute path of the media file.
    pub path: String,
    /// Locale hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Cancellation handle, or 0.
    pub cancel_token: u64,
    /// Wall-clock budget; 0 selects the library default.
    pub timeout_ms: u64,
    /// Whether the user authorized an asset download for this run (spec 12).
    pub allow_asset_download: bool,
    /// Whether word timings are wanted.
    pub want_word_timestamps: bool,
}

/// Request for `generate_text`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerateRequest {
    /// Fully rendered prompt.
    pub prompt: String,
    /// System instructions kept separate from untrusted transcript text
    /// (spec 9.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// JSON Schema for guided generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    /// Cancellation handle, or 0.
    pub cancel_token: u64,
    /// Wall-clock budget; 0 selects the library default.
    pub timeout_ms: u64,
    /// Output ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// Serialize a request body into a complete envelope.
///
/// Infallible by construction: every request type is a plain struct of
/// JSON-representable fields, so the only way this could fail is a bug in the
/// type definitions, which the round-trip tests catch.
pub fn encode_request<T: Serialize + ?Sized>(op: BridgeOp, body: &T) -> String {
    let mut value = serde_json::to_value(body).unwrap_or(serde_json::Value::Null);
    let map = value.as_object_mut().expect("request bodies are objects");
    map.insert("envelope_version".into(), ENVELOPE_VERSION.into());
    map.insert("op".into(), op.as_str().into());
    serde_json::to_string(&value).expect("a JSON object always serializes")
}

#[cfg(test)]
mod tests {
    use plaudon_domain::transcript::AlignmentOrigin;

    use super::*;

    fn ok_frame(op: BridgeOp, result: &serde_json::Value) -> String {
        serde_json::json!({
            "envelope_version": ENVELOPE_VERSION,
            "abi_version": EXPECTED_ABI_VERSION,
            "op": op.as_str(),
            "ok": true,
            "result": result,
        })
        .to_string()
    }

    #[test]
    fn every_known_code_round_trips_through_its_wire_name() {
        for code in BridgeErrorCode::KNOWN {
            let parsed = BridgeErrorCode::parse(code.as_str());
            assert_eq!(parsed, code, "{} did not round trip", code.as_str());
        }
    }

    #[test]
    fn an_unknown_code_is_carried_and_never_becomes_a_known_one() {
        let code = BridgeErrorCode::parse("telepathic_refusal");
        assert_eq!(
            code,
            BridgeErrorCode::Unrecognized("telepathic_refusal".into())
        );
        assert!(!code.is_capability_verdict());
        assert_eq!(code.as_str(), "telepathic_refusal");
    }

    #[test]
    fn an_unparseable_document_is_a_failure_not_availability() {
        let reply = BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, "{{{");
        let error = reply.err().expect("must not parse as success");
        assert_eq!(error.code, BridgeErrorCode::EngineFailure);
        assert_eq!(error.detail.as_deref(), Some("unparseable_document"));
    }

    #[test]
    fn a_mismatched_abi_version_is_refused() {
        let document = serde_json::json!({
            "envelope_version": ENVELOPE_VERSION,
            "abi_version": EXPECTED_ABI_VERSION + 1,
            "op": "probe_speech",
            "ok": true,
            "result": { "available": true },
        })
        .to_string();
        let reply = BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document);
        assert_eq!(
            reply.err().map(|e| e.detail.clone()),
            Some(Some("abi_version_mismatch".into()))
        );
    }

    #[test]
    fn a_future_envelope_version_is_refused_by_name() {
        let document = serde_json::json!({
            "envelope_version": ENVELOPE_VERSION + 7,
            "abi_version": EXPECTED_ABI_VERSION,
            "op": "probe_speech",
            "ok": true,
            "result": {},
        })
        .to_string();
        let reply = BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document);
        assert_eq!(
            reply.err().map(|e| e.code.clone()),
            Some(BridgeErrorCode::UnsupportedEnvelopeVersion)
        );
    }

    #[test]
    fn a_response_for_another_operation_is_refused() {
        let document = ok_frame(BridgeOp::GenerateText, &serde_json::json!({ "text": "x" }));
        let reply = BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document);
        assert_eq!(
            reply.err().map(|e| e.detail.clone()),
            Some(Some("op_mismatch".into()))
        );
    }

    #[test]
    fn a_success_frame_without_a_result_is_refused() {
        let document = serde_json::json!({
            "envelope_version": ENVELOPE_VERSION,
            "abi_version": EXPECTED_ABI_VERSION,
            "op": "probe_speech",
            "ok": true,
        })
        .to_string();
        let reply = BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document);
        assert_eq!(
            reply.err().map(|e| e.detail.clone()),
            Some(Some("missing_result".into()))
        );
    }

    #[test]
    fn a_failure_frame_without_an_error_is_refused() {
        let document = serde_json::json!({
            "envelope_version": ENVELOPE_VERSION,
            "abi_version": EXPECTED_ABI_VERSION,
            "op": "probe_speech",
            "ok": false,
        })
        .to_string();
        let reply = BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document);
        assert_eq!(
            reply.err().map(|e| e.detail.clone()),
            Some(Some("missing_error".into()))
        );
    }

    #[test]
    fn unknown_result_fields_do_not_break_decoding() {
        let document = ok_frame(
            BridgeOp::ProbeFoundationModel,
            &serde_json::json!({
                "available": true,
                "is_transcriber": false,
                "a_field_from_the_future": [1, 2, 3],
            }),
        );
        let reply = BridgeReply::<TextModelProbe>::parse(BridgeOp::ProbeFoundationModel, &document);
        let probe = reply.ok().expect("forward-compatible decode");
        assert!(probe.available);
        // Unreported capabilities stay at their closed default.
        assert!(!probe.supports_structured_output);
        assert_eq!(probe.max_context_tokens, None);
    }

    #[test]
    fn missing_optional_fields_decode_to_the_closed_default() {
        let document = ok_frame(
            BridgeOp::ProbeSpeech,
            &serde_json::json!({ "available": true }),
        );
        let probe = BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document)
            .ok()
            .cloned()
            .expect("decodes");
        assert_eq!(probe.asset_status, AssetStatus::Unsupported);
        assert!(!probe.supports_on_device);
        assert!(!probe.supports_word_timestamps);
        assert_eq!(probe.max_concurrent_locales, 0);
        assert_eq!(probe.supported_locales, Vec::<String>::new());
    }

    #[test]
    fn an_unknown_asset_status_is_unsupported() {
        let decoded: AssetStatus = serde_json::from_str("\"quantum_installed\"").unwrap();
        assert_eq!(decoded, AssetStatus::Unsupported);
        assert!(!decoded.is_ready());
    }

    #[test]
    fn an_unknown_progress_phase_decodes_to_unknown() {
        let event = ProgressEvent::parse(
            r#"{"envelope_version":1,"op":"transcribe_file","phase":"telepathy","fraction":0.5}"#,
        )
        .expect("decodes");
        assert_eq!(event.phase, ProgressPhase::Unknown);
        assert_eq!(event.fraction, Some(0.5));
    }

    #[test]
    fn a_progress_document_from_another_envelope_version_is_dropped() {
        assert!(ProgressEvent::parse(r#"{"envelope_version":2,"phase":"transcribe"}"#).is_none());
        assert!(ProgressEvent::parse("not json").is_none());
    }

    #[test]
    fn a_transcription_decodes_with_times_and_words() {
        let document = ok_frame(
            BridgeOp::TranscribeFile,
            &serde_json::json!({
                "text": "hello world",
                "alignment_origin": "native",
                "locale": "en_US",
                "final": true,
                "duration_ms": 2000,
                "segments": [{
                    "seq_index": 0,
                    "start_ms": 0,
                    "end_ms": 900,
                    "text": "hello world",
                    "confidence": 0.8,
                    "words": [{ "text": "hello", "start_ms": 0, "end_ms": 400 }],
                }],
            }),
        );
        let result = BridgeReply::<Transcription>::parse(BridgeOp::TranscribeFile, &document)
            .ok()
            .cloned()
            .expect("decodes");
        assert_eq!(result.alignment_origin, AlignmentOrigin::Native);
        assert_eq!(result.segments[0].words.len(), 1);
        assert_eq!(result.segments[0].confidence, Some(0.8));
    }

    #[test]
    fn requests_carry_the_envelope_frame() {
        let encoded = encode_request(
            BridgeOp::TranscribeFile,
            &TranscribeRequest {
                path: "/tmp/a.wav".into(),
                locale: Some("en-US".into()),
                cancel_token: 7,
                timeout_ms: 1000,
                allow_asset_download: false,
                want_word_timestamps: true,
            },
        );
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["envelope_version"], ENVELOPE_VERSION);
        assert_eq!(value["op"], "transcribe_file");
        assert_eq!(value["cancel_token"], 7);
        assert_eq!(value["allow_asset_download"], false);
    }

    #[test]
    fn an_absent_optional_request_field_is_omitted_rather_than_nulled() {
        let encoded = encode_request(BridgeOp::ProbeSpeech, &ProbeRequest { locale: None });
        assert!(
            !encoded.contains("locale"),
            "an omitted locale must not appear as null: {encoded}"
        );
    }

    #[test]
    fn capability_verdicts_are_distinguished_from_call_failures() {
        assert!(BridgeErrorCode::HardwareUnsupported.is_capability_verdict());
        assert!(BridgeErrorCode::DisabledByUser.is_capability_verdict());
        assert!(!BridgeErrorCode::EngineTimeout.is_capability_verdict());
        assert!(!BridgeErrorCode::Cancelled.is_capability_verdict());
    }
}
