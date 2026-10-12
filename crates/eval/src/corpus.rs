//! Corpus loading and validation for evaluation fixtures.

use std::collections::HashMap;
use std::path::Path;

use plaudon_domain::ids::TranscriptRevisionId;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{AlignmentOrigin, TranscriptRevision, TranscriptSegment};
use serde::{Deserialize, Serialize};

/// Evidence pointer in corpus reference data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePointer {
    /// Segment index.
    pub seq_index: u32,
    /// Unicode character start offset.
    pub char_start: u32,
    /// Unicode character end offset (exclusive).
    pub char_end: u32,
}

/// Reference decision in corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceDecision {
    /// The decision statement.
    pub statement: String,
    /// Participants involved.
    #[serde(default)]
    pub participants: Vec<String>,
    /// Time reference.
    pub time_reference: String,
    /// Assertion type.
    pub assertion: String,
    /// Evidence pointer.
    pub evidence: EvidencePointer,
}

/// Reference action item in corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceActionItem {
    /// Verb.
    pub verb: String,
    /// Description.
    pub description: String,
    /// Owner.
    pub owner: String,
    /// Due date.
    pub due_date: String,
    /// Status.
    pub status: String,
    /// Assertion type.
    pub assertion: String,
    /// Evidence pointer.
    pub evidence: EvidencePointer,
}

/// Reference risk in corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRisk {
    /// Description.
    pub description: String,
    /// Consequence.
    pub consequence: String,
    /// Mitigation.
    pub mitigation: String,
    /// Evidence pointer.
    pub evidence: EvidencePointer,
}

/// Reference question in corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceQuestion {
    /// The question.
    pub question: String,
    /// Owner.
    pub owner: String,
    /// Evidence pointer.
    pub evidence: EvidencePointer,
}

/// Reference topic in corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceTopic {
    /// Heading.
    pub heading: String,
    /// First segment.
    pub first_segment: u32,
    /// Last segment.
    pub last_segment: u32,
    /// Evidence pointer.
    pub evidence: EvidencePointer,
}

/// Reference executive summary statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceExecSummary {
    /// Summary text.
    pub text: String,
    /// Facet.
    pub facet: String,
    /// Evidence pointer.
    pub evidence: EvidencePointer,
}

/// Reference data for a corpus fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceData {
    /// Decisions.
    #[serde(default)]
    pub decisions: Vec<ReferenceDecision>,
    /// Action items.
    #[serde(default)]
    pub action_items: Vec<ReferenceActionItem>,
    /// Risks.
    #[serde(default)]
    pub risks: Vec<ReferenceRisk>,
    /// Questions.
    #[serde(default)]
    pub questions: Vec<ReferenceQuestion>,
    /// Topics.
    #[serde(default)]
    pub topics: Vec<ReferenceTopic>,
    /// Executive summary.
    #[serde(default)]
    pub executive_summary: Vec<ReferenceExecSummary>,
}

/// Segment in the corpus format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusSegment {
    /// Sequence index.
    pub seq_index: u32,
    /// Start time in milliseconds.
    pub start_ms: u64,
    /// End time in milliseconds.
    pub end_ms: u64,
    /// Speaker label.
    pub speaker: String,
    /// Segment text.
    pub text: String,
}

/// A corpus fixture loaded from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusFixture {
    /// Fixture ID.
    pub id: String,
    /// Title.
    pub title: String,
    /// Template ID.
    pub template_id: String,
    /// Locale.
    pub locale: String,
    /// Provenance statement.
    pub provenance: String,
    /// Speaker names.
    pub speakers: Vec<String>,
    /// Segments.
    pub segments: Vec<CorpusSegment>,
    /// Reference data.
    pub reference: ReferenceData,
}

/// Error loading or validating a corpus fixture.
#[derive(Debug, thiserror::Error)]
pub enum CorpusLoadError {
    /// File could not be read.
    #[error("failed to read corpus file: {0}")]
    Io(#[from] std::io::Error),
    /// JSON deserialization failed.
    #[error("failed to parse corpus JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Evidence pointer validation failed.
    #[error("evidence validation failed for {kind} item at index {index}: {reason}")]
    InvalidEvidence {
        /// Item kind.
        kind: String,
        /// Item index.
        index: usize,
        /// Failure reason.
        reason: String,
    },
}

impl CorpusFixture {
    /// Build a `TranscriptRevision` from this corpus fixture.
    ///
    /// Uses `native` alignment origin and creates timed segments.
    #[must_use]
    pub fn to_transcript_revision(&self) -> TranscriptRevision {
        use plaudon_domain::ids::RecordingId;
        use plaudon_domain::media::Sha256Digest;
        use plaudon_domain::model::{EngineId, ModelIdentity};
        use plaudon_domain::timerange::SourceTimeMs;
        use plaudon_domain::transcript::{RevisionOrigin, RevisionStatus, TranscriptRevisionDraft};

        let segments: Vec<TranscriptSegment> = self
            .segments
            .iter()
            .map(|seg| TranscriptSegment {
                seq_index: seg.seq_index,
                start: Some(SourceTimeMs::new(seg.start_ms)),
                end: Some(SourceTimeMs::new(seg.end_ms)),
                text: seg.text.clone(),
                confidence: None,
                speaker_id: None,
                words: Vec::new(),
            })
            .collect();

        let draft = TranscriptRevisionDraft {
            recording_id: RecordingId::new(),
            parent_id: None,
            origin: RevisionOrigin::Machine,
            input_sha256: Sha256Digest::from_bytes([0; 32]),
            engine: EngineId::new("fake"),
            model: ModelIdentity {
                model_id: "corpus-fixture".to_owned(),
                engine: EngineId::new("fake"),
                model_version: None,
            },
            locale: None,
            alignment_origin: AlignmentOrigin::Native,
            status: RevisionStatus::Complete,
            normalization_profile: "production".to_owned(),
            chunking_strategy: "v1".to_owned(),
            duration_ms: self.segments.last().map(|s| s.end_ms),
            created_at: Timestamp::now(),
            segments,
        };

        TranscriptRevision::try_new(draft).expect("corpus segments are valid")
    }

    /// Validate that all evidence pointers are exact substrings.
    ///
    /// This is a hard requirement: a corpus file with invalid evidence is rejected.
    pub fn validate_evidence(&self) -> Result<(), CorpusLoadError> {
        // Build a lookup for segments by seq_index
        let mut segments_map: HashMap<u32, &CorpusSegment> = HashMap::new();
        for seg in &self.segments {
            segments_map.insert(seg.seq_index, seg);
        }

        // Check decisions
        for (idx, decision) in self.reference.decisions.iter().enumerate() {
            validate_evidence_pointer(&decision.evidence, &segments_map, "decision", idx)?;
        }

        // Check action items
        for (idx, action) in self.reference.action_items.iter().enumerate() {
            validate_evidence_pointer(&action.evidence, &segments_map, "action_item", idx)?;
        }

        // Check risks
        for (idx, risk) in self.reference.risks.iter().enumerate() {
            validate_evidence_pointer(&risk.evidence, &segments_map, "risk", idx)?;
        }

        // Check questions
        for (idx, question) in self.reference.questions.iter().enumerate() {
            validate_evidence_pointer(&question.evidence, &segments_map, "question", idx)?;
        }

        // Check topics
        for (idx, topic) in self.reference.topics.iter().enumerate() {
            validate_evidence_pointer(&topic.evidence, &segments_map, "topic", idx)?;
        }

        // Check executive summary
        for (idx, summary) in self.reference.executive_summary.iter().enumerate() {
            validate_evidence_pointer(&summary.evidence, &segments_map, "executive_summary", idx)?;
        }

        Ok(())
    }

    /// Compute SHA256 of the corpus file content.
    #[must_use]
    pub fn content_sha256(&self) -> String {
        use sha2::{Digest, Sha256};
        let serialized = serde_json::to_vec(self).unwrap_or_default();
        let hash = Sha256::digest(&serialized);
        hex::encode(hash)
    }
}

/// Validate one evidence pointer against the segments.
fn validate_evidence_pointer(
    evidence: &EvidencePointer,
    segments: &HashMap<u32, &CorpusSegment>,
    kind: &str,
    index: usize,
) -> Result<(), CorpusLoadError> {
    let segment =
        segments
            .get(&evidence.seq_index)
            .ok_or_else(|| CorpusLoadError::InvalidEvidence {
                kind: kind.to_owned(),
                index,
                reason: format!("segment {} not found", evidence.seq_index),
            })?;

    if evidence.char_end <= evidence.char_start {
        return Err(CorpusLoadError::InvalidEvidence {
            kind: kind.to_owned(),
            index,
            reason: format!(
                "empty span: char_start={}, char_end={}",
                evidence.char_start, evidence.char_end
            ),
        });
    }

    let segment_chars: Vec<char> = segment.text.chars().collect();
    let segment_len = segment_chars.len();

    if evidence.char_end as usize > segment_len {
        return Err(CorpusLoadError::InvalidEvidence {
            kind: kind.to_owned(),
            index,
            reason: format!(
                "span out of bounds: char_end={}, segment_len={}",
                evidence.char_end, segment_len
            ),
        });
    }

    // Extract the substring
    let extracted: String = segment_chars[evidence.char_start as usize..evidence.char_end as usize]
        .iter()
        .collect();

    if extracted.is_empty() {
        return Err(CorpusLoadError::InvalidEvidence {
            kind: kind.to_owned(),
            index,
            reason: "extracted substring is empty".to_owned(),
        });
    }

    Ok(())
}

/// Load and validate a corpus file.
///
/// Returns an error if the file cannot be read, parsed, or if evidence validation fails.
pub fn load_corpus_file(path: impl AsRef<Path>) -> Result<CorpusFixture, CorpusLoadError> {
    let content = std::fs::read_to_string(path)?;
    let fixture: CorpusFixture = serde_json::from_str(&content)?;
    fixture.validate_evidence()?;
    Ok(fixture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_validation_rejects_out_of_bounds() {
        let mut fixture = CorpusFixture {
            id: "test".to_owned(),
            title: "Test".to_owned(),
            template_id: "general_meeting".to_owned(),
            locale: "en-US".to_owned(),
            provenance: "test".to_owned(),
            speakers: vec!["A".to_owned()],
            segments: vec![CorpusSegment {
                seq_index: 0,
                start_ms: 0,
                end_ms: 1000,
                speaker: "A".to_owned(),
                text: "Hello".to_owned(), // 5 chars
            }],
            reference: ReferenceData {
                decisions: vec![ReferenceDecision {
                    statement: "test".to_owned(),
                    participants: vec![],
                    time_reference: "not_specified".to_owned(),
                    assertion: "directly_stated".to_owned(),
                    evidence: EvidencePointer {
                        seq_index: 0,
                        char_start: 0,
                        char_end: 10, // Out of bounds
                    },
                }],
                action_items: vec![],
                risks: vec![],
                questions: vec![],
                topics: vec![],
                executive_summary: vec![],
            },
        };

        let result = fixture.validate_evidence();
        assert!(result.is_err());
    }

    #[test]
    fn evidence_validation_accepts_valid_span() {
        let fixture = CorpusFixture {
            id: "test".to_owned(),
            title: "Test".to_owned(),
            template_id: "general_meeting".to_owned(),
            locale: "en-US".to_owned(),
            provenance: "test".to_owned(),
            speakers: vec!["A".to_owned()],
            segments: vec![CorpusSegment {
                seq_index: 0,
                start_ms: 0,
                end_ms: 1000,
                speaker: "A".to_owned(),
                text: "Hello world".to_owned(),
            }],
            reference: ReferenceData {
                decisions: vec![ReferenceDecision {
                    statement: "test".to_owned(),
                    participants: vec![],
                    time_reference: "not_specified".to_owned(),
                    assertion: "directly_stated".to_owned(),
                    evidence: EvidencePointer {
                        seq_index: 0,
                        char_start: 0,
                        char_end: 5,
                    },
                }],
                action_items: vec![],
                risks: vec![],
                questions: vec![],
                topics: vec![],
                executive_summary: vec![],
            },
        };

        assert!(fixture.validate_evidence().is_ok());
    }
}
