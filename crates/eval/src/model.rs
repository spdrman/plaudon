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
        _request: StructuredRequest,
        _ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        // Apply mutation profile
        let body = self.build_extraction_response();
        let mutated = self.profile.apply(body, &self.fixture, self.revision_id);

        Ok(StructuredResponse {
            body: mutated,
            repair_attempts: 0,
            usage: None,
        })
    }
}
