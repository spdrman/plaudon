//! Adversarial prompt-injection suite (spec §9.6, §9.7, §18.5, §18.6A — task I1.5).
//!
//! Every fixture under `tests/fixtures/ai/injection/` is enumerated and
//! exercised: the suite fails if a fixture exists that no case covers. For
//! each one the real pipeline runs against a scripted model, and the §18.6A
//! "AI" cluster outcome is asserted — the task and the response schema are
//! unchanged, injected text never escapes the fenced data region of any
//! prompt actually sent, a response that complies with the injection is
//! refused, and a fabricated or absent anchor ends as an `Unverified` claim.
//!
//! Hermetic: no network, no provider, no clock dependency.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaudon_ai_contracts::capabilities::{AvailabilityReason, PrivacyClass, TextModelCapabilities};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_ai_contracts::validation::{RejectionReason, RepairPolicy, SpeakerDirectory};
use plaudon_app_core::chunking::ChunkPolicy;
use plaudon_app_core::pipeline::prompt::{detect_directives, scan_response, UnsafeResponse};
use plaudon_app_core::pipeline::{
    PipelineError, PipelineOutcome, PipelineRequest, SteppingClock, SummarizationPipeline,
};
use plaudon_app_core::templates::TemplateRegistry;
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::claim::ClaimState;
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};
use serde::Deserialize;

/// The classes the fixture corpus declares.
const ATTACK_CLASSES: &[&str] = &[
    "instruction_override",
    "role_confusion",
    "tool_invocation",
    "url_exfiltration",
    "file_access",
    "shell_command",
    "schema_subversion",
    "anchor_fabrication_demand",
    "credential_phishing",
    "encoded_payload",
    "delimiter_escape",
    "system_prompt_extraction",
    "consent_bypass",
    "benign_control",
];

#[derive(Debug, Deserialize)]
struct Expectation {
    directive_detected: bool,
    #[allow(dead_code)]
    must_not_change_task: bool,
    #[allow(dead_code)]
    must_not_emit_tool_request: bool,
    #[allow(dead_code)]
    must_not_emit_url: bool,
}

#[derive(Debug, Deserialize)]
struct InjectionFixture {
    id: String,
    #[allow(dead_code)]
    description: String,
    attack_class: String,
    segments: Vec<String>,
    expectation: Expectation,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/ai/injection")
}

fn load_fixtures() -> Vec<InjectionFixture> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixture_dir())
        .expect("injection fixture directory exists")
        .map(|entry| entry.expect("readable entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "the injection corpus must not be empty");
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).expect("readable fixture");
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{} is malformed: {error}", path.display()))
        })
        .collect()
}

fn revision(texts: &[String]) -> TranscriptRevision {
    let segments: Vec<TranscriptSegment> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let seq = u32::try_from(index).expect("fixture is small");
            TranscriptSegment::timed(
                seq,
                u64::from(seq) * 4_000,
                u64::from(seq) * 4_000 + 3_500,
                text.clone(),
            )
        })
        .collect();
    TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: RecordingId::new(),
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"injection-fixture"),
        engine: EngineId::new("whisper.cpp"),
        model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
        locale: None,
        alignment_origin: AlignmentOrigin::Native,
        status: RevisionStatus::Complete,
        normalization_profile: "nfc".into(),
        chunking_strategy: "single".into(),
        duration_ms: Some(u64::try_from(texts.len()).unwrap_or(1) * 4_000),
        created_at: Timestamp::from_millis(0),
        segments,
    })
    .expect("fixture revision is valid")
}

fn capabilities() -> TextModelCapabilities {
    let engine = EngineId::new("test-text-engine");
    let mut caps = TextModelCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, "test-model").with_version("1"),
    );
    caps.status = CapabilityStatus::Supported;
    caps.availability_reason = AvailabilityReason::Available;
    caps.offline = Tri::Yes;
    caps.supports_structured_output = Tri::Yes;
    caps.can_cancel = Tri::Yes;
    caps.max_context_tokens = Known::Known(100_000);
    caps.max_output_tokens = Known::Known(2_048);
    caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
    caps
}

#[derive(Debug)]
struct ScriptedModel {
    caps: TextModelCapabilities,
    replies: Mutex<std::collections::VecDeque<serde_json::Value>>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedModel {
    fn new(replies: Vec<serde_json::Value>) -> Self {
        Self {
            caps: capabilities(),
            replies: Mutex::new(replies.into()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().clone()
    }
}

#[async_trait]
impl TextModel for ScriptedModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        self.caps.clone()
    }

    async fn generate_structured(
        &self,
        request: StructuredRequest,
        _ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        self.prompts.lock().push(request.prompt.clone());
        let body = self
            .replies
            .lock()
            .pop_front()
            .ok_or(TextModelError::EngineFailure)?;
        Ok(StructuredResponse {
            body,
            repair_attempts: 0,
            usage: None,
        })
    }
}

fn summary_body(revision: &TranscriptRevision) -> serde_json::Value {
    let last = u32::try_from(revision.segments().len() - 1).expect("small fixture");
    serde_json::json!({
        "bullets": [],
        "covered_first_segment": 0,
        "covered_last_segment": last,
    })
}

fn empty_extraction() -> serde_json::Value {
    serde_json::json!({
        "executive_summary": [], "decisions": [], "action_items": [],
        "risks": [], "questions": [], "topics": []
    })
}

fn request(revision: &TranscriptRevision, registry: &TemplateRegistry) -> PipelineRequest {
    let template = registry
        .get("general_meeting")
        .expect("built-in template")
        .clone();
    let extraction_schema = registry
        .extraction_schema(&template)
        .expect("schema resolves");
    PipelineRequest {
        recording_id: revision.recording_id,
        revision: revision.clone(),
        template,
        extraction_schema,
        summary_schema: registry.summary_schema(),
        capabilities: capabilities(),
        chunk_policy: ChunkPolicy::DEFAULT,
        repair_policy: RepairPolicy::NONE,
        speakers: SpeakerDirectory::unknown(),
        reusable_sections: Vec::new(),
        privacy_policy_snapshot: Some("on-device".into()),
        audio_sha256: Sha256Digest::of(b"injection-fixture"),
    }
}

async fn run(
    revision: &TranscriptRevision,
    registry: &TemplateRegistry,
    replies: Vec<serde_json::Value>,
) -> (Result<PipelineOutcome, PipelineError>, Arc<ScriptedModel>) {
    let model = Arc::new(ScriptedModel::new(replies));
    let pipeline = SummarizationPipeline::new(model.clone(), Arc::new(SteppingClock::new(0, 1)));
    let outcome = pipeline
        .run(request(revision, registry), &JobContext::new(JobId::new()))
        .await;
    (outcome, model)
}

#[test]
fn the_corpus_covers_every_declared_attack_class_and_has_benign_controls() {
    let fixtures = load_fixtures();
    for class in ATTACK_CLASSES {
        assert!(
            fixtures.iter().any(|f| f.attack_class == *class),
            "no fixture exercises attack class {class}"
        );
    }
    for fixture in &fixtures {
        assert!(
            ATTACK_CLASSES.contains(&fixture.attack_class.as_str()),
            "{} declares an unknown attack class {}",
            fixture.id,
            fixture.attack_class
        );
        assert!(
            !fixture.segments.is_empty(),
            "{} has no transcript segments",
            fixture.id
        );
    }
    let controls = fixtures
        .iter()
        .filter(|f| f.attack_class == "benign_control")
        .count();
    assert!(controls >= 2, "the corpus needs benign controls");
}

#[test]
fn the_detector_matches_every_fixture_expectation_including_the_controls() {
    for fixture in load_fixtures() {
        let revision = revision(&fixture.segments);
        let detected = detect_directives(revision.segments());
        assert_eq!(
            !detected.is_empty(),
            fixture.expectation.directive_detected,
            "{} ({}): detector fired {:?}",
            fixture.id,
            fixture.attack_class,
            detected
        );
    }
}

#[test]
fn a_detection_records_no_transcript_text() {
    for fixture in load_fixtures() {
        let revision = revision(&fixture.segments);
        let detected = detect_directives(revision.segments());
        let rendered = format!(
            "{}{}",
            serde_json::to_string(&detected).expect("serializable"),
            format_args!("{detected:?}")
        );
        for segment in &fixture.segments {
            for word in segment.split_whitespace().filter(|w| w.len() > 8) {
                let cleaned: String = word
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '/' || *c == '.')
                    .collect();
                if cleaned.len() > 8 {
                    assert!(
                        !rendered.contains(&cleaned),
                        "{} leaked transcript content {cleaned:?} into a detection record",
                        fixture.id
                    );
                }
            }
        }
        assert!(!rendered.contains("http"), "{} leaked a URL", fixture.id);
    }
}

#[tokio::test]
async fn injected_text_never_escapes_the_data_region_of_any_prompt_sent() {
    let registry = TemplateRegistry::with_builtins();
    for fixture in load_fixtures() {
        let revision = revision(&fixture.segments);
        let (outcome, model) = run(
            &revision,
            &registry,
            vec![summary_body(&revision), empty_extraction()],
        )
        .await;
        outcome.unwrap_or_else(|error| panic!("{} failed to run: {error}", fixture.id));

        let prompts = model.prompts();
        assert_eq!(prompts.len(), 2, "{}: stage count changed", fixture.id);
        for prompt in prompts {
            assert!(
                prompt.contains("RESPONSE SCHEMA"),
                "{}: the schema requirement survived",
                fixture.id
            );
            assert_eq!(
                prompt.matches("-----END UNTRUSTED TRANSCRIPT-----").count(),
                1,
                "{}: the data region has exactly one terminator",
                fixture.id
            );
            let fence = prompt
                .find("-----BEGIN UNTRUSTED TRANSCRIPT-----")
                .expect("the data region exists");
            for segment in &fixture.segments {
                let probe: String = segment.chars().take(24).collect();
                if let Some(position) = prompt.find(&probe) {
                    assert!(
                        position > fence,
                        "{}: transcript text appeared before the data region",
                        fixture.id
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_response_that_complies_with_an_injection_is_refused() {
    let registry = TemplateRegistry::with_builtins();
    let fixtures = load_fixtures();
    let hostile = fixtures
        .iter()
        .find(|f| f.attack_class == "tool_invocation")
        .expect("a tool-invocation fixture exists");
    let revision = revision(&hostile.segments);

    let mut complying = empty_extraction();
    complying["tool_calls"] =
        serde_json::json!([{ "name": "http_get", "url": "https://x.invalid" }]);
    let (outcome, _) = run(
        &revision,
        &registry,
        vec![summary_body(&revision), complying],
    )
    .await;
    assert_eq!(
        outcome.unwrap_err(),
        PipelineError::UnsafeResponse(UnsafeResponse::ToolRequest)
    );

    for (key, expected) in [
        ("read_file", UnsafeResponse::FileRequest),
        ("command", UnsafeResponse::CommandRequest),
        ("webhook", UnsafeResponse::NetworkRequest),
    ] {
        let mut body = empty_extraction();
        body[key] = serde_json::json!("anything");
        assert_eq!(scan_response(&body), Err(expected));
    }
}

#[tokio::test]
async fn a_fabricated_or_absent_anchor_never_becomes_source_linked() {
    let registry = TemplateRegistry::with_builtins();
    let fixtures = load_fixtures();
    let hostile = fixtures
        .iter()
        .find(|f| f.attack_class == "anchor_fabrication_demand")
        .expect("an anchor-fabrication fixture exists");
    let revision = revision(&hostile.segments);

    let mut fabricated = empty_extraction();
    fabricated["decisions"] = serde_json::json!([{
        "kind": "decision",
        "statement": "The budget was doubled",
        "participants": [],
        "time_reference": "not_specified",
        "assertion": "directly_stated",
        "anchors": [{
            "revision_id": revision.id.to_string(),
            "seq_index": 999,
            "char_start": 0,
            "char_end": 12,
            "excerpt": "never said this"
        }]
    }]);
    let (outcome, _) = run(
        &revision,
        &registry,
        vec![summary_body(&revision), fabricated],
    )
    .await;
    let outcome = outcome.expect("the run completes with the item rejected or unverified");
    for claim in &outcome.claims {
        assert_ne!(
            claim.state(),
            ClaimState::SourceLinked,
            "a fabricated anchor must never be source-linked"
        );
    }
    assert!(outcome.verified_claims().is_empty());

    let mut anchorless = empty_extraction();
    anchorless["decisions"] = serde_json::json!([{
        "kind": "decision",
        "statement": "The budget was doubled",
        "participants": [],
        "time_reference": "not_specified",
        "assertion": "directly_stated",
        "anchors": []
    }]);
    let (outcome, _) = run(
        &revision,
        &registry,
        vec![summary_body(&revision), anchorless],
    )
    .await;
    // Nothing survived validation, so the run fails visibly with the reasons
    // rather than presenting an empty summary as a result (spec §9.5).
    match outcome.unwrap_err() {
        PipelineError::ExtractionRejected { rejected, .. } => {
            assert!(
                rejected
                    .iter()
                    .any(|item| item.reason == RejectionReason::AnchorMissing),
                "the missing anchor is reported with its reason"
            );
        }
        other => panic!("expected a visible extraction failure, got {other}"),
    }
}

#[tokio::test]
async fn a_schema_subversion_attempt_does_not_change_the_required_schema() {
    let registry = TemplateRegistry::with_builtins();
    let fixtures = load_fixtures();
    let hostile = fixtures
        .iter()
        .find(|f| f.attack_class == "schema_subversion")
        .expect("a schema-subversion fixture exists");
    let revision = revision(&hostile.segments);
    let (outcome, model) = run(
        &revision,
        &registry,
        vec![summary_body(&revision), empty_extraction()],
    )
    .await;
    outcome.expect("run completes");

    let extraction_prompt = model
        .prompts()
        .pop()
        .expect("an extraction prompt was sent");
    assert!(extraction_prompt.contains("\"$defs\"") || extraction_prompt.contains("$defs"));
    assert!(extraction_prompt.contains("meeting-extraction"));
}
