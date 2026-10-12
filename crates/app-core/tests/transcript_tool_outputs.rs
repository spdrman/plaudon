//! Study artifacts and recording conversations stored against a real library
//! (M1.1, M1.2, M1.3; spec 9.7, 9.8).

use plaudon_ai_contracts::capabilities::{AvailabilityReason, PrivacyClass, TextModelCapabilities};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_ai_contracts::transcript::SourceTime;
use plaudon_ai_tools::registry::ToolRegistry;
use plaudon_ai_tools::study::{study_artifacts_from_value, STUDY_TEMPLATE_VERSION};
use plaudon_ai_tools::tool::{ToolId, TranscriptToolDefinition};
use plaudon_ai_tools::AskOutcomeKind;
use plaudon_app_core::transcript_tools::{
    ConversationService, OutputProvenance, StudyOutputWriter,
};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::ids::{JobId, RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::recording::Recording;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::RecordingRepository;
use plaudon_persistence::tools::{StoredToolDefinition, ToolRepository};
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use plaudon_persistence::Database;

const SEGMENTS: &[&str] = &[
    "We will ship the migration on Friday.",
    "Idempotency means the same request can be retried without duplicating work.",
];

struct Fixture {
    _dir: tempfile::TempDir,
    db: Database,
    recording_id: RecordingId,
    revision_id: TranscriptRevisionId,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = Database::open(dir.path().join("library.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");

    let recording_id = RecordingId::new();
    RecordingRepository::new(db.clone())
        .insert(&Recording::new(
            recording_id,
            "Platform sync",
            Timestamp::from_millis(1),
        ))
        .await
        .expect("insert recording");

    let revision_id = TranscriptRepository::new(db.clone())
        .save_draft(
            &TranscriptDraft {
                engine_id: EngineId::new("whisper.cpp"),
                model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
                input_sha256: Sha256Digest::of(b"fixture"),
                locale: None,
                alignment_origin: AlignmentOrigin::Native,
                segments: SEGMENTS
                    .iter()
                    .enumerate()
                    .map(|(index, text)| {
                        let seq = u32::try_from(index).unwrap();
                        TranscriptSegmentDraft {
                            seq_index: seq,
                            start: Some(SourceTime::from_millis(u64::from(seq) * 1_000, 16_000)),
                            end: Some(SourceTime::from_millis(
                                u64::from(seq) * 1_000 + 900,
                                16_000,
                            )),
                            text: (*text).to_string(),
                            confidence: None,
                            speaker_id: None,
                            words: Vec::new(),
                        }
                    })
                    .collect(),
                duration_ms: Some(2_000),
                engine_params: Default::default(),
                chunking_strategy: Default::default(),
                normalization_profile: None,
                annotations: Vec::new(),
                backend: None,
            },
            &RevisionContext::machine(recording_id, Timestamp::from_millis(2)),
        )
        .await
        .expect("save draft");

    Fixture {
        _dir: dir,
        db,
        recording_id,
        revision_id,
    }
}

/// Rebuild the domain revision carrying the persisted identity, so anchors
/// resolve against the same revision the rows name.
fn revision_for(fixture: &Fixture) -> TranscriptRevision {
    let mut revision = TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: fixture.recording_id,
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"fixture"),
        engine: EngineId::new("whisper.cpp"),
        model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
        locale: None,
        alignment_origin: AlignmentOrigin::Native,
        status: RevisionStatus::Complete,
        normalization_profile: "none".to_string(),
        chunking_strategy: "fixed".to_string(),
        duration_ms: Some(2_000),
        created_at: Timestamp::from_millis(2),
        segments: SEGMENTS
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let seq = u32::try_from(index).unwrap();
                TranscriptSegment::timed(
                    seq,
                    u64::from(seq) * 1_000,
                    u64::from(seq) * 1_000 + 900,
                    *text,
                )
            })
            .collect(),
    })
    .expect("revision");
    revision.id = fixture.revision_id;
    revision
}

fn definition_row(tool: &TranscriptToolDefinition) -> StoredToolDefinition {
    StoredToolDefinition {
        tool_id: tool.id.to_string(),
        version: tool.version,
        display_name: tool.display_name.clone(),
        kind: tool.kind.as_str().to_string(),
        prompt_version: tool.prompt_version.clone(),
        output_schema_version: tool.output_schema_version.clone(),
        definition_sha256: tool.definition_sha256(),
        origin: "builtin".to_string(),
        optional: tool.optional,
        created_at: Timestamp::from_millis(5),
    }
}

fn capabilities() -> TextModelCapabilities {
    TextModelCapabilities {
        payload_version: 1,
        status: CapabilityStatus::Supported,
        engine_id: EngineId::new("openrouter"),
        model: ModelIdentity::new(EngineId::new("openrouter"), "vendor/model-a"),
        offline: Tri::No,
        max_context_tokens: Known::Known(128_000),
        max_output_tokens: Known::Known(8_192),
        supports_structured_output: Tri::Yes,
        can_cancel: Tri::Yes,
        privacy_class: Known::Known(PrivacyClass::CloudZeroRetention),
        pricing_basis: Known::Unknown,
        availability_reason: AvailabilityReason::Available,
    }
}

#[derive(Debug)]
struct ScriptedModel(serde_json::Value);

#[async_trait::async_trait]
impl TextModel for ScriptedModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        capabilities()
    }
    async fn generate_structured(
        &self,
        _request: StructuredRequest,
        _ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        Ok(StructuredResponse {
            body: self.0.clone(),
            repair_attempts: 0,
            usage: None,
        })
    }
}

#[tokio::test]
async fn study_artifacts_become_outputs_with_one_evidence_row_per_citation() {
    let fixture = fixture().await;
    let revision = revision_for(&fixture);
    let validation = study_artifacts_from_value(
        &serde_json::json!({
            "flashcards": [{
                "front": "What does idempotency mean?",
                "back": "Retrying does not duplicate work.",
                "anchors": [{"seq_index": 1, "char_start": 0, "char_end": 11, "excerpt": "Idempotency"}]
            }],
            "quiz": [{
                "question": "When does the migration ship?",
                "answer": "Friday",
                "anchors": [{"seq_index": 0, "char_start": 0, "char_end": 7, "excerpt": "We will"}]
            }]
        }),
        &revision,
    )
    .expect("validated");
    assert!(validation.rejected.is_empty());

    let stored = StudyOutputWriter::new(fixture.db.clone())
        .persist(
            &validation.accepted,
            &OutputProvenance {
                recording_id: fixture.recording_id,
                transcript_revision_id: fixture.revision_id,
                model_id: "vendor/model-a".to_string(),
                prompt_version: "flashcards/1".to_string(),
                schema_version: "flashcards.v1".to_string(),
                created_at: Timestamp::from_millis(10),
            },
        )
        .await
        .expect("persist");

    assert_eq!(stored.output_ids.len(), 2);
    assert_eq!(stored.evidence_rows, 2);

    let types: Vec<String> =
        sqlx::query_scalar("SELECT output_type FROM ai_outputs ORDER BY output_type")
            .fetch_all(fixture.db.pool())
            .await
            .unwrap();
    assert_eq!(types, vec!["flashcards", "quiz"]);

    let template: String = sqlx::query_scalar("SELECT template_version FROM ai_outputs LIMIT 1")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    assert_eq!(template, STUDY_TEMPLATE_VERSION);

    let paths: Vec<String> =
        sqlx::query_scalar("SELECT item_path FROM output_evidence ORDER BY item_path")
            .fetch_all(fixture.db.pool())
            .await
            .unwrap();
    assert_eq!(paths, vec!["/flashcards/0", "/quiz/0"]);
    fixture.db.close().await;
}

#[tokio::test]
async fn an_empty_artifact_set_writes_nothing() {
    let fixture = fixture().await;
    let revision = revision_for(&fixture);
    let validation =
        study_artifacts_from_value(&serde_json::json!({}), &revision).expect("validated");

    let stored = StudyOutputWriter::new(fixture.db.clone())
        .persist(
            &validation.accepted,
            &OutputProvenance {
                recording_id: fixture.recording_id,
                transcript_revision_id: fixture.revision_id,
                model_id: "vendor/model-a".to_string(),
                prompt_version: "flashcards/1".to_string(),
                schema_version: "flashcards.v1".to_string(),
                created_at: Timestamp::from_millis(10),
            },
        )
        .await
        .expect("persist");
    assert!(stored.output_ids.is_empty());
    assert_eq!(stored.evidence_rows, 0);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_outputs")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    fixture.db.close().await;
}

#[tokio::test]
async fn an_answered_question_is_recorded_against_its_exact_revision() {
    let fixture = fixture().await;
    let revision = revision_for(&fixture);
    let registry = ToolRegistry::with_defaults();
    let tool = registry.latest(&ToolId::new("ask_recording")).unwrap();

    let model = ScriptedModel(serde_json::json!({
        "outcome": "answered",
        "answer": "On Friday.",
        "anchors": [{"seq_index": 0, "char_start": 0, "char_end": 7, "excerpt": "We will"}]
    }));

    let answered = ConversationService::new(fixture.db.clone())
        .ask(
            &model,
            &capabilities(),
            tool,
            &definition_row(tool),
            fixture.recording_id,
            &revision,
            "when does the migration ship?",
            JobContext::new(JobId::new()),
            Timestamp::from_millis(20),
        )
        .await
        .expect("ask");

    assert_eq!(answered.answer.outcome, AskOutcomeKind::Answered);
    assert_eq!(answered.answer.anchors.len(), 1);
    assert!(answered.tool_run_id.is_some());

    let repository = ToolRepository::new(fixture.db.clone());
    let conversations = repository
        .conversations_for(fixture.recording_id)
        .await
        .unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].1, fixture.revision_id);

    let messages = repository.messages(conversations[0].0).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].outcome.as_deref(), Some("answered"));
    assert!(messages[1].source_anchors_json.contains("We will"));
    assert_eq!(messages[1].tool_run_id, answered.tool_run_id);

    let runs = repository.runs_for(fixture.recording_id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "succeeded");
    assert_eq!(runs[0].transcript_revision_id, fixture.revision_id);
    assert_eq!(runs[0].cost, plaudon_domain::money::MoneyEstimate::Unknown);
    fixture.db.close().await;
}

#[tokio::test]
async fn an_ungrounded_answer_is_recorded_as_insufficient_evidence_with_a_rejected_run() {
    let fixture = fixture().await;
    let revision = revision_for(&fixture);
    let registry = ToolRegistry::with_defaults();
    let tool = registry.latest(&ToolId::new("ask_recording")).unwrap();

    let model = ScriptedModel(serde_json::json!({
        "outcome": "answered",
        "answer": "The budget is four million dollars.",
        "anchors": [{"seq_index": 0, "char_start": 0, "char_end": 7, "excerpt": "Invented"}]
    }));

    let answered = ConversationService::new(fixture.db.clone())
        .ask(
            &model,
            &capabilities(),
            tool,
            &definition_row(tool),
            fixture.recording_id,
            &revision,
            "when does the migration ship?",
            JobContext::new(JobId::new()),
            Timestamp::from_millis(20),
        )
        .await
        .expect("ask");

    assert_eq!(
        answered.answer.outcome,
        AskOutcomeKind::InsufficientEvidence
    );
    assert!(!answered.answer.text.contains("four million"));

    let repository = ToolRepository::new(fixture.db.clone());
    let runs = repository.runs_for(fixture.recording_id).await.unwrap();
    assert_eq!(runs[0].status, "rejected");
    assert_eq!(runs[0].output_sha256, None);

    let conversations = repository
        .conversations_for(fixture.recording_id)
        .await
        .unwrap();
    let messages = repository.messages(conversations[0].0).await.unwrap();
    assert_eq!(
        messages[1].outcome.as_deref(),
        Some("insufficient_evidence")
    );
    assert_eq!(messages[1].source_anchors_json, "[]");
    fixture.db.close().await;
}

#[tokio::test]
async fn a_question_the_recording_cannot_answer_is_recorded_without_a_run() {
    let fixture = fixture().await;
    let revision = revision_for(&fixture);
    let registry = ToolRegistry::with_defaults();
    let tool = registry.latest(&ToolId::new("ask_recording")).unwrap();

    let model = ScriptedModel(serde_json::json!({
        "outcome": "answered", "answer": "unused", "anchors": []
    }));

    let answered = ConversationService::new(fixture.db.clone())
        .ask(
            &model,
            &capabilities(),
            tool,
            &definition_row(tool),
            fixture.recording_id,
            &revision,
            "zarquon telemetry spike",
            JobContext::new(JobId::new()),
            Timestamp::from_millis(20),
        )
        .await
        .expect("ask");

    assert_eq!(
        answered.answer.outcome,
        AskOutcomeKind::NotSupportedByRecording
    );
    assert!(!answered.answer.dispatched);
    assert_eq!(answered.tool_run_id, None);

    let repository = ToolRepository::new(fixture.db.clone());
    assert!(repository
        .runs_for(fixture.recording_id)
        .await
        .unwrap()
        .is_empty());
    fixture.db.close().await;
}
