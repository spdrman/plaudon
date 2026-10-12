//! Pricing units, pre-dispatch estimates and post-response usage (spec 10.4).
//!
//! Three rules shape every type here:
//!
//! 1. An unknown price is [`Known::Unknown`] and an unknown cost is
//!    [`MoneyEstimate::Unknown`]. Neither is ever folded into zero.
//! 2. An estimate and a finalized amount are different values of the same
//!    enum, so a caller cannot accidentally render one as the other.
//! 3. Nothing is estimated that cannot be calculated from observed data. The
//!    public catalog prices audio per *input token*, and the number of audio
//!    tokens for a file is not derivable locally, so an audio estimate exists
//!    only when a previous observed exchange established a tokens-per-second
//!    rate for that model.

use plaudon_ai_contracts::capabilities::PricingBasis;
use plaudon_domain::capability::Known;
use plaudon_domain::money::{Currency, Money, MoneyEstimate};
use serde::{Deserialize, Serialize};

use crate::catalog::{parse_decimal_usd, CatalogEntry, PRICE_SCALE};
use crate::wire::UsageDto;

/// Per-unit prices for one model, as reported by the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingUnits {
    /// USD per prompt token.
    pub prompt_per_token: Known<Money>,
    /// USD per completion token.
    pub completion_per_token: Known<Money>,
    /// USD per request, for endpoints that charge a flat fee.
    pub per_request: Known<Money>,
    /// USD per audio input token.
    pub audio_input_per_token: Known<Money>,
    /// Audio input tokens per second of media, when a previous observed
    /// exchange established the rate. Never guessed from a model family name.
    pub audio_tokens_per_second: Known<u32>,
}

impl Default for PricingUnits {
    fn default() -> Self {
        Self::unknown()
    }
}

impl PricingUnits {
    /// Prices for a model nothing is known about.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            prompt_per_token: Known::Unknown,
            completion_per_token: Known::Unknown,
            per_request: Known::Unknown,
            audio_input_per_token: Known::Unknown,
            audio_tokens_per_second: Known::Unknown,
        }
    }

    /// Read the prices a catalog entry reports.
    ///
    /// A `-1` price means "depends on routing" and is read as unknown, never as
    /// free.
    #[must_use]
    pub fn from_catalog(entry: &CatalogEntry) -> Self {
        Self {
            prompt_per_token: price(entry, "prompt"),
            completion_per_token: price(entry, "completion"),
            per_request: price(entry, "request"),
            audio_input_per_token: price(entry, "audio"),
            audio_tokens_per_second: Known::Unknown,
        }
    }

    /// Record an observed audio tokenization rate.
    #[must_use]
    pub fn with_audio_tokens_per_second(mut self, rate: u32) -> Self {
        self.audio_tokens_per_second = Known::Known(rate);
        self
    }

    /// The basis this model is charged on, as far as the catalog shows.
    #[must_use]
    pub fn basis(&self) -> Known<PricingBasis> {
        if self.audio_input_per_token.is_known() {
            Known::Known(PricingBasis::PerToken)
        } else if self.prompt_per_token.is_known() {
            Known::Known(PricingBasis::PerToken)
        } else if self.per_request.is_known() {
            Known::Known(PricingBasis::PerRequest)
        } else {
            Known::Unknown
        }
    }

    /// Human-readable unit label, or `None` when no price is known.
    #[must_use]
    pub fn unit_label(&self) -> Option<&'static str> {
        if self.audio_input_per_token.is_known() {
            Some("USD per audio input token")
        } else if self.prompt_per_token.is_known() {
            Some("USD per prompt token")
        } else if self.per_request.is_known() {
            Some("USD per request")
        } else {
            None
        }
    }
}

fn price(entry: &CatalogEntry, key: &str) -> Known<Money> {
    let Some(raw) = entry.pricing.get(key) else {
        return Known::Unknown;
    };
    match parse_decimal_usd(raw) {
        Some(scaled) if scaled >= 0 => Known::Known(Money::new(scaled, PRICE_SCALE, Currency::USD)),
        _ => Known::Unknown,
    }
}

/// What a request will consume, as far as it is locally calculable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateInput {
    /// An audio request of a known media duration.
    Audio {
        /// Media duration in milliseconds, when the decoder established one.
        duration_ms: Option<u64>,
    },
    /// A text request with a locally counted prompt size and an output budget.
    Text {
        /// Prompt tokens, as counted by the caller's tokenizer approximation.
        prompt_tokens: u64,
        /// Maximum output tokens the request allows.
        max_output_tokens: u64,
    },
}

/// Why an estimate could not be produced. Displayed verbatim as the reason the
/// price is unknown, so the UI never has to invent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimateGap {
    /// The catalog reported no usable price for this model.
    PriceUnknown,
    /// The media duration was not established, so no quantity exists.
    DurationUnknown,
    /// Audio is priced per token and no observed tokens-per-second rate exists
    /// for this model, so the token count is not locally calculable.
    AudioTokenRateUnobserved,
    /// The arithmetic overflowed, which means the inputs were nonsense.
    Overflow,
}

/// A pre-dispatch estimate, or an explicit gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEstimate {
    /// The amount. [`MoneyEstimate::Estimated`] or [`MoneyEstimate::Unknown`];
    /// never [`MoneyEstimate::Finalized`], which only a provider response can
    /// produce.
    pub amount: MoneyEstimate,
    /// Why the amount is unknown, when it is.
    pub gap: Option<EstimateGap>,
    /// The quantity the estimate was computed over, for display.
    pub quantity: Option<u64>,
    /// Label of that quantity, e.g. `audio input tokens`.
    pub quantity_label: Option<String>,
}

impl CostEstimate {
    /// An estimate that could not be computed.
    #[must_use]
    pub const fn unknown(gap: EstimateGap) -> Self {
        Self {
            amount: MoneyEstimate::Unknown,
            gap: Some(gap),
            quantity: None,
            quantity_label: None,
        }
    }

    /// Whether this estimate carries a usable amount.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        self.amount.amount().is_some()
    }
}

/// Estimate the cost of one request before dispatch.
#[must_use]
pub fn estimate(units: &PricingUnits, input: EstimateInput) -> CostEstimate {
    match input {
        EstimateInput::Audio { duration_ms } => estimate_audio(units, duration_ms),
        EstimateInput::Text {
            prompt_tokens,
            max_output_tokens,
        } => estimate_text(units, prompt_tokens, max_output_tokens),
    }
}

fn estimate_audio(units: &PricingUnits, duration_ms: Option<u64>) -> CostEstimate {
    let Known::Known(per_token) = &units.audio_input_per_token else {
        return CostEstimate::unknown(EstimateGap::PriceUnknown);
    };
    let Some(duration_ms) = duration_ms else {
        return CostEstimate::unknown(EstimateGap::DurationUnknown);
    };
    let Known::Known(rate) = &units.audio_tokens_per_second else {
        return CostEstimate::unknown(EstimateGap::AudioTokenRateUnobserved);
    };
    // Round the token count up: an estimate that is low by rounding would be
    // the wrong direction for a budget guardrail.
    let seconds = duration_ms.div_ceil(1_000);
    let Some(tokens) = seconds.checked_mul(u64::from(*rate)) else {
        return CostEstimate::unknown(EstimateGap::Overflow);
    };
    let Some(amount) = multiply(*per_token, tokens) else {
        return CostEstimate::unknown(EstimateGap::Overflow);
    };
    CostEstimate {
        amount: MoneyEstimate::Estimated(amount),
        gap: None,
        quantity: Some(tokens),
        quantity_label: Some("audio input tokens".to_owned()),
    }
}

fn estimate_text(units: &PricingUnits, prompt_tokens: u64, max_output_tokens: u64) -> CostEstimate {
    let prompt = match &units.prompt_per_token {
        Known::Known(price) => multiply(*price, prompt_tokens),
        Known::Unknown => None,
    };
    let completion = match &units.completion_per_token {
        Known::Known(price) => multiply(*price, max_output_tokens),
        // An unknown completion price makes the total unknown rather than
        // silently pricing the output at zero.
        Known::Unknown => None,
    };
    let (Some(prompt), Some(completion)) = (prompt, completion) else {
        return CostEstimate::unknown(EstimateGap::PriceUnknown);
    };
    let Some(total) = prompt.checked_add(completion) else {
        return CostEstimate::unknown(EstimateGap::Overflow);
    };
    CostEstimate {
        amount: MoneyEstimate::Estimated(total),
        gap: None,
        quantity: prompt_tokens.checked_add(max_output_tokens),
        quantity_label: Some("tokens".to_owned()),
    }
}

fn multiply(price: Money, quantity: u64) -> Option<Money> {
    let scaled = price.scaled_amount.checked_mul(i128::from(quantity))?;
    Some(Money::new(scaled, price.scale, price.currency))
}

/// How a settled amount was arrived at. Recorded on the ledger row so the UI
/// can say *why* a number is an estimate rather than an actual charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostOrigin {
    /// The provider returned a cost field.
    ProviderReported,
    /// Computed from the provider's reported token counts and the catalog
    /// prices. Still an estimate: the route may have been priced differently.
    DerivedFromReportedUsage,
    /// Nothing usable was returned.
    Unavailable,
}

/// The settled cost of one attempt, with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettledCost {
    /// The amount, finalized only when the provider itself reported it.
    pub amount: MoneyEstimate,
    /// How it was obtained.
    pub origin: CostOrigin,
}

impl SettledCost {
    /// An attempt whose cost is not known.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            amount: MoneyEstimate::Unknown,
            origin: CostOrigin::Unavailable,
        }
    }
}

/// Settle the cost of one attempt from the provider's usage block.
///
/// A provider-reported `cost` becomes [`MoneyEstimate::Finalized`]. Token
/// counts multiplied by catalog prices stay [`MoneyEstimate::Estimated`],
/// because the route may have been priced differently from the catalog
/// (spec 10.4: estimates are marked distinctly from finalized amounts).
#[must_use]
pub fn settle(units: &PricingUnits, usage: Option<&UsageDto>) -> SettledCost {
    let Some(usage) = usage else {
        return SettledCost::unavailable();
    };
    if let Some(cost) = usage.cost {
        if cost.is_finite() && cost >= 0.0 {
            if let Some(amount) = money_from_f64(cost) {
                return SettledCost {
                    amount: MoneyEstimate::Finalized(amount),
                    origin: CostOrigin::ProviderReported,
                };
            }
        }
    }
    let prompt = match (&units.prompt_per_token, usage.prompt_tokens) {
        (Known::Known(price), Some(tokens)) => multiply(*price, tokens),
        _ => None,
    };
    let completion = match (&units.completion_per_token, usage.completion_tokens) {
        (Known::Known(price), Some(tokens)) => multiply(*price, tokens),
        _ => None,
    };
    match (prompt, completion) {
        (Some(prompt), Some(completion)) => match prompt.checked_add(completion) {
            Some(total) => SettledCost {
                amount: MoneyEstimate::Estimated(total),
                origin: CostOrigin::DerivedFromReportedUsage,
            },
            None => SettledCost::unavailable(),
        },
        _ => SettledCost::unavailable(),
    }
}

/// Convert a provider-reported USD amount into exact scaled money.
///
/// Formatting through a fixed number of decimals keeps the conversion
/// deterministic and avoids carrying binary floating point into stored amounts.
#[must_use]
pub fn money_from_f64(value: f64) -> Option<Money> {
    if !value.is_finite() {
        return None;
    }
    let text = format!("{value:.*}", usize::from(PRICE_SCALE));
    parse_decimal_usd(&text).map(|scaled| Money::new(scaled, PRICE_SCALE, Currency::USD))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(scaled: i128) -> Money {
        Money::new(scaled, PRICE_SCALE, Currency::USD)
    }

    fn audio_units() -> PricingUnits {
        PricingUnits {
            // 0.000004 USD per audio input token.
            audio_input_per_token: Known::Known(usd(4_000_000)),
            ..PricingUnits::unknown()
        }
    }

    #[test]
    fn an_audio_estimate_needs_an_observed_token_rate() {
        let estimate = estimate(
            &audio_units(),
            EstimateInput::Audio {
                duration_ms: Some(60_000),
            },
        );
        assert_eq!(estimate.amount, MoneyEstimate::Unknown);
        assert_eq!(estimate.gap, Some(EstimateGap::AudioTokenRateUnobserved));
    }

    #[test]
    fn an_observed_rate_makes_the_audio_estimate_calculable() {
        let units = audio_units().with_audio_tokens_per_second(50);
        let estimate = estimate(
            &units,
            EstimateInput::Audio {
                duration_ms: Some(60_000),
            },
        );
        assert_eq!(estimate.quantity, Some(3_000));
        assert_eq!(
            estimate.amount,
            MoneyEstimate::Estimated(usd(4_000_000 * 3_000))
        );
        assert!(estimate.gap.is_none());
    }

    #[test]
    fn a_partial_duration_second_rounds_up() {
        let units = audio_units().with_audio_tokens_per_second(10);
        let estimate = estimate(
            &units,
            EstimateInput::Audio {
                duration_ms: Some(1_001),
            },
        );
        assert_eq!(estimate.quantity, Some(20));
    }

    #[test]
    fn an_unknown_completion_price_makes_the_text_total_unknown() {
        let units = PricingUnits {
            prompt_per_token: Known::Known(usd(1)),
            ..PricingUnits::unknown()
        };
        let estimate = estimate(
            &units,
            EstimateInput::Text {
                prompt_tokens: 1_000,
                max_output_tokens: 500,
            },
        );
        assert_eq!(estimate.amount, MoneyEstimate::Unknown);
        assert_eq!(estimate.gap, Some(EstimateGap::PriceUnknown));
    }

    #[test]
    fn a_text_estimate_adds_prompt_and_output_budget() {
        let units = PricingUnits {
            prompt_per_token: Known::Known(usd(2)),
            completion_per_token: Known::Known(usd(6)),
            ..PricingUnits::unknown()
        };
        let estimate = estimate(
            &units,
            EstimateInput::Text {
                prompt_tokens: 100,
                max_output_tokens: 10,
            },
        );
        assert_eq!(estimate.amount, MoneyEstimate::Estimated(usd(200 + 60)));
    }

    #[test]
    fn a_router_price_of_minus_one_is_unknown_not_free() {
        let mut entry = CatalogEntry {
            id: "x/y".to_owned(),
            name: String::new(),
            context_length: None,
            architecture: crate::catalog::Architecture::default(),
            pricing: std::collections::BTreeMap::new(),
        };
        entry.pricing.insert("prompt".to_owned(), "-1".to_owned());
        let units = PricingUnits::from_catalog(&entry);
        assert_eq!(units.prompt_per_token, Known::Unknown);
        assert!(units.unit_label().is_none());
    }

    #[test]
    fn a_provider_reported_cost_is_finalized_and_tokens_are_not() {
        let units = PricingUnits {
            prompt_per_token: Known::Known(usd(1_000_000)),
            completion_per_token: Known::Known(usd(2_000_000)),
            ..PricingUnits::unknown()
        };
        let reported = settle(
            &units,
            Some(&UsageDto {
                cost: Some(0.25),
                ..UsageDto::default()
            }),
        );
        assert_eq!(reported.origin, CostOrigin::ProviderReported);
        assert!(reported.amount.is_finalized());

        let derived = settle(
            &units,
            Some(&UsageDto {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                ..UsageDto::default()
            }),
        );
        assert_eq!(derived.origin, CostOrigin::DerivedFromReportedUsage);
        assert!(!derived.amount.is_finalized());
        assert_eq!(
            derived.amount.amount(),
            Some(usd(10_000_000 + 10_000_000))
        );
    }

    #[test]
    fn a_response_without_usage_leaves_the_cost_unknown_not_zero() {
        let settled = settle(&PricingUnits::unknown(), None);
        assert_eq!(settled.amount, MoneyEstimate::Unknown);
        assert_eq!(settled.origin, CostOrigin::Unavailable);
        assert_eq!(settled.amount.amount(), None);
    }

    #[test]
    fn provider_costs_convert_exactly() {
        assert_eq!(money_from_f64(0.25), Some(usd(250_000_000_000)));
        assert_eq!(money_from_f64(f64::NAN), None);
    }
}
