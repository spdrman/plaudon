//! The speech-to-text contract (spec 6.5, 8.5, 8.6A).

use std::path::PathBuf;

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::SpeakerId;
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::timerange::SourceTimeMs;
use plaudon_domain::transcript::{AlignmentOrigin, Confidence, WordAlignment};
use serde::{Deserialize, Serialize};

use crate::capabilities::{MediaFormat, SttCapabilities};
use crate::job::JobContext;

/// Audio handed to an engine.
///
/// The engine receives a path plus the already-computed identity of the bytes,
/// so it can record provenance without re-reading the file and cannot silently
/// transcribe different bytes than the caller intended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttInput {
    /// Path to the media file to transcribe.
    pub media_path: PathBuf,
    /// Digest of the exact bytes at `media_path`.
    pub media_sha256: Sha256Digest,
    /// Byte length of the media.
    pub byte_length: u64,
    /// Format of the media as determined by content sniffing.
    pub format: MediaFormat,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Locale hint; `None` asks the engine to detect the language.
    pub locale_hint: Option<LanguageTag>,
}

/// A segment as produced by an engine, before revision validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegmentDraft {
    /// Zero-based position in the draft.
    pub seq_index: u32,
    /// Start offset; absent when the engine reported no timing.
    pub start: Option<SourceTimeMs>,
    /// End offset; absent when the engine reported no timing.
    pub end: Option<SourceTimeMs>,
    /// Segment text, verbatim from the engine.
    pub text: String,
    /// Engine-scoped confidence, when reported.
    pub confidence: Option<Confidence>,
    /// Speaker attribution, when reported.
    pub speaker_id: Option<SpeakerId>,
    /// Word alignments, when reported.
    #[serde(default)]
    pub words: Vec<WordAlignment>,
}

/// An engine's raw result, before it becomes an immutable revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptDraft {
    /// Engine that produced the draft.
    pub engine_id: EngineId,
    /// Model that produced the draft.
    pub model: ModelIdentity,
    /// Digest of the transcribed bytes.
    pub input_sha256: Sha256Digest,
    /// Detected or configured locale.
    pub locale: Option<LanguageTag>,
    /// Where the segment offsets came from.
    pub alignment_origin: AlignmentOrigin,
    /// Ordered segments.
    pub segments: Vec<TranscriptSegmentDraft>,
    /// Media duration as observed by the engine, when reported.
    pub duration_ms: Option<u64>,
}

/// Failures an engine may report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SttError {
    /// The run was cancelled. No partial transcript is produced.
    #[error("transcription was cancelled")]
    Cancelled,
    /// The engine or its model assets are unavailable.
    #[error("the transcription engine is unavailable")]
    Unavailable,
    /// The input exceeds a declared engine limit.
    #[error("the input exceeds an engine limit")]
    InputTooLarge,
    /// The input codec or container is not accepted.
    #[error("the engine does not accept this media format")]
    UnsupportedFormat,
    /// Consent has not been granted for this engine's privacy class.
    #[error("consent is required before this engine may run")]
    ConsentRequired,
    /// The route's privacy policy could not be resolved.
    #[error("the privacy route for this engine could not be resolved")]
    PolicyUnresolved,
    /// The provider refused the request.
    #[error("the provider refused the request")]
    ProviderRejected,
    /// Bytes were transmitted but no result arrived.
    #[error("the request was transmitted but the outcome is unknown")]
    OutcomeUnknown,
    /// The engine returned something we cannot interpret.
    #[error("the engine returned output we could not interpret")]
    MalformedOutput,
    /// The engine failed in a way this build does not model.
    #[error("the transcription engine failed")]
    EngineFailure,
}

impl From<SttError> for AppError {
    fn from(error: SttError) -> Self {
        let code = match error {
            SttError::Cancelled => AppErrorCode::Canceled,
            SttError::Unavailable => AppErrorCode::ModelUnavailable,
            SttError::InputTooLarge => AppErrorCode::ContextTooLarge,
            SttError::UnsupportedFormat => AppErrorCode::UnsupportedCodec,
            SttError::ConsentRequired => AppErrorCode::ConsentRequired,
            SttError::PolicyUnresolved => AppErrorCode::PolicyUnresolved,
            SttError::ProviderRejected => AppErrorCode::ProviderRejected,
            SttError::OutcomeUnknown => AppErrorCode::OutcomeUnknown,
            SttError::MalformedOutput => AppErrorCode::MalformedOutput,
            SttError::EngineFailure => AppErrorCode::InternalError,
        };
        Self::new(code, error.to_string())
    }
}

/// A speech-to-text engine (spec 6.5).
#[async_trait]
pub trait SpeechToText: Send + Sync {
    /// Runtime-probed capabilities. Implementations must probe, not infer.
    async fn capabilities(&self) -> SttCapabilities;

    /// Transcribe `input`.
    ///
    /// A cancelled run must return [`SttError::Cancelled`] and must not return
    /// a partial [`TranscriptDraft`].
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
    fn cancellation_maps_to_the_cancel_error_code() {
        assert_eq!(
            AppError::from(SttError::Cancelled).code(),
            AppErrorCode::Canceled
        );
    }

    #[test]
    fn an_unknown_outcome_maps_to_a_non_retryable_code() {
        let error = AppError::from(SttError::OutcomeUnknown);
        assert_eq!(error.code(), AppErrorCode::OutcomeUnknown);
        assert!(
            !error.is_retryable(),
            "an unknown outcome must never auto-retry"
        );
    }

    #[test]
    fn every_stt_error_maps_to_a_distinct_stable_code() {
        let errors = [
            SttError::Cancelled,
            SttError::Unavailable,
            SttError::InputTooLarge,
            SttError::UnsupportedFormat,
            SttError::ConsentRequired,
            SttError::PolicyUnresolved,
            SttError::ProviderRejected,
            SttError::OutcomeUnknown,
            SttError::MalformedOutput,
            SttError::EngineFailure,
        ];
        let codes: std::collections::BTreeSet<_> = errors
            .iter()
            .cloned()
            .map(|e| AppError::from(e).code())
            .collect();
        assert_eq!(codes.len(), errors.len());
    }
}
