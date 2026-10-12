//! Deterministic fixtures shared by the pipeline unit tests.

use std::sync::Mutex;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::TextModelCapabilities;
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};

/// Build a timed machine revision from segment texts.
pub(crate) fn revision(texts: &[&str]) -> TranscriptRevision {
    build(None, texts, RevisionOrigin::Machine)
}

/// Build a human-edited child of `parent`.
pub(crate) fn revision_with_parent(
    parent: &TranscriptRevision,
    texts: &[&str],
) -> TranscriptRevision {
    build(Some(parent), texts, RevisionOrigin::HumanEdit)
}

fn build(
    parent: Option<&TranscriptRevision>,
    texts: &[&str],
    origin: RevisionOrigin,
) -> TranscriptRevision {
    let segments: Vec<TranscriptSegment> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let seq = u32::try_from(index).unwrap();
            TranscriptSegment::timed(
                seq,
                u64::from(seq) * 5_000,
                u64::from(seq) * 5_000 + 4_500,
                *text,
            )
        })
        .collect();
    TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: parent.map_or_else(RecordingId::new, |rev| rev.recording_id),
        parent_id: parent.map(|rev| rev.id),
        origin,
        input_sha256: Sha256Digest::of(b"fixture-media"),
        engine: EngineId::new("whisper.cpp"),
        model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
        locale: None,
        alignment_origin: AlignmentOrigin::Native,
        status: RevisionStatus::Complete,
        normalization_profile: "nfc".into(),
        chunking_strategy: "single".into(),
        duration_ms: Some(u64::try_from(texts.len()).unwrap_or(1) * 5_000),
        created_at: Timestamp::from_millis(0),
        segments,
    })
    .expect("fixture revision is valid")
}

/// Build a text-only revision, which permits no seek position.
pub(crate) fn text_only_revision(texts: &[&str]) -> TranscriptRevision {
    let segments: Vec<TranscriptSegment> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| TranscriptSegment::text_only(u32::try_from(index).unwrap(), *text))
        .collect();
    TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: RecordingId::new(),
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"fixture-media"),
        engine: EngineId::new("whisper.cpp"),
        model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
        locale: None,
        alignment_origin: AlignmentOrigin::None,
        status: RevisionStatus::Complete,
        normalization_profile: "nfc".into(),
        chunking_strategy: "single".into(),
        duration_ms: None,
        created_at: Timestamp::from_millis(0),
        segments,
    })
    .expect("fixture revision is valid")
}

/// Capabilities of a verified, structured-output-capable text model.
pub(crate) fn capabilities(context_tokens: u64) -> TextModelCapabilities {
    let engine = EngineId::new("test-text-engine");
    let mut caps = TextModelCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, "test-model").with_version("2026-01"),
    );
    caps.status = CapabilityStatus::Supported;
    caps.availability_reason = plaudon_ai_contracts::capabilities::AvailabilityReason::Available;
    caps.offline = Tri::Yes;
    caps.supports_structured_output = Tri::Yes;
    caps.can_cancel = Tri::Yes;
    caps.max_context_tokens = Known::Known(context_tokens);
    caps.max_output_tokens = Known::Known(2_048);
    caps.privacy_class = Known::Known(plaudon_ai_contracts::capabilities::PrivacyClass::OnDevice);
    caps
}

/// What a scripted model should do for the next call.
#[derive(Debug, Clone)]
pub(crate) enum Reply {
    /// Return this body.
    Body(serde_json::Value),
    /// Fail with this error.
    Fail(TextModelError),
}

/// A deterministic text model that replays a script.
///
/// Hermetic by construction: there is no network, no clock and no randomness,
/// so a pipeline test asserts on exactly the bodies it supplied.
#[derive(Debug)]
pub(crate) struct ScriptedModel {
    capabilities: TextModelCapabilities,
    script: Mutex<std::collections::VecDeque<Reply>>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedModel {
    pub(crate) fn new(capabilities: TextModelCapabilities, replies: Vec<Reply>) -> Self {
        Self {
            capabilities,
            script: Mutex::new(replies.into()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    /// Every prompt the pipeline sent, in order.
    pub(crate) fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("prompt log").clone()
    }

    /// Number of model calls that happened.
    pub(crate) fn call_count(&self) -> usize {
        self.prompts.lock().expect("prompt log").len()
    }
}

#[async_trait]
impl TextModel for ScriptedModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        self.capabilities.clone()
    }

    async fn generate_structured(
        &self,
        request: StructuredRequest,
        ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        if ctx.is_cancelled() {
            return Err(TextModelError::Cancelled);
        }
        self.prompts
            .lock()
            .expect("prompt log")
            .push(request.prompt.clone());
        let next = self
            .script
            .lock()
            .expect("script")
            .pop_front()
            .unwrap_or(Reply::Fail(TextModelError::EngineFailure));
        match next {
            Reply::Body(body) => Ok(StructuredResponse {
                body,
                repair_attempts: 0,
                usage: None,
            }),
            Reply::Fail(error) => Err(error),
        }
    }
}
