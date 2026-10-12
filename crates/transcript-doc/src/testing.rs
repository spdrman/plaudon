//! Deterministic fixtures.
//!
//! Public because the editor widget, the property tests and the scale
//! measurements all need the *same* documents: a fixture that differs between
//! crates makes a latency budget meaningless. Nothing here reads a clock, a
//! random number generator or the filesystem; identifiers are minted by
//! `uuid::now_v7` inside the domain types, and no assertion depends on them.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};

use crate::doc::{DocumentSource, SegmentSeed, TranscriptDocument};

/// An unaligned (text-only) document over the given segment texts.
#[must_use]
pub fn document_with(texts: &[&str]) -> TranscriptDocument {
    TranscriptDocument::new(DocumentSource {
        recording_id: RecordingId::new(),
        revision_id: TranscriptRevisionId::new(),
        alignment_origin: AlignmentOrigin::None,
        duration_ms: None,
        editable: true,
        segments: texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                SegmentSeed::text_only(u32::try_from(index).unwrap_or(u32::MAX), *text)
            })
            .collect(),
    })
}

/// The same document, opened read-only (a superseded revision).
#[must_use]
pub fn read_only_document(texts: &[&str]) -> TranscriptDocument {
    let mut source = DocumentSource {
        recording_id: RecordingId::new(),
        revision_id: TranscriptRevisionId::new(),
        alignment_origin: AlignmentOrigin::None,
        duration_ms: None,
        editable: false,
        segments: Vec::new(),
    };
    source.segments = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            SegmentSeed::text_only(u32::try_from(index).unwrap_or(u32::MAX), *text)
        })
        .collect();
    TranscriptDocument::new(source)
}

/// A natively aligned document over `(start_ms, end_ms, text)` triples.
#[must_use]
pub fn timed_document(segments: &[(u64, u64, &str)]) -> TranscriptDocument {
    TranscriptDocument::new(DocumentSource {
        recording_id: RecordingId::new(),
        revision_id: TranscriptRevisionId::new(),
        alignment_origin: AlignmentOrigin::Native,
        duration_ms: segments.last().map(|(_, end, _)| *end),
        editable: true,
        segments: segments
            .iter()
            .enumerate()
            .map(|(index, (start, end, text))| {
                SegmentSeed::timed(
                    u32::try_from(index).unwrap_or(u32::MAX),
                    *start,
                    *end,
                    *text,
                )
            })
            .collect(),
    })
}

/// An immutable machine revision over the given texts, for tests that need the
/// domain type itself (anchor resolution, child revisions).
#[must_use]
pub fn revision_with(texts: &[&str]) -> TranscriptRevision {
    TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: RecordingId::new(),
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"transcript-doc fixture"),
        engine: EngineId::new("fixture"),
        model: ModelIdentity::new(EngineId::new("fixture"), "fixture-1"),
        locale: None,
        alignment_origin: AlignmentOrigin::None,
        status: RevisionStatus::Complete,
        normalization_profile: "unspecified".to_owned(),
        chunking_strategy: "unspecified".to_owned(),
        duration_ms: None,
        created_at: Timestamp::from_millis(0),
        segments: texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                TranscriptSegment::text_only(u32::try_from(index).unwrap_or(u32::MAX), *text)
            })
            .collect(),
    })
    .expect("fixture revision is valid")
}

/// Words used by [`synthetic_transcript`]. A fixed vocabulary keeps segment
/// lengths and match counts identical on every machine and every run.
const VOCABULARY: &[&str] = &[
    "we", "will", "not", "pay", "the", "invoice", "before", "the", "audit", "closes", "and", "the",
    "team", "agreed", "to", "revisit", "the", "schedule", "next", "quarter",
];

/// A synthetic transcript of `segment_count` timed segments.
///
/// Shaped to the scale target in the parity matrix (T14) and issue #205: at
/// 30,000 segments it is three hours of audio (360 ms of media per segment)
/// carrying roughly ten words each, which is about 2 MB of text.
#[must_use]
pub fn synthetic_transcript(segment_count: usize) -> TranscriptDocument {
    let mut segments = Vec::with_capacity(segment_count);
    let mut cursor_ms = 0u64;
    for index in 0..segment_count {
        let word_count = 8 + index % 5;
        let mut text = String::with_capacity(word_count * 7);
        for word in 0..word_count {
            if word > 0 {
                text.push(' ');
            }
            text.push_str(VOCABULARY[(index + word) % VOCABULARY.len()]);
        }
        let duration = 360u64;
        segments.push(SegmentSeed::timed(
            u32::try_from(index).unwrap_or(u32::MAX),
            cursor_ms,
            cursor_ms + duration,
            text,
        ));
        cursor_ms += duration;
    }
    TranscriptDocument::new(DocumentSource {
        recording_id: RecordingId::new(),
        revision_id: TranscriptRevisionId::new(),
        alignment_origin: AlignmentOrigin::Native,
        duration_ms: Some(cursor_ms),
        editable: true,
        segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthetic_transcript_is_the_documented_scale() {
        let document = synthetic_transcript(30_000);
        assert_eq!(document.len(), 30_000);
        let duration = document.duration_ms().expect("duration");
        assert_eq!(duration, 3 * 3_600_000, "three hours of audio");
    }

    #[test]
    fn the_synthetic_transcript_is_identical_on_every_run() {
        let first = synthetic_transcript(64);
        let second = synthetic_transcript(64);
        let texts = |document: &TranscriptDocument| {
            document
                .segments()
                .iter()
                .map(|segment| segment.text().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&first), texts(&second));
    }
}
