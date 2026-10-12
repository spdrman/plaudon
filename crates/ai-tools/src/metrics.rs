//! Tool quality metrics for study artifacts and Q&A (spec 17.4).
//!
//! Spec 17.4: "For flashcards/Q&A we will score factual correctness, coverage,
//! redundant-question rate and passage-grounding precision using an opt-in
//! labeled educational corpus."
//!
//! Two properties matter more than the exact formulas:
//!
//! * The corpus is **opt-in**. When it is absent the harness reports
//!   [`CorpusAvailability::Absent`] and the scores are skipped. Nothing here
//!   fabricates a number for a corpus that was never loaded.
//! * Every score carries its sample size, because a ratio over three items is
//!   not a measurement (spec 17.2, 17.6).

use std::collections::BTreeSet;
use std::path::Path;

use plaudon_domain::transcript::TranscriptRevision;
use serde::{Deserialize, Serialize};

use crate::anchor::verify_anchor;
use crate::retrieval::tokenize;
use crate::study::StudyArtifacts;

/// Token-overlap ratio above which two strings are considered the same claim.
pub const MATCH_THRESHOLD: f64 = 0.6;
/// Token-overlap ratio above which two questions are considered redundant.
pub const REDUNDANCY_THRESHOLD: f64 = 0.8;

/// One labeled key point a competent study set is expected to cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledKeyPoint {
    /// Stable identifier inside the corpus.
    pub id: String,
    /// The fact, phrased as the reference answer.
    pub statement: String,
}

/// An opt-in labeled educational corpus for one recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledCorpus {
    /// Corpus name, recorded in the report.
    pub name: String,
    /// Where the corpus came from, recorded so a score is attributable.
    pub provenance: String,
    /// BCP-47 locale of the material.
    pub locale: String,
    /// The labeled key points.
    pub key_points: Vec<LabeledKeyPoint>,
}

/// Whether the opt-in corpus was available for this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CorpusAvailability {
    /// The corpus was loaded.
    Present {
        /// Corpus name.
        name: String,
        /// Corpus provenance.
        provenance: String,
        /// Number of labeled key points.
        key_point_count: usize,
    },
    /// The corpus was not available, so scoring was skipped.
    Absent {
        /// Machine-readable reason.
        reason: String,
    },
}

/// A scored metric with its sample size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoredMetric {
    /// The ratio in `[0, 1]`.
    pub value: f64,
    /// Number of observations the ratio was computed over.
    pub sample_size: usize,
}

impl ScoredMetric {
    fn ratio(hits: usize, total: usize) -> Option<Self> {
        if total == 0 {
            return None;
        }
        Some(Self {
            value: hits as f64 / total as f64,
            sample_size: total,
        })
    }
}

/// The spec 17.4 tool-quality report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolQualityReport {
    /// Whether the opt-in corpus was present.
    pub corpus: CorpusAvailability,
    /// Fraction of generated items that match a labeled key point.
    pub factual_correctness: Option<ScoredMetric>,
    /// Fraction of labeled key points covered by at least one item.
    pub coverage: Option<ScoredMetric>,
    /// Fraction of questions that duplicate an earlier question.
    pub redundant_question_rate: Option<ScoredMetric>,
    /// Fraction of items whose every citation verifies.
    ///
    /// This one does not need the labeled corpus: grounding is checked against
    /// the transcript revision itself, which is always available.
    pub grounding_precision: Option<ScoredMetric>,
}

impl ToolQualityReport {
    /// A report for a run where the corpus was unavailable.
    ///
    /// Grounding precision is still computed, because it depends only on the
    /// transcript. The corpus-dependent scores stay `None`.
    #[must_use]
    pub fn skipped(reason: impl Into<String>, grounding: Option<ScoredMetric>) -> Self {
        Self {
            corpus: CorpusAvailability::Absent {
                reason: reason.into(),
            },
            factual_correctness: None,
            coverage: None,
            redundant_question_rate: None,
            grounding_precision: grounding,
        }
    }
}

/// Load a corpus from a JSON file, treating absence as a skip rather than an
/// error.
///
/// # Errors
///
/// Returns the parse error message when the file exists but is malformed: a
/// corrupt corpus must not be silently treated as "no corpus".
pub fn load_corpus(path: &Path) -> Result<Option<LabeledCorpus>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map(Some).map_err(|e| e.to_string())
}

/// Token-set overlap, symmetric, in `[0, 1]`.
#[must_use]
pub fn similarity(left: &str, right: &str) -> f64 {
    let a: BTreeSet<String> = tokenize(left).into_iter().collect();
    let b: BTreeSet<String> = tokenize(right).into_iter().collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    intersection / union
}

/// Every claim a study set asserts, as scorable strings.
fn claims(artifacts: &StudyArtifacts) -> Vec<String> {
    let mut out: Vec<String> = artifacts
        .flashcards
        .iter()
        .map(|c| format!("{} {}", c.front, c.back))
        .collect();
    out.extend(
        artifacts
            .quiz
            .iter()
            .map(|q| format!("{} {}", q.question, q.answer)),
    );
    out.extend(
        artifacts
            .definitions
            .iter()
            .map(|d| format!("{} {}", d.term, d.definition)),
    );
    out
}

/// Questions a study set poses, used for the redundancy rate.
fn questions(artifacts: &StudyArtifacts) -> Vec<String> {
    let mut out: Vec<String> = artifacts.flashcards.iter().map(|c| c.front.clone()).collect();
    out.extend(artifacts.quiz.iter().map(|q| q.question.clone()));
    out
}

/// Fraction of items whose every citation verifies against the revision.
#[must_use]
pub fn grounding_precision(
    artifacts: &StudyArtifacts,
    revision: &TranscriptRevision,
) -> Option<ScoredMetric> {
    let mut total = 0;
    let mut grounded = 0;
    let mut check = |anchors: &[crate::anchor::SourceAnchor]| {
        total += 1;
        if !anchors.is_empty()
            && anchors
                .iter()
                .all(|a| verify_anchor(a, revision).is_verified())
        {
            grounded += 1;
        }
    };
    for card in &artifacts.flashcards {
        check(&card.anchors);
    }
    for item in &artifacts.quiz {
        check(&item.anchors);
    }
    for definition in &artifacts.definitions {
        check(&definition.anchors);
    }
    ScoredMetric::ratio(grounded, total)
}

/// Fraction of questions that duplicate an earlier question.
#[must_use]
pub fn redundant_question_rate(artifacts: &StudyArtifacts) -> Option<ScoredMetric> {
    let questions = questions(artifacts);
    let mut redundant = 0;
    for (index, question) in questions.iter().enumerate() {
        if questions[..index]
            .iter()
            .any(|earlier| similarity(earlier, question) >= REDUNDANCY_THRESHOLD)
        {
            redundant += 1;
        }
    }
    ScoredMetric::ratio(redundant, questions.len())
}

/// Score a study set against an optional labeled corpus.
#[must_use]
pub fn score_study_artifacts(
    artifacts: &StudyArtifacts,
    revision: &TranscriptRevision,
    corpus: Option<&LabeledCorpus>,
) -> ToolQualityReport {
    let grounding = grounding_precision(artifacts, revision);
    let Some(corpus) = corpus else {
        return ToolQualityReport::skipped("corpus_not_available", grounding);
    };

    let claims = claims(artifacts);
    let correct = claims
        .iter()
        .filter(|claim| {
            corpus
                .key_points
                .iter()
                .any(|point| similarity(&point.statement, claim) >= MATCH_THRESHOLD)
        })
        .count();
    let covered = corpus
        .key_points
        .iter()
        .filter(|point| {
            claims
                .iter()
                .any(|claim| similarity(&point.statement, claim) >= MATCH_THRESHOLD)
        })
        .count();

    ToolQualityReport {
        corpus: CorpusAvailability::Present {
            name: corpus.name.clone(),
            provenance: corpus.provenance.clone(),
            key_point_count: corpus.key_points.len(),
        },
        factual_correctness: ScoredMetric::ratio(correct, claims.len()),
        coverage: ScoredMetric::ratio(covered, corpus.key_points.len()),
        redundant_question_rate: redundant_question_rate(artifacts),
        grounding_precision: grounding,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor::SourceAnchor;
    use crate::study::{Flashcard, QuizItem, STUDY_TEMPLATE_VERSION};
    use crate::testing::{fixture_revision, FIXTURE_SENTENCES};
    use plaudon_domain::transcript::AlignmentOrigin;

    fn corpus() -> LabeledCorpus {
        LabeledCorpus {
            name: "fixture-lecture".to_string(),
            provenance: "synthetic, authored for this test".to_string(),
            locale: "en-US".to_string(),
            key_points: vec![
                LabeledKeyPoint {
                    id: "kp1".to_string(),
                    statement: "Idempotency means the same request can be retried without \
                                duplicating work"
                        .to_string(),
                },
                LabeledKeyPoint {
                    id: "kp2".to_string(),
                    statement: "The migration ships on Friday".to_string(),
                },
            ],
        }
    }

    fn set(revision: &TranscriptRevision, grounded: bool) -> StudyArtifacts {
        let excerpt: String = FIXTURE_SENTENCES[4].chars().take(11).collect();
        let anchors = vec![SourceAnchor::new(
            revision.id,
            4,
            0,
            11,
            if grounded {
                excerpt
            } else {
                "NOT THE TEXT".to_string()
            },
        )];
        StudyArtifacts {
            template_version: STUDY_TEMPLATE_VERSION.to_string(),
            flashcards: vec![Flashcard {
                front: "What does idempotency mean?".to_string(),
                back: "The same request can be retried without duplicating work.".to_string(),
                anchors: anchors.clone(),
            }],
            quiz: vec![QuizItem {
                question: "When does the migration ship?".to_string(),
                answer: "Friday".to_string(),
                distractors: Vec::new(),
                anchors,
            }],
            definitions: Vec::new(),
        }
    }

    #[test]
    fn an_absent_corpus_skips_the_labeled_scores_without_faking_them() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let report = score_study_artifacts(&set(&revision, true), &revision, None);
        assert!(matches!(report.corpus, CorpusAvailability::Absent { .. }));
        assert!(report.factual_correctness.is_none());
        assert!(report.coverage.is_none());
        // Grounding does not depend on the corpus, so it is still measured.
        assert!(report.grounding_precision.is_some());
    }

    #[test]
    fn a_present_corpus_produces_correctness_and_coverage_with_sample_sizes() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let corpus = corpus();
        let report = score_study_artifacts(&set(&revision, true), &revision, Some(&corpus));
        let correctness = report.factual_correctness.unwrap();
        assert!(correctness.value > 0.0);
        assert_eq!(correctness.sample_size, 2);
        let coverage = report.coverage.unwrap();
        assert_eq!(coverage.sample_size, 2);
        assert!(matches!(report.corpus, CorpusAvailability::Present { .. }));
    }

    #[test]
    fn ungrounded_items_lower_grounding_precision_to_zero() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let report = score_study_artifacts(&set(&revision, false), &revision, None);
        assert_eq!(report.grounding_precision.unwrap().value, 0.0);
    }

    #[test]
    fn duplicate_questions_are_counted_as_redundant() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let mut artifacts = set(&revision, true);
        let duplicate = artifacts.flashcards[0].clone();
        artifacts.flashcards.push(duplicate);
        let rate = redundant_question_rate(&artifacts).unwrap();
        assert_eq!(rate.sample_size, 3);
        assert!(rate.value > 0.0);
    }

    #[test]
    fn distinct_questions_are_not_counted_as_redundant() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        assert_eq!(
            redundant_question_rate(&set(&revision, true)).unwrap().value,
            0.0
        );
    }

    #[test]
    fn an_empty_set_yields_no_ratio_rather_than_a_perfect_score() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let empty = StudyArtifacts {
            template_version: STUDY_TEMPLATE_VERSION.to_string(),
            flashcards: Vec::new(),
            quiz: Vec::new(),
            definitions: Vec::new(),
        };
        assert!(grounding_precision(&empty, &revision).is_none());
        assert!(redundant_question_rate(&empty).is_none());
    }

    #[test]
    fn similarity_is_symmetric_and_bounded() {
        assert_eq!(similarity("a b c", "a b c"), 1.0);
        assert_eq!(similarity("a b", "c d"), 0.0);
        assert!((similarity("a b", "b c") - similarity("b c", "a b")).abs() < f64::EPSILON);
        assert_eq!(similarity("", "a"), 0.0);
    }

    #[test]
    fn a_missing_corpus_file_is_a_skip_and_a_corrupt_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_corpus(&dir.path().join("absent.json")).unwrap(), None);
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{ nope").unwrap();
        assert!(load_corpus(&bad).is_err());
    }

    #[test]
    fn a_corpus_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corpus.json");
        std::fs::write(&path, serde_json::to_string(&corpus()).unwrap()).unwrap();
        assert_eq!(load_corpus(&path).unwrap(), Some(corpus()));
    }
}
