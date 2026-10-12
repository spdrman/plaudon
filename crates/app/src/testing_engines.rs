//! Scripted AI engines for hermetic tests.
//!
//! The real engines are an Apple framework, a linked whisper.cpp and an
//! HTTPS client; none of them belongs in a test that is about the
//! application. These two stand in at exactly the contract boundary
//! (`plaudon_ai_contracts`), so everything above them — job states, resource
//! arbitration, cancellation, progress, the commit and the events — is the
//! production code path.
//!
//! Compiled into the library only for `cargo test` and for a dependent
//! crate that asks for the `test-support` feature.

use std::sync::Arc;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, MediaFormat, PricingBasis, PrivacyClass, RequestFamily, SttCapabilities,
    TextModelCapabilities,
};
use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_ai_contracts::stt::{
    SpeechToText, SttError, SttInput, TranscriptDraft, TranscriptSegmentDraft,
};
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::transcript::AlignmentOrigin;

/// Engine id the scripted speech engine reports.
pub const SCRIPTED_STT_ENGINE: &str = "scripted-stt";

/// Engine id the scripted text model reports.
pub const SCRIPTED_TEXT_ENGINE: &str = "scripted-text";

/// A speech engine that returns fixed segments.
///
/// It reports progress the way a real one does (a fraction per segment) and
/// honours cancellation between segments, so a test can assert both.
#[derive(Debug)]
pub struct ScriptedStt {
    model_id: String,
    segments: Vec<String>,
    available: bool,
    /// Milliseconds of "work" per segment, so a test can cancel mid-run.
    pace_ms: u64,
}

impl ScriptedStt {
    /// An engine that transcribes to `segments`.
    #[must_use]
    pub fn new(segments: &[&str]) -> Self {
        Self {
            model_id: "scripted-small".to_owned(),
            segments: segments.iter().map(|text| (*text).to_owned()).collect(),
            available: true,
            pace_ms: 0,
        }
    }

    /// An engine whose probe reports it unavailable.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            available: false,
            ..Self::new(&[])
        }
    }

    /// Spend `pace_ms` per segment, so a cancellation can land mid-run.
    #[must_use]
    pub const fn with_pace_ms(mut self, pace_ms: u64) -> Self {
        self.pace_ms = pace_ms;
        self
    }

    /// Report a different model id.
    #[must_use]
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = model_id.into();
        self
    }

    /// The engine as a shared contract object.
    #[must_use]
    pub fn shared(self) -> Arc<dyn SpeechToText> {
        Arc::new(self) as Arc<dyn SpeechToText>
    }
}

#[async_trait]
impl SpeechToText for ScriptedStt {
    async fn capabilities(&self) -> SttCapabilities {
        let engine_id = EngineId::new(SCRIPTED_STT_ENGINE);
        SttCapabilities {
            payload_version: 1,
            status: if self.available {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Unknown
            },
            engine_id: engine_id.clone(),
            model: ModelIdentity::new(engine_id, self.model_id.clone()),
            request_family: RequestFamily::Unsupported,
            offline: Tri::Yes,
            locales: Known::Unknown,
            supported_formats: Known::Known(vec![MediaFormat::new("wav", "pcm_s16le")]),
            max_file_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            supports_segments: Tri::Yes,
            supports_word_timestamps: Tri::No,
            supports_diarization: Tri::No,
            supports_language_detection: Tri::No,
            can_cancel: Tri::Yes,
            privacy_class: Known::Known(PrivacyClass::OnDevice),
            pricing_basis: Known::Known(PricingBasis::None),
            model_download_bytes: Known::Known(0),
            hardware_requirements: Known::Unknown,
            availability_reason: if self.available {
                AvailabilityReason::Available
            } else {
                AvailabilityReason::ModelAssetMissing {
                    asset: "scripted".to_owned(),
                }
            },
        }
    }

    async fn transcribe(
        &self,
        input: SttInput,
        ctx: JobContext,
    ) -> Result<TranscriptDraft, SttError> {
        if !self.available {
            return Err(SttError::ModelNotInstalled);
        }
        let mut segments = Vec::with_capacity(self.segments.len());
        let total = self.segments.len().max(1);
        for (index, text) in self.segments.iter().enumerate() {
            if ctx.is_cancelled() {
                return Err(SttError::Cancelled);
            }
            if self.pace_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.pace_ms)).await;
            }
            #[allow(clippy::cast_precision_loss)]
            ctx.report(Progress::fraction((index + 1) as f32 / total as f32));
            segments.push(TranscriptSegmentDraft::text_only(
                u32::try_from(index).unwrap_or(0),
                text.clone(),
            ));
        }
        if ctx.is_cancelled() {
            return Err(SttError::Cancelled);
        }
        let engine_id = EngineId::new(SCRIPTED_STT_ENGINE);
        let mut draft = TranscriptDraft::text_only(
            engine_id.clone(),
            ModelIdentity::new(engine_id, self.model_id.clone()),
            input.media_sha256,
            segments,
        );
        draft.alignment_origin = AlignmentOrigin::None;
        draft.duration_ms = input.duration_ms;
        Ok(draft)
    }
}

/// A text model that answers every structured request with one body.
#[derive(Debug)]
pub struct ScriptedTextModel {
    /// The transcript the earlier stages quoted, so a later stage can anchor
    /// into it the way a real model does.
    seen: std::sync::Mutex<Option<(String, u64, String)>>,
    body: serde_json::Value,
    context_tokens: u64,
    output_tokens: u64,
    fail: Option<TextModelError>,
}

impl ScriptedTextModel {
    /// A model that always returns `body`.
    ///
    /// `Value::Null` means "answer whatever the sent schema asks for",
    /// which is what a test that only cares that the pipeline *ran* wants.
    #[must_use]
    pub const fn new(body: serde_json::Value) -> Self {
        Self {
            seen: std::sync::Mutex::new(None),
            body,
            context_tokens: 100_000,
            output_tokens: 4_096,
            fail: None,
        }
    }

    /// A model that always fails with `error`.
    #[must_use]
    pub const fn failing(error: TextModelError) -> Self {
        Self {
            seen: std::sync::Mutex::new(None),
            body: serde_json::Value::Null,
            context_tokens: 100_000,
            output_tokens: 4_096,
            fail: Some(error),
        }
    }

    /// The model as a shared contract object.
    #[must_use]
    pub fn shared(self) -> Arc<dyn TextModel> {
        Arc::new(self) as Arc<dyn TextModel>
    }
}

#[async_trait]
impl TextModel for ScriptedTextModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        let engine_id = EngineId::new(SCRIPTED_TEXT_ENGINE);
        let mut capabilities = TextModelCapabilities::unknown(
            engine_id.clone(),
            ModelIdentity::new(engine_id, "scripted-text-1"),
        );
        capabilities.status = CapabilityStatus::Supported;
        capabilities.availability_reason = AvailabilityReason::Available;
        capabilities.offline = Tri::Yes;
        capabilities.can_cancel = Tri::Yes;
        capabilities.supports_structured_output = Tri::Yes;
        capabilities.max_context_tokens = Known::Known(self.context_tokens);
        capabilities.max_output_tokens = Known::Known(self.output_tokens);
        capabilities.privacy_class = Known::Known(PrivacyClass::OnDevice);
        capabilities.pricing_basis = Known::Known(PricingBasis::None);
        capabilities
    }

    async fn generate_structured(
        &self,
        request: StructuredRequest,
        ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        if ctx.is_cancelled() {
            return Err(TextModelError::Cancelled);
        }
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        // The pipeline asks three different shapes of the same model: a
        // section summary, a consolidated summary and the extraction body.
        // A scripted model that answered the extraction body to a summary
        // request would fail schema validation, so the shape is chosen from
        // the schema that was actually sent.
        if let Some(quoted) = quoted_transcript(&request.prompt) {
            *self.seen.lock().expect("the quoted transcript") = Some(quoted);
        }
        let body = if requires_section_summary(&request.output_schema) {
            section_summary(&request.prompt)
        } else if self.body.is_null() {
            extraction(self.seen.lock().expect("the quoted transcript").clone())
        } else {
            self.body.clone()
        };
        Ok(StructuredResponse {
            body,
            repair_attempts: 0,
            usage: None,
        })
    }
}

/// Whether the request asked for the §9.4 section-summary document.
fn requires_section_summary(schema: &serde_json::Value) -> bool {
    schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .any(|name| name.as_str() == Some("covered_first_segment"))
        })
}

/// A summary that claims exactly the segment range the prompt contained.
///
/// The pipeline proves coverage from what the model says it read, so a
/// scripted answer has to report the truth about the chunk it was given
/// rather than a constant.
fn section_summary(prompt: &str) -> serde_json::Value {
    let indices: Vec<u64> = prompt
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix('[')?;
            let (index, _) = rest.split_once(']')?;
            index.trim().parse::<u64>().ok()
        })
        .collect();
    let first = indices.iter().copied().min().unwrap_or(0);
    let last = indices.iter().copied().max().unwrap_or(0);
    serde_json::json!({
        "bullets": [],
        "covered_first_segment": first,
        "covered_last_segment": last,
    })
}

/// The revision id, segment index and text of the first fenced segment.
fn quoted_transcript(prompt: &str) -> Option<(String, u64, String)> {
    let revision_id = prompt
        .lines()
        .find_map(|line| line.trim().strip_prefix("revision_id: "))?
        .trim()
        .to_owned();
    let (seq_index, text) = prompt.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix('[')?;
        let (index, text) = rest.split_once("] ")?;
        Some((index.trim().parse::<u64>().ok()?, text.trim().to_owned()))
    })?;
    (!text.is_empty()).then_some((revision_id, seq_index, text))
}

/// One anchored statement over the first segment the prompt carried.
///
/// Extraction refuses an item it cannot ground, so a scripted answer quotes
/// the transcript it was actually given: the revision id and the excerpt
/// both come out of the fenced region.
fn extraction(quoted: Option<(String, u64, String)>) -> serde_json::Value {
    let Some((revision_id, seq_index, text)) = quoted else {
        // Nothing was quoted, so nothing can be grounded: an empty body is
        // the only honest answer, and the pipeline will reject it.
        return serde_json::json!({
            "executive_summary": [],
            "decisions": [],
            "action_items": [],
            "risks": [],
            "questions": [],
            "topics": [],
        });
    };
    serde_json::json!({
        "executive_summary": [{
            "kind": "summary_statement",
            "facet": "outcome",
            "text": text.clone(),
            "anchors": [{
                "revision_id": revision_id,
                "seq_index": seq_index,
                "char_start": 0,
                "char_end": text.chars().count().max(1),
                "excerpt": text,
            }],
        }],
        "decisions": [],
        "action_items": [],
        "risks": [],
        "questions": [],
        "topics": [],
    })
}
