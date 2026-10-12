//! The document: an ordered list of segments with identifiers that survive
//! every edit.
//!
//! A transcript revision is immutable (`plaudon_domain::transcript`), so the
//! document never mutates the revision it was loaded from. It keeps the base
//! revision's text inside each segment's piece table and produces *edits*,
//! which the host persists as a child revision through the existing
//! `save_transcript_revision` port. Nothing in this crate touches SQLite.
//!
//! Segments are neither created, split, merged nor deleted. That is a
//! deliberate constraint, not a missing feature: the save contract carries
//! `(seq_index, text)` pairs against a base revision, and
//! [`plaudon_domain::anchor::SourceAnchor`] cites `seq_index`, so a document
//! that renumbered segments would silently invalidate every citation and every
//! speaker turn in the recording.

use std::collections::HashMap;

use plaudon_domain::ids::{RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_domain::timerange::SourceTimeMs;
use plaudon_domain::transcript::{
    AlignmentOrigin, Confidence, TranscriptRevision, TranscriptSegment, WordAlignment,
};

use crate::text::SegmentText;

/// Identity of a segment inside one open document.
///
/// Stable for the lifetime of the document: it is allocated once, when the
/// segment is loaded, and no edit ever reassigns it. Carets, selections,
/// search results, speaker chips and anchor mappings all address segments
/// through this type rather than through a list index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u64);

impl SegmentId {
    /// The identity of "no segment".
    ///
    /// A document with no segments still needs a caret anchor. Identities are
    /// allocated upwards from zero, so this value is never minted.
    pub const NONE: Self = Self(u64::MAX);

    /// The raw value, for stable ordering and for egui widget ids.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SegmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "segment-{}", self.0)
    }
}

/// One segment of the open document.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Stable identity within this document.
    pub id: SegmentId,
    /// Position of the segment in the base revision. This is the coordinate
    /// anchors and the save command use, and it never changes.
    pub seq_index: u32,
    /// Start offset, present only when the revision's alignment permits times.
    pub start: Option<SourceTimeMs>,
    /// End offset, present only when the revision's alignment permits times.
    pub end: Option<SourceTimeMs>,
    /// Current speaker attribution.
    pub speaker_id: Option<SpeakerId>,
    /// Speaker attribution in the base revision, so a reassignment is
    /// reversible without re-reading the database.
    pub base_speaker_id: Option<SpeakerId>,
    /// Engine-scoped confidence, carried through unchanged.
    pub confidence: Option<Confidence>,
    /// Word alignments of the base revision. They are dropped from a segment
    /// whose text the user edited, because they no longer describe it.
    pub words: Vec<WordAlignment>,
    text: SegmentText,
    version: u64,
}

impl Segment {
    /// The current text.
    #[must_use]
    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        self.text.text()
    }

    /// The text of this segment in the base revision.
    #[must_use]
    pub fn original_text(&self) -> &str {
        self.text.original()
    }

    /// Length of the current text in `char`s.
    #[must_use]
    pub const fn char_len(&self) -> usize {
        self.text.char_len()
    }

    /// Whether the current text differs from the base revision's text.
    ///
    /// The cheap structural test comes first: a segment whose piece table was
    /// never touched cannot differ. A segment that was edited back to its
    /// original wording is *not* dirty — there is nothing to save — even
    /// though its anchor evidence was destroyed, which is why anchor mapping
    /// asks [`SegmentText::is_edited`] instead.
    #[must_use]
    pub fn is_text_edited(&self) -> bool {
        self.text.is_edited() && self.text.text() != self.text.original()
    }

    /// Whether the speaker differs from the base revision.
    #[must_use]
    pub fn is_speaker_reassigned(&self) -> bool {
        self.speaker_id != self.base_speaker_id
    }

    /// A monotonically increasing counter bumped on every change to this
    /// segment. Render caches key on it.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// The piece table, for search and anchor remapping.
    #[must_use]
    pub const fn piece_table(&self) -> &SegmentText {
        &self.text
    }

    /// The substring `[start, end)` in `char` offsets.
    #[must_use]
    pub fn slice(&self, start: usize, end: usize) -> String {
        self.text.slice(start, end)
    }

    /// Whether a seek affordance may be offered for this segment.
    #[must_use]
    pub const fn permits_seek(&self, origin: AlignmentOrigin) -> bool {
        origin.permits_seek() && self.start.is_some()
    }
}

/// Everything needed to open a document without a full domain revision.
///
/// The desktop shell pages segments in through `get_transcript`, which returns
/// DTOs rather than a [`TranscriptRevision`]; this is the constructor it uses.
#[derive(Debug, Clone)]
pub struct DocumentSource {
    /// Recording the transcript belongs to.
    pub recording_id: RecordingId,
    /// Revision the document is based on.
    pub revision_id: TranscriptRevisionId,
    /// Alignment origin of that revision. It decides whether timestamps and
    /// seek affordances may be shown at all (spec 8.6A).
    pub alignment_origin: AlignmentOrigin,
    /// Media duration, when known.
    pub duration_ms: Option<u64>,
    /// Whether the revision is the head of its lineage. A non-head revision is
    /// opened read-only.
    pub editable: bool,
    /// Ordered segments.
    pub segments: Vec<SegmentSeed>,
}

/// One segment as loaded from a revision.
#[derive(Debug, Clone)]
pub struct SegmentSeed {
    /// Position in the base revision.
    pub seq_index: u32,
    /// Start offset in milliseconds.
    pub start_ms: Option<u64>,
    /// End offset in milliseconds.
    pub end_ms: Option<u64>,
    /// Segment text.
    pub text: String,
    /// Speaker attribution.
    pub speaker_id: Option<SpeakerId>,
    /// Engine-scoped confidence.
    pub confidence: Option<Confidence>,
    /// Word alignments.
    pub words: Vec<WordAlignment>,
}

impl SegmentSeed {
    /// A text-only seed.
    #[must_use]
    pub fn text_only(seq_index: u32, text: impl Into<String>) -> Self {
        Self {
            seq_index,
            start_ms: None,
            end_ms: None,
            text: text.into(),
            speaker_id: None,
            confidence: None,
            words: Vec::new(),
        }
    }

    /// A timed seed.
    #[must_use]
    pub fn timed(seq_index: u32, start_ms: u64, end_ms: u64, text: impl Into<String>) -> Self {
        Self {
            seq_index,
            start_ms: Some(start_ms),
            end_ms: Some(end_ms),
            text: text.into(),
            speaker_id: None,
            confidence: None,
            words: Vec::new(),
        }
    }

    /// Attach a speaker.
    #[must_use]
    pub fn with_speaker(mut self, speaker_id: SpeakerId) -> Self {
        self.speaker_id = Some(speaker_id);
        self
    }
}

/// Why a document operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocError {
    /// No segment with that identity exists in this document.
    #[error("no segment {0} in this document")]
    UnknownSegment(SegmentId),
    /// An offset addressed a position past the end of a segment.
    #[error("offset {offset} is past the end of segment {segment} ({len} characters)")]
    OffsetOutOfRange {
        /// Segment addressed.
        segment: SegmentId,
        /// Offset requested.
        offset: usize,
        /// Current length of the segment, in `char`s.
        len: usize,
    },
    /// Replaying a delete found different text than the operation recorded.
    /// The log and the document have diverged, which is a bug, not user error.
    #[error("segment {segment} holds {found:?} where the operation log expected {expected:?}")]
    TextMismatch {
        /// Segment addressed.
        segment: SegmentId,
        /// Text the operation recorded.
        expected: String,
        /// Text actually present.
        found: String,
    },
    /// The document was opened read-only (a superseded revision).
    #[error("this revision is not the head of its lineage and cannot be edited")]
    ReadOnly,
}

/// An ordered transcript with stable segment identities.
#[derive(Debug, Clone)]
pub struct TranscriptDocument {
    recording_id: RecordingId,
    base_revision_id: TranscriptRevisionId,
    alignment_origin: AlignmentOrigin,
    duration_ms: Option<u64>,
    editable: bool,
    segments: Vec<Segment>,
    index: HashMap<SegmentId, usize>,
    next_id: u64,
    version: u64,
    saved_revisions: Vec<TranscriptRevisionId>,
}

impl TranscriptDocument {
    /// Open a document over a loaded revision.
    #[must_use]
    pub fn new(source: DocumentSource) -> Self {
        let mut document = Self {
            recording_id: source.recording_id,
            base_revision_id: source.revision_id,
            alignment_origin: source.alignment_origin,
            duration_ms: source.duration_ms,
            editable: source.editable,
            segments: Vec::with_capacity(source.segments.len()),
            index: HashMap::with_capacity(source.segments.len()),
            next_id: 0,
            version: 0,
            saved_revisions: Vec::new(),
        };
        for seed in source.segments {
            document.push_seed(seed);
        }
        document
    }

    /// Open a document over an immutable domain revision.
    ///
    /// The revision itself is never modified; it remains the original the
    /// lineage keeps (spec 8.6).
    #[must_use]
    pub fn from_revision(revision: &TranscriptRevision) -> Self {
        let permits_times = revision.alignment_origin.permits_times();
        Self::new(DocumentSource {
            recording_id: revision.recording_id,
            revision_id: revision.id,
            alignment_origin: revision.alignment_origin,
            duration_ms: revision.duration_ms,
            editable: true,
            segments: revision
                .segments()
                .iter()
                .map(|segment| SegmentSeed {
                    seq_index: segment.seq_index,
                    start_ms: permits_times
                        .then(|| segment.start.map(SourceTimeMs::get))
                        .flatten(),
                    end_ms: permits_times
                        .then(|| segment.end.map(SourceTimeMs::get))
                        .flatten(),
                    text: segment.text.clone(),
                    speaker_id: segment.speaker_id,
                    confidence: segment.confidence.clone(),
                    words: segment.words.clone(),
                })
                .collect(),
        })
    }

    fn push_seed(&mut self, seed: SegmentSeed) {
        let id = SegmentId(self.next_id);
        self.next_id += 1;
        let position = self.segments.len();
        let permits_times = self.alignment_origin.permits_times();
        self.segments.push(Segment {
            id,
            seq_index: seed.seq_index,
            start: permits_times
                .then(|| seed.start_ms.map(SourceTimeMs::new))
                .flatten(),
            end: permits_times
                .then(|| seed.end_ms.map(SourceTimeMs::new))
                .flatten(),
            speaker_id: seed.speaker_id,
            base_speaker_id: seed.speaker_id,
            confidence: seed.confidence,
            words: seed.words,
            text: SegmentText::new(seed.text),
            version: 0,
        });
        self.index.insert(id, position);
    }

    /// Recording the document belongs to.
    #[must_use]
    pub const fn recording_id(&self) -> RecordingId {
        self.recording_id
    }

    /// The revision the current text is expressed against. Anchors resolve
    /// against this revision until a child revision is saved.
    #[must_use]
    pub const fn base_revision_id(&self) -> TranscriptRevisionId {
        self.base_revision_id
    }

    /// Revisions this session has created, oldest first.
    #[must_use]
    pub fn saved_revisions(&self) -> &[TranscriptRevisionId] {
        &self.saved_revisions
    }

    /// Alignment origin of the base revision.
    #[must_use]
    pub const fn alignment_origin(&self) -> AlignmentOrigin {
        self.alignment_origin
    }

    /// Whether a seek affordance may be offered anywhere in this document.
    #[must_use]
    pub const fn permits_seek(&self) -> bool {
        self.alignment_origin.permits_seek()
    }

    /// Media duration, when the revision recorded one.
    #[must_use]
    pub const fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    /// Whether this document may be edited at all.
    #[must_use]
    pub const fn is_editable(&self) -> bool {
        self.editable
    }

    /// A counter bumped on every mutation, for render and search caches.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// The ordered segments.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Number of segments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Whether the document has no segments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Position of a segment in document order.
    #[must_use]
    pub fn position(&self, id: SegmentId) -> Option<usize> {
        self.index.get(&id).copied()
    }

    /// A segment by identity.
    #[must_use]
    pub fn segment(&self, id: SegmentId) -> Option<&Segment> {
        self.position(id).map(|index| &self.segments[index])
    }

    /// A segment by document position.
    #[must_use]
    pub fn segment_at(&self, position: usize) -> Option<&Segment> {
        self.segments.get(position)
    }

    /// The segment carrying a base-revision index.
    #[must_use]
    pub fn segment_by_seq_index(&self, seq_index: u32) -> Option<&Segment> {
        // `seq_index` is contiguous and zero-based in a validated revision, so
        // the direct hit is tried first and the scan is only a fallback for a
        // partially paged document.
        let direct = self.segments.get(seq_index as usize);
        match direct {
            Some(segment) if segment.seq_index == seq_index => Some(segment),
            _ => self
                .segments
                .iter()
                .find(|segment| segment.seq_index == seq_index),
        }
    }

    /// The first segment identity in document order.
    #[must_use]
    pub fn first_id(&self) -> Option<SegmentId> {
        self.segments.first().map(|segment| segment.id)
    }

    /// The last segment identity in document order.
    #[must_use]
    pub fn last_id(&self) -> Option<SegmentId> {
        self.segments.last().map(|segment| segment.id)
    }

    /// The segment after `id`, in document order.
    #[must_use]
    pub fn next_id(&self, id: SegmentId) -> Option<SegmentId> {
        let position = self.position(id)?;
        self.segments.get(position + 1).map(|segment| segment.id)
    }

    /// The segment before `id`, in document order.
    #[must_use]
    pub fn previous_id(&self, id: SegmentId) -> Option<SegmentId> {
        let position = self.position(id)?.checked_sub(1)?;
        self.segments.get(position).map(|segment| segment.id)
    }

    /// The segment that covers `position_ms`, or the last one that started
    /// before it.
    ///
    /// In a gap between two segments the last spoken segment stays selected:
    /// the playhead is not inside any segment, and jumping the highlight to a
    /// segment that has not started would be a lie about what is being said.
    ///
    /// Returns `None` for a revision whose alignment origin forbids times, so
    /// an unaligned transcript cannot be highlighted against a synthesized
    /// timeline (spec 8.6A).
    #[must_use]
    pub fn segment_at_ms(&self, position_ms: u64) -> Option<SegmentId> {
        if !self.permits_seek() {
            return None;
        }
        // Starts are non-decreasing in a validated revision, so the split
        // point is a binary search even on a 30,000-segment transcript.
        let started = self
            .segments
            .partition_point(|segment| segment.start.is_some_and(|s| s.get() <= position_ms));
        self.segments[..started]
            .iter()
            .rev()
            .find(|segment| segment.start.is_some())
            .map(|segment| segment.id)
    }

    /// Segments whose text differs from the base revision, as the
    /// `(seq_index, text)` pairs `save_transcript_revision` expects.
    #[must_use]
    pub fn text_edits(&self) -> Vec<(u32, String)> {
        self.segments
            .iter()
            .filter(|segment| segment.is_text_edited())
            .map(|segment| (segment.seq_index, segment.text().into_owned()))
            .collect()
    }

    /// Segments whose speaker was reassigned, as `(seq_index, speaker)` pairs.
    #[must_use]
    pub fn speaker_reassignments(&self) -> Vec<(u32, Option<SpeakerId>)> {
        self.segments
            .iter()
            .filter(|segment| segment.is_speaker_reassigned())
            .map(|segment| (segment.seq_index, segment.speaker_id))
            .collect()
    }

    /// Whether anything differs from the base revision.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.is_text_edited() || segment.is_speaker_reassigned())
    }

    /// Adopt a revision that was persisted from the current text.
    ///
    /// The current text becomes the new base text, so subsequent edits are
    /// expressed against the saved revision and anchors are remapped against
    /// it. Call [`crate::anchors::map_anchor`] for any anchor that must be
    /// carried forward *before* this, because this is the point at which the
    /// previous base coordinates stop existing.
    pub fn adopt_saved_revision(&mut self, revision_id: TranscriptRevisionId) {
        for segment in &mut self.segments {
            segment.text.rebase();
            segment.base_speaker_id = segment.speaker_id;
        }
        self.saved_revisions.push(revision_id);
        self.base_revision_id = revision_id;
        self.version += 1;
    }

    pub(crate) fn insert_text(
        &mut self,
        id: SegmentId,
        offset: usize,
        text: &str,
    ) -> Result<(), DocError> {
        self.require_editable()?;
        let segment = self.segment_mut(id)?;
        if offset > segment.text.char_len() {
            return Err(DocError::OffsetOutOfRange {
                segment: id,
                offset,
                len: segment.text.char_len(),
            });
        }
        segment.text.insert(offset, text);
        segment.words.clear();
        segment.version += 1;
        self.version += 1;
        Ok(())
    }

    pub(crate) fn delete_text(
        &mut self,
        id: SegmentId,
        offset: usize,
        expected: &str,
    ) -> Result<String, DocError> {
        self.require_editable()?;
        let count = expected.chars().count();
        let segment = self.segment_mut(id)?;
        if offset + count > segment.text.char_len() {
            return Err(DocError::OffsetOutOfRange {
                segment: id,
                offset: offset + count,
                len: segment.text.char_len(),
            });
        }
        let found = segment.text.slice(offset, offset + count);
        if found != expected {
            return Err(DocError::TextMismatch {
                segment: id,
                expected: expected.to_owned(),
                found,
            });
        }
        let removed = segment.text.remove(offset, offset + count);
        segment.words.clear();
        segment.version += 1;
        self.version += 1;
        Ok(removed)
    }

    pub(crate) fn set_speaker(
        &mut self,
        id: SegmentId,
        speaker: Option<SpeakerId>,
    ) -> Result<(), DocError> {
        self.require_editable()?;
        let segment = self.segment_mut(id)?;
        segment.speaker_id = speaker;
        segment.version += 1;
        self.version += 1;
        Ok(())
    }

    fn require_editable(&self) -> Result<(), DocError> {
        if self.editable {
            Ok(())
        } else {
            Err(DocError::ReadOnly)
        }
    }

    fn segment_mut(&mut self, id: SegmentId) -> Result<&mut Segment, DocError> {
        let position = *self.index.get(&id).ok_or(DocError::UnknownSegment(id))?;
        Ok(&mut self.segments[position])
    }
}

/// Build the domain segments for a child revision carrying the current text.
///
/// The caller owns persistence; this exists so a host that already holds the
/// parent [`TranscriptRevision`] can call
/// [`TranscriptRevision::edited`] without rebuilding the mapping by hand.
#[must_use]
pub fn domain_segments(document: &TranscriptDocument) -> Vec<TranscriptSegment> {
    document
        .segments()
        .iter()
        .map(|segment| TranscriptSegment {
            seq_index: segment.seq_index,
            start: segment.start,
            end: segment.end,
            text: segment.text().into_owned(),
            confidence: segment.confidence.clone(),
            speaker_id: segment.speaker_id,
            words: segment.words.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::document_with;

    #[test]
    fn segment_ids_are_allocated_in_document_order_and_resolve() {
        let document = document_with(&["one", "two", "three"]);
        let ids: Vec<SegmentId> = document.segments().iter().map(|s| s.id).collect();
        assert_eq!(document.position(ids[1]), Some(1));
        assert_eq!(document.next_id(ids[0]), Some(ids[1]));
        assert_eq!(document.previous_id(ids[0]), None);
        assert_eq!(
            document.segment(ids[2]).map(|s| s.text().into_owned()),
            Some("three".to_owned())
        );
    }

    #[test]
    fn editing_does_not_change_identities_or_seq_indices() {
        let mut document = document_with(&["one", "two"]);
        let ids: Vec<SegmentId> = document.segments().iter().map(|s| s.id).collect();
        document
            .insert_text(ids[0], 3, " thousand")
            .expect("insert");
        assert_eq!(
            document.segments().iter().map(|s| s.id).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(
            document
                .segments()
                .iter()
                .map(|s| s.seq_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(document.text_edits(), vec![(0, "one thousand".to_owned())]);
    }

    #[test]
    fn a_text_only_revision_exposes_no_times_and_no_seek() {
        let document = TranscriptDocument::new(DocumentSource {
            recording_id: RecordingId::new(),
            revision_id: TranscriptRevisionId::new(),
            alignment_origin: AlignmentOrigin::None,
            duration_ms: Some(10_000),
            editable: true,
            segments: vec![SegmentSeed {
                start_ms: Some(1_000),
                end_ms: Some(2_000),
                ..SegmentSeed::text_only(0, "hello")
            }],
        });
        assert!(!document.permits_seek());
        assert_eq!(document.segment_at(0).and_then(|s| s.start), None);
        assert_eq!(document.segment_at_ms(1_500), None);
    }

    #[test]
    fn the_segment_at_a_playback_position_is_the_last_one_started() {
        let document = TranscriptDocument::new(DocumentSource {
            recording_id: RecordingId::new(),
            revision_id: TranscriptRevisionId::new(),
            alignment_origin: AlignmentOrigin::Native,
            duration_ms: Some(10_000),
            editable: true,
            segments: vec![
                SegmentSeed::timed(0, 0, 1_000, "first"),
                SegmentSeed::timed(1, 1_000, 2_000, "second"),
                SegmentSeed::timed(2, 5_000, 6_000, "third"),
            ],
        });
        let ids: Vec<SegmentId> = document.segments().iter().map(|s| s.id).collect();
        assert_eq!(document.segment_at_ms(0), Some(ids[0]));
        assert_eq!(document.segment_at_ms(1_500), Some(ids[1]));
        // In the gap after the second segment the highlight stays put rather
        // than jumping ahead to a segment that has not started.
        assert_eq!(document.segment_at_ms(3_000), Some(ids[1]));
        assert_eq!(document.segment_at_ms(5_500), Some(ids[2]));
    }

    #[test]
    fn adopting_a_saved_revision_rebases_the_text_and_the_speakers() {
        let mut document = document_with(&["one", "two"]);
        let id = document.first_id().expect("segment");
        document.insert_text(id, 3, "!").expect("insert");
        assert!(document.is_dirty());
        let saved = TranscriptRevisionId::new();
        document.adopt_saved_revision(saved);
        assert!(!document.is_dirty());
        assert_eq!(document.base_revision_id(), saved);
        assert_eq!(document.saved_revisions(), &[saved]);
        assert_eq!(
            document.segment(id).expect("segment").original_text(),
            "one!"
        );
    }

    #[test]
    fn a_read_only_document_refuses_every_mutation() {
        let mut document = document_with(&["one"]);
        document.editable = false;
        let id = document.first_id().expect("segment");
        assert_eq!(document.insert_text(id, 0, "x"), Err(DocError::ReadOnly));
        assert_eq!(document.set_speaker(id, None), Err(DocError::ReadOnly));
    }

    #[test]
    fn a_delete_that_disagrees_with_the_log_is_refused() {
        let mut document = document_with(&["hello"]);
        let id = document.first_id().expect("segment");
        let error = document.delete_text(id, 0, "help").expect_err("mismatch");
        assert!(matches!(error, DocError::TextMismatch { .. }));
    }
}
