//! Cost aggregation and local budgets (spec 10.4, 10.7).
//!
//! Two rules decide every function here:
//!
//! * **Unknown is absorbing for truth, not for sums.** An attempt whose cost
//!   could not be determined is counted in [`CostTotals::unknown_count`] and
//!   contributes nothing to either sum. A total is therefore always honest
//!   about how much of the period it fails to describe.
//! * **An estimate never becomes an actual.** Finalized and estimated amounts
//!   are summed separately and are never added together, so a display can
//!   never present a projection as a charge.

use crate::pricing::SettledCost;
use plaudon_domain::money::{Currency, Money, MoneyEstimate};
use serde::{Deserialize, Serialize};

use super::ledger::AttemptRow;

/// Fractional digits used for stored and displayed amounts.
///
/// Matches `crate::catalog::PRICE_SCALE` so a per-unit price
/// multiplied by a quantity needs no rescaling.
pub const AMOUNT_SCALE: u8 = crate::catalog::PRICE_SCALE;

/// Render an amount as a plain decimal string, without the currency.
#[must_use]
pub fn money_to_string(amount: Money) -> String {
    let divisor = 10i128.pow(u32::from(amount.scale));
    let whole = amount.scaled_amount / divisor;
    let frac = (amount.scaled_amount % divisor).abs();
    format!("{whole}.{frac:0width$}", width = usize::from(amount.scale))
}

/// Parse a plain decimal string into money at [`AMOUNT_SCALE`].
#[must_use]
pub fn money_from_string(raw: &str, currency: Currency) -> Option<Money> {
    crate::catalog::parse_decimal_usd(raw).map(|scaled| Money::new(scaled, AMOUNT_SCALE, currency))
}

/// The `cost_confidence` column value for a settled cost.
#[must_use]
pub fn cost_confidence(cost: &SettledCost) -> &'static str {
    match cost.amount {
        MoneyEstimate::Unknown => "unknown",
        MoneyEstimate::Estimated(_) => "estimated",
        MoneyEstimate::Finalized(_) => "finalized",
    }
}

/// Separated totals over a set of attempts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CostTotals {
    /// Sum of provider-reported amounts.
    pub finalized: Option<Money>,
    /// Sum of estimate-only amounts. Never merged into `finalized`.
    pub estimated: Option<Money>,
    /// Attempts whose cost is not derivable. Never counted as zero.
    pub unknown_count: u32,
    /// Attempts included in the totals.
    pub attempts: u32,
}

impl CostTotals {
    /// Fold one settled cost in.
    pub fn add(&mut self, cost: Option<&SettledCost>) {
        self.attempts += 1;
        let Some(cost) = cost else {
            self.unknown_count += 1;
            return;
        };
        match cost.amount {
            MoneyEstimate::Unknown => self.unknown_count += 1,
            MoneyEstimate::Estimated(amount) => {
                self.estimated = Some(add(self.estimated, amount));
            }
            MoneyEstimate::Finalized(amount) => {
                self.finalized = Some(add(self.finalized, amount));
            }
        }
    }

    /// Total of everything that is known, for a budget comparison.
    ///
    /// Finalized and estimated amounts are added here *only* because a budget
    /// guardrail must account for work already dispatched but not yet priced.
    /// The display never shows this number as a charge.
    #[must_use]
    pub fn committed(&self) -> Option<Money> {
        match (self.finalized, self.estimated) {
            (Some(a), Some(b)) => a.checked_add(b),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }

    /// Whether any attempt in the period has an undeterminable cost.
    #[must_use]
    pub const fn has_unknown(&self) -> bool {
        self.unknown_count > 0
    }
}

fn add(current: Option<Money>, amount: Money) -> Money {
    match current {
        Some(existing) => existing.checked_add(amount).unwrap_or(existing),
        None => amount,
    }
}

/// Totals for one model within a period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTotals {
    /// Model identifier.
    pub model_id: String,
    /// Totals.
    pub totals: CostTotals,
}

/// A usage summary over a period.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Totals over the whole period.
    pub totals: CostTotals,
    /// Totals per model, in identifier order.
    pub by_model: Vec<ModelTotals>,
}

/// Summarize a set of ledger rows.
#[must_use]
pub fn summarize(rows: &[AttemptRow]) -> UsageSummary {
    let mut summary = UsageSummary::default();
    let mut per_model: std::collections::BTreeMap<String, CostTotals> =
        std::collections::BTreeMap::new();
    for row in rows {
        let cost = row.settled_cost();
        summary.totals.add(cost.as_ref());
        per_model
            .entry(row.model_id.clone())
            .or_default()
            .add(cost.as_ref());
    }
    summary.by_model = per_model
        .into_iter()
        .map(|(model_id, totals)| ModelTotals { model_id, totals })
        .collect();
    summary
}

/// The user's local spending guardrails (spec 10.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudBudget {
    /// Maximum estimated cost of a single job.
    pub per_job_limit: Option<Money>,
    /// Maximum committed cost in a calendar month.
    pub monthly_limit: Option<Money>,
}

impl Default for CloudBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl CloudBudget {
    /// No limits configured.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            per_job_limit: None,
            monthly_limit: None,
        }
    }

    /// Whether either limit is set, i.e. whether a block is ever possible.
    #[must_use]
    pub const fn is_enforcing(&self) -> bool {
        self.per_job_limit.is_some() || self.monthly_limit.is_some()
    }
}

/// What the local budget says about one prospective dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum BudgetDecision {
    /// Nothing in the local budget blocks this dispatch.
    Allow,
    /// A locally calculable limit would be exceeded, so nothing is sent.
    Block {
        /// Which limit.
        limit: BudgetLimit,
        /// The limit value.
        allowed: Money,
        /// What the dispatch would bring the total to.
        projected: Money,
    },
    /// A limit is set but the estimate is unknown, so the block cannot be
    /// calculated. The user is warned and must acknowledge the cost.
    AcknowledgementRequired {
        /// Which limit could not be evaluated.
        limit: BudgetLimit,
    },
}

impl BudgetDecision {
    /// Whether the dispatch may proceed without further user action.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, BudgetDecision::Allow)
    }
}

/// Which guardrail a decision refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLimit {
    /// The per-job limit.
    PerJob,
    /// The calendar-month limit.
    Monthly,
}

/// Evaluate the local budget before a dispatch (spec 10.4, 10.7).
///
/// `estimate` is the pre-dispatch estimate for this job and `month_to_date` is
/// everything already committed this month. An unknown estimate cannot be
/// compared to a limit: rather than pretend it is zero, the caller is told to
/// obtain an explicit cost acknowledgement.
#[must_use]
pub fn evaluate(
    budget: &CloudBudget,
    estimate: MoneyEstimate,
    month_to_date: &CostTotals,
) -> BudgetDecision {
    let Some(estimated) = estimate.amount() else {
        if budget.is_enforcing() {
            return BudgetDecision::AcknowledgementRequired {
                limit: budget
                    .per_job_limit
                    .map_or(BudgetLimit::Monthly, |_| BudgetLimit::PerJob),
            };
        }
        return BudgetDecision::Allow;
    };

    if let Some(limit) = budget.per_job_limit {
        if exceeds(estimated, limit) {
            return BudgetDecision::Block {
                limit: BudgetLimit::PerJob,
                allowed: limit,
                projected: estimated,
            };
        }
    }

    if let Some(limit) = budget.monthly_limit {
        let spent = month_to_date.committed();
        let projected = match spent {
            Some(spent) => spent.checked_add(estimated).unwrap_or(estimated),
            None => estimated,
        };
        if exceeds(projected, limit) {
            return BudgetDecision::Block {
                limit: BudgetLimit::Monthly,
                allowed: limit,
                projected,
            };
        }
        if month_to_date.has_unknown() {
            // Part of the month is unpriced, so the comparison is incomplete
            // and we say so rather than implying the headroom is exact.
            return BudgetDecision::AcknowledgementRequired {
                limit: BudgetLimit::Monthly,
            };
        }
    }

    BudgetDecision::Allow
}

/// Compare two amounts, normalising scale. Different currencies never compare,
/// and a mismatch is treated as "exceeds" so the fail-closed direction wins.
fn exceeds(amount: Money, limit: Money) -> bool {
    if amount.currency != limit.currency {
        return true;
    }
    let scale = amount.scale.max(limit.scale);
    let lift = |value: Money| {
        value
            .scaled_amount
            .saturating_mul(10i128.pow(u32::from(scale - value.scale)))
    };
    lift(amount) > lift(limit)
}

/// The fixed statement shown wherever a local budget is configured.
///
/// Spec 10.4 requires the UI to say plainly that a client-side limit cannot
/// prevent charges initiated elsewhere under the same key.
pub const BUDGET_ENFORCEMENT_NOTE: &str = "This budget is enforced on this device only. It cannot prevent charges made with the same API key elsewhere, and it cannot cancel a request the provider has already accepted. For hard enforcement, set a spending limit on the key in your OpenRouter account.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::CostOrigin;

    fn usd(units: i128) -> Money {
        Money::new(units, AMOUNT_SCALE, Currency::USD)
    }

    fn finalized(units: i128) -> SettledCost {
        SettledCost {
            amount: MoneyEstimate::Finalized(usd(units)),
            origin: CostOrigin::ProviderReported,
        }
    }

    fn estimated(units: i128) -> SettledCost {
        SettledCost {
            amount: MoneyEstimate::Estimated(usd(units)),
            origin: CostOrigin::DerivedFromReportedUsage,
        }
    }

    #[test]
    fn unknown_costs_are_counted_not_zeroed() {
        let mut totals = CostTotals::default();
        totals.add(Some(&finalized(100)));
        totals.add(None);
        totals.add(Some(&SettledCost::unavailable()));
        assert_eq!(totals.finalized, Some(usd(100)));
        assert_eq!(totals.estimated, None);
        assert_eq!(totals.unknown_count, 2);
        assert_eq!(totals.attempts, 3);
        assert!(totals.has_unknown());
    }

    #[test]
    fn estimates_and_finalized_amounts_never_merge() {
        let mut totals = CostTotals::default();
        totals.add(Some(&finalized(100)));
        totals.add(Some(&estimated(50)));
        assert_eq!(totals.finalized, Some(usd(100)));
        assert_eq!(totals.estimated, Some(usd(50)));
        // Only the explicitly-named budget view adds them.
        assert_eq!(totals.committed(), Some(usd(150)));
    }

    #[test]
    fn an_empty_period_has_no_total_rather_than_a_zero_one() {
        let totals = CostTotals::default();
        assert_eq!(totals.finalized, None);
        assert_eq!(totals.estimated, None);
        assert!(!totals.has_unknown());
    }

    #[test]
    fn a_per_job_limit_blocks_before_dispatch() {
        let budget = CloudBudget {
            per_job_limit: Some(usd(100)),
            monthly_limit: None,
        };
        let decision = evaluate(
            &budget,
            MoneyEstimate::Estimated(usd(101)),
            &CostTotals::default(),
        );
        assert_eq!(
            decision,
            BudgetDecision::Block {
                limit: BudgetLimit::PerJob,
                allowed: usd(100),
                projected: usd(101),
            }
        );
        assert!(!decision.is_allowed());
    }

    #[test]
    fn a_limit_exactly_met_is_allowed() {
        let budget = CloudBudget {
            per_job_limit: Some(usd(100)),
            monthly_limit: None,
        };
        assert!(evaluate(
            &budget,
            MoneyEstimate::Estimated(usd(100)),
            &CostTotals::default()
        )
        .is_allowed());
    }

    #[test]
    fn a_monthly_limit_accounts_for_what_is_already_committed() {
        let budget = CloudBudget {
            per_job_limit: None,
            monthly_limit: Some(usd(200)),
        };
        let mut month = CostTotals::default();
        month.add(Some(&finalized(150)));
        let decision = evaluate(&budget, MoneyEstimate::Estimated(usd(60)), &month);
        assert_eq!(
            decision,
            BudgetDecision::Block {
                limit: BudgetLimit::Monthly,
                allowed: usd(200),
                projected: usd(210),
            }
        );
    }

    #[test]
    fn an_unknown_estimate_cannot_be_compared_and_asks_the_user() {
        let budget = CloudBudget {
            per_job_limit: Some(usd(100)),
            monthly_limit: None,
        };
        assert_eq!(
            evaluate(&budget, MoneyEstimate::Unknown, &CostTotals::default()),
            BudgetDecision::AcknowledgementRequired {
                limit: BudgetLimit::PerJob
            }
        );
        // With no budget at all there is nothing to acknowledge.
        assert!(evaluate(
            &CloudBudget::unlimited(),
            MoneyEstimate::Unknown,
            &CostTotals::default()
        )
        .is_allowed());
    }

    #[test]
    fn an_unpriced_month_makes_the_monthly_headroom_uncertain() {
        let budget = CloudBudget {
            per_job_limit: None,
            monthly_limit: Some(usd(1_000)),
        };
        let mut month = CostTotals::default();
        month.add(None);
        assert_eq!(
            evaluate(&budget, MoneyEstimate::Estimated(usd(1)), &month),
            BudgetDecision::AcknowledgementRequired {
                limit: BudgetLimit::Monthly
            }
        );
    }

    #[test]
    fn a_currency_mismatch_fails_closed() {
        let budget = CloudBudget {
            per_job_limit: Some(Money::new(
                100,
                AMOUNT_SCALE,
                Currency::parse("eur").expect("eur"),
            )),
            monthly_limit: None,
        };
        assert!(!evaluate(
            &budget,
            MoneyEstimate::Estimated(usd(1)),
            &CostTotals::default()
        )
        .is_allowed());
    }

    #[test]
    fn amounts_round_trip_through_their_decimal_rendering() {
        let amount = usd(1_234_500_000_000);
        let rendered = money_to_string(amount);
        assert_eq!(rendered, "1.234500000000");
        assert_eq!(money_from_string(&rendered, Currency::USD), Some(amount));
    }
}
