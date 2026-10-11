//! A deterministic in-memory [`SpeechToText`] engine.

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::SttCapabilities;
use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_ai_contracts::stt::{
    SpeechToText, SttError, SttInput, TranscriptDraft, TranscriptSegmentDraft,
};
use plaudon_domain::transcript::AlignmentOrigin;

/// An engine that returns a canned draft.
#[derive(Debug)]
pub struct FakeSpeechToText {
    capabilities: SttCapabilities,
    segments: Vec<TranscriptSegmentDraft>,
    alignment_origin: AlignmentOrigin,
    failure: Option<SttError>,
}

impl FakeSpeechToText {
    /// Create an engine that succeeds with `segments`.
    #[must_use]
    pub fn new(
        capabilities: SttCapabilities,
        alignment_origin: AlignmentOrigin,
        segments: Vec<TranscriptSegmentDraft>,
    ) -> Self {
        Self { capabilities, segments, alignment_origin, failure: None }
    }

    /// Create an engine that always fails with `error`.
    #[must_use]
    pub fn failing(capabilities: SttCapabilities, error: SttError) -> Self {
        Self {
            capabilities,
            segments: Vec::new(),
            alignment_origin: AlignmentOrigin::None,
            failure: Some(error),
        }
    }
}

#[async_trait]
impl SpeechToText for FakeSpeechToText {
    async fn capabilities(&self) -> SttCapabilities {
        self.capabilities.clone()
    }

    async fn transcribe(
        &self,
        input: SttInput,
        ctx: JobContext,
    ) -> Result<TranscriptDraft, SttError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        for (index, _) in self.segments.iter().enumerate() {
            if ctx.is_cancelled() {
                return Err(SttError::Cancelled);
            }
            ctx.report(Progress::fraction((index + 1) as f32 / self.segments.len().max(1) as f32));
        }
        if ctx.is_cancelled() {
            return Err(SttError::Cancelled);
        }
        Ok(TranscriptDraft {
            engine_id: self.capabilities.engine_id.clone(),
            model: self.capabilities.model.clone(),
            input_sha256: input.media_sha256,
            locale: input.locale_hint,
            alignment_origin: self.alignment_origin,
            segments: self.segments.clone(),
            duration_ms: input.duration_ms,
        })
    }
}
