//! Fail-closed capability negotiation (spec 8.3, 8.4, 8.6A).
//!
//! Selection answers one question: *may this engine run this job?* Every
//! rejection carries the reason, so the UI can explain the decision instead of
//! silently downgrading.

use plaudon_domain::locale::LanguageTag;
use plaudon_domain::model::EngineId;
use serde::{Deserialize, Serialize};

use crate::capabilities::{MediaFormat, SttCapabilities};

/// What a job needs from a transcription engine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SttRequirements {
    /// Media format of the input.
    pub format: Option<MediaFormat>,
    /// Size of the input in bytes.
    pub byte_length: Option<u64>,
    /// Duration of the input in milliseconds.
    pub duration_ms: Option<u64>,
    /// Required locale.
    pub locale: Option<LanguageTag>,
    /// Whether the job must run without leaving the machine.
    pub require_offline: bool,
    /// Whether the job needs segment boundaries.
    pub require_segments: bool,
    /// Whether the job needs word-level timings.
    pub require_word_timestamps: bool,
    /// Whether the job must be cancellable.
    pub require_cancellation: bool,
}

/// Why a candidate engine was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rejection", rename_all = "snake_case")]
pub enum EngineRejection {
    /// The capability is not validated for this configuration.
    NotSupportedStatus,
    /// The runtime probe says the engine is unavailable.
    NotAvailable,
    /// Offline processing was required but the engine does not declare it.
    NotOffline,
    /// The engine does not declare the input format.
    FormatNotDeclared,
    /// The input is larger than the declared maximum.
    FileTooLarge,
    /// The input is longer than the declared maximum.
    DurationTooLong,
    /// The engine does not declare the required locale.
    LocaleNotDeclared,
    /// Segment boundaries were required but not declared.
    SegmentsNotDeclared,
    /// Word timings were required but not declared.
    WordTimestampsNotDeclared,
    /// Cancellation was required but not declared.
    CancellationNotDeclared,
    /// The engine declares no usable request family.
    NoRequestBuilder,
    /// The privacy route could not be resolved.
    PrivacyUnresolved,
}

/// The outcome of engine selection.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineSelection {
    /// The chosen engine, if any candidate qualified.
    pub chosen: Option<EngineId>,
    /// Every rejected candidate with its reason, in candidate order.
    pub rejected: Vec<(EngineId, EngineRejection)>,
}

/// Evaluate one candidate against a job's requirements.
///
/// Returns `None` when the candidate qualifies.
#[must_use]
pub fn evaluate_stt_candidate(
    caps: &SttCapabilities,
    requirements: &SttRequirements,
) -> Option<EngineRejection> {
    if !caps.effective_status().is_selectable() {
        return Some(EngineRejection::NotSupportedStatus);
    }
    if !caps.availability_reason.is_available() {
        return Some(EngineRejection::NotAvailable);
    }
    let offline = caps.offline.satisfies();
    if requirements.require_offline && !offline {
        return Some(EngineRejection::NotOffline);
    }
    if !offline {
        match caps.privacy_class.get() {
            Some(class) if class.is_dispatchable() => {}
            _ => return Some(EngineRejection::PrivacyUnresolved),
        }
        if !caps.request_family.has_builder() {
            return Some(EngineRejection::NoRequestBuilder);
        }
    }
    if let Some(format) = &requirements.format {
        if !caps.supports_format(format) {
            return Some(EngineRejection::FormatNotDeclared);
        }
    }
    if let Some(bytes) = requirements.byte_length {
        match caps.max_file_bytes.get() {
            Some(&max) if bytes <= max => {}
            Some(_) => return Some(EngineRejection::FileTooLarge),
            None => {}
        }
    }
    if let Some(duration) = requirements.duration_ms {
        match caps.max_duration_ms.get() {
            Some(&max) if duration <= max => {}
            Some(_) => return Some(EngineRejection::DurationTooLong),
            None => {}
        }
    }
    if let Some(locale) = &requirements.locale {
        if !caps.supports_locale(locale) {
            return Some(EngineRejection::LocaleNotDeclared);
        }
    }
    if requirements.require_segments && !caps.supports_segments.satisfies() {
        return Some(EngineRejection::SegmentsNotDeclared);
    }
    if requirements.require_word_timestamps && !caps.supports_word_timestamps.satisfies() {
        return Some(EngineRejection::WordTimestampsNotDeclared);
    }
    if requirements.require_cancellation && !caps.can_cancel.satisfies() {
        return Some(EngineRejection::CancellationNotDeclared);
    }
    None
}

/// Choose the first candidate that satisfies `requirements`.
///
/// Candidates are evaluated in the caller's preference order. There is no
/// automatic fallback across privacy classes: a caller that wants a local
/// engine passes only local candidates (spec 11.6, 10.6).
#[must_use]
pub fn select_stt_engine(
    candidates: &[SttCapabilities],
    requirements: &SttRequirements,
) -> EngineSelection {
    let mut rejected = Vec::new();
    for caps in candidates {
        match evaluate_stt_candidate(caps, requirements) {
            None => return EngineSelection { chosen: Some(caps.engine_id.clone()), rejected },
            Some(reason) => rejected.push((caps.engine_id.clone(), reason)),
        }
    }
    EngineSelection { chosen: None, rejected }
}

#[cfg(test)]
mod tests {
    use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
    use plaudon_domain::model::ModelIdentity;

    use super::*;
    use crate::capabilities::{AvailabilityReason, PrivacyClass, RequestFamily};

    fn local_engine(name: &str) -> SttCapabilities {
        let engine = EngineId::new(name);
        let mut caps = SttCapabilities::unknown(engine.clone(), ModelIdentity::new(engine, "m"));
        caps.status = CapabilityStatus::Supported;
        caps.availability_reason = AvailabilityReason::Available;
        caps.offline = Tri::Yes;
        caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
        caps.supported_formats = Known::Known(vec![MediaFormat::new("wav", "pcm_s16le")]);
        caps.supports_segments = Tri::Yes;
        caps.can_cancel = Tri::Yes;
        caps.locales = Known::Known(vec![LanguageTag::parse("en").unwrap()]);
        caps
    }

    fn wav_job() -> SttRequirements {
        SttRequirements {
            format: Some(MediaFormat::new("wav", "pcm_s16le")),
            locale: Some(LanguageTag::parse("en-US").unwrap()),
            ..SttRequirements::default()
        }
    }

    #[test]
    fn a_qualified_engine_is_chosen() {
        let selection = select_stt_engine(&[local_engine("whisper-cpp")], &wav_job());
        assert_eq!(selection.chosen, Some(EngineId::new("whisper-cpp")));
        assert!(selection.rejected.is_empty());
    }

    #[test]
    fn an_experimental_capability_cannot_be_selected() {
        let mut caps = local_engine("experimental");
        caps.status = CapabilityStatus::Experimental;
        let selection = select_stt_engine(&[caps], &wav_job());
        assert_eq!(selection.chosen, None);
        assert_eq!(selection.rejected[0].1, EngineRejection::NotSupportedStatus);
    }

    #[test]
    fn an_unknown_capability_cannot_be_selected() {
        let mut caps = local_engine("unknown-version");
        caps.payload_version = 42;
        let selection = select_stt_engine(&[caps], &wav_job());
        assert_eq!(selection.rejected[0].1, EngineRejection::NotSupportedStatus);
    }

    #[test]
    fn an_offline_requirement_rejects_a_cloud_engine() {
        let mut cloud = local_engine("cloud");
        cloud.offline = Tri::No;
        cloud.privacy_class = Known::Known(PrivacyClass::CloudZeroRetention);
        cloud.request_family = RequestFamily::MultipartStt;

        let mut requirements = wav_job();
        requirements.require_offline = true;
        let selection = select_stt_engine(&[cloud], &requirements);
        assert_eq!(selection.rejected[0].1, EngineRejection::NotOffline);
    }

    #[test]
    fn a_cloud_engine_with_an_unresolved_route_is_refused() {
        let mut cloud = local_engine("cloud");
        cloud.offline = Tri::No;
        cloud.request_family = RequestFamily::MultipartStt;
        cloud.privacy_class = Known::Known(PrivacyClass::Unresolved);
        let selection = select_stt_engine(&[cloud], &wav_job());
        assert_eq!(selection.rejected[0].1, EngineRejection::PrivacyUnresolved);
    }

    #[test]
    fn a_cloud_engine_with_an_unknown_request_family_has_no_builder() {
        let mut cloud = local_engine("cloud");
        cloud.offline = Tri::No;
        cloud.privacy_class = Known::Known(PrivacyClass::CloudZeroRetention);
        cloud.request_family = RequestFamily::Unknown;
        let selection = select_stt_engine(&[cloud], &wav_job());
        assert_eq!(selection.rejected[0].1, EngineRejection::NoRequestBuilder);
    }

    #[test]
    fn declared_limits_are_enforced() {
        let mut caps = local_engine("limited");
        caps.max_file_bytes = Known::Known(1_000);
        caps.max_duration_ms = Known::Known(60_000);

        let mut too_big = wav_job();
        too_big.byte_length = Some(2_000);
        assert_eq!(
            evaluate_stt_candidate(&caps, &too_big),
            Some(EngineRejection::FileTooLarge)
        );

        let mut too_long = wav_job();
        too_long.duration_ms = Some(120_000);
        assert_eq!(
            evaluate_stt_candidate(&caps, &too_long),
            Some(EngineRejection::DurationTooLong)
        );
    }

    #[test]
    fn unknown_optional_capabilities_never_satisfy_a_requirement() {
        let mut caps = local_engine("partial");
        caps.supports_word_timestamps = Tri::Unknown;
        let mut requirements = wav_job();
        requirements.require_word_timestamps = true;
        assert_eq!(
            evaluate_stt_candidate(&caps, &requirements),
            Some(EngineRejection::WordTimestampsNotDeclared)
        );
    }

    #[test]
    fn selection_reports_every_rejection_in_candidate_order() {
        let mut unavailable = local_engine("apple-speech");
        unavailable.availability_reason = AvailabilityReason::DisabledByUser;
        let selection =
            select_stt_engine(&[unavailable, local_engine("whisper-cpp")], &wav_job());
        assert_eq!(selection.chosen, Some(EngineId::new("whisper-cpp")));
        assert_eq!(
            selection.rejected,
            vec![(EngineId::new("apple-speech"), EngineRejection::NotAvailable)]
        );
    }
}
