//! Presentation rules for the cloud surfaces (§4.4, §8.6, §8.6A, §10.2,
//! §10.4, §10.5, §10.8).
//!
//! The React shell held these rules in `cloudPresentation.ts` and `money.ts`;
//! EPIC O moves them into Rust so the egui views render them. Every label here
//! is derived from a field the backend actually sent: where a field is absent
//! the label says so, no missing price becomes zero, and no unverified model
//! is described as checked.
//!
//! Money is handled as integer micro-units parsed from the exact decimal
//! strings the backend sends, so nothing is ever rounded into existence.

/// Re-exported so a user interface can name the purpose and the content class
/// of a grant without depending on `plaudon-security` directly: the view layer
/// talks to this crate and to nothing else.
pub use plaudon_security::consent::{ConsentPurpose, ContentType};
use serde::{Deserialize, Serialize};

use crate::dto::{
    ConsentSummary, CostConfidence, CostValue, ModelCard, ModelFeatures, ModelPricing,
    ModelPrivacy, MoneyAmount, TriState,
};
use crate::tone::Tone;

/* ------------------------------------------------------------------ stages */

/// A pipeline stage the user can target (§9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    /// Speech to text.
    Transcribe,
    /// Speaker diarization.
    Diarize,
    /// Transcript normalization.
    Normalize,
    /// Section summaries.
    SectionSummary,
    /// Consolidated summary.
    ConsolidatedSummary,
    /// Structured extraction.
    StructuredExtraction,
    /// Citation verification.
    CitationVerification,
    /// A transcript tool run.
    ToolRun,
}

impl ProcessingStage {
    /// Every stage, in the published order.
    pub const ALL: &'static [Self] = &[
        Self::Transcribe,
        Self::Diarize,
        Self::Normalize,
        Self::SectionSummary,
        Self::ConsolidatedSummary,
        Self::StructuredExtraction,
        Self::CitationVerification,
        Self::ToolRun,
    ];

    /// Stable wire string, as the commands expect it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transcribe => "transcribe",
            Self::Diarize => "diarize",
            Self::Normalize => "normalize",
            Self::SectionSummary => "section_summary",
            Self::ConsolidatedSummary => "consolidated_summary",
            Self::StructuredExtraction => "structured_extraction",
            Self::CitationVerification => "citation_verification",
            Self::ToolRun => "tool_run",
        }
    }

    /// Parse a stage string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|stage| stage.as_str() == value)
    }

    /// Name shown to the user.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Transcribe => "Transcription",
            Self::Diarize => "Speaker diarization",
            Self::Normalize => "Transcript normalization",
            Self::SectionSummary => "Section summaries",
            Self::ConsolidatedSummary => "Consolidated summary",
            Self::StructuredExtraction => "Structured extraction",
            Self::CitationVerification => "Citation verification",
            Self::ToolRun => "Transcript tool run",
        }
    }

    /// §4.4(3): the consent purpose follows the content that actually leaves
    /// the device. `credential_validation` is reserved for a key check that
    /// sends no recording-derived content, so no stage maps to it.
    #[must_use]
    pub const fn consent_purpose(self) -> ConsentPurpose {
        match self {
            Self::Transcribe | Self::Diarize => ConsentPurpose::Transcription,
            Self::Normalize
            | Self::SectionSummary
            | Self::ConsolidatedSummary
            | Self::StructuredExtraction
            | Self::CitationVerification => ConsentPurpose::Summarization,
            Self::ToolRun => ConsentPurpose::TranscriptTool,
        }
    }

    /// What would actually leave the device for this stage.
    #[must_use]
    pub const fn content_type(self) -> ContentType {
        match self {
            Self::Transcribe | Self::Diarize => ContentType::AudioBytes,
            _ => ContentType::TranscriptText,
        }
    }

    /// Which capability check applies to a model used for this stage.
    #[must_use]
    pub const fn verification_task(self) -> &'static str {
        match self {
            Self::Transcribe | Self::Diarize => "transcription",
            _ => "text_generation",
        }
    }
}

/* ------------------------------------------------------------------ labels */

/// §8.6: which OpenRouter request contract the model was shown to implement.
#[must_use]
pub fn request_family_label(family: &str) -> &'static str {
    match family {
        "multipart_stt" => "Dedicated speech-to-text endpoint",
        "audio_chat" => "Chat request carrying audio",
        "unsupported" => "No supported request contract",
        _ => "Request contract not established",
    }
}

/// §8.6A: the machine reason a model is not verified, in plain words.
#[must_use]
pub fn verification_reason_label(reason: &str) -> &'static str {
    match reason {
        "not_in_allowlist" => "It is not on the tested allowlist.",
        "no_observed_exchange" => "No request and response has been observed for it.",
        "catalog_only" => "It appears in the provider catalogue only; nothing has been tested.",
        "privacy_route_unresolved" => {
            "Its privacy route could not be resolved for this exact endpoint."
        }
        "no_request_builder" => "No request builder implements its endpoint contract.",
        "user_supplied_custom_id" => "You typed this model id, so nothing has been tested for it.",
        _ => "No reason was reported.",
    }
}

/// Retention posture in words.
#[must_use]
pub fn retention_posture_label(retention: &str) -> &'static str {
    match retention {
        "zero_retention" => "Documented zero retention",
        "limited_retention" => "Limited retention",
        "provider_default" => "Provider default retention",
        _ => "Retention unknown",
    }
}

/// Observed-feature tri-state in words.
#[must_use]
pub const fn tri_state_label(value: TriState) -> &'static str {
    match value {
        TriState::Yes => "Yes",
        TriState::No => "No",
        TriState::Unknown => "Unknown",
    }
}

/// Why the model list is the pinned allowlist rather than the live catalogue.
#[must_use]
pub fn degraded_reason_label(reason: &str) -> &'static str {
    match reason {
        "offline" => "the provider catalogue could not be reached",
        "local_only_mode" => "local-only mode is on, so the catalogue was not fetched",
        "no_key" => "no OpenRouter key is saved, so the catalogue was not fetched",
        _ => "the provider catalogue could not be read",
    }
}

/// What a consent grant says will leave the device.
#[must_use]
pub const fn content_type_label(content: ContentType) -> &'static str {
    match content {
        ContentType::AudioBytes => "The recording's audio bytes are uploaded.",
        ContentType::TranscriptText => "The transcript text is uploaded.",
        ContentType::MetadataOnly => {
            "Only metadata is sent; no audio or transcript leaves the device."
        }
    }
}

/// The purpose a grant is given for, in words.
#[must_use]
pub const fn consent_purpose_label(purpose: ConsentPurpose) -> &'static str {
    match purpose {
        ConsentPurpose::Transcription => "Transcription",
        ConsentPurpose::Summarization => "Summarization",
        ConsentPurpose::TranscriptTool => "Transcript tool",
        ConsentPurpose::CredentialValidation => "Credential validation",
    }
}

/* ------------------------------------------------------------------- texts */

/// Version of the privacy statement these words correspond to (§4.4 #5).
pub const CONSENT_DISCLOSURE_VERSION: &str = "2026-02-01";

/// §10.5: zero retention still means the content is processed off the device,
/// and approving it now does not delete anything a provider already holds.
pub const ZDR_CAVEAT: &str = "Zero retention still processes your content outside this device, and it does not retroactively delete provider data, exports, logs or backups created earlier.";

/// Milliseconds of bundled fixture audio the capability check sends.
pub const CAPABILITY_CHECK_CLIP_MS: u64 = 5_000;

/// What the capability check actually does, stated before it is run.
pub const CAPABILITY_CHECK_DISCLOSURE: &str = "Running the check sends about 5 seconds of bundled test audio to this model through the provider. It is a real request: it may be billed, and it is the only way the model can become Verified.";

/// Refusal shown when Local-only Mode blocks a cloud operation (§4.3).
pub const LOCAL_ONLY_REFUSAL: &str = "Local-only Mode is on, so nothing is sent to a cloud provider. This run was refused rather than queued.";

/* ------------------------------------------------------------------- scopes */

/// One of the three §4.4 consent scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentScopeKind {
    /// Records a preference only; it authorises nothing.
    AskEveryTime,
    /// Covers further cloud runs for one recording with this model.
    Recording,
    /// Covers any recording processed with this model until revoked.
    ModelPolicy,
}

impl ConsentScopeKind {
    /// Every scope, in presentation order.
    pub const ALL: &'static [Self] = &[Self::AskEveryTime, Self::Recording, Self::ModelPolicy];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AskEveryTime => "ask_every_time",
            Self::Recording => "recording",
            Self::ModelPolicy => "model_policy",
        }
    }

    /// Label of the choice.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AskEveryTime => "Ask every time",
            Self::Recording => "Allow for this recording",
            Self::ModelPolicy => "Allow for this model policy",
        }
    }

    /// What choosing it does.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::AskEveryTime => "Records the preference only. It does not authorise this run, so nothing will be sent now.",
            Self::Recording => "Covers further cloud runs for this recording with this model.",
            Self::ModelPolicy => "Covers any recording processed with this model until you revoke it.",
        }
    }

    /// Whether a grant with this scope authorises the pending dispatch.
    #[must_use]
    pub const fn authorizes(self) -> bool {
        !matches!(self, Self::AskEveryTime)
    }
}

/// Whether an active grant already covers this model for this recording.
///
/// A revoked grant never covers anything, and a recording-scoped grant covers
/// only the recording it names (§16.3).
#[must_use]
pub fn consent_covers(
    grants: &[ConsentSummary],
    model_id: &str,
    recording_id: Option<&str>,
) -> bool {
    grants.iter().any(|grant| {
        grant.revoked_at.is_none()
            && grant.model_id == model_id
            && (grant.scope_kind == "model_policy"
                || (grant.scope_kind == "recording"
                    && recording_id.is_some()
                    && grant.recording_id.as_deref() == recording_id))
    })
}

/// Whether an active model-policy grant covers a capability check, which is
/// never tied to one recording.
#[must_use]
pub fn consent_covers_capability_check(grants: &[ConsentSummary], model_id: &str) -> bool {
    grants.iter().any(|grant| {
        grant.revoked_at.is_none()
            && grant.model_id == model_id
            && grant.scope_kind == "model_policy"
    })
}

/* -------------------------------------------------------------------- money */

const MICROS: i128 = 1_000_000;

/// Parses a plain decimal string into integer micro-units; `None` when the
/// string is not a non-negative decimal with at most six fraction digits.
#[must_use]
pub fn parse_micros(value: &str) -> Option<i128> {
    let trimmed = value.trim();
    let (whole, fraction) = match trimmed.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (trimmed, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if fraction.is_empty() && trimmed.contains('.') {
        return None;
    }
    if fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut padded = String::with_capacity(6);
    padded.push_str(fraction);
    while padded.len() < 6 {
        padded.push('0');
    }
    let whole: i128 = whole.parse().ok()?;
    let fraction: i128 = if padded.is_empty() {
        0
    } else {
        padded.parse().ok()?
    };
    Some(whole * MICROS + fraction)
}

/// Renders integer micro-units back as a six-decimal string.
#[must_use]
pub fn format_micros(micros: i128) -> String {
    let whole = micros / MICROS;
    let fraction = (micros % MICROS).abs();
    format!("{whole}.{fraction:06}")
}

/// A US dollar amount from micro-units.
#[must_use]
pub fn usd(micros: i128) -> MoneyAmount {
    MoneyAmount {
        amount: format_micros(micros),
        currency: "USD".to_owned(),
    }
}

/// Money is shown as the currency code plus the exact decimal we were given.
///
/// It is neither localised nor rounded: a shortened `$0.00` would claim a
/// precision — and for sub-cent amounts a value — the data does not carry.
#[must_use]
pub fn format_money(amount: &MoneyAmount) -> String {
    format!("{} {}", amount.currency, amount.amount)
}

/// Pre-dispatch estimate used for the §4.4 disclosure.
///
/// Only a duration-based unit price is calculable before a request is built;
/// token and request based pricing stays unknown rather than being guessed at.
#[must_use]
pub fn estimate_dispatch_cost(model: &ModelCard, duration_ms: Option<u64>) -> CostValue {
    let (Some(unit), Some(duration_ms)) = (model.pricing.amount_per_unit.as_ref(), duration_ms)
    else {
        return CostValue::unknown();
    };
    if model.pricing.basis != "per_audio_second" {
        return CostValue::unknown();
    }
    let Some(per_second) = parse_micros(&unit.amount) else {
        return CostValue::unknown();
    };
    let micros = (per_second * i128::from(duration_ms) + 500) / 1000;
    CostValue {
        confidence: CostConfidence::Estimated,
        amount: Some(MoneyAmount {
            amount: format_micros(micros),
            currency: unit.currency.clone(),
        }),
    }
}

/* ------------------------------------------------------------------- costs */

/// How a cost is rendered, with its confidence always stated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostPresentation {
    /// Short label; always says which of the three confidences it is.
    pub label: String,
    /// Longer sentence for assistive technology and inline notes.
    pub detail: &'static str,
    /// Tone of the badge.
    pub tone: Tone,
    /// Text glyph, so confidence is never signalled by colour alone (§5.3).
    pub glyph: &'static str,
}

/// §10.4: an estimate is never rendered as an amount that was charged, and an
/// unknown cost is never rendered as zero.
#[must_use]
pub fn cost_presentation(cost: &CostValue) -> CostPresentation {
    let Some(amount) = cost.amount.as_ref() else {
        return CostPresentation {
            label: "Cost unknown".to_owned(),
            detail: "No cost could be derived for this run, so none is shown.",
            tone: Tone::Neutral,
            glyph: "?",
        };
    };
    match cost.confidence {
        CostConfidence::Unknown => CostPresentation {
            label: "Cost unknown".to_owned(),
            detail: "No cost could be derived for this run, so none is shown.",
            tone: Tone::Neutral,
            glyph: "?",
        },
        CostConfidence::Estimated => CostPresentation {
            label: format!("Estimated {}", format_money(amount)),
            detail: "This is an estimate before dispatch, not an amount you have been charged.",
            tone: Tone::Warning,
            glyph: "≈",
        },
        CostConfidence::Finalized => CostPresentation {
            label: format!("Final {}", format_money(amount)),
            detail: "Reported by the provider for this request.",
            tone: Tone::Success,
            glyph: "=",
        },
    }
}

/// §10.4: a missing unit price is stated as unknown, never printed as zero.
#[must_use]
pub fn pricing_label(pricing: &ModelPricing) -> String {
    match pricing.amount_per_unit.as_ref() {
        None => format!("Price unknown ({})", pricing.unit_label),
        Some(amount) => format!("{} — {}", format_money(amount), pricing.unit_label),
    }
}

/// §10.8: relay plus the downstream provider, or an explicit non-disclosure.
#[must_use]
pub fn privacy_route_label(privacy: &ModelPrivacy) -> String {
    let downstream = privacy
        .downstream_provider
        .as_deref()
        .unwrap_or("provider not disclosed");
    format!("{} → {downstream}", privacy.relay)
}

/// Retention posture, including the day count when one applies.
#[must_use]
pub fn retention_label(privacy: &ModelPrivacy) -> String {
    match (privacy.retention.as_str(), privacy.retention_days) {
        ("limited_retention", Some(days)) => {
            format!(
                "{} ({days} days)",
                retention_posture_label("limited_retention")
            )
        }
        (retention, _) => retention_posture_label(retention).to_owned(),
    }
}

/* ------------------------------------------------------------ verification */

/// How a model's verification state is rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationPresentation {
    /// `Verified` or `Not verified`.
    pub label: &'static str,
    /// Tone of the badge.
    pub tone: Tone,
    /// Text glyph.
    pub glyph: &'static str,
    /// Machine reason in plain words; empty when verified.
    pub reason: &'static str,
}

/// §8.6A: `Verified` means our smoke test passed, nothing weaker.
#[must_use]
pub fn verification_presentation(model: &ModelCard) -> VerificationPresentation {
    if model.verification == "verified" {
        return VerificationPresentation {
            label: "Verified",
            tone: Tone::Success,
            glyph: "✓",
            reason: "",
        };
    }
    VerificationPresentation {
        label: "Not verified",
        tone: Tone::Warning,
        glyph: "!",
        reason: match model.verification_reason.as_deref() {
            None => "No reason was reported.",
            Some(reason) => verification_reason_label(reason),
        },
    }
}

/// Why a model cannot be dispatched to right now; `None` when it can.
#[must_use]
pub fn unselectable_reason(model: &ModelCard) -> Option<String> {
    if model.selectable {
        return None;
    }
    if model.verification == "not_verified" {
        return Some(format!(
            "This model cannot be used yet: {}",
            verification_presentation(model).reason
        ));
    }
    Some("This model cannot be used for this stage right now.".to_owned())
}

/// §10.2 advanced custom model id.
///
/// A model the user typed has been tested in no way at all, so every fact
/// about it is reported as unknown and it is never marked selectable.
#[must_use]
pub fn custom_model_card(model_id: &str, engine: &str) -> ModelCard {
    ModelCard {
        model_id: model_id.to_owned(),
        display_name: model_id.to_owned(),
        engine: engine.to_owned(),
        request_family: "unknown".to_owned(),
        verification: "not_verified".to_owned(),
        verification_reason: Some("user_supplied_custom_id".to_owned()),
        fixture_id: None,
        validated_at: None,
        features: ModelFeatures {
            segments: TriState::Unknown,
            word_timestamps: TriState::Unknown,
            diarization: TriState::Unknown,
            language_detection: TriState::Unknown,
        },
        locales: None,
        max_input_bytes: None,
        max_duration_ms: None,
        pricing: ModelPricing {
            basis: "other".to_owned(),
            unit_label: "no pricing retrieved".to_owned(),
            amount_per_unit: None,
        },
        privacy: ModelPrivacy {
            relay: engine.to_owned(),
            downstream_provider: None,
            retention: "unknown".to_owned(),
            retention_days: None,
            zero_retention_available: false,
        },
        selectable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> ModelCard {
        custom_model_card("vendor/model", "openrouter")
    }

    #[test]
    fn an_unknown_cost_is_never_zero() {
        let presentation = cost_presentation(&CostValue::unknown());
        assert_eq!(presentation.label, "Cost unknown");
        assert!(!presentation.label.contains('0'));
    }

    #[test]
    fn an_estimate_is_labelled_as_an_estimate() {
        let cost = CostValue {
            confidence: CostConfidence::Estimated,
            amount: Some(usd(12_500)),
        };
        assert_eq!(cost_presentation(&cost).label, "Estimated USD 0.012500");
    }

    #[test]
    fn a_finalized_cost_is_labelled_as_reported() {
        let cost = CostValue {
            confidence: CostConfidence::Finalized,
            amount: Some(usd(2_000_000)),
        };
        assert_eq!(cost_presentation(&cost).label, "Final USD 2.000000");
    }

    #[test]
    fn a_missing_unit_price_is_stated_as_unknown() {
        let pricing = ModelPricing {
            basis: "per_token".to_owned(),
            unit_label: "per 1K tokens".to_owned(),
            amount_per_unit: None,
        };
        assert_eq!(pricing_label(&pricing), "Price unknown (per 1K tokens)");
    }

    #[test]
    fn a_user_supplied_model_is_never_selectable() {
        let card = card();
        assert!(!card.selectable);
        assert_eq!(card.verification, "not_verified");
        assert!(unselectable_reason(&card)
            .expect("reason")
            .contains("You typed this model id"));
    }

    #[test]
    fn an_undisclosed_downstream_provider_is_stated() {
        assert_eq!(
            privacy_route_label(&card().privacy),
            "openrouter → provider not disclosed"
        );
    }

    #[test]
    fn limited_retention_states_the_day_count() {
        let mut privacy = card().privacy;
        privacy.retention = "limited_retention".to_owned();
        privacy.retention_days = Some(30);
        assert_eq!(retention_label(&privacy), "Limited retention (30 days)");
    }

    #[test]
    fn only_duration_pricing_yields_an_estimate() {
        let mut model = card();
        assert_eq!(
            estimate_dispatch_cost(&model, Some(60_000)).confidence,
            CostConfidence::Unknown
        );
        model.pricing.basis = "per_audio_second".to_owned();
        model.pricing.amount_per_unit = Some(usd(100));
        let estimate = estimate_dispatch_cost(&model, Some(60_000));
        assert_eq!(estimate.confidence, CostConfidence::Estimated);
        assert_eq!(estimate.amount.expect("amount").amount, "0.006000");
        assert_eq!(
            estimate_dispatch_cost(&model, None).confidence,
            CostConfidence::Unknown
        );
    }

    #[test]
    fn micros_round_trip_without_inventing_precision() {
        assert_eq!(parse_micros("1.5"), Some(1_500_000));
        assert_eq!(parse_micros("0.000001"), Some(1));
        assert_eq!(parse_micros("2"), Some(2_000_000));
        assert_eq!(parse_micros("1.0000001"), None);
        assert_eq!(parse_micros("-1"), None);
        assert_eq!(parse_micros("abc"), None);
        assert_eq!(parse_micros("1."), None);
        assert_eq!(format_micros(1_500_000), "1.500000");
    }

    #[test]
    fn stage_consent_mapping_follows_the_content_that_leaves() {
        assert_eq!(
            ProcessingStage::Transcribe.consent_purpose(),
            ConsentPurpose::Transcription
        );
        assert_eq!(
            ProcessingStage::Transcribe.content_type(),
            ContentType::AudioBytes
        );
        assert_eq!(
            ProcessingStage::ConsolidatedSummary.consent_purpose(),
            ConsentPurpose::Summarization
        );
        assert_eq!(
            ProcessingStage::ConsolidatedSummary.content_type(),
            ContentType::TranscriptText
        );
        assert_eq!(
            ProcessingStage::ToolRun.consent_purpose(),
            ConsentPurpose::TranscriptTool
        );
        for stage in ProcessingStage::ALL {
            assert_ne!(
                stage.consent_purpose(),
                ConsentPurpose::CredentialValidation,
                "{stage:?} must not use the credential purpose"
            );
            assert_eq!(ProcessingStage::parse(stage.as_str()), Some(*stage));
        }
    }

    #[test]
    fn ask_every_time_authorises_nothing() {
        assert!(!ConsentScopeKind::AskEveryTime.authorizes());
        assert!(ConsentScopeKind::Recording.authorizes());
        assert!(ConsentScopeKind::ModelPolicy.authorizes());
    }

    fn grant(scope: &str, model: &str, recording: Option<&str>, revoked: bool) -> ConsentSummary {
        ConsentSummary {
            consent_id: "c1".to_owned(),
            purpose: "transcription".to_owned(),
            content_type: "audio_bytes".to_owned(),
            scope_kind: scope.to_owned(),
            recording_id: recording.map(str::to_owned),
            model_id: model.to_owned(),
            downstream_provider: None,
            required_retention: None,
            disclosure_version: CONSENT_DISCLOSURE_VERSION.to_owned(),
            granted_at: "2026-02-01T00:00:00Z".to_owned(),
            expires_at: None,
            revoked_at: revoked.then(|| "2026-02-02T00:00:00Z".to_owned()),
        }
    }

    #[test]
    fn a_revoked_grant_covers_nothing() {
        let grants = vec![grant("model_policy", "m", None, true)];
        assert!(!consent_covers(&grants, "m", Some("r")));
        assert!(!consent_covers_capability_check(&grants, "m"));
    }

    #[test]
    fn a_recording_grant_covers_only_its_recording() {
        let grants = vec![grant("recording", "m", Some("r1"), false)];
        assert!(consent_covers(&grants, "m", Some("r1")));
        assert!(!consent_covers(&grants, "m", Some("r2")));
        assert!(!consent_covers(&grants, "m", None));
        assert!(!consent_covers_capability_check(&grants, "m"));
    }

    #[test]
    fn a_model_policy_grant_covers_any_recording_and_the_check() {
        let grants = vec![grant("model_policy", "m", None, false)];
        assert!(consent_covers(&grants, "m", Some("r1")));
        assert!(consent_covers_capability_check(&grants, "m"));
        assert!(!consent_covers(&grants, "other", Some("r1")));
    }
}
