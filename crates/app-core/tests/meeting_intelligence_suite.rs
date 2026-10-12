//! End-to-end proof for EPIC I: a multi-segment recording is summarized, its
//! anchors resolve to real text ranges in the immutable revision, a transcript
//! edit marks the output stale without mutating it, and regeneration reuses
//! the untouched section while recomputing only the edited one.
//!
//! Hermetic: a scripted text model, a temporary SQLite library, no network.

use std::sync::Arc;

use parking_lot::Mutex;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{AvailabilityReason, PrivacyClass, TextModelCapabilities};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_ai_contracts::validation::{RepairPolicy, SpeakerDirectory};
use plaudon_app_core::chunking::{ChunkPolicy, UnknownContext};
use plaudon_app_core::pipeline::staleness::{
    plan_regeneration, DependencyGraph, OutputDependency, StaleReason,
};
use plaudon_app_core::pipeline::{
    canonical_range_digest, canonical_transcript_digest, PipelineRequest, SteppingClock,
    SummarizationPipeline,
};
use plaudon_app_core::templates::TemplateRegistry;
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::claim::{ClaimState, HumanConfirmation};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::output::{OutputStatus, StageKind};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};
use plaudon_persistence::intelligence::OutputRepository;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::Database;

const MEETING: &[&str] = &[
    "Ana: Right, let's start with the Harrier release.",
    "Bevan: We agreed to ship the Harrier release on Friday.",
    "Ana: I will send the revised quote to Northwind on Monday.",
    "Bevan: The scaffold inspection is overdue, which is a safety risk.",
    "Ana: Who owns the migration plan? Nobody has picked it up yet.",
    "Bevan: Let's close there.",
];

fn revision(texts: &[&str], parent: Option<&TranscriptRevision>) -> TranscriptRevision {
    let segments: Vec<TranscriptSegment> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let seq = u32::try_from(index).unwrap();
            TranscriptSegment::timed(
                seq,
                u64::from(seq) * 6_000,
                u64::from(seq) * 6_000 + 5_500,
                *text,
            )
        })
        .collect();
    TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: parent.map_or_else(RecordingId::new, |rev| rev.recording_id),
        parent_id: parent.map(|rev| rev.id),
        origin: parent.map_or(RevisionOrigin::Machine, |_| RevisionOrigin::HumanEdit),
        input_sha256: Sha256Digest::of(b"meeting-audio"),
        engine: EngineId::new("whisper.cpp"),
        model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
        locale: None,
        alignment_origin: AlignmentOrigin::Native,
        status: RevisionStatus::Complete,
        normalization_profile: "nfc".into(),
        chunking_strategy: "single".into(),
        duration_ms: Some(36_000),
        created_at: Timestamp::from_millis(0),
        segments,
    })
    .expect("fixture revision is valid")
}

fn capabilities(context_tokens: u64) -> TextModelCapabilities {
    let engine = EngineId::new("test-text-engine");
    let mut caps = TextModelCapabilities::unknown(
        engine.clone(),
        ModelIdentity::new(engine, "test-model").with_version("2026-01"),
    );
    caps.status = CapabilityStatus::Supported;
    caps.availability_reason = AvailabilityReason::Available;
    caps.offline = Tri::Yes;
    caps.supports_structured_output = Tri::Yes;
    caps.can_cancel = Tri::Yes;
    caps.max_context_tokens = Known::Known(context_tokens);
    caps.max_output_tokens = Known::Known(1_024);
    caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
    caps
}

#[derive(Debug)]
struct ScriptedModel {
    capabilities: TextModelCapabilities,
    replies: Mutex<std::collections::VecDeque<serde_json::Value>>,
    calls: Mutex<usize>,
}

impl ScriptedModel {
    fn new(replies: Vec<serde_json::Value>) -> Self {
        Self {
            capabilities: capabilities(100_000),
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
        }
    }

    fn calls(&self) -> usize {
        *self.calls.lock()
    }
}

#[async_trait]
impl TextModel for ScriptedModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        self.capabilities.clone()
    }

    async fn generate_structured(
        &self,
        _request: StructuredRequest,
        _ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        *self.calls.lock() += 1;
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

fn anchor(revision: &TranscriptRevision, seq: u32, chars: u32) -> serde_json::Value {
    let text: String = revision.segments()[seq as usize]
        .text
        .chars()
        .take(chars as usize)
        .collect();
    serde_json::json!({
        "revision_id": revision.id.to_string(),
        "seq_index": seq,
        "char_start": 0,
        "char_end": chars,
        "excerpt": text,
    })
}

fn summary(revision: &TranscriptRevision, first: u32, last: u32) -> serde_json::Value {
    serde_json::json!({
        "bullets": [{
            "text": "The team agreed to ship on Friday and raised a safety concern.",
            "anchors": [anchor(revision, first, 20)]
        }],
        "covered_first_segment": first,
        "covered_last_segment": last,
    })
}

fn extraction(revision: &TranscriptRevision) -> serde_json::Value {
    serde_json::json!({
        "executive_summary": [{
            "kind": "summary_statement",
            "text": "We agreed to ship the Harrier release on Friday.",
            "facet": "outcome",
            "anchors": [anchor(revision, 1, 55)]
        }],
        "decisions": [{
            "kind": "decision",
            "statement": "We agreed to ship the Harrier release on Friday",
            "participants": [],
            "time_reference": "not_specified",
            "assertion": "directly_stated",
            "anchors": [anchor(revision, 1, 55)]
        }],
        "action_items": [{
            "kind": "action_item",
            "verb": "send",
            "description": "the revised quote to Northwind",
            "owner": "unassigned",
            "due_date": "not_specified",
            "status": "open",
            "assertion": "directly_stated",
            "anchors": [anchor(revision, 2, 58)]
        }],
        "risks": [{
            "kind": "risk",
            "description": "The scaffold inspection is overdue, which is a safety risk",
            "consequence": "not_specified",
            "mitigation": "not_specified",
            "anchors": [anchor(revision, 3, 66)]
        }],
        "questions": [{
            "kind": "question",
            "question": "Who owns the migration plan?",
            "owner": "unassigned",
            "anchors": [anchor(revision, 4, 33)]
        }],
        "topics": []
    })
}

fn request(revision: &TranscriptRevision, registry: &TemplateRegistry) -> PipelineRequest {
    let template = registry.get("general_meeting").unwrap().clone();
    let extraction_schema = registry.extraction_schema(&template).unwrap();
    PipelineRequest {
        recording_id: revision.recording_id,
        revision: revision.clone(),
        template,
        extraction_schema,
        summary_schema: registry.summary_schema(),
        capabilities: capabilities(100_000),
        chunk_policy: ChunkPolicy {
            reserved_output_tokens: 64,
            prompt_overhead_tokens: 64,
            overlap_segments: 1,
            unknown_context: UnknownContext::FailClosed,
        },
        repair_policy: RepairPolicy::DEFAULT,
        speakers: SpeakerDirectory::unknown(),
        reusable_sections: Vec::new(),
        privacy_policy_snapshot: Some("on-device".into()),
        audio_sha256: Sha256Digest::of(b"meeting-audio"),
    }
}

async fn library() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(dir.path().join("app.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");
    (dir, db)
}

/// Insert the recording and immutable revision an output references.
///
/// `ai_outputs` and `pipeline_stage_runs` both carry restricting foreign keys,
/// so a generated output cannot exist without the meeting and the exact
/// revision it was produced from.
async fn seed(db: &Database, revision: &TranscriptRevision) {
    sqlx::query(
        "INSERT OR IGNORE INTO recordings (id, title, captured_at, duration_ms, created_at)
         VALUES (?1, 'Fixture meeting', 0, ?2, 0)",
    )
    .bind(revision.recording_id.to_string())
    .bind(i64::try_from(revision.duration_ms.unwrap_or(0)).unwrap_or(0))
    .execute(db.pool())
    .await
    .expect("seed recording");

    sqlx::query(
        "INSERT OR IGNORE INTO transcript_revisions (
             id, recording_id, parent_id, origin, input_sha256, engine, model_id,
             alignment_origin, status, normalization_profile, chunking_strategy,
             duration_ms, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'whisper.cpp', 'base.en', 'native', 'complete',
                   'nfc', 'single', ?6, 0)",
    )
    .bind(revision.id.to_string())
    .bind(revision.recording_id.to_string())
    .bind(revision.parent_id.map(|id| id.to_string()))
    .bind(match revision.origin {
        RevisionOrigin::Machine => "machine",
        RevisionOrigin::HumanEdit => "human_edit",
        RevisionOrigin::DerivedAlignment => "derived_alignment",
        RevisionOrigin::OverlapReconciliation => "overlap_reconciliation",
    })
    .bind(revision.input_sha256.to_hex())
    .bind(i64::try_from(revision.duration_ms.unwrap_or(0)).unwrap_or(0))
    .execute(db.pool())
    .await
    .expect("seed revision");
}

#[tokio::test]
async fn a_multi_segment_recording_is_summarized_stored_and_anchored_to_real_text() {
    let (_dir, db) = library().await;
    let registry = TemplateRegistry::with_builtins();
    let base = revision(MEETING, None);
    seed(&db, &base).await;

    let model = Arc::new(ScriptedModel::new(vec![
        summary(&base, 0, 5),
        extraction(&base),
    ]));
    let pipeline = SummarizationPipeline::new(model, Arc::new(SteppingClock::new(1_000, 10)));
    let outcome = pipeline
        .run(request(&base, &registry), &JobContext::new(JobId::new()))
        .await
        .expect("the run succeeds");

    // Every anchor resolves to the exact characters it names in the revision.
    assert_eq!(outcome.claims.len(), 5);
    for claim in &outcome.claims {
        assert!(!claim.anchors.is_empty(), "every claim carries an anchor");
        for anchor in &claim.anchors {
            let verified = anchor.verified().expect("anchor resolves");
            let segment = &base.segments()[verified.seq_index as usize];
            let expected: String = segment
                .text
                .chars()
                .skip(verified.char_start as usize)
                .take((verified.char_end - verified.char_start) as usize)
                .collect();
            assert_eq!(verified.matched_text, expected);
            assert_eq!(
                verified.seek_ms,
                segment
                    .start
                    .map(plaudon_domain::timerange::SourceTimeMs::get)
            );
        }
    }
    assert_eq!(outcome.grounding.verified_anchors, 5);
    assert_eq!(outcome.grounding.anchor_correctness(), Some(1.0));

    // The safety risk is high-consequence and therefore review-required; no
    // claim is verified until a human says so.
    assert!(outcome
        .claims
        .iter()
        .any(|claim| claim.review_required() && claim.text.contains("safety")));
    assert_eq!(
        outcome.verified_claims(),
        [] as [&plaudon_domain::claim::Claim; 0]
    );
    assert_eq!(outcome.status(), OutputStatus::NeedsReview);

    // Persisting the run keeps every stage's provenance.
    let repo = OutputRepository::new(db.clone());
    let stored = outcome.to_generated_output("meeting_summary");
    repo.save(&stored).await.expect("save");

    let headers = repo.headers_for(base.recording_id).await.expect("headers");
    assert_eq!(headers.len(), 1);
    assert!(!headers[0].is_stale());
    assert_eq!(headers[0].template_version, "1.0.0");
    assert_eq!(
        repo.stages_for(outcome.output_id).await.unwrap(),
        vec![
            StageKind::Normalize,
            StageKind::SectionSummary,
            StageKind::ConsolidatedSummary,
            StageKind::StructuredExtraction,
            StageKind::CitationVerification,
        ]
    );

    let claims = repo.claims_for(outcome.output_id).await.expect("claims");
    assert_eq!(claims.len(), 5);
    assert!(claims.iter().all(plaudon_domain::claim::Claim::is_grounded));
    assert!(claims
        .iter()
        .all(|claim| claim.state() == ClaimState::SourceLinked));
}

#[tokio::test]
async fn a_transcript_edit_marks_the_output_stale_and_targets_regeneration() {
    let (_dir, db) = library().await;
    let registry = TemplateRegistry::with_builtins();
    let base = revision(MEETING, None);
    seed(&db, &base).await;
    let repo = OutputRepository::new(db.clone());

    let model = Arc::new(ScriptedModel::new(vec![
        summary(&base, 0, 5),
        extraction(&base),
    ]));
    let pipeline = SummarizationPipeline::new(model, Arc::new(SteppingClock::new(1_000, 10)));
    let first = pipeline
        .run(request(&base, &registry), &JobContext::new(JobId::new()))
        .await
        .expect("first run");
    repo.save(&first.to_generated_output("meeting_summary"))
        .await
        .expect("save");

    // A human confirms one claim. Regeneration must not erase that.
    let confirmed = first
        .claims
        .iter()
        .find(|claim| claim.item_path.starts_with("/decisions"))
        .expect("a decision exists");
    repo.review_claim(
        confirmed.id,
        &HumanConfirmation::accept("local", Timestamp::from_millis(5_000)),
    )
    .await
    .expect("review");

    // The user corrects one segment, producing a child revision.
    let mut edited_texts: Vec<String> = MEETING.iter().map(|s| (*s).to_owned()).collect();
    edited_texts[4] = "Ana: Who owns the migration plan? Bevan has picked it up.".to_owned();
    let edited_refs: Vec<&str> = edited_texts.iter().map(String::as_str).collect();
    let edited = revision(&edited_refs, Some(&base));
    assert_eq!(edited.parent_id, Some(base.id));
    seed(&db, &edited).await;

    let marked = repo
        .mark_stale(
            &[base.id],
            StaleReason::TranscriptEdited.as_str(),
            Timestamp::from_millis(6_000),
        )
        .await
        .expect("mark stale");
    assert_eq!(marked, vec![first.output_id]);

    // The stale output is preserved whole, including the human confirmation.
    let headers = repo.headers_for(base.recording_id).await.unwrap();
    assert!(headers[0].is_stale());
    assert_eq!(
        headers[0].stale.as_ref().map(|s| s.reason.as_str()),
        Some("transcript_edited")
    );
    let preserved = repo.claims_for(first.output_id).await.unwrap();
    let preserved_decision = preserved
        .iter()
        .find(|claim| claim.id == confirmed.id)
        .expect("the confirmed claim survives");
    assert_eq!(preserved_decision.state(), ClaimState::HumanConfirmed);

    // Marking twice does not overwrite the first marking.
    assert_eq!(
        repo.mark_stale(
            &[base.id],
            "transcript_replaced",
            Timestamp::from_millis(9_000)
        )
        .await
        .unwrap(),
        [] as [plaudon_domain::ids::OutputId; 0]
    );

    // Targeted regeneration: the stored section covers the whole transcript,
    // and segment 4 changed, so that section must be recomputed.
    let stored_sections = repo
        .section_summaries_for(first.output_id)
        .await
        .expect("sections");
    assert_eq!(stored_sections.len(), 1);
    let plan = plan_regeneration(
        first.output_id,
        &base,
        &edited,
        &stored_sections,
        canonical_range_digest,
    );
    assert_eq!(plan.changed_segments, vec![4]);
    assert!(!plan.reuses_anything());
    assert_eq!(plan.recompute_ranges, vec![0..6]);

    // Regeneration is explicit and produces a new output revision.
    let model = Arc::new(ScriptedModel::new(vec![
        summary(&edited, 0, 5),
        extraction(&edited),
    ]));
    let pipeline =
        SummarizationPipeline::new(model.clone(), Arc::new(SteppingClock::new(10_000, 10)));
    let mut regen = request(&edited, &registry);
    regen.reusable_sections = plan.reusable_sections.clone();
    let second = pipeline
        .run(regen, &JobContext::new(JobId::new()))
        .await
        .expect("regeneration");
    repo.save(&second.to_generated_output("meeting_summary"))
        .await
        .expect("save regenerated");
    repo.record_supersession(first.output_id, second.output_id)
        .await
        .expect("supersede");

    assert_ne!(second.output_id, first.output_id);
    assert_eq!(model.calls(), 2, "nothing was reusable, so both stages ran");

    let headers = repo.headers_for(base.recording_id).await.unwrap();
    assert_eq!(
        headers.len(),
        2,
        "the stale output is retained, not replaced"
    );
    assert!(headers[0].is_stale());
    assert!(!headers[1].is_stale());
    assert_eq!(headers[1].transcript_revision_id, edited.id);
}

#[tokio::test]
async fn an_untouched_section_is_reused_when_only_a_later_section_changes() {
    let registry = TemplateRegistry::with_builtins();
    let long: Vec<String> = (0..16)
        .map(|index| {
            format!("Speaker {index}: a reasonably long utterance about the plan and the budget.")
        })
        .collect();
    let refs: Vec<&str> = long.iter().map(String::as_str).collect();
    let base = revision(&refs, None);

    let mut narrow = request(&base, &registry);
    narrow.capabilities = capabilities(220);
    narrow.chunk_policy = ChunkPolicy {
        reserved_output_tokens: 32,
        prompt_overhead_tokens: 32,
        overlap_segments: 0,
        unknown_context: UnknownContext::FailClosed,
    };

    let budget = plaudon_app_core::chunking::ContextBudget::from_capabilities(
        &narrow.capabilities,
        &narrow.chunk_policy,
    )
    .expect("budget");
    let plan = plaudon_app_core::chunking::plan_chunks_for_revision(&base, &budget).expect("plan");
    assert!(plan.chunks.len() >= 2, "the fixture must chunk");

    let mut replies: Vec<serde_json::Value> = plan
        .chunks
        .iter()
        .map(|chunk| summary(&base, chunk.segments.start, chunk.segments.end - 1))
        .collect();
    replies.push(summary(&base, 0, 15));
    replies.push(serde_json::json!({
        "executive_summary": [], "decisions": [], "action_items": [],
        "risks": [], "questions": [], "topics": []
    }));
    let model = Arc::new(ScriptedModel::new(replies));
    let pipeline = SummarizationPipeline::new(model, Arc::new(SteppingClock::new(0, 1)));
    let first = pipeline
        .run(narrow.clone(), &JobContext::new(JobId::new()))
        .await
        .expect("first run");
    assert_eq!(first.section_summaries.len(), plan.chunks.len());

    // Edit the final segment only.
    let mut edited_texts = long.clone();
    let last = edited_texts.len() - 1;
    edited_texts[last].push_str(" And one correction.");
    let edited_refs: Vec<&str> = edited_texts.iter().map(String::as_str).collect();
    let edited = revision(&edited_refs, Some(&base));

    let regen_plan = plan_regeneration(
        first.output_id,
        &base,
        &edited,
        &first.section_summaries,
        canonical_range_digest,
    );
    assert_eq!(
        regen_plan.changed_segments,
        vec![u32::try_from(last).unwrap()]
    );
    assert!(
        regen_plan.reuses_anything(),
        "an earlier, untouched section must be reusable"
    );
    assert_eq!(
        regen_plan.recompute_ranges.len(),
        1,
        "only the section containing the edit is recomputed"
    );

    let reused = regen_plan.reusable_sections.len();
    let mut replies: Vec<serde_json::Value> = regen_plan
        .recompute_ranges
        .iter()
        .map(|range| summary(&edited, range.start, range.end - 1))
        .collect();
    replies.push(summary(&edited, 0, 15));
    replies.push(serde_json::json!({
        "executive_summary": [], "decisions": [], "action_items": [],
        "risks": [], "questions": [], "topics": []
    }));
    let model = Arc::new(ScriptedModel::new(replies));
    let pipeline = SummarizationPipeline::new(model.clone(), Arc::new(SteppingClock::new(0, 1)));
    let mut regen = request(&edited, &registry);
    regen.capabilities = narrow.capabilities.clone();
    regen.chunk_policy = narrow.chunk_policy;
    regen.reusable_sections = regen_plan.reusable_sections;
    let second = pipeline
        .run(regen, &JobContext::new(JobId::new()))
        .await
        .expect("regeneration");

    assert_eq!(
        model.calls(),
        plan.chunks.len() - reused + 2,
        "reused sections are not re-billed"
    );
    let reused_stages = second
        .stages
        .iter()
        .filter(|stage| stage.stage == StageKind::SectionSummary && stage.reused_from.is_some())
        .count();
    assert_eq!(reused_stages, reused);
    assert!(second
        .stages
        .iter()
        .all(plaudon_app_core::StageProvenance::is_complete));
}

#[tokio::test]
async fn the_dependency_graph_closure_matches_what_the_repository_marks() {
    let (_dir, db) = library().await;
    let registry = TemplateRegistry::with_builtins();
    let base = revision(MEETING, None);
    seed(&db, &base).await;
    let repo = OutputRepository::new(db.clone());

    let mut ids = Vec::new();
    for step in 0..3 {
        let model = Arc::new(ScriptedModel::new(vec![
            summary(&base, 0, 5),
            extraction(&base),
        ]));
        let pipeline =
            SummarizationPipeline::new(model, Arc::new(SteppingClock::new(1_000 * step, 10)));
        let outcome = pipeline
            .run(request(&base, &registry), &JobContext::new(JobId::new()))
            .await
            .expect("run");
        repo.save(&outcome.to_generated_output("meeting_summary"))
            .await
            .expect("save");
        ids.push(outcome.output_id);
    }

    let graph = DependencyGraph::from_edges(ids.iter().map(|id| OutputDependency {
        output_id: *id,
        transcript_revision_id: base.id,
        input_sha256: canonical_transcript_digest(&base),
        stale: false,
    }));
    let closure = graph.stale_closure(
        &[base.id],
        StaleReason::TranscriptEdited,
        Timestamp::from_millis(7_000),
    );
    let mut expected: Vec<_> = closure.iter().map(|m| m.output_id).collect();
    expected.sort_unstable();

    let mut marked = repo
        .mark_stale(
            &[base.id],
            StaleReason::TranscriptEdited.as_str(),
            Timestamp::from_millis(7_000),
        )
        .await
        .expect("mark");
    marked.sort_unstable();

    assert_eq!(marked, expected, "no dependent output is left unmarked");
    assert_eq!(marked.len(), 3);
}
