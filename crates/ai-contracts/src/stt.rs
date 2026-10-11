//! The `SpeechToText` capability contract (§6.5) and its error taxonomy.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::capabilities::{MediaFormat, SttCapabilities};
use crate::job::JobContext;
use crate::transcript::{ChunkingStrategy, NormalizationProfile, TranscriptDraft};

/// Audio handed to an engine, together with everything the engine needs to
/// place its output on the source timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttInput {
    /// Path to the bytes the engine should read. For local engines this is the
    /// canonical PCM derivative produced by F1.2.
    pub audio_path: PathBuf,
    pub format: MediaFormat,
    pub sample_rate_hz: u32,
    pub channels: u16,
    /// Total frames in this input (per channel).
    pub frames: u64,
    /// Absolute offset of this input within the source media, in source
    /// samples. Non-zero for chunk `n > 0` (§8.2).
    pub source_sample_offset: u64,
    /// Sample rate of the *source* media, which `source_sample_offset` is
    /// expressed in.
    pub source_sample_rate_hz: u32,
    /// SHA-256 (hex) of the original source bytes, used for revision keying.
    pub input_sha256: String,
    /// BCP-47 locale hint. `None` asks the engine to detect, which it may only
    /// do when it advertises `supports_language_detection`.
    pub locale: Option<String>,
    pub normalization_profile: Option<NormalizationProfile>,
    pub chunking_strategy: ChunkingStrategy,
}

impl SttInput {
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate_hz == 0 {
            0
        } else {
            (self.frames * 1000) / u64::from(self.sample_rate_hz)
        }
    }
}

/// Stable STT error taxonomy. No third-party error type is ever exposed
/// through this boundary (§6.6).
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "code", content = "detail")]
pub enum SttError {
    /// The engine is not usable; mirrors `availability_reason`.
    #[error("engine unavailable: {0}")]
    Unavailable(String),

    /// Required model weights are not installed. Never triggers a download and
    /// never falls back to another engine (§8.6A).
    #[error("model not installed: {0}")]
    ModelNotInstalled(String),

    /// The input violates a declared capability limit.
    #[error("input rejected: {0}")]
    InputRejected(String),

    /// Hardware could not satisfy the workload.
    #[error("insufficient resources: {0}")]
    InsufficientResources(String),

    /// The engine ran but produced an unusable or contract-violating result.
    #[error("engine failure: {0}")]
    EngineFailure(String),

    /// Cooperative cancellation completed. No partial result is final.
    #[error("cancelled")]
    Cancelled,

    /// Dispatch refused before any bytes left the machine.
    #[error("blocked by policy: {0}")]
    BlockedByPolicy(String),

    /// A cloud request was accepted but its outcome is unknown. Never
    /// auto-retried (§14.3, §8.6A).
    #[error("outcome unknown: {0}")]
    OutcomeUnknown(String),

    /// A result failed the truthfulness contract (§8.6A).
    #[error("truthfulness violation: {0}")]
    Truthfulness(String),

    #[error("io error: {0}")]
    Io(String),
}

impl SttError {
    /// Stable machine-readable code for the frontend error envelope.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            SttError::Unavailable(_) => "STT_UNAVAILABLE",
            SttError::ModelNotInstalled(_) => "STT_MODEL_NOT_INSTALLED",
            SttError::InputRejected(_) => "STT_INPUT_REJECTED",
            SttError::InsufficientResources(_) => "STT_INSUFFICIENT_RESOURCES",
            SttError::EngineFailure(_) => "STT_ENGINE_FAILURE",
            SttError::Cancelled => "STT_CANCELLED",
            SttError::BlockedByPolicy(_) => "STT_BLOCKED_BY_POLICY",
            SttError::OutcomeUnknown(_) => "STT_OUTCOME_UNKNOWN",
            SttError::Truthfulness(_) => "STT_TRUTHFULNESS_VIOLATION",
            SttError::Io(_) => "STT_IO",
        }
    }

    /// Whether the *same* request may be safely reissued.
    ///
    /// `OutcomeUnknown` is deliberately not retryable: the work may already be
    /// billed (§14.3).
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, SttError::Io(_) | SttError::InsufficientResources(_))
    }
}

impl From<crate::job::Cancelled> for SttError {
    fn from(_: crate::job::Cancelled) -> Self {
        SttError::Cancelled
    }
}

/// Speech-to-text engine contract (§6.5).
#[async_trait::async_trait]
pub trait SpeechToText: Send + Sync {
    /// Honest, runtime-probed capability payload (§8.3).
    async fn capabilities(&self) -> SttCapabilities;

    /// Transcribe one input. Implementations must poll `ctx.cancel`, report
    /// progress through `ctx`, and never fall back to another engine.
    async fn transcribe(
        &self,
        input: SttInput,
        ctx: JobContext,
    ) -> Result<TranscriptDraft, SttError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_unknown_is_never_retryable() {
        assert!(!SttError::OutcomeUnknown("dispatched".into()).retryable());
        assert!(!SttError::Cancelled.retryable());
        assert!(!SttError::ModelNotInstalled("tiny".into()).retryable());
        assert!(SttError::Io("disk".into()).retryable());
    }

    #[test]
    fn error_codes_are_stable_and_distinct() {
        let codes = [
            SttError::Unavailable(String::new()).code(),
            SttError::ModelNotInstalled(String::new()).code(),
            SttError::InputRejected(String::new()).code(),
            SttError::InsufficientResources(String::new()).code(),
            SttError::EngineFailure(String::new()).code(),
            SttError::Cancelled.code(),
            SttError::BlockedByPolicy(String::new()).code(),
            SttError::OutcomeUnknown(String::new()).code(),
            SttError::Truthfulness(String::new()).code(),
            SttError::Io(String::new()).code(),
        ];
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
        assert!(codes.iter().all(|c| c.starts_with("STT_")));
    }

    #[test]
    fn stt_input_duration_uses_frames_and_rate() {
        let input = SttInput {
            audio_path: PathBuf::from("/tmp/x.pcm"),
            format: MediaFormat::PcmS16Le,
            sample_rate_hz: 16_000,
            channels: 1,
            frames: 32_000,
            source_sample_offset: 0,
            source_sample_rate_hz: 16_000,
            input_sha256: "0".repeat(64),
            locale: None,
            normalization_profile: None,
            chunking_strategy: ChunkingStrategy::SingleChunk,
        };
        assert_eq!(input.duration_ms(), 2000);
    }
}
