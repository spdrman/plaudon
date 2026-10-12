//! Schema and repository guarantees for generated outputs (spec 9.1, 9.6A).
//!
//! These assert the invariants the *database* enforces, so they still hold if
//! a future caller bypasses the repository.

use plaudon_domain::anchor::{AnchorRejection, AnchorVerification, SourceAnchor, VerifiedAnchor};
use plaudon_domain::claim::{
    Claim, ClaimCategory, ClaimDraft, ClaimKind, ClaimState, EntailmentCheck, GroundingSummary,
    HumanConfirmation,
};
use plaudon_domain::ids::{OutputId, RecordingId, StageRunId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::output::{
    CompatibilityMode, CoverageState, OutputStatus, StageKind, StageProvenance,
};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::intelligence::{GeneratedOutput, OutputRepository};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::{Database, PersistenceError};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Database,
    recording_id: RecordingId,
    revision_id: TranscriptRevisionId,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(dir.path().join("app.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");

    let recording_id = RecordingId::new();
    let revision_id = TranscriptRevisionId::new();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1, 'Fixture', 0)")
        .bind(recording_id.to_string())
        .execute(db.pool())
        .await
        .expect("recording");
    sqlx::query(
        "INSERT INTO transcript_revisions (
             id, recording_id, origin, input_sha256, engine, model_id, alignment_origin,
             status, normalization_profile, chunking_strategy, created_at
         ) VALUES (?1, ?2, 'machine', ?3, 'whisper.cpp', 'base.en', 'native', 'complete',
                   'nfc', 'single', 0)",
    )
    .bind(revision_id.to_string())
    .bind(recording_id.to_string())
    .bind(Sha256Digest::of(b"audio").to_hex())
    .execute(db.pool())
    .await
    .expect("revision");

    Fixture {
        _dir: dir,
        db,
        recording_id,
        revision_id,
    }
}

fn stage(
    revision_id: TranscriptRevisionId,
    kind: StageKind,
    sequence: u32,
    with_model: bool,
) -> StageProvenance {
    StageProvenance {
        id: StageRunId::new(),
        sequence,
        stage: kind,
        stage_version: kind.stage_version().to_owned(),
        pipeline_version: "1.0.0".into(),
        transcript_revision_id: revision_id,
        input_sha256: Sha256Digest::of(b"in"),
        output_sha256: Sha256Digest::of(b"out"),
        model: with_model
            .then(|| ModelIdentity::new(EngineId::new("test"), "model").with_version("1")),
        compatibility_mode: with_model.then_some(CompatibilityMode::SchemaEnforced),
        repair_attempts: 0,
        prompt_version: "1.0.0".into(),
        prompt_envelope_version: "1.0.0".into(),
        template_id: "general_meeting".into(),
        template_version: "1.0.0".into(),
        schema_version: "1.0.0".into(),
        segment_range: None,
        reused_from: None,
        started_at: Timestamp::from_millis(0),
        ended_at: Timestamp::from_millis(1),
    }
}

fn claim(revision_id: TranscriptRevisionId, verified: bool) -> Claim {
    let anchor = SourceAnchor::new(revision_id, 0, 0, 5, "Alpha");
    let verification = if verified {
        AnchorVerification::Verified(VerifiedAnchor {
            revision_id,
            seq_index: 0,
            char_start: 0,
            char_end: 5,
            matched_text: "Alpha".into(),
            alignment_origin: AlignmentOrigin::Native,
            seek_ms: Some(0),
        })
    } else {
        AnchorVerification::Rejected(AnchorRejection::SegmentNotFound { seq_index: 0 })
    };
    Claim::from_draft(
        ClaimDraft::new(
            "/decisions/0",
            ClaimKind::Decision,
            "Ship Friday",
            vec![anchor],
        ),
        vec![verification],
        ClaimCategory::General,
        EntailmentCheck::Supported,
        true,
    )
}

fn output(fixture: &Fixture, stages: Vec<StageProvenance>, claims: Vec<Claim>) -> GeneratedOutput {
    let grounding = GroundingSummary::of(&claims);
    GeneratedOutput {
        id: OutputId::new(),
        recording_id: fixture.recording_id,
        transcript_revision_id: fixture.revision_id,
        output_type: "meeting_summary".into(),
        pipeline_version: "1.0.0".into(),
        template_id: "general_meeting".into(),
        template_version: "1.0.0".into(),
        model_id: "test/model".into(),
        prompt_version: "1.0.0".into(),
        schema_version: "1.0.0".into(),
        input_sha256: Sha256Digest::of(b"in"),
        output_sha256: Sha256Digest::of(b"out"),
        body: serde_json::json!({"decisions": []}),
        status: OutputStatus::Complete,
        coverage: CoverageState::Full,
        compatibility_mode: CompatibilityMode::SchemaEnforced,
        grounding,
        rejected: Vec::new(),
        directives: serde_json::json!([]),
        stages,
        section_summaries: Vec::new(),
        claims,
        created_at: Timestamp::from_millis(10),
    }
}

#[tokio::test]
async fn a_saved_output_round_trips_with_its_claims_and_anchors() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(
        &fixture,
        vec![
            stage(fixture.revision_id, StageKind::Normalize, 0, false),
            stage(
                fixture.revision_id,
                StageKind::StructuredExtraction,
                1,
                true,
            ),
        ],
        vec![claim(fixture.revision_id, true)],
    );
    repo.save(&stored).await.expect("save");

    let headers = repo.headers_for(fixture.recording_id).await.unwrap();
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].status, OutputStatus::Complete);
    assert_eq!(headers[0].coverage, CoverageState::Full);
    assert!(!headers[0].is_stale());

    let claims = repo.claims_for(stored.id).await.unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].state(), ClaimState::SourceLinked);
    let anchor = &claims[0].anchors[0];
    let verified = anchor.verified().expect("anchor verified");
    assert_eq!(verified.matched_text, "Alpha");
    assert_eq!(verified.seek_ms, Some(0));
}

#[tokio::test]
async fn a_rejected_anchor_is_stored_with_its_reason_rather_than_dropped() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        vec![claim(fixture.revision_id, false)],
    );
    repo.save(&stored).await.expect("save");

    let claims = repo.claims_for(stored.id).await.unwrap();
    assert_eq!(claims[0].state(), ClaimState::Unverified);
    assert_eq!(
        claims[0].anchors[0].rejection().map(AnchorRejection::code),
        Some("segment_not_found")
    );
    assert!(claims[0].review_required());
}

#[tokio::test]
async fn an_inference_stage_without_provenance_is_refused_before_any_write() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(
        &fixture,
        vec![stage(
            fixture.revision_id,
            StageKind::StructuredExtraction,
            0,
            false,
        )],
        Vec::new(),
    );
    assert!(matches!(
        repo.save(&stored).await,
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(
        repo.headers_for(fixture.recording_id).await.unwrap(),
        [] as [plaudon_persistence::intelligence::OutputHeader; 0]
    );
}

#[tokio::test]
async fn an_output_with_no_stage_is_refused() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(&fixture, Vec::new(), Vec::new());
    assert!(matches!(
        repo.save(&stored).await,
        Err(PersistenceError::Invalid(_))
    ));
}

#[tokio::test]
async fn the_schema_refuses_a_human_confirmed_claim_without_a_reviewer() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        Vec::new(),
    );
    repo.save(&stored).await.expect("save");

    let result = sqlx::query(
        "INSERT INTO output_claims (
             id, output_id, item_path, kind, category, claim_text, state, entailment
         ) VALUES (?1, ?2, '/decisions/0', 'decision', 'general', 'x', 'human_confirmed',
                   'not_checked')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(stored.id.to_string())
    .execute(fixture.db.pool())
    .await;
    assert!(
        result.is_err(),
        "only an explicit human review may produce human_confirmed"
    );
}

#[tokio::test]
async fn an_explicit_review_is_the_only_path_to_human_confirmed() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let claim_row = claim(fixture.revision_id, true);
    let claim_id = claim_row.id;
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        vec![claim_row],
    );
    repo.save(&stored).await.expect("save");

    let state = repo
        .review_claim(
            claim_id,
            &HumanConfirmation::accept("local", Timestamp::from_millis(99)).with_note("checked"),
        )
        .await
        .expect("review");
    assert_eq!(state, ClaimState::HumanConfirmed);

    let reloaded = repo.claims_for(stored.id).await.unwrap();
    assert_eq!(reloaded[0].state(), ClaimState::HumanConfirmed);
    assert_eq!(
        reloaded[0].confirmation().map(|c| c.reviewer.as_str()),
        Some("local")
    );
    assert_eq!(
        reloaded[0].confirmation().and_then(|c| c.note.as_deref()),
        Some("checked")
    );
}

#[tokio::test]
async fn a_rejection_review_forces_unverified() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let claim_row = claim(fixture.revision_id, true);
    let claim_id = claim_row.id;
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        vec![claim_row],
    );
    repo.save(&stored).await.expect("save");

    let state = repo
        .review_claim(
            claim_id,
            &HumanConfirmation::reject("local", Timestamp::from_millis(99)),
        )
        .await
        .expect("review");
    assert_eq!(state, ClaimState::Unverified);
}

#[tokio::test]
async fn reviewing_an_unknown_claim_reports_not_found() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    assert!(matches!(
        repo.review_claim(
            plaudon_domain::ids::ClaimId::new(),
            &HumanConfirmation::accept("local", Timestamp::from_millis(1)),
        )
        .await,
        Err(PersistenceError::RecordNotFound)
    ));
}

#[tokio::test]
async fn a_stored_body_cannot_be_rewritten_in_place() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        Vec::new(),
    );
    repo.save(&stored).await.expect("save");

    let result =
        sqlx::query("UPDATE ai_outputs SET body_json = '{\"decisions\":[1]}' WHERE id = ?1")
            .bind(stored.id.to_string())
            .execute(fixture.db.pool())
            .await;
    assert!(
        result.is_err(),
        "regeneration must create a new row, never rewrite one"
    );
}

#[tokio::test]
async fn staleness_requires_both_an_instant_and_a_reason() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        Vec::new(),
    );
    repo.save(&stored).await.expect("save");

    let result = sqlx::query("UPDATE ai_outputs SET stale_at = 1 WHERE id = ?1")
        .bind(stored.id.to_string())
        .execute(fixture.db.pool())
        .await;
    assert!(result.is_err(), "a stale output always records why");

    let marked = repo
        .mark_stale(
            &[fixture.revision_id],
            "transcript_edited",
            Timestamp::from_millis(42),
        )
        .await
        .expect("mark");
    assert_eq!(marked, vec![stored.id]);

    let headers = repo.headers_for(fixture.recording_id).await.unwrap();
    assert_eq!(
        headers[0].stale.as_ref().map(|s| s.stale_at),
        Some(Timestamp::from_millis(42))
    );
}

#[tokio::test]
async fn marking_an_unrelated_revision_marks_nothing() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        Vec::new(),
    );
    repo.save(&stored).await.expect("save");
    assert_eq!(
        repo.mark_stale(
            &[TranscriptRevisionId::new()],
            "transcript_edited",
            Timestamp::from_millis(1)
        )
        .await
        .unwrap(),
        [] as [plaudon_domain::ids::OutputId; 0]
    );
    assert_eq!(
        repo.mark_stale(&[], "x", Timestamp::from_millis(1))
            .await
            .unwrap(),
        [] as [plaudon_domain::ids::OutputId; 0]
    );
}

#[tokio::test]
async fn an_anchor_may_not_carry_a_seek_position_without_alignment() {
    let fixture = fixture().await;
    let repo = OutputRepository::new(fixture.db.clone());
    let claim_row = claim(fixture.revision_id, true);
    let claim_id = claim_row.id;
    let stored = output(
        &fixture,
        vec![stage(fixture.revision_id, StageKind::Normalize, 0, false)],
        vec![claim_row],
    );
    repo.save(&stored).await.expect("save");

    let result = sqlx::query(
        "INSERT INTO output_evidence (
             id, output_id, claim_id, position, item_path, cited_revision_id, seq_index,
             char_start, char_end, quote, verification_status, matched_text,
             alignment_origin, seek_ms
         ) VALUES (?1, ?2, ?3, 9, '/decisions/0', ?4, 0, 0, 5, 'Alpha', 'verified', 'Alpha',
                   'none', 1234)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(stored.id.to_string())
    .bind(claim_id.to_string())
    .bind(fixture.revision_id.to_string())
    .execute(fixture.db.pool())
    .await;
    assert!(result.is_err(), "spec 8.6A forbids an invented timeline");
}
