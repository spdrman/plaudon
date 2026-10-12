//! Reference model that answers from corpus reference data (the "perfect" model).

use std::sync::Arc;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, PricingBasis, PrivacyClass, TextModelCapabilities,
};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::ids::TranscriptRevisionId;
use plaudon_domain::model::{EngineId, ModelIdentity};
use serde_json::json;

use crate::corpus::{CorpusFixture, EvidencePointer};
use crate::mutation::MutationProfile;

/// A TextModel that returns reference answers from a corpus fixture.
///
/// This is the "perfect" model for evaluation: it always returns exactly what
/// the corpus reference says, with correct revision IDs and character offsets.
pub struct ReferenceModel {
    fixture: Arc<CorpusFixture>,
    revision_id: TranscriptRevisionId,
    profile: MutationProfile,
}

impl ReferenceModel {
    /// Create a reference model for the given fixture and revision.
    #[must_use]
    pub fn new(
        fixture: Arc<CorpusFixture>,
        revision_id: TranscriptRevisionId,
        profile: MutationProfile,
    ) -> Self {
        Self {
            fixture,
            revision_id,
            profile,
        }
    }

    /// Build the extraction response body from the corpus reference data.
    fn build_extraction_response(&self) -> serde_json::Value {
        let ref_data = &self.fixture.reference;

        let executive_summary: Vec<serde_json::Value> = ref_data
            .executive_summary
            .iter()
            .map(|item| {
                json!({
                    "kind": "summary_statement",
                    "text": item.text,
                    "facet": item.facet,
                    "anchors": [self.evidence_to_anchor(&item.evidence)]
                })
            })
            .collect();

        let decisions: Vec<serde_json::Value> = ref_data
            .decisions
            .iter()
            .map(|item| {
                json!({
                    "kind": "decision",
                    "statement": item.statement,
                    "participants": item.participants,
                    "time_reference": item.time_reference,
                    "assertion": item.assertion,
                    "anchors": [self.evidence_to_anchor(&item.evidence)]
                })
            })
            .collect();

        let action_items: Vec<serde_json::Value> = ref_data
            .action_items
            .iter()
            .map(|item| {
                json!({
                    "kind": "action_item",
                    "verb": item.verb,
                    "description": item.description,
                    "owner": item.owner,
                    "due_date": item.due_date,
                    "status": item.status,
                    "assertion": item.assertion,
                    "anchors": [self.evidence_to_anchor(&item.evidence)]
                })
            })
            .collect();

        let risks: Vec<serde_json::Value> = ref_data
            .risks
            .iter()
            .map(|item| {
                json!({
                    "kind": "risk",
                    "description": item.description,
                    "consequence": item.consequence,
                    "mitigation": item.mitigation,
                    "anchors": [self.evidence_to_anchor(&item.evidence)]
                })
            })
            .collect();

        let questions: Vec<serde_json::Value> = ref_data
            .questions
            .iter()
            .map(|item| {
                json!({
                    "kind": "question",
                    "question": item.question,
                    "owner": item.owner,
                    "anchors": [self.evidence_to_anchor(&item.evidence)]
                })
            })
            .collect();

        let topics: Vec<serde_json::Value> = ref_data
            .topics
            .iter()
            .map(|item| {
                json!({
                    "kind": "topic",
                    "heading": item.heading,
                    "first_segment": item.first_segment,
                    "last_segment": item.last_segment,
                    "anchors": [self.evidence_to_anchor(&item.evidence)]
                })
            })
            .collect();

        json!({
            "executive_summary": executive_summary,
            "decisions": decisions,
            "action_items": action_items,
            "risks": risks,
            "questions": questions,
            "topics": topics
        })
    }

    /// Convert an evidence pointer to a SourceAnchor JSON object.
    fn evidence_to_anchor(&self, evidence: &EvidencePointer) -> serde_json::Value {
        // Extract the actual text for the excerpt
        let segment = self
            .fixture
            .segments
            .iter()
            .find(|s| s.seq_index == evidence.seq_index)
            .expect("evidence validation ensures segment exists");

        let chars: Vec<char> = segment.text.chars().collect();
        let excerpt: String = chars[evidence.char_start as usize..evidence.char_end as usize]
            .iter()
            .collect();

        json!({
            "revision_id": self.revision_id.to_string(),
            "seq_index": evidence.seq_index,
            "char_start": evidence.char_start,
            "char_end": evidence.char_end,
            "excerpt": excerpt
        })
    }

    /// Build the section/consolidated summary response for one prompt.
    ///
    /// The prompt's fenced data region labels every segment it contains with
    /// `[index]`, so the reference model reports exactly the range it was
    /// actually given rather than claiming to have read the whole recording.
    fn build_summary_response(&self, prompt: &str) -> serde_json::Value {
        let (first, last) = segment_range_in(prompt).unwrap_or_else(|| {
            let last = self
                .fixture
                .segments
                .last()
                .map_or(0, |segment| segment.seq_index);
            (0, last)
        });

        let bullets: Vec<serde_json::Value> = self
            .fixture
            .reference
            .evidence_pointers()
            .into_iter()
            .filter(|(_, evidence)| evidence.seq_index >= first && evidence.seq_index <= last)
            .map(|(text, evidence)| {
                json!({
                    "text": text,
                    "anchors": [self.evidence_to_anchor(&evidence)],
                })
            })
            .collect();

        json!({
            "bullets": bullets,
            "covered_first_segment": first,
            "covered_last_segment": last,
        })
    }
}

/// Lowest and highest `[index]` marker in a fenced transcript region.
fn segment_range_in(prompt: &str) -> Option<(u32, u32)> {
    let mut first: Option<u32> = None;
    let mut last: Option<u32> = None;
    for line in prompt.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix('[') else {
            continue;
        };
        let Some(end) = rest.find(']') else {
            continue;
        };
        let Ok(index) = rest[..end].parse::<u32>() else {
            continue;
        };
        first = Some(first.map_or(index, |value: u32| value.min(index)));
        last = Some(last.map_or(index, |value: u32| value.max(index)));
    }
    Some((first?, last?))
}

#[async_trait]
impl TextModel for ReferenceModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        TextModelCapabilities {
            payload_version: TextModelCapabilities::SUPPORTED_PAYLOAD_VERSION,
            status: CapabilityStatus::Supported,
            engine_id: EngineId::new("fake"),
            model: ModelIdentity {
                model_id: "reference-model".to_owned(),
                engine: EngineId::new("fake"),
                model_version: Some("1.0.0".to_owned()),
            },
            offline: Tri::Yes,
            max_context_tokens: Known::Known(128_000),
            max_output_tokens: Known::Known(16_000),
            supports_structured_output: Tri::Yes,
            can_cancel: Tri::Yes,
            privacy_class: Known::Known(PrivacyClass::OnDevice),
            pricing_basis: Known::Known(PricingBasis::None),
            availability_reason: AvailabilityReason::Available,
        }
    }

    async fn generate_structured(
        &self,
        request: StructuredRequest,
        _ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        // The pipeline asks three different stages for three different
        // shapes. The response schema says which one: only the section and
        // consolidated summary stages require `bullets`.
        let wants_summary = request
            .output_schema
            .get("properties")
            .and_then(|properties| properties.get("bullets"))
            .is_some();

        let body = if wants_summary {
            self.build_summary_response(&request.prompt)
        } else {
            let extraction = self.build_extraction_response();
            self.profile
                .apply(extraction, &self.fixture, self.revision_id)
        };

        Ok(StructuredResponse {
            body,
            repair_attempts: 0,
            usage: None,
        })
    }
}
