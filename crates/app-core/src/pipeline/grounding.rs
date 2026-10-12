//! Citation verification and claim construction (spec 9.6, 9.6A, tasks I2.1, I2.2).
//!
//! Everything in this module runs offline. Verification never calls a model,
//! so a stored verification result is reproducible from the database alone
//! and a model cannot certify its own citations (spec 9.6: "we will not rely
//! on a single LLM assertion to prove citation accuracy").

use std::collections::BTreeSet;

use plaudon_ai_contracts::extraction::{DueDate, ExtractedItem, OptionalText};
use plaudon_ai_contracts::template::OutputTemplate;
use plaudon_ai_contracts::validation::AcceptedItem;
use plaudon_domain::anchor::{AnchorVerification, SourceAnchor};
use plaudon_domain::claim::{Claim, ClaimCategory, EntailmentCheck, GroundingSummary};
use plaudon_domain::transcript::TranscriptRevision;

/// Verify one anchor against the immutable revision the run consumed.
///
/// Thin, named entry point so callers never reach past the pipeline into the
/// domain type directly; the behaviour is defined by
/// [`SourceAnchor::resolve`].
#[must_use]
pub fn verify_anchor(anchor: &SourceAnchor, revision: &TranscriptRevision) -> AnchorVerification {
    anchor.resolve(revision)
}

/// Result of grounding one model output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundingOutcome {
    /// One claim per accepted item, in item order.
    pub claims: Vec<Claim>,
    /// Aggregate statistics for the §17.3 metrics.
    pub summary: GroundingSummary,
}

/// Ground every accepted item against `revision`.
///
/// The template decides whether a field's items must carry anchors; the
/// revision decides whether the anchors resolve. The model decides neither.
#[must_use]
pub fn ground_items(
    items: &[AcceptedItem],
    revision: &TranscriptRevision,
    template: &OutputTemplate,
) -> GroundingOutcome {
    let mut claims = Vec::with_capacity(items.len());
    for accepted in items {
        let draft = accepted.item.to_claim_draft(accepted.item_path.clone());
        let verifications: Vec<AnchorVerification> = draft
            .anchors
            .iter()
            .map(|anchor| verify_anchor(anchor, revision))
            .collect();
        // The span proves the citation is precise; entailment is judged
        // against the whole cited segment, which is what a reviewer reads
        // when they open the anchor. Judging a summarized sentence against a
        // single quoted clause would mark almost every faithful paraphrase
        // unsupported and make "review required" meaningless.
        let supporting: Vec<&str> = verifications
            .iter()
            .filter_map(|verification| verification.verified())
            .filter_map(|verified| {
                revision
                    .segments()
                    .iter()
                    .find(|segment| segment.seq_index == verified.seq_index)
                    .map(|segment| segment.text.as_str())
            })
            .collect();
        let entailment = spot_check_item(&accepted.item, &draft.text, &supporting);
        let category = classify_item(&accepted.item);
        let anchors_required = template.anchors_required(&accepted.field);
        claims.push(Claim::from_draft(
            draft,
            verifications,
            category,
            entailment,
            anchors_required,
        ));
    }
    let summary = GroundingSummary::of(&claims);
    GroundingOutcome { claims, summary }
}

/// Entailment spot-check for one extracted item.
///
/// Runs the lexical check over the rendered claim and additionally requires
/// that any name the model put in a structured owner or participant field
/// actually occurs in the cited span. That is the §18.6A "falsified owner"
/// case, and checking the field rather than the sentence keeps the check
/// precise without punishing paraphrase.
#[must_use]
pub fn spot_check_item(
    item: &ExtractedItem,
    claim_text: &str,
    supporting_spans: &[&str],
) -> EntailmentCheck {
    let lexical = spot_check_entailment(claim_text, supporting_spans);
    if lexical != EntailmentCheck::Supported {
        return lexical;
    }
    let evidence = supporting_spans.join(" ").to_lowercase();
    for name in named_references(item) {
        let lowered = name.to_lowercase();
        if !lowered.is_empty() && !evidence.contains(&lowered) {
            return EntailmentCheck::Unsupported;
        }
    }
    EntailmentCheck::Supported
}

/// Names the model asserted in a structured field, which the cited span must
/// contain.
fn named_references(item: &ExtractedItem) -> Vec<&str> {
    use plaudon_ai_contracts::extraction::OwnerRef;
    match item {
        ExtractedItem::Decision(decision) => {
            decision.participants.iter().map(String::as_str).collect()
        }
        ExtractedItem::ActionItem(action) => match &action.owner {
            OwnerRef::Named(name) => vec![name.as_str()],
            OwnerRef::Unassigned => Vec::new(),
        },
        ExtractedItem::Question(question) => match &question.owner {
            OwnerRef::Named(name) => vec![name.as_str()],
            OwnerRef::Unassigned => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Classify the consequence category of one extracted item (spec 9.6A).
///
/// Structure is consulted before prose: an action item that carries a real
/// due date is a deadline whatever its wording.
#[must_use]
pub fn classify_item(item: &ExtractedItem) -> ClaimCategory {
    let mut text = item.claim_text();
    match item {
        ExtractedItem::ActionItem(action) => {
            if let DueDate::On(_) = action.due_date {
                // A stated date is a deadline unless something graver applies.
                if let Some(category) = classify_text_high(&text) {
                    return category;
                }
                return ClaimCategory::Deadline;
            }
        }
        ExtractedItem::Decision(decision) => {
            if let OptionalText::Stated(reference) = &decision.time_reference {
                text.push(' ');
                text.push_str(reference);
            }
        }
        ExtractedItem::Risk(risk) => {
            for part in [risk.consequence.stated(), risk.mitigation.stated()]
                .into_iter()
                .flatten()
            {
                text.push(' ');
                text.push_str(part);
            }
        }
        _ => {}
    }
    classify_text_high(&text).unwrap_or(ClaimCategory::General)
}

/// Classify free text into a high-consequence category, if any.
///
/// Order matters: safety outranks money, money outranks a bare obligation, and
/// an obligation outranks a date. A claim can only carry one category, and we
/// pick the one that most justifies review.
fn classify_text_high(text: &str) -> Option<ClaimCategory> {
    let lowered = text.to_lowercase();
    if contains_any(
        &lowered,
        &[
            "safety",
            "hazard",
            "injury",
            "incident",
            "evacuat",
            "lockout",
            "ppe",
            "harm",
            "toxic",
            "fire ",
            "emergency",
            "fatality",
        ],
    ) {
        return Some(ClaimCategory::SafetyCommitment);
    }
    if has_monetary_amount(&lowered) {
        return Some(ClaimCategory::FinancialAmount);
    }
    if contains_any(
        &lowered,
        &[
            "contract",
            "obligat",
            "liable",
            "liability",
            "penalt",
            "guarantee",
            "warrant",
            "indemnif",
            "sla",
            "we must ",
            "we shall ",
            "committed to",
            "commit to",
            "legally",
        ],
    ) {
        return Some(ClaimCategory::Obligation);
    }
    if contains_any(
        &lowered,
        &[
            "deadline",
            "due by",
            "due on",
            "no later than",
            "by end of",
            "end of day",
            "cut-off",
            "cutoff",
            "expires",
        ],
    ) || has_iso_date(&lowered)
    {
        return Some(ClaimCategory::Deadline);
    }
    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn has_monetary_amount(lowered: &str) -> bool {
    if lowered.contains('$')
        || lowered.contains('€')
        || lowered.contains('£')
        || lowered.contains('¥')
    {
        return true;
    }
    let currency_words = [
        "usd", "eur", "gbp", "dollar", "euro", "pound", "cad", "aud", "chf", "yen",
    ];
    let has_digit = lowered.chars().any(|c| c.is_ascii_digit());
    has_digit && contains_any(lowered, &currency_words)
}

fn has_iso_date(lowered: &str) -> bool {
    let bytes = lowered.as_bytes();
    bytes.windows(10).any(|window| {
        window[..4].iter().all(u8::is_ascii_digit)
            && window[4] == b'-'
            && window[5..7].iter().all(u8::is_ascii_digit)
            && window[7] == b'-'
            && window[8..].iter().all(u8::is_ascii_digit)
    })
}

/// Entailment spot-check against the exact cited spans (spec 9.6A).
///
/// This is a conservative lexical check, not a natural-language inference
/// model, and it is named accordingly. It answers one question: do the
/// distinguishing tokens of the claim — numbers, dates and proper nouns —
/// actually occur in the text that was cited? That is enough to catch a
/// falsified owner, amount or date, which is what spec 18.6A's AI cluster
/// requires. Anything subtler returns [`EntailmentCheck::Supported`] and is
/// still subject to human review.
#[must_use]
pub fn spot_check_entailment(claim_text: &str, supporting_spans: &[&str]) -> EntailmentCheck {
    if supporting_spans.is_empty() {
        return EntailmentCheck::NotChecked;
    }
    let evidence: String = supporting_spans.join(" ").to_lowercase();
    let evidence_tokens: BTreeSet<String> = tokenize(&evidence).collect();

    let mut distinguishing = 0_u32;
    let mut distinguishing_found = 0_u32;
    let mut content = 0_u32;
    let mut content_found = 0_u32;

    for (raw, lowered) in tokenize_with_case(claim_text) {
        if is_sentinel_or_stopword(&lowered) {
            continue;
        }
        let present = evidence_tokens.contains(&lowered) || evidence.contains(&lowered);
        if is_distinguishing(&raw) {
            distinguishing += 1;
            if present {
                distinguishing_found += 1;
            }
        } else if lowered.chars().count() >= 4 {
            content += 1;
            if present {
                content_found += 1;
            }
        }
    }

    if distinguishing > 0 && distinguishing_found < distinguishing {
        return EntailmentCheck::Unsupported;
    }
    if content > 0 && f64::from(content_found) / f64::from(content) < 0.34 {
        return EntailmentCheck::Unsupported;
    }
    EntailmentCheck::Supported
}

fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'')
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
}

fn tokenize_with_case(text: &str) -> impl Iterator<Item = (String, String)> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'')
        .filter(|token| !token.is_empty())
        .map(|token| (token.to_owned(), token.to_lowercase()))
}

/// Whether a token is specific enough that its absence from the cited span is
/// evidence of fabrication.
///
/// Only tokens containing a digit qualify: amounts, quantities, dates and
/// times. A faithful summary legitimately paraphrases prose, so requiring
/// every proper noun of a rendered claim to appear inside one short cited
/// clause would mark almost every honest claim unsupported. Named references
/// are checked structurally instead, by [`named_references`], which reads the
/// owner and participant *fields* rather than the rendered sentence.
fn is_distinguishing(raw: &str) -> bool {
    raw.chars().any(|c| c.is_ascii_digit())
}

fn is_sentinel_or_stopword(lowered: &str) -> bool {
    const STOP: &[&str] = &[
        "the",
        "a",
        "an",
        "and",
        "or",
        "but",
        "to",
        "of",
        "in",
        "on",
        "at",
        "for",
        "with",
        "by",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "will",
        "would",
        "should",
        "it",
        "that",
        "this",
        "we",
        "i",
        "they",
        "he",
        "she",
        "as",
        "from",
        "owner",
        "due",
        "unassigned",
        "not",
        "specified",
        "none",
        "unknown",
    ];
    STOP.contains(&lowered)
}

#[cfg(test)]
mod tests {
    use super::*;

    use plaudon_ai_contracts::extraction::{
        ActionItem, ActionStatus, Assertion, CalendarDate, Decision, OwnerRef, Risk,
    };
    use plaudon_domain::ids::TranscriptRevisionId;

    fn action(owner: OwnerRef, due: DueDate, description: &str) -> ExtractedItem {
        ExtractedItem::ActionItem(ActionItem {
            verb: "Send".into(),
            description: description.into(),
            owner,
            due_date: due,
            status: ActionStatus::Open,
            assertion: Assertion::DirectlyStated,
            anchors: Vec::new(),
        })
    }

    fn decision(statement: &str) -> ExtractedItem {
        ExtractedItem::Decision(Decision {
            statement: statement.into(),
            participants: Vec::new(),
            time_reference: OptionalText::NotSpecified,
            assertion: Assertion::DirectlyStated,
            anchors: Vec::new(),
        })
    }

    #[test]
    fn a_monetary_decision_is_high_consequence() {
        assert_eq!(
            classify_item(&decision("We approved the £40,000 variation")),
            ClaimCategory::FinancialAmount
        );
        assert_eq!(
            classify_item(&decision("We approved 40000 USD for the variation")),
            ClaimCategory::FinancialAmount
        );
    }

    #[test]
    fn a_safety_commitment_outranks_money() {
        assert_eq!(
            classify_item(&decision("We will spend $5,000 on new safety harnesses")),
            ClaimCategory::SafetyCommitment
        );
    }

    #[test]
    fn an_action_with_a_real_date_is_a_deadline() {
        assert_eq!(
            classify_item(&action(
                OwnerRef::Named("Ana".into()),
                DueDate::On(CalendarDate::parse("2026-03-04").unwrap()),
                "the revised drawings"
            )),
            ClaimCategory::Deadline
        );
    }

    #[test]
    fn an_action_without_a_date_or_money_is_general() {
        assert_eq!(
            classify_item(&action(
                OwnerRef::Unassigned,
                DueDate::NotSpecified,
                "the meeting notes"
            )),
            ClaimCategory::General
        );
    }

    #[test]
    fn an_obligation_is_detected_from_contract_language() {
        assert_eq!(
            classify_item(&decision("We are contractually liable for the delay")),
            ClaimCategory::Obligation
        );
    }

    #[test]
    fn a_risk_consequence_contributes_to_classification() {
        let risk = ExtractedItem::Risk(Risk {
            description: "The scaffold is unstable".into(),
            consequence: OptionalText::Stated("Risk of injury to the crew".into()),
            mitigation: OptionalText::NotSpecified,
            anchors: Vec::new(),
        });
        assert_eq!(classify_item(&risk), ClaimCategory::SafetyCommitment);
    }

    #[test]
    fn an_unsupported_span_fails_the_entailment_spot_check() {
        assert_eq!(
            spot_check_entailment(
                "Send the revised quote — owner Bevan, due 2026-03-04",
                &["we should look at the quote again"]
            ),
            EntailmentCheck::Unsupported
        );
    }

    #[test]
    fn a_supported_span_passes_the_entailment_spot_check() {
        assert_eq!(
            spot_check_entailment(
                "Send the revised quote",
                &["Bevan will send the revised quote tomorrow"]
            ),
            EntailmentCheck::Supported
        );
    }

    #[test]
    fn a_falsified_amount_is_caught() {
        assert_eq!(
            spot_check_entailment(
                "We approved 40000 for the variation",
                &["we approved 4000 for the variation"]
            ),
            EntailmentCheck::Unsupported
        );
    }

    #[test]
    fn no_verified_span_means_not_checked_rather_than_supported() {
        assert_eq!(
            spot_check_entailment("anything at all", &[]),
            EntailmentCheck::NotChecked
        );
    }

    #[test]
    fn sentinel_words_do_not_count_against_entailment() {
        assert_eq!(
            spot_check_entailment(
                "Send the notes — owner Unassigned, due Not specified",
                &["Could somebody send the notes"]
            ),
            EntailmentCheck::Supported
        );
    }

    #[test]
    fn verify_anchor_delegates_to_the_domain_resolver() {
        let revision = crate::pipeline::test_support::revision(&["Alpha beta gamma."]);
        let good = SourceAnchor::new(revision.id, 0, 0, 5, "Alpha");
        assert!(verify_anchor(&good, &revision).is_verified());
        let bad = SourceAnchor::new(TranscriptRevisionId::new(), 0, 0, 5, "Alpha");
        assert!(!verify_anchor(&bad, &revision).is_verified());
    }
}
