//! Transcript revisions and segments (spec 8.5, 8.6A).
//!
//! Three invariants are enforced at construction rather than by convention:
//!
//! 1. Segments are ordered, contiguously indexed and have non-inverted times.
//! 2. A revision whose alignment origin is [`AlignmentOrigin::None`] carries no
//!    times at all, so no consumer can surface an invented seek position.
//! 3. A revision is immutable. Corrections and reconciliations produce a child
//!    revision that references the untouched parent.

use serde::{Deserialize, Serialize};

use crate::ids::{RecordingId, SpeakerId, TranscriptRevisionId};
use crate::locale::LanguageTag;
use crate::media::Sha256Digest;
use crate::model::{EngineId, ModelIdentity};
use crate::time::Timestamp;
use crate::timerange::SourceTimeMs;

/// Where a segment's source-time offsets came from (spec 8.6A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentOrigin {
    /// Offsets were reported by the transcription engine itself.
    Native,
    /// Offsets were produced by a separate, explicitly labelled alignment pass.
    Derived,
    /// The engine produced text only. No offsets exist.
    None,
}

impl AlignmentOrigin {
    /// Whether segments of a revision with this origin may carry times.
    #[must_use]
    pub const fn permits_times(self) -> bool {
        matches!(self, Self::Native | Self::Derived)
    }

    /// Whether a seek affordance may be offered for this origin.
    #[must_use]
    pub const fn permits_seek(self) -> bool {
        self.permits_times()
    }
}

/// The numeric scale a confidence value is expressed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceScale {
    /// A probability in `[0, 1]`.
    Probability,
    /// An average log-probability, which is at most zero.
    AverageLogProbability,
    /// A vendor-defined score with no published calibration.
    VendorDefined,
}

/// An engine-scoped confidence value.
///
/// Spec 8.5 forbids comparing confidence across vendors without calibration, so
/// this type deliberately implements neither `PartialOrd` nor `Ord`. The only
/// comparison available is [`Confidence::compare_within_engine`], which refuses
/// to order values produced by different engines or scales.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Confidence {
    /// Engine that produced the value.
    pub engine: EngineId,
    /// Scale the value is expressed on.
    pub scale: ConfidenceScale,
    /// The raw value, verbatim from the engine.
    pub value: f64,
}

impl Confidence {
    /// Construct a confidence value.
    #[must_use]
    pub fn new(engine: EngineId, scale: ConfidenceScale, value: f64) -> Self {
        Self { engine, scale, value }
    }

    /// Order two values only when they share an engine and a scale.
    #[must_use]
    pub fn compare_within_engine(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.engine != other.engine || self.scale != other.scale {
            return None;
        }
        self.value.partial_cmp(&other.value)
    }
}

/// Word-level alignment inside a segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordAlignment {
    /// The word as normalized by the engine.
    pub text: String,
    /// Start offset of the word.
    pub start: SourceTimeMs,
    /// End offset of the word.
    pub end: SourceTimeMs,
}

/// One ordered transcript segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    /// Zero-based position of this segment in the revision.
    pub seq_index: u32,
    /// Start offset, when the revision has an alignment origin that permits times.
    pub start: Option<SourceTimeMs>,
    /// End offset, when the revision has an alignment origin that permits times.
    pub end: Option<SourceTimeMs>,
    /// Normalized segment text.
    pub text: String,
    /// Engine-scoped confidence, when the engine reported one.
    pub confidence: Option<Confidence>,
    /// Speaker attribution, when diarization ran.
    pub speaker_id: Option<SpeakerId>,
    /// Word alignments, when the engine reported them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<WordAlignment>,
}

impl TranscriptSegment {
    /// Construct a text-only segment.
    #[must_use]
    pub fn text_only(seq_index: u32, text: impl Into<String>) -> Self {
        Self {
            seq_index,
            start: None,
            end: None,
            text: text.into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
        }
    }

    /// Construct a timed segment.
    #[must_use]
    pub fn timed(seq_index: u32, start_ms: u64, end_ms: u64, text: impl Into<String>) -> Self {
        Self {
            seq_index,
            start: Some(SourceTimeMs::new(start_ms)),
            end: Some(SourceTimeMs::new(end_ms)),
            text: text.into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
        }
    }
}

/// How a revision came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionOrigin {
    /// Produced directly by a transcription engine.
    Machine,
    /// Produced by a user editing a previous revision.
    HumanEdit,
    /// Produced by a labelled alignment pass over a text-only revision.
    DerivedAlignment,
    /// Produced by overlap-aware reconciliation of a previous revision.
    OverlapReconciliation,
}

/// Quality/lifecycle status of a revision (spec 13.2 `transcript_revisions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionStatus {
    /// Complete and usable.
    Complete,
    /// Produced, but flagged for user review.
    NeedsReview,
    /// Partial output from an interrupted run; never presented as final.
    Partial,
}

/// Reason a revision could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptError {
    /// Segment indices are not a contiguous ascending run starting at zero.
    #[error("segment at position {position} declares index {seq_index}, expected {expected}")]
    SegmentOrder {
        /// Position of the offending segment in the supplied slice.
        position: usize,
        /// Index the segment declared.
        seq_index: u32,
        /// Index the segment was required to declare.
        expected: u32,
    },
    /// A segment's end precedes its start.
    #[error("segment {seq_index} is inverted: end {end_ms} precedes start {start_ms}")]
    InvertedRange {
        /// Index of the offending segment.
        seq_index: u32,
        /// Declared start.
        start_ms: u64,
        /// Declared end.
        end_ms: u64,
    },
    /// Segment start offsets move backwards.
    #[error("segment {seq_index} starts at {start_ms}, before the previous segment start {previous_start_ms}")]
    NonMonotonicStart {
        /// Index of the offending segment.
        seq_index: u32,
        /// Declared start.
        start_ms: u64,
        /// Start of the preceding segment.
        previous_start_ms: u64,
    },
    /// A segment declares only one of its two offsets.
    #[error("segment {seq_index} declares only one of start/end")]
    PartialRange {
        /// Index of the offending segment.
        seq_index: u32,
    },
    /// A revision with alignment origin `none` carried times.
    #[error("segment {seq_index} carries a time offset but the revision alignment origin is none")]
    TimesWithoutAlignment {
        /// Index of the offending segment.
        seq_index: u32,
    },
    /// A revision with a timed alignment origin had an untimed segment.
    #[error("segment {seq_index} has no time offset but the revision alignment origin is {origin:?}")]
    MissingTimes {
        /// Index of the offending segment.
        seq_index: u32,
        /// Alignment origin declared by the revision.
        origin: AlignmentOrigin,
    },
    /// A word alignment fell outside its segment.
    #[error("word alignment in segment {seq_index} lies outside the segment range")]
    WordOutsideSegment {
        /// Index of the offending segment.
        seq_index: u32,
    },
}

/// An immutable transcript revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptRevision {
    /// Revision identity.
    pub id: TranscriptRevisionId,
    /// Recording the transcript belongs to.
    pub recording_id: RecordingId,
    /// Revision this one was derived from, if any.
    pub parent_id: Option<TranscriptRevisionId>,
    /// How this revision came to exist.
    pub origin: RevisionOrigin,
    /// Digest of the exact media bytes that were transcribed.
    pub input_sha256: Sha256Digest,
    /// Engine that produced the revision, or that produced its ancestor.
    pub engine: EngineId,
    /// Model that produced the revision.
    pub model: ModelIdentity,
    /// Detected or configured locale.
    pub locale: Option<LanguageTag>,
    /// Declared alignment origin for every segment in this revision.
    pub alignment_origin: AlignmentOrigin,
    /// Lifecycle status.
    pub status: RevisionStatus,
    /// Text normalization profile applied.
    pub normalization_profile: String,
    /// Chunking strategy used to produce the revision.
    pub chunking_strategy: String,
    /// Media duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Creation instant.
    pub created_at: Timestamp,
    segments: Vec<TranscriptSegment>,
}

/// Everything required to build a revision, before validation.
#[derive(Debug, Clone)]
pub struct TranscriptRevisionDraft {
    /// Recording the transcript belongs to.
    pub recording_id: RecordingId,
    /// Parent revision, if this is a correction or reconciliation.
    pub parent_id: Option<TranscriptRevisionId>,
    /// How the revision came to exist.
    pub origin: RevisionOrigin,
    /// Digest of the exact transcribed media bytes.
    pub input_sha256: Sha256Digest,
    /// Producing engine.
    pub engine: EngineId,
    /// Producing model.
    pub model: ModelIdentity,
    /// Detected or configured locale.
    pub locale: Option<LanguageTag>,
    /// Declared alignment origin.
    pub alignment_origin: AlignmentOrigin,
    /// Lifecycle status.
    pub status: RevisionStatus,
    /// Normalization profile applied.
    pub normalization_profile: String,
    /// Chunking strategy used.
    pub chunking_strategy: String,
    /// Media duration, when known.
    pub duration_ms: Option<u64>,
    /// Creation instant.
    pub created_at: Timestamp,
    /// Ordered segments.
    pub segments: Vec<TranscriptSegment>,
}

impl TranscriptRevision {
    /// Validate a draft and freeze it into an immutable revision.
    pub fn try_new(draft: TranscriptRevisionDraft) -> Result<Self, TranscriptError> {
        validate_segments(draft.alignment_origin, &draft.segments)?;
        Ok(Self {
            id: TranscriptRevisionId::new(),
            recording_id: draft.recording_id,
            parent_id: draft.parent_id,
            origin: draft.origin,
            input_sha256: draft.input_sha256,
            engine: draft.engine,
            model: draft.model,
            locale: draft.locale,
            alignment_origin: draft.alignment_origin,
            status: draft.status,
            normalization_profile: draft.normalization_profile,
            chunking_strategy: draft.chunking_strategy,
            duration_ms: draft.duration_ms,
            created_at: draft.created_at,
            segments: draft.segments,
        })
    }

    /// The ordered segments. There is no mutable accessor: a revision never changes.
    #[must_use]
    pub fn segments(&self) -> &[TranscriptSegment] {
        &self.segments
    }

    /// Whether a seek position may be offered for this revision.
    #[must_use]
    pub const fn permits_seek(&self) -> bool {
        self.alignment_origin.permits_seek()
    }

    /// Segment start offsets, or `None` when the revision has no alignment.
    ///
    /// Consumers must call this rather than reading `segment.start` directly,
    /// so a text-only revision can never leak a synthesized timeline.
    #[must_use]
    pub fn seek_offsets(&self) -> Option<Vec<SourceTimeMs>> {
        if !self.permits_seek() {
            return None;
        }
        self.segments.iter().map(|segment| segment.start).collect()
    }

    /// Whether this revision contains human edits.
    #[must_use]
    pub const fn is_human_edited(&self) -> bool {
        matches!(self.origin, RevisionOrigin::HumanEdit)
    }

    /// Produce a child revision carrying edited segments.
    ///
    /// The receiver is untouched and remains retrievable.
    pub fn edited(
        &self,
        segments: Vec<TranscriptSegment>,
        created_at: Timestamp,
    ) -> Result<Self, TranscriptError> {
        self.derive(RevisionOrigin::HumanEdit, segments, self.alignment_origin, created_at)
    }

    /// Produce a child revision with overlapping segments reconciled.
    ///
    /// Spec 8.6A: overlap correction preserves the unmodified provider result
    /// and records the correction as a separate revision.
    pub fn reconciled_overlaps(&self, created_at: Timestamp) -> Result<Self, TranscriptError> {
        let reconciled = reconcile_overlaps(&self.segments);
        self.derive(
            RevisionOrigin::OverlapReconciliation,
            reconciled,
            self.alignment_origin,
            created_at,
        )
    }

    /// Produce a child revision that attaches derived alignment to a text-only parent.
    pub fn with_derived_alignment(
        &self,
        segments: Vec<TranscriptSegment>,
        created_at: Timestamp,
    ) -> Result<Self, TranscriptError> {
        self.derive(RevisionOrigin::DerivedAlignment, segments, AlignmentOrigin::Derived, created_at)
    }

    fn derive(
        &self,
        origin: RevisionOrigin,
        segments: Vec<TranscriptSegment>,
        alignment_origin: AlignmentOrigin,
        created_at: Timestamp,
    ) -> Result<Self, TranscriptError> {
        Self::try_new(TranscriptRevisionDraft {
            recording_id: self.recording_id,
            parent_id: Some(self.id),
            origin,
            input_sha256: self.input_sha256,
            engine: self.engine.clone(),
            model: self.model.clone(),
            locale: self.locale.clone(),
            alignment_origin,
            status: self.status,
            normalization_profile: self.normalization_profile.clone(),
            chunking_strategy: self.chunking_strategy.clone(),
            duration_ms: self.duration_ms,
            created_at,
            segments,
        })
    }
}

fn validate_segments(
    origin: AlignmentOrigin,
    segments: &[TranscriptSegment],
) -> Result<(), TranscriptError> {
    let mut previous_start: Option<u64> = None;
    for (position, segment) in segments.iter().enumerate() {
        let expected = u32::try_from(position).unwrap_or(u32::MAX);
        if segment.seq_index != expected {
            return Err(TranscriptError::SegmentOrder {
                position,
                seq_index: segment.seq_index,
                expected,
            });
        }
        match (segment.start, segment.end) {
            (None, None) => {
                if origin.permits_times() {
                    return Err(TranscriptError::MissingTimes {
                        seq_index: segment.seq_index,
                        origin,
                    });
                }
                if !segment.words.is_empty() {
                    return Err(TranscriptError::WordOutsideSegment {
                        seq_index: segment.seq_index,
                    });
                }
            }
            (Some(start), Some(end)) => {
                if !origin.permits_times() {
                    return Err(TranscriptError::TimesWithoutAlignment {
                        seq_index: segment.seq_index,
                    });
                }
                if end.get() < start.get() {
                    return Err(TranscriptError::InvertedRange {
                        seq_index: segment.seq_index,
                        start_ms: start.get(),
                        end_ms: end.get(),
                    });
                }
                if let Some(previous) = previous_start {
                    if start.get() < previous {
                        return Err(TranscriptError::NonMonotonicStart {
                            seq_index: segment.seq_index,
                            start_ms: start.get(),
                            previous_start_ms: previous,
                        });
                    }
                }
                previous_start = Some(start.get());
                for word in &segment.words {
                    if word.start.get() < start.get()
                        || word.end.get() > end.get()
                        || word.end.get() < word.start.get()
                    {
                        return Err(TranscriptError::WordOutsideSegment {
                            seq_index: segment.seq_index,
                        });
                    }
                }
            }
            _ => return Err(TranscriptError::PartialRange { seq_index: segment.seq_index }),
        }
    }
    Ok(())
}

/// Trim overlapping segment boundaries without reordering or dropping text.
///
/// A later segment that starts before its predecessor ends has its start moved
/// forward to the predecessor's end; a segment fully contained in its
/// predecessor collapses to a zero-length range at that boundary. Text is never
/// altered, so the reconciliation is auditable against the parent revision.
fn reconcile_overlaps(segments: &[TranscriptSegment]) -> Vec<TranscriptSegment> {
    let mut out: Vec<TranscriptSegment> = Vec::with_capacity(segments.len());
    let mut previous_end: Option<u64> = None;
    for segment in segments {
        let mut next = segment.clone();
        if let (Some(start), Some(end)) = (next.start, next.end) {
            let adjusted_start = previous_end.map_or(start.get(), |pe| start.get().max(pe));
            let adjusted_end = end.get().max(adjusted_start);
            next.start = Some(SourceTimeMs::new(adjusted_start));
            next.end = Some(SourceTimeMs::new(adjusted_end));
            next.words.retain(|w| w.start.get() >= adjusted_start && w.end.get() <= adjusted_end);
            previous_end = Some(adjusted_end);
        }
        out.push(next);
    }
    out
}

/// An append-only chain of revisions for one recording.
///
/// Reprocessing with a different engine appends a sibling revision; it never
/// replaces a human-edited revision (spec 8.6).
#[derive(Debug, Clone, Default)]
pub struct TranscriptLineage {
    revisions: Vec<TranscriptRevision>,
}

impl TranscriptLineage {
    /// Create an empty lineage.
    #[must_use]
    pub fn new() -> Self {
        Self { revisions: Vec::new() }
    }

    /// Append a revision. Existing revisions are never modified or removed.
    pub fn push(&mut self, revision: TranscriptRevision) {
        self.revisions.push(revision);
    }

    /// Every revision, in insertion order.
    #[must_use]
    pub fn revisions(&self) -> &[TranscriptRevision] {
        &self.revisions
    }

    /// Look up a revision by identity.
    #[must_use]
    pub fn get(&self, id: TranscriptRevisionId) -> Option<&TranscriptRevision> {
        self.revisions.iter().find(|revision| revision.id == id)
    }

    /// The most recent human-edited revision, if any.
    #[must_use]
    pub fn latest_human_edit(&self) -> Option<&TranscriptRevision> {
        self.revisions.iter().rev().find(|revision| revision.is_human_edited())
    }

    /// Whether a summary produced from `input_revision` is stale.
    ///
    /// A summary is stale once a later revision of the same recording exists.
    #[must_use]
    pub fn is_stale(&self, input_revision: TranscriptRevisionId) -> bool {
        match self.revisions.iter().position(|revision| revision.id == input_revision) {
            Some(position) => position + 1 < self.revisions.len(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(origin: AlignmentOrigin, segments: Vec<TranscriptSegment>) -> TranscriptRevisionDraft {
        TranscriptRevisionDraft {
            recording_id: RecordingId::new(),
            parent_id: None,
            origin: RevisionOrigin::Machine,
            input_sha256: Sha256Digest::of(b"audio"),
            engine: EngineId::new("whisper-cpp"),
            model: ModelIdentity::new(EngineId::new("whisper-cpp"), "base.en"),
            locale: None,
            alignment_origin: origin,
            status: RevisionStatus::Complete,
            normalization_profile: "nfc-basic".into(),
            chunking_strategy: "single-pass".into(),
            duration_ms: Some(1_000),
            created_at: Timestamp::EPOCH,
            segments,
        }
    }

    #[test]
    fn a_text_only_revision_exposes_no_seek_offsets() {
        let revision = TranscriptRevision::try_new(draft(
            AlignmentOrigin::None,
            vec![TranscriptSegment::text_only(0, "hello"), TranscriptSegment::text_only(1, "world")],
        ))
        .unwrap();
        assert!(!revision.permits_seek());
        assert_eq!(revision.seek_offsets(), None);
        assert!(revision.segments().iter().all(|s| s.start.is_none() && s.end.is_none()));
    }

    #[test]
    fn times_on_an_unaligned_revision_are_rejected() {
        let err = TranscriptRevision::try_new(draft(
            AlignmentOrigin::None,
            vec![TranscriptSegment::timed(0, 0, 10, "hello")],
        ))
        .unwrap_err();
        assert_eq!(err, TranscriptError::TimesWithoutAlignment { seq_index: 0 });
    }

    #[test]
    fn missing_times_on_an_aligned_revision_are_rejected() {
        let err = TranscriptRevision::try_new(draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegment::text_only(0, "hello")],
        ))
        .unwrap_err();
        assert_eq!(
            err,
            TranscriptError::MissingTimes { seq_index: 0, origin: AlignmentOrigin::Native }
        );
    }

    #[test]
    fn inverted_and_non_monotonic_ranges_are_rejected() {
        let inverted = TranscriptRevision::try_new(draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegment::timed(0, 10, 5, "x")],
        ))
        .unwrap_err();
        assert_eq!(
            inverted,
            TranscriptError::InvertedRange { seq_index: 0, start_ms: 10, end_ms: 5 }
        );

        let backwards = TranscriptRevision::try_new(draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegment::timed(0, 100, 200, "a"), TranscriptSegment::timed(1, 50, 300, "b")],
        ))
        .unwrap_err();
        assert_eq!(
            backwards,
            TranscriptError::NonMonotonicStart { seq_index: 1, start_ms: 50, previous_start_ms: 100 }
        );
    }

    #[test]
    fn out_of_order_segment_indices_are_rejected() {
        let err = TranscriptRevision::try_new(draft(
            AlignmentOrigin::None,
            vec![TranscriptSegment::text_only(0, "a"), TranscriptSegment::text_only(5, "b")],
        ))
        .unwrap_err();
        assert_eq!(err, TranscriptError::SegmentOrder { position: 1, seq_index: 5, expected: 1 });
    }

    #[test]
    fn a_half_specified_range_is_rejected() {
        let mut segment = TranscriptSegment::timed(0, 0, 10, "x");
        segment.end = None;
        let err = TranscriptRevision::try_new(draft(AlignmentOrigin::Native, vec![segment]))
            .unwrap_err();
        assert_eq!(err, TranscriptError::PartialRange { seq_index: 0 });
    }

    #[test]
    fn word_alignments_must_lie_inside_their_segment() {
        let mut segment = TranscriptSegment::timed(0, 100, 200, "x");
        segment.words = vec![WordAlignment {
            text: "x".into(),
            start: SourceTimeMs::new(50),
            end: SourceTimeMs::new(150),
        }];
        let err = TranscriptRevision::try_new(draft(AlignmentOrigin::Native, vec![segment]))
            .unwrap_err();
        assert_eq!(err, TranscriptError::WordOutsideSegment { seq_index: 0 });
    }

    #[test]
    fn editing_produces_a_child_and_leaves_the_parent_intact() {
        let parent = TranscriptRevision::try_new(draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegment::timed(0, 0, 10, "teh meeting")],
        ))
        .unwrap();
        let child = parent
            .edited(vec![TranscriptSegment::timed(0, 0, 10, "the meeting")], Timestamp::from_millis(5))
            .unwrap();

        assert_eq!(child.parent_id, Some(parent.id));
        assert_eq!(child.origin, RevisionOrigin::HumanEdit);
        assert_eq!(parent.segments()[0].text, "teh meeting");
        assert_eq!(child.segments()[0].text, "the meeting");
        assert_ne!(child.id, parent.id);
        assert_eq!(child.input_sha256, parent.input_sha256);
    }

    #[test]
    fn overlap_reconciliation_is_a_separate_revision_preserving_the_provider_result() {
        let provider = TranscriptRevision::try_new(draft(
            AlignmentOrigin::Native,
            vec![
                TranscriptSegment::timed(0, 0, 1_200, "first chunk"),
                TranscriptSegment::timed(1, 1_000, 2_000, "second chunk"),
            ],
        ))
        .unwrap();
        let reconciled = provider.reconciled_overlaps(Timestamp::from_millis(9)).unwrap();

        assert_eq!(provider.segments()[1].start, Some(SourceTimeMs::new(1_000)));
        assert_eq!(reconciled.segments()[1].start, Some(SourceTimeMs::new(1_200)));
        assert_eq!(reconciled.origin, RevisionOrigin::OverlapReconciliation);
        assert_eq!(reconciled.parent_id, Some(provider.id));
        assert_eq!(reconciled.segments()[1].text, "second chunk");
    }

    #[test]
    fn derived_alignment_is_labelled_distinctly_from_native() {
        let text_only = TranscriptRevision::try_new(draft(
            AlignmentOrigin::None,
            vec![TranscriptSegment::text_only(0, "hello")],
        ))
        .unwrap();
        let aligned = text_only
            .with_derived_alignment(
                vec![TranscriptSegment::timed(0, 0, 500, "hello")],
                Timestamp::from_millis(1),
            )
            .unwrap();
        assert_eq!(aligned.alignment_origin, AlignmentOrigin::Derived);
        assert!(aligned.permits_seek());
        assert_eq!(text_only.alignment_origin, AlignmentOrigin::None);
    }

    #[test]
    fn confidence_cannot_be_compared_across_engines_or_scales() {
        let whisper = Confidence::new(EngineId::new("whisper-cpp"), ConfidenceScale::AverageLogProbability, -0.1);
        let apple = Confidence::new(EngineId::new("apple-speech"), ConfidenceScale::Probability, 0.9);
        assert_eq!(whisper.compare_within_engine(&apple), None);

        let worse = Confidence::new(EngineId::new("whisper-cpp"), ConfidenceScale::AverageLogProbability, -0.8);
        assert_eq!(whisper.compare_within_engine(&worse), Some(std::cmp::Ordering::Greater));

        let other_scale = Confidence::new(EngineId::new("whisper-cpp"), ConfidenceScale::Probability, 0.9);
        assert_eq!(whisper.compare_within_engine(&other_scale), None);
    }

    #[test]
    fn reprocessing_never_removes_a_human_edited_revision() {
        let machine = TranscriptRevision::try_new(draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegment::timed(0, 0, 10, "machine")],
        ))
        .unwrap();
        let human = machine
            .edited(vec![TranscriptSegment::timed(0, 0, 10, "human")], Timestamp::from_millis(2))
            .unwrap();
        let human_id = human.id;

        let mut lineage = TranscriptLineage::new();
        lineage.push(machine);
        lineage.push(human);

        let reprocessed = TranscriptRevision::try_new(draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegment::timed(0, 0, 10, "reprocessed")],
        ))
        .unwrap();
        let reprocessed_id = reprocessed.id;
        lineage.push(reprocessed);

        assert_eq!(lineage.revisions().len(), 3);
        assert_eq!(lineage.get(human_id).unwrap().segments()[0].text, "human");
        assert_eq!(lineage.latest_human_edit().unwrap().id, human_id);
        assert!(lineage.is_stale(human_id), "a summary built on the edited revision is now stale");
        assert!(!lineage.is_stale(reprocessed_id));
    }
}
