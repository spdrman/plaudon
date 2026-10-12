//! Claim states and review requirements (spec 9.6A, 17.3, task I2.2).
//!
//! Spec 9.6A is blunt: a segment link establishes that the model referenced
//! existing text, **not** that the text entails the claim. The type system
//! here encodes that distinction so it cannot be lost downstream:
//!
//! * A model never produces a [`Claim`]. It produces a [`ClaimDraft`], which
//!   has no state field at all and cannot name one.
//! * [`Claim::from_draft`] can only ever yield [`ClaimState::SourceLinked`] or
//!   [`ClaimState::Unverified`]; the decision is made by anchor verification,
//!   not by the model's assertion.
//! * [`ClaimState::HumanConfirmed`] is reachable only through
//!   [`Claim::confirm`], which requires a [`HumanConfirmation`] — a value a
//!   deserialized model response cannot construct, because it is not part of
//!   the draft schema.
//!
//! High-consequence claims (amounts, obligations, deadlines, safety
//! commitments) are additionally marked **review required** and are excluded
//! from automatic downstream dispatch until a human acts.

use serde::{Deserialize, Serialize};

use crate::anchor::{AnchorRejection, AnchorVerification, SourceAnchor, VerifiedAnchor};
use crate::ids::ClaimId;
use crate::time::Timestamp;

/// Verification state of one generated claim (spec 9.6A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimState {
    /// At least one anchor resolves to real text in the exact input revision.
    ///
    /// This is a traceability statement, not a truth statement.
    SourceLinked,
    /// A human reviewed the claim against its cited spans and accepted it.
    HumanConfirmed,
    /// No anchor resolved, or the claim was reviewed and rejected.
    Unverified,
}

impl ClaimState {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceLinked => "source_linked",
            Self::HumanConfirmed => "human_confirmed",
            Self::Unverified => "unverified",
        }
    }

    /// Whether the claim may appear in a "verified decisions" view (spec 9.6).
    ///
    /// Only a human confirmation qualifies. A `SourceLinked` claim is shown in
    /// the ordinary review list with its anchors, never in the verified view.
    #[must_use]
    pub const fn is_verified_view_eligible(self) -> bool {
        matches!(self, Self::HumanConfirmed)
    }
}

/// Which §9.2 output type a claim belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// A sentence of the executive summary.
    SummaryStatement,
    /// A decision.
    Decision,
    /// An action item.
    ActionItem,
    /// A risk or issue.
    Risk,
    /// An open question.
    Question,
    /// A topic or highlight heading.
    Topic,
}

impl ClaimKind {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SummaryStatement => "summary_statement",
            Self::Decision => "decision",
            Self::ActionItem => "action_item",
            Self::Risk => "risk",
            Self::Question => "question",
            Self::Topic => "topic",
        }
    }
}

/// Consequence class of a claim (spec 9.6A, 17.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimCategory {
    /// Nothing in the claim triggers additional validation.
    General,
    /// The claim states a monetary amount.
    FinancialAmount,
    /// The claim states a commitment one party owes another.
    Obligation,
    /// The claim states a date or deadline.
    Deadline,
    /// The claim states a safety-relevant commitment.
    SafetyCommitment,
}

impl ClaimCategory {
    /// Every category, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::General,
        Self::FinancialAmount,
        Self::Obligation,
        Self::Deadline,
        Self::SafetyCommitment,
    ];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::FinancialAmount => "financial_amount",
            Self::Obligation => "obligation",
            Self::Deadline => "deadline",
            Self::SafetyCommitment => "safety_commitment",
        }
    }

    /// Parse a persisted wire string.
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == raw)
    }

    /// Whether spec 9.6A requires additional validation for this category.
    #[must_use]
    pub const fn is_high_consequence(self) -> bool {
        !matches!(self, Self::General)
    }
}

/// Why a claim is presented as **Review required**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReason {
    /// The claim is in a high-consequence category (spec 9.6A).
    HighConsequence,
    /// No anchor resolved against the input revision.
    GroundingFailed,
    /// The claim carried no anchor at all although the template required one.
    MissingAnchor,
    /// The entailment spot-check did not find the claim supported by its spans.
    EntailmentUnsupported,
}

impl ReviewReason {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HighConsequence => "high_consequence",
            Self::GroundingFailed => "grounding_failed",
            Self::MissingAnchor => "missing_anchor",
            Self::EntailmentUnsupported => "entailment_unsupported",
        }
    }
}

/// Result of the entailment spot-check against the exact cited spans (spec 9.6A).
///
/// This is deliberately three-valued. We do not ship a natural-language
/// inference model in V1, so the honest outcomes are "the cited span contains
/// the claim's own content words", "it demonstrably does not", and "we did not
/// check".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntailmentCheck {
    /// The cited spans support the claim's content words.
    Supported,
    /// The cited spans do not contain the claim's distinguishing content.
    Unsupported,
    /// No spot-check ran.
    NotChecked,
}

/// A human's explicit review decision.
///
/// There is no `Serialize`-only shortcut into [`ClaimState::HumanConfirmed`]:
/// the only constructors are [`HumanConfirmation::accept`] and
/// [`HumanConfirmation::reject`], both of which require a reviewer and an
/// instant supplied by the application, never by a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanConfirmation {
    /// Local reviewer label. There are no accounts; this is the OS user or `local`.
    pub reviewer: String,
    /// When the review happened.
    pub reviewed_at: Timestamp,
    /// Whether the reviewer accepted the claim.
    pub accepted: bool,
    /// Optional short note the reviewer typed.
    pub note: Option<String>,
}

impl HumanConfirmation {
    /// Record an acceptance.
    #[must_use]
    pub fn accept(reviewer: impl Into<String>, reviewed_at: Timestamp) -> Self {
        Self {
            reviewer: reviewer.into(),
            reviewed_at,
            accepted: true,
            note: None,
        }
    }

    /// Record a rejection.
    #[must_use]
    pub fn reject(reviewer: impl Into<String>, reviewed_at: Timestamp) -> Self {
        Self {
            reviewer: reviewer.into(),
            reviewed_at,
            accepted: false,
            note: None,
        }
    }

    /// Attach a reviewer note.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// What a model is allowed to produce for one extracted item.
///
/// Note what is absent: there is no state, no `verified` flag and no
/// `review_required` field. A model cannot assert its own trustworthiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraft {
    /// JSON Pointer of the item inside the output body, e.g. `/decisions/0`.
    pub item_path: String,
    /// Which §9.2 output type the item belongs to.
    pub kind: ClaimKind,
    /// The claim text as rendered to the user.
    pub text: String,
    /// Anchors the model supplied.
    pub anchors: Vec<SourceAnchor>,
}

impl ClaimDraft {
    /// Construct a draft.
    #[must_use]
    pub fn new(
        item_path: impl Into<String>,
        kind: ClaimKind,
        text: impl Into<String>,
        anchors: Vec<SourceAnchor>,
    ) -> Self {
        Self {
            item_path: item_path.into(),
            kind,
            text: text.into(),
            anchors,
        }
    }
}

/// One anchor of a claim, together with its verification outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAnchor {
    /// The anchor exactly as the model supplied it.
    pub anchor: SourceAnchor,
    /// Outcome of resolving it against the immutable input revision.
    pub verification: AnchorVerification,
}

impl ClaimAnchor {
    /// The resolved anchor, when it verified.
    #[must_use]
    pub const fn verified(&self) -> Option<&VerifiedAnchor> {
        self.verification.verified()
    }

    /// The rejection, when it did not.
    #[must_use]
    pub const fn rejection(&self) -> Option<&AnchorRejection> {
        self.verification.rejection()
    }
}

/// Everything a repository needs to rebuild a [`Claim`] it previously stored.
///
/// A separate record type keeps the private state fields of [`Claim`] private
/// while still allowing exact reconstruction, including a prior human
/// confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClaim {
    /// Claim identity.
    pub id: ClaimId,
    /// JSON Pointer of the item inside the output body.
    pub item_path: String,
    /// Which §9.2 output type the item belongs to.
    pub kind: ClaimKind,
    /// Consequence class.
    pub category: ClaimCategory,
    /// The claim text as rendered to the user.
    pub text: String,
    /// Anchors with their stored verification outcomes.
    pub anchors: Vec<ClaimAnchor>,
    /// Stored entailment spot-check outcome.
    pub entailment: EntailmentCheck,
    /// Stored state.
    pub state: ClaimState,
    /// Stored review reasons.
    pub review_reasons: Vec<ReviewReason>,
    /// Stored human confirmation, when one happened.
    pub confirmation: Option<HumanConfirmation>,
}

/// A generated claim with its grounding state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// Claim identity.
    pub id: ClaimId,
    /// JSON Pointer of the item inside the output body.
    pub item_path: String,
    /// Which §9.2 output type the item belongs to.
    pub kind: ClaimKind,
    /// Consequence class.
    pub category: ClaimCategory,
    /// The claim text as rendered to the user.
    pub text: String,
    /// Anchors with their verification outcomes.
    pub anchors: Vec<ClaimAnchor>,
    /// Entailment spot-check outcome against the exact cited spans.
    pub entailment: EntailmentCheck,
    state: ClaimState,
    review_reasons: Vec<ReviewReason>,
    confirmation: Option<HumanConfirmation>,
}

impl Claim {
    /// Build a claim from a model draft and the verification of its anchors.
    ///
    /// The resulting state is a function of the verification outcome alone.
    /// `anchors_required` comes from the template (spec 9.3) and decides
    /// whether an anchorless item is admissible at all.
    #[must_use]
    pub fn from_draft(
        draft: ClaimDraft,
        verifications: Vec<AnchorVerification>,
        category: ClaimCategory,
        entailment: EntailmentCheck,
        anchors_required: bool,
    ) -> Self {
        debug_assert_eq!(
            draft.anchors.len(),
            verifications.len(),
            "one verification per supplied anchor"
        );
        let anchors: Vec<ClaimAnchor> = draft
            .anchors
            .into_iter()
            .zip(verifications)
            .map(|(anchor, verification)| ClaimAnchor {
                anchor,
                verification,
            })
            .collect();

        let any_verified = anchors.iter().any(|a| a.verification.is_verified());
        let mut review_reasons = Vec::new();
        if anchors.is_empty() && anchors_required {
            review_reasons.push(ReviewReason::MissingAnchor);
        }
        if !any_verified && !anchors.is_empty() {
            review_reasons.push(ReviewReason::GroundingFailed);
        }
        if entailment == EntailmentCheck::Unsupported {
            review_reasons.push(ReviewReason::EntailmentUnsupported);
        }
        if category.is_high_consequence() {
            review_reasons.push(ReviewReason::HighConsequence);
        }

        let state = if any_verified {
            ClaimState::SourceLinked
        } else {
            ClaimState::Unverified
        };

        Self {
            id: ClaimId::new(),
            item_path: draft.item_path,
            kind: draft.kind,
            category,
            text: draft.text,
            anchors,
            entailment,
            state,
            review_reasons,
            confirmation: None,
        }
    }

    /// Reassemble a persisted claim.
    ///
    /// Used by the repository only. The state is taken from storage because
    /// storage is where a previous human confirmation lives.
    #[must_use]
    pub fn from_stored(record: StoredClaim) -> Self {
        Self {
            id: record.id,
            item_path: record.item_path,
            kind: record.kind,
            category: record.category,
            text: record.text,
            anchors: record.anchors,
            entailment: record.entailment,
            state: record.state,
            review_reasons: record.review_reasons,
            confirmation: record.confirmation,
        }
    }

    /// Current state. There is no setter.
    #[must_use]
    pub const fn state(&self) -> ClaimState {
        self.state
    }

    /// Reasons this claim is presented as **Review required**.
    #[must_use]
    pub fn review_reasons(&self) -> &[ReviewReason] {
        &self.review_reasons
    }

    /// Whether this claim is presented as **Review required** by default.
    #[must_use]
    pub fn review_required(&self) -> bool {
        self.confirmation.is_none() && !self.review_reasons.is_empty()
    }

    /// The recorded human review, when one happened.
    #[must_use]
    pub const fn confirmation(&self) -> Option<&HumanConfirmation> {
        self.confirmation.as_ref()
    }

    /// Whether any anchor resolved against the immutable input revision.
    #[must_use]
    pub fn is_grounded(&self) -> bool {
        self.anchors.iter().any(|a| a.verification.is_verified())
    }

    /// Whether this claim counts towards the unsupported-claim rate (spec 17.3).
    ///
    /// A claim is unsupported when nothing links it to the transcript, or when
    /// the entailment spot-check actively contradicted it.
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        !self.is_grounded() || self.entailment == EntailmentCheck::Unsupported
    }

    /// Whether this claim may appear in a "verified decisions" view.
    #[must_use]
    pub fn is_verified_view_eligible(&self) -> bool {
        self.state.is_verified_view_eligible()
    }

    /// Whether an automated downstream action (export automation, dispatch)
    /// may consume this claim without asking (spec 9.6A).
    #[must_use]
    pub fn permits_automatic_dispatch(&self) -> bool {
        !self.review_required() && self.state != ClaimState::Unverified
    }

    /// Apply an explicit human review decision.
    ///
    /// Accepting promotes the claim to [`ClaimState::HumanConfirmed`] even when
    /// grounding failed: a human may know the claim is correct. Rejecting
    /// forces [`ClaimState::Unverified`] regardless of anchors.
    pub fn review(&mut self, confirmation: HumanConfirmation) {
        self.state = if confirmation.accepted {
            ClaimState::HumanConfirmed
        } else {
            ClaimState::Unverified
        };
        self.confirmation = Some(confirmation);
    }

    /// Accept the claim. Convenience over [`Claim::review`].
    pub fn confirm(&mut self, reviewer: impl Into<String>, reviewed_at: Timestamp) {
        self.review(HumanConfirmation::accept(reviewer, reviewed_at));
    }

    /// Reject the claim. Convenience over [`Claim::review`].
    pub fn reject(&mut self, reviewer: impl Into<String>, reviewed_at: Timestamp) {
        self.review(HumanConfirmation::reject(reviewer, reviewed_at));
    }
}

/// Aggregate grounding statistics for one generated output (spec 17.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GroundingSummary {
    /// Number of claims the output contains.
    pub total_claims: u32,
    /// Claims with at least one resolving anchor.
    pub grounded_claims: u32,
    /// Claims counted as unsupported (spec 17.3 unsupported-claim rate).
    pub unsupported_claims: u32,
    /// Claims presented as **Review required**.
    pub review_required_claims: u32,
    /// Anchors supplied by the model.
    pub total_anchors: u32,
    /// Anchors that resolved.
    pub verified_anchors: u32,
}

impl GroundingSummary {
    /// Summarize a claim set.
    #[must_use]
    pub fn of(claims: &[Claim]) -> Self {
        let mut summary = Self::default();
        for claim in claims {
            summary.total_claims += 1;
            if claim.is_grounded() {
                summary.grounded_claims += 1;
            }
            if claim.is_unsupported() {
                summary.unsupported_claims += 1;
            }
            if claim.review_required() {
                summary.review_required_claims += 1;
            }
            for anchor in &claim.anchors {
                summary.total_anchors += 1;
                if anchor.verification.is_verified() {
                    summary.verified_anchors += 1;
                }
            }
        }
        summary
    }

    /// Unsupported-claim rate in `[0, 1]`, or `None` when there are no claims.
    ///
    /// `None` is not zero: an output with no claims has no measured rate.
    #[must_use]
    pub fn unsupported_claim_rate(&self) -> Option<f64> {
        if self.total_claims == 0 {
            return None;
        }
        Some(f64::from(self.unsupported_claims) / f64::from(self.total_claims))
    }

    /// Anchor correctness in `[0, 1]`, or `None` when no anchor was supplied.
    #[must_use]
    pub fn anchor_correctness(&self) -> Option<f64> {
        if self.total_anchors == 0 {
            return None;
        }
        Some(f64::from(self.verified_anchors) / f64::from(self.total_anchors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::anchor::AnchorRejection;
    use crate::ids::TranscriptRevisionId;

    fn verified(revision: TranscriptRevisionId) -> AnchorVerification {
        AnchorVerification::Verified(crate::anchor::VerifiedAnchor {
            revision_id: revision,
            seq_index: 0,
            char_start: 0,
            char_end: 5,
            matched_text: "Alpha".into(),
            alignment_origin: crate::transcript::AlignmentOrigin::Native,
            seek_ms: Some(0),
        })
    }

    fn rejected() -> AnchorVerification {
        AnchorVerification::Rejected(AnchorRejection::SegmentNotFound { seq_index: 42 })
    }

    fn draft(anchors: Vec<SourceAnchor>) -> ClaimDraft {
        ClaimDraft::new(
            "/decisions/0",
            ClaimKind::Decision,
            "We ship Friday",
            anchors,
        )
    }

    fn anchor(revision: TranscriptRevisionId) -> SourceAnchor {
        SourceAnchor::new(revision, 0, 0, 5, "Alpha")
    }

    #[test]
    fn a_resolving_anchor_yields_source_linked_not_confirmed() {
        let revision = TranscriptRevisionId::new();
        let claim = Claim::from_draft(
            draft(vec![anchor(revision)]),
            vec![verified(revision)],
            ClaimCategory::General,
            EntailmentCheck::NotChecked,
            true,
        );
        assert_eq!(claim.state(), ClaimState::SourceLinked);
        assert!(!claim.is_verified_view_eligible());
        assert!(!claim.review_required());
    }

    #[test]
    fn a_failed_anchor_forces_unverified_and_review() {
        let revision = TranscriptRevisionId::new();
        let claim = Claim::from_draft(
            draft(vec![anchor(revision)]),
            vec![rejected()],
            ClaimCategory::General,
            EntailmentCheck::NotChecked,
            true,
        );
        assert_eq!(claim.state(), ClaimState::Unverified);
        assert!(claim.review_required());
        assert!(claim
            .review_reasons()
            .contains(&ReviewReason::GroundingFailed));
        assert!(claim.is_unsupported());
    }

    #[test]
    fn an_anchorless_claim_is_unverified_when_the_template_requires_anchors() {
        let claim = Claim::from_draft(
            draft(Vec::new()),
            Vec::new(),
            ClaimCategory::General,
            EntailmentCheck::NotChecked,
            true,
        );
        assert_eq!(claim.state(), ClaimState::Unverified);
        assert!(claim
            .review_reasons()
            .contains(&ReviewReason::MissingAnchor));
    }

    #[test]
    fn a_high_consequence_claim_is_review_required_even_when_grounded() {
        let revision = TranscriptRevisionId::new();
        for category in ClaimCategory::ALL
            .iter()
            .copied()
            .filter(|c| c.is_high_consequence())
        {
            let claim = Claim::from_draft(
                draft(vec![anchor(revision)]),
                vec![verified(revision)],
                category,
                EntailmentCheck::Supported,
                true,
            );
            assert_eq!(claim.state(), ClaimState::SourceLinked);
            assert!(claim.review_required(), "{category:?} must require review");
            assert!(!claim.permits_automatic_dispatch());
        }
    }

    #[test]
    fn only_an_explicit_review_produces_human_confirmed() {
        let revision = TranscriptRevisionId::new();
        let mut claim = Claim::from_draft(
            draft(vec![anchor(revision)]),
            vec![verified(revision)],
            ClaimCategory::Deadline,
            EntailmentCheck::Supported,
            true,
        );
        assert_ne!(claim.state(), ClaimState::HumanConfirmed);
        claim.confirm("local", Timestamp::from_millis(10));
        assert_eq!(claim.state(), ClaimState::HumanConfirmed);
        assert!(claim.is_verified_view_eligible());
        assert!(
            !claim.review_required(),
            "a reviewed claim is no longer pending"
        );
        assert_eq!(claim.confirmation().map(|c| c.accepted), Some(true));
    }

    #[test]
    fn a_rejection_forces_unverified_even_with_resolving_anchors() {
        let revision = TranscriptRevisionId::new();
        let mut claim = Claim::from_draft(
            draft(vec![anchor(revision)]),
            vec![verified(revision)],
            ClaimCategory::General,
            EntailmentCheck::Supported,
            true,
        );
        claim.reject("local", Timestamp::from_millis(11));
        assert_eq!(claim.state(), ClaimState::Unverified);
        assert!(!claim.is_verified_view_eligible());
    }

    #[test]
    fn a_model_draft_cannot_name_a_claim_state() {
        // The draft schema has no state field; an attempt to smuggle one is
        // ignored by serde rather than honoured.
        let json = serde_json::json!({
            "item_path": "/decisions/0",
            "kind": "decision",
            "text": "We ship Friday",
            "anchors": [],
            "state": "human_confirmed",
            "review_required": false
        });
        let parsed: ClaimDraft = serde_json::from_value(json).unwrap();
        let claim = Claim::from_draft(
            parsed,
            Vec::new(),
            ClaimCategory::General,
            EntailmentCheck::NotChecked,
            true,
        );
        assert_eq!(claim.state(), ClaimState::Unverified);
    }

    #[test]
    fn an_unsupported_entailment_marks_the_claim_for_review() {
        let revision = TranscriptRevisionId::new();
        let claim = Claim::from_draft(
            draft(vec![anchor(revision)]),
            vec![verified(revision)],
            ClaimCategory::General,
            EntailmentCheck::Unsupported,
            true,
        );
        assert_eq!(claim.state(), ClaimState::SourceLinked);
        assert!(claim.is_unsupported());
        assert!(claim
            .review_reasons()
            .contains(&ReviewReason::EntailmentUnsupported));
    }

    #[test]
    fn grounding_summary_reports_unknown_rather_than_zero_for_an_empty_output() {
        let summary = GroundingSummary::of(&[]);
        assert_eq!(summary.unsupported_claim_rate(), None);
        assert_eq!(summary.anchor_correctness(), None);
    }

    #[test]
    fn grounding_summary_counts_anchors_and_claims() {
        let revision = TranscriptRevisionId::new();
        let good = Claim::from_draft(
            draft(vec![anchor(revision)]),
            vec![verified(revision)],
            ClaimCategory::General,
            EntailmentCheck::Supported,
            true,
        );
        let bad = Claim::from_draft(
            draft(vec![anchor(revision), anchor(revision)]),
            vec![rejected(), rejected()],
            ClaimCategory::FinancialAmount,
            EntailmentCheck::NotChecked,
            true,
        );
        let summary = GroundingSummary::of(&[good, bad]);
        assert_eq!(summary.total_claims, 2);
        assert_eq!(summary.grounded_claims, 1);
        assert_eq!(summary.unsupported_claims, 1);
        assert_eq!(summary.review_required_claims, 1);
        assert_eq!(summary.total_anchors, 3);
        assert_eq!(summary.verified_anchors, 1);
        assert!((summary.unsupported_claim_rate().unwrap() - 0.5).abs() < f64::EPSILON);
    }
}
