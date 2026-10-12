//! The Study/Revision template: flashcards, quiz with answer key, and
//! definitions (spec 9.3, 9.7).
//!
//! Every generated artifact carries a traceable segment citation. An item
//! whose citation does not verify is **rejected with its path**, not dropped:
//! spec 9.5 requires rejecting malformed items rather than dropping silent
//! errors, so the caller learns exactly what was refused.
//!
//! These tools are optional. They are registered with `optional = true` and
//! their kinds are excluded from the spec 9.2 default meeting outputs, so the
//! meeting product is unchanged by their presence.

use plaudon_domain::transcript::TranscriptRevision;
use serde::{Deserialize, Serialize};

use crate::anchor::{partition_anchors, AnchorRejection, SourceAnchor};
use crate::error::ToolError;
use crate::tool::ToolKind;

/// Version of the study template contract.
pub const STUDY_TEMPLATE_VERSION: &str = "study-revision/1";

/// One flashcard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flashcard {
    /// Prompt side.
    pub front: String,
    /// Answer side.
    pub back: String,
    /// Citations into the pinned revision.
    pub anchors: Vec<SourceAnchor>,
}

/// One quiz item with its answer key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuizItem {
    /// The question.
    pub question: String,
    /// The correct answer.
    pub answer: String,
    /// Optional incorrect options.
    #[serde(default)]
    pub distractors: Vec<String>,
    /// Citations into the pinned revision.
    pub anchors: Vec<SourceAnchor>,
}

/// One important term and the definition the recording gives for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    /// The term.
    pub term: String,
    /// The definition as stated in the recording.
    pub definition: String,
    /// Citations into the pinned revision.
    pub anchors: Vec<SourceAnchor>,
}

/// A validated set of study artifacts.
///
/// There is no public constructor that skips validation: the only way to build
/// one is [`validate_study_artifacts`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyArtifacts {
    /// Template version that produced these artifacts.
    pub template_version: String,
    /// Flashcards, every one of them cited.
    pub flashcards: Vec<Flashcard>,
    /// Quiz items with answer key, every one of them cited.
    pub quiz: Vec<QuizItem>,
    /// Definitions, every one of them cited.
    pub definitions: Vec<Definition>,
}

impl StudyArtifacts {
    /// Whether there is nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.flashcards.is_empty() && self.quiz.is_empty() && self.definitions.is_empty()
    }

    /// Total number of items across all three kinds.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.flashcards.len() + self.quiz.len() + self.definitions.len()
    }
}

/// An item that failed validation, identified by its JSON Pointer path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedItem {
    /// JSON Pointer to the offending item, e.g. `/flashcards/3`.
    pub item_path: String,
    /// Why it was refused.
    pub reason: RejectionReason,
}

/// Why a study item was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RejectionReason {
    /// The item carried no citation at all.
    Uncited,
    /// Every citation the item carried failed verification.
    AllAnchorsRejected {
        /// The first refusal, for diagnostics.
        first: AnchorRejection,
    },
    /// A required text field was blank.
    EmptyField {
        /// Which field.
        field: String,
    },
}

/// Outcome of validating a raw study payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudyValidation {
    /// Items that passed.
    pub accepted: StudyArtifacts,
    /// Items that were refused, with their paths and reasons.
    pub rejected: Vec<RejectedItem>,
}

/// The raw shape a model returns for a study tool.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawStudyPayload {
    /// Flashcards, when the tool was `flashcards`.
    #[serde(default)]
    pub flashcards: Vec<RawStudyItem>,
    /// Quiz items, when the tool was `quiz`.
    #[serde(default)]
    pub quiz: Vec<RawQuizItem>,
    /// Definitions, when the tool was `definitions`.
    #[serde(default)]
    pub definitions: Vec<RawDefinition>,
}

/// A raw flashcard, before anchor verification.
#[derive(Debug, Clone, Deserialize)]
pub struct RawStudyItem {
    /// Prompt side.
    pub front: String,
    /// Answer side.
    pub back: String,
    /// Claimed citations.
    #[serde(default)]
    pub anchors: Vec<RawStudyAnchor>,
}

/// A raw quiz item, before anchor verification.
#[derive(Debug, Clone, Deserialize)]
pub struct RawQuizItem {
    /// The question.
    pub question: String,
    /// The correct answer.
    pub answer: String,
    /// Optional incorrect options.
    #[serde(default)]
    pub distractors: Vec<String>,
    /// Claimed citations.
    #[serde(default)]
    pub anchors: Vec<RawStudyAnchor>,
}

/// A raw definition, before anchor verification.
#[derive(Debug, Clone, Deserialize)]
pub struct RawDefinition {
    /// The term.
    pub term: String,
    /// The definition.
    pub definition: String,
    /// Claimed citations.
    #[serde(default)]
    pub anchors: Vec<RawStudyAnchor>,
}

/// A claimed citation, before it is bound to a revision.
#[derive(Debug, Clone, Deserialize)]
pub struct RawStudyAnchor {
    /// Segment position.
    pub seq_index: u32,
    /// Inclusive start, in characters.
    pub char_start: u32,
    /// Exclusive end, in characters.
    pub char_end: u32,
    /// The quoted span.
    pub excerpt: String,
}

fn bind(raw: &[RawStudyAnchor], revision: &TranscriptRevision) -> Vec<SourceAnchor> {
    raw.iter()
        .map(|a| {
            SourceAnchor::new(
                revision.id,
                a.seq_index,
                a.char_start,
                a.char_end,
                a.excerpt.clone(),
            )
        })
        .collect()
}

fn check_anchors(
    raw: &[RawStudyAnchor],
    revision: &TranscriptRevision,
) -> Result<Vec<SourceAnchor>, RejectionReason> {
    if raw.is_empty() {
        return Err(RejectionReason::Uncited);
    }
    let bound = bind(raw, revision);
    let (verified, rejected) = partition_anchors(&bound, revision);
    if verified.is_empty() {
        let first = rejected
            .first()
            .map(|(_, reason)| *reason)
            .unwrap_or(AnchorRejection::ExcerptMismatch);
        return Err(RejectionReason::AllAnchorsRejected { first });
    }
    Ok(verified)
}

fn non_empty(value: &str, field: &'static str) -> Result<(), RejectionReason> {
    if value.trim().is_empty() {
        Err(RejectionReason::EmptyField {
            field: field.to_string(),
        })
    } else {
        Ok(())
    }
}

/// Validate a raw study payload against the pinned revision.
///
/// Items whose citations verify are accepted; every other item is listed in
/// [`StudyValidation::rejected`] with its JSON Pointer path.
#[must_use]
pub fn validate_study_artifacts(
    payload: &RawStudyPayload,
    revision: &TranscriptRevision,
) -> StudyValidation {
    let mut accepted = StudyArtifacts {
        template_version: STUDY_TEMPLATE_VERSION.to_string(),
        flashcards: Vec::new(),
        quiz: Vec::new(),
        definitions: Vec::new(),
    };
    let mut rejected = Vec::new();

    for (index, item) in payload.flashcards.iter().enumerate() {
        let path = format!("/flashcards/{index}");
        match non_empty(&item.front, "front")
            .and_then(|()| non_empty(&item.back, "back"))
            .and_then(|()| check_anchors(&item.anchors, revision))
        {
            Ok(anchors) => accepted.flashcards.push(Flashcard {
                front: item.front.clone(),
                back: item.back.clone(),
                anchors,
            }),
            Err(reason) => rejected.push(RejectedItem {
                item_path: path,
                reason,
            }),
        }
    }

    for (index, item) in payload.quiz.iter().enumerate() {
        let path = format!("/quiz/{index}");
        match non_empty(&item.question, "question")
            .and_then(|()| non_empty(&item.answer, "answer"))
            .and_then(|()| check_anchors(&item.anchors, revision))
        {
            Ok(anchors) => accepted.quiz.push(QuizItem {
                question: item.question.clone(),
                answer: item.answer.clone(),
                distractors: item.distractors.clone(),
                anchors,
            }),
            Err(reason) => rejected.push(RejectedItem {
                item_path: path,
                reason,
            }),
        }
    }

    for (index, item) in payload.definitions.iter().enumerate() {
        let path = format!("/definitions/{index}");
        match non_empty(&item.term, "term")
            .and_then(|()| non_empty(&item.definition, "definition"))
            .and_then(|()| check_anchors(&item.anchors, revision))
        {
            Ok(anchors) => accepted.definitions.push(Definition {
                term: item.term.clone(),
                definition: item.definition.clone(),
                anchors,
            }),
            Err(reason) => rejected.push(RejectedItem {
                item_path: path,
                reason,
            }),
        }
    }

    StudyValidation { accepted, rejected }
}

/// Parse and validate a model response for a study tool.
///
/// # Errors
///
/// Returns [`ToolError::SchemaValidationFailed`] if the payload cannot be read
/// at all, and [`ToolError::UngroundedOutput`] if every item was refused.
pub fn study_artifacts_from_value(
    value: &serde_json::Value,
    revision: &TranscriptRevision,
) -> Result<StudyValidation, ToolError> {
    let payload: RawStudyPayload =
        serde_json::from_value(value.clone()).map_err(|_| ToolError::SchemaValidationFailed)?;
    let validation = validate_study_artifacts(&payload, revision);
    if validation.accepted.is_empty() && !validation.rejected.is_empty() {
        return Err(ToolError::UngroundedOutput(validation.rejected.len()));
    }
    Ok(validation)
}

/// The `ai_outputs.output_type` value a study tool writes.
#[must_use]
pub const fn output_type_for(kind: ToolKind) -> Option<&'static str> {
    match kind {
        ToolKind::Flashcards => Some("flashcards"),
        ToolKind::Quiz => Some("quiz"),
        ToolKind::Definitions => Some("definitions"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fixture_revision, FIXTURE_SENTENCES};
    use plaudon_domain::transcript::AlignmentOrigin;

    fn anchor(seq: u32, start: u32, end: u32) -> RawStudyAnchor {
        let text: Vec<char> = FIXTURE_SENTENCES[seq as usize].chars().collect();
        RawStudyAnchor {
            seq_index: seq,
            char_start: start,
            char_end: end,
            excerpt: text[start as usize..end as usize].iter().collect(),
        }
    }

    #[test]
    fn a_cited_flashcard_is_accepted_with_its_verified_anchor() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let payload = RawStudyPayload {
            flashcards: vec![RawStudyItem {
                front: "What does idempotency mean?".to_string(),
                back: "The same request can be retried without duplicating work.".to_string(),
                anchors: vec![anchor(4, 0, 11)],
            }],
            ..Default::default()
        };
        let result = validate_study_artifacts(&payload, &revision);
        assert_eq!(result.accepted.flashcards.len(), 1);
        assert!(result.rejected.is_empty());
        assert_eq!(result.accepted.flashcards[0].anchors.len(), 1);
    }

    #[test]
    fn an_uncited_item_is_rejected_by_path_rather_than_dropped() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let payload = RawStudyPayload {
            quiz: vec![RawQuizItem {
                question: "When does the migration ship?".to_string(),
                answer: "Friday".to_string(),
                distractors: Vec::new(),
                anchors: Vec::new(),
            }],
            ..Default::default()
        };
        let result = validate_study_artifacts(&payload, &revision);
        assert!(result.accepted.quiz.is_empty());
        assert_eq!(
            result.rejected,
            vec![RejectedItem {
                item_path: "/quiz/0".to_string(),
                reason: RejectionReason::Uncited,
            }]
        );
    }

    #[test]
    fn a_fabricated_citation_rejects_the_item() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let payload = RawStudyPayload {
            definitions: vec![RawDefinition {
                term: "Blast radius".to_string(),
                definition: "Invented".to_string(),
                anchors: vec![RawStudyAnchor {
                    seq_index: 0,
                    char_start: 0,
                    char_end: 7,
                    excerpt: "Nothing".to_string(),
                }],
            }],
            ..Default::default()
        };
        let result = validate_study_artifacts(&payload, &revision);
        assert!(result.accepted.definitions.is_empty());
        assert!(matches!(
            result.rejected[0].reason,
            RejectionReason::AllAnchorsRejected { .. }
        ));
    }

    #[test]
    fn an_item_keeps_its_verified_anchors_when_only_some_citations_fail() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let mut good = anchor(0, 0, 7);
        good.excerpt = good.excerpt.clone();
        let payload = RawStudyPayload {
            flashcards: vec![RawStudyItem {
                front: "Ship date?".to_string(),
                back: "Friday".to_string(),
                anchors: vec![
                    good,
                    RawStudyAnchor {
                        seq_index: 0,
                        char_start: 0,
                        char_end: 7,
                        excerpt: "Bogus!!".to_string(),
                    },
                ],
            }],
            ..Default::default()
        };
        let result = validate_study_artifacts(&payload, &revision);
        assert_eq!(result.accepted.flashcards.len(), 1);
        assert_eq!(result.accepted.flashcards[0].anchors.len(), 1);
    }

    #[test]
    fn a_blank_required_field_is_rejected_with_the_field_name() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let payload = RawStudyPayload {
            flashcards: vec![RawStudyItem {
                front: "  ".to_string(),
                back: "x".to_string(),
                anchors: vec![anchor(0, 0, 7)],
            }],
            ..Default::default()
        };
        let result = validate_study_artifacts(&payload, &revision);
        assert_eq!(
            result.rejected[0].reason,
            RejectionReason::EmptyField {
                field: "front".to_string()
            }
        );
    }

    #[test]
    fn a_payload_where_everything_is_fabricated_is_an_error_not_an_empty_success() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let value = serde_json::json!({
            "flashcards": [
                {"front": "a", "back": "b", "anchors": [
                    {"seq_index": 0, "char_start": 0, "char_end": 4, "excerpt": "zzzz"}]}
            ]
        });
        assert_eq!(
            study_artifacts_from_value(&value, &revision).unwrap_err(),
            ToolError::UngroundedOutput(1)
        );
    }

    #[test]
    fn the_template_version_is_recorded_on_every_accepted_set() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let result = validate_study_artifacts(&RawStudyPayload::default(), &revision);
        assert_eq!(result.accepted.template_version, STUDY_TEMPLATE_VERSION);
    }

    #[test]
    fn only_study_kinds_map_to_an_output_type() {
        assert_eq!(output_type_for(ToolKind::Flashcards), Some("flashcards"));
        assert_eq!(output_type_for(ToolKind::Quiz), Some("quiz"));
        assert_eq!(output_type_for(ToolKind::Definitions), Some("definitions"));
        assert_eq!(output_type_for(ToolKind::MeetingSummary), None);
    }
}
