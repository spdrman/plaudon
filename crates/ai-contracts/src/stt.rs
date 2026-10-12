//! The speech-to-text contract (spec 6.5, 8.5, 8.6A).

use std::path::PathBuf;

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::SpeakerId;
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::transcript::{AlignmentOrigin, Confidence};
use serde::{Deserialize, Serialize};

use crate::capabilities::{MediaFormat, SttCapabilities};
use crate::job::JobContext;
use crate::transcript::{
    ChunkingStrategy, DraftWordAlignment, NormalizationProfile, SourceTime, TimePrecision,
    TranscriptAnnotation,
};

/// Audio handed to an engine.
///
/// The engine receives a path plus the already-computed identity of the bytes,
/// so it can record provenance without re-reading the file and cannot silently
/// transcribe different bytes than the caller intended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttInput {
    /// Path to the media file to transcribe.
    ///
    /// For local engines this is the canonical PCM derivative produced by the
    /// audio pipeline, not the original container.
    pub media_path: PathBuf,
    /// Digest of the *original source* bytes, used for revision keying (§8.5).
    pub media_sha256: Sha256Digest,
    /// Byte length of the file at `media_path`.
    pub byte_length: u64,
    /// Format of the media as determined by content sniffing.
    pub format: MediaFormat,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Locale hint; `None` asks the engine to detect the language, which it
    /// may only do when it declares `supports_language_detection`.
    pub locale_hint: Option<LanguageTag>,
    /// Sample rate of the audio at `media_path`.
    pub sample_rate_hz: u32,
    /// Channel count of the audio at `media_path`.
    pub channels: u16,
    /// Absolute offset of this input within the source media, in source
    /// samples. Non-zero for chunk `n > 0` (§8.2); the engine adds it to every
    /// time it reports so the draft lands on the source timeline.
    pub source_sample_offset: u64,
    /// Canonicalisation that produced `media_path`, when one was applied.
    pub normalization_profile: Option<NormalizationProfile>,
    /// How the source was split, recorded on the resulting revision (§8.5).
    pub chunking_strategy: ChunkingStrategy,
}

impl SttInput {
    /// A single-chunk input over an already-canonical derivative.
    #[must_use]
    pub fn whole_file(
        media_path: PathBuf,
        media_sha256: Sha256Digest,
        byte_length: u64,
        format: MediaFormat,
        sample_rate_hz: u32,
        channels: u16,
    ) -> Self {
        Self {
            media_path,
            media_sha256,
            byte_length,
            format,
            duration_ms: None,
            locale_hint: None,
            sample_rate_hz,
            channels,
            source_sample_offset: 0,
            normalization_profile: None,
            chunking_strategy: ChunkingStrategy::SingleChunk,
        }
    }
}

/// A segment as produced by an engine, before revision validation.
///
/// Times are [`SourceTime`] (sample offsets), never milliseconds: see
/// [`crate::transcript`] for why. A segment with no measured time has `None`,
/// never a zero sentinel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegmentDraft {
    /// Zero-based position in the draft.
    pub seq_index: u32,
    /// Start offset; absent when the engine reported no timing.
    pub start: Option<SourceTime>,
    /// End offset; absent when the engine reported no timing.
    pub end: Option<SourceTime>,
    /// Segment text, verbatim from the engine. Correction passes never edit
    /// this in place; they create a new revision (§8.5).
    pub text: String,
    /// Engine-scoped confidence, when reported.
    pub confidence: Option<Confidence>,
    /// Speaker attribution, when reported.
    pub speaker_id: Option<SpeakerId>,
    /// Word alignments, when reported.
    #[serde(default)]
    pub words: Vec<DraftWordAlignment>,
}

impl TranscriptSegmentDraft {
    /// A text-only segment: the only shape valid under
    /// [`AlignmentOrigin::None`].
    #[must_use]
    pub fn text_only(seq_index: u32, text: impl Into<String>) -> Self {
        Self {
            seq_index,
            start: None,
            end: None,
            text: text.into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
        }
    }

    /// A segment with engine-reported times.
    #[must_use]
    pub fn timed(
        seq_index: u32,
        start: SourceTime,
        end: SourceTime,
        text: impl Into<String>,
    ) -> Self {
        Self {
            seq_index,
            start: Some(start),
            end: Some(end),
            text: text.into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
        }
    }

    /// Attach an engine-scoped confidence value.
    #[must_use]
    pub fn with_confidence(mut self, confidence: Confidence) -> Self {
        self.confidence = Some(confidence);
        self
    }

    /// Whether this segment claims any time at all.
    #[must_use]
    pub fn has_times(&self) -> bool {
        self.start.is_some() || self.end.is_some() || !self.words.is_empty()
    }

    /// Strongest precision claimed anywhere in this segment.
    #[must_use]
    pub fn claimed_precision(&self) -> Option<TimePrecision> {
        self.start
            .iter()
            .chain(self.end.iter())
            .map(|time| time.precision)
            .chain(
                self.words
                    .iter()
                    .flat_map(|word| [word.start.precision, word.end.precision]),
            )
            .max()
    }

    /// Shift every time in this segment onto the absolute source timeline.
    #[must_use]
    pub fn shifted_by_samples(mut self, samples: u64) -> Self {
        self.start = self.start.map(|time| time.shifted_by_samples(samples));
        self.end = self.end.map(|time| time.shifted_by_samples(samples));
        for word in &mut self.words {
            word.start = word.start.shifted_by_samples(samples);
            word.end = word.end.shifted_by_samples(samples);
        }
        self
    }

    /// Convert to the persisted domain segment (§8.5).
    ///
    /// This is where sample offsets become milliseconds. It happens once, at
    /// the persistence boundary, so every computation upstream still has the
    /// full-precision value.
    #[must_use]
    pub fn to_domain_segment(&self) -> plaudon_domain::transcript::TranscriptSegment {
        plaudon_domain::transcript::TranscriptSegment {
            seq_index: self.seq_index,
            start: self.start.map(|time| time.as_source_time_ms()),
            end: self.end.map(|time| time.as_source_time_ms()),
            text: self.text.clone(),
            confidence: self.confidence.clone(),
            speaker_id: self.speaker_id,
            words: self
                .words
                .iter()
                .map(|word| plaudon_domain::transcript::WordAlignment {
                    text: word.text.clone(),
                    start: word.start.as_source_time_ms(),
                    end: word.end.as_source_time_ms(),
                })
                .collect(),
        }
    }
}

/// An engine's raw result, before it becomes an immutable revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptDraft {
    /// Engine that produced the draft.
    pub engine_id: EngineId,
    /// Model that produced the draft.
    pub model: ModelIdentity,
    /// Digest of the transcribed source bytes.
    pub input_sha256: Sha256Digest,
    /// Detected or configured locale.
    pub locale: Option<LanguageTag>,
    /// Where the segment offsets came from.
    pub alignment_origin: AlignmentOrigin,
    /// Ordered segments.
    pub segments: Vec<TranscriptSegmentDraft>,
    /// Media duration as observed by the engine, when reported.
    pub duration_ms: Option<u64>,
    /// Engine parameters that affect output, recorded verbatim so a rerun can
    /// be reproduced exactly (§8.5, F2.1). Ordered for stable serialisation.
    #[serde(default)]
    pub engine_params: std::collections::BTreeMap<String, String>,
    /// How the source was split (§8.5 "chunking strategy").
    #[serde(default)]
    pub chunking_strategy: ChunkingStrategy,
    /// Canonicalisation applied before transcription (§8.5).
    #[serde(default)]
    pub normalization_profile: Option<NormalizationProfile>,
    /// Non-destructive quality flags. Never used to remove text (§8.6A).
    #[serde(default)]
    pub annotations: Vec<TranscriptAnnotation>,
    /// Compute backend actually used by a local engine, e.g. `metal`/`cpu`
    /// (F2.5 requires this in revision metadata).
    #[serde(default)]
    pub backend: Option<String>,
}

impl TranscriptDraft {
    /// A draft with no timeline at all. Valid, and explicitly seek-free.
    #[must_use]
    pub fn text_only(
        engine_id: EngineId,
        model: ModelIdentity,
        input_sha256: Sha256Digest,
        segments: Vec<TranscriptSegmentDraft>,
    ) -> Self {
        Self {
            engine_id,
            model,
            input_sha256,
            locale: None,
            alignment_origin: AlignmentOrigin::None,
            segments,
            duration_ms: None,
            engine_params: std::collections::BTreeMap::new(),
            chunking_strategy: ChunkingStrategy::SingleChunk,
            normalization_profile: None,
            annotations: Vec::new(),
            backend: None,
        }
    }

    /// A draft with engine-native timestamps.
    #[must_use]
    pub fn native(
        engine_id: EngineId,
        model: ModelIdentity,
        input_sha256: Sha256Digest,
        segments: Vec<TranscriptSegmentDraft>,
    ) -> Self {
        Self {
            alignment_origin: AlignmentOrigin::Native,
            ..Self::text_only(engine_id, model, input_sha256, segments)
        }
    }

    /// Concatenated segment text, used for diagnostics and repeatability
    /// fixtures.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Source span covered by timed segments, in samples.
    #[must_use]
    pub fn timed_span_samples(&self) -> Option<(u64, u64)> {
        let start = self
            .segments
            .iter()
            .filter_map(|segment| segment.start)
            .map(|time| time.sample_offset)
            .min()?;
        let end = self
            .segments
            .iter()
            .filter_map(|segment| segment.end)
            .map(|time| time.sample_offset)
            .max()?;
        Some((start, end))
    }
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
    /// Model weights are not installed on this machine.
    ///
    /// Distinct from [`SttError::Unavailable`] because it is actionable: the
    /// user can install the model. It never triggers a download by itself and
    /// never falls back to another engine (§8.6A, §12).
    #[error("the model weights for this engine are not installed")]
    ModelNotInstalled,
    /// Preflight refused the workload, or the run exhausted memory.
    ///
    /// Fail-closed rather than OOM (§14.4).
    #[error("this machine cannot satisfy the engine's resource requirements")]
    InsufficientResources,
    /// A result violated the timestamp-truthfulness contract (§8.6A).
    #[error("the engine returned timestamps it is not entitled to claim")]
    TruthfulnessViolation,
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
            SttError::ModelNotInstalled
            | SttError::InsufficientResources
            | SttError::Unavailable => AppErrorCode::ModelUnavailable,
            SttError::TruthfulnessViolation => AppErrorCode::MalformedOutput,
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
