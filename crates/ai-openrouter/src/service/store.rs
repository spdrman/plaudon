//! Durable state EPIC G owns: the capability registry, model selection and
//! documented per-route retention policies (migration `0400_cloud_ai.sql`).

use crate::allowlist::ModelTask;
use crate::catalog::{ModelProbeRecord, ProbeEvidence};
use crate::privacy::DocumentedPolicy;
use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_domain::money::Currency;
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_persistence::error::{PersistenceError, PersistenceResult};
use plaudon_security::consent::{ContentType, RetentionPosture};

use super::selection::{ModelChoice, Stage};
use super::usage::{money_from_string, money_to_string, CloudBudget};

/// `app_preferences` key the local budget is stored under.
pub const BUDGET_PREFERENCE_KEY: &str = "cloud.budget";

/// Schema version of the budget preference payload.
pub const BUDGET_PREFERENCE_VERSION: i64 = 1;

/// Reads and writes the EPIC G tables.
#[derive(Debug, Clone)]
pub struct CloudStore {
    db: Database,
}

impl CloudStore {
    /// Wrap an open database.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    // -----------------------------------------------------------------
    // Capability registry
    // -----------------------------------------------------------------

    /// Record an observed probe result for one model and task.
    ///
    /// The `CHECK` constraint in the migration refuses an `observed_exchange`
    /// row without a fixture id and a validation timestamp, so a verification
    /// cannot be stored without its evidence.
    pub async fn record_probe(
        &self,
        task: ModelTask,
        record: &ModelProbeRecord,
        fixture_id: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        let evidence = match record.evidence {
            ProbeEvidence::ObservedExchange => "observed_exchange",
            ProbeEvidence::CatalogOnly => "catalog_only",
            ProbeEvidence::None => "none",
        };
        let (fixture, validated_at) = if record.evidence == ProbeEvidence::ObservedExchange {
            (Some(fixture_id), Some(record.observed_on.clone()))
        } else {
            (None, None)
        };
        let json = serde_json::to_string(record).map_err(|error| {
            PersistenceError::Migration(format!("cloud settings encoding failed: {error}"))
        })?;

        sqlx::query(
            "INSERT INTO model_capability_records \
                 (model_id, task, request_family, evidence, fixture_id, validated_at, record_json, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(model_id, task) DO UPDATE SET \
                 request_family = excluded.request_family, evidence = excluded.evidence, \
                 fixture_id = excluded.fixture_id, validated_at = excluded.validated_at, \
                 record_json = excluded.record_json, updated_at = excluded.updated_at",
        )
        .bind(&record.model_id)
        .bind(task.label())
        .bind(family_label(record.request_family))
        .bind(evidence)
        .bind(fixture)
        .bind(validated_at)
        .bind(json)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Every stored probe record for one task, with the fixture that backs it.
    pub async fn probe_records(
        &self,
        task: ModelTask,
    ) -> PersistenceResult<Vec<(ModelProbeRecord, String)>> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT record_json, fixture_id FROM model_capability_records \
             WHERE task = ?1 ORDER BY model_id",
        )
        .bind(task.label())
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(json, fixture)| {
                serde_json::from_str::<ModelProbeRecord>(&json)
                    .ok()
                    .map(|record| (record, fixture.unwrap_or_default()))
            })
            .collect())
    }

    // -----------------------------------------------------------------
    // Model selection
    // -----------------------------------------------------------------

    /// Store the default model for a stage.
    pub async fn set_task_default(
        &self,
        stage: Stage,
        choice: &ModelChoice,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO model_task_defaults (stage, engine, model_id, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(stage) DO UPDATE SET engine = excluded.engine, \
                 model_id = excluded.model_id, updated_at = excluded.updated_at",
        )
        .bind(stage.as_str())
        .bind(&choice.engine)
        .bind(choice.model_id.as_deref())
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// The default model for a stage, when one is configured.
    pub async fn task_default(&self, stage: Stage) -> PersistenceResult<Option<ModelChoice>> {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT engine, model_id FROM model_task_defaults WHERE stage = ?1")
                .bind(stage.as_str())
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(engine, model_id)| ModelChoice { engine, model_id }))
    }

    /// Store a per-recording override. Never touches the task default.
    pub async fn set_recording_override(
        &self,
        recording_id: &str,
        stage: Stage,
        choice: &ModelChoice,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO recording_model_overrides (recording_id, stage, engine, model_id, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(recording_id, stage) DO UPDATE SET engine = excluded.engine, \
                 model_id = excluded.model_id, updated_at = excluded.updated_at",
        )
        .bind(recording_id)
        .bind(stage.as_str())
        .bind(&choice.engine)
        .bind(choice.model_id.as_deref())
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Remove a per-recording override, restoring the default.
    pub async fn clear_recording_override(
        &self,
        recording_id: &str,
        stage: Stage,
    ) -> PersistenceResult<()> {
        sqlx::query("DELETE FROM recording_model_overrides WHERE recording_id = ?1 AND stage = ?2")
            .bind(recording_id)
            .bind(stage.as_str())
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// The override stored for one recording and stage.
    pub async fn recording_override(
        &self,
        recording_id: &str,
        stage: Stage,
    ) -> PersistenceResult<Option<ModelChoice>> {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT engine, model_id FROM recording_model_overrides \
             WHERE recording_id = ?1 AND stage = ?2",
        )
        .bind(recording_id)
        .bind(stage.as_str())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(engine, model_id)| ModelChoice { engine, model_id }))
    }

    // -----------------------------------------------------------------
    // Route privacy policies
    // -----------------------------------------------------------------

    /// Record a documented retention policy for one exact route and content
    /// type (spec 10.8).
    pub async fn record_route_policy(
        &self,
        policy: &DocumentedPolicy,
        source_note: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        let (posture, days) = match policy.posture {
            RetentionPosture::ZeroRetention => ("zero_retention", None),
            RetentionPosture::LimitedRetention { days } => {
                ("limited_retention", Some(i64::from(days)))
            }
            RetentionPosture::ProviderDefault => ("provider_default", None),
            // An unresolved posture is the absence of a policy; storing it
            // would make "we do not know" look like a recorded decision.
            RetentionPosture::Unknown => return Ok(()),
        };
        sqlx::query(
            "INSERT INTO route_privacy_policies \
                 (model_id, downstream_provider, content_type, posture, retention_days, source_note, recorded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(model_id, content_type, IFNULL(downstream_provider, '')) DO UPDATE SET \
                 posture = excluded.posture, retention_days = excluded.retention_days, \
                 source_note = excluded.source_note, recorded_at = excluded.recorded_at",
        )
        .bind(&policy.model_id)
        .bind(policy.downstream_provider.as_deref())
        .bind(content_label(policy.content_type))
        .bind(posture)
        .bind(days)
        .bind(source_note)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Every documented policy.
    pub async fn route_policies(&self) -> PersistenceResult<Vec<DocumentedPolicy>> {
        let rows: Vec<PolicyRow> = sqlx::query_as(
            "SELECT model_id, downstream_provider, content_type, posture, retention_days \
             FROM route_privacy_policies",
        )
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(model_id, provider, content_type, posture, days)| {
                Some(DocumentedPolicy {
                    model_id,
                    downstream_provider: provider,
                    content_type: parse_content(&content_type)?,
                    posture: match posture.as_str() {
                        "zero_retention" => RetentionPosture::ZeroRetention,
                        "limited_retention" => RetentionPosture::LimitedRetention {
                            days: u32::try_from(days.unwrap_or(0)).ok()?,
                        },
                        "provider_default" => RetentionPosture::ProviderDefault,
                        _ => return None,
                    },
                })
            })
            .collect())
    }

    // -----------------------------------------------------------------
    // Budget
    // -----------------------------------------------------------------

    /// The stored local budget, or the unlimited default.
    pub async fn budget(&self) -> PersistenceResult<CloudBudget> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value_json FROM app_preferences WHERE key = ?1")
                .bind(BUDGET_PREFERENCE_KEY)
                .fetch_optional(self.db.pool())
                .await?;
        let Some((json,)) = row else {
            return Ok(CloudBudget::unlimited());
        };
        let stored: StoredBudget = serde_json::from_str(&json).unwrap_or_default();
        let currency = Currency::parse(&stored.currency).unwrap_or(Currency::USD);
        Ok(CloudBudget {
            per_job_limit: stored
                .per_job_limit
                .as_deref()
                .and_then(|raw| money_from_string(raw, currency)),
            monthly_limit: stored
                .monthly_limit
                .as_deref()
                .and_then(|raw| money_from_string(raw, currency)),
        })
    }

    /// Store the local budget.
    pub async fn set_budget(&self, budget: &CloudBudget) -> PersistenceResult<()> {
        let currency = budget.per_job_limit.or(budget.monthly_limit).map_or_else(
            || Currency::USD.as_str().to_owned(),
            |m| m.currency.as_str().to_owned(),
        );
        let stored = StoredBudget {
            per_job_limit: budget.per_job_limit.map(money_to_string),
            monthly_limit: budget.monthly_limit.map(money_to_string),
            currency,
        };
        let json = serde_json::to_string(&stored).map_err(|error| {
            PersistenceError::Migration(format!("cloud settings encoding failed: {error}"))
        })?;
        sqlx::query(
            "INSERT INTO app_preferences (key, version, value_json) VALUES (?1, ?2, ?3) \
             ON CONFLICT(key) DO UPDATE SET version = excluded.version, \
                 value_json = excluded.value_json",
        )
        .bind(BUDGET_PREFERENCE_KEY)
        .bind(BUDGET_PREFERENCE_VERSION)
        .bind(json)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Mark dependent outputs stale after a re-run produced a new revision.
    ///
    /// Outputs are preserved and relabelled, never deleted or regenerated
    /// (spec 9.6A).
    pub async fn mark_outputs_stale(&self, output_ids: &[String]) -> PersistenceResult<u64> {
        let mut updated = 0;
        for id in output_ids {
            let result = sqlx::query("UPDATE ai_outputs SET status = 'stale' WHERE id = ?1")
                .bind(id)
                .execute(self.db.pool())
                .await?;
            updated += result.rows_affected();
        }
        Ok(updated)
    }
}

/// One `route_privacy_policies` row as read back.
type PolicyRow = (String, Option<String>, String, String, Option<i64>);

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct StoredBudget {
    per_job_limit: Option<String>,
    monthly_limit: Option<String>,
    currency: String,
}

const fn family_label(family: RequestFamily) -> &'static str {
    match family {
        RequestFamily::MultipartStt => "multipart_stt",
        RequestFamily::AudioChat => "audio_chat",
        RequestFamily::Unsupported => "unsupported",
        RequestFamily::Unknown => "unknown",
    }
}

const fn content_label(content_type: ContentType) -> &'static str {
    match content_type {
        ContentType::AudioBytes => "audio_bytes",
        ContentType::TranscriptText => "transcript_text",
        ContentType::MetadataOnly => "metadata_only",
    }
}

fn parse_content(raw: &str) -> Option<ContentType> {
    match raw {
        "audio_bytes" => Some(ContentType::AudioBytes),
        "transcript_text" => Some(ContentType::TranscriptText),
        "metadata_only" => Some(ContentType::MetadataOnly),
        _ => None,
    }
}
