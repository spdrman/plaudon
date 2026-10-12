//! Persistence for transcript tools, their runs and per-recording
//! conversations (spec 9.7, 13.3).
//!
//! Three invariants are carried by the schema in migration `0400` and
//! exercised by this module rather than being re-checked in Rust:
//!
//! * A tool definition row is immutable and keyed by `(tool_id, version)`.
//!   [`ToolRepository::register`] is an upsert on that pair whose only legal
//!   effect is re-writing identical values, so a prompt edit has to arrive as
//!   a new version.
//! * A `tool_runs` row carries a composite foreign key to its definition, so a
//!   run can never reference a definition that was never registered.
//! * A conversation message cannot exist without a conversation, and a
//!   conversation cannot exist without a recording **and** a transcript
//!   revision: both columns are `NOT NULL` references.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::money::MoneyEstimate;
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// A tool definition as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToolDefinition {
    /// Stable tool identity.
    pub tool_id: String,
    /// Definition version.
    pub version: u32,
    /// Name shown in the tool picker.
    pub display_name: String,
    /// What the tool produces.
    pub kind: String,
    /// Prompt template version.
    pub prompt_version: String,
    /// Output schema version.
    pub output_schema_version: String,
    /// SHA-256 of the canonical definition.
    pub definition_sha256: String,
    /// `builtin` or `user`.
    pub origin: String,
    /// Whether the tool is an opt-in extra rather than a default output.
    pub optional: bool,
    /// When the definition was first registered.
    pub created_at: Timestamp,
}

/// A tool run as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredToolRun {
    /// Run identity.
    pub id: Uuid,
    /// Recording the run belongs to.
    pub recording_id: RecordingId,
    /// Exact transcript revision consumed.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Tool identity.
    pub tool_id: String,
    /// Tool definition version.
    pub tool_version: u32,
    /// Engine that ran.
    pub engine: String,
    /// Model identifier.
    pub model_id: String,
    /// Request family used.
    pub request_family: String,
    /// Prompt template version.
    pub prompt_version: String,
    /// Output schema version.
    pub schema_version: String,
    /// SHA-256 of the dispatched prompt.
    pub input_sha256: String,
    /// SHA-256 of the accepted output.
    pub output_sha256: Option<String>,
    /// Terminal status.
    pub status: String,
    /// Consent grant that authorized the run.
    pub consent_id: Option<String>,
    /// Cost, with unknown preserved as unknown.
    pub cost: MoneyEstimate,
    /// When the run started.
    pub created_at: Timestamp,
    /// When the run finished.
    pub finished_at: Option<Timestamp>,
}

/// What a caller supplies to record a run.
#[derive(Debug, Clone)]
pub struct ToolRunInsert {
    /// Run identity.
    pub id: Uuid,
    /// Recording the run belongs to.
    pub recording_id: RecordingId,
    /// Exact transcript revision consumed.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Tool identity.
    pub tool_id: String,
    /// Tool definition version.
    pub tool_version: u32,
    /// Engine that ran.
    pub engine: String,
    /// Model identifier.
    pub model_id: String,
    /// Model version, when the engine exposes one.
    pub model_version: Option<String>,
    /// Request family used.
    pub request_family: String,
    /// Prompt template version.
    pub prompt_version: String,
    /// Output schema version.
    pub schema_version: String,
    /// SHA-256 of the dispatched prompt.
    pub input_sha256: String,
    /// SHA-256 of the accepted output.
    pub output_sha256: Option<String>,
    /// Terminal status: `succeeded`, `rejected`, `failed` or `canceled`.
    pub status: String,
    /// Consent grant that authorized the run.
    pub consent_id: Option<String>,
    /// Serialized consent and routing posture.
    pub consent_policy_json: String,
    /// Cost, with unknown preserved as unknown.
    pub cost: MoneyEstimate,
    /// When the run started.
    pub created_at: Timestamp,
    /// When the run finished.
    pub finished_at: Option<Timestamp>,
}

/// A conversation message as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
    /// Message identity.
    pub id: Uuid,
    /// Position in the conversation.
    pub seq_index: u32,
    /// `user` or `assistant`.
    pub role: String,
    /// Message text.
    pub text: String,
    /// Verified anchors, as a JSON array.
    pub source_anchors_json: String,
    /// Outcome, on assistant turns only.
    pub outcome: Option<String>,
    /// Tool run that produced an assistant turn.
    pub tool_run_id: Option<Uuid>,
    /// When the turn was recorded.
    pub created_at: Timestamp,
}

fn cost_columns(
    cost: MoneyEstimate,
) -> (&'static str, Option<String>, Option<String>, Option<String>) {
    match cost {
        MoneyEstimate::Unknown => ("unknown", None, None, None),
        MoneyEstimate::Estimated(amount) => (
            "estimated",
            Some(amount.scaled_amount.to_string()),
            None,
            Some(amount.currency.as_str().to_string()),
        ),
        MoneyEstimate::Finalized(amount) => (
            "finalized",
            None,
            Some(amount.scaled_amount.to_string()),
            Some(amount.currency.as_str().to_string()),
        ),
    }
}

fn cost_from_row(row: &sqlx::sqlite::SqliteRow) -> PersistenceResult<MoneyEstimate> {
    let confidence: String = row.try_get("cost_confidence")?;
    let currency: Option<String> = row.try_get("currency")?;
    let estimated: Option<String> = row.try_get("estimated_cost")?;
    let actual: Option<String> = row.try_get("actual_cost")?;

    let parse = |raw: Option<String>| -> PersistenceResult<Option<plaudon_domain::money::Money>> {
        let (Some(raw), Some(code)) = (raw, currency.as_deref()) else {
            return Ok(None);
        };
        let amount: i128 = raw.parse().map_err(|_| PersistenceError::IntegrityFailed)?;
        let currency = plaudon_domain::money::Currency::parse(code)
            .map_err(|_| PersistenceError::IntegrityFailed)?;
        Ok(Some(plaudon_domain::money::Money::new(amount, 2, currency)))
    };

    Ok(match confidence.as_str() {
        "estimated" => parse(estimated)?.map_or(MoneyEstimate::Unknown, MoneyEstimate::Estimated),
        "finalized" => parse(actual)?.map_or(MoneyEstimate::Unknown, MoneyEstimate::Finalized),
        _ => MoneyEstimate::Unknown,
    })
}

/// Reads and writes tool definitions, runs and conversations.
#[derive(Debug, Clone)]
pub struct ToolRepository {
    db: Database,
}

impl ToolRepository {
    /// Open the repository.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Register a tool definition, idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Conflict`] when a definition with the same
    /// identity and version already exists with a different hash: that would
    /// mean a prompt or schema edit was made without bumping the version,
    /// which spec 9.7 forbids because prior runs point at this row.
    pub async fn register(&self, definition: &StoredToolDefinition) -> PersistenceResult<()> {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT definition_sha256 FROM transcript_tools WHERE tool_id = ?1 AND version = ?2",
        )
        .bind(&definition.tool_id)
        .bind(i64::from(definition.version))
        .fetch_optional(self.db.pool())
        .await?;

        if let Some(existing) = existing {
            if existing == definition.definition_sha256 {
                return Ok(());
            }
            return Err(PersistenceError::Conflict);
        }

        sqlx::query(
            "INSERT INTO transcript_tools (tool_id, version, display_name, kind, prompt_version,
                 output_schema_version, definition_sha256, origin, optional, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&definition.tool_id)
        .bind(i64::from(definition.version))
        .bind(&definition.display_name)
        .bind(&definition.kind)
        .bind(&definition.prompt_version)
        .bind(&definition.output_schema_version)
        .bind(&definition.definition_sha256)
        .bind(&definition.origin)
        .bind(i64::from(definition.optional))
        .bind(definition.created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Every registered version of every tool, ordered.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn definitions(&self) -> PersistenceResult<Vec<StoredToolDefinition>> {
        let rows = sqlx::query(
            "SELECT tool_id, version, display_name, kind, prompt_version, output_schema_version,
                    definition_sha256, origin, optional, created_at
             FROM transcript_tools ORDER BY tool_id, version",
        )
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredToolDefinition {
                    tool_id: row.try_get("tool_id")?,
                    version: u32::try_from(row.try_get::<i64, _>("version")?).unwrap_or(1),
                    display_name: row.try_get("display_name")?,
                    kind: row.try_get("kind")?,
                    prompt_version: row.try_get("prompt_version")?,
                    output_schema_version: row.try_get("output_schema_version")?,
                    definition_sha256: row.try_get("definition_sha256")?,
                    origin: row.try_get("origin")?,
                    optional: row.try_get::<i64, _>("optional")? != 0,
                    created_at: Timestamp::from_millis(row.try_get("created_at")?),
                })
            })
            .collect()
    }

    /// Record a tool run.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures, including the composite foreign key to
    /// `transcript_tools` and the cost-confidence constraints.
    pub async fn record_run(&self, run: &ToolRunInsert) -> PersistenceResult<()> {
        let (confidence, estimated, actual, currency) = cost_columns(run.cost);
        sqlx::query(
            "INSERT INTO tool_runs (id, recording_id, transcript_revision_id, tool_id,
                 tool_version, engine, model_id, model_version, request_family, prompt_version,
                 schema_version, input_sha256, output_sha256, status, consent_id,
                 consent_policy_json, estimated_cost, actual_cost, currency, cost_confidence,
                 output_id, created_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, NULL, ?21, ?22)",
        )
        .bind(run.id.to_string())
        .bind(run.recording_id.to_string())
        .bind(run.transcript_revision_id.to_string())
        .bind(&run.tool_id)
        .bind(i64::from(run.tool_version))
        .bind(&run.engine)
        .bind(&run.model_id)
        .bind(run.model_version.as_deref())
        .bind(&run.request_family)
        .bind(&run.prompt_version)
        .bind(&run.schema_version)
        .bind(&run.input_sha256)
        .bind(run.output_sha256.as_deref())
        .bind(&run.status)
        .bind(run.consent_id.as_deref())
        .bind(&run.consent_policy_json)
        .bind(estimated)
        .bind(actual)
        .bind(currency)
        .bind(confidence)
        .bind(run.created_at.as_millis())
        .bind(run.finished_at.map(Timestamp::as_millis))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Runs for a recording, newest first.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn runs_for(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<StoredToolRun>> {
        let rows = sqlx::query(
            "SELECT id, recording_id, transcript_revision_id, tool_id, tool_version, engine,
                    model_id, request_family, prompt_version, schema_version, input_sha256,
                    output_sha256, status, consent_id, estimated_cost, actual_cost, currency,
                    cost_confidence, created_at, finished_at
             FROM tool_runs WHERE recording_id = ?1 ORDER BY created_at DESC, id DESC",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                let revision: String = row.try_get("transcript_revision_id")?;
                Ok(StoredToolRun {
                    id: id.parse().map_err(|_| PersistenceError::IntegrityFailed)?,
                    recording_id,
                    transcript_revision_id: revision
                        .parse()
                        .map_err(|_| PersistenceError::IntegrityFailed)?,
                    tool_id: row.try_get("tool_id")?,
                    tool_version: u32::try_from(row.try_get::<i64, _>("tool_version")?)
                        .unwrap_or(1),
                    engine: row.try_get("engine")?,
                    model_id: row.try_get("model_id")?,
                    request_family: row.try_get("request_family")?,
                    prompt_version: row.try_get("prompt_version")?,
                    schema_version: row.try_get("schema_version")?,
                    input_sha256: row.try_get("input_sha256")?,
                    output_sha256: row.try_get("output_sha256")?,
                    status: row.try_get("status")?,
                    consent_id: row.try_get("consent_id")?,
                    cost: cost_from_row(&row)?,
                    created_at: Timestamp::from_millis(row.try_get("created_at")?),
                    finished_at: row
                        .try_get::<Option<i64>, _>("finished_at")?
                        .map(Timestamp::from_millis),
                })
            })
            .collect()
    }

    /// Start a conversation pinned to one revision.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures; the `NOT NULL` references make a
    /// conversation without a recording or a revision impossible.
    pub async fn start_conversation(
        &self,
        id: Uuid,
        recording_id: RecordingId,
        transcript_revision_id: TranscriptRevisionId,
        created_at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO recording_conversations
                 (id, recording_id, transcript_revision_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(id.to_string())
        .bind(recording_id.to_string())
        .bind(transcript_revision_id.to_string())
        .bind(created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Append a message to a conversation.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures, including the schema rules that an
    /// assistant turn must carry an outcome and that an `answered` turn must
    /// carry at least one anchor.
    pub async fn append_message(
        &self,
        conversation_id: Uuid,
        message: &StoredMessage,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO conversation_messages (id, conversation_id, seq_index, role, text,
                 source_anchors_json, outcome, tool_run_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(message.id.to_string())
        .bind(conversation_id.to_string())
        .bind(i64::from(message.seq_index))
        .bind(&message.role)
        .bind(&message.text)
        .bind(&message.source_anchors_json)
        .bind(message.outcome.as_deref())
        .bind(message.tool_run_id.map(|id| id.to_string()))
        .bind(message.created_at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Messages of a conversation, in order.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn messages(&self, conversation_id: Uuid) -> PersistenceResult<Vec<StoredMessage>> {
        let rows = sqlx::query(
            "SELECT id, seq_index, role, text, source_anchors_json, outcome, tool_run_id,
                    created_at
             FROM conversation_messages WHERE conversation_id = ?1 ORDER BY seq_index",
        )
        .bind(conversation_id.to_string())
        .fetch_all(self.db.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                let tool_run_id: Option<String> = row.try_get("tool_run_id")?;
                Ok(StoredMessage {
                    id: id.parse().map_err(|_| PersistenceError::IntegrityFailed)?,
                    seq_index: u32::try_from(row.try_get::<i64, _>("seq_index")?).unwrap_or(0),
                    role: row.try_get("role")?,
                    text: row.try_get("text")?,
                    source_anchors_json: row.try_get("source_anchors_json")?,
                    outcome: row.try_get("outcome")?,
                    tool_run_id: tool_run_id
                        .map(|raw| raw.parse())
                        .transpose()
                        .map_err(|_| PersistenceError::IntegrityFailed)?,
                    created_at: Timestamp::from_millis(row.try_get("created_at")?),
                })
            })
            .collect()
    }

    /// Conversations for a recording, oldest first.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn conversations_for(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<(Uuid, TranscriptRevisionId)>> {
        let rows = sqlx::query(
            "SELECT id, transcript_revision_id FROM recording_conversations
             WHERE recording_id = ?1 ORDER BY created_at, id",
        )
        .bind(recording_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                let revision: String = row.try_get("transcript_revision_id")?;
                Ok((
                    id.parse().map_err(|_| PersistenceError::IntegrityFailed)?,
                    revision
                        .parse()
                        .map_err(|_| PersistenceError::IntegrityFailed)?,
                ))
            })
            .collect()
    }
}
