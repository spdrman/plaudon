//! Persistence invariants for tool runs and per-recording conversations
//! (M1.1, spec 9.7, 13.3).

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::transcript::SourceTime;
use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::money::{Currency, Money, MoneyEstimate};
use plaudon_domain::recording::Recording;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::RecordingRepository;
use plaudon_persistence::tools::{
    StoredMessage, StoredToolDefinition, ToolRepository, ToolRunInsert,
};
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use plaudon_persistence::{Database, PersistenceError};
use uuid::Uuid;

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
                segments: vec![TranscriptSegmentDraft {
                    seq_index: 0,
                    start: Some(SourceTime::from_millis(0, 16_000)),
                    end: Some(SourceTime::from_millis(1_000, 16_000)),
                    text: "We will ship on Friday.".to_string(),
                    confidence: None,
                    speaker_id: None,
                    words: Vec::new(),
                }],
                duration_ms: Some(1_000),
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

fn definition(version: u32, hash: &str) -> StoredToolDefinition {
    StoredToolDefinition {
        tool_id: "meeting_summary".to_string(),
        version,
        display_name: "Meeting summary".to_string(),
        kind: "meeting_summary".to_string(),
        prompt_version: format!("meeting_summary/{version}"),
        output_schema_version: "meeting_summary.v1".to_string(),
        definition_sha256: hash.to_string(),
        origin: "builtin".to_string(),
        optional: false,
        created_at: Timestamp::from_millis(10),
    }
}

fn run(fixture: &Fixture, version: u32, cost: MoneyEstimate) -> ToolRunInsert {
    ToolRunInsert {
        id: Uuid::now_v7(),
        recording_id: fixture.recording_id,
        transcript_revision_id: fixture.revision_id,
        tool_id: "meeting_summary".to_string(),
        tool_version: version,
        engine: "openrouter".to_string(),
        model_id: "vendor/model-a".to_string(),
        model_version: None,
        request_family: "audio_chat".to_string(),
        prompt_version: format!("meeting_summary/{version}"),
        schema_version: "meeting_summary.v1".to_string(),
        input_sha256: "a".repeat(64),
        output_sha256: Some("b".repeat(64)),
        status: "succeeded".to_string(),
        consent_id: None,
        consent_policy_json: "{}".to_string(),
        cost,
        created_at: Timestamp::from_millis(20),
        finished_at: Some(Timestamp::from_millis(30)),
    }
}

#[tokio::test]
async fn a_tool_version_change_appends_a_row_and_leaves_prior_runs_pointing_at_the_old_one() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());

    tools
        .register(&definition(1, &"1".repeat(64)))
        .await
        .unwrap();
    tools
        .record_run(&run(&fixture, 1, MoneyEstimate::Unknown))
        .await
        .unwrap();

    tools
        .register(&definition(2, &"2".repeat(64)))
        .await
        .unwrap();

    let definitions = tools.definitions().await.unwrap();
    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[0].version, 1);
    assert_eq!(definitions[0].definition_sha256, "1".repeat(64));

    let runs = tools.runs_for(fixture.recording_id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].tool_version, 1);
    fixture.db.close().await;
}

#[tokio::test]
async fn re_registering_the_same_version_with_different_content_is_a_conflict() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    tools
        .register(&definition(1, &"1".repeat(64)))
        .await
        .unwrap();

    // Identical content is idempotent.
    tools
        .register(&definition(1, &"1".repeat(64)))
        .await
        .unwrap();

    // Edited content under the same version is refused.
    let err = tools
        .register(&definition(1, &"9".repeat(64)))
        .await
        .unwrap_err();
    assert!(matches!(err, PersistenceError::Conflict));
    fixture.db.close().await;
}

#[tokio::test]
async fn a_run_cannot_reference_a_definition_that_was_never_registered() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    assert!(tools
        .record_run(&run(&fixture, 7, MoneyEstimate::Unknown))
        .await
        .is_err());
    fixture.db.close().await;
}

#[tokio::test]
async fn an_unknown_cost_round_trips_as_unknown_and_never_as_zero() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    tools
        .register(&definition(1, &"1".repeat(64)))
        .await
        .unwrap();
    tools
        .record_run(&run(&fixture, 1, MoneyEstimate::Unknown))
        .await
        .unwrap();

    let runs = tools.runs_for(fixture.recording_id).await.unwrap();
    assert_eq!(runs[0].cost, MoneyEstimate::Unknown);
    assert_eq!(runs[0].cost.amount(), None);
    fixture.db.close().await;
}

#[tokio::test]
async fn a_finalized_cost_round_trips_with_its_currency() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    tools
        .register(&definition(1, &"1".repeat(64)))
        .await
        .unwrap();
    let cost = MoneyEstimate::Finalized(Money::new(1_234, 2, Currency::USD));
    tools.record_run(&run(&fixture, 1, cost)).await.unwrap();

    let runs = tools.runs_for(fixture.recording_id).await.unwrap();
    assert_eq!(runs[0].cost, cost);
    fixture.db.close().await;
}

#[tokio::test]
async fn a_conversation_turn_cannot_exist_without_its_conversation() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    let orphan = Uuid::now_v7();
    let result = tools
        .append_message(
            orphan,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 0,
                role: "user".to_string(),
                text: "who owns the rollback?".to_string(),
                source_anchors_json: "[]".to_string(),
                outcome: None,
                tool_run_id: None,
                created_at: Timestamp::from_millis(40),
            },
        )
        .await;
    assert!(result.is_err(), "an orphan turn must be impossible");
    fixture.db.close().await;
}

#[tokio::test]
async fn a_conversation_is_scoped_to_a_recording_and_an_exact_revision() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    let conversation = Uuid::now_v7();
    tools
        .start_conversation(
            conversation,
            fixture.recording_id,
            fixture.revision_id,
            Timestamp::from_millis(40),
        )
        .await
        .unwrap();

    let conversations = tools.conversations_for(fixture.recording_id).await.unwrap();
    assert_eq!(conversations, vec![(conversation, fixture.revision_id)]);

    // A conversation naming a revision that does not exist is refused by the
    // foreign key, so there is no way to record provenance-free chat.
    assert!(tools
        .start_conversation(
            Uuid::now_v7(),
            fixture.recording_id,
            TranscriptRevisionId::new(),
            Timestamp::from_millis(41),
        )
        .await
        .is_err());
    fixture.db.close().await;
}

#[tokio::test]
async fn an_answered_turn_without_anchors_is_rejected_by_the_schema() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    let conversation = Uuid::now_v7();
    tools
        .start_conversation(
            conversation,
            fixture.recording_id,
            fixture.revision_id,
            Timestamp::from_millis(40),
        )
        .await
        .unwrap();

    let result = tools
        .append_message(
            conversation,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 0,
                role: "assistant".to_string(),
                text: "Confident prose with no evidence.".to_string(),
                source_anchors_json: "[]".to_string(),
                outcome: Some("answered".to_string()),
                tool_run_id: None,
                created_at: Timestamp::from_millis(41),
            },
        )
        .await;
    assert!(result.is_err(), "an unanchored answer must not persist");
    fixture.db.close().await;
}

#[tokio::test]
async fn a_user_turn_has_no_outcome_and_an_assistant_turn_must_have_one() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    let conversation = Uuid::now_v7();
    tools
        .start_conversation(
            conversation,
            fixture.recording_id,
            fixture.revision_id,
            Timestamp::from_millis(40),
        )
        .await
        .unwrap();

    // A user turn carrying an outcome is refused.
    assert!(tools
        .append_message(
            conversation,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 0,
                role: "user".to_string(),
                text: "question".to_string(),
                source_anchors_json: "[]".to_string(),
                outcome: Some("answered".to_string()),
                tool_run_id: None,
                created_at: Timestamp::from_millis(41),
            },
        )
        .await
        .is_err());

    // An assistant turn without one is refused.
    assert!(tools
        .append_message(
            conversation,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 1,
                role: "assistant".to_string(),
                text: "answer".to_string(),
                source_anchors_json: "[]".to_string(),
                outcome: None,
                tool_run_id: None,
                created_at: Timestamp::from_millis(42),
            },
        )
        .await
        .is_err());
    fixture.db.close().await;
}

#[tokio::test]
async fn a_full_exchange_round_trips_in_order() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    let conversation = Uuid::now_v7();
    tools
        .start_conversation(
            conversation,
            fixture.recording_id,
            fixture.revision_id,
            Timestamp::from_millis(40),
        )
        .await
        .unwrap();

    let anchors = serde_json::json!([{
        "revision_id": fixture.revision_id.to_string(),
        "seq_index": 0, "char_start": 0, "char_end": 7, "excerpt": "We will"
    }])
    .to_string();

    tools
        .append_message(
            conversation,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 0,
                role: "user".to_string(),
                text: "when do we ship?".to_string(),
                source_anchors_json: "[]".to_string(),
                outcome: None,
                tool_run_id: None,
                created_at: Timestamp::from_millis(41),
            },
        )
        .await
        .unwrap();
    tools
        .append_message(
            conversation,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 1,
                role: "assistant".to_string(),
                text: "Friday.".to_string(),
                source_anchors_json: anchors.clone(),
                outcome: Some("answered".to_string()),
                tool_run_id: None,
                created_at: Timestamp::from_millis(42),
            },
        )
        .await
        .unwrap();

    let messages = tools.messages(conversation).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].outcome.as_deref(), Some("answered"));
    assert_eq!(messages[1].source_anchors_json, anchors);
    fixture.db.close().await;
}

#[tokio::test]
async fn an_insufficient_evidence_turn_may_legitimately_carry_no_anchors() {
    let fixture = fixture().await;
    let tools = ToolRepository::new(fixture.db.clone());
    let conversation = Uuid::now_v7();
    tools
        .start_conversation(
            conversation,
            fixture.recording_id,
            fixture.revision_id,
            Timestamp::from_millis(40),
        )
        .await
        .unwrap();
    tools
        .append_message(
            conversation,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 0,
                role: "user".to_string(),
                text: "what is the budget?".to_string(),
                source_anchors_json: "[]".to_string(),
                outcome: None,
                tool_run_id: None,
                created_at: Timestamp::from_millis(41),
            },
        )
        .await
        .unwrap();
    tools
        .append_message(
            conversation,
            &StoredMessage {
                id: Uuid::now_v7(),
                seq_index: 1,
                role: "assistant".to_string(),
                text: "The recording does not say.".to_string(),
                source_anchors_json: "[]".to_string(),
                outcome: Some("insufficient_evidence".to_string()),
                tool_run_id: None,
                created_at: Timestamp::from_millis(42),
            },
        )
        .await
        .unwrap();

    assert_eq!(tools.messages(conversation).await.unwrap().len(), 2);
    fixture.db.close().await;
}
