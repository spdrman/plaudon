//! Source anchors: a claim's pointer into an immutable transcript revision
//! (spec 9.6, 1.2(7), task I2.1).
//!
//! An anchor proves **traceability**, never factual correctness. Resolving one
//! establishes exactly three things and nothing more:
//!
//! 1. the referenced revision is the revision the output was generated from,
//! 2. the referenced segment exists in that revision, and
//! 3. the quoted excerpt is the text that actually occupies the cited span.
//!
//! Whether the cited text *entails* the claim is a separate question, handled
//! by [`crate::claim`]. Whether a seek position may be shown is decided by the
//! revision's [`AlignmentOrigin`]: a revision with
//! [`AlignmentOrigin::None`] yields `seek_ms: None`, so no consumer can invent
//! a timeline (spec 8.6A).

use serde::{Deserialize, Serialize};

use crate::ids::TranscriptRevisionId;
use crate::timerange::SourceTimeMs;
use crate::transcript::{AlignmentOrigin, TranscriptRevision, TranscriptSegment};

/// Longest excerpt an anchor may carry, in Unicode scalar values.
///
/// The bound exists so a model cannot smuggle an entire transcript — or an
/// unbounded allocation — through the citation field (spec 9.5 string length
/// validation).
pub const MAX_EXCERPT_CHARS: u32 = 2_000;

/// A citation into one immutable transcript revision.
///
/// Offsets are Unicode scalar ([`char`]) offsets into
/// [`TranscriptSegment::text`], not byte offsets, so a model that counts
/// characters cannot accidentally split a multi-byte grapheme's encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAnchor {
    /// Revision the excerpt was quoted from.
    pub revision_id: TranscriptRevisionId,
    /// Zero-based index of the segment inside that revision.
    pub seq_index: u32,
    /// Inclusive start of the span, in `char` offsets.
    pub char_start: u32,
    /// Exclusive end of the span, in `char` offsets.
    pub char_end: u32,
    /// The text the model claims occupies the span.
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

    /// Resolve the anchor against the revision it claims to cite.
    ///
    /// This performs no model call and no I/O, so a stored verification result
    /// is reproducible offline (I2.1 acceptance).
    pub fn resolve(&self, revision: &TranscriptRevision) -> AnchorVerification {
        if let Err(rejection) = self.check_revision(revision) {
            return AnchorVerification::Rejected(rejection);
        }
        let Some(segment) = revision
            .segments()
            .iter()
            .find(|segment| segment.seq_index == self.seq_index)
        else {
            return AnchorVerification::Rejected(AnchorRejection::SegmentNotFound {
                seq_index: self.seq_index,
            });
        };
        match self.span_text(segment) {
            Ok(matched) => AnchorVerification::Verified(VerifiedAnchor {
                revision_id: self.revision_id,
                seq_index: self.seq_index,
                char_start: self.char_start,
                char_end: self.char_end,
                matched_text: matched,
                alignment_origin: revision.alignment_origin,
                seek_ms: seek_position(revision.alignment_origin, segment),
            }),
            Err(rejection) => AnchorVerification::Rejected(rejection),
        }
    }

    fn check_revision(&self, revision: &TranscriptRevision) -> Result<(), AnchorRejection> {
        if revision.id == self.revision_id {
            Ok(())
        } else {
            Err(AnchorRejection::RevisionMismatch {
                cited: self.revision_id,
                resolved: revision.id,
            })
        }
    }

    fn span_text(&self, segment: &TranscriptSegment) -> Result<String, AnchorRejection> {
        if self.char_end <= self.char_start {
            return Err(AnchorRejection::EmptySpan {
                seq_index: self.seq_index,
            });
        }
        let span_chars = self.char_end - self.char_start;
        if span_chars > MAX_EXCERPT_CHARS {
            return Err(AnchorRejection::SpanTooLong {
                seq_index: self.seq_index,
                chars: span_chars,
                max_chars: MAX_EXCERPT_CHARS,
            });
        }
        let segment_chars = u32::try_from(segment.text.chars().count()).unwrap_or(u32::MAX);
        if self.char_end > segment_chars {
            return Err(AnchorRejection::SpanOutOfBounds {
                seq_index: self.seq_index,
                char_end: self.char_end,
                segment_chars,
            });
        }
        let matched: String = segment
            .text
            .chars()
            .skip(self.char_start as usize)
            .take(span_chars as usize)
            .collect();
        if normalize_excerpt(&matched) == normalize_excerpt(&self.excerpt) {
            Ok(matched)
        } else {
            Err(AnchorRejection::ExcerptMismatch {
                seq_index: self.seq_index,
            })
        }
    }
}

/// The seek position a verified anchor may expose, if any.
///
/// `None` for [`AlignmentOrigin::None`]: there is no timeline to seek into and
/// we never synthesize one (spec 8.6A).
fn seek_position(origin: AlignmentOrigin, segment: &TranscriptSegment) -> Option<u64> {
    if origin.permits_seek() {
        segment.start.map(SourceTimeMs::get)
    } else {
        None
    }
}

/// Collapse runs of Unicode whitespace and trim, without case folding.
///
/// Transcript text has already been canonicalized by the revision's
/// `normalization_profile`; the only remaining difference we tolerate between
/// a model's quote and the stored text is whitespace shape. We deliberately do
/// **not** fold case or strip punctuation, because "we will not pay" and "We
/// will not pay." are different claims to a reviewer.
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

/// A resolved, verified anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedAnchor {
    /// Revision that was resolved.
    pub revision_id: TranscriptRevisionId,
    /// Segment that was resolved.
    pub seq_index: u32,
    /// Inclusive start of the verified span.
    pub char_start: u32,
    /// Exclusive end of the verified span.
    pub char_end: u32,
    /// The stored text occupying the span, taken from the revision, not the model.
    pub matched_text: String,
    /// Alignment origin of the revision, for the UI label (spec 8.6A).
    pub alignment_origin: AlignmentOrigin,
    /// Seek position, present only when alignment permits one.
    pub seek_ms: Option<u64>,
}

impl VerifiedAnchor {
    /// Whether the UI may render a seek affordance for this anchor.
    #[must_use]
    pub const fn permits_seek(&self) -> bool {
        self.seek_ms.is_some()
    }
}

/// Why an anchor failed to resolve.
///
/// Every variant is a typed value: grounding failures are rendered in the UI
/// (spec 9.6) and counted by the evaluation suite (spec 17.3), so they must not
/// degrade to prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "rejection", rename_all = "snake_case")]
pub enum AnchorRejection {
    /// The anchor cites a revision other than the one the output was generated from.
    #[error("anchor cites revision {cited}, but the output was generated from {resolved}")]
    RevisionMismatch {
        /// Revision the anchor named.
        cited: TranscriptRevisionId,
        /// Revision the output actually consumed.
        resolved: TranscriptRevisionId,
    },
    /// No segment with that index exists in the revision.
    #[error("revision has no segment {seq_index}")]
    SegmentNotFound {
        /// Index the anchor named.
        seq_index: u32,
    },
    /// The span ends past the end of the segment text.
    #[error("span ends at {char_end} but segment {seq_index} has {segment_chars} characters")]
    SpanOutOfBounds {
        /// Index of the cited segment.
        seq_index: u32,
        /// Exclusive end the anchor named.
        char_end: u32,
        /// Length of the segment, in `char`s.
        segment_chars: u32,
    },
    /// The span is empty or inverted.
    #[error("span of segment {seq_index} is empty")]
    EmptySpan {
        /// Index of the cited segment.
        seq_index: u32,
    },
    /// The span is longer than an excerpt is allowed to be.
    #[error("span of segment {seq_index} covers {chars} characters, more than the {max_chars} allowed")]
    SpanTooLong {
        /// Index of the cited segment.
        seq_index: u32,
        /// Length the anchor named.
        chars: u32,
        /// Configured ceiling.
        max_chars: u32,
    },
    /// The quoted excerpt is not the text stored at the cited span.
    #[error("quoted excerpt does not match the stored text of segment {seq_index}")]
    ExcerptMismatch {
        /// Index of the cited segment.
        seq_index: u32,
    },
}

impl AnchorRejection {
    /// Stable machine-readable discriminator, persisted and reported.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::RevisionMismatch { .. } => "revision_mismatch",
            Self::SegmentNotFound { .. } => "segment_not_found",
            Self::SpanOutOfBounds { .. } => "span_out_of_bounds",
            Self::EmptySpan { .. } => "empty_span",
            Self::SpanTooLong { .. } => "span_too_long",
            Self::ExcerptMismatch { .. } => "excerpt_mismatch",
        }
    }
}

/// The outcome of resolving one anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AnchorVerification {
    /// The anchor resolves to real text in the cited revision.
    Verified(VerifiedAnchor),
    /// The anchor does not resolve.
    Rejected(AnchorRejection),
}

impl AnchorVerification {
    /// Whether the anchor resolved.
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    /// The verified anchor, when the anchor resolved.
    #[must_use]
    pub const fn verified(&self) -> Option<&VerifiedAnchor> {
        match self {
            Self::Verified(anchor) => Some(anchor),
            Self::Rejected(_) => None,
        }
    }

    /// The rejection, when the anchor did not resolve.
    #[must_use]
    pub const fn rejection(&self) -> Option<&AnchorRejection> {
        match self {
            Self::Verified(_) => None,
            Self::Rejected(rejection) => Some(rejection),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ids::RecordingId;
    use crate::media::Sha256Digest;
    use crate::model::{EngineId, ModelIdentity};
    use crate::time::Timestamp;
    use crate::transcript::{RevisionOrigin, RevisionStatus, TranscriptRevisionDraft};

    fn revision(origin: AlignmentOrigin, texts: &[&str]) -> TranscriptRevision {
        let segments = texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let seq = u32::try_from(index).unwrap();
                if origin.permits_times() {
                    TranscriptSegment::timed(
                        seq,
                        u64::from(seq) * 1_000,
                        u64::from(seq) * 1_000 + 900,
                        *text,
                    )
                } else {
                    TranscriptSegment::text_only(seq, *text)
                }
            })
            .collect();
        TranscriptRevision::try_new(TranscriptRevisionDraft {
            recording_id: RecordingId::new(),
            parent_id: None,
            origin: RevisionOrigin::Machine,
            input_sha256: Sha256Digest::of(&[7_u8; 32]),
            engine: EngineId::new("test-engine"),
            model: ModelIdentity::new(EngineId::new("test-engine"), "test-model"),
            locale: None,
            alignment_origin: origin,
            status: RevisionStatus::Complete,
            normalization_profile: "nfc".into(),
            chunking_strategy: "single".into(),
            duration_ms: Some(10_000),
            created_at: Timestamp::from_millis(0),
            segments,
        })
        .expect("valid fixture revision")
    }

    #[test]
    fn an_anchor_over_real_text_verifies_and_exposes_a_seek_position() {
        let rev = revision(AlignmentOrigin::Native, &["We will ship on Friday."]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, 7, "We will");
        let outcome = anchor.resolve(&rev);
        let verified = outcome.verified().expect("anchor resolves");
        assert_eq!(verified.matched_text, "We will");
        assert_eq!(verified.seek_ms, Some(0));
        assert!(verified.permits_seek());
    }

    #[test]
    fn a_text_only_revision_never_yields_a_seek_position() {
        let rev = revision(AlignmentOrigin::None, &["We will ship on Friday."]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, 7, "We will");
        let verified = anchor.resolve(&rev).verified().cloned().expect("resolves");
        assert_eq!(verified.alignment_origin, AlignmentOrigin::None);
        assert_eq!(verified.seek_ms, None, "no invented timeline (spec 8.6A)");
        assert!(!verified.permits_seek());
    }

    #[test]
    fn a_derived_alignment_still_permits_a_labelled_seek() {
        let rev = revision(AlignmentOrigin::Derived, &["Budget approved."]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, 6, "Budget");
        let verified = anchor.resolve(&rev).verified().cloned().expect("resolves");
        assert_eq!(verified.alignment_origin, AlignmentOrigin::Derived);
        assert_eq!(verified.seek_ms, Some(0));
    }

    #[test]
    fn an_anchor_into_a_different_revision_is_rejected() {
        let rev = revision(AlignmentOrigin::Native, &["Alpha."]);
        let other = revision(AlignmentOrigin::Native, &["Alpha."]);
        let anchor = SourceAnchor::new(other.id, 0, 0, 5, "Alpha");
        assert!(matches!(
            anchor.resolve(&rev).rejection(),
            Some(AnchorRejection::RevisionMismatch { .. })
        ));
    }

    #[test]
    fn a_fabricated_segment_index_is_rejected() {
        let rev = revision(AlignmentOrigin::Native, &["Alpha."]);
        let anchor = SourceAnchor::new(rev.id, 9, 0, 5, "Alpha");
        assert_eq!(
            anchor.resolve(&rev).rejection().map(AnchorRejection::code),
            Some("segment_not_found")
        );
    }

    #[test]
    fn an_off_by_one_span_past_the_segment_end_is_rejected() {
        let rev = revision(AlignmentOrigin::Native, &["Alpha."]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, 7, "Alpha.");
        assert_eq!(
            anchor.resolve(&rev).rejection().map(AnchorRejection::code),
            Some("span_out_of_bounds")
        );
    }

    #[test]
    fn an_empty_or_inverted_span_is_rejected() {
        let rev = revision(AlignmentOrigin::Native, &["Alpha."]);
        for (start, end) in [(3, 3), (4, 2)] {
            let anchor = SourceAnchor::new(rev.id, 0, start, end, "");
            assert_eq!(
                anchor.resolve(&rev).rejection().map(AnchorRejection::code),
                Some("empty_span")
            );
        }
    }

    #[test]
    fn a_quote_the_transcript_does_not_contain_is_rejected() {
        let rev = revision(AlignmentOrigin::Native, &["We will ship on Friday."]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, 7, "We will not");
        assert_eq!(
            anchor.resolve(&rev).rejection().map(AnchorRejection::code),
            Some("excerpt_mismatch")
        );
    }

    #[test]
    fn whitespace_shape_does_not_break_a_truthful_quote() {
        let rev = revision(AlignmentOrigin::Native, &["We  will\nship."]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, 8, "We will");
        assert!(anchor.resolve(&rev).is_verified());
    }

    #[test]
    fn case_changes_are_not_tolerated() {
        let rev = revision(AlignmentOrigin::Native, &["We will ship."]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, 7, "we will");
        assert!(!anchor.resolve(&rev).is_verified());
    }

    #[test]
    fn multibyte_spans_are_counted_in_characters_not_bytes() {
        let rev = revision(AlignmentOrigin::Native, &["naïve café décision"]);
        let anchor = SourceAnchor::new(rev.id, 0, 6, 10, "café");
        let verified = anchor.resolve(&rev).verified().cloned().expect("resolves");
        assert_eq!(verified.matched_text, "café");
    }

    #[test]
    fn an_excerpt_longer_than_the_ceiling_is_rejected_before_allocation() {
        let rev = revision(AlignmentOrigin::Native, &["short"]);
        let anchor = SourceAnchor::new(rev.id, 0, 0, MAX_EXCERPT_CHARS + 1, "short");
        assert_eq!(
            anchor.resolve(&rev).rejection().map(AnchorRejection::code),
            Some("span_too_long")
        );
    }

    #[test]
    fn rejection_codes_are_distinct() {
        let codes = [
            AnchorRejection::RevisionMismatch {
                cited: TranscriptRevisionId::new(),
                resolved: TranscriptRevisionId::new(),
            },
            AnchorRejection::SegmentNotFound { seq_index: 0 },
            AnchorRejection::SpanOutOfBounds {
                seq_index: 0,
                char_end: 1,
                segment_chars: 0,
            },
            AnchorRejection::EmptySpan { seq_index: 0 },
            AnchorRejection::SpanTooLong {
                seq_index: 0,
                chars: 1,
                max_chars: 0,
            },
            AnchorRejection::ExcerptMismatch { seq_index: 0 },
        ]
        .iter()
        .map(AnchorRejection::code)
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(codes.len(), 6);
    }
}
