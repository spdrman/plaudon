//! The diarization plugin contract (spec 8.7, 6.6).
//!
//! Diarization is an **independent stage**: `Audio → STT` and, optionally,
//! `Audio → Diarization`. Nothing in this module can touch a transcript, by
//! construction — the trait takes media bytes and returns speaker turns, and
//! has no access to a [`plaudon_domain::transcript::TranscriptRevision`] at
//! all. Reconciliation against transcript time ranges happens afterwards, in
//! `plaudon_app_core::diarization`.
//!
//! A cluster is not an identity. The output carries opaque cluster keys; the
//! label `Speaker 1` is assigned later and is explicitly a cluster label, not
//! a legal identity (spec 17.6).

use std::fmt;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{AvailabilityReason, HardwareRequirements, PrivacyClass};
use plaudon_ai_contracts::job::JobContext;
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::money::MoneyEstimate;
use serde::{Deserialize, Serialize};

/// Contract version of the diarization payloads (spec 6.6).
pub const DIARIZATION_CONTRACT_VERSION: u32 = 1;

/// Opaque identity of one inferred speaker cluster within a run.
///
/// Deliberately a string rather than a number: providers emit their own
/// cluster names and remapping them to indices would lose the ability to
/// correlate a turn with the provider's own diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClusterKey(String);

impl ClusterKey {
    /// Wrap a provider cluster identifier.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClusterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One speaker turn as reported by a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerTurn {
    /// Cluster this turn belongs to.
    pub cluster: ClusterKey,
    /// Inclusive start offset in milliseconds.
    pub start_ms: u64,
    /// Exclusive end offset in milliseconds.
    pub end_ms: u64,
    /// Provider confidence, when one is reported. Never compared across
    /// providers.
    pub confidence: Option<f64>,
}

impl SpeakerTurn {
    /// Construct a turn.
    #[must_use]
    pub fn new(cluster: impl Into<String>, start_ms: u64, end_ms: u64) -> Self {
        Self {
            cluster: ClusterKey::new(cluster),
            start_ms,
            end_ms,
            confidence: None,
        }
    }

    /// Duration in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// Overlap in milliseconds with a half-open range.
    #[must_use]
    pub const fn overlap_ms(&self, start_ms: u64, end_ms: u64) -> u64 {
        let start = if self.start_ms > start_ms {
            self.start_ms
        } else {
            start_ms
        };
        let end = if self.end_ms < end_ms {
            self.end_ms
        } else {
            end_ms
        };
        if end > start {
            end - start
        } else {
            0
        }
    }
}

/// A complete diarization result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiarizationResult {
    /// Contract version of this payload.
    pub payload_version: u32,
    /// Provider that produced it.
    pub provider_id: String,
    /// Provider version, when known.
    pub provider_version: Option<String>,
    /// Turns in chronological order. Overlapping turns are legal: two people
    /// can talk at once, and flattening that would be a lie.
    pub turns: Vec<SpeakerTurn>,
}

impl DiarizationResult {
    /// Distinct clusters in the result, in first-appearance order.
    #[must_use]
    pub fn clusters(&self) -> Vec<ClusterKey> {
        let mut seen = Vec::new();
        for turn in &self.turns {
            if !seen.contains(&turn.cluster) {
                seen.push(turn.cluster.clone());
            }
        }
        seen
    }

    /// Whether any two turns overlap in time.
    #[must_use]
    pub fn has_overlapping_speech(&self) -> bool {
        for (index, turn) in self.turns.iter().enumerate() {
            for other in &self.turns[index + 1..] {
                if turn.overlap_ms(other.start_ms, other.end_ms) > 0 {
                    return true;
                }
            }
        }
        false
    }

    /// Validate the structural invariants a provider must satisfy.
    pub fn validate(&self) -> Result<(), DiarizationError> {
        if self.payload_version != DIARIZATION_CONTRACT_VERSION {
            return Err(DiarizationError::UnsupportedPayloadVersion);
        }
        for turn in &self.turns {
            if turn.end_ms < turn.start_ms {
                return Err(DiarizationError::MalformedOutput);
            }
            if turn.cluster.as_str().trim().is_empty() {
                return Err(DiarizationError::MalformedOutput);
            }
        }
        if self
            .turns
            .windows(2)
            .any(|pair| pair[1].start_ms < pair[0].start_ms)
        {
            return Err(DiarizationError::MalformedOutput);
        }
        Ok(())
    }
}

/// What a diarization provider can do, probed at runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiarizationCapabilities {
    /// Contract version of this payload.
    pub payload_version: u32,
    /// Validation status for the current configuration.
    pub status: CapabilityStatus,
    /// Stable provider identifier.
    pub provider_id: String,
    /// Whether the provider runs without leaving the machine.
    pub offline: Tri,
    /// Whether the provider reports overlapping speech at all.
    pub supports_overlapping_speech: Tri,
    /// Whether an in-flight run can be cancelled.
    pub can_cancel: Tri,
    /// Maximum media duration the provider accepts, in milliseconds.
    pub max_duration_ms: Known<u64>,
    /// Where processing happens.
    pub privacy_class: Known<PrivacyClass>,
    /// Added cost of running this provider, where the basis is published.
    pub added_cost: MoneyEstimate,
    /// Bytes that must be downloaded before first use.
    pub model_download_bytes: Known<u64>,
    /// Minimum hardware.
    pub hardware_requirements: Option<HardwareRequirements>,
    /// Runtime availability result.
    pub availability_reason: AvailabilityReason,
}

impl DiarizationCapabilities {
    /// Whether this provider may be offered in the processing sheet.
    ///
    /// Spec 8.7: the option is exposed only for validated providers. `Unknown`
    /// is not validated.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        matches!(
            self.status,
            CapabilityStatus::Supported | CapabilityStatus::Experimental
        ) && matches!(self.availability_reason, AvailabilityReason::Available)
            && self
                .privacy_class
                .get()
                .is_some_and(|c| c.is_dispatchable())
    }

    /// Whether choosing this provider needs explicit cloud consent.
    #[must_use]
    pub fn requires_cloud_consent(&self) -> bool {
        self.privacy_class
            .get()
            .is_none_or(|class| class.requires_cloud_consent())
    }
}

/// Failures a diarization provider may report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiarizationError {
    /// The run was cancelled.
    #[error("diarization was cancelled")]
    Cancelled,
    /// The provider or its assets are unavailable.
    #[error("the diarization provider is unavailable")]
    Unavailable,
    /// The provider is not in the validated allowlist.
    #[error("this diarization provider is not validated")]
    NotValidated,
    /// Consent has not been granted for this provider's privacy class.
    #[error("consent is required before this diarization provider may run")]
    ConsentRequired,
    /// Local-only Mode forbids this provider's network use.
    #[error("local-only mode refused this diarization provider")]
    LocalOnlyMode,
    /// The media exceeds the provider's duration limit.
    #[error("the recording is longer than this provider accepts")]
    DurationExceeded,
    /// The provider returned output that violates the contract.
    #[error("the diarization provider returned malformed output")]
    MalformedOutput,
    /// The payload version is not one this build understands.
    #[error("the diarization payload version is not supported")]
    UnsupportedPayloadVersion,
    /// The provider failed in a way this build does not model.
    #[error("diarization failed")]
    EngineFailure,
}

impl From<DiarizationError> for AppError {
    fn from(error: DiarizationError) -> Self {
        let code = match error {
            DiarizationError::Cancelled => AppErrorCode::Canceled,
            DiarizationError::Unavailable => AppErrorCode::ModelUnavailable,
            DiarizationError::NotValidated => AppErrorCode::CapabilityUnverified,
            DiarizationError::ConsentRequired => AppErrorCode::ConsentRequired,
            DiarizationError::LocalOnlyMode => AppErrorCode::LocalOnlyMode,
            DiarizationError::DurationExceeded => AppErrorCode::ContextTooLarge,
            DiarizationError::MalformedOutput | DiarizationError::UnsupportedPayloadVersion => {
                AppErrorCode::MalformedOutput
            }
            DiarizationError::EngineFailure => AppErrorCode::InternalError,
        };
        Self::new(code, error.to_string())
    }
}

/// What a provider is asked to diarize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiarizationInput {
    /// Path of the decoded media on disk.
    pub media_path: std::path::PathBuf,
    /// Media duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Expected number of speakers, when the user supplied a hint.
    pub speaker_count_hint: Option<u8>,
}

/// A speaker diarization provider.
#[async_trait]
pub trait DiarizationProvider: Send + Sync + std::fmt::Debug {
    /// Runtime-probed capabilities.
    async fn capabilities(&self) -> DiarizationCapabilities;

    /// Diarize one media file.
    async fn diarize(
        &self,
        input: DiarizationInput,
        ctx: JobContext,
    ) -> Result<DiarizationResult, DiarizationError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(turns: Vec<SpeakerTurn>) -> DiarizationResult {
        DiarizationResult {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            provider_id: "fake".to_string(),
            provider_version: None,
            turns,
        }
    }

    #[test]
    fn overlap_is_computed_on_half_open_ranges() {
        let turn = SpeakerTurn::new("a", 1_000, 2_000);
        assert_eq!(turn.overlap_ms(1_500, 2_500), 500);
        assert_eq!(turn.overlap_ms(2_000, 3_000), 0);
        assert_eq!(turn.overlap_ms(0, 1_000), 0);
        assert_eq!(turn.duration_ms(), 1_000);
    }

    #[test]
    fn clusters_are_listed_in_first_appearance_order() {
        let result = result(vec![
            SpeakerTurn::new("B", 0, 1_000),
            SpeakerTurn::new("A", 1_000, 2_000),
            SpeakerTurn::new("B", 2_000, 3_000),
        ]);
        assert_eq!(
            result.clusters(),
            vec![ClusterKey::new("B"), ClusterKey::new("A")]
        );
    }

    #[test]
    fn overlapping_speech_is_detected_rather_than_flattened() {
        assert!(result(vec![
            SpeakerTurn::new("A", 0, 2_000),
            SpeakerTurn::new("B", 1_000, 3_000),
        ])
        .has_overlapping_speech());
        assert!(!result(vec![
            SpeakerTurn::new("A", 0, 1_000),
            SpeakerTurn::new("B", 1_000, 2_000),
        ])
        .has_overlapping_speech());
    }

    #[test]
    fn an_inverted_or_unnamed_turn_is_malformed() {
        assert_eq!(
            result(vec![SpeakerTurn::new("A", 2_000, 1_000)]).validate(),
            Err(DiarizationError::MalformedOutput)
        );
        assert_eq!(
            result(vec![SpeakerTurn::new("  ", 0, 1_000)]).validate(),
            Err(DiarizationError::MalformedOutput)
        );
    }

    #[test]
    fn turns_must_arrive_in_chronological_order() {
        assert_eq!(
            result(vec![
                SpeakerTurn::new("A", 5_000, 6_000),
                SpeakerTurn::new("B", 1_000, 2_000),
            ])
            .validate(),
            Err(DiarizationError::MalformedOutput)
        );
    }

    #[test]
    fn a_foreign_payload_version_is_refused() {
        let mut r = result(Vec::new());
        r.payload_version = 99;
        assert_eq!(
            r.validate(),
            Err(DiarizationError::UnsupportedPayloadVersion)
        );
    }

    #[test]
    fn every_error_maps_to_a_distinct_stable_code() {
        let errors = [
            DiarizationError::Cancelled,
            DiarizationError::Unavailable,
            DiarizationError::NotValidated,
            DiarizationError::ConsentRequired,
            DiarizationError::LocalOnlyMode,
            DiarizationError::DurationExceeded,
            DiarizationError::EngineFailure,
        ];
        let codes: std::collections::BTreeSet<_> = errors
            .iter()
            .cloned()
            .map(|e| AppError::from(e).code())
            .collect();
        assert_eq!(codes.len(), errors.len());
    }

    #[test]
    fn an_unknown_status_provider_is_never_selectable() {
        let mut caps = DiarizationCapabilities {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            status: CapabilityStatus::Unknown,
            provider_id: "x".to_string(),
            offline: Tri::Yes,
            supports_overlapping_speech: Tri::Unknown,
            can_cancel: Tri::Yes,
            max_duration_ms: Known::Unknown,
            privacy_class: Known::Known(PrivacyClass::OnDevice),
            added_cost: MoneyEstimate::Unknown,
            model_download_bytes: Known::Unknown,
            hardware_requirements: None,
            availability_reason: AvailabilityReason::Available,
        };
        assert!(!caps.is_selectable());
        caps.status = CapabilityStatus::Supported;
        assert!(caps.is_selectable());
        caps.privacy_class = Known::Known(PrivacyClass::Unresolved);
        assert!(!caps.is_selectable());
    }

    #[test]
    fn an_unresolved_route_is_treated_as_needing_consent() {
        let caps = DiarizationCapabilities {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            status: CapabilityStatus::Supported,
            provider_id: "x".to_string(),
            offline: Tri::Unknown,
            supports_overlapping_speech: Tri::Unknown,
            can_cancel: Tri::Yes,
            max_duration_ms: Known::Unknown,
            privacy_class: Known::Unknown,
            added_cost: MoneyEstimate::Unknown,
            model_download_bytes: Known::Unknown,
            hardware_requirements: None,
            availability_reason: AvailabilityReason::Available,
        };
        assert!(caps.requires_cloud_consent());
    }
}
