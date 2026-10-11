//! STT capability model (spec §8.3) and request-family typing (§8.6).

use serde::{Deserialize, Serialize};

use crate::tri::{Known, Tri};

/// Media container/codec families the pipeline can name precisely.
///
/// A format we cannot name is not representable here; an engine that accepts
/// something else must declare [`Known::Unknown`] for `supported_formats`,
/// which never satisfies a requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaFormat {
    WavPcm,
    Mp3,
    M4aAac,
    Flac,
    OggVorbis,
    OggOpus,
    /// Raw interleaved 16-bit little-endian PCM, the canonical derivative.
    PcmS16Le,
    /// Raw interleaved 32-bit float PCM.
    PcmF32Le,
}

impl MediaFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MediaFormat::WavPcm => "wav_pcm",
            MediaFormat::Mp3 => "mp3",
            MediaFormat::M4aAac => "m4a_aac",
            MediaFormat::Flac => "flac",
            MediaFormat::OggVorbis => "ogg_vorbis",
            MediaFormat::OggOpus => "ogg_opus",
            MediaFormat::PcmS16Le => "pcm_s16le",
            MediaFormat::PcmF32Le => "pcm_f32le",
        }
    }
}

/// Which wire contract a model actually implements (§8.6).
///
/// "'audio capable' in a model catalog does not specify which endpoint
/// contract it implements." Only [`RequestFamily::MultipartStt`] and
/// [`RequestFamily::AudioChat`] are dispatchable, and only after explicit
/// verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestFamily {
    MultipartStt,
    AudioChat,
    Unsupported,
    Unknown,
}

impl RequestFamily {
    /// Verified-dispatchable. `Unknown` and `Unsupported` both fail closed and
    /// surface as **Not verified** in the model picker (§8.6A).
    #[inline]
    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, RequestFamily::MultipartStt | RequestFamily::AudioChat)
    }
}

/// Where audio bytes go when this engine runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PrivacyClass {
    /// Audio never leaves the machine.
    OnDeviceOnly,
    /// Audio is transmitted to the named provider under the named route.
    CloudProviderRouted {
        provider: String,
        /// Whether a zero-data-retention route is contractually in force.
        zero_data_retention: Tri,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PricingBasis {
    /// No metered cost: local compute.
    NoMeteredCost,
    PerAudioMinute { currency: String, micros: u64 },
    PerToken { currency: String, micros_per_1k: u64 },
}

/// Hardware the engine needs in order to run at all.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareRequirements {
    /// Working-set estimate for the model plus decode buffers.
    pub min_ram_bytes: Known<u64>,
    /// Disk needed for installed weights.
    pub disk_bytes: Known<u64>,
    /// True only when the engine *cannot* run without a GPU. whisper.cpp is
    /// `No`: GPU is an accelerator, never a requirement (§12).
    pub requires_gpu: Tri,
    /// Accelerators the engine can use when present.
    pub optional_accelerators: Vec<Accelerator>,
}

impl Default for HardwareRequirements {
    fn default() -> Self {
        Self {
            min_ram_bytes: Known::Unknown,
            disk_bytes: Known::Unknown,
            requires_gpu: Tri::Unknown,
            optional_accelerators: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accelerator {
    Cpu,
    Metal,
    CoreMl,
    Cuda,
    Vulkan,
    DirectMl,
}

impl Accelerator {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Accelerator::Cpu => "cpu",
            Accelerator::Metal => "metal",
            Accelerator::CoreMl => "coreml",
            Accelerator::Cuda => "cuda",
            Accelerator::Vulkan => "vulkan",
            Accelerator::DirectMl => "directml",
        }
    }
}

/// Why an engine is not usable right now. Present means "not usable".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "code", content = "detail")]
pub enum AvailabilityReason {
    /// Weights are not installed. Carries the model id the user must install.
    ModelNotInstalled(String),
    /// Runtime probe for a platform engine failed (§8.4: tested, not deduced).
    RuntimeProbeFailed(String),
    /// Engine exists but not on this OS/arch.
    UnsupportedPlatform(String),
    /// Hardware does not meet the declared requirements.
    InsufficientHardware(String),
    /// Credentials absent for a cloud engine.
    CredentialMissing(String),
    /// Local-only mode or absent consent blocks a networked engine.
    BlockedByPolicy(String),
}

impl AvailabilityReason {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            AvailabilityReason::ModelNotInstalled(m)
            | AvailabilityReason::RuntimeProbeFailed(m)
            | AvailabilityReason::UnsupportedPlatform(m)
            | AvailabilityReason::InsufficientHardware(m)
            | AvailabilityReason::CredentialMissing(m)
            | AvailabilityReason::BlockedByPolicy(m) => m,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            AvailabilityReason::ModelNotInstalled(_) => "model_not_installed",
            AvailabilityReason::RuntimeProbeFailed(_) => "runtime_probe_failed",
            AvailabilityReason::UnsupportedPlatform(_) => "unsupported_platform",
            AvailabilityReason::InsufficientHardware(_) => "insufficient_hardware",
            AvailabilityReason::CredentialMissing(_) => "credential_missing",
            AvailabilityReason::BlockedByPolicy(_) => "blocked_by_policy",
        }
    }
}

/// The complete §8.3 capability payload advertised by every STT engine.
///
/// Versioned independently of the app release (§6.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SttCapabilities {
    /// Schema version of this payload (§6.6).
    pub payload_version: u32,
    /// Stable engine identifier, e.g. `whisper.cpp`, `apple.speech`.
    pub engine_id: String,
    /// Model identifier within the engine, when a specific model is bound.
    pub model_id: Option<String>,
    pub model_version: Option<String>,

    // ---- §8.3 required fields, in spec order ----
    pub offline: Tri,
    pub locales: Known<Vec<String>>,
    pub supported_formats: Known<Vec<MediaFormat>>,
    pub max_file_bytes: Known<u64>,
    pub max_duration_ms: Known<u64>,
    pub supports_segments: Tri,
    pub supports_word_timestamps: Tri,
    pub supports_diarization: Tri,
    pub supports_language_detection: Tri,
    pub can_cancel: Tri,
    pub privacy_class: Known<PrivacyClass>,
    pub pricing_basis: Known<PricingBasis>,
    pub model_download_bytes: Known<u64>,
    pub hardware_requirements: Known<HardwareRequirements>,
    pub availability_reason: Option<AvailabilityReason>,

    /// Which wire contract this model implements (§8.6). Local engines declare
    /// [`RequestFamily::Unsupported`]: they are not dispatched over a wire.
    pub request_family: RequestFamily,
}

impl SttCapabilities {
    /// All-unknown baseline. An engine that forgets to fill a field advertises
    /// `unknown`, which can never satisfy a selection requirement.
    #[must_use]
    pub fn unknown(engine_id: impl Into<String>) -> Self {
        Self {
            payload_version: CAPABILITY_PAYLOAD_VERSION,
            engine_id: engine_id.into(),
            model_id: None,
            model_version: None,
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
            availability_reason: None,
            request_family: RequestFamily::Unknown,
        }
    }

    /// True when the engine declares it is usable right now.
    #[inline]
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.availability_reason.is_none()
    }

    /// Locale match is case-insensitive on the primary subtag, so `en-GB`
    /// satisfies an engine advertising `en`. An unknown locale list never
    /// satisfies a locale requirement.
    #[must_use]
    pub fn supports_locale(&self, requested: &str) -> bool {
        let Some(locales) = self.locales.known() else {
            return false;
        };
        let requested_primary = primary_subtag(requested);
        locales.iter().any(|advertised| {
            advertised.eq_ignore_ascii_case(requested)
                || primary_subtag(advertised).eq_ignore_ascii_case(requested_primary)
        })
    }

    #[must_use]
    pub fn supports_format(&self, format: MediaFormat) -> bool {
        self.supported_formats
            .known()
            .is_some_and(|formats| formats.contains(&format))
    }
}

fn primary_subtag(tag: &str) -> &str {
    tag.split(['-', '_']).next().unwrap_or(tag)
}

/// Bumped when the capability payload shape changes (§6.6).
pub const CAPABILITY_PAYLOAD_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_baseline_satisfies_nothing() {
        let caps = SttCapabilities::unknown("test");
        assert!(!caps.offline.satisfies());
        assert!(!caps.supports_locale("en"));
        assert!(!caps.supports_format(MediaFormat::WavPcm));
        assert!(!caps.request_family.is_verified());
        assert!(caps.max_file_bytes.is_unknown());
    }

    #[test]
    fn locale_matching_uses_primary_subtag() {
        let mut caps = SttCapabilities::unknown("test");
        caps.locales = Known::Known(vec!["en".into()]);
        assert!(caps.supports_locale("en-GB"));
        assert!(caps.supports_locale("EN"));
        assert!(!caps.supports_locale("de"));
    }

    #[test]
    fn unknown_locale_list_never_matches() {
        let caps = SttCapabilities::unknown("test");
        assert!(!caps.supports_locale("en"));
    }

    #[test]
    fn unknown_and_unsupported_request_families_both_fail_closed() {
        assert!(!RequestFamily::Unknown.is_verified());
        assert!(!RequestFamily::Unsupported.is_verified());
        assert!(RequestFamily::MultipartStt.is_verified());
        assert!(RequestFamily::AudioChat.is_verified());
    }

    #[test]
    fn capabilities_round_trip_through_json() {
        let mut caps = SttCapabilities::unknown("whisper.cpp");
        caps.offline = Tri::Yes;
        caps.supported_formats = Known::Known(vec![MediaFormat::PcmS16Le]);
        caps.privacy_class = Known::Known(PrivacyClass::OnDeviceOnly);
        caps.pricing_basis = Known::Known(PricingBasis::NoMeteredCost);
        let json = serde_json::to_string(&caps).unwrap();
        let back: SttCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, back);
    }

    #[test]
    fn availability_reason_present_means_unavailable() {
        let mut caps = SttCapabilities::unknown("whisper.cpp");
        assert!(caps.is_available());
        caps.availability_reason = Some(AvailabilityReason::ModelNotInstalled("tiny.en".into()));
        assert!(!caps.is_available());
        assert_eq!(
            caps.availability_reason.as_ref().unwrap().code(),
            "model_not_installed"
        );
    }
}
