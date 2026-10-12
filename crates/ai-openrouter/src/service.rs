//! Cloud AI (OpenRouter BYOK) use cases: model catalogue, cost ledger, local
//! budgets, consent-gated dispatch and model selection (spec 10).
//!
//! The rest of this crate owns the wire contracts and no state;
//! `plaudon-security` owns the egress gate and consent; `plaudon-persistence`
//! owns SQLite. This module is the only place the three meet, so there is
//! exactly one answer to "may this request leave, what will it cost, and what
//! did it actually cost". It lives here rather than in `plaudon-app-core`
//! because spec 6.3(1) keeps the vendor-neutral core free of vendor adapters,
//! and this orchestration is OpenRouter-specific by construction.

pub mod authority;
pub mod ledger;
pub mod selection;
pub mod store;
pub mod usage;

use std::sync::Arc;

use crate::allowlist::{Allowlist, ModelTask};
use crate::discovery::{CatalogClient, DegradedReason};
use crate::privacy;
use crate::registry::{CatalogSource, ModelCard, ModelRegistry};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_persistence::error::PersistenceResult;
use plaudon_security::consent::ContentType;

pub use authority::{ConsentAuthority, DispatchContext};
pub use ledger::{AttemptRow, InferenceLedger};
pub use selection::{
    effective, ChoiceSource, DependentOutput, EffectiveChoice, ModelChoice, Stage,
};
pub use store::CloudStore;
pub use usage::{
    evaluate as evaluate_budget, summarize, BudgetDecision, BudgetLimit, CloudBudget, CostTotals,
    ModelTotals, UsageSummary, BUDGET_ENFORCEMENT_NOTE,
};

/// The catalogue as the picker should render it.
#[derive(Debug, Clone)]
pub struct CatalogView {
    /// Cards for the requested task, in identifier order.
    pub cards: Vec<ModelCard>,
    /// Whether a live catalogue was merged.
    pub source: CatalogSource,
    /// When the live catalogue was fetched.
    pub fetched_at: Option<String>,
    /// Why the list degraded to the pinned allowlist, when it did.
    pub degraded: Option<DegradedReason>,
}

/// Cloud AI use cases over one library.
#[derive(Debug, Clone)]
pub struct CloudAi {
    store: CloudStore,
    ledger: InferenceLedger,
    allowlist: Allowlist,
}

impl CloudAi {
    /// Open the use cases over a database.
    ///
    /// # Errors
    ///
    /// Returns an error only when the embedded allowlist manifest is
    /// unusable, which is a build defect rather than a runtime condition.
    pub fn open(db: Database) -> Result<Self, crate::allowlist::AllowlistError> {
        Ok(Self {
            store: CloudStore::new(db.clone()),
            ledger: InferenceLedger::new(db),
            allowlist: Allowlist::embedded()?,
        })
    }

    /// The durable attempt ledger.
    #[must_use]
    pub fn ledger(&self) -> &InferenceLedger {
        &self.ledger
    }

    /// The EPIC G settings store.
    #[must_use]
    pub fn store(&self) -> &CloudStore {
        &self.store
    }

    /// The pinned allowlist.
    #[must_use]
    pub fn allowlist(&self) -> &Allowlist {
        &self.allowlist
    }

    /// Build the model catalogue for one task.
    ///
    /// `client` is optional: without one (no key, Local-only Mode, or an
    /// offline host) the view is the pinned allowlist, explicitly marked as
    /// degraded rather than silently presented as live (spec 10.1).
    pub async fn catalog(
        &self,
        task: ModelTask,
        client: Option<&CatalogClient>,
        degraded_without_client: DegradedReason,
    ) -> PersistenceResult<CatalogView> {
        let mut registry = ModelRegistry::from_allowlist(&self.allowlist);
        let mut degraded = Some(degraded_without_client);

        if let Some(client) = client {
            let discovered = client.models().await;
            if discovered.is_live() {
                registry.apply_catalog(&discovered.entries, today());
                degraded = None;
            } else {
                degraded = discovered.degraded;
            }
        }

        // Observed probe records are the only thing that can verify a model.
        for (record, fixture_id) in self.store.probe_records(task).await? {
            registry.apply_probe(task, &record, &fixture_id);
        }

        // Attach the resolved destination for the content type this task sends.
        let policies = self.store.route_policies().await?;
        let content_type = match task {
            ModelTask::Transcription => ContentType::AudioBytes,
            ModelTask::TextGeneration => ContentType::TranscriptText,
        };
        let model_ids: Vec<String> = registry
            .for_task(task)
            .iter()
            .map(|card| card.model_id.clone())
            .collect();
        for model_id in model_ids {
            let resolution = privacy::resolve(&model_id, content_type, None, &policies);
            registry.apply_privacy(task, resolution);
        }

        Ok(CatalogView {
            cards: registry.for_task(task).into_iter().cloned().collect(),
            source: registry.source(),
            fetched_at: registry.catalog_fetched_at().map(str::to_owned),
            degraded,
        })
    }

    /// Resolve which model runs for one recording and stage (spec 10.2).
    pub async fn effective_choice(
        &self,
        recording_id: &str,
        stage: Stage,
        one_shot: Option<&ModelChoice>,
    ) -> PersistenceResult<Option<EffectiveChoice>> {
        let recording_override = self.store.recording_override(recording_id, stage).await?;
        let task_default = self.store.task_default(stage).await?;
        Ok(effective(
            one_shot,
            recording_override.as_ref(),
            task_default.as_ref(),
        ))
    }

    /// Usage over `[from, to)`.
    pub async fn usage(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<UsageSummary, crate::ledger::LedgerUnavailable> {
        let rows = self.ledger.attempts_between(from, to).await?;
        Ok(summarize(&rows))
    }

    /// Totals for the calendar month containing `at`.
    pub async fn month_to_date(
        &self,
        at: Timestamp,
    ) -> Result<CostTotals, crate::ledger::LedgerUnavailable> {
        let start = month_start(at);
        let rows = self.ledger.attempts_between(start, at).await?;
        Ok(summarize(&rows).totals)
    }

    /// The ledger as a trait object, for the OpenRouter clients.
    #[must_use]
    pub fn ledger_handle(&self) -> Arc<dyn crate::ledger::AttemptLedger> {
        Arc::new(self.ledger.clone())
    }
}

/// Start of the calendar month containing `at`, in UTC.
#[must_use]
pub fn month_start(at: Timestamp) -> Timestamp {
    let days = at.as_millis().div_euclid(86_400_000);
    let (year, month, _) = civil_from_days(days);
    Timestamp::from_millis(days_from_civil(year, month, 1) * 86_400_000)
}

/// Today's date as `YYYY-MM-DD`, for catalogue timestamps.
#[must_use]
pub fn today() -> String {
    let days = Timestamp::now().as_millis().div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_month_starts_at_midnight_on_the_first() {
        // 2026-10-11T12:34:56Z
        let at = Timestamp::from_millis(1_791_000_000_000);
        let start = month_start(at);
        assert!(start.as_millis() <= at.as_millis());
        let (year, month, day) = civil_from_days(start.as_millis() / 86_400_000);
        assert_eq!(day, 1);
        let (at_year, at_month, _) = civil_from_days(at.as_millis() / 86_400_000);
        assert_eq!((year, month), (at_year, at_month));
        assert_eq!(start.as_millis() % 86_400_000, 0);
    }

    #[test]
    fn the_civil_calendar_round_trips() {
        for days in [0_i64, 19_000, 20_372, -1, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{days}");
        }
    }
}
