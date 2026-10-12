//! Bridge document to capability mapping (spec 8.3, 11.3, 11.4, 11.5).
//!
//! Pure, platform-independent and therefore testable without a Mac. Given
//! what the Swift side reported, decide what the application is allowed to
//! believe.

use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, MediaFormat, PrivacyClass, RequestFamily, SttCapabilities,
    TextModelCapabilities,
};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::transcript::AlignmentOrigin;
use serde::{Deserialize, Serialize};

/// Engine identity of the Apple speech transcriber.
pub const APPLE_SPEECH_ENGINE: &str = "apple-speech";

/// Engine identity of the Apple on-device language model.
pub const APPLE_FOUNDATION_ENGINE: &str = "apple-foundation";

/// An error the bridge reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeError {
    /// Machine-readable reason, from the bridge's fixed vocabulary.
    pub error: String,
    /// Optional short discriminator. Never user-facing prose.
    #[serde(default)]
    pub detail: Option<String>,
}

/// A successful availability probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeDocument {
    /// Whether the capability is ready now.
    #[serde(default)]
    pub available: bool,
    /// Locales the platform reports as installed or installable.
    #[serde(default)]
    pub locales: Vec<String>,
    /// Whether on-device recognition is possible.
    #[serde(default)]
    pub supports_on_device: bool,
    /// Whether this capability transcribes. Always false for the text model.
    #[serde(default)]
    pub is_transcriber: bool,
}

/// A successful transcription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionDocument {
    /// Full formatted text.
    pub text: String,
    /// Segments, when the engine reported any.
    #[serde(default)]
    pub segments: Vec<serde_json::Value>,
    /// Alignment origin the bridge observed in this response.
    pub alignment_origin: AlignmentOrigin,
    /// Locale the run used.
    #[serde(default)]
    pub locale: Option<String>,
}

/// Either half of a bridge reply.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeDocument<T> {
    /// The call succeeded.
    Ok(T),
    /// The call failed, with a reason from the bridge vocabulary.
    Err(BridgeError),
}

impl<T: serde::de::DeserializeOwned> BridgeDocument<T> {
    /// Parse a bridge reply.
    ///
    /// A document this build cannot understand is a `probe_failed` error, not
    /// a success: an unparsed reply must never read as availability.
    #[must_use]
    pub fn parse(json: &str) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::Err(BridgeError {
                error: "probe_failed".to_owned(),
                detail: Some("unparseable_document".to_owned()),
            });
        };
        if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            return match serde_json::from_value(value) {
                Ok(parsed) => Self::Ok(parsed),
                Err(_) => Self::Err(BridgeError {
                    error: "probe_failed".to_owned(),
                    detail: Some("unexpected_document_shape".to_owned()),
                }),
            };
        }
        match serde_json::from_value::<BridgeError>(value) {
            Ok(error) => Self::Err(error),
            Err(_) => Self::Err(BridgeError {
                error: "probe_failed".to_owned(),
                detail: Some("unexpected_document_shape".to_owned()),
            }),
        }
    }
}

/// Map a bridge error reason onto the capability vocabulary.
#[must_use]
pub fn availability_reason(error: &BridgeError) -> AvailabilityReason {
    match error.error.as_str() {
        "platform_unsupported" => AvailabilityReason::PlatformUnsupported {
            platform: error
                .detail
                .clone()
                .unwrap_or_else(|| "non_macos".to_owned()),
        },
        "hardware_unsupported" => AvailabilityReason::HardwareUnsupported {
            detail: error
                .detail
                .clone()
                .unwrap_or_else(|| "device_not_eligible".to_owned()),
        },
        "disabled_by_user" => AvailabilityReason::DisabledByUser,
        // The operating system has not granted this process speech
        // recognition. Only an application bundle carrying
        // NSSpeechRecognitionUsageDescription can obtain it.
        "permission_denied" => AvailabilityReason::ProbeFailed {
            detail: error
                .detail
                .clone()
                .unwrap_or_else(|| "permission_denied".to_owned()),
        },
        "engine_timeout" => AvailabilityReason::ProbeFailed {
            detail: error
                .detail
                .clone()
                .unwrap_or_else(|| "engine_timeout".to_owned()),
        },
        "model_asset_not_ready" => AvailabilityReason::ModelAssetNotReady {
            asset: error
                .detail
                .clone()
                .unwrap_or_else(|| "speech_locale".to_owned()),
        },
        "model_asset_missing" => AvailabilityReason::ModelAssetMissing {
            asset: error
                .detail
                .clone()
                .unwrap_or_else(|| "speech_locale".to_owned()),
        },
        "locale_unsupported" => error
            .detail
            .as_deref()
            .and_then(|tag| LanguageTag::parse(tag).ok())
            .map_or_else(
                || AvailabilityReason::ProbeFailed {
                    detail: "locale_unsupported".to_owned(),
                },
                |locale| AvailabilityReason::LocaleUnsupported { locale },
            ),
        // Anything we do not recognize is a failed probe. It is never
        // "available with a caveat".
        _ => AvailabilityReason::ProbeFailed {
            detail: error.detail.clone().unwrap_or_else(|| error.error.clone()),
        },
    }
}

/// Build speech capabilities from a probe reply.
///
/// `Supported` requires an affirmative, on-device, locale-bearing probe. A
/// probe that merely parsed is `Unknown`.
#[must_use]
pub fn speech_availability(document: &BridgeDocument<ProbeDocument>) -> SttCapabilities {
    let engine = EngineId::new(APPLE_SPEECH_ENGINE);
    let mut caps = SttCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, "speech-transcriber"),
    );
    // Apple speech is a platform capability, not a cloud route.
    caps.request_family = RequestFamily::Unsupported;
    caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
    caps.model_download_bytes = Known::Unknown;

    match document {
        BridgeDocument::Err(error) => {
            caps.availability_reason = availability_reason(error);
            caps.status = CapabilityStatus::Unsupported;
            caps.offline = Tri::Unknown;
            caps
        }
        BridgeDocument::Ok(probe) => {
            if !probe.available {
                caps.availability_reason = AvailabilityReason::ProbeFailed {
                    detail: "reported_unavailable".to_owned(),
                };
                caps.status = CapabilityStatus::Unsupported;
                return caps;
            }
            let locales: Vec<LanguageTag> = probe
                .locales
                .iter()
                .filter_map(|tag| LanguageTag::parse(tag).ok())
                .collect();
            caps.availability_reason = AvailabilityReason::Available;
            caps.offline = Tri::from_observation(Some(probe.supports_on_device));
            caps.can_cancel = Tri::Yes;
            caps.supported_formats = Known::Known(vec![
                MediaFormat::new("wav", "pcm_s16le"),
                MediaFormat::new("mp4", "aac"),
                MediaFormat::new("mp3", "mp3"),
            ]);
            if locales.is_empty() {
                caps.locales = Known::Unknown;
            } else {
                caps.locales = Known::Known(locales);
            }
            // Segment and word-timestamp support are only known once a real
            // response has been seen; the probe cannot establish them, and
            // spec 8.3 says unknown is not supported.
            caps.supports_segments = Tri::Unknown;
            caps.supports_word_timestamps = Tri::Unknown;
            caps.supports_diarization = Tri::No;
            caps.status = if probe.supports_on_device && !caps.locales.is_known() {
                CapabilityStatus::Experimental
            } else if probe.supports_on_device {
                CapabilityStatus::Supported
            } else {
                // Without on-device recognition the audio would leave the
                // machine, which is not what this engine is for.
                CapabilityStatus::Unsupported
            };
            caps
        }
    }
}

/// Build text-model capabilities from a probe reply.
#[must_use]
pub fn text_model_availability(document: &BridgeDocument<ProbeDocument>) -> TextModelCapabilities {
    let engine = EngineId::new(APPLE_FOUNDATION_ENGINE);
    let mut caps = TextModelCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, "system-language-model"),
    );
    caps.privacy_class = Known::Known(PrivacyClass::OnDevice);

    match document {
        BridgeDocument::Err(error) => {
            caps.availability_reason = availability_reason(error);
            caps.status = CapabilityStatus::Unsupported;
        }
        BridgeDocument::Ok(probe) => {
            if probe.available {
                caps.availability_reason = AvailabilityReason::Available;
                caps.status = CapabilityStatus::Supported;
                caps.offline = Tri::Yes;
                caps.can_cancel = Tri::Yes;
                // Not probed: the bridge does not report a context window, and
                // guessing one would be a fabricated limit.
                caps.max_context_tokens = Known::Unknown;
                caps.supports_structured_output = Tri::Unknown;
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

/// Whether a transcription reply may be turned into a revision.
///
/// Spec 11.3: a partial or volatile update is never a final transcript, and
/// spec 8.6A forbids a timeline the engine did not report.
#[must_use]
pub fn transcription_is_final(document: &TranscriptionDocument) -> bool {
    if document.text.trim().is_empty() {
        return false;
    }
    match document.alignment_origin {
        AlignmentOrigin::None => document.segments.is_empty(),
        AlignmentOrigin::Native | AlignmentOrigin::Derived => !document.segments.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(json: &str) -> BridgeDocument<ProbeDocument> {
        BridgeDocument::parse(json)
    }

    #[test]
    fn a_successful_on_device_probe_yields_a_supported_capability() {
        let caps = speech_availability(&probe(
            r#"{"ok":true,"available":true,"supports_on_device":true,"locales":["en-US","de-DE"]}"#,
        ));
        assert_eq!(caps.status, CapabilityStatus::Supported);
        assert_eq!(caps.availability_reason, AvailabilityReason::Available);
        assert!(caps.offline.satisfies());
        assert!(caps.supports_locale(&LanguageTag::parse("en-GB").unwrap()));
        assert!(caps.is_selectable());
    }

    #[test]
    fn segment_and_word_timing_support_are_never_assumed_from_a_probe() {
        let caps = speech_availability(&probe(
            r#"{"ok":true,"available":true,"supports_on_device":true,"locales":["en-US"]}"#,
        ));
        assert_eq!(caps.supports_segments, Tri::Unknown);
        assert_eq!(caps.supports_word_timestamps, Tri::Unknown);
        assert!(!caps.supports_segments.satisfies());
    }

    #[test]
    fn every_unavailable_case_yields_a_typed_reason_and_is_not_selectable() {
        let cases = [
            (
                r#"{"ok":false,"error":"platform_unsupported"}"#,
                AvailabilityReason::PlatformUnsupported {
                    platform: "non_macos".to_owned(),
                },
            ),
            (
                r#"{"ok":false,"error":"hardware_unsupported"}"#,
                AvailabilityReason::HardwareUnsupported {
                    detail: "device_not_eligible".to_owned(),
                },
            ),
            (
                r#"{"ok":false,"error":"disabled_by_user"}"#,
                AvailabilityReason::DisabledByUser,
            ),
            (
                r#"{"ok":false,"error":"model_asset_not_ready","detail":"en-US"}"#,
                AvailabilityReason::ModelAssetNotReady {
                    asset: "en-US".to_owned(),
                },
            ),
        ];
        for (json, expected) in cases {
            let caps = speech_availability(&probe(json));
            assert_eq!(caps.availability_reason, expected, "{json}");
            assert!(!caps.is_selectable(), "{json}");
        }
    }

    #[test]
    fn an_unsupported_locale_is_reported_as_a_locale_problem() {
        let caps = speech_availability(&probe(
            r#"{"ok":false,"error":"locale_unsupported","detail":"ja-JP"}"#,
        ));
        assert_eq!(
            caps.availability_reason,
            AvailabilityReason::LocaleUnsupported {
                locale: LanguageTag::parse("ja-JP").unwrap()
            }
        );
    }

    #[test]
    fn an_unparseable_or_unknown_document_is_a_failed_probe_not_availability() {
        for json in [
            "not json at all",
            "{}",
            r#"{"ok":false}"#,
            r#"{"ok":false,"error":"the_vibes_are_off"}"#,
            r#"{"ok":true,"available":"yes"}"#,
        ] {
            let caps = speech_availability(&probe(json));
            assert!(!caps.is_selectable(), "{json} must not be selectable");
            assert_ne!(
                caps.availability_reason,
                AvailabilityReason::Available,
                "{json}"
            );
        }
    }

    #[test]
    fn an_engine_without_on_device_recognition_is_not_offered() {
        let caps = speech_availability(&probe(
            r#"{"ok":true,"available":true,"supports_on_device":false,"locales":["en-US"]}"#,
        ));
        assert_eq!(caps.status, CapabilityStatus::Unsupported);
        assert!(!caps.is_selectable());
    }

    #[test]
    fn an_available_engine_with_no_locale_list_stays_experimental() {
        let caps = speech_availability(&probe(
            r#"{"ok":true,"available":true,"supports_on_device":true,"locales":[]}"#,
        ));
        assert_eq!(caps.status, CapabilityStatus::Experimental);
        assert!(!caps.is_selectable());
    }

    #[test]
    fn the_foundation_model_is_a_text_model_and_never_a_transcriber() {
        let caps = text_model_availability(&probe(
            r#"{"ok":true,"available":true,"is_transcriber":false}"#,
        ));
        assert_eq!(caps.status, CapabilityStatus::Supported);
        assert!(caps.offline.satisfies());
        assert_eq!(
            caps.max_context_tokens,
            Known::Unknown,
            "a context window is never guessed"
        );

        // The speech mapping and the text mapping are different engines; a
        // text model can never satisfy a transcription requirement.
        let speech = speech_availability(&probe(r#"{"ok":false,"error":"disabled_by_user"}"#));
        assert_ne!(speech.engine_id.as_str(), caps.engine_id.as_str());
    }

    #[test]
    fn a_guardrail_refusal_is_an_unavailability_not_a_result() {
        let caps = text_model_availability(&probe(
            r#"{"ok":false,"error":"guardrail_refusal","detail":"GenerationError"}"#,
        ));
        assert_eq!(caps.status, CapabilityStatus::Unsupported);
        assert_eq!(
            caps.availability_reason,
            AvailabilityReason::ProbeFailed {
                detail: "GenerationError".to_owned()
            }
        );
    }

    #[test]
    fn a_text_only_transcription_is_final_only_without_segments() {
        let text_only = TranscriptionDocument {
            text: "hello".into(),
            segments: Vec::new(),
            alignment_origin: AlignmentOrigin::None,
            locale: Some("en-US".into()),
        };
        assert!(transcription_is_final(&text_only));

        let inconsistent = TranscriptionDocument {
            alignment_origin: AlignmentOrigin::Native,
            ..text_only.clone()
        };
        assert!(
            !transcription_is_final(&inconsistent),
            "a native alignment claim with no segments is not a usable result"
        );

        let empty = TranscriptionDocument {
            text: "   ".into(),
            ..text_only
        };
        assert!(!transcription_is_final(&empty));
    }

    #[test]
    fn an_unauthorized_process_is_refused_rather_than_left_to_stall() {
        // Observed during the A1.3 spike: an unauthorized recognition task
        // never finalizes, so the bridge checks the authorization status
        // before starting one.
        let caps = speech_availability(&probe(
            r#"{"ok":false,"error":"permission_denied","detail":"speech_recognition_not_authorized_for_this_process"}"#,
        ));
        assert_eq!(
            caps.availability_reason,
            AvailabilityReason::ProbeFailed {
                detail: "speech_recognition_not_authorized_for_this_process".to_owned()
            }
        );
        assert!(!caps.is_selectable());
    }

    #[test]
    fn a_stalled_engine_times_out_rather_than_hanging_the_caller() {
        // Observed during the A1.3 spike: SFSpeechRecognizer never finalizes
        // on audio containing no speech, so every long-running bridge call
        // carries a wall-clock budget and reports engine_timeout.
        let document: BridgeDocument<TranscriptionDocument> =
            BridgeDocument::parse(r#"{"ok":false,"error":"engine_timeout","detail":"120000ms"}"#);
        let BridgeDocument::Err(error) = document else {
            panic!("a timeout must not yield a transcript");
        };
        assert_eq!(
            availability_reason(&error),
            AvailabilityReason::ProbeFailed {
                detail: "120000ms".to_owned()
            }
        );
    }

    #[test]
    fn a_cancelled_call_is_an_error_document_and_produces_no_transcript() {
        let document: BridgeDocument<TranscriptionDocument> =
            BridgeDocument::parse(r#"{"ok":false,"error":"cancelled"}"#);
        match document {
            BridgeDocument::Err(error) => assert_eq!(error.error, "cancelled"),
            BridgeDocument::Ok(_) => panic!("a cancelled call must not yield a transcript"),
        }
    }
}
