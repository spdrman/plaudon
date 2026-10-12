//! Mutation profiles for testing precision/recall by injecting controlled defects.

use plaudon_domain::ids::TranscriptRevisionId;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};

use crate::corpus::CorpusFixture;

/// Seed for deterministic mutation.
pub type MutationSeed = u64;

/// A mutation profile that injects controlled defects into model responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationProfile {
    /// Perfect responses, no defects.
    Perfect,
    /// Drop a fraction of items.
    DropItems { fraction: u32 },
    /// Add fabricated items.
    FabricateItems { count: u32 },
    /// Fabricate an anchor pointing to nonexistent segment.
    FabricateAnchor,
    /// Shift character spans by a delta.
    ShiftSpan { delta: i32 },
    /// Falsify the owner field.
    FalsifyOwner,
    /// Falsify the due date field.
    FalsifyDueDate,
    /// Insert an invalid date (e.g., 2025-02-30).
    InvalidDate,
    /// Create an oversized array.
    OversizedArray,
    /// Remove anchors from items.
    MissingAnchor,
}

impl MutationProfile {
    /// Apply this profile to a model response body.
    ///
    /// Returns a mutated copy. Deterministic given the seed embedded in the profile.
    #[must_use]
    pub fn apply(
        &self,
        body: Value,
        fixture: &CorpusFixture,
        revision_id: TranscriptRevisionId,
    ) -> Value {
        match self {
            Self::Perfect => body,
            Self::DropItems { fraction } => self.apply_drop_items(body, *fraction),
            Self::FabricateItems { count } => self.apply_fabricate_items(body, *count, revision_id),
            Self::FabricateAnchor => self.apply_fabricate_anchor(body, revision_id),
            Self::ShiftSpan { delta } => self.apply_shift_span(body, *delta),
            Self::FalsifyOwner => self.apply_falsify_owner(body),
            Self::FalsifyDueDate => self.apply_falsify_due_date(body),
            Self::InvalidDate => self.apply_invalid_date(body),
            Self::OversizedArray => self.apply_oversized_array(body),
            Self::MissingAnchor => self.apply_missing_anchor(body),
        }
    }

    fn apply_drop_items(&self, mut body: Value, fraction: u32) -> Value {
        // Drop items from decisions and action_items
        if let Some(obj) = body.as_object_mut() {
            for key in &["decisions", "action_items"] {
                if let Some(Value::Array(arr)) = obj.get_mut(*key) {
                    let keep_count = (arr.len() * (100 - fraction as usize)) / 100;
                    arr.truncate(keep_count);
                }
            }
        }
        body
    }

    fn apply_fabricate_items(
        &self,
        mut body: Value,
        count: u32,
        revision_id: TranscriptRevisionId,
    ) -> Value {
        if let Some(obj) = body.as_object_mut() {
            if let Some(Value::Array(arr)) = obj.get_mut("decisions") {
                for i in 0..count {
                    arr.push(json!({
                        "kind": "decision",
                        "statement": format!("Fabricated decision {}", i),
                        "participants": [],
                        "time_reference": "not_specified",
                        "assertion": "model_interpretation",
                        "anchors": [{
                            "revision_id": revision_id.to_string(),
                            "seq_index": 0,
                            "char_start": 0,
                            "char_end": 5,
                            "excerpt": "Fabri"
                        }]
                    }));
                }
            }
        }
        body
    }

    fn apply_fabricate_anchor(&self, mut body: Value, revision_id: TranscriptRevisionId) -> Value {
        if let Some(obj) = body.as_object_mut() {
            if let Some(Value::Array(arr)) = obj.get_mut("decisions") {
                if let Some(item) = arr.first_mut() {
                    if let Some(item_obj) = item.as_object_mut() {
                        // Replace anchors with one pointing to nonexistent segment 999
                        item_obj.insert(
                            "anchors".to_owned(),
                            json!([{
                                "revision_id": revision_id.to_string(),
                                "seq_index": 999,
                                "char_start": 0,
                                "char_end": 5,
                                "excerpt": "Oops!"
                            }]),
                        );
                    }
                }
            }
        }
        body
    }

    fn apply_shift_span(&self, mut body: Value, delta: i32) -> Value {
        if let Some(obj) = body.as_object_mut() {
            for key in &["decisions", "action_items"] {
                if let Some(Value::Array(arr)) = obj.get_mut(*key) {
                    for item in arr {
                        if let Some(item_obj) = item.as_object_mut() {
                            if let Some(Value::Array(anchors)) = item_obj.get_mut("anchors") {
                                for anchor in anchors {
                                    if let Some(anchor_obj) = anchor.as_object_mut() {
                                        if let Some(Value::Number(start)) =
                                            anchor_obj.get("char_start")
                                        {
                                            if let Some(start_val) = start.as_u64() {
                                                let new_start =
                                                    (start_val as i64 + delta as i64).max(0);
                                                anchor_obj.insert(
                                                    "char_start".to_owned(),
                                                    json!(new_start),
                                                );
                                            }
                                        }
                                        if let Some(Value::Number(end)) = anchor_obj.get("char_end")
                                        {
                                            if let Some(end_val) = end.as_u64() {
                                                let new_end =
                                                    (end_val as i64 + delta as i64).max(1);
                                                anchor_obj
                                                    .insert("char_end".to_owned(), json!(new_end));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        body
    }

    fn apply_falsify_owner(&self, mut body: Value) -> Value {
        if let Some(obj) = body.as_object_mut() {
            if let Some(Value::Array(arr)) = obj.get_mut("action_items") {
                for item in arr {
                    if let Some(item_obj) = item.as_object_mut() {
                        item_obj.insert("owner".to_owned(), json!("Wrong Person"));
                    }
                }
            }
        }
        body
    }

    fn apply_falsify_due_date(&self, mut body: Value) -> Value {
        if let Some(obj) = body.as_object_mut() {
            if let Some(Value::Array(arr)) = obj.get_mut("action_items") {
                for item in arr {
                    if let Some(item_obj) = item.as_object_mut() {
                        item_obj.insert("due_date".to_owned(), json!("2099-12-31"));
                    }
                }
            }
        }
        body
    }

    fn apply_invalid_date(&self, mut body: Value) -> Value {
        if let Some(obj) = body.as_object_mut() {
            if let Some(Value::Array(arr)) = obj.get_mut("action_items") {
                if let Some(item) = arr.first_mut() {
                    if let Some(item_obj) = item.as_object_mut() {
                        item_obj.insert("due_date".to_owned(), json!("2025-02-30"));
                    }
                }
            }
        }
        body
    }

    fn apply_oversized_array(&self, mut body: Value) -> Value {
        if let Some(obj) = body.as_object_mut() {
            if let Some(Value::Array(arr)) = obj.get_mut("decisions") {
                // Duplicate items to exceed reasonable size
                let original = arr.clone();
                for _ in 0..50 {
                    arr.extend(original.clone());
                }
            }
        }
        body
    }

    fn apply_missing_anchor(&self, mut body: Value) -> Value {
        if let Some(obj) = body.as_object_mut() {
            for key in &["decisions", "action_items"] {
                if let Some(Value::Array(arr)) = obj.get_mut(*key) {
                    for item in arr {
                        if let Some(item_obj) = item.as_object_mut() {
                            item_obj.insert("anchors".to_owned(), json!([]));
                        }
                    }
                }
            }
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_domain::ids::TranscriptRevisionId;

    #[test]
    fn perfect_profile_returns_unchanged() {
        let body = json!({"decisions": [], "action_items": []});
        let fixture = crate::corpus::CorpusFixture {
            id: "test".to_owned(),
            title: "Test".to_owned(),
            template_id: "general_meeting".to_owned(),
            locale: "en-US".to_owned(),
            provenance: "test".to_owned(),
            speakers: vec![],
            segments: vec![],
            reference: crate::corpus::ReferenceData {
                decisions: vec![],
                action_items: vec![],
                risks: vec![],
                questions: vec![],
                topics: vec![],
                executive_summary: vec![],
            },
        };
        let result =
            MutationProfile::Perfect.apply(body.clone(), &fixture, TranscriptRevisionId::new());
        assert_eq!(result, body);
    }

    #[test]
    fn drop_items_reduces_count() {
        let body = json!({
            "decisions": [
                {"kind": "decision", "statement": "A"},
                {"kind": "decision", "statement": "B"},
                {"kind": "decision", "statement": "C"},
                {"kind": "decision", "statement": "D"}
            ],
            "action_items": []
        });
        let fixture = crate::corpus::CorpusFixture {
            id: "test".to_owned(),
            title: "Test".to_owned(),
            template_id: "general_meeting".to_owned(),
            locale: "en-US".to_owned(),
            provenance: "test".to_owned(),
            speakers: vec![],
            segments: vec![],
            reference: crate::corpus::ReferenceData {
                decisions: vec![],
                action_items: vec![],
                risks: vec![],
                questions: vec![],
                topics: vec![],
                executive_summary: vec![],
            },
        };
        let result = MutationProfile::DropItems { fraction: 50 }.apply(
            body,
            &fixture,
            TranscriptRevisionId::new(),
        );
        assert_eq!(result["decisions"].as_array().unwrap().len(), 2);
    }
}
