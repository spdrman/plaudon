//! Use cases that join the transcript tools to the library (spec 9.7, 9.8).
//!
//! `plaudon_ai_tools` is deliberately free of a database, so this module is
//! where a tool run becomes durable:
//!
//! * [`StudyOutputWriter`] persists validated study artifacts as `ai_outputs`
//!   rows with one `output_evidence` row per verified citation, which is how
//!   the export and portable-archive surfaces pick them up without knowing
//!   anything about study templates.
//! * [`ConversationService`] runs a question through `ask_recording` and
//!   records the exchange against the exact transcript revision it was
//!   answered from, with the tool run that produced it.
//!
//! Neither writes anything when the generated output fails grounding: an
//! ungrounded answer is a refusal, not a row.

use plaudon_ai_contracts::capabilities::{RequestFamily, TextModelCapabilities};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::TextModel;
use plaudon_ai_tools::anchor::SourceAnchor;
use plaudon_ai_tools::ask::{ask_recording, AskAnswer, AskOutcomeKind, AskRequest};
use plaudon_ai_tools::error::ToolError;
use plaudon_ai_tools::run::{ConsentPolicySnapshot, ToolRunDraft, ToolRunRecord, ToolRunStatus};
use plaudon_ai_tools::study::StudyArtifacts;
use plaudon_ai_tools::tool::TranscriptToolDefinition;
use plaudon_domain::ids::{OutputId, RecordingId, TranscriptRevisionId};
use plaudon_domain::money::MoneyEstimate;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::TranscriptRevision;
use plaudon_persistence::tools::{
    StoredMessage, StoredToolDefinition, ToolRepository, ToolRunInsert,
};
use plaudon_persistence::{Database, PersistenceError, PersistenceResult};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

/// How a study artifact set was stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStudyOutput {
    /// One `ai_outputs` row per artifact kind that had accepted items.
    pub output_ids: Vec<OutputId>,
    /// Number of `output_evidence` rows written.
    pub evidence_rows: usize,
}

/// Persists study artifacts in the generic output tables.
#[derive(Debug, Clone)]
pub struct StudyOutputWriter {
    db: Database,
}

/// Everything needed to attribute a stored output.
#[derive(Debug, Clone)]
pub struct OutputProvenance {
    /// Recording the artifacts belong to.
    pub recording_id: RecordingId,
    /// Exact transcript revision the artifacts cite.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Model that produced them.
    pub model_id: String,
    /// Prompt template version.
    pub prompt_version: String,
    /// Output schema version.
    pub schema_version: String,
    /// When they were produced.
    pub created_at: Timestamp,
}

impl StudyOutputWriter {
    /// Open the writer.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Persist accepted study artifacts.
    ///
    /// Each artifact kind becomes one `ai_outputs` row whose `output_type` is
    /// `flashcards`, `quiz` or `definitions`, and every verified citation
    /// becomes an `output_evidence` row with a JSON Pointer `item_path`. The
    /// evidence rows are what make an export show the citation.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures. An artifact citing a segment that is
    /// not in the named revision fails the `output_evidence` foreign key,
    /// which is the last line of defence behind anchor verification.
    pub async fn persist(
        &self,
        artifacts: &StudyArtifacts,
        provenance: &OutputProvenance,
    ) -> PersistenceResult<StoredStudyOutput> {
        let segments = self.segments(provenance.transcript_revision_id).await?;
        let mut output_ids = Vec::new();
        let mut evidence_rows = 0;
        let mut tx = self.db.pool().begin().await?;

        let groups: [(&str, serde_json::Value, Vec<(String, Vec<SourceAnchor>)>); 3] = [
            (
                "flashcards",
                serde_json::json!({ "flashcards": artifacts.flashcards }),
                artifacts
                    .flashcards
                    .iter()
                    .enumerate()
                    .map(|(index, card)| (format!("/flashcards/{index}"), card.anchors.clone()))
                    .collect(),
            ),
            (
                "quiz",
                serde_json::json!({ "quiz": artifacts.quiz }),
                artifacts
                    .quiz
                    .iter()
                    .enumerate()
                    .map(|(index, item)| (format!("/quiz/{index}"), item.anchors.clone()))
                    .collect(),
            ),
            (
                "definitions",
                serde_json::json!({ "definitions": artifacts.definitions }),
                artifacts
                    .definitions
                    .iter()
                    .enumerate()
                    .map(|(index, item)| (format!("/definitions/{index}"), item.anchors.clone()))
                    .collect(),
            ),
        ];

        for (output_type, body, items) in groups {
            if items.is_empty() {
                continue;
            }
            let output_id = OutputId::new();
            sqlx::query(
                "INSERT INTO ai_outputs (id, recording_id, output_type, transcript_revision_id,
                     template_version, model_id, prompt_version, schema_version, body_json,
                     status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'complete', ?10)",
            )
            .bind(output_id.to_string())
            .bind(provenance.recording_id.to_string())
            .bind(output_type)
            .bind(provenance.transcript_revision_id.to_string())
            .bind(&artifacts.template_version)
            .bind(&provenance.model_id)
            .bind(&provenance.prompt_version)
            .bind(&provenance.schema_version)
            .bind(body.to_string())
            .bind(provenance.created_at.as_millis())
            .execute(&mut *tx)
            .await?;

            for (item_path, anchors) in items {
                // Every cited item is a claim; the anchors hang off it
                // (EPIC I schema: `output_evidence.claim_id` is NOT NULL).
                let claim_id = Uuid::now_v7().to_string();
                sqlx::query(
                    "INSERT INTO output_claims (
                         id, output_id, item_path, kind, category, claim_text, state, entailment
                     ) VALUES (?1, ?2, ?3, 'summary_statement', 'general', ?4,
                               'source_linked', 'not_checked')",
                )
                .bind(&claim_id)
                .bind(output_id.to_string())
                .bind(&item_path)
                .bind(&item_path)
                .execute(&mut *tx)
                .await?;

                for (position, anchor) in anchors.iter().enumerate() {
                    let segment = segments
                        .iter()
                        .find(|segment| segment.seq_index == anchor.seq_index)
                        .ok_or(PersistenceError::RecordNotFound)?;
                    let (status, rejection, matched, origin) =
                        match span_text(segment, anchor.char_start, anchor.char_end) {
                            Some(text) if text == anchor.excerpt => {
                                ("verified", None, Some(text), Some("none"))
                            }
                            Some(_) => ("rejected", Some("excerpt_mismatch"), None, None),
                            None => ("rejected", Some("span_out_of_bounds"), None, None),
                        };
                    sqlx::query(
                        "INSERT INTO output_evidence (
                             id, output_id, claim_id, position, item_path, cited_revision_id,
                             seq_index, char_start, char_end, quote, verification_status,
                             rejection_code, matched_text, alignment_origin
                         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                    )
                    .bind(Uuid::now_v7().to_string())
                    .bind(output_id.to_string())
                    .bind(&claim_id)
                    .bind(i64::try_from(position).unwrap_or(i64::MAX))
                    .bind(&item_path)
                    .bind(anchor.revision_id.to_string())
                    .bind(i64::from(anchor.seq_index))
                    .bind(i64::from(anchor.char_start))
                    .bind(i64::from(anchor.char_end))
                    .bind(&anchor.excerpt)
                    .bind(status)
                    .bind(rejection)
                    .bind(matched)
                    .bind(origin)
                    .execute(&mut *tx)
                    .await?;
                    evidence_rows += 1;
                }
            }
            output_ids.push(output_id);
        }

        tx.commit().await?;
        Ok(StoredStudyOutput {
            output_ids,
            evidence_rows,
        })
    }

    async fn segments(
        &self,
        revision_id: TranscriptRevisionId,
    ) -> PersistenceResult<Vec<StoredSegment>> {
        let rows = sqlx::query(
            "SELECT seq_index, text FROM transcript_segments
             WHERE transcript_revision_id = ?1 ORDER BY seq_index",
        )
        .bind(revision_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredSegment {
                    seq_index: u32::try_from(row.try_get::<i64, _>("seq_index")?).unwrap_or(0),
                    text: row.try_get("text")?,
                })
            })
            .collect()
    }
}

/// One segment of the cited revision, as stored.
struct StoredSegment {
    seq_index: u32,
    text: String,
}

/// The text occupying `[start, end)` in `char` offsets, or `None` when the
/// span runs past the end of the segment.
fn span_text(segment: &StoredSegment, start: u32, end: u32) -> Option<String> {
    if end <= start {
        return None;
    }
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    let chars: Vec<char> = segment.text.chars().collect();
    if end > chars.len() {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// The outcome of one scripted or interactive question.
#[derive(Debug, Clone, PartialEq)]
pub struct AnsweredQuestion {
    /// Conversation the exchange was recorded in.
    pub conversation_id: Uuid,
    /// The answer, with its verified anchors.
    pub answer: AskAnswer,
    /// The run that produced it, when a model was actually dispatched.
    pub tool_run_id: Option<Uuid>,
}

/// Why a question could not be recorded.
#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    /// The tool or the model refused.
    #[error(transparent)]
    Tool(#[from] ToolError),
    /// The exchange could not be stored.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// Runs questions and records them against their exact transcript revision.
#[derive(Debug, Clone)]
pub struct ConversationService {
    repository: ToolRepository,
}

impl ConversationService {
    /// Open the service.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            repository: ToolRepository::new(db),
        }
    }

    /// Ask a question about one recording and persist the exchange.
    ///
    /// The conversation is created against `revision.id`, so a later
    /// transcript edit leaves this exchange pinned to the text it was actually
    /// answered from rather than silently re-pointing at new text
    /// (spec 9.6A).
    ///
    /// # Errors
    ///
    /// Propagates the tool refusal or the persistence failure. Nothing is
    /// written when the model refuses.
    pub async fn ask(
        &self,
        model: &dyn TextModel,
        capabilities: &TextModelCapabilities,
        tool: &TranscriptToolDefinition,
        definition_row: &StoredToolDefinition,
        recording_id: RecordingId,
        revision: &TranscriptRevision,
        question: &str,
        ctx: JobContext,
        now: Timestamp,
    ) -> Result<AnsweredQuestion, ConversationError> {
        let execution = ask_recording(
            model,
            capabilities,
            tool,
            AskRequest::new(question, revision),
            ctx,
        )
        .await?;

        self.repository.register(definition_row).await?;

        let tool_run_id = if execution.answer.dispatched {
            let run = ToolRunRecord::open(
                tool,
                ToolRunDraft {
                    recording_id,
                    transcript_revision_id: revision.id,
                    model: capabilities.model.clone(),
                    request_family: RequestFamily::AudioChat,
                    prompt_text: execution.prompt_text.clone(),
                    consent_policy: ConsentPolicySnapshot::on_device(false),
                    estimated_cost: MoneyEstimate::Unknown,
                    created_at: now,
                },
            );
            let status = match execution.answer.outcome {
                AskOutcomeKind::Answered => ToolRunStatus::Succeeded,
                _ => ToolRunStatus::Rejected,
            };
            let output_sha256 = (status == ToolRunStatus::Succeeded)
                .then(|| plaudon_ai_tools::run::sha256_hex(execution.answer.text.as_bytes()));

            self.repository
                .record_run(&ToolRunInsert {
                    id: run.id,
                    recording_id,
                    transcript_revision_id: revision.id,
                    tool_id: tool.id.to_string(),
                    tool_version: tool.version,
                    engine: capabilities.engine_id.as_str().to_string(),
                    model_id: capabilities.model.model_id.clone(),
                    model_version: capabilities.model.model_version.clone(),
                    request_family: "audio_chat".to_string(),
                    prompt_version: tool.prompt_version.clone(),
                    schema_version: tool.output_schema_version.clone(),
                    input_sha256: run.input_sha256.clone(),
                    output_sha256,
                    status: status.as_str().to_string(),
                    consent_id: None,
                    consent_policy_json: serde_json::to_string(&run.consent_policy)
                        .unwrap_or_else(|_| "{}".to_string()),
                    cost: MoneyEstimate::Unknown,
                    created_at: now,
                    finished_at: Some(now),
                })
                .await?;
            Some(run.id)
        } else {
            None
        };

        let conversation_id = Uuid::now_v7();
        self.repository
            .start_conversation(conversation_id, recording_id, revision.id, now)
            .await?;
        self.repository
            .append_message(
                conversation_id,
                &StoredMessage {
                    id: Uuid::now_v7(),
                    seq_index: 0,
                    role: "user".to_string(),
                    text: question.to_string(),
                    source_anchors_json: "[]".to_string(),
                    outcome: None,
                    tool_run_id: None,
                    created_at: now,
                },
            )
            .await?;
        self.repository
            .append_message(
                conversation_id,
                &StoredMessage {
                    id: Uuid::now_v7(),
                    seq_index: 1,
                    role: "assistant".to_string(),
                    text: execution.answer.text.clone(),
                    source_anchors_json: serde_json::to_string(&execution.answer.anchors)
                        .unwrap_or_else(|_| "[]".to_string()),
                    outcome: Some(execution.answer.outcome.as_str().to_string()),
                    tool_run_id,
                    created_at: now,
                },
            )
            .await?;

        Ok(AnsweredQuestion {
            conversation_id,
            answer: execution.answer,
            tool_run_id,
        })
    }
}
