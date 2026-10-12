//! The validated-provider allowlist and the gate every run passes through
//! (spec 8.7, 3.1).
//!
//! Spec 8.7: "We will expose the option only for validated providers and show
//! possible added cost." Spec 3.1 marks advanced diarization `✓*`, strictly
//! conditional on measured runtime capability. So the allowlist is a compiled
//! constant with an explicit evidence reference per entry, and invoking a
//! provider outside it fails closed with [`DiarizationError::NotValidated`].
//!
//! The allowlist starts with exactly one entry: the deterministic fake used by
//! tests. Adding `pyannote` or any cloud provider requires the ADR recorded in
//! `docs/adr/ADR-0400-diarization-runtime-and-licensing.md` to approve it
//! first, because both carry a Python runtime, a gated model token or an
//! egress decision.

use plaudon_ai_contracts::job::JobContext;
use plaudon_domain::capability::CapabilityStatus;

use crate::contract::{
    DiarizationCapabilities, DiarizationError, DiarizationInput, DiarizationProvider,
    DiarizationResult,
};

/// One entry in the validated-provider allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedProvider {
    /// Stable provider identifier.
    pub provider_id: &'static str,
    /// Evidence that justifies the entry: an ADR or benchmark document.
    pub evidence_ref: &'static str,
    /// Whether the entry is a test double rather than a shipping provider.
    pub test_only: bool,
}

/// Providers that may be offered to a user.
pub const VALIDATED_PROVIDERS: &[ValidatedProvider] = &[ValidatedProvider {
    provider_id: "fake-deterministic",
    evidence_ref: "docs/adr/ADR-0400-diarization-runtime-and-licensing.md",
    test_only: true,
}];

/// Whether a provider id is in the allowlist.
#[must_use]
pub fn is_validated(provider_id: &str) -> bool {
    VALIDATED_PROVIDERS
        .iter()
        .any(|entry| entry.provider_id == provider_id)
}

/// The allowlist entry for a provider, if any.
#[must_use]
pub fn validated_entry(provider_id: &str) -> Option<&'static ValidatedProvider> {
    VALIDATED_PROVIDERS
        .iter()
        .find(|entry| entry.provider_id == provider_id)
}

/// The policy inputs a run is evaluated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunPolicy {
    /// Whether Local-only Mode is enabled.
    pub local_only_mode: bool,
    /// Whether cloud consent covering this provider has been granted.
    pub cloud_consent_granted: bool,
}

impl RunPolicy {
    /// The posture of a machine with no consent and no Local-only Mode.
    #[must_use]
    pub const fn default_posture() -> Self {
        Self {
            local_only_mode: false,
            cloud_consent_granted: false,
        }
    }
}

/// Decide whether a diarization run may proceed.
///
/// # Errors
///
/// Fails closed: an unvalidated provider, an unprobed capability, a missing
/// consent or Local-only Mode each refuse the run before any work starts.
pub fn authorize(
    capabilities: &DiarizationCapabilities,
    policy: RunPolicy,
    duration_ms: Option<u64>,
) -> Result<(), DiarizationError> {
    if !is_validated(&capabilities.provider_id) {
        return Err(DiarizationError::NotValidated);
    }
    if capabilities.status == CapabilityStatus::Unsupported {
        return Err(DiarizationError::Unavailable);
    }
    if !capabilities.is_selectable() {
        return Err(DiarizationError::NotValidated);
    }
    if capabilities.requires_cloud_consent() {
        if policy.local_only_mode {
            return Err(DiarizationError::LocalOnlyMode);
        }
        if !policy.cloud_consent_granted {
            return Err(DiarizationError::ConsentRequired);
        }
    }
    if let (Some(limit), Some(duration)) = (capabilities.max_duration_ms.get(), duration_ms) {
        if duration > *limit {
            return Err(DiarizationError::DurationExceeded);
        }
    }
    Ok(())
}

/// Run a provider through the gate.
///
/// This is the only function the application calls: a provider is never
/// invoked directly, so the allowlist cannot be bypassed by a caller that
/// forgets to check it.
///
/// # Errors
///
/// Propagates the authorization refusal or the provider's own failure, and
/// rejects output that violates the contract.
pub async fn run_guarded(
    provider: &dyn DiarizationProvider,
    input: DiarizationInput,
    policy: RunPolicy,
    ctx: JobContext,
) -> Result<DiarizationResult, DiarizationError> {
    let capabilities = provider.capabilities().await;
    authorize(&capabilities, policy, input.duration_ms)?;
    if ctx.is_cancelled() {
        return Err(DiarizationError::Cancelled);
    }
    let result = provider.diarize(input, ctx).await?;
    result.validate()?;
    if result.provider_id != capabilities.provider_id {
        return Err(DiarizationError::MalformedOutput);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeDiarizationProvider;
    use plaudon_ai_contracts::capabilities::{AvailabilityReason, PrivacyClass};
    use plaudon_domain::capability::{Known, Tri};
    use plaudon_domain::ids::JobId;
    use plaudon_domain::money::MoneyEstimate;

    fn caps(provider_id: &str) -> DiarizationCapabilities {
        DiarizationCapabilities {
            payload_version: crate::contract::DIARIZATION_CONTRACT_VERSION,
            status: CapabilityStatus::Supported,
            provider_id: provider_id.to_string(),
            offline: Tri::Yes,
            supports_overlapping_speech: Tri::Yes,
            can_cancel: Tri::Yes,
            max_duration_ms: Known::Known(3_600_000),
            privacy_class: Known::Known(PrivacyClass::OnDevice),
            added_cost: MoneyEstimate::Unknown,
            model_download_bytes: Known::Known(0),
            hardware_requirements: None,
            availability_reason: AvailabilityReason::Available,
        }
    }

    fn input() -> DiarizationInput {
        DiarizationInput {
            media_path: std::path::PathBuf::from("/tmp/fixture.wav"),
            duration_ms: Some(60_000),
            speaker_count_hint: None,
        }
    }

    #[test]
    fn an_unlisted_provider_fails_closed() {
        assert_eq!(
            authorize(&caps("pyannote"), RunPolicy::default_posture(), Some(1_000)),
            Err(DiarizationError::NotValidated)
        );
    }

    #[test]
    fn the_allowlist_only_contains_entries_with_evidence() {
        for entry in VALIDATED_PROVIDERS {
            assert!(entry.evidence_ref.starts_with("docs/"), "{entry:?}");
        }
    }

    #[test]
    fn a_validated_on_device_provider_runs_without_consent() {
        assert!(authorize(
            &caps("fake-deterministic"),
            RunPolicy::default_posture(),
            Some(1_000)
        )
        .is_ok());
    }

    #[test]
    fn a_cloud_provider_needs_consent_and_is_refused_in_local_only_mode() {
        let mut capabilities = caps("fake-deterministic");
        capabilities.privacy_class = Known::Known(PrivacyClass::CloudRetaining);
        assert_eq!(
            authorize(&capabilities, RunPolicy::default_posture(), Some(1_000)),
            Err(DiarizationError::ConsentRequired)
        );
        assert_eq!(
            authorize(
                &capabilities,
                RunPolicy {
                    local_only_mode: true,
                    cloud_consent_granted: true
                },
                Some(1_000)
            ),
            Err(DiarizationError::LocalOnlyMode)
        );
        assert!(authorize(
            &capabilities,
            RunPolicy {
                local_only_mode: false,
                cloud_consent_granted: true
            },
            Some(1_000)
        )
        .is_ok());
    }

    #[test]
    fn an_unprobed_provider_is_refused() {
        let mut capabilities = caps("fake-deterministic");
        capabilities.status = CapabilityStatus::Unknown;
        assert_eq!(
            authorize(&capabilities, RunPolicy::default_posture(), Some(1_000)),
            Err(DiarizationError::NotValidated)
        );
    }

    #[test]
    fn media_longer_than_the_provider_accepts_is_refused_before_dispatch() {
        assert_eq!(
            authorize(
                &caps("fake-deterministic"),
                RunPolicy::default_posture(),
                Some(7_200_000)
            ),
            Err(DiarizationError::DurationExceeded)
        );
    }

    #[tokio::test]
    async fn the_guard_rejects_output_from_a_different_provider_than_it_probed() {
        let provider = FakeDiarizationProvider::new(vec![]).with_result_provider_id("someone-else");
        let err = run_guarded(
            &provider,
            input(),
            RunPolicy::default_posture(),
            JobContext::new(JobId::new()),
        )
        .await
        .unwrap_err();
        assert_eq!(err, DiarizationError::MalformedOutput);
    }

    #[tokio::test]
    async fn the_guard_refuses_an_unvalidated_provider_without_running_it() {
        let provider = FakeDiarizationProvider::new(vec![]).with_provider_id("not-in-allowlist");
        let err = run_guarded(
            &provider,
            input(),
            RunPolicy::default_posture(),
            JobContext::new(JobId::new()),
        )
        .await
        .unwrap_err();
        assert_eq!(err, DiarizationError::NotValidated);
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn a_cancelled_context_stops_before_the_provider_runs() {
        let provider = FakeDiarizationProvider::new(vec![]);
        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();
        let err = run_guarded(&provider, input(), RunPolicy::default_posture(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err, DiarizationError::Cancelled);
        assert_eq!(provider.call_count(), 0);
    }
}
