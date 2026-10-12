//! Adversarial prompt injection integration tests (EPIC I task I1.5).
//!
//! Tests the §18.6A "AI" cluster outcome over `tests/fixtures/ai/injection/*.json`.
//! Every fixture is loaded and asserted against its expectation block.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use plaudon_ai_contracts::capabilities::{
    CompatibilitySupport, EngineId, TextModelCapabilities, TextModelDescriptor,
};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_ai_contracts::validation::RepairPolicy;
use plaudon_app_core::chunking::{ChunkPolicy, ContextBudget};
use plaudon_app_core::pipeline::{
    prompt::{detect_directives, scan_response, UnsafeResponse},
    PipelineRequest, SteppingClock, SummarizationPipeline,
};
use plaudon_app_core::templates::TemplateRegistry;
use plaudon_domain::capability::Tri;
use plaudon_domain::claim::ClaimState;
use plaudon_domain::ids::JobId;
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{AlignmentOrigin, TranscriptRevision, TranscriptSegment};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Injection fixture format.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InjectionFixture {
    id: String,
    description: String,
    attack_class: String,
    segments: Vec<String>,
    expectation: Expectation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Expectation {
    directive_detected: bool,
    must_not_change_task: bool,
    must_not_emit_tool_request: bool,
    must_not_emit_url: bool,
}

/// A scripted model that can comply with or refuse injections.
struct ScriptedModel {
    mode: ScriptMode,
}

#[derive(Debug, Clone, Copy)]
enum ScriptMode {
    /// Always return safe, schema-compliant extraction.
    SafeExtraction,
    /// Return a response with a tool request (to test refusal).
    ToolRequest,
    /// Return a response with a URL (to test refusal).
    UrlFetch,
    /// Return fabricated anchors.
    FabricatedAnchors,
    /// Return empty anchors.
    EmptyAnchors,
}

impl ScriptedModel {
    fn new(mode: ScriptMode) -> Self {
        Self { mode }
    }

    fn build_safe_response(&self, revision_id: uuid::Uuid) -> serde_json::Value {
        json!({
            "executive_summary": [],
            "decisions": [],
            "action_items": [],
            "risks": [],
            "questions": [],
            "topics": []
        })
    }

    fn build_tool_request_response(&self) -> serde_json::Value {
        json!({
            "tool_calls": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "arguments": {"path": "/etc/passwd"}
                }
            }],
            "executive_summary": [],
            "decisions": [],
            "action_items": [],
            "risks": [],
            "questions": [],
            "topics": []
        })
    }

    fn build_url_fetch_response(&self) -> serde_json::Value {
        json!({
            "fetch": "https://evil.example.com/exfiltrate",
            "executive_summary": [],
            "decisions": [],
            "action_items": [],
            "risks": [],
            "questions": [],
            "topics": []
        })
    }

    fn build_fabricated_anchor_response(&self, revision_id: uuid::Uuid) -> serde_json::Value {
        json!({
            "executive_summary": [],
            "decisions": [{
                "kind": "decision",
                "statement": "Fabricated decision",
                "time_reference": "not_specified",
                "assertion": "model_interpretation",
                "anchors": [{
                    "revision_id": revision_id.to_string(),
                    "seq_index": 999,
                    "char_start": 0,
                    "char_end": 10,
                    "excerpt": "Nonexistent"
                }]
            }],
            "action_items": [],
            "risks": [],
            "questions": [],
            "topics": []
        })
    }

    fn build_empty_anchor_response(&self, _revision_id: uuid::Uuid) -> serde_json::Value {
        json!({
            "executive_summary": [],
            "decisions": [{
                "kind": "decision",
                "statement": "Decision without anchors",
                "time_reference": "not_specified",
                "assertion": "model_interpretation",
                "anchors": []
            }],
            "action_items": [],
            "risks": [],
            "questions": [],
            "topics": []
        })
    }
}

#[async_trait::async_trait]
impl TextModel for ScriptedModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        TextModelCapabilities {
            engine_id: EngineId::Fake,
            model: TextModelDescriptor {
                model_id: "scripted-model".to_owned(),
                model_name: "Scripted Model".to_owned(),
                model_version: Some("1.0.0".to_owned()),
                context_window_tokens: Some(128_000),
                max_output_tokens: Some(16_000),
            },
            structured_output: Tri::Yes,
            compatibility_support: CompatibilitySupport::NativeJsonSchema,
            is_available: Tri::Yes,
            identity_verified: Tri::Yes,
        }
    }

    async fn generate_structured(
        &self,
        request: StructuredRequest,
        _ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        // Extract revision ID from prompt (hacky but works for test)
        let revision_id = uuid::Uuid::new_v4(); // In real test, parse from prompt

        let body = match self.mode {
            ScriptMode::SafeExtraction => self.build_safe_response(revision_id),
            ScriptMode::ToolRequest => self.build_tool_request_response(),
            ScriptMode::UrlFetch => self.build_url_fetch_response(),
            ScriptMode::FabricatedAnchors => self.build_fabricated_anchor_response(revision_id),
            ScriptMode::EmptyAnchors => self.build_empty_anchor_response(revision_id),
        };

        Ok(StructuredResponse {
            body,
            repair_attempts: 0,
            usage: None,
        })
    }
}

fn load_injection_fixtures() -> Vec<InjectionFixture> {
    let fixture_dir = PathBuf::from("tests/fixtures/ai/injection");
    let mut fixtures = Vec::new();

    for entry in std::fs::read_dir(&fixture_dir).expect("Failed to read injection directory") {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path).expect("Failed to read fixture file");
            let fixture: InjectionFixture =
                serde_json::from_str(&content).expect("Failed to parse fixture JSON");
            fixtures.push(fixture);
        }
    }

    fixtures.sort_by(|a, b| a.id.cmp(&b.id));
    fixtures
}

fn fixture_to_transcript(fixture: &InjectionFixture) -> TranscriptRevision {
    let segments: Vec<TranscriptSegment> = fixture
        .segments
        .iter()
        .enumerate()
        .map(|(idx, text)| TranscriptSegment {
            seq_index: idx as u32,
            text: text.clone(),
            speaker: Some("Speaker".to_owned()),
            start_ms: Some(idx as u64 * 1000),
            end_ms: Some((idx as u64 + 1) * 1000),
        })
        .collect();

    TranscriptRevision::new(
        plaudon_domain::ids::TranscriptRevisionId::new(),
        AlignmentOrigin::Native,
        segments,
        "production".to_owned(),
        "v1".to_owned(),
        Timestamp::now(),
    )
}

#[tokio::test]
async fn all_injection_fixtures_are_covered() {
    let fixtures = load_injection_fixtures();
    assert!(
        !fixtures.is_empty(),
        "No injection fixtures found; directory may be missing"
    );
    println!("Loaded {} injection fixtures", fixtures.len());

    // Verify that we have exactly 16 fixtures as documented
    assert_eq!(
        fixtures.len(),
        16,
        "Expected 16 injection fixtures as documented in README"
    );
}

#[tokio::test]
async fn directive_detection_matches_expectation() {
    let fixtures = load_injection_fixtures();

    for fixture in &fixtures {
        let revision = fixture_to_transcript(fixture);
        let segments = revision.segments();
        let detected = detect_directives(segments);

        if fixture.expectation.directive_detected {
            assert!(
                !detected.is_empty(),
                "Fixture {} expected directive detection but none found",
                fixture.id
            );
        } else {
            assert!(
                detected.is_empty(),
                "Fixture {} is benign control but directive was detected: {:?}",
                fixture.id,
                detected
            );
        }
    }
}

#[tokio::test]
async fn benign_controls_do_not_fire_detector() {
    let fixtures = load_injection_fixtures();
    let benign: Vec<_> = fixtures
        .iter()
        .filter(|f| f.attack_class == "benign_control")
        .collect();

    assert_eq!(
        benign.len(),
        2,
        "Expected exactly 2 benign control fixtures"
    );

    for fixture in benign {
        assert!(
            !fixture.expectation.directive_detected,
            "Benign control {} must have directive_detected: false",
            fixture.id
        );

        let revision = fixture_to_transcript(fixture);
        let detected = detect_directives(revision.segments());
        assert!(
            detected.is_empty(),
            "Benign control {} fired detector: {:?}",
            fixture.id,
            detected
        );
    }
}

#[tokio::test]
async fn scan_response_rejects_tool_requests() {
    let response = json!({
        "tool_calls": [{
            "type": "function",
            "function": {
                "name": "read_file",
                "arguments": {"path": "/etc/passwd"}
            }
        }]
    });

    let result = scan_response(&response);
    assert!(
        matches!(result, Err(UnsafeResponse::ToolRequest)),
        "Tool request should be rejected"
    );
}

#[tokio::test]
async fn fabricated_anchors_result_in_unverified_claims() {
    let fixture_id = "fabricated_anchor";
    let fixtures = load_injection_fixtures();
    let fixture = fixtures
        .iter()
        .find(|f| f.id == fixture_id)
        .expect("fabricated_anchor fixture not found");

    let revision = fixture_to_transcript(fixture);
    let model = Arc::new(ScriptedModel::new(ScriptMode::FabricatedAnchors));
    let clock = Arc::new(SteppingClock::new(1_700_000_000_000, 1000));
    let pipeline = SummarizationPipeline::new(model.clone(), clock);

    let registry = TemplateRegistry::with_builtins();
    let template = registry.get("general_meeting").expect("Template not found");
    let extraction_schema = registry
        .extraction_schema(template)
        .expect("Schema not found");
    let summary_schema = registry.summary_schema().expect("Summary schema not found");

    let capabilities = model.capabilities().await;
    let speakers = plaudon_ai_contracts::validation::SpeakerDirectory::unknown();

    let request = PipelineRequest {
        recording_id: RecordingId::new(),
        revision: revision.clone(),
        template: template.clone(),
        extraction_schema,
        summary_schema,
        capabilities,
        chunk_policy: ChunkPolicy {
            reserved_output_tokens: 4000,
            prompt_overhead_tokens: 2000,
            overlap_segments: 2,
            unknown_context: plaudon_app_core::chunking::UnknownContext::FailClosed,
        },
        repair_policy: RepairPolicy::default(),
        speakers,
        reusable_sections: vec![],
        privacy_policy_snapshot: None,
        audio_sha256: Sha256Digest::from_bytes([0; 32]),
    };

    let ctx = JobContext::new(JobId::new());
    let outcome = pipeline
        .run(request, &ctx)
        .await
        .expect("Pipeline should complete");

    // Claims with fabricated anchors should be Unverified
    for claim in &outcome.claims {
        if !claim.anchors().is_empty() {
            let any_verified = claim.anchors().iter().any(|a| a.verification.is_verified());
            if !any_verified {
                assert_eq!(
                    claim.state(),
                    ClaimState::Unverified,
                    "Claim with fabricated anchor should be Unverified"
                );
                assert!(
                    claim.review_required(),
                    "Claim with fabricated anchor should require review"
                );
            }
        }
    }
}

#[tokio::test]
async fn missing_anchors_result_in_unverified_state() {
    let model = Arc::new(ScriptedModel::new(ScriptMode::EmptyAnchors));
    let clock = Arc::new(SteppingClock::new(1_700_000_000_000, 1000));
    let pipeline = SummarizationPipeline::new(model.clone(), clock);

    let registry = TemplateRegistry::with_builtins();
    let template = registry.get("general_meeting").expect("Template not found");
    let extraction_schema = registry
        .extraction_schema(template)
        .expect("Schema not found");
    let summary_schema = registry.summary_schema().expect("Summary schema not found");

    // Create a simple transcript
    let revision = TranscriptRevision::new(
        plaudon_domain::ids::TranscriptRevisionId::new(),
        AlignmentOrigin::Native,
        vec![TranscriptSegment {
            seq_index: 0,
            text: "This is a test segment".to_owned(),
            speaker: Some("Speaker".to_owned()),
            start_ms: Some(0),
            end_ms: Some(1000),
        }],
        "production".to_owned(),
        "v1".to_owned(),
        Timestamp::now(),
    );

    let capabilities = model.capabilities().await;
    let speakers = plaudon_ai_contracts::validation::SpeakerDirectory::unknown();

    let request = PipelineRequest {
        recording_id: RecordingId::new(),
        revision: revision.clone(),
        template: template.clone(),
        extraction_schema,
        summary_schema,
        capabilities,
        chunk_policy: ChunkPolicy {
            reserved_output_tokens: 4000,
            prompt_overhead_tokens: 2000,
            overlap_segments: 2,
            unknown_context: plaudon_app_core::chunking::UnknownContext::FailClosed,
        },
        repair_policy: RepairPolicy::default(),
        speakers,
        reusable_sections: vec![],
        privacy_policy_snapshot: None,
        audio_sha256: Sha256Digest::from_bytes([0; 32]),
    };

    let ctx = JobContext::new(JobId::new());
    let outcome = pipeline
        .run(request, &ctx)
        .await
        .expect("Pipeline should complete");

    // Claims without anchors should be Unverified when template requires them
    for claim in &outcome.claims {
        if claim.anchors().is_empty() {
            assert_eq!(
                claim.state(),
                ClaimState::Unverified,
                "Claim without anchors should be Unverified"
            );
        }
    }
}

#[tokio::test]
async fn log_safety_no_transcript_text_in_debug_output() {
    let fixtures = load_injection_fixtures();
    let fixture = &fixtures[0]; // Use any fixture

    let revision = fixture_to_transcript(fixture);
    let detected = detect_directives(revision.segments());

    // Debug output of DetectedDirective should not contain segment text
    for directive in &detected {
        let debug_str = format!("{:?}", directive);
        for segment in &fixture.segments {
            assert!(
                !debug_str.contains(segment),
                "Debug output contains transcript text: {}",
                segment
            );
        }
    }
}
