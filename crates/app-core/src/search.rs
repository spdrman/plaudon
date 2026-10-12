//! Cross-recording local full-text search (spec 3.1, 9.8, 17.5).
//!
//! Spec 9.8 keeps cross-recording search "optional and privacy-aware". It is
//! privacy-aware in the strongest available sense: it is one SQL statement
//! against the user's own SQLite file. No embedding model, no index service,
//! no request.
//!
//! The hostile-input rule is handled by **not having a query language**. User
//! input is tokenized into alphanumeric words and re-emitted as quoted FTS5
//! terms, so `NEAR`, `*`, `"`, `:`, `^`, `-` and every shell metacharacter
//! lose their meaning before SQLite ever sees them. There is no escaping to
//! get wrong because nothing is passed through.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::search::{SearchPage, TranscriptSearchRepository};
use plaudon_persistence::{Database, PersistenceResult};
use serde::{Deserialize, Serialize};

/// Characters either side of a matched term in an excerpt.
pub const EXCERPT_CONTEXT_CHARS: usize = 60;

/// One search result, shaped for the library UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Recording the hit belongs to.
    pub recording_id: RecordingId,
    /// Title of that recording.
    pub recording_title: String,
    /// The exact revision the matching segment belongs to.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Segment position, used to scroll the transcript to the match.
    pub seq_index: u32,
    /// A window of the segment text around the match.
    pub excerpt: String,
    /// Playback position, present only when the revision carries real times.
    ///
    /// Spec 8.6A: a text-only transcript offers no seek affordance, so the UI
    /// must receive `None` rather than a zero it might render as a button.
    pub seek_ms: Option<u64>,
    /// Where those times came from, so the UI can label the affordance.
    pub alignment_origin: AlignmentOrigin,
}

/// Turn user input into a safe FTS5 match expression.
///
/// Every token is quoted, so FTS5 treats it as a literal string rather than an
/// operator. Tokens are joined with `AND` semantics (FTS5's default for
/// juxtaposed terms). Input with no alphanumeric content yields `None`, which
/// callers surface as "no results" without touching the database.
#[must_use]
pub fn to_match_expression(query: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Extract a window of `text` around the first occurrence of any term.
#[must_use]
pub fn excerpt_around(text: &str, terms: &[String]) -> String {
    let lower = text.to_lowercase();
    let hit = terms
        .iter()
        .filter_map(|term| lower.find(term.as_str()))
        .min();
    let chars: Vec<char> = text.chars().collect();
    let Some(byte_index) = hit else {
        return chars.iter().take(EXCERPT_CONTEXT_CHARS * 2).collect();
    };
    // Convert the byte offset to a character offset without slicing on a
    // non-boundary.
    let char_index = text[..byte_index].chars().count();
    let start = char_index.saturating_sub(EXCERPT_CONTEXT_CHARS);
    let end = (char_index + EXCERPT_CONTEXT_CHARS).min(chars.len());
    let mut excerpt = String::new();
    if start > 0 {
        excerpt.push('…');
    }
    excerpt.extend(&chars[start..end]);
    if end < chars.len() {
        excerpt.push('…');
    }
    excerpt
}

fn parse_origin(raw: &str) -> AlignmentOrigin {
    match raw {
        "native" => AlignmentOrigin::Native,
        "derived" => AlignmentOrigin::Derived,
        // An unrecognized value is treated as "no alignment", which is the
        // conservative reading: it withholds a seek affordance.
        _ => AlignmentOrigin::None,
    }
}

/// Cross-recording search over the local library.
#[derive(Debug, Clone)]
pub struct LibrarySearch {
    repository: TranscriptSearchRepository,
}

impl LibrarySearch {
    /// Open a search service over a database.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            repository: TranscriptSearchRepository::new(db),
        }
    }

    /// Run a user query.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures. A query with no searchable content is
    /// not an error: it returns no results.
    pub async fn search(
        &self,
        query: &str,
        page: SearchPage,
    ) -> PersistenceResult<Vec<SearchResult>> {
        let Some(expression) = to_match_expression(query) else {
            return Ok(Vec::new());
        };
        let terms: Vec<String> = expression
            .split(' ')
            .map(|term| term.trim_matches('"').to_string())
            .collect();

        let hits = self.repository.search(&expression, page).await?;
        Ok(hits
            .into_iter()
            .map(|hit| {
                let origin = parse_origin(&hit.alignment_origin);
                SearchResult {
                    recording_id: hit.recording_id,
                    recording_title: hit.recording_title,
                    transcript_revision_id: hit.transcript_revision_id,
                    seq_index: hit.seq_index,
                    excerpt: excerpt_around(&hit.text, &terms),
                    seek_ms: if origin.permits_seek() {
                        hit.start_ms
                    } else {
                        None
                    },
                    alignment_origin: origin,
                }
            })
            .collect())
    }

    /// Rebuild the index, for use after a portable restore.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn rebuild_index(&self) -> PersistenceResult<u64> {
        self.repository.rebuild().await
    }

    /// Number of indexed rows.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn indexed_row_count(&self) -> PersistenceResult<i64> {
        self.repository.indexed_row_count().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_query_becomes_quoted_terms() {
        assert_eq!(
            to_match_expression("rollback plan"),
            Some("\"rollback\" \"plan\"".to_string())
        );
    }

    #[test]
    fn fts_operators_lose_their_meaning() {
        assert_eq!(
            to_match_expression("rollback NEAR/3 plan*"),
            Some("\"rollback\" \"near\" \"3\" \"plan\"".to_string())
        );
        assert_eq!(
            to_match_expression("a OR b AND NOT c"),
            Some("\"a\" \"or\" \"b\" \"and\" \"not\" \"c\"".to_string())
        );
    }

    #[test]
    fn quotes_and_shell_metacharacters_cannot_escape_the_expression() {
        let hostile = "\"; DROP TABLE recordings; -- $(rm -rf /) `id` 'x'";
        let expression = to_match_expression(hostile).unwrap();
        assert!(!expression.contains(';'));
        assert!(!expression.contains('$'));
        assert!(!expression.contains('`'));
        assert!(!expression.contains('\''));
        // Every double quote in the output is a delimiter we put there.
        assert_eq!(expression.matches('"').count() % 2, 0);
    }

    #[test]
    fn a_query_with_no_searchable_content_yields_no_expression() {
        assert_eq!(to_match_expression("   "), None);
        assert_eq!(to_match_expression("*** ??? !!!"), None);
        assert_eq!(to_match_expression(""), None);
    }

    #[test]
    fn an_excerpt_is_centred_on_the_match_and_elided_at_both_ends() {
        let text = format!("{}needle{}", "a".repeat(200), "b".repeat(200));
        let excerpt = excerpt_around(&text, &["needle".to_string()]);
        assert!(excerpt.contains("needle"));
        assert!(excerpt.starts_with('…'));
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= EXCERPT_CONTEXT_CHARS * 2 + 8);
    }

    #[test]
    fn an_excerpt_without_a_match_falls_back_to_the_head_of_the_segment() {
        let excerpt = excerpt_around("short text", &["absent".to_string()]);
        assert_eq!(excerpt, "short text");
    }

    #[test]
    fn excerpting_multi_byte_text_does_not_split_a_character() {
        let text = "héllo wörld needle über alles";
        let excerpt = excerpt_around(text, &["needle".to_string()]);
        assert!(excerpt.contains("needle"));
        assert!(excerpt.contains('ö'));
    }

    #[test]
    fn an_unknown_alignment_origin_withholds_the_seek_affordance() {
        assert_eq!(parse_origin("native"), AlignmentOrigin::Native);
        assert_eq!(parse_origin("derived"), AlignmentOrigin::Derived);
        assert_eq!(parse_origin("none"), AlignmentOrigin::None);
        assert_eq!(parse_origin("something-new"), AlignmentOrigin::None);
        assert!(!parse_origin("something-new").permits_seek());
    }
}
