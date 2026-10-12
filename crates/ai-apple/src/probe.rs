//! Runtime availability probes and their capability mapping (task H1.3).
//!
//! Spec 11.5 is the rule this module encodes: platform feature availability
//! is labelled "using runtime evidence, not marketing labels". Nothing here
//! infers support from an operating-system version; every `Supported` verdict
//! traces to a field the platform reported in this process, on this machine,
//! within the last probe window.
//!
//! The mapping is pure and is tested on every platform with no Apple
//! framework linked. The only macOS-specific part is who produces the
//! documents it maps.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, HardwareRequirements, MediaFormat, PricingBasis, PrivacyClass,
    RequestFamily, SttCapabilities, TextModelCapabilities,
};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::model::{EngineId, ModelIdentity};

use crate::bridge::{AppleBridge, FfiError};
use crate::envelope::{
    encode_request, AssetStatus, BridgeError, BridgeErrorCode, BridgeOp, BridgeReply, ProbeRequest,
    SpeechProbe, TextModelProbe,
};

/// Engine identity of the Apple speech transcriber.
///
/// Must equal `plaudon_app_core::stt::selection::APPLE_ENGINE_ID`; the
/// workspace conformance test asserts it.
pub const APPLE_SPEECH_ENGINE: &str = "apple-speech";

/// Engine identity of the Apple on-device language model.
pub const APPLE_FOUNDATION_ENGINE: &str = "apple-foundation";

/// Model identifier of the speech transcriber.
pub const APPLE_SPEECH_MODEL: &str = "speech-transcriber";

/// Model identifier of the on-device language model.
pub const APPLE_FOUNDATION_MODEL: &str = "system-language-model";

/// Wall-clock ceiling for one availability probe.
///
/// Availability is asked before every engine decision, so it has to be cheap
/// enough to be asked. A probe that overruns is reported as `Unknown` and the
/// application proceeds with whisper.cpp: a slow answer and no answer are the
/// same thing to a user waiting on a transcript.
///
/// Measured cold on macOS 27.0.1 / M-series: see
/// `docs/spikes/A1.3-apple-availability.md`.
pub const PROBE_BUDGET: Duration = Duration::from_millis(100);

/// How long a probe result may be reused before it is re-taken.
///
/// Short enough that an asset installation or an Apple Intelligence toggle
/// becomes visible without the user restarting, long enough that a library
/// view listing a hundred recordings does not probe a hundred times.
pub const PROBE_MAX_AGE: Duration = Duration::from_secs(30);

/// Containers the Apple analyzer reads.
///
/// `AVAudioFile` decodes through AudioToolbox, so the accepted set is the
/// system codec set rather than an engine-specific list. Only pairs that an
/// import can actually produce are declared, and each one is exercised by the
/// opt-in hardware suite before a cell is marked Supported.
fn accepted_media_formats() -> Vec<MediaFormat> {
    vec![
        MediaFormat::new("wav", "pcm_s16le"),
        MediaFormat::new("wav", "pcm_f32le"),
        MediaFormat::new("mp3", "mp3"),
        MediaFormat::new("mp4", "aac"),
        MediaFormat::new("flac", "flac"),
    ]
}

/// What a probe observed, independent of any capability interpretation.
///
/// Kept separate from [`SttCapabilities`] because the compatibility register
/// and the UI need the raw facts — which locales are installed as opposed to
/// merely supported, what the concurrency limit is, which OS build answered —
/// and a capability payload is a decision, not evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct AppleAvailability {
    /// Speech probe reply.
    pub speech: Result<SpeechProbe, BridgeError>,
    /// Text-model probe reply.
    pub text_model: Result<TextModelProbe, BridgeError>,
    /// Locale the probes were taken for.
    pub requested_locale: Option<String>,
    /// Wall-clock cost of taking both probes.
    pub elapsed: Duration,
}

impl AppleAvailability {
    /// An availability snapshot for a host with no bridge at all.
    #[must_use]
    pub fn unavailable(reason: BridgeError) -> Self {
        Self {
            speech: Err(reason.clone()),
            text_model: Err(reason),
            requested_locale: None,
            elapsed: Duration::ZERO,
        }
    }

    /// OS build string the platform reported, when either probe answered.
    #[must_use]
    pub fn os_build(&self) -> Option<&str> {
        self.speech
            .as_ref()
            .ok()
            .map(|p| p.os_build.as_str())
            .or_else(|| self.text_model.as_ref().ok().map(|p| p.os_build.as_str()))
            .filter(|value| !value.is_empty())
    }

    /// Hardware string the platform reported.
    #[must_use]
    pub fn hardware(&self) -> Option<&str> {
        self.speech
            .as_ref()
            .ok()
            .map(|p| p.hardware.as_str())
            .or_else(|| self.text_model.as_ref().ok().map(|p| p.hardware.as_str()))
            .filter(|value| !value.is_empty())
    }

    /// Fingerprint of everything that, if it changed, invalidates the probe.
    ///
    /// Spec 11.6 lists OS build, Apple Intelligence availability, language,
    /// installed assets and model restrictions. All five are here, so a
    /// background OS update, an asset install or a settings toggle produces a
    /// different fingerprint on the next probe and the cached verdict is
    /// replaced rather than trusted.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(6);
        parts.push(format!("os={}", self.os_build().unwrap_or("unknown")));
        parts.push(format!("hw={}", self.hardware().unwrap_or("unknown")));
        match &self.speech {
            Ok(probe) => {
                parts.push(format!("speech=ok:{}", probe.available));
                parts.push(format!("installed={}", probe.installed_locales.join(",")));
                parts.push(format!("assets={:?}", probe.asset_status));
            }
            Err(error) => parts.push(format!("speech=err:{}", error.code.as_str())),
        }
        match &self.text_model {
            Ok(probe) => parts.push(format!(
                "text=ok:{}:{}",
                probe.available,
                probe.supported_languages.join(",")
            )),
            Err(error) => parts.push(format!("text=err:{}", error.code.as_str())),
        }
        parts.join("|")
    }
}

/// Map a bridge error onto the capability vocabulary.
///
/// Every mapping is a narrowing: an unrecognized code becomes
/// `ProbeFailed`, never `Available`.
#[must_use]
pub fn availability_reason(error: &BridgeError) -> AvailabilityReason {
    let detail = || {
        error
            .detail
            .clone()
            .unwrap_or_else(|| error.code.as_str().to_owned())
    };
    match &error.code {
        BridgeErrorCode::PlatformUnsupported => {
            AvailabilityReason::PlatformUnsupported { platform: detail() }
        }
        BridgeErrorCode::HardwareUnsupported => {
            AvailabilityReason::HardwareUnsupported { detail: detail() }
        }
        BridgeErrorCode::DisabledByUser => AvailabilityReason::DisabledByUser,
        BridgeErrorCode::ModelAssetMissing => {
            AvailabilityReason::ModelAssetMissing { asset: detail() }
        }
        BridgeErrorCode::ModelAssetNotReady | BridgeErrorCode::AssetDownloadFailed => {
            AvailabilityReason::ModelAssetNotReady { asset: detail() }
        }
        BridgeErrorCode::LocaleUnsupported | BridgeErrorCode::UnsupportedLanguage => error
            .detail
            .as_deref()
            .and_then(|tag| parse_locale(tag).ok())
            .map_or_else(
                || AvailabilityReason::ProbeFailed {
                    detail: "locale_unsupported".to_owned(),
                },
                |locale| AvailabilityReason::LocaleUnsupported { locale },
            ),
        // The OS withheld an authorization. That is a permission answer, but
        // the capability vocabulary has no permission variant, and inventing
        // availability for it would be worse than a typed probe failure.
        BridgeErrorCode::PermissionDenied => AvailabilityReason::ProbeFailed {
            detail: format!("permission_denied:{}", detail()),
        },
        _ => AvailabilityReason::ProbeFailed { detail: detail() },
    }
}

/// Parse a locale the platform reported.
///
/// Apple writes locale identifiers with an underscore (`en_US`); BCP 47 uses
/// a hyphen. Normalizing here keeps the rest of the application in one
/// vocabulary and means a locale requirement matches what the engine offers.
fn parse_locale(raw: &str) -> Result<LanguageTag, plaudon_domain::locale::LanguageTagError> {
    LanguageTag::parse(&raw.replace('_', "-"))
}

/// Whether the engine may run `locale` without downloading anything.
fn installed_locales(probe: &SpeechProbe) -> Vec<LanguageTag> {
    probe
        .installed_locales
        .iter()
        .filter_map(|tag| parse_locale(tag).ok())
        .collect()
}

/// Locales the platform could serve after a consented download.
fn supported_locales(probe: &SpeechProbe) -> Vec<LanguageTag> {
    probe
        .supported_locales
        .iter()
        .filter_map(|tag| parse_locale(tag).ok())
        .collect()
}

/// Whether the caller has obtained consent to download speech assets.
///
/// Spec 12 forbids silently fetching model data, so this is an explicit input
/// to the capability mapping rather than a global setting the mapping reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssetDownloadConsent {
    /// Only locales already on disk may be offered.
    #[default]
    Withheld,
    /// Locales the platform can install may also be offered.
    Granted,
}

impl AssetDownloadConsent {
    /// Whether an uninstalled locale may be advertised as usable.
    #[must_use]
    pub const fn permits_download(self) -> bool {
        matches!(self, Self::Granted)
    }
}

/// Build speech capabilities from a probe reply.
///
/// `Supported` requires an affirmative, on-device probe whose resolved locale
/// is installed, or installable with consent. Everything else carries a typed
/// reason the interface can render (spec 5.4).
#[must_use]
pub fn speech_capabilities(
    reply: &Result<SpeechProbe, BridgeError>,
    consent: AssetDownloadConsent,
) -> SttCapabilities {
    let engine = EngineId::new(APPLE_SPEECH_ENGINE);
    let mut caps = SttCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, APPLE_SPEECH_MODEL),
    );
    // A platform capability, not a cloud route: there is no request builder
    // and no provider to resolve.
    caps.request_family = RequestFamily::Unsupported;
    caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
    caps.pricing_basis = Known::Known(PricingBasis::None);
    caps.model_download_bytes = Known::Unknown;

    let probe = match reply {
        Err(error) => {
            caps.availability_reason = availability_reason(error);
            caps.status = if error.code.is_capability_verdict() {
                CapabilityStatus::Unsupported
            } else {
                // A timed-out or unreadable probe has not established that the
                // engine is absent, only that we do not know. Unknown is not
                // selectable either way (spec 8.3).
                CapabilityStatus::Unknown
            };
            return caps;
        }
        Ok(probe) => probe,
    };

    if !probe.available {
        caps.availability_reason = AvailabilityReason::ProbeFailed {
            detail: "reported_unavailable".to_owned(),
        };
        caps.status = CapabilityStatus::Unsupported;
        return caps;
    }
    if !probe.is_transcriber {
        // Spec 2.3: refuse to treat anything that does not declare itself a
        // transcriber as one.
        caps.availability_reason = AvailabilityReason::ProbeFailed {
            detail: "not_a_transcriber".to_owned(),
        };
        caps.status = CapabilityStatus::Unsupported;
        return caps;
    }

    caps.offline = Tri::from_observation(Some(probe.supports_on_device));
    caps.can_cancel = Tri::Yes;
    caps.supports_diarization = Tri::No;
    // SpeechTranscriber is constructed for one locale; it does not detect the
    // spoken language.
    caps.supports_language_detection = Tri::No;
    caps.supported_formats = Known::Known(accepted_media_formats());
    caps.hardware_requirements = Known::Known(HardwareRequirements {
        // The platform manages its own residency; no figure is published, and
        // a guessed one would be a fabricated requirement.
        min_ram_bytes: 0,
        requires_accelerator: false,
        accelerator_backends: vec!["apple-neural-engine".to_owned()],
    });
    // Observed on a real response during H2.1: finalized results carry
    // `CMTimeRange` boundaries and per-run `audioTimeRange` attributes. The
    // bridge reports what it requested and received; the mapping does not
    // assume it.
    caps.supports_segments = Tri::Yes;
    caps.supports_word_timestamps = Tri::from_observation(Some(probe.supports_word_timestamps));

    let installed = installed_locales(probe);
    let offerable = if consent.permits_download() {
        let mut all = installed.clone();
        for locale in supported_locales(probe) {
            if !all.contains(&locale) {
                all.push(locale);
            }
        }
        all
    } else {
        installed.clone()
    };
    caps.locales = if offerable.is_empty() {
        Known::Unknown
    } else {
        Known::Known(offerable)
    };

    if !probe.supports_on_device {
        // Without on-device recognition the audio would leave the machine,
        // which is not what this engine is for.
        caps.availability_reason = AvailabilityReason::ProbeFailed {
            detail: "on_device_recognition_unavailable".to_owned(),
        };
        caps.status = CapabilityStatus::Unsupported;
        return caps;
    }

    if probe.locale_installed {
        // The platform lists the resolved locale as installed, which is the
        // signal that tracks what transcription can actually do.
        caps.availability_reason = AvailabilityReason::Available;
        caps.status = CapabilityStatus::Supported;
        return caps;
    }

    match probe.asset_status {
        AssetStatus::Installed => {
            caps.availability_reason = AvailabilityReason::Available;
            caps.status = CapabilityStatus::Supported;
        }
        AssetStatus::Downloading => {
            caps.availability_reason = AvailabilityReason::ModelAssetNotReady {
                asset: probe
                    .resolved_locale
                    .clone()
                    .unwrap_or_else(|| "speech_locale".to_owned()),
            };
            caps.status = CapabilityStatus::Unsupported;
        }
        AssetStatus::Supported => {
            caps.availability_reason = AvailabilityReason::ModelAssetMissing {
                asset: probe
                    .resolved_locale
                    .clone()
                    .unwrap_or_else(|| "speech_locale".to_owned()),
            };
            // Installable, but not installed. Selection must not pick it:
            // doing so would start a multi-hundred-megabyte download the user
            // never approved (spec 12).
            caps.status = CapabilityStatus::Unsupported;
        }
        AssetStatus::Unsupported => {
            caps.availability_reason = probe
                .resolved_locale
                .as_deref()
                .and_then(|tag| parse_locale(tag).ok())
                .map_or_else(
                    || AvailabilityReason::ProbeFailed {
                        detail: "no_resolved_locale".to_owned(),
                    },
                    |locale| AvailabilityReason::LocaleUnsupported { locale },
                );
            caps.status = CapabilityStatus::Unsupported;
        }
    }
    caps
}

/// Build text-model capabilities from a probe reply.
#[must_use]
pub fn text_model_capabilities(
    reply: &Result<TextModelProbe, BridgeError>,
) -> TextModelCapabilities {
    let engine = EngineId::new(APPLE_FOUNDATION_ENGINE);
    let mut caps = TextModelCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, APPLE_FOUNDATION_MODEL),
    );
    caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
    caps.pricing_basis = Known::Known(PricingBasis::None);

    match reply {
        Err(error) => {
            caps.availability_reason = availability_reason(error);
            caps.status = if error.code.is_capability_verdict() {
                CapabilityStatus::Unsupported
            } else {
                CapabilityStatus::Unknown
            };
        }
        Ok(probe) => {
            if probe.is_transcriber {
                // Spec 2.3: a model claiming to transcribe is not the system
                // language model, and we will not route text work to it.
                caps.availability_reason = AvailabilityReason::ProbeFailed {
                    detail: "text_model_claims_transcription".to_owned(),
                };
                caps.status = CapabilityStatus::Unsupported;
                return caps;
            }
            if probe.available {
                caps.availability_reason = AvailabilityReason::Available;
                caps.status = CapabilityStatus::Supported;
                caps.offline = Tri::Yes;
                caps.can_cancel = Tri::Yes;
                caps.supports_structured_output =
                    Tri::from_observation(Some(probe.supports_structured_output));
                // The framework publishes no context window. It is revealed
                // only inside a context-overflow error, so the honest value is
                // Unknown and the chunker uses a labelled conservative bound
                // (spec 9.4; plaudon_app_core::chunking).
                caps.max_context_tokens = probe
                    .max_context_tokens
                    .map_or(Known::Unknown, Known::Known);
                // Nor an output ceiling: `probe_foundation_model` has no
                // field for one, and a probe cannot invent what the platform
                // does not state. The only thing that establishes an output
                // budget for this engine is a capability check that really
                // generated within a declared one, which is recorded as an
                // observed exchange and read back by the caller.
                caps.max_output_tokens = Known::Unknown;
                if let Some(version) = &probe.model_version {
                    caps.model = caps.model.clone().with_version(version.clone());
                }
            } else {
                caps.availability_reason = AvailabilityReason::ProbeFailed {
                    detail: "reported_unavailable".to_owned(),
                };
                caps.status = CapabilityStatus::Unsupported;
            }
        }
    }
    caps
}

/// Both capability payloads derived from one snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct AppleCapabilities {
    /// Speech-to-text capability.
    pub speech: SttCapabilities,
    /// Text-generation capability.
    pub text_model: TextModelCapabilities,
}

impl AppleCapabilities {
    /// Derive both payloads.
    #[must_use]
    pub fn from_availability(
        availability: &AppleAvailability,
        consent: AssetDownloadConsent,
    ) -> Self {
        Self {
            speech: speech_capabilities(&availability.speech, consent),
            text_model: text_model_capabilities(&availability.text_model),
        }
    }
}

/// Take both probes through `bridge`, with a hard wall-clock budget.
///
/// Blocking. Callers on an async runtime run it on a blocking thread.
///
/// Never panics and never hangs: the bridge bounds its own work, and the
/// `FfiError` paths turn into typed unavailability rather than propagating.
#[must_use]
pub fn probe_once(
    bridge: &dyn AppleBridge,
    locale: Option<&str>,
    budget: Duration,
) -> AppleAvailability {
    let started = Instant::now();

    if let Err(error) = bridge.abi_version().and_then(|found| {
        if found == crate::envelope::EXPECTED_ABI_VERSION {
            Ok(())
        } else {
            Err(FfiError::AbiMismatch {
                found,
                expected: crate::envelope::EXPECTED_ABI_VERSION,
            })
        }
    }) {
        return AppleAvailability {
            elapsed: started.elapsed(),
            requested_locale: locale.map(str::to_owned),
            ..AppleAvailability::unavailable(ffi_error_to_bridge_error(&error))
        };
    }

    let request = encode_request(
        BridgeOp::ProbeSpeech,
        &ProbeRequest {
            locale: locale.map(str::to_owned),
        },
    );
    let speech = match bridge.call(BridgeOp::ProbeSpeech, &request, None) {
        Ok(document) => match BridgeReply::<SpeechProbe>::parse(BridgeOp::ProbeSpeech, &document) {
            BridgeReply::Ok(probe) => Ok(probe),
            BridgeReply::Err(error) => Err(error),
        },
        Err(error) => Err(ffi_error_to_bridge_error(&error)),
    };

    // The budget covers both probes. If the first one consumed it, the second
    // is not attempted: spending twice the budget to report a slow answer
    // helps nobody.
    let text_model = if started.elapsed() >= budget {
        Err(BridgeError::new(
            BridgeErrorCode::EngineTimeout,
            Some("probe_budget_exceeded".to_owned()),
        ))
    } else {
        let request = encode_request(
            BridgeOp::ProbeFoundationModel,
            &ProbeRequest {
                locale: locale.map(str::to_owned),
            },
        );
        match bridge.call(BridgeOp::ProbeFoundationModel, &request, None) {
            Ok(document) => {
                match BridgeReply::<TextModelProbe>::parse(
                    BridgeOp::ProbeFoundationModel,
                    &document,
                ) {
                    BridgeReply::Ok(probe) => Ok(probe),
                    BridgeReply::Err(error) => Err(error),
                }
            }
            Err(error) => Err(ffi_error_to_bridge_error(&error)),
        }
    };

    let elapsed = started.elapsed();
    let overran = elapsed > budget;
    AppleAvailability {
        speech: if overran {
            Err(BridgeError::new(
                BridgeErrorCode::EngineTimeout,
                Some("probe_budget_exceeded".to_owned()),
            ))
        } else {
            speech
        },
        text_model: if overran {
            Err(BridgeError::new(
                BridgeErrorCode::EngineTimeout,
                Some("probe_budget_exceeded".to_owned()),
            ))
        } else {
            text_model
        },
        requested_locale: locale.map(str::to_owned),
        elapsed,
    }
}

/// Translate a boundary failure into the bridge error vocabulary.
fn ffi_error_to_bridge_error(error: &FfiError) -> BridgeError {
    match error {
        FfiError::NotLinked => BridgeError::new(
            BridgeErrorCode::PlatformUnsupported,
            Some("bridge_not_linked".to_owned()),
        ),
        FfiError::AbiMismatch { found, expected } => BridgeError::new(
            BridgeErrorCode::EngineFailure,
            Some(format!("abi_mismatch_{found}_expected_{expected}")),
        ),
        FfiError::InvalidArgument => BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            Some("interior_nul".to_owned()),
        ),
        FfiError::NullDocument => BridgeError::new(
            BridgeErrorCode::EngineFailure,
            Some("null_document".to_owned()),
        ),
    }
}

/// Why a cache read did or did not reuse a previous probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    /// A fresh probe was taken because none was cached.
    Probed,
    /// A cached probe was reused.
    Reused,
    /// The cached probe was too old and was replaced.
    Expired,
    /// The environment fingerprint changed and the cached probe was replaced.
    EnvironmentChanged,
    /// The cache was explicitly invalidated and the probe was replaced.
    Invalidated,
}

impl CacheOutcome {
    /// Whether the bridge was called for this read.
    #[must_use]
    pub const fn took_fresh_probe(self) -> bool {
        !matches!(self, Self::Reused)
    }
}

#[derive(Debug, Clone)]
struct CacheEntry {
    availability: AppleAvailability,
    fingerprint: String,
    taken_at: Instant,
}

/// Caches availability so it can be asked constantly without cost.
///
/// Invalidation has three independent triggers, because no single one is
/// sufficient: an age bound catches drift nothing told us about, an explicit
/// [`ProbeCache::invalidate`] catches the changes the application itself
/// caused (an asset installed, a locale chosen), and a fingerprint comparison
/// catches the ones it did not (an OS update, Apple Intelligence switched off
/// in System Settings). Spec 11.6 names all of these.
#[derive(Debug)]
pub struct ProbeCache {
    bridge: Arc<dyn AppleBridge>,
    max_age: Duration,
    budget: Duration,
    entries: Mutex<std::collections::BTreeMap<String, CacheEntry>>,
}

impl ProbeCache {
    /// A cache over `bridge` with the default age bound and probe budget.
    #[must_use]
    pub fn new(bridge: Arc<dyn AppleBridge>) -> Self {
        Self {
            bridge,
            max_age: PROBE_MAX_AGE,
            budget: PROBE_BUDGET,
            entries: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// Override the age bound.
    #[must_use]
    pub const fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// Override the probe budget.
    #[must_use]
    pub const fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Drop every cached probe.
    ///
    /// Called after an asset installation finishes and whenever the user
    /// changes something the probe reads.
    pub fn invalidate(&self) {
        self.entries.lock().clear();
    }

    /// Availability for `locale`, probing only when the cache cannot answer.
    ///
    /// Blocking. Returns the snapshot and why it was or was not reused.
    pub fn availability(&self, locale: Option<&str>) -> (AppleAvailability, CacheOutcome) {
        let key = locale.unwrap_or("").to_owned();
        let cached = self.entries.lock().get(&key).cloned();

        if let Some(entry) = cached {
            if entry.taken_at.elapsed() <= self.max_age {
                return (entry.availability, CacheOutcome::Reused);
            }
            let fresh = probe_once(self.bridge.as_ref(), locale, self.budget);
            let fingerprint = fresh.fingerprint();
            let outcome = if fingerprint == entry.fingerprint {
                CacheOutcome::Expired
            } else {
                CacheOutcome::EnvironmentChanged
            };
            self.store(key, &fresh, fingerprint);
            return (fresh, outcome);
        }

        let fresh = probe_once(self.bridge.as_ref(), locale, self.budget);
        let fingerprint = fresh.fingerprint();
        self.store(key, &fresh, fingerprint);
        (fresh, CacheOutcome::Probed)
    }

    /// Force a fresh probe, replacing any cached value.
    pub fn refresh(&self, locale: Option<&str>) -> (AppleAvailability, CacheOutcome) {
        let key = locale.unwrap_or("").to_owned();
        let previous = self.entries.lock().get(&key).map(|e| e.fingerprint.clone());
        let fresh = probe_once(self.bridge.as_ref(), locale, self.budget);
        let fingerprint = fresh.fingerprint();
        let outcome = match previous {
            Some(previous) if previous == fingerprint => CacheOutcome::Invalidated,
            Some(_) => CacheOutcome::EnvironmentChanged,
            None => CacheOutcome::Probed,
        };
        self.store(key, &fresh, fingerprint);
        (fresh, outcome)
    }

    fn store(&self, key: String, availability: &AppleAvailability, fingerprint: String) {
        self.entries.lock().insert(
            key,
            CacheEntry {
                availability: availability.clone(),
                fingerprint,
                taken_at: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::{failure_document, success_document, FakeBridge, ScriptedCall};

    use super::*;

    fn speech_result(installed: &[&str], status: &str) -> serde_json::Value {
        serde_json::json!({
            "available": true,
            "supported_locales": ["en_US", "de_DE", "ja_JP"],
            "installed_locales": installed,
            "resolved_locale": "en_US",
            "asset_status": status,
            "locale_installed": installed.contains(&"en_US"),
            "supports_on_device": true,
            "is_transcriber": true,
            "max_concurrent_locales": 5,
            "reserved_locales": [],
            "accepted_formats": [{ "sample_rate_hz": 16000.0, "channels": 1 }],
            "supports_word_timestamps": true,
            "os_build": "26A434",
            "os_version": "27.0.1",
            "hardware": "Apple M4 Pro",
        })
    }

    fn text_result(available: bool) -> serde_json::Value {
        serde_json::json!({
            "available": available,
            "is_transcriber": false,
            "supported_languages": ["en-Latn-US"],
            "supports_structured_output": true,
            "max_context_tokens": null,
            "os_build": "26A434",
            "os_version": "27.0.1",
            "hardware": "Apple M4 Pro",
        })
    }

    fn ready_bridge() -> Arc<FakeBridge> {
        Arc::new(
            FakeBridge::new()
                .reply(
                    BridgeOp::ProbeSpeech,
                    success_document(
                        BridgeOp::ProbeSpeech,
                        speech_result(&["en_US"], "installed"),
                    ),
                )
                .reply(
                    BridgeOp::ProbeFoundationModel,
                    success_document(BridgeOp::ProbeFoundationModel, text_result(true)),
                ),
        )
    }

    #[test]
    fn a_ready_host_maps_to_supported_on_device_capabilities() {
        let bridge = ready_bridge();
        let availability = probe_once(bridge.as_ref(), Some("en-US"), PROBE_BUDGET);
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);

        assert_eq!(caps.speech.status, CapabilityStatus::Supported);
        assert_eq!(
            caps.speech.availability_reason,
            AvailabilityReason::Available
        );
        assert!(caps.speech.offline.satisfies());
        assert!(caps.speech.supports_segments.satisfies());
        assert!(caps.speech.supports_word_timestamps.satisfies());
        assert!(!caps.speech.supports_diarization.satisfies());
        assert_eq!(caps.speech.request_family, RequestFamily::Unsupported);
        assert!(caps
            .speech
            .supports_locale(&LanguageTag::parse("en-US").unwrap()));

        assert_eq!(caps.text_model.status, CapabilityStatus::Supported);
        assert!(caps.text_model.offline.satisfies());
        assert!(caps.text_model.supports_structured_output.satisfies());
        assert_eq!(caps.text_model.max_context_tokens, Known::Unknown);
    }

    #[test]
    fn an_installed_locale_outranks_a_pessimistic_asset_status() {
        // Measured on macOS 27.0.1 (26A434): `AssetInventory.status` reported
        // `supported` for `en_US` while the locale was in `installedLocales`
        // and transcription worked. Gating on the status would refuse a
        // language the machine can transcribe.
        let bridge = FakeBridge::new().reply(
            BridgeOp::ProbeSpeech,
            success_document(
                BridgeOp::ProbeSpeech,
                speech_result(&["en_US"], "supported"),
            ),
        );
        let availability = probe_once(&bridge, Some("en-US"), PROBE_BUDGET);
        let caps = speech_capabilities(&availability.speech, AssetDownloadConsent::Withheld);
        assert_eq!(caps.status, CapabilityStatus::Supported);
        assert_eq!(caps.availability_reason, AvailabilityReason::Available);
    }

    #[test]
    fn an_uninstalled_locale_is_unsupported_with_a_named_asset() {
        let bridge = FakeBridge::new().reply(
            BridgeOp::ProbeSpeech,
            success_document(BridgeOp::ProbeSpeech, speech_result(&[], "supported")),
        );
        let availability = probe_once(&bridge, Some("en-US"), PROBE_BUDGET);
        let caps = speech_capabilities(&availability.speech, AssetDownloadConsent::Withheld);
        assert_eq!(caps.status, CapabilityStatus::Unsupported);
        assert_eq!(
            caps.availability_reason,
            AvailabilityReason::ModelAssetMissing {
                asset: "en_US".to_owned()
            }
        );
        assert!(!caps.is_selectable());
    }

    #[test]
    fn consent_widens_the_offered_locales_but_not_the_status() {
        let bridge = FakeBridge::new().reply(
            BridgeOp::ProbeSpeech,
            success_document(
                BridgeOp::ProbeSpeech,
                speech_result(&["en_US"], "installed"),
            ),
        );
        let availability = probe_once(&bridge, Some("en-US"), PROBE_BUDGET);

        let withheld = speech_capabilities(&availability.speech, AssetDownloadConsent::Withheld);
        assert!(!withheld.supports_locale(&LanguageTag::parse("ja-JP").unwrap()));

        let granted = speech_capabilities(&availability.speech, AssetDownloadConsent::Granted);
        assert!(granted.supports_locale(&LanguageTag::parse("ja-JP").unwrap()));
        assert_eq!(granted.status, CapabilityStatus::Supported);
    }

    #[test]
    fn apple_intelligence_disabled_is_a_capability_verdict() {
        let bridge = FakeBridge::new()
            .reply(
                BridgeOp::ProbeSpeech,
                failure_document(BridgeOp::ProbeSpeech, "platform_unsupported", Some("x")),
            )
            .reply(
                BridgeOp::ProbeFoundationModel,
                failure_document(
                    BridgeOp::ProbeFoundationModel,
                    "disabled_by_user",
                    Some("apple_intelligence_not_enabled"),
                ),
            );
        let availability = probe_once(&bridge, None, PROBE_BUDGET);
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);
        assert_eq!(caps.text_model.status, CapabilityStatus::Unsupported);
        assert_eq!(
            caps.text_model.availability_reason,
            AvailabilityReason::DisabledByUser
        );
        assert_eq!(caps.speech.status, CapabilityStatus::Unsupported);
    }

    #[test]
    fn an_intel_mac_maps_to_hardware_unsupported() {
        let bridge = FakeBridge::new().with_default(|op| {
            failure_document(op, "hardware_unsupported", Some("device_not_eligible"))
        });
        let availability = probe_once(&bridge, None, PROBE_BUDGET);
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);
        assert_eq!(
            caps.text_model.availability_reason,
            AvailabilityReason::HardwareUnsupported {
                detail: "device_not_eligible".to_owned()
            }
        );
        assert_eq!(caps.speech.status, CapabilityStatus::Unsupported);
    }

    #[test]
    fn a_boundary_failure_is_unknown_not_unsupported() {
        // A timeout has not established that the engine is absent.
        let bridge = FakeBridge::new()
            .with_default(|op| failure_document(op, "engine_timeout", Some("2000ms")));
        let availability = probe_once(&bridge, None, PROBE_BUDGET);
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);
        assert_eq!(caps.speech.status, CapabilityStatus::Unknown);
        assert_eq!(caps.text_model.status, CapabilityStatus::Unknown);
        assert!(!caps.speech.is_selectable());
        assert!(!caps.text_model.is_selectable());
    }

    #[test]
    fn an_unlinked_bridge_is_platform_unsupported_and_never_panics() {
        #[derive(Debug)]
        struct Unlinked;
        impl AppleBridge for Unlinked {
            fn abi_version(&self) -> Result<i32, FfiError> {
                Err(FfiError::NotLinked)
            }
            fn new_cancel_token(&self) -> u64 {
                0
            }
            fn cancel(&self, _token: u64) {}
            fn release_cancel_token(&self, _token: u64) {}
            fn is_cancelled(&self, _token: u64) -> bool {
                false
            }
            fn live_token_count(&self) -> u64 {
                0
            }
            fn call(
                &self,
                _op: BridgeOp,
                _request: &str,
                _progress: Option<&crate::bridge::ProgressSink<'_>>,
            ) -> Result<String, FfiError> {
                Err(FfiError::NotLinked)
            }
        }
        let availability = probe_once(&Unlinked, Some("en-US"), PROBE_BUDGET);
        let caps =
            AppleCapabilities::from_availability(&availability, AssetDownloadConsent::Withheld);
        assert_eq!(caps.speech.status, CapabilityStatus::Unsupported);
        assert!(matches!(
            caps.speech.availability_reason,
            AvailabilityReason::PlatformUnsupported { .. }
        ));
    }

    #[test]
    fn an_abi_mismatch_refuses_before_any_operation_runs() {
        let bridge = FakeBridge::new().with_abi_version(crate::envelope::EXPECTED_ABI_VERSION + 1);
        let availability = probe_once(&bridge, None, PROBE_BUDGET);
        assert!(availability.speech.is_err());
        assert!(
            bridge.calls().is_empty(),
            "a version-mismatched library must not be called"
        );
    }

    #[test]
    fn a_text_model_claiming_transcription_is_refused() {
        let bridge = FakeBridge::new().reply(
            BridgeOp::ProbeFoundationModel,
            success_document(
                BridgeOp::ProbeFoundationModel,
                serde_json::json!({ "available": true, "is_transcriber": true }),
            ),
        );
        let availability = probe_once(&bridge, None, PROBE_BUDGET);
        let caps = text_model_capabilities(&availability.text_model);
        assert_eq!(caps.status, CapabilityStatus::Unsupported);
        assert_eq!(
            caps.availability_reason,
            AvailabilityReason::ProbeFailed {
                detail: "text_model_claims_transcription".to_owned()
            }
        );
    }

    #[test]
    fn a_partial_probe_payload_never_yields_support() {
        let bridge = FakeBridge::new().reply(
            BridgeOp::ProbeSpeech,
            success_document(
                BridgeOp::ProbeSpeech,
                serde_json::json!({ "available": true }),
            ),
        );
        let availability = probe_once(&bridge, None, PROBE_BUDGET);
        let caps = speech_capabilities(&availability.speech, AssetDownloadConsent::Withheld);
        // No `is_transcriber`, so the mapping refuses rather than assuming.
        assert_eq!(caps.status, CapabilityStatus::Unsupported);
    }

    #[test]
    fn the_cache_reuses_a_fresh_probe_and_answers_far_inside_the_budget() {
        let bridge = ready_bridge();
        let cache = ProbeCache::new(bridge.clone());

        let (_, first) = cache.availability(Some("en-US"));
        assert_eq!(first, CacheOutcome::Probed);

        let started = Instant::now();
        let (_, second) = cache.availability(Some("en-US"));
        let elapsed = started.elapsed();
        assert_eq!(second, CacheOutcome::Reused);
        assert!(
            elapsed < PROBE_BUDGET,
            "a cached read took {elapsed:?}, over the {PROBE_BUDGET:?} budget"
        );
        assert_eq!(
            bridge.calls_for(BridgeOp::ProbeSpeech).len(),
            1,
            "a reused probe must not call the bridge"
        );
    }

    #[test]
    fn explicit_invalidation_forces_a_re_probe() {
        let bridge = ready_bridge();
        let cache = ProbeCache::new(bridge.clone());
        let _ = cache.availability(None);
        cache.invalidate();
        let (_, outcome) = cache.availability(None);
        assert_eq!(outcome, CacheOutcome::Probed);
        assert_eq!(bridge.calls_for(BridgeOp::ProbeSpeech).len(), 2);
    }

    #[test]
    fn a_changed_environment_replaces_the_cached_verdict() {
        // First probe: no locale installed. Second: installed. The cache is
        // past its age bound, so it re-probes and must notice the change.
        let bridge = Arc::new(
            FakeBridge::new()
                .reply(
                    BridgeOp::ProbeSpeech,
                    success_document(BridgeOp::ProbeSpeech, speech_result(&[], "supported")),
                )
                .reply(
                    BridgeOp::ProbeFoundationModel,
                    success_document(BridgeOp::ProbeFoundationModel, text_result(true)),
                )
                .reply(
                    BridgeOp::ProbeSpeech,
                    success_document(
                        BridgeOp::ProbeSpeech,
                        speech_result(&["en_US"], "installed"),
                    ),
                )
                .reply(
                    BridgeOp::ProbeFoundationModel,
                    success_document(BridgeOp::ProbeFoundationModel, text_result(true)),
                ),
        );
        let cache = ProbeCache::new(bridge).with_max_age(Duration::ZERO);

        let (before, _) = cache.availability(Some("en-US"));
        assert_eq!(
            speech_capabilities(&before.speech, AssetDownloadConsent::Withheld).status,
            CapabilityStatus::Unsupported
        );

        let (after, outcome) = cache.availability(Some("en-US"));
        assert_eq!(outcome, CacheOutcome::EnvironmentChanged);
        assert_eq!(
            speech_capabilities(&after.speech, AssetDownloadConsent::Withheld).status,
            CapabilityStatus::Supported
        );
    }

    #[test]
    fn an_unchanged_environment_past_its_age_is_merely_expired() {
        let bridge = Arc::new(FakeBridge::new().with_default(|op| match op {
            BridgeOp::ProbeSpeech => success_document(op, speech_result(&["en_US"], "installed")),
            _ => success_document(op, text_result(true)),
        }));
        let cache = ProbeCache::new(bridge).with_max_age(Duration::ZERO);
        let _ = cache.availability(None);
        let (_, outcome) = cache.availability(None);
        assert_eq!(outcome, CacheOutcome::Expired);
    }

    #[test]
    fn exceeding_the_probe_budget_yields_unknown_rather_than_a_stale_answer() {
        let bridge = FakeBridge::new().script(
            BridgeOp::ProbeSpeech,
            ScriptedCall::Dynamic(Box::new(|_request, _progress| {
                std::thread::sleep(Duration::from_millis(40));
                Ok(success_document(
                    BridgeOp::ProbeSpeech,
                    serde_json::json!({ "available": true, "is_transcriber": true }),
                ))
            })),
        );
        let availability = probe_once(&bridge, None, Duration::from_millis(1));
        assert_eq!(
            availability.speech.as_ref().err().map(|e| e.code.clone()),
            Some(BridgeErrorCode::EngineTimeout)
        );
        let caps = speech_capabilities(&availability.speech, AssetDownloadConsent::Withheld);
        assert_eq!(caps.status, CapabilityStatus::Unknown);
    }

    #[test]
    fn apple_underscore_locales_normalize_to_bcp_47() {
        assert_eq!(parse_locale("en_US").unwrap().as_str(), "en-US");
        assert_eq!(parse_locale("zh_Hans_CN").unwrap().as_str(), "zh-hans-CN");
        assert!(parse_locale("").is_err());
    }

    #[test]
    fn the_fingerprint_changes_with_each_invalidating_fact() {
        let base = probe_once(ready_bridge().as_ref(), Some("en-US"), PROBE_BUDGET);
        let baseline = base.fingerprint();

        let mut changed = base.clone();
        if let Ok(probe) = &mut changed.speech {
            probe.os_build = "26B100".to_owned();
        }
        assert_ne!(changed.fingerprint(), baseline, "an OS update must show");

        let mut changed = base.clone();
        if let Ok(probe) = &mut changed.speech {
            probe.installed_locales.push("ja_JP".to_owned());
        }
        assert_ne!(changed.fingerprint(), baseline, "a new asset must show");

        let mut changed = base.clone();
        changed.text_model = Err(BridgeError::new(BridgeErrorCode::DisabledByUser, None));
        assert_ne!(
            changed.fingerprint(),
            baseline,
            "an Apple Intelligence toggle must show"
        );
    }
}
