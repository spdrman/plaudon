//! Source anchors into an immutable transcript revision (spec 9.6, 9.8).
//!
//! An anchor is the only way a generated claim is allowed to refer to the
//! recording. Verification is purely local: it resolves the anchor against the
//! exact revision it names and compares the quoted excerpt to the stored
//! segment text. A model assertion never verifies itself (spec 9.6).
//!
//! A verified anchor establishes that the model referenced existing text. It
//! does **not** establish that the text entails the claim (spec 9.6A); that
//! distinction belongs to the claim state, not to this module.

use plaudon_domain::ids::TranscriptRevisionId;
use plaudon_domain::transcript::{AlignmentOrigin, TranscriptRevision};
use serde::{Deserialize, Serialize};

/// A reference to a character span of one segment of one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAnchor {
    /// The immutable revision the span belongs to.
    pub revision_id: TranscriptRevisionId,
    /// Zero-based segment position inside that revision.
    pub seq_index: u32,
    /// Inclusive start, counted in Unicode scalar values.
    pub char_start: u32,
    /// Exclusive end, counted in Unicode scalar values.
    pub char_end: u32,
    /// The quoted span, as the producer claims it reads.
    pub excerpt: String,
}

impl SourceAnchor {
    /// Construct an anchor.
    #[must_use]
    pub fn new(
        revision_id: TranscriptRevisionId,
        seq_index: u32,
        char_start: u32,
        char_end: u32,
        excerpt: impl Into<String>,
    ) -> Self {
        Self {
            revision_id,
            seq_index,
            char_start,
            char_end,
            excerpt: excerpt.into(),
        }
    }
}

/// Why an anchor was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorRejection {
    /// The anchor names a different revision than the one being verified.
    RevisionMismatch,
    /// The revision has no segment at that position.
    SegmentNotFound,
    /// The character span falls outside the segment text.
    SpanOutOfBounds,
    /// The quoted excerpt is not what the segment actually says there.
    ExcerptMismatch,
    /// The span is empty, so it quotes nothing.
    EmptySpan,
}

impl AnchorRejection {
    /// Stable machine-readable discriminator.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RevisionMismatch => "revision_mismatch",
            Self::SegmentNotFound => "segment_not_found",
            Self::SpanOutOfBounds => "span_out_of_bounds",
            Self::ExcerptMismatch => "excerpt_mismatch",
            Self::EmptySpan => "empty_span",
        }
    }
}

/// Outcome of verifying one anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AnchorVerification {
    /// The anchor resolves and the excerpt matches.
    Verified {
        /// Playback position, present only when the revision carries real
        /// times (spec 8.6A). `None` means no seek affordance may be shown.
        seek_ms: Option<u64>,
        /// Where those times came from.
        alignment_origin: AlignmentOrigin,
    },
    /// The anchor was refused and must not be displayed.
    Rejected(AnchorRejection),
}

impl AnchorVerification {
    /// Whether the anchor may be surfaced to the user.
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    /// The rejection reason, when there is one.
    #[must_use]
    pub const fn rejection(&self) -> Option<AnchorRejection> {
        match self {
            Self::Verified { .. } => None,
            Self::Rejected(reason) => Some(*reason),
        }
    }
}

/// Collapse runs of whitespace and trim, so an excerpt that differs only in
/// spacing still matches. Nothing else about the text is altered: a changed
/// word is a mismatch.
#[must_use]
pub fn normalize_excerpt(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

/// Resolve and check one anchor against the revision it names.
#[must_use]
pub fn verify_anchor(anchor: &SourceAnchor, revision: &TranscriptRevision) -> AnchorVerification {
    if anchor.revision_id != revision.id {
        return AnchorVerification::Rejected(AnchorRejection::RevisionMismatch);
    }
    if anchor.char_end <= anchor.char_start {
        return AnchorVerification::Rejected(AnchorRejection::EmptySpan);
    }
    let Some(segment) = revision
        .segments()
        .iter()
        .find(|s| s.seq_index == anchor.seq_index)
    else {
        return AnchorVerification::Rejected(AnchorRejection::SegmentNotFound);
    };

    let chars: Vec<char> = segment.text.chars().collect();
    let start = anchor.char_start as usize;
    let end = anchor.char_end as usize;
    if end > chars.len() {
        return AnchorVerification::Rejected(AnchorRejection::SpanOutOfBounds);
    }
    let actual: String = chars[start..end].iter().collect();
    if normalize_excerpt(&actual) != normalize_excerpt(&anchor.excerpt) {
        return AnchorVerification::Rejected(AnchorRejection::ExcerptMismatch);
    }

    let seek_ms = if revision.alignment_origin.permits_seek() {
        segment
            .start
            .map(plaudon_domain::timerange::SourceTimeMs::get)
    } else {
        None
    };
    AnchorVerification::Verified {
        seek_ms,
        alignment_origin: revision.alignment_origin,
    }
}

/// Split anchors into the ones that verify and the ones that do not.
///
/// Fabricated anchors are returned with their reason rather than silently
/// dropped, so a caller can refuse the whole claim (spec 9.5).
#[must_use]
pub fn partition_anchors(
    anchors: &[SourceAnchor],
    revision: &TranscriptRevision,
) -> (Vec<SourceAnchor>, Vec<(SourceAnchor, AnchorRejection)>) {
    let mut verified = Vec::new();
    let mut rejected = Vec::new();
    for anchor in anchors {
        match verify_anchor(anchor, revision) {
            AnchorVerification::Verified { .. } => verified.push(anchor.clone()),
            AnchorVerification::Rejected(reason) => rejected.push((anchor.clone(), reason)),
        }
    }
    (verified, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::fixture_revision;

    #[test]
    fn an_exact_span_verifies_and_offers_a_seek_when_times_are_real() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let anchor = SourceAnchor::new(revision.id, 0, 0, 7, "We will");
        let result = verify_anchor(&anchor, &revision);
        assert_eq!(
            result,
            AnchorVerification::Verified {
                seek_ms: Some(0),
                alignment_origin: AlignmentOrigin::Native,
            }
        );
    }

    #[test]
    fn a_text_only_revision_never_yields_a_seek_position() {
        let revision = fixture_revision(AlignmentOrigin::None);
        let anchor = SourceAnchor::new(revision.id, 0, 0, 7, "We will");
        let AnchorVerification::Verified { seek_ms, .. } = verify_anchor(&anchor, &revision) else {
            panic!("expected verification");
        };
        assert_eq!(seek_ms, None);
    }

    #[test]
    fn a_fabricated_excerpt_is_rejected() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let anchor = SourceAnchor::new(revision.id, 0, 0, 7, "They did");
        assert_eq!(
            verify_anchor(&anchor, &revision).rejection(),
            Some(AnchorRejection::ExcerptMismatch)
        );
    }

    #[test]
    fn an_anchor_into_another_revision_is_rejected() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let other = fixture_revision(AlignmentOrigin::Native);
        let anchor = SourceAnchor::new(other.id, 0, 0, 7, "We will");
        assert_eq!(
            verify_anchor(&anchor, &revision).rejection(),
            Some(AnchorRejection::RevisionMismatch)
        );
    }

    #[test]
    fn out_of_range_segments_and_spans_are_rejected_distinctly() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let missing = SourceAnchor::new(revision.id, 99, 0, 3, "We ");
        assert_eq!(
            verify_anchor(&missing, &revision).rejection(),
            Some(AnchorRejection::SegmentNotFound)
        );
        let oversize = SourceAnchor::new(revision.id, 0, 0, 10_000, "We will");
        assert_eq!(
            verify_anchor(&oversize, &revision).rejection(),
            Some(AnchorRejection::SpanOutOfBounds)
        );
        let empty = SourceAnchor::new(revision.id, 0, 3, 3, "");
        assert_eq!(
            verify_anchor(&empty, &revision).rejection(),
            Some(AnchorRejection::EmptySpan)
        );
    }

    #[test]
    fn multi_byte_spans_are_counted_in_characters_not_bytes() {
        let revision = crate::testing::revision_with_text("héllo wörld");
        let anchor = SourceAnchor::new(revision.id, 0, 0, 5, "héllo");
        assert!(verify_anchor(&anchor, &revision).is_verified());
    }

    #[test]
    fn whitespace_differences_do_not_break_a_match_but_word_changes_do() {
        assert_eq!(normalize_excerpt("  a   b \n c "), "a b c");
        let revision = fixture_revision(AlignmentOrigin::Native);
        let spaced = SourceAnchor::new(revision.id, 0, 0, 7, "We   will");
        assert!(verify_anchor(&spaced, &revision).is_verified());
    }

    #[test]
    fn partitioning_reports_every_rejected_anchor_rather_than_dropping_it() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let anchors = vec![
            SourceAnchor::new(revision.id, 0, 0, 7, "We will"),
            SourceAnchor::new(revision.id, 0, 0, 7, "invented"),
        ];
        let (ok, bad) = partition_anchors(&anchors, &revision);
        assert_eq!(ok.len(), 1);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].1, AnchorRejection::ExcerptMismatch);
    }
}
