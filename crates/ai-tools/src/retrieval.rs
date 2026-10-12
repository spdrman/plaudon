//! Local lexical retrieval over one pinned transcript revision (spec 9.8).
//!
//! Spec 9.8 requires that a question retrieves "only relevant transcript
//! chunks using local search" rather than dispatching the whole transcript.
//! This module is that local search: an Okapi BM25 ranking computed in-process
//! with no index server, no embedding model and no network call of any kind.
//!
//! The scorer is deliberately boring. Semantic search is not in the spec, and
//! a lexical score is explainable: a user can see why a passage was retrieved.

use std::collections::HashMap;

use plaudon_domain::transcript::TranscriptRevision;

use crate::prompt::FencedPassage;

/// BM25 term-frequency saturation.
const K1: f64 = 1.2;
/// BM25 length-normalization weight.
const B: f64 = 0.75;
/// Below this many segments, document frequency is too coarse to call a term
/// ubiquitous, so the high-frequency term cutoff is not applied.
const MIN_SEGMENTS_FOR_CUTOFF: usize = 4;

/// A retrieved passage with its score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredPassage {
    /// Segment position in the revision.
    pub seq_index: u32,
    /// Segment text, verbatim.
    pub text: String,
    /// BM25 score. Comparable only within one query.
    pub score: f64,
}

impl ScoredPassage {
    /// The passage as presented to a model.
    #[must_use]
    pub fn to_fenced(&self) -> FencedPassage {
        FencedPassage {
            seq_index: self.seq_index,
            text: self.text.clone(),
        }
    }
}

/// Split text into lowercase alphanumeric tokens.
///
/// Query operators, shell metacharacters and punctuation are not tokens, so a
/// hostile query degrades to its harmless word content rather than reaching a
/// query parser.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// A BM25 index over the segments of exactly one revision.
#[derive(Debug, Clone)]
pub struct Retriever {
    documents: Vec<Document>,
    average_length: f64,
    document_frequency: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
struct Document {
    seq_index: u32,
    text: String,
    length: usize,
    term_frequency: HashMap<String, usize>,
}

impl Retriever {
    /// Build an index over a revision's segments.
    #[must_use]
    pub fn over_revision(revision: &TranscriptRevision) -> Self {
        let documents: Vec<Document> = revision
            .segments()
            .iter()
            .map(|segment| {
                let tokens = tokenize(&segment.text);
                let mut term_frequency: HashMap<String, usize> = HashMap::new();
                for token in &tokens {
                    *term_frequency.entry(token.clone()).or_insert(0) += 1;
                }
                Document {
                    seq_index: segment.seq_index,
                    text: segment.text.clone(),
                    length: tokens.len(),
                    term_frequency,
                }
            })
            .collect();

        let mut document_frequency: HashMap<String, usize> = HashMap::new();
        for doc in &documents {
            for term in doc.term_frequency.keys() {
                *document_frequency.entry(term.clone()).or_insert(0) += 1;
            }
        }

        let total: usize = documents.iter().map(|d| d.length).sum();
        let average_length = if documents.is_empty() {
            0.0
        } else {
            total as f64 / documents.len() as f64
        };

        Self {
            documents,
            average_length,
            document_frequency,
        }
    }

    /// Number of indexed segments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    /// Whether the revision had no segments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Rank segments against a query, returning at most `limit` positives.
    ///
    /// A segment that shares no query term scores zero and is never returned,
    /// so an off-topic question retrieves nothing and the caller can report
    /// insufficient evidence without dispatching anything.
    #[must_use]
    pub fn retrieve(&self, query: &str, limit: usize) -> Vec<ScoredPassage> {
        let all_terms = tokenize(query);
        if all_terms.is_empty() || self.documents.is_empty() {
            return Vec::new();
        }
        let n = self.documents.len() as f64;

        // Terms present in at least half the segments carry no discriminative
        // signal: "the" matching every passage is not evidence that the
        // recording discusses the question. Dropping them is what makes an
        // off-topic question retrieve nothing, so the caller can answer
        // "not supported by this recording" without dispatching anything.
        //
        // Below `MIN_SEGMENTS_FOR_CUTOFF` segments the document frequency of
        // any term is too coarse to call a term ubiquitous, so every term is
        // kept and the BM25 ranking alone decides.
        let terms: Vec<String> = if self.documents.len() >= MIN_SEGMENTS_FOR_CUTOFF {
            all_terms
                .iter()
                .filter(|term| {
                    let df = *self.document_frequency.get(*term).unwrap_or(&0) as f64;
                    df > 0.0 && df / n < 0.5
                })
                .cloned()
                .collect()
        } else {
            all_terms
        };
        if terms.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<ScoredPassage> = self
            .documents
            .iter()
            .map(|doc| {
                let mut score = 0.0;
                for term in &terms {
                    let Some(&tf) = doc.term_frequency.get(term) else {
                        continue;
                    };
                    let df = *self.document_frequency.get(term).unwrap_or(&0) as f64;
                    // Standard BM25 idf, floored at zero so a term present in
                    // every segment contributes nothing instead of going
                    // negative and pushing matches below non-matches.
                    let idf = (((n - df + 0.5) / (df + 0.5)) + 1.0).ln().max(0.0);
                    let tf = tf as f64;
                    let norm = if self.average_length > 0.0 {
                        1.0 - B + B * (doc.length as f64 / self.average_length)
                    } else {
                        1.0
                    };
                    score += idf * (tf * (K1 + 1.0)) / (tf + K1 * norm);
                }
                ScoredPassage {
                    seq_index: doc.seq_index,
                    text: doc.text.clone(),
                    score,
                }
            })
            .filter(|p| p.score > 0.0)
            .collect();

        // Ties resolve by transcript order, so retrieval is deterministic and
        // the model sees passages chronologically.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.seq_index.cmp(&b.seq_index))
        });
        scored.truncate(limit);
        scored.sort_by_key(|p| p.seq_index);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::fixture_revision;
    use plaudon_domain::transcript::AlignmentOrigin;

    #[test]
    fn a_question_retrieves_the_segment_that_answers_it() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let hits = Retriever::over_revision(&revision).retrieve("who owns the rollback plan", 3);
        assert!(hits.iter().any(|h| h.seq_index == 1));
    }

    #[test]
    fn an_unrelated_question_retrieves_nothing() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let hits =
            Retriever::over_revision(&revision).retrieve("quarterly dividend in Patagonia", 5);
        assert!(hits.is_empty());
    }

    #[test]
    fn retrieval_is_capped_and_returned_in_transcript_order() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let hits = Retriever::over_revision(&revision).retrieve("the will and plan test", 2);
        assert!(hits.len() <= 2);
        let indices: Vec<u32> = hits.iter().map(|h| h.seq_index).collect();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        assert_eq!(indices, sorted);
    }

    #[test]
    fn hostile_query_syntax_degrades_to_its_word_content() {
        assert_eq!(
            tokenize("rollback\" OR 1=1 --; rm -rf /"),
            ["rollback", "or", "1", "1", "rm", "rf"]
        );
        let revision = fixture_revision(AlignmentOrigin::Native);
        let hits = Retriever::over_revision(&revision).retrieve("\"rollback\" OR 1=1 --", 5);
        assert!(hits.iter().all(|h| h.score > 0.0));
    }

    #[test]
    fn an_empty_or_punctuation_only_query_retrieves_nothing() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let retriever = Retriever::over_revision(&revision);
        assert!(retriever.retrieve("", 5).is_empty());
        assert!(retriever.retrieve("*** !!! ???", 5).is_empty());
    }

    #[test]
    fn an_empty_revision_is_handled_without_dividing_by_zero() {
        let revision = crate::testing::revision_from_segments(AlignmentOrigin::None, Vec::new());
        let retriever = Retriever::over_revision(&revision);
        assert!(retriever.is_empty());
        assert!(retriever.retrieve("anything", 5).is_empty());
    }

    #[test]
    fn ranking_prefers_the_segment_with_more_query_terms() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let hits = Retriever::over_revision(&revision).retrieve("staging cluster load test", 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq_index, 2);
    }

    #[test]
    fn tokenization_is_case_and_diacritic_literal_but_lowercased() {
        assert_eq!(
            tokenize("Priya OWNS Rollback"),
            ["priya", "owns", "rollback"]
        );
    }
}
