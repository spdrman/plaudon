//! Capability declarations for speech and text engines (spec 8.3, 8.6, 10.5).

use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::model::{EngineId, ModelIdentity};
use serde::{Deserialize, Serialize};

/// Which transport contract an audio-capable model actually implements (spec 8.6).
///
/// A model catalog saying "audio capable" does not say which endpoint contract
/// applies, so this is probed per model and defaults to `Unknown`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RequestFamily {
    /// A dedicated multipart speech-to-text endpoint.
    MultipartStt,
    /// A chat or multimodal endpoint that accepts audio parts.
    AudioChat,
    /// Probed and found not to accept audio at all.
    Unsupported,
    /// Not probed, or the probe was inconclusive.
    #[default]
    #[serde(other)]
    Unknown,
}

impl RequestFamily {
    /// Whether a request builder exists for this family.
    ///
    /// Fail-closed: `Unsupported` and `Unknown` have no builder, so no request
    /// can be constructed for them (spec 8.6A).
    #[must_use]
    pub const fn has_builder(self) -> bool {
        matches!(self, Self::MultipartStt | Self::AudioChat)
    }
}

/// A container/codec pair an engine accepts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MediaFormat {
    /// Container name, lowercase (`wav`, `mp3`, `mp4`, `ogg`).
    pub container: String,
    /// Codec name, lowercase (`pcm_s16le`, `mp3`, `aac`, `opus`).
    pub codec: String,
}

impl MediaFormat {
    /// Construct a format pair.
    #[must_use]
    pub fn new(container: impl Into<String>, codec: impl Into<String>) -> Self {
        Self {
            container: container.into(),
            codec: codec.into(),
        }
    }
}

/// Where audio or text is processed, for consent and routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    /// Runs entirely on this machine.
    OnDevice,
    /// Leaves the machine; the provider published a zero-retention route.
    CloudZeroRetention,
    /// Leaves the machine; the provider may retain data.
    CloudRetaining,
    /// Routing could not be resolved. Treated as "do not dispatch".
    #[serde(other)]
    Unresolved,
}

impl PrivacyClass {
    /// Whether this class requires explicit cloud consent before dispatch.
    #[must_use]
    pub const fn requires_cloud_consent(self) -> bool {
        !matches!(self, Self::OnDevice)
    }

    /// Whether a dispatch may proceed at all.
    ///
    /// Spec 10.8 fails closed when a privacy policy cannot be resolved for the
    /// exact endpoint route.
    #[must_use]
    pub const fn is_dispatchable(self) -> bool {
        !matches!(self, Self::Unresolved)
    }
}

/// How a provider charges for a request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingBasis {
    /// Free at the point of use (local engines).
    None,
    /// Charged per second of input audio.
    PerAudioSecond,
    /// Charged per token.
    PerToken,
    /// Charged per request.
    PerRequest,
    /// A basis this build does not model.
    Other(String),
}

/// Minimum hardware an engine needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareRequirements {
    /// Minimum free RAM in bytes.
    pub min_ram_bytes: u64,
    /// Whether a GPU or neural accelerator is required (not merely preferred).
    pub requires_accelerator: bool,
    /// Named accelerator backends the engine can use.
    pub accelerator_backends: Vec<String>,
}

/// Why a capability is unavailable, as a typed value rather than prose.
///
/// Spec 8.3 and 11.5 require that unavailability is explained by a runtime
/// probe result and never inferred from an operating-system version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AvailabilityReason {
    /// The engine is ready.
    Available,
    /// The probe has not run yet.
    NotProbed,
    /// The platform does not provide this engine at all.
    PlatformUnsupported {
        /// Platform identifier observed at runtime.
        platform: String,
    },
    /// The host lacks required hardware.
    HardwareUnsupported {
        /// Short machine-readable discriminator.
        detail: String,
    },
    /// A model asset is not installed.
    ModelAssetMissing {
        /// Identifier of the missing asset.
        asset: String,
    },
    /// A model asset is installed but still downloading or verifying.
    ModelAssetNotReady {
        /// Identifier of the asset.
        asset: String,
    },
    /// The requested locale is not covered.
    LocaleUnsupported {
        /// Requested locale.
        locale: LanguageTag,
    },
    /// The platform feature is switched off by the user or by policy.
    DisabledByUser,
    /// A credential is required but absent.
    CredentialMissing,
    /// Local-only mode forbids this engine.
    LocalOnlyMode,
    /// The probe failed for a reason this build does not model.
    ProbeFailed {
        /// Short machine-readable discriminator.
        detail: String,
    },
}

impl AvailabilityReason {
    /// Whether the engine may be used.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Declared capabilities of one speech-to-text engine (spec 8.3).
///
/// Every field that an engine may fail to report is a `Tri` or `Known`, so a
/// silent default can never be read as support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SttCapabilities {
    /// Contract version of this payload (spec 6.6).
    pub payload_version: u32,
    /// Validation status of this engine for the current configuration.
    pub status: CapabilityStatus,
    /// Engine implementing the capability.
    pub engine_id: EngineId,
    /// Model the capability describes.
    pub model: ModelIdentity,
    /// Endpoint contract family (cloud engines only; local engines report `Unsupported`).
    pub request_family: RequestFamily,
    /// Whether transcription runs without leaving the machine.
    pub offline: Tri,
    /// Locales the engine declares.
    pub locales: Known<Vec<LanguageTag>>,
    /// Container/codec pairs the engine accepts.
    pub supported_formats: Known<Vec<MediaFormat>>,
    /// Maximum accepted payload size.
    pub max_file_bytes: Known<u64>,
    /// Maximum accepted media duration in milliseconds.
    pub max_duration_ms: Known<u64>,
    /// Whether the engine returns segment boundaries.
    pub supports_segments: Tri,
    /// Whether the engine returns word-level timings.
    pub supports_word_timestamps: Tri,
    /// Whether the engine returns speaker turns.
    pub supports_diarization: Tri,
    /// Whether the engine detects the spoken language.
    pub supports_language_detection: Tri,
    /// Whether an in-flight run can be cancelled.
    pub can_cancel: Tri,
    /// Where processing happens.
    pub privacy_class: Known<PrivacyClass>,
    /// How the provider charges.
    pub pricing_basis: Known<PricingBasis>,
    /// Bytes that must be downloaded before first use.
    pub model_download_bytes: Known<u64>,
    /// Hardware the engine needs.
    pub hardware_requirements: Known<HardwareRequirements>,
    /// Runtime availability result.
    pub availability_reason: AvailabilityReason,
}

impl SttCapabilities {
    /// Contract version understood by this build.
    pub const SUPPORTED_PAYLOAD_VERSION: u32 = 1;

    /// A fully unknown capability set for `engine`/`model`.
    ///
    /// This is the correct starting point for every adapter: fields are filled
    /// in only as a probe observes them.
    #[must_use]
    pub fn unknown(engine_id: EngineId, model: ModelIdentity) -> Self {
        Self {
            payload_version: Self::SUPPORTED_PAYLOAD_VERSION,
            status: CapabilityStatus::Unknown,
            engine_id,
            model,
            request_family: RequestFamily::Unknown,
            offline: Tri::Unknown,
            locales: Known::Unknown,
            supported_formats: Known::Unknown,
            max_file_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            supports_segments: Tri::Unknown,
            supports_word_timestamps: Tri::Unknown,
            supports_diarization: Tri::Unknown,
            supports_language_detection: Tri::Unknown,
            can_cancel: Tri::Unknown,
            privacy_class: Known::Unknown,
            pricing_basis: Known::Unknown,
            model_download_bytes: Known::Unknown,
            hardware_requirements: Known::Unknown,
            availability_reason: AvailabilityReason::NotProbed,
        }
    }

    /// Effective status after contract-version negotiation.
    ///
    /// A payload produced by an unknown contract version is reported as
    /// `Unknown` regardless of the status it claims (spec 6.6).
    #[must_use]
    pub fn effective_status(&self) -> CapabilityStatus {
        if self.payload_version == Self::SUPPORTED_PAYLOAD_VERSION {
            self.status
        } else {
            CapabilityStatus::Unknown
        }
    }

    /// Whether engine policy may select this capability at all.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        self.effective_status().is_selectable() && self.availability_reason.is_available()
    }

    /// Whether the engine declares support for `locale`.
    ///
    /// An unknown locale list never satisfies a locale requirement.
    #[must_use]
    pub fn supports_locale(&self, locale: &LanguageTag) -> bool {
        self.locales.get().is_some_and(|locales| {
            locales.iter().any(|candidate| {
                candidate == locale || candidate.primary_language() == locale.primary_language()
            })
        })
    }

    /// Whether the engine declares support for `format`.
    #[must_use]
    pub fn supports_format(&self, format: &MediaFormat) -> bool {
        self.supported_formats
            .get()
            .is_some_and(|formats| formats.contains(format))
    }
}

/// Declared capabilities of one text generation model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextModelCapabilities {
    /// Contract version of this payload (spec 6.6).
    pub payload_version: u32,
    /// Validation status of this model for the current configuration.
    pub status: CapabilityStatus,
    /// Engine implementing the capability.
    pub engine_id: EngineId,
    /// Model the capability describes.
    pub model: ModelIdentity,
    /// Whether generation runs without leaving the machine.
    pub offline: Tri,
    /// Maximum context length in tokens.
    pub max_context_tokens: Known<u64>,
    /// Maximum output length in tokens.
    pub max_output_tokens: Known<u64>,
    /// Whether the model can be constrained to a JSON schema.
    pub supports_structured_output: Tri,
    /// Whether an in-flight run can be cancelled.
    pub can_cancel: Tri,
    /// Where processing happens.
    pub privacy_class: Known<PrivacyClass>,
    /// How the provider charges.
    pub pricing_basis: Known<PricingBasis>,
    /// Runtime availability result.
    pub availability_reason: AvailabilityReason,
}

impl TextModelCapabilities {
    /// Contract version understood by this build.
    pub const SUPPORTED_PAYLOAD_VERSION: u32 = 1;

    /// A fully unknown capability set for `engine`/`model`.
    #[must_use]
    pub fn unknown(engine_id: EngineId, model: ModelIdentity) -> Self {
        Self {
            payload_version: Self::SUPPORTED_PAYLOAD_VERSION,
            status: CapabilityStatus::Unknown,
            engine_id,
            model,
            offline: Tri::Unknown,
            max_context_tokens: Known::Unknown,
            max_output_tokens: Known::Unknown,
            supports_structured_output: Tri::Unknown,
            can_cancel: Tri::Unknown,
            privacy_class: Known::Unknown,
            pricing_basis: Known::Unknown,
            availability_reason: AvailabilityReason::NotProbed,
        }
    }

    /// Effective status after contract-version negotiation.
    #[must_use]
    pub fn effective_status(&self) -> CapabilityStatus {
        if self.payload_version == Self::SUPPORTED_PAYLOAD_VERSION {
            self.status
        } else {
            CapabilityStatus::Unknown
        }
    }

    /// Whether policy may select this model.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        self.effective_status().is_selectable() && self.availability_reason.is_available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> EngineId {
        EngineId::new("test-engine")
    }

    fn model() -> ModelIdentity {
        ModelIdentity::new(engine(), "test-model")
    }

    #[test]
    fn a_fresh_capability_set_claims_nothing() {
        let caps = SttCapabilities::unknown(engine(), model());
        assert_eq!(caps.status, CapabilityStatus::Unknown);
        assert!(!caps.is_selectable());
        assert!(!caps.offline.satisfies());
        assert!(!caps.supports_segments.satisfies());
        assert!(!caps.supports_format(&MediaFormat::new("wav", "pcm_s16le")));
        assert_eq!(caps.availability_reason, AvailabilityReason::NotProbed);
    }

    #[test]
    fn a_newer_payload_version_downgrades_to_unknown() {
        let mut caps = SttCapabilities::unknown(engine(), model());
        caps.status = CapabilityStatus::Supported;
        caps.availability_reason = AvailabilityReason::Available;
        assert!(caps.is_selectable());

        caps.payload_version = SttCapabilities::SUPPORTED_PAYLOAD_VERSION + 1;
        assert_eq!(caps.effective_status(), CapabilityStatus::Unknown);
        assert!(!caps.is_selectable());
    }

    #[test]
    fn an_unavailable_engine_is_not_selectable_even_when_supported() {
        let mut caps = SttCapabilities::unknown(engine(), model());
        caps.status = CapabilityStatus::Supported;
        caps.availability_reason = AvailabilityReason::ModelAssetMissing {
            asset: "ggml-base.en".into(),
        };
        assert!(!caps.is_selectable());
    }

    #[test]
    fn an_unknown_request_family_has_no_builder() {
        assert!(RequestFamily::MultipartStt.has_builder());
        assert!(RequestFamily::AudioChat.has_builder());
        assert!(!RequestFamily::Unsupported.has_builder());
        assert!(!RequestFamily::Unknown.has_builder());
        let decoded: RequestFamily = serde_json::from_str("\"telepathic_audio\"").unwrap();
        assert_eq!(decoded, RequestFamily::Unknown);
    }

    #[test]
    fn an_unresolved_privacy_route_is_not_dispatchable() {
        assert!(!PrivacyClass::Unresolved.is_dispatchable());
        assert!(PrivacyClass::OnDevice.is_dispatchable());
        assert!(!PrivacyClass::OnDevice.requires_cloud_consent());
        assert!(PrivacyClass::CloudZeroRetention.requires_cloud_consent());
        let decoded: PrivacyClass = serde_json::from_str("\"maybe_private\"").unwrap();
        assert_eq!(decoded, PrivacyClass::Unresolved);
    }

    #[test]
    fn unknown_locale_lists_never_satisfy_a_locale_requirement() {
        let mut caps = SttCapabilities::unknown(engine(), model());
        let english = LanguageTag::parse("en-US").unwrap();
        assert!(!caps.supports_locale(&english));

        caps.locales = Known::Known(vec![LanguageTag::parse("en-GB").unwrap()]);
        assert!(
            caps.supports_locale(&english),
            "primary language match is sufficient"
        );
        assert!(!caps.supports_locale(&LanguageTag::parse("ja").unwrap()));
    }
}
