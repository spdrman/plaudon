//! Generated-output persistence: provenance, claims, anchors and the
//! staleness dependency graph (spec 9.1, 9.6A, 13.2, 13.3; tasks I1.1, I2.4).
//!
//! Three invariants are enforced below the application, by the schema:
//!
//! * a stage row cannot exist without its versions, and an inference stage
//!   cannot exist without either a model or a reuse link;
//! * a claim cannot be `human_confirmed` without a recorded reviewer; and
//! * a stored output body is immutable — regeneration writes a new row, so a
//!   previously reviewed decision is never silently rewritten.
//!
//! The repository adds the transactional parts: an output and all of its
//! stages, claims, anchors and dependency edges commit together or not at all,
//! and staleness marking is a single statement over the dependency graph.

use plaudon_ai_contracts::validation::RejectedItem;
use plaudon_domain::anchor::{AnchorRejection, AnchorVerification, SourceAnchor, VerifiedAnchor};
use plaudon_domain::claim::{
    Claim, ClaimAnchor, ClaimCategory, ClaimKind, ClaimState, EntailmentCheck, GroundingSummary,
    HumanConfirmation, ReviewReason, StoredClaim,
};
use plaudon_domain::ids::{ClaimId, OutputId, RecordingId, StageRunId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::output::{
    CompatibilityMode, CoverageState, OutputStatus, SectionSummary, StageKind, StageProvenance,
    SummaryResponse,
};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use sqlx::Row;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// Everything one completed pipeline run contributes to the library.
///
/// The pipeline crate builds this from its own `PipelineOutcome`; the
/// repository never depends on the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedOutput {
    /// Output identity.
    pub id: OutputId,
    /// Recording the output belongs to.
    pub recording_id: RecordingId,
    /// Exact transcript revision every stage read.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Output type, e.g. `meeting_summary`.
    pub output_type: String,
    /// Pipeline version.
    pub pipeline_version: String,
    /// Template identity.
    pub template_id: String,
    /// Template version.
    pub template_version: String,
    /// Model identifier of the extraction stage.
    pub model_id: String,
    /// Prompt version.
    pub prompt_version: String,
    /// Output schema version.
    pub schema_version: String,
    /// Digest of the transcript content consumed.
    pub input_sha256: Sha256Digest,
    /// Digest of the canonical body.
    pub output_sha256: Sha256Digest,
    /// The accepted items.
    pub body: serde_json::Value,
    /// Lifecycle status.
    pub status: OutputStatus,
    /// Coverage of the transcript.
    pub coverage: CoverageState,
    /// Compatibility mode of the extraction stage.
    pub compatibility_mode: CompatibilityMode,
    /// Aggregate grounding statistics.
    pub grounding: GroundingSummary,
    /// Items the validator rejected.
    pub rejected: Vec<RejectedItem>,
    /// Apparent instructions found in the transcript, as opaque JSON.
    pub directives: serde_json::Value,
    /// Per-stage provenance.
    pub stages: Vec<StageProvenance>,
    /// Section summaries, keyed by the stage run that produced them.
    pub section_summaries: Vec<SectionSummary>,
    /// Claims with their grounding state.
    pub claims: Vec<Claim>,
    /// When the run finished.
    pub created_at: Timestamp,
}

/// Staleness state of a stored output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleState {
    /// When it was marked stale.
    pub stale_at: Timestamp,
    /// Why.
    pub reason: String,
}

/// A stored output header, without its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputHeader {
    /// Output identity.
    pub id: OutputId,
    /// Recording the output belongs to.
    pub recording_id: RecordingId,
    /// Transcript revision it read.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Output type.
    pub output_type: String,
    /// Template identity.
    pub template_id: String,
    /// Template version it ran with; never rewritten by a template upgrade.
    pub template_version: String,
    /// Pipeline version.
    pub pipeline_version: String,
    /// Lifecycle status.
    pub status: OutputStatus,
    /// Coverage claim.
    pub coverage: CoverageState,
    /// Staleness, when the output has been superseded.
    pub stale: Option<StaleState>,
    /// Creation instant.
    pub created_at: Timestamp,
}

impl OutputHeader {
    /// Whether the output needs regeneration before it can be trusted again.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.stale.is_some()
    }
}

/// Repository over generated outputs.
#[derive(Debug, Clone)]
pub struct OutputRepository {
    db: Database,
}

impl OutputRepository {
    /// Construct a repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Persist a completed run atomically.
    ///
    /// Rejects the write before touching the database when any stage carries
    /// incomplete provenance: spec 9.1 has no "stage without versions" state,
    /// and the repository is the last place that can still refuse one.
    pub async fn save(&self, output: &GeneratedOutput) -> PersistenceResult<()> {
        if let Some(bad) = output.stages.iter().find(|s| !s.is_complete()) {
            return Err(PersistenceError::Invalid(format!(
                "stage {} of output {} has incomplete provenance",
                bad.stage.as_str(),
                output.id
            )));
        }
        if output.stages.is_empty() {
            return Err(PersistenceError::Invalid(format!(
                "output {} records no stage",
                output.id
            )));
        }

        let mut tx = self.db.begin_write().await?;

        sqlx::query(
            "INSERT INTO ai_outputs (
                 id, recording_id, output_type, transcript_revision_id, template_version,
                 model_id, prompt_version, schema_version, body_json, status, created_at,
                 pipeline_version, template_id, input_sha256, output_sha256,
                 compatibility_mode, coverage_state, coverage_json, grounding_json,
                 rejected_json, directives_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
        )
        .bind(output.id.to_string())
        .bind(output.recording_id.to_string())
        .bind(&output.output_type)
        .bind(output.transcript_revision_id.to_string())
        .bind(&output.template_version)
        .bind(&output.model_id)
        .bind(&output.prompt_version)
        .bind(&output.schema_version)
        .bind(json_text(&output.body)?)
        .bind(output.status.as_str())
        .bind(output.created_at.as_millis())
        .bind(&output.pipeline_version)
        .bind(&output.template_id)
        .bind(output.input_sha256.to_hex())
        .bind(output.output_sha256.to_hex())
        .bind(output.compatibility_mode.as_str())
        .bind(output.coverage.as_str())
        .bind(json_text(&output.coverage)?)
        .bind(json_text(&output.grounding)?)
        .bind(json_text(&output.rejected)?)
        .bind(json_text(&output.directives)?)
        .execute(&mut *tx)
        .await?;

        for stage in &output.stages {
            let section = output
                .section_summaries
                .iter()
                .find(|section| section.stage_run_id == stage.id);
            sqlx::query(
                "INSERT INTO pipeline_stage_runs (
                     id, output_id, sequence, stage, stage_version, pipeline_version,
                     transcript_revision_id, input_sha256, output_sha256, engine, model_id,
                     model_version, compatibility_mode, repair_attempts, prompt_version,
                     prompt_envelope_version, template_id, template_version, schema_version,
                     segment_first, segment_last, chunk_index, section_response_json,
                     reused_from, started_at, ended_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,
                           ?20,?21,?22,?23,?24,?25,?26)",
            )
            .bind(stage.id.to_string())
            .bind(output.id.to_string())
            .bind(i64::from(stage.sequence))
            .bind(stage.stage.as_str())
            .bind(&stage.stage_version)
            .bind(&stage.pipeline_version)
            .bind(stage.transcript_revision_id.to_string())
            .bind(stage.input_sha256.to_hex())
            .bind(stage.output_sha256.to_hex())
            .bind(stage.model.as_ref().map(|m| m.engine.as_str().to_owned()))
            .bind(stage.model.as_ref().map(|m| m.model_id.clone()))
            .bind(stage.model.as_ref().and_then(|m| m.model_version.clone()))
            .bind(stage.compatibility_mode.map(CompatibilityMode::as_str))
            .bind(i64::from(stage.repair_attempts))
            .bind(&stage.prompt_version)
            .bind(&stage.prompt_envelope_version)
            .bind(&stage.template_id)
            .bind(&stage.template_version)
            .bind(&stage.schema_version)
            .bind(stage.segment_range.as_ref().map(|r| i64::from(r.start)))
            .bind(stage.segment_range.as_ref().map(|r| i64::from(r.end) - 1))
            .bind(section.map(|s| i64::from(s.chunk_index)))
            .bind(section.map(|s| json_text(&s.response)).transpose()?)
            .bind(stage.reused_from.map(|id| id.to_string()))
            .bind(stage.started_at.as_millis())
            .bind(stage.ended_at.as_millis())
            .execute(&mut *tx)
            .await?;
        }

        for claim in &output.claims {
            let confirmation = claim.confirmation();
            sqlx::query(
                "INSERT INTO output_claims (
                     id, output_id, item_path, kind, category, claim_text, state, entailment,
                     review_reasons_json, reviewer, reviewed_at, review_accepted, review_note
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            )
            .bind(claim.id.to_string())
            .bind(output.id.to_string())
            .bind(&claim.item_path)
            .bind(claim.kind.as_str())
            .bind(claim.category.as_str())
            .bind(&claim.text)
            .bind(claim.state().as_str())
            .bind(entailment_str(claim.entailment))
            .bind(json_text(&claim.review_reasons())?)
            .bind(confirmation.map(|c| c.reviewer.clone()))
            .bind(confirmation.map(|c| c.reviewed_at.as_millis()))
            .bind(confirmation.map(|c| i64::from(c.accepted)))
            .bind(confirmation.and_then(|c| c.note.clone()))
            .execute(&mut *tx)
            .await?;

            for (position, anchor) in claim.anchors.iter().enumerate() {
                let (status, rejection, matched, origin, seek) = match &anchor.verification {
                    AnchorVerification::Verified(verified) => (
                        "verified",
                        None,
                        Some(verified.matched_text.clone()),
                        Some(alignment_str(verified.alignment_origin)),
                        verified.seek_ms,
                    ),
                    AnchorVerification::Rejected(rejection) => {
                        ("rejected", Some(rejection.code()), None, None, None)
                    }
                };
                sqlx::query(
                    "INSERT INTO output_evidence (
                         id, output_id, claim_id, position, item_path, cited_revision_id,
                         seq_index, char_start, char_end, quote, verification_status,
                         rejection_code, matched_text, alignment_origin, seek_ms
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                )
                .bind(uuid::Uuid::now_v7().to_string())
                .bind(output.id.to_string())
                .bind(claim.id.to_string())
                .bind(i64::try_from(position).unwrap_or(i64::MAX))
                .bind(&claim.item_path)
                .bind(anchor.anchor.revision_id.to_string())
                .bind(i64::from(anchor.anchor.seq_index))
                .bind(i64::from(anchor.anchor.char_start))
                .bind(i64::from(anchor.anchor.char_end))
                .bind(&anchor.anchor.excerpt)
                .bind(status)
                .bind(rejection)
                .bind(matched)
                .bind(origin)
                .bind(seek.map(|ms| i64::try_from(ms).unwrap_or(i64::MAX)))
                .execute(&mut *tx)
                .await?;
            }
        }

        sqlx::query(
            "INSERT INTO output_dependencies (output_id, transcript_revision_id, input_sha256)
             VALUES (?1, ?2, ?3)",
        )
        .bind(output.id.to_string())
        .bind(output.transcript_revision_id.to_string())
        .bind(output.input_sha256.to_hex())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Headers of every output of a recording, newest last.
    pub async fn headers_for(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<OutputHeader>> {
        let rows = sqlx::query(
            "SELECT id, recording_id, transcript_revision_id, output_type, template_id,
                    template_version, pipeline_version, status, coverage_state, coverage_json,
                    stale_at, stale_reason, created_at
             FROM ai_outputs WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(header_from_row).collect()
    }

    /// Claims of one output, in item-path order.
    pub async fn claims_for(&self, output_id: OutputId) -> PersistenceResult<Vec<Claim>> {
        let rows = sqlx::query(
            "SELECT id, item_path, kind, category, claim_text, state, entailment,
                    review_reasons_json, reviewer, reviewed_at, review_accepted, review_note
             FROM output_claims WHERE output_id = ?1 ORDER BY item_path",
        )
        .bind(output_id.to_string())
        .fetch_all(self.db.pool())
        .await?;

        let mut claims = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            let claim_id = parse_id::<ClaimId>(&id)?;
            let anchors = self.anchors_for(claim_id).await?;
            let reviewer: Option<String> = row.get("reviewer");
            let reviewed_at: Option<i64> = row.get("reviewed_at");
            let accepted: Option<i64> = row.get("review_accepted");
            let confirmation = match (reviewer, reviewed_at, accepted) {
                (Some(reviewer), Some(at), Some(accepted)) => Some(HumanConfirmation {
                    reviewer,
                    reviewed_at: Timestamp::from_millis(at),
                    accepted: accepted == 1,
                    note: row.get("review_note"),
                }),
                _ => None,
            };
            let reasons: Vec<ReviewReason> = serde_json::from_str::<Vec<ReviewReason>>(
                &row.get::<String, _>("review_reasons_json"),
            )
            .map_err(|e| PersistenceError::Invalid(format!("review reasons: {e}")))?;

            claims.push(Claim::from_stored(StoredClaim {
                id: claim_id,
                item_path: row.get("item_path"),
                kind: claim_kind(&row.get::<String, _>("kind"))?,
                category: ClaimCategory::from_str_opt(&row.get::<String, _>("category"))
                    .ok_or_else(|| PersistenceError::Invalid("unknown claim category".into()))?,
                text: row.get("claim_text"),
                anchors,
                entailment: entailment(&row.get::<String, _>("entailment"))?,
                state: claim_state(&row.get::<String, _>("state"))?,
                review_reasons: reasons,
                confirmation,
            }));
        }
        Ok(claims)
    }

    async fn anchors_for(&self, claim_id: ClaimId) -> PersistenceResult<Vec<ClaimAnchor>> {
        let rows = sqlx::query(
            "SELECT cited_revision_id, seq_index, char_start, char_end, quote,
                    verification_status, rejection_code, matched_text, alignment_origin, seek_ms
             FROM output_evidence WHERE claim_id = ?1 ORDER BY position",
        )
        .bind(claim_id.to_string())
        .fetch_all(self.db.pool())
        .await?;

        rows.iter()
            .map(|row| {
                let revision_id =
                    parse_id::<TranscriptRevisionId>(&row.get::<String, _>("cited_revision_id"))?;
                let seq_index = u32::try_from(row.get::<i64, _>("seq_index")).unwrap_or(u32::MAX);
                let char_start = u32::try_from(row.get::<i64, _>("char_start")).unwrap_or(0);
                let char_end = u32::try_from(row.get::<i64, _>("char_end")).unwrap_or(0);
                let anchor = SourceAnchor {
                    revision_id,
                    seq_index,
                    char_start,
                    char_end,
                    excerpt: row.get("quote"),
                };
                let verification = if row.get::<String, _>("verification_status") == "verified" {
                    AnchorVerification::Verified(VerifiedAnchor {
                        revision_id,
                        seq_index,
                        char_start,
                        char_end,
                        matched_text: row
                            .get::<Option<String>, _>("matched_text")
                            .unwrap_or_default(),
                        alignment_origin: alignment(
                            &row.get::<Option<String>, _>("alignment_origin")
                                .unwrap_or_else(|| "none".into()),
                        )?,
                        seek_ms: row
                            .get::<Option<i64>, _>("seek_ms")
                            .map(|ms| u64::try_from(ms).unwrap_or(0)),
                    })
                } else {
                    AnchorVerification::Rejected(rejection_from_code(
                        &row.get::<Option<String>, _>("rejection_code")
                            .unwrap_or_else(|| "excerpt_mismatch".into()),
                        seq_index,
                    ))
                };
                Ok(ClaimAnchor {
                    anchor,
                    verification,
                })
            })
            .collect()
    }

    /// Section summaries of an output, reusable by a targeted regeneration.
    pub async fn section_summaries_for(
        &self,
        output_id: OutputId,
    ) -> PersistenceResult<Vec<SectionSummary>> {
        let rows = sqlx::query(
            "SELECT id, chunk_index, segment_first, segment_last, input_sha256,
                    section_response_json
             FROM pipeline_stage_runs
             WHERE output_id = ?1 AND stage = 'section_summary'
               AND section_response_json IS NOT NULL
             ORDER BY sequence",
        )
        .bind(output_id.to_string())
        .fetch_all(self.db.pool())
        .await?;

        rows.iter()
            .map(|row| {
                let response: SummaryResponse =
                    serde_json::from_str(&row.get::<String, _>("section_response_json"))
                        .map_err(|e| PersistenceError::Invalid(format!("section response: {e}")))?;
                let first = u32::try_from(row.get::<i64, _>("segment_first")).unwrap_or(0);
                let last = u32::try_from(row.get::<i64, _>("segment_last")).unwrap_or(0);
                Ok(SectionSummary {
                    stage_run_id: parse_id::<StageRunId>(&row.get::<String, _>("id"))?,
                    chunk_index: u32::try_from(row.get::<i64, _>("chunk_index")).unwrap_or(0),
                    segments: first..last + 1,
                    input_sha256: digest(&row.get::<String, _>("input_sha256"))?,
                    response,
                })
            })
            .collect()
    }

    /// Stage provenance of an output, in execution order.
    pub async fn stages_for(&self, output_id: OutputId) -> PersistenceResult<Vec<StageKind>> {
        let rows = sqlx::query(
            "SELECT stage FROM pipeline_stage_runs WHERE output_id = ?1 ORDER BY sequence",
        )
        .bind(output_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter()
            .map(|row| {
                StageKind::from_str_opt(&row.get::<String, _>("stage"))
                    .ok_or_else(|| PersistenceError::Invalid("unknown stage".into()))
            })
            .collect()
    }

    /// Mark every output that read any of `superseded` as stale.
    ///
    /// Returns the outputs newly marked. Already-stale outputs are untouched,
    /// so the original `stale_at` and reason survive a second edit, and no
    /// output body, claim state or human confirmation is modified.
    pub async fn mark_stale(
        &self,
        superseded: &[TranscriptRevisionId],
        reason: &str,
        at: Timestamp,
    ) -> PersistenceResult<Vec<OutputId>> {
        if superseded.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.db.begin_write().await?;
        let mut marked = Vec::new();
        for revision in superseded {
            let rows = sqlx::query(
                "SELECT o.id AS id
                 FROM ai_outputs o
                 JOIN output_dependencies d ON d.output_id = o.id
                 WHERE d.transcript_revision_id = ?1 AND o.stale_at IS NULL",
            )
            .bind(revision.to_string())
            .fetch_all(&mut *tx)
            .await?;
            for row in rows {
                let id: String = row.get("id");
                sqlx::query(
                    "UPDATE ai_outputs SET stale_at = ?2, stale_reason = ?3
                     WHERE id = ?1 AND stale_at IS NULL",
                )
                .bind(&id)
                .bind(at.as_millis())
                .bind(reason)
                .execute(&mut *tx)
                .await?;
                marked.push(parse_id::<OutputId>(&id)?);
            }
        }
        tx.commit().await?;
        marked.sort_unstable();
        marked.dedup();
        Ok(marked)
    }

    /// Record an explicit human review of one claim (spec 9.6A).
    ///
    /// The reviewer and instant are supplied by the application, never by a
    /// model, which is what makes `human_confirmed` reachable at all.
    pub async fn review_claim(
        &self,
        claim_id: ClaimId,
        confirmation: &HumanConfirmation,
    ) -> PersistenceResult<ClaimState> {
        let state = if confirmation.accepted {
            ClaimState::HumanConfirmed
        } else {
            ClaimState::Unverified
        };
        let affected = sqlx::query(
            "UPDATE output_claims
             SET state = ?2, reviewer = ?3, reviewed_at = ?4, review_accepted = ?5,
                 review_note = ?6
             WHERE id = ?1",
        )
        .bind(claim_id.to_string())
        .bind(state.as_str())
        .bind(&confirmation.reviewer)
        .bind(confirmation.reviewed_at.as_millis())
        .bind(i64::from(confirmation.accepted))
        .bind(confirmation.note.clone())
        .execute(self.db.pool())
        .await?;
        if affected.rows_affected() == 0 {
            return Err(PersistenceError::RecordNotFound);
        }
        Ok(state)
    }

    /// Link a regenerated output to the one it replaces.
    ///
    /// The previous output keeps its body, claims and confirmations; only the
    /// forward pointer is added.
    pub async fn record_supersession(
        &self,
        previous: OutputId,
        replacement: OutputId,
    ) -> PersistenceResult<()> {
        sqlx::query("UPDATE ai_outputs SET superseded_by = ?2 WHERE id = ?1")
            .bind(previous.to_string())
            .bind(replacement.to_string())
            .execute(self.db.pool())
            .await?;
        Ok(())
    }
}

fn header_from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<OutputHeader> {
    let stale_at: Option<i64> = row.get("stale_at");
    let stale_reason: Option<String> = row.get("stale_reason");
    Ok(OutputHeader {
        id: parse_id::<OutputId>(&row.get::<String, _>("id"))?,
        recording_id: parse_id::<RecordingId>(&row.get::<String, _>("recording_id"))?,
        transcript_revision_id: parse_id::<TranscriptRevisionId>(
            &row.get::<String, _>("transcript_revision_id"),
        )?,
        output_type: row.get("output_type"),
        template_id: row.get("template_id"),
        template_version: row.get("template_version"),
        pipeline_version: row.get("pipeline_version"),
        status: OutputStatus::from_str_opt(&row.get::<String, _>("status"))
            .ok_or_else(|| PersistenceError::Invalid("unknown output status".into()))?,
        coverage: serde_json::from_str(&row.get::<String, _>("coverage_json"))
            .map_err(|e| PersistenceError::Invalid(format!("coverage: {e}")))?,
        stale: match (stale_at, stale_reason) {
            (Some(at), Some(reason)) => Some(StaleState {
                stale_at: Timestamp::from_millis(at),
                reason,
            }),
            _ => None,
        },
        created_at: Timestamp::from_millis(row.get::<i64, _>("created_at")),
    })
}

fn json_text<T: serde::Serialize>(value: &T) -> PersistenceResult<String> {
    serde_json::to_string(value).map_err(|e| PersistenceError::Invalid(format!("serialize: {e}")))
}

fn parse_id<T: std::str::FromStr>(raw: &str) -> PersistenceResult<T> {
    raw.parse::<T>()
        .map_err(|_| PersistenceError::Invalid(format!("malformed identifier {raw}")))
}

fn digest(raw: &str) -> PersistenceResult<Sha256Digest> {
    raw.parse::<Sha256Digest>()
        .map_err(|_| PersistenceError::Invalid("malformed digest".into()))
}

const fn entailment_str(value: EntailmentCheck) -> &'static str {
    match value {
        EntailmentCheck::Supported => "supported",
        EntailmentCheck::Unsupported => "unsupported",
        EntailmentCheck::NotChecked => "not_checked",
    }
}

fn entailment(raw: &str) -> PersistenceResult<EntailmentCheck> {
    match raw {
        "supported" => Ok(EntailmentCheck::Supported),
        "unsupported" => Ok(EntailmentCheck::Unsupported),
        "not_checked" => Ok(EntailmentCheck::NotChecked),
        _ => Err(PersistenceError::Invalid("unknown entailment".into())),
    }
}

fn claim_state(raw: &str) -> PersistenceResult<ClaimState> {
    match raw {
        "source_linked" => Ok(ClaimState::SourceLinked),
        "human_confirmed" => Ok(ClaimState::HumanConfirmed),
        "unverified" => Ok(ClaimState::Unverified),
        _ => Err(PersistenceError::Invalid("unknown claim state".into())),
    }
}

fn claim_kind(raw: &str) -> PersistenceResult<ClaimKind> {
    match raw {
        "summary_statement" => Ok(ClaimKind::SummaryStatement),
        "decision" => Ok(ClaimKind::Decision),
        "action_item" => Ok(ClaimKind::ActionItem),
        "risk" => Ok(ClaimKind::Risk),
        "question" => Ok(ClaimKind::Question),
        "topic" => Ok(ClaimKind::Topic),
        _ => Err(PersistenceError::Invalid("unknown claim kind".into())),
    }
}

const fn alignment_str(origin: AlignmentOrigin) -> &'static str {
    match origin {
        AlignmentOrigin::Native => "native",
        AlignmentOrigin::Derived => "derived",
        AlignmentOrigin::None => "none",
    }
}

fn alignment(raw: &str) -> PersistenceResult<AlignmentOrigin> {
    match raw {
        "native" => Ok(AlignmentOrigin::Native),
        "derived" => Ok(AlignmentOrigin::Derived),
        "none" => Ok(AlignmentOrigin::None),
        _ => Err(PersistenceError::Invalid("unknown alignment origin".into())),
    }
}

/// Rebuild a rejection from its persisted discriminator.
///
/// The structured detail of a rejection is not persisted: the code is the
/// stable contract, and re-verifying against the revision reproduces the rest.
fn rejection_from_code(code: &str, seq_index: u32) -> AnchorRejection {
    match code {
        "segment_not_found" => AnchorRejection::SegmentNotFound { seq_index },
        "span_out_of_bounds" => AnchorRejection::SpanOutOfBounds {
            seq_index,
            char_end: 0,
            segment_chars: 0,
        },
        "empty_span" => AnchorRejection::EmptySpan { seq_index },
        "span_too_long" => AnchorRejection::SpanTooLong {
            seq_index,
            chars: 0,
            max_chars: plaudon_domain::anchor::MAX_EXCERPT_CHARS,
        },
        "revision_mismatch" => AnchorRejection::RevisionMismatch {
            cited: TranscriptRevisionId::from_uuid(uuid::Uuid::nil()),
            resolved: TranscriptRevisionId::from_uuid(uuid::Uuid::nil()),
        },
        _ => AnchorRejection::ExcerptMismatch { seq_index },
    }
}
