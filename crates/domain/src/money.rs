//! Currency-tagged amounts with explicit unknown handling.
//!
//! Spec 13.2 requires that an unknown cost is never recorded as zero.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An ISO 4217 currency code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Currency([u8; 3]);

/// Reason a currency code was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("currency code must be three ASCII letters")]
pub struct CurrencyError;

impl Currency {
    /// United States dollar.
    pub const USD: Self = Self(*b"USD");

    /// Parse a three-letter currency code.
    pub fn parse(raw: &str) -> Result<Self, CurrencyError> {
        let bytes = raw.as_bytes();
        if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_alphabetic) {
            return Err(CurrencyError);
        }
        Ok(Self([
            bytes[0].to_ascii_uppercase(),
            bytes[1].to_ascii_uppercase(),
            bytes[2].to_ascii_uppercase(),
        ]))
    }

    /// The uppercase code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("???")
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Currency {
    type Err = CurrencyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// A monetary amount in integral minor-unit fractions.
///
/// Provider prices are frequently quoted with more precision than two decimal
/// places, so the amount is stored as a scaled integer with an explicit number
/// of fractional digits rather than a lossy floating-point value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    /// Amount scaled by `10^scale`.
    pub scaled_amount: i128,
    /// Number of fractional decimal digits represented in `scaled_amount`.
    pub scale: u8,
    /// Currency of the amount.
    pub currency: Currency,
}

impl Money {
    /// Construct an amount.
    #[must_use]
    pub const fn new(scaled_amount: i128, scale: u8, currency: Currency) -> Self {
        Self {
            scaled_amount,
            scale,
            currency,
        }
    }

    /// Add two amounts of the same currency and scale.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        if self.currency != other.currency || self.scale != other.scale {
            return None;
        }
        Some(Self {
            scaled_amount: self.scaled_amount.checked_add(other.scaled_amount)?,
            ..self
        })
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let divisor = 10i128.pow(u32::from(self.scale));
        let whole = self.scaled_amount / divisor;
        let frac = (self.scaled_amount % divisor).abs();
        write!(
            f,
            "{whole}.{frac:0width$} {}",
            self.currency,
            width = usize::from(self.scale)
        )
    }
}

/// An amount that may be unknown. Unknown is never rendered as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MoneyEstimate {
    /// No usable cost information was returned by the provider.
    Unknown,
    /// A pre-dispatch estimate.
    Estimated(Money),
    /// A provider-finalized amount.
    Finalized(Money),
}

impl MoneyEstimate {
    /// The amount, if one is actually known.
    #[must_use]
    pub const fn amount(self) -> Option<Money> {
        match self {
            Self::Unknown => None,
            Self::Estimated(m) | Self::Finalized(m) => Some(m),
        }
    }

    /// Whether this value is a provider-confirmed amount.
    #[must_use]
    pub const fn is_finalized(self) -> bool {
        matches!(self, Self::Finalized(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_cost_is_not_zero() {
        assert_eq!(MoneyEstimate::Unknown.amount(), None);
        assert_ne!(
            MoneyEstimate::Unknown,
            MoneyEstimate::Estimated(Money::new(0, 6, Currency::USD))
        );
    }

    #[test]
    fn amounts_of_different_currencies_do_not_add() {
        let usd = Money::new(1_000, 6, Currency::USD);
        let eur = Money::new(1_000, 6, Currency::parse("eur").unwrap());
        assert!(usd.checked_add(eur).is_none());
        assert_eq!(usd.checked_add(usd).unwrap().scaled_amount, 2_000);
    }

    #[test]
    fn currency_parsing_is_strict() {
        assert!(Currency::parse("US").is_err());
        assert!(Currency::parse("US1").is_err());
        assert_eq!(Currency::parse("usd").unwrap(), Currency::USD);
    }

    #[test]
    fn money_renders_with_its_declared_scale() {
        assert_eq!(
            Money::new(1_234_500, 6, Currency::USD).to_string(),
            "1.234500 USD"
        );
    }
}
