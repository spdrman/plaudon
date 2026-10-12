//! §17.3 metrics computation for extraction evaluation.

use std::collections::HashSet;

use plaudon_domain::claim::{Claim, GroundingSummary};
use serde::{Deserialize, Serialize};

use crate::corpus::{CorpusFixture, EvidencePointer};

/// Matching rule for pairing predicted items with reference items.
///
/// Two items match when:
/// 1. Their evidence pointers reference overlapping segments (seq_index overlap)
/// 2. Their normalized text has similarity above a threshold
///
/// For this evaluation, we use a simple rule: items match if their evidence
/// segments overlap.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemMatch {
    /// Index in predicted items.
    pub predicted_index: usize,
    /// Index in reference items.
    pub reference_index: usize,
}

/// All §17.3 metrics for one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct EvalMetrics {
    // Decision metrics
    /// Decision precision.
    pub decision_precision: Option<f64>,
    /// Decision recall.
    pub decision_recall: Option<f64>,
    /// Decision F1 score.
    pub decision_f1: Option<f64>,
    /// Decision sample size.
    pub decision_sample_size: u32,

    // Action item metrics
    /// Action item precision.
    pub action_item_precision: Option<f64>,
    /// Action item recall.
    pub action_item_recall: Option<f64>,
    /// Action item F1 score.
    pub action_item_f1: Option<f64>,
    /// Action item sample size.
    pub action_item_sample_size: u32,

    // Owner and due date accuracy (over matched action items)
    /// Owner accuracy.
    pub owner_accuracy: Option<f64>,
    /// Owner sample size.
    pub owner_sample_size: u32,
    /// Due date accuracy.
    pub due_date_accuracy: Option<f64>,
    /// Due date sample size.
    pub due_date_sample_size: u32,

    // Schema and grounding
    /// Schema validity rate.
    pub schema_validity_rate: Option<f64>,
    /// Schema sample size.
    pub schema_sample_size: u32,
    /// Anchor correctness (from pipeline's GroundingSummary).
    pub anchor_correctness: Option<f64>,
    /// Anchor sample size.
    pub anchor_sample_size: u32,
    /// Unsupported claim rate.
    pub unsupported_claim_rate: Option<f64>,
    /// Unsupported claim sample size.
    pub unsupported_claim_sample_size: u32,
}

impl EvalMetrics {
    /// Compute all metrics.
    #[must_use]
    pub fn compute(
        fixture: &CorpusFixture,
        _claims: &[Claim],
        grounding: &GroundingSummary,
        body: &serde_json::Value,
        schema_valid_count: u32,
        total_item_count: u32,
    ) -> Self {
        // Extract predicted and reference items
        let predicted_decisions = extract_items(body, "decisions");
        let predicted_actions = extract_items(body, "action_items");

        let reference_decisions = &fixture.reference.decisions;
        let reference_actions = &fixture.reference.action_items;

        // Compute decision metrics
        let decision_matches = match_items(&predicted_decisions, reference_decisions);
        let (decision_precision, decision_recall, decision_f1) = compute_precision_recall_f1(
            predicted_decisions.len(),
            reference_decisions.len(),
            decision_matches.len(),
        );

        // Compute action item metrics
        let action_matches = match_items(&predicted_actions, reference_actions);
        let (action_item_precision, action_item_recall, action_item_f1) =
            compute_precision_recall_f1(
                predicted_actions.len(),
                reference_actions.len(),
                action_matches.len(),
            );

        // Compute owner and due date accuracy over matched action items
        let (owner_accuracy, owner_sample_size) =
            compute_owner_accuracy(body, reference_actions, &action_matches);
        let (due_date_accuracy, due_date_sample_size) =
            compute_due_date_accuracy(body, reference_actions, &action_matches);

        // Schema validity rate
        let schema_validity_rate = if total_item_count > 0 {
            Some(f64::from(schema_valid_count) / f64::from(total_item_count))
        } else {
            None
        };

        // Anchor correctness and unsupported claim rate from grounding summary
        let anchor_correctness = grounding.anchor_correctness();
        let unsupported_claim_rate = grounding.unsupported_claim_rate();

        Self {
            decision_precision,
            decision_recall,
            decision_f1,
            decision_sample_size: (predicted_decisions.len() + reference_decisions.len()) as u32,

            action_item_precision,
            action_item_recall,
            action_item_f1,
            action_item_sample_size: (predicted_actions.len() + reference_actions.len()) as u32,

            owner_accuracy,
            owner_sample_size,
            due_date_accuracy,
            due_date_sample_size,

            schema_validity_rate,
            schema_sample_size: total_item_count,
            anchor_correctness,
            anchor_sample_size: grounding.total_anchors,
            unsupported_claim_rate,
            unsupported_claim_sample_size: grounding.total_claims,
        }
    }
}

/// Extract items from a body field.
fn extract_items(body: &serde_json::Value, field: &str) -> Vec<serde_json::Value> {
    body.get(field)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Match predicted items to reference items based on evidence segment overlap.
fn match_items<T>(predicted: &[serde_json::Value], reference: &[T]) -> Vec<ItemMatch>
where
    T: AsRef<EvidencePointer>,
{
    let mut matches = Vec::new();
    let mut used_references: HashSet<usize> = HashSet::new();

    for (pred_idx, pred_item) in predicted.iter().enumerate() {
        if let Some(pred_segments) = extract_evidence_segments(pred_item) {
            // Find the first reference item with overlapping segments
            for (ref_idx, ref_item) in reference.iter().enumerate() {
                if used_references.contains(&ref_idx) {
                    continue;
                }
                let ref_seg = ref_item.as_ref().seq_index;
                if pred_segments.contains(&ref_seg) {
                    matches.push(ItemMatch {
                        predicted_index: pred_idx,
                        reference_index: ref_idx,
                    });
                    used_references.insert(ref_idx);
                    break;
                }
            }
        }
    }

    matches
}

/// Extract segment indices from an item's anchors.
fn extract_evidence_segments(item: &serde_json::Value) -> Option<HashSet<u32>> {
    item.get("anchors")
        .and_then(|v| v.as_array())
        .map(|anchors| {
            anchors
                .iter()
                .filter_map(|anchor| {
                    anchor
                        .get("seq_index")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as u32)
                })
                .collect()
        })
}

/// Compute precision, recall, and F1.
fn compute_precision_recall_f1(
    predicted_count: usize,
    reference_count: usize,
    match_count: usize,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let precision = if predicted_count > 0 {
        Some(match_count as f64 / predicted_count as f64)
    } else {
        None
    };

    let recall = if reference_count > 0 {
        Some(match_count as f64 / reference_count as f64)
    } else {
        None
    };

    let f1 = match (precision, recall) {
        (Some(p), Some(r)) if p + r > 0.0 => Some(2.0 * p * r / (p + r)),
        _ => None,
    };

    (precision, recall, f1)
}

/// Compute owner accuracy over matched action items.
fn compute_owner_accuracy(
    body: &serde_json::Value,
    reference: &[crate::corpus::ReferenceActionItem],
    matches: &[ItemMatch],
) -> (Option<f64>, u32) {
    if matches.is_empty() {
        return (None, 0);
    }

    let predicted_actions = extract_items(body, "action_items");
    let mut correct = 0;
    let mut total = 0;

    for m in matches {
        if let Some(pred_item) = predicted_actions.get(m.predicted_index) {
            if let Some(ref_owner) = reference.get(m.reference_index) {
                let pred_owner = pred_item
                    .get("owner")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let ref_owner_str = ref_owner.owner.as_str();
                if pred_owner == ref_owner_str {
                    correct += 1;
                }
                total += 1;
            }
        }
    }

    if total > 0 {
        (Some(correct as f64 / total as f64), total as u32)
    } else {
        (None, 0)
    }
}

/// Compute due date accuracy over matched action items.
fn compute_due_date_accuracy(
    body: &serde_json::Value,
    reference: &[crate::corpus::ReferenceActionItem],
    matches: &[ItemMatch],
) -> (Option<f64>, u32) {
    if matches.is_empty() {
        return (None, 0);
    }

    let predicted_actions = extract_items(body, "action_items");
    let mut correct = 0;
    let mut total = 0;

    for m in matches {
        if let Some(pred_item) = predicted_actions.get(m.predicted_index) {
            if let Some(ref_date) = reference.get(m.reference_index) {
                let pred_date = pred_item
                    .get("due_date")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let ref_date_str = ref_date.due_date.as_str();
                if pred_date == ref_date_str {
                    correct += 1;
                }
                total += 1;
            }
        }
    }

    if total > 0 {
        (Some(correct as f64 / total as f64), total as u32)
    } else {
        (None, 0)
    }
}

// Implement AsRef for reference types
impl AsRef<EvidencePointer> for crate::corpus::ReferenceDecision {
    fn as_ref(&self) -> &EvidencePointer {
        &self.evidence
    }
}

impl AsRef<EvidencePointer> for crate::corpus::ReferenceActionItem {
    fn as_ref(&self) -> &EvidencePointer {
        &self.evidence
    }
}

impl AsRef<str> for crate::corpus::ReferenceActionItem {
    fn as_ref(&self) -> &str {
        &self.owner
    }
}

// Wrapper for due date reference
pub struct DueDateRef<'a>(pub &'a str);

impl<'a> AsRef<str> for DueDateRef<'a> {
    fn as_ref(&self) -> &str {
        self.0
    }
}

/// Compute metrics (public API).
#[must_use]
pub fn compute_metrics(
    fixture: &CorpusFixture,
    claims: &[Claim],
    grounding: &GroundingSummary,
    body: &serde_json::Value,
    schema_valid_count: u32,
    total_item_count: u32,
) -> EvalMetrics {
    EvalMetrics::compute(
        fixture,
        claims,
        grounding,
        body,
        schema_valid_count,
        total_item_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_recall_f1_with_perfect_match() {
        let (p, r, f1) = compute_precision_recall_f1(10, 10, 10);
        assert_eq!(p, Some(1.0));
        assert_eq!(r, Some(1.0));
        assert_eq!(f1, Some(1.0));
    }

    #[test]
    fn precision_recall_with_zero_reference_gives_none_recall() {
        let (p, r, f1) = compute_precision_recall_f1(5, 0, 0);
        assert_eq!(p, Some(0.0));
        assert_eq!(r, None);
        assert_eq!(f1, None);
    }

    #[test]
    fn precision_recall_with_zero_predicted_gives_none_precision() {
        let (p, r, f1) = compute_precision_recall_f1(0, 5, 0);
        assert_eq!(p, None);
        assert_eq!(r, Some(0.0));
        assert_eq!(f1, None);
    }
}
