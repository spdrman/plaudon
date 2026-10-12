//! A deterministic diarization provider for contract tests (spec 18.3).
//!
//! It is not a mock in the "returns whatever the test wants" sense: it
//! implements the whole contract, validates its own input against the declared
//! capabilities, honours cancellation and reports a stable capability payload.
//! A test that passes against this provider has exercised the real gate.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{AvailabilityReason, PrivacyClass};
use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::money::MoneyEstimate;

use crate::contract::{
    DiarizationCapabilities, DiarizationError, DiarizationInput, DiarizationProvider,
    DiarizationResult, SpeakerTurn, DIARIZATION_CONTRACT_VERSION,
};

/// Provider id of the deterministic fake, matching the allowlist entry.
pub const FAKE_PROVIDER_ID: &str = "fake-deterministic";

/// A provider that replays a fixed turn list.
#[derive(Debug)]
pub struct FakeDiarizationProvider {
    turns: Vec<SpeakerTurn>,
    provider_id: String,
    result_provider_id: Option<String>,
    status: CapabilityStatus,
    privacy_class: PrivacyClass,
    max_duration_ms: Option<u64>,
    failure: Option<DiarizationError>,
    calls: AtomicUsize,
}

impl FakeDiarizationProvider {
    /// A provider that returns `turns`.
    #[must_use]
    pub fn new(turns: Vec<SpeakerTurn>) -> Self {
        Self {
            turns,
            provider_id: FAKE_PROVIDER_ID.to_string(),
            result_provider_id: None,
            status: CapabilityStatus::Supported,
            privacy_class: PrivacyClass::OnDevice,
            max_duration_ms: Some(3_600_000),
            failure: None,
            calls: AtomicUsize::new(0),
        }
    }

    /// Override the advertised provider id, to test the allowlist.
    #[must_use]
    pub fn with_provider_id(mut self, id: &str) -> Self {
        self.provider_id = id.to_string();
        self
    }

    /// Return a result that claims a different provider than was probed.
    #[must_use]
    pub fn with_result_provider_id(mut self, id: &str) -> Self {
        self.result_provider_id = Some(id.to_string());
        self
    }

    /// Override the advertised capability status.
    #[must_use]
    pub const fn with_status(mut self, status: CapabilityStatus) -> Self {
        self.status = status;
        self
    }

    /// Override the advertised privacy class.
    #[must_use]
    pub const fn with_privacy_class(mut self, class: PrivacyClass) -> Self {
        self.privacy_class = class;
        self
    }

    /// Fail with a specific error instead of returning turns.
    #[must_use]
    pub fn failing(mut self, error: DiarizationError) -> Self {
        self.failure = Some(error);
        self
    }

    /// How many times `diarize` actually ran.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DiarizationProvider for FakeDiarizationProvider {
    async fn capabilities(&self) -> DiarizationCapabilities {
        DiarizationCapabilities {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            status: self.status,
            provider_id: self.provider_id.clone(),
            offline: Tri::Yes,
            supports_overlapping_speech: Tri::Yes,
            can_cancel: Tri::Yes,
            max_duration_ms: self.max_duration_ms.map_or(Known::Unknown, Known::Known),
            privacy_class: Known::Known(self.privacy_class),
            added_cost: MoneyEstimate::Unknown,
            model_download_bytes: Known::Known(0),
            hardware_requirements: None,
            availability_reason: AvailabilityReason::Available,
        }
    }

    async fn diarize(
        &self,
        input: DiarizationInput,
        ctx: JobContext,
    ) -> Result<DiarizationResult, DiarizationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.failure.clone() {
            return Err(error);
        }
        if let (Some(limit), Some(duration)) = (self.max_duration_ms, input.duration_ms) {
            if duration > limit {
                return Err(DiarizationError::DurationExceeded);
            }
        }
        for (index, _) in self.turns.iter().enumerate() {
            if ctx.is_cancelled() {
                return Err(DiarizationError::Cancelled);
            }
            let total = self.turns.len().max(1) as f32;
            ctx.report(Progress::fraction((index + 1) as f32 / total));
        }
        Ok(DiarizationResult {
            payload_version: DIARIZATION_CONTRACT_VERSION,
            provider_id: self
                .result_provider_id
                .clone()
                .unwrap_or_else(|| self.provider_id.clone()),
            provider_version: Some("fixture-1".to_string()),
            turns: self.turns.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_ai_contracts::job::RecordingProgressSink;
    use plaudon_domain::ids::JobId;
    use std::sync::Arc;

    fn input(duration_ms: Option<u64>) -> DiarizationInput {
        DiarizationInput {
            media_path: std::path::PathBuf::from("/tmp/fixture.wav"),
            duration_ms,
            speaker_count_hint: None,
        }
    }

    #[tokio::test]
    async fn the_fake_replays_its_turns_and_validates() {
        let provider = FakeDiarizationProvider::new(vec![
            SpeakerTurn::new("A", 0, 1_000),
            SpeakerTurn::new("B", 1_000, 2_000),
        ]);
        let result = provider
            .diarize(input(Some(2_000)), JobContext::new(JobId::new()))
            .await
            .unwrap();
        assert!(result.validate().is_ok());
        assert_eq!(result.turns.len(), 2);
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn the_fake_reports_progress() {
        let sink = Arc::new(RecordingProgressSink::new());
        let provider = FakeDiarizationProvider::new(vec![SpeakerTurn::new("A", 0, 1_000)]);
        provider
            .diarize(
                input(Some(1_000)),
                JobContext::new(JobId::new()).with_progress(sink.clone()),
            )
            .await
            .unwrap();
        assert_eq!(sink.updates().len(), 1);
    }

    #[tokio::test]
    async fn the_fake_honours_cancellation() {
        let provider = FakeDiarizationProvider::new(vec![SpeakerTurn::new("A", 0, 1_000)]);
        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();
        assert_eq!(
            provider.diarize(input(Some(1_000)), ctx).await,
            Err(DiarizationError::Cancelled)
        );
    }

    #[tokio::test]
    async fn the_fake_enforces_its_own_duration_limit() {
        let provider = FakeDiarizationProvider::new(Vec::new());
        assert_eq!(
            provider
                .diarize(input(Some(99_999_999)), JobContext::new(JobId::new()))
                .await,
            Err(DiarizationError::DurationExceeded)
        );
    }

    #[tokio::test]
    async fn the_fake_can_simulate_an_engine_failure() {
        let provider =
            FakeDiarizationProvider::new(Vec::new()).failing(DiarizationError::EngineFailure);
        assert_eq!(
            provider
                .diarize(input(Some(1_000)), JobContext::new(JobId::new()))
                .await,
            Err(DiarizationError::EngineFailure)
        );
    }

    #[tokio::test]
    async fn the_fake_advertises_the_allowlisted_identity_by_default() {
        let provider = FakeDiarizationProvider::new(Vec::new());
        assert_eq!(provider.capabilities().await.provider_id, FAKE_PROVIDER_ID);
        assert!(crate::provider::is_validated(FAKE_PROVIDER_ID));
    }
}
