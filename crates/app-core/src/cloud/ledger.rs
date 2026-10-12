//! The durable cloud request ledger (spec 10.7, 14.3).
//!
//! Implements [`AttemptLedger`] over the `inference_attempts` and
//! `usage_events` tables that migration `0002_core.sql` defines. The contract
//! the client relies on is simple and strict: [`InferenceLedger::open`] does
//! not return until the row is committed, so no user content can be dispatched
//! without a durable record of the attempt.
//!
//! Startup reconciliation ([`InferenceLedger::reconcile_in_flight`]) moves
//! every row left in `sending` or `accepted_by_provider` by a crash to
//! `outcome_unknown`: the request may already have been billed, so it is never
//! re-sent without the user explicitly accepting that risk.

use async_trait::async_trait;
use plaudon_ai_openrouter::ledger::{
    AttemptId, AttemptIntent, AttemptLedger, AttemptUpdate, LedgerUnavailable,
};
use plaudon_ai_openrouter::pricing::{CostOrigin, SettledCost};
use plaudon_domain::capability::DispatchState;
use plaudon_domain::money::MoneyEstimate;
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use uuid::Uuid;

use super::usage::{cost_confidence, money_to_string};

/// The ledger over SQLite.
#[derive(Debug, Clone)]
pub struct InferenceLedger {
    db: Database,
}

impl InferenceLedger {
    /// Wrap an open database.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// The database, for callers that need to read the ledger directly.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Resolve every attempt a crash left in flight (spec 14.3).
    ///
    /// Returns the identifiers that were moved, so the job engine can park the
    /// owning jobs as **possibly billed** rather than requeue them.
    pub async fn reconcile_in_flight(
        &self,
        at: Timestamp,
    ) -> Result<Vec<String>, LedgerUnavailable> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM inference_attempts \
             WHERE dispatch_state IN ('sending','accepted_by_provider')",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(unavailable)?;

        for (id,) in &rows {
            sqlx::query(
                "UPDATE inference_attempts \
                 SET dispatch_state = 'outcome_unknown', settled_at = ?2 WHERE id = ?1",
            )
            .bind(id)
            .bind(at.as_millis())
            .execute(self.db.pool())
            .await
            .map_err(unavailable)?;
        }
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Every attempt recorded for one job, oldest first.
    pub async fn attempts_for_job(
        &self,
        job_id: &str,
    ) -> Result<Vec<AttemptRow>, LedgerUnavailable> {
        sqlx::query_as::<_, AttemptRow>(
            "SELECT a.id, a.job_id, a.provider, a.model_id, a.request_family, a.dispatch_state, \
                    a.content_sha256, a.route, a.privacy_policy_snapshot, a.consent_id, \
                    a.estimated_units_json, a.final_cost_json, a.provider_request_id, \
                    a.created_at, a.dispatched_at, a.responded_at \
             FROM inference_attempts a WHERE a.job_id = ?1 ORDER BY a.created_at ASC, a.id ASC",
        )
        .bind(job_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(unavailable)
    }

    /// Attempts created in `[from, to)`, oldest first.
    pub async fn attempts_between(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<AttemptRow>, LedgerUnavailable> {
        sqlx::query_as::<_, AttemptRow>(
            "SELECT a.id, a.job_id, a.provider, a.model_id, a.request_family, a.dispatch_state, \
                    a.content_sha256, a.route, a.privacy_policy_snapshot, a.consent_id, \
                    a.estimated_units_json, a.final_cost_json, a.provider_request_id, \
                    a.created_at, a.dispatched_at, a.responded_at \
             FROM inference_attempts a WHERE a.created_at >= ?1 AND a.created_at < ?2 \
             ORDER BY a.created_at ASC, a.id ASC",
        )
        .bind(from.as_millis())
        .bind(to.as_millis())
        .fetch_all(self.db.pool())
        .await
        .map_err(unavailable)
    }
}

/// One ledger row as stored.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AttemptRow {
    /// Attempt identifier.
    pub id: String,
    /// Owning job.
    pub job_id: String,
    /// Relay.
    pub provider: String,
    /// Model.
    pub model_id: String,
    /// Validated request family.
    pub request_family: String,
    /// Dispatch state as stored.
    pub dispatch_state: String,
    /// Checksum of the dispatched content.
    pub content_sha256: Option<String>,
    /// Resolved route label.
    pub route: Option<String>,
    /// Privacy posture snapshot.
    pub privacy_policy_snapshot: Option<String>,
    /// Consent grant that authorized the dispatch.
    pub consent_id: Option<String>,
    /// Pre-dispatch estimate, as JSON.
    pub estimated_units_json: Option<String>,
    /// Settled cost, as JSON.
    pub final_cost_json: Option<String>,
    /// Vendor request id.
    pub provider_request_id: Option<String>,
    /// Creation instant, epoch millis.
    pub created_at: i64,
    /// Dispatch instant, epoch millis.
    pub dispatched_at: Option<i64>,
    /// Response instant, epoch millis.
    pub responded_at: Option<i64>,
}

impl AttemptRow {
    /// The typed dispatch state.
    #[must_use]
    pub fn state(&self) -> DispatchState {
        match self.dispatch_state.as_str() {
            "sending" => DispatchState::Sending,
            "accepted_by_provider" => DispatchState::AcceptedByProvider,
            "outcome_unknown" => DispatchState::OutcomeUnknown,
            "completed" => DispatchState::Completed,
            "failed_before_send" => DispatchState::FailedBeforeSend,
            _ => DispatchState::NotSent,
        }
    }

    /// Whether this attempt may already have been billed (spec 14.3).
    #[must_use]
    pub fn possibly_billed(&self) -> bool {
        matches!(
            self.state(),
            DispatchState::Sending
                | DispatchState::AcceptedByProvider
                | DispatchState::OutcomeUnknown
                | DispatchState::Completed
        )
    }

    /// The settled cost, when one was recorded.
    #[must_use]
    pub fn settled_cost(&self) -> Option<SettledCost> {
        self.final_cost_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<StoredCost>(raw).ok())
            .map(StoredCost::into_settled)
    }

    /// The pre-dispatch estimate amount, when one was recorded.
    #[must_use]
    pub fn estimate(&self) -> MoneyEstimate {
        self.estimated_units_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<StoredEstimate>(raw).ok())
            .map_or(MoneyEstimate::Unknown, |stored| stored.into_estimate())
    }
}

/// How a cost is stored in the ledger's JSON columns.
///
/// Written as explicit, human-readable fields rather than by serializing the
/// domain enum: an internally-tagged enum carrying an `i128` cannot be read
/// back by `serde_json`, and a column a support engineer has to decode with a
/// Rust binary is not a useful audit record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredCost {
    /// `unknown` | `estimated` | `finalized`.
    pub confidence: String,
    /// Decimal amount, absent when unknown.
    pub amount: Option<String>,
    /// Currency code, absent when unknown.
    pub currency: Option<String>,
    /// How the amount was obtained.
    pub origin: String,
}

impl StoredCost {
    /// Project a settled cost into its stored form.
    #[must_use]
    pub fn from_settled(cost: &SettledCost) -> Self {
        let (confidence, amount) = match cost.amount {
            MoneyEstimate::Unknown => ("unknown", None),
            MoneyEstimate::Estimated(amount) => ("estimated", Some(amount)),
            MoneyEstimate::Finalized(amount) => ("finalized", Some(amount)),
        };
        Self {
            confidence: confidence.to_owned(),
            amount: amount.map(money_to_string),
            currency: amount.map(|amount| amount.currency.as_str().to_owned()),
            origin: match cost.origin {
                CostOrigin::ProviderReported => "provider_reported",
                CostOrigin::DerivedFromReportedUsage => "derived_from_reported_usage",
                CostOrigin::Unavailable => "unavailable",
            }
            .to_owned(),
        }
    }

    /// Read the stored form back.
    ///
    /// Anything unrecognised reads as unknown, never as zero.
    #[must_use]
    pub fn into_settled(self) -> SettledCost {
        let money = self.money();
        let amount = match (self.confidence.as_str(), money) {
            ("finalized", Some(amount)) => MoneyEstimate::Finalized(amount),
            ("estimated", Some(amount)) => MoneyEstimate::Estimated(amount),
            _ => MoneyEstimate::Unknown,
        };
        SettledCost {
            amount,
            origin: match self.origin.as_str() {
                "provider_reported" => CostOrigin::ProviderReported,
                "derived_from_reported_usage" => CostOrigin::DerivedFromReportedUsage,
                _ => CostOrigin::Unavailable,
            },
        }
    }

    fn money(&self) -> Option<plaudon_domain::money::Money> {
        let raw = self.amount.as_deref()?;
        let currency = plaudon_domain::money::Currency::parse(self.currency.as_deref()?).ok()?;
        super::usage::money_from_string(raw, currency)
    }
}

/// How a pre-dispatch estimate is stored.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredEstimate {
    /// Decimal amount, absent when the estimate was not calculable.
    pub amount: Option<String>,
    /// Currency code, absent when the estimate was not calculable.
    pub currency: Option<String>,
    /// Why no amount could be calculated.
    pub gap: Option<String>,
    /// Quantity the estimate was computed over.
    pub quantity: Option<u64>,
    /// Label of that quantity.
    pub quantity_label: Option<String>,
}

impl StoredEstimate {
    /// Project an estimate into its stored form.
    #[must_use]
    pub fn from_estimate(estimate: &plaudon_ai_openrouter::pricing::CostEstimate) -> Self {
        let amount = estimate.amount.amount();
        Self {
            amount: amount.map(money_to_string),
            currency: amount.map(|amount| amount.currency.as_str().to_owned()),
            gap: estimate
                .gap
                .and_then(|gap| serde_json::to_value(gap).ok())
                .and_then(|value| value.as_str().map(str::to_owned)),
            quantity: estimate.quantity,
            quantity_label: estimate.quantity_label.clone(),
        }
    }

    /// The stored amount as a money estimate. Never finalized: a pre-dispatch
    /// figure is an estimate by construction.
    #[must_use]
    pub fn into_estimate(self) -> MoneyEstimate {
        let Some(raw) = self.amount.as_deref() else {
            return MoneyEstimate::Unknown;
        };
        let Some(currency) = self
            .currency
            .as_deref()
            .and_then(|code| plaudon_domain::money::Currency::parse(code).ok())
        else {
            return MoneyEstimate::Unknown;
        };
        super::usage::money_from_string(raw, currency).map_or(MoneyEstimate::Unknown, MoneyEstimate::Estimated)
    }
}

fn unavailable(error: sqlx::Error) -> LedgerUnavailable {
    LedgerUnavailable {
        detail: format!("ledger write failed: {}", error_kind(&error)),
    }
}

/// A non-secret label for a storage failure. The error's own `Display` can
/// include the SQL statement, which may name columns but never values; the
/// label keeps even that out of the diagnostic.
fn error_kind(error: &sqlx::Error) -> &'static str {
    match error {
        sqlx::Error::Database(_) => "database",
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => "pool",
        sqlx::Error::RowNotFound => "row_not_found",
        _ => "storage",
    }
}

#[async_trait]
impl AttemptLedger for InferenceLedger {
    async fn open(&self, intent: &AttemptIntent) -> Result<AttemptId, LedgerUnavailable> {
        let id = Uuid::now_v7().to_string();
        let now = Timestamp::now().as_millis();
        let estimate = serde_json::to_string(&StoredEstimate::from_estimate(&intent.estimate)).ok();

        sqlx::query(
            "INSERT INTO inference_attempts ( \
                id, job_id, provider, model_id, request_family, dispatch_state, \
                content_sha256, route, privacy_policy_snapshot, consent_id, \
                estimated_units_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'not_sent', ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&id)
        .bind(&intent.job_id)
        .bind(&intent.relay)
        .bind(&intent.model_id)
        .bind(family_label(intent.request_family))
        .bind(&intent.content_sha256)
        .bind(&intent.route_label)
        .bind(&intent.privacy_snapshot)
        .bind(&intent.consent_id)
        .bind(estimate)
        .bind(now)
        .execute(self.db.pool())
        .await
        .map_err(unavailable)?;

        Ok(AttemptId::committed(id))
    }

    async fn update(
        &self,
        id: &AttemptId,
        update: AttemptUpdate,
    ) -> Result<(), LedgerUnavailable> {
        let now = Timestamp::now().as_millis();
        let state = update.dispatch_state();
        let state_label = dispatch_label(state);

        match &update {
            AttemptUpdate::Sending => {
                sqlx::query(
                    "UPDATE inference_attempts SET dispatch_state = ?2, dispatched_at = ?3 \
                     WHERE id = ?1",
                )
                .bind(id.as_str())
                .bind(state_label)
                .bind(now)
                .execute(self.db.pool())
                .await
                .map_err(unavailable)?;
            }
            AttemptUpdate::Accepted {
                provider_request_id,
            }
            | AttemptUpdate::OutcomeUnknown {
                provider_request_id,
            } => {
                sqlx::query(
                    "UPDATE inference_attempts \
                     SET dispatch_state = ?2, provider_request_id = COALESCE(?3, provider_request_id), \
                         responded_at = ?4 \
                     WHERE id = ?1",
                )
                .bind(id.as_str())
                .bind(state_label)
                .bind(provider_request_id.as_deref())
                .bind(now)
                .execute(self.db.pool())
                .await
                .map_err(unavailable)?;
            }
            AttemptUpdate::FailedBeforeSend { reason } => {
                sqlx::query(
                    "UPDATE inference_attempts SET dispatch_state = ?2, settled_at = ?3, \
                         privacy_policy_snapshot = privacy_policy_snapshot \
                     WHERE id = ?1",
                )
                .bind(id.as_str())
                .bind(state_label)
                .bind(now)
                .execute(self.db.pool())
                .await
                .map_err(unavailable)?;
                tracing::debug!(reason = %reason, "cloud attempt failed before transmission");
            }
            AttemptUpdate::Completed {
                provider_request_id,
                cost,
                usage,
            } => {
                let final_cost = serde_json::to_string(&StoredCost::from_settled(cost)).ok();
                sqlx::query(
                    "UPDATE inference_attempts \
                     SET dispatch_state = ?2, provider_request_id = COALESCE(?3, provider_request_id), \
                         responded_at = ?4, settled_at = ?4, final_cost_json = ?5 \
                     WHERE id = ?1",
                )
                .bind(id.as_str())
                .bind(state_label)
                .bind(provider_request_id.as_deref())
                .bind(now)
                .bind(final_cost)
                .execute(self.db.pool())
                .await
                .map_err(unavailable)?;

                self.record_usage(id, cost, usage.as_ref(), now).await?;
            }
        }
        Ok(())
    }
}

impl InferenceLedger {
    async fn record_usage(
        &self,
        id: &AttemptId,
        cost: &SettledCost,
        usage: Option<&serde_json::Value>,
        now: i64,
    ) -> Result<(), LedgerUnavailable> {
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT job_id, provider, model_id FROM inference_attempts WHERE id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(unavailable)?;
        let Some((job_id, provider, model_id)) = row else {
            return Ok(());
        };

        // `unknown` is a real state: the row records it rather than storing a
        // zero that would silently join a sum (spec 13.2).
        let confidence = cost_confidence(cost);
        let (estimated, actual, currency) = match (&cost.amount, cost.origin) {
            (MoneyEstimate::Finalized(amount), _) => (
                None,
                Some(money_to_string(*amount)),
                Some(amount.currency.as_str().to_owned()),
            ),
            (MoneyEstimate::Estimated(amount), CostOrigin::DerivedFromReportedUsage) => (
                Some(money_to_string(*amount)),
                None,
                Some(amount.currency.as_str().to_owned()),
            ),
            (MoneyEstimate::Estimated(amount), _) => (
                Some(money_to_string(*amount)),
                None,
                Some(amount.currency.as_str().to_owned()),
            ),
            (MoneyEstimate::Unknown, _) => (None, None, None),
        };

        sqlx::query(
            "INSERT INTO usage_events (id, job_id, attempt_id, provider, model_id, usage_json, \
                 estimated_cost, actual_cost, currency, cost_confidence, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(job_id)
        .bind(id.as_str())
        .bind(provider)
        .bind(model_id)
        .bind(
            usage
                .and_then(|value| serde_json::to_string(value).ok())
                .unwrap_or_else(|| "{}".to_owned()),
        )
        .bind(estimated)
        .bind(actual)
        .bind(currency)
        .bind(confidence)
        .bind(now)
        .execute(self.db.pool())
        .await
        .map_err(unavailable)?;
        Ok(())
    }
}

const fn dispatch_label(state: DispatchState) -> &'static str {
    match state {
        DispatchState::NotSent => "not_sent",
        DispatchState::Sending => "sending",
        DispatchState::AcceptedByProvider => "accepted_by_provider",
        DispatchState::OutcomeUnknown => "outcome_unknown",
        DispatchState::Completed => "completed",
        DispatchState::FailedBeforeSend => "failed_before_send",
    }
}

fn family_label(family: plaudon_ai_contracts::capabilities::RequestFamily) -> &'static str {
    use plaudon_ai_contracts::capabilities::RequestFamily;
    match family {
        RequestFamily::MultipartStt => "multipart_stt",
        RequestFamily::AudioChat => "audio_chat",
        RequestFamily::Unsupported => "unsupported",
        RequestFamily::Unknown => "unknown",
    }
}
