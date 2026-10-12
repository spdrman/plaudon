//! Model catalog records and the per-model probe record (spec 2.2, 2.5B, 8.6).
//!
//! Catalog finding, captured from `GET /api/v1/models` on 2026-10-11 and
//! preserved as `tests/fixtures/openrouter/catalog-snapshot.json`:
//!
//! * 458 models listed, 44 of which declare `audio` in
//!   `architecture.input_modalities`.
//! * **No field in the catalog identifies the request family.** There is no
//!   `endpoints`, `supported_endpoints` or equivalent key saying whether a
//!   model implements `POST /audio/transcriptions`. `supported_parameters`
//!   describes chat-completion parameters only.
//! * Pricing for audio input is a separate per-unit key (`pricing.audio`,
//!   and `pricing.input_audio_cache` on 25 of the 44), distinct from
//!   `pricing.prompt`. Router entries report `-1`, i.e. price unknown ahead of
//!   routing.
//! * Nothing in the catalog reports timestamp, segment or diarization
//!   support.
//!
//! Consequently a catalog entry can only ever *downgrade* a model to
//! `Unsupported` (no audio input modality at all); it can never promote one.

use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::money::{Currency, Money};
use serde::{Deserialize, Serialize};

/// The subset of a catalog entry we rely on.
///
/// Unknown fields are preserved verbatim so a newer catalog shape round-trips
/// without being reinterpreted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Provider-scoped model identifier.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Maximum context length in tokens, when reported.
    #[serde(default)]
    pub context_length: Option<u64>,
    /// Architecture block.
    #[serde(default)]
    pub architecture: Architecture,
    /// Pricing block. Values are decimal strings in USD per unit.
    #[serde(default)]
    pub pricing: std::collections::BTreeMap<String, String>,
}

/// Architecture block of a catalog entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Architecture {
    /// Declared input modalities.
    #[serde(default)]
    pub input_modalities: Vec<String>,
    /// Declared output modalities.
    #[serde(default)]
    pub output_modalities: Vec<String>,
}

impl CatalogEntry {
    /// Whether the catalog says the model accepts audio input at all.
    #[must_use]
    pub fn declares_audio_input(&self) -> bool {
        self.architecture
            .input_modalities
            .iter()
            .any(|m| m == "audio")
    }

    /// The request family implied by the catalog.
    ///
    /// Never `MultipartStt` or `AudioChat`: the catalog cannot establish
    /// either. A model without an audio input modality is `Unsupported`;
    /// everything else is `Unknown` until a probe observes a real exchange.
    #[must_use]
    pub fn implied_request_family(&self) -> RequestFamily {
        if self.declares_audio_input() {
            RequestFamily::Unknown
        } else {
            RequestFamily::Unsupported
        }
    }

    /// Price per unit of audio input, when the catalog reports one.
    ///
    /// A negative value means the price depends on routing and is reported as
    /// unknown rather than as zero.
    #[must_use]
    pub fn audio_input_price(&self) -> Known<Money> {
        self.decimal_price("audio")
    }

    /// Price per prompt token, when the catalog reports one.
    #[must_use]
    pub fn prompt_price(&self) -> Known<Money> {
        self.decimal_price("prompt")
    }

    fn decimal_price(&self, key: &str) -> Known<Money> {
        let Some(raw) = self.pricing.get(key) else {
            return Known::Unknown;
        };
        let Some(scaled) = parse_decimal_usd(raw) else {
            return Known::Unknown;
        };
        if scaled < 0 {
            return Known::Unknown;
        }
        Known::Known(Money::new(scaled, PRICE_SCALE, Currency::USD))
    }
}

/// Fractional digits used when storing OpenRouter prices.
///
/// Catalog prices go down to 1e-9 USD per unit, so a 12-digit scale keeps the
/// smallest observed value exact without floating point.
pub const PRICE_SCALE: u8 = 12;

/// Parse a decimal USD string into a `10^PRICE_SCALE`-scaled integer.
///
/// Returns `None` rather than guessing when the string is not a plain decimal.
#[must_use]
pub fn parse_decimal_usd(raw: &str) -> Option<i128> {
    let trimmed = raw.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty() {
        return None;
    }
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if fraction.len() > usize::from(PRICE_SCALE) {
        return None;
    }
    let mut padded = String::with_capacity(whole.len() + usize::from(PRICE_SCALE));
    padded.push_str(if whole.is_empty() { "0" } else { whole });
    padded.push_str(fraction);
    for _ in fraction.len()..usize::from(PRICE_SCALE) {
        padded.push('0');
    }
    let value: i128 = padded.parse().ok()?;
    Some(if negative { -value } else { value })
}

/// Whether and how a model reports source-time offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampSupport {
    /// Segment-level offsets were present in an observed response.
    ObservedSegments,
    /// Word-level offsets were present in an observed response.
    ObservedWords,
    /// A response was observed and contained no offsets at all.
    ObservedNone,
    /// No response has been observed.
    #[serde(other)]
    NotObserved,
}

impl TimestampSupport {
    /// The alignment origin a transcript from this model may declare.
    ///
    /// Spec 8.6A: anything not actually observed yields `None`, so no seek
    /// position can be invented.
    #[must_use]
    pub const fn alignment_origin(self) -> plaudon_domain::transcript::AlignmentOrigin {
        match self {
            Self::ObservedSegments | Self::ObservedWords => {
                plaudon_domain::transcript::AlignmentOrigin::Native
            }
            Self::ObservedNone | Self::NotObserved => {
                plaudon_domain::transcript::AlignmentOrigin::None
            }
        }
    }
}

/// How a probe record was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeEvidence {
    /// A real request and response were observed and archived.
    ObservedExchange,
    /// Only the public catalog was read.
    CatalogOnly,
    /// Nothing has been read yet.
    #[serde(other)]
    None,
}

/// Everything the A1.2 probe records for one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProbeRecord {
    /// Provider-scoped model identifier.
    pub model_id: String,
    /// Request family, established only by an observed exchange.
    pub request_family: RequestFamily,
    /// How this record was established.
    pub evidence: ProbeEvidence,
    /// Containers and codecs an observed exchange accepted.
    pub accepted_formats: Known<Vec<String>>,
    /// Largest payload an observed exchange accepted.
    pub max_input_bytes: Known<u64>,
    /// Longest media an observed exchange accepted.
    pub max_duration_ms: Known<u64>,
    /// Timestamp fields actually present in an observed response.
    pub timestamps: TimestampSupport,
    /// Whether an observed response carried speaker turns.
    pub diarization: Tri,
    /// Whether an observed response carried a detected language.
    pub language_detection: Tri,
    /// Provider route the request was served by.
    pub provider_route: Known<String>,
    /// Retention or zero-data-retention control verified **for this route**.
    pub retention_control: Known<String>,
    /// Price per unit of audio input, when known.
    pub audio_input_price: Known<Money>,
    /// Unit the audio price is quoted in.
    pub pricing_unit: Known<String>,
    /// Audio input tokens per second of media, measured from an observed
    /// exchange. Until something measures it, an audio cost estimate is not
    /// locally calculable and is reported as unknown rather than guessed.
    #[serde(default)]
    pub audio_tokens_per_second: Known<u32>,
    /// Whether an observed exchange returned a body that satisfied the JSON
    /// schema the request declared.
    ///
    /// Text generation has no request family to establish (App. A.3), so this
    /// is what an observed text exchange actually demonstrates. `Unknown` is
    /// not support: a text card is never verified without a `Yes` here.
    #[serde(default)]
    pub structured_output: Tri,
    /// Output budget an observed exchange accepted and answered within.
    ///
    /// The declared budget of a successful exchange, never a catalogue figure
    /// and never rounded upward: the exchange proves the model answered under
    /// *this* ceiling, not that a larger one would be honoured.
    #[serde(default)]
    pub max_output_tokens: Known<u64>,
    /// Date the record was established, as `YYYY-MM-DD`.
    pub observed_on: String,
    /// Free-form notes. Never contains request or response bodies.
    #[serde(default)]
    pub notes: String,
}

impl ModelProbeRecord {
    /// A record built from the public catalog alone.
    ///
    /// Everything that requires a real exchange stays unknown, so this record
    /// can never make a model selectable.
    #[must_use]
    pub fn from_catalog(entry: &CatalogEntry, observed_on: impl Into<String>) -> Self {
        Self {
            model_id: entry.id.clone(),
            request_family: entry.implied_request_family(),
            evidence: ProbeEvidence::CatalogOnly,
            accepted_formats: Known::Unknown,
            max_input_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            timestamps: TimestampSupport::NotObserved,
            diarization: Tri::Unknown,
            language_detection: Tri::Unknown,
            provider_route: Known::Unknown,
            retention_control: Known::Unknown,
            audio_input_price: entry.audio_input_price(),
            pricing_unit: if entry.pricing.contains_key("audio") {
                Known::Known("usd_per_audio_input_token".to_owned())
            } else {
                Known::Unknown
            },
            audio_tokens_per_second: Known::Unknown,
            structured_output: Tri::Unknown,
            max_output_tokens: Known::Unknown,
            observed_on: observed_on.into(),
            notes: String::new(),
        }
    }

    /// Validation status this record justifies.
    ///
    /// Spec 8.6A: a model is shown as **Not verified** unless an observed
    /// exchange established an explicit request family and a resolved privacy
    /// route for that exact endpoint.
    #[must_use]
    pub fn capability_status(&self) -> CapabilityStatus {
        if self.evidence != ProbeEvidence::ObservedExchange {
            return CapabilityStatus::Unknown;
        }
        if !self.request_family.has_builder() {
            return CapabilityStatus::Unsupported;
        }
        if !self.retention_control.is_known() || !self.provider_route.is_known() {
            // The route exists but its privacy policy was not resolved, so we
            // must fail closed rather than dispatch (spec 10.8).
            return CapabilityStatus::Experimental;
        }
        CapabilityStatus::Supported
    }

    /// Whether the model picker may offer this model for transcription.
    #[must_use]
    pub fn is_offerable(&self) -> bool {
        self.capability_status().is_selectable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio_entry(id: &str) -> CatalogEntry {
        CatalogEntry {
            id: id.to_owned(),
            name: id.to_owned(),
            context_length: Some(128_000),
            architecture: Architecture {
                input_modalities: vec!["text".into(), "audio".into()],
                output_modalities: vec!["text".into()],
            },
            pricing: [
                ("prompt".to_owned(), "0.0000025".to_owned()),
                ("audio".to_owned(), "0.000032".to_owned()),
            ]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn the_catalog_can_never_establish_a_request_family() {
        let entry = audio_entry("openai/gpt-audio");
        assert!(entry.declares_audio_input());
        assert_eq!(entry.implied_request_family(), RequestFamily::Unknown);
        assert!(!entry.implied_request_family().has_builder());
    }

    #[test]
    fn a_model_without_audio_input_is_unsupported_not_unknown() {
        let mut entry = audio_entry("vendor/text-only");
        entry.architecture.input_modalities = vec!["text".into()];
        assert_eq!(entry.implied_request_family(), RequestFamily::Unsupported);
    }

    #[test]
    fn decimal_prices_are_parsed_exactly_without_floating_point() {
        assert_eq!(parse_decimal_usd("0.000032"), Some(32_000_000));
        assert_eq!(parse_decimal_usd("1"), Some(1_000_000_000_000));
        assert_eq!(parse_decimal_usd("0.000000000375"), Some(375));
        assert_eq!(parse_decimal_usd("-1"), Some(-1_000_000_000_000));
        assert_eq!(parse_decimal_usd("0"), Some(0));
    }

    #[test]
    fn unparseable_or_routed_prices_are_unknown_not_zero() {
        assert_eq!(parse_decimal_usd(""), None);
        assert_eq!(parse_decimal_usd("free"), None);
        assert_eq!(parse_decimal_usd("1.2.3"), None);
        assert_eq!(
            parse_decimal_usd("0.0000000000001"),
            None,
            "beyond our scale"
        );

        let mut router = audio_entry("openrouter/auto");
        router.pricing.insert("audio".into(), "-1".into());
        assert_eq!(router.audio_input_price(), Known::Unknown);

        let mut missing = audio_entry("vendor/model");
        missing.pricing.remove("audio");
        assert_eq!(missing.audio_input_price(), Known::Unknown);
    }

    #[test]
    fn a_catalog_only_record_is_never_offerable() {
        let record = ModelProbeRecord::from_catalog(&audio_entry("openai/gpt-audio"), "2026-10-11");
        assert_eq!(record.evidence, ProbeEvidence::CatalogOnly);
        assert_eq!(record.capability_status(), CapabilityStatus::Unknown);
        assert!(!record.is_offerable());
        assert_eq!(record.timestamps, TimestampSupport::NotObserved);
    }

    #[test]
    fn an_observed_exchange_without_a_resolved_route_stays_experimental() {
        let mut record =
            ModelProbeRecord::from_catalog(&audio_entry("openai/gpt-audio"), "2026-10-11");
        record.evidence = ProbeEvidence::ObservedExchange;
        record.request_family = RequestFamily::MultipartStt;
        assert_eq!(record.capability_status(), CapabilityStatus::Experimental);
        assert!(!record.is_offerable());

        record.provider_route = Known::Known("openai".into());
        record.retention_control = Known::Known("zdr_enforced".into());
        assert_eq!(record.capability_status(), CapabilityStatus::Supported);
        assert!(record.is_offerable());
    }

    #[test]
    fn an_observed_exchange_on_an_unsupported_family_is_unsupported() {
        let mut record = ModelProbeRecord::from_catalog(&audio_entry("vendor/text"), "2026-10-11");
        record.evidence = ProbeEvidence::ObservedExchange;
        record.request_family = RequestFamily::Unsupported;
        assert_eq!(record.capability_status(), CapabilityStatus::Unsupported);
    }

    #[test]
    fn unobserved_timestamps_never_yield_a_timeline() {
        use plaudon_domain::transcript::AlignmentOrigin;
        assert_eq!(
            TimestampSupport::NotObserved.alignment_origin(),
            AlignmentOrigin::None
        );
        assert_eq!(
            TimestampSupport::ObservedNone.alignment_origin(),
            AlignmentOrigin::None
        );
        assert_eq!(
            TimestampSupport::ObservedSegments.alignment_origin(),
            AlignmentOrigin::Native
        );
        let decoded: TimestampSupport = serde_json::from_str("\"psychic\"").unwrap();
        assert_eq!(decoded, TimestampSupport::NotObserved);
    }
}
