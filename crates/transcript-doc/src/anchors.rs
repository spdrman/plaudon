//! Keeping EPIC I's citations resolvable across an edit.
//!
//! A [`SourceAnchor`] cites `(revision, seq_index, char_start, char_end)` and
//! carries the excerpt it claims occupies that span
//! (`plaudon_domain::anchor`). Revisions are immutable, so an anchor against
//! the base revision never breaks — but the *claim it supports* is stale once
//! a newer revision of the same transcript exists
//! (`TranscriptLineage::is_stale`), and `persistence::intelligence::mark_stale`
//! marks every output that read the superseded revision.
//!
//! That rule is coarse by design: a user who fixed a typo three paragraphs
//! away invalidates nothing about the cited sentence. This module refines it
//! without weakening it. For each anchor it answers one question the piece
//! table can answer exactly:
//!
//! * the cited characters are all still present, unmodified and contiguous →
//!   [`AnchorMapping::Mapped`], with offsets in the new revision's
//!   coordinates. The host rewrites the anchor to the saved revision and the
//!   claim keeps its evidence: `SourceAnchor::resolve` against the child
//!   revision verifies, with the same `matched_text`.
//! * anything else → [`AnchorMapping::Stale`], with the reason. The host marks
//!   the output stale exactly as it does today; nothing is silently carried
//!   forward.
//!
//! Mapping must be computed **before** [`TranscriptDocument::adopt_saved_revision`],
//! because that call makes the edited text the new base and the old
//! coordinates cease to exist.

use plaudon_domain::anchor::SourceAnchor;
use plaudon_domain::ids::TranscriptRevisionId;

use crate::doc::TranscriptDocument;
use crate::text::SpanMapping;

/// Where a cited span ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorMapping {
    /// The cited text survived the edits verbatim.
    Mapped(MappedSpan),
    /// The citation no longer points at the text it quoted.
    Stale(AnchorStaleness),
}

impl AnchorMapping {
    /// Whether the anchor can be carried forward.
    #[must_use]
    pub const fn is_mapped(&self) -> bool {
        matches!(self, Self::Mapped(_))
    }

    /// The mapped span, when the anchor survived.
    #[must_use]
    pub const fn mapped(&self) -> Option<&MappedSpan> {
        match self {
            Self::Mapped(span) => Some(span),
            Self::Stale(_) => None,
        }
    }

    /// The reason, when the anchor did not survive.
    #[must_use]
    pub const fn staleness(&self) -> Option<&AnchorStaleness> {
        match self {
            Self::Mapped(_) => None,
            Self::Stale(reason) => Some(reason),
        }
    }
}

/// A cited span, re-expressed in current document coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedSpan {
    /// Segment index, unchanged: segments are never renumbered.
    pub seq_index: u32,
    /// Inclusive start, in `char` offsets of the current text.
    pub char_start: u32,
    /// Exclusive end, in `char` offsets of the current text.
    pub char_end: u32,
    /// The excerpt, unchanged — it is still the text at the span.
    pub excerpt: String,
    /// Whether the offsets moved at all.
    pub shifted: bool,
}

impl MappedSpan {
    /// The anchor to store against the revision the edits were saved as.
    #[must_use]
    pub fn into_anchor(self, revision_id: TranscriptRevisionId) -> SourceAnchor {
        SourceAnchor::new(
            revision_id,
            self.seq_index,
            self.char_start,
            self.char_end,
            self.excerpt,
        )
    }
}

/// Why an anchor can no longer be carried forward.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnchorStaleness {
    /// The anchor cites a revision this document is not based on.
    #[error("anchor cites revision {cited}, but the document is based on {base}")]
    RevisionMismatch {
        /// Revision the anchor named.
        cited: TranscriptRevisionId,
        /// Revision the document is based on.
        base: TranscriptRevisionId,
    },
    /// The document holds no segment with that index.
    #[error("the document has no segment {seq_index}")]
    SegmentNotFound {
        /// Index the anchor named.
        seq_index: u32,
    },
    /// The cited span is empty or inverted; it was never resolvable.
    #[error("the cited span of segment {seq_index} is empty")]
    EmptySpan {
        /// Index the anchor named.
        seq_index: u32,
    },
    /// The cited characters were edited: deleted, or typed into.
    #[error("the cited text of segment {seq_index} was edited")]
    SpanEdited {
        /// Index the anchor named.
        seq_index: u32,
    },
    /// The anchor's excerpt never matched the base revision, so there is
    /// nothing to carry forward.
    #[error("the excerpt of segment {seq_index} does not match the base revision")]
    ExcerptMismatch {
        /// Index the anchor named.
        seq_index: u32,
    },
}

impl AnchorStaleness {
    /// Stable machine-readable discriminator, for the review panel and for
    /// `ai_outputs.stale_reason`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::RevisionMismatch { .. } => "revision_mismatch",
            Self::SegmentNotFound { .. } => "segment_not_found",
            Self::EmptySpan { .. } => "empty_span",
            Self::SpanEdited { .. } => "span_edited",
            Self::ExcerptMismatch { .. } => "excerpt_mismatch",
        }
    }
}

/// Map one anchor through the edits applied so far.
#[must_use]
pub fn map_anchor(document: &TranscriptDocument, anchor: &SourceAnchor) -> AnchorMapping {
    if anchor.revision_id != document.base_revision_id() {
        return AnchorMapping::Stale(AnchorStaleness::RevisionMismatch {
            cited: anchor.revision_id,
            base: document.base_revision_id(),
        });
    }
    if anchor.char_end <= anchor.char_start {
        return AnchorMapping::Stale(AnchorStaleness::EmptySpan {
            seq_index: anchor.seq_index,
        });
    }
    let Some(segment) = document.segment_by_seq_index(anchor.seq_index) else {
        return AnchorMapping::Stale(AnchorStaleness::SegmentNotFound {
            seq_index: anchor.seq_index,
        });
    };

    let start = anchor.char_start as usize;
    let end = anchor.char_end as usize;
    let original: String = segment
        .original_text()
        .chars()
        .skip(start)
        .take(end - start)
        .collect();
    if original.chars().count() != end - start {
        return AnchorMapping::Stale(AnchorStaleness::SpanEdited {
            seq_index: anchor.seq_index,
        });
    }
    if plaudon_domain::anchor::normalize_excerpt(&original)
        != plaudon_domain::anchor::normalize_excerpt(&anchor.excerpt)
    {
        return AnchorMapping::Stale(AnchorStaleness::ExcerptMismatch {
            seq_index: anchor.seq_index,
        });
    }

    match segment.piece_table().map_original_span(start, end) {
        SpanMapping::Mapped {
            start: new_start,
            end: new_end,
        } => AnchorMapping::Mapped(MappedSpan {
            seq_index: anchor.seq_index,
            char_start: u32::try_from(new_start).unwrap_or(u32::MAX),
            char_end: u32::try_from(new_end).unwrap_or(u32::MAX),
            excerpt: anchor.excerpt.clone(),
            shifted: new_start != start,
        }),
        SpanMapping::Broken => AnchorMapping::Stale(AnchorStaleness::SpanEdited {
            seq_index: anchor.seq_index,
        }),
    }
}

/// Map a batch of anchors in one pass.
#[must_use]
pub fn map_anchors(document: &TranscriptDocument, anchors: &[SourceAnchor]) -> Vec<AnchorMapping> {
    anchors
        .iter()
        .map(|anchor| map_anchor(document, anchor))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{document_with, revision_with};
    use plaudon_domain::anchor::AnchorVerification;
    use plaudon_domain::time::Timestamp;

    fn anchor_for(
        document: &TranscriptDocument,
        seq_index: u32,
        start: u32,
        end: u32,
    ) -> SourceAnchor {
        let segment = document
            .segment_by_seq_index(seq_index)
            .expect("segment exists");
        let excerpt: String = segment
            .original_text()
            .chars()
            .skip(start as usize)
            .take((end - start) as usize)
            .collect();
        SourceAnchor::new(document.base_revision_id(), seq_index, start, end, excerpt)
    }

    #[test]
    fn an_edit_elsewhere_in_the_segment_shifts_the_anchor() {
        let mut document = document_with(&["we will not pay the invoice"]);
        let anchor = anchor_for(&document, 0, 8, 15);
        let id = document.first_id().expect("segment");
        document.insert_text(id, 0, "Frankly, ").expect("insert");
        let mapping = map_anchor(&document, &anchor);
        let span = mapping.mapped().expect("mapped");
        assert_eq!((span.char_start, span.char_end), (17, 24));
        assert!(span.shifted);
    }

    #[test]
    fn an_edit_in_another_segment_leaves_the_anchor_untouched() {
        let mut document = document_with(&["we will not pay", "later that week"]);
        let anchor = anchor_for(&document, 0, 0, 15);
        let second = document.segment_at(1).expect("segment").id;
        document.insert_text(second, 0, "much ").expect("insert");
        let span = map_anchor(&document, &anchor)
            .mapped()
            .expect("mapped")
            .clone();
        assert_eq!((span.char_start, span.char_end), (0, 15));
        assert!(!span.shifted);
    }

    #[test]
    fn editing_the_cited_text_marks_the_anchor_stale() {
        let mut document = document_with(&["we will not pay"]);
        let anchor = anchor_for(&document, 0, 0, 15);
        let id = document.first_id().expect("segment");
        document.delete_text(id, 8, "not ").expect("delete");
        let staleness = map_anchor(&document, &anchor)
            .staleness()
            .expect("stale")
            .clone();
        assert_eq!(staleness.code(), "span_edited");
    }

    #[test]
    fn an_anchor_citing_another_revision_is_refused() {
        let document = document_with(&["we will not pay"]);
        let anchor = SourceAnchor::new(
            plaudon_domain::ids::TranscriptRevisionId::new(),
            0,
            0,
            5,
            "we wi",
        );
        assert_eq!(
            map_anchor(&document, &anchor)
                .staleness()
                .map(AnchorStaleness::code),
            Some("revision_mismatch")
        );
    }

    #[test]
    fn a_mapped_anchor_resolves_against_the_saved_child_revision() {
        // The whole point of the mapping: the carried-forward anchor must
        // verify against the revision the edits were saved as, using the
        // domain resolver, not a claim of ours.
        let original = revision_with(&["we will not pay the invoice", "that is final"]);
        let mut document = TranscriptDocument::from_revision(&original);
        let anchor = {
            let segment = document.segment_at(0).expect("segment");
            SourceAnchor::new(document.base_revision_id(), 0, 8, 15, segment.slice(8, 15))
        };
        assert!(anchor.resolve(&original).is_verified());

        let id = document.first_id().expect("segment");
        document.insert_text(id, 0, "Frankly, ").expect("insert");
        let mapping = map_anchor(&document, &anchor);
        let span = mapping.mapped().expect("mapped").clone();

        let child = original
            .edited(crate::doc::domain_segments(&document), Timestamp::now())
            .expect("child revision");
        document.adopt_saved_revision(child.id);

        let carried = span.into_anchor(child.id);
        match carried.resolve(&child) {
            AnchorVerification::Verified(verified) => {
                assert_eq!(verified.matched_text, "not pay");
            }
            AnchorVerification::Rejected(rejection) => {
                panic!("carried anchor must resolve, got {rejection}")
            }
        }
        // The original revision is untouched and still resolves its own anchor.
        assert!(anchor.resolve(&original).is_verified());
    }
}
