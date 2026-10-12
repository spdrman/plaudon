//! Literal find over the document, with optional case sensitivity and
//! whole-word matching.
//!
//! Deliberately not a regular-expression engine. The React pane searched
//! case-insensitively for a literal, and a regex search over a 30,000-segment
//! transcript invites catastrophic backtracking on every keystroke. A literal
//! scan over the piece-table runs allocates nothing for an unedited segment.

use crate::caret::is_word_char;
use crate::doc::{SegmentId, TranscriptDocument};

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    /// The literal to find.
    pub needle: String,
    /// Whether case matters. Case-insensitive matching uses simple (not full)
    /// Unicode case folding, which is what a reader expects from a find bar.
    pub case_sensitive: bool,
    /// Whether the match must be bounded by non-word characters.
    pub whole_word: bool,
}

impl SearchQuery {
    /// A case-insensitive literal query.
    #[must_use]
    pub fn literal(needle: impl Into<String>) -> Self {
        Self {
            needle: needle.into(),
            case_sensitive: false,
            whole_word: false,
        }
    }

    /// Whether the query can match anything.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.needle.is_empty()
    }

    fn folded_needle(&self) -> Vec<char> {
        self.needle
            .chars()
            .map(|c| fold(c, self.case_sensitive))
            .collect()
    }
}

/// One match, in `char` offsets inside a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchMatch {
    /// Segment the match is in.
    pub segment: SegmentId,
    /// Inclusive start, in `char` offsets.
    pub start: usize,
    /// Exclusive end, in `char` offsets.
    pub end: usize,
}

/// Matches over the whole document, in document order, with a current match.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    query: SearchQuery,
    matches: Vec<SearchMatch>,
    current: usize,
    document_version: u64,
}

impl SearchResults {
    /// Run `query` over `document`.
    #[must_use]
    pub fn run(document: &TranscriptDocument, query: SearchQuery) -> Self {
        let matches = find_all(document, &query);
        Self {
            query,
            matches,
            current: 0,
            document_version: document.version(),
        }
    }

    /// Re-run the query if the document changed under it, keeping the current
    /// match index where it still exists.
    pub fn refresh(&mut self, document: &TranscriptDocument) {
        if self.document_version == document.version() {
            return;
        }
        self.matches = find_all(document, &self.query);
        self.document_version = document.version();
        if self.current >= self.matches.len() {
            self.current = 0;
        }
    }

    /// The query.
    #[must_use]
    pub const fn query(&self) -> &SearchQuery {
        &self.query
    }

    /// Every match, in document order.
    #[must_use]
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    /// Matches inside one segment, for highlight painting.
    pub fn matches_in(&self, segment: SegmentId) -> impl Iterator<Item = &SearchMatch> + '_ {
        self.matches
            .iter()
            .filter(move |hit| hit.segment == segment)
    }

    /// Number of matches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Whether nothing matched.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Zero-based index of the current match.
    #[must_use]
    pub const fn current_index(&self) -> usize {
        self.current
    }

    /// The current match.
    #[must_use]
    pub fn current(&self) -> Option<SearchMatch> {
        self.matches.get(self.current).copied()
    }

    /// Advance to the next match, wrapping.
    pub fn next_match(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + 1) % self.matches.len();
        self.current()
    }

    /// Step back to the previous match, wrapping.
    pub fn previous_match(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
        self.current()
    }

    /// Select the first match at or after a position in document order.
    pub fn focus_nearest(
        &mut self,
        document: &TranscriptDocument,
        segment: SegmentId,
        offset: usize,
    ) {
        let Some(position) = document.position(segment) else {
            return;
        };
        let found = self.matches.iter().position(|hit| {
            document
                .position(hit.segment)
                .is_some_and(|hit_position| (hit_position, hit.start) >= (position, offset))
        });
        self.current = found.unwrap_or(0);
    }

    /// A screen-reader friendly status, e.g. `3 of 12 matches`.
    #[must_use]
    pub fn status(&self) -> String {
        if !self.query.is_active() {
            return String::new();
        }
        if self.matches.is_empty() {
            return "no matches".to_owned();
        }
        format!("{} of {} matches", self.current + 1, self.matches.len())
    }
}

/// Every match in the document, in document order.
#[must_use]
pub fn find_all(document: &TranscriptDocument, query: &SearchQuery) -> Vec<SearchMatch> {
    if !query.is_active() {
        return Vec::new();
    }
    let needle = query.folded_needle();
    let mut matches = Vec::new();
    for segment in document.segments() {
        let text = segment.text();
        find_in_text(&text, &needle, query, &mut |start, end| {
            matches.push(SearchMatch {
                segment: segment.id,
                start,
                end,
            });
        });
    }
    matches
}

/// Matches inside one string, as `char` offsets.
#[must_use]
pub fn find_in_str(haystack: &str, query: &SearchQuery) -> Vec<(usize, usize)> {
    if !query.is_active() {
        return Vec::new();
    }
    let needle = query.folded_needle();
    let mut out = Vec::new();
    find_in_text(haystack, &needle, query, &mut |start, end| {
        out.push((start, end));
    });
    out
}

fn find_in_text(
    haystack: &str,
    needle: &[char],
    query: &SearchQuery,
    emit: &mut impl FnMut(usize, usize),
) {
    let Some(&first) = needle.first() else {
        return;
    };
    let needle_len = needle.len();
    let mut previous: Option<char> = None;
    let mut next_allowed = 0usize;
    for (char_index, (byte_index, candidate)) in haystack.char_indices().enumerate() {
        if char_index >= next_allowed && fold(candidate, query.case_sensitive) == first {
            let mut rest = haystack[byte_index..].chars();
            let matched = needle
                .iter()
                .all(|wanted| rest.next().map(|c| fold(c, query.case_sensitive)) == Some(*wanted));
            if matched {
                let following = rest.next();
                let bounded = !query.whole_word
                    || (previous.is_none_or(|c| !is_word_char(c))
                        && following.is_none_or(|c| !is_word_char(c)));
                if bounded {
                    emit(char_index, char_index + needle_len);
                    // Matches never overlap: the next search resumes after
                    // this one, which is what prev/next navigation and
                    // replace-all both assume.
                    next_allowed = char_index + needle_len;
                }
            }
        }
        previous = Some(candidate);
    }
}

fn fold(value: char, case_sensitive: bool) -> char {
    if case_sensitive {
        value
    } else if value.is_ascii() {
        value.to_ascii_lowercase()
    } else {
        value.to_lowercase().next().unwrap_or(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::document_with;

    #[test]
    fn a_literal_search_is_case_insensitive_by_default() {
        let document = document_with(&["We will not pay", "we will PAY later"]);
        let results = SearchResults::run(&document, SearchQuery::literal("pay"));
        assert_eq!(results.len(), 2);
        assert_eq!(results.status(), "1 of 2 matches");
    }

    #[test]
    fn case_sensitivity_narrows_the_result() {
        let document = document_with(&["We will not pay", "we will PAY later"]);
        let results = SearchResults::run(
            &document,
            SearchQuery {
                needle: "pay".to_owned(),
                case_sensitive: true,
                whole_word: false,
            },
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn whole_word_rejects_a_substring_hit() {
        let document = document_with(&["payment plan", "we pay now"]);
        let whole = SearchResults::run(
            &document,
            SearchQuery {
                needle: "pay".to_owned(),
                case_sensitive: false,
                whole_word: true,
            },
        );
        assert_eq!(whole.len(), 1);
        let partial = SearchResults::run(&document, SearchQuery::literal("pay"));
        assert_eq!(partial.len(), 2);
    }

    #[test]
    fn overlapping_candidates_match_once_each() {
        let document = document_with(&["aaaa"]);
        let results = SearchResults::run(&document, SearchQuery::literal("aa"));
        assert_eq!(
            results
                .matches()
                .iter()
                .map(|hit| (hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, 2), (2, 4)]
        );
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let document = document_with(&["one two", "two three"]);
        let mut results = SearchResults::run(&document, SearchQuery::literal("two"));
        assert_eq!(results.current_index(), 0);
        results.next_match();
        assert_eq!(results.current_index(), 1);
        results.next_match();
        assert_eq!(results.current_index(), 0);
        results.previous_match();
        assert_eq!(results.current_index(), 1);
    }

    #[test]
    fn offsets_are_char_offsets() {
        let document = document_with(&["naïve café pay"]);
        let results = SearchResults::run(&document, SearchQuery::literal("pay"));
        assert_eq!(
            results.matches().first().map(|hit| (hit.start, hit.end)),
            Some((11, 14))
        );
    }

    #[test]
    fn refreshing_after_an_edit_re_runs_the_query() {
        let mut document = document_with(&["pay"]);
        let mut results = SearchResults::run(&document, SearchQuery::literal("pay"));
        assert_eq!(results.len(), 1);
        let id = document.first_id().expect("segment");
        document.insert_text(id, 3, " pay").expect("insert");
        results.refresh(&document);
        assert_eq!(results.len(), 2);
    }
}
