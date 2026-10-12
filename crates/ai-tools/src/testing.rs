//! Deterministic fixtures shared by this crate's tests and by integration
//! tests in `tests/`.
//!
//! These are real domain values, not mocks: a fixture revision is built
//! through the same validating constructor the pipeline uses, so a test cannot
//! accidentally assert against a shape the application could never produce.

use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};

/// Sentences used by the fixture transcript, in order.
pub const FIXTURE_SENTENCES: &[&str] = &[
    "We will ship the migration on Friday.",
    "Priya owns the rollback plan and will confirm it by Thursday.",
    "The staging cluster still fails the load test above four thousand users.",
    "No decision was reached about the pricing change.",
    "Idempotency means the same request can be retried without duplicating work.",
];

/// Build a revision over [`FIXTURE_SENTENCES`] with the given alignment origin.
///
/// # Panics
///
/// Panics if the fixture itself violates a revision invariant, which would be
/// a bug in this helper rather than in a test.
#[must_use]
pub fn fixture_revision(origin: AlignmentOrigin) -> TranscriptRevision {
    let segments = FIXTURE_SENTENCES
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let seq = u32::try_from(index).expect("fixture is small");
            if origin.permits_times() {
                TranscriptSegment::timed(
                    seq,
                    u64::from(seq) * 10_000,
                    u64::from(seq) * 10_000 + 9_000,
                    *text,
                )
            } else {
                TranscriptSegment::text_only(seq, *text)
            }
        })
        .collect();
    revision_from_segments(origin, segments)
}

/// Build a single-segment revision containing exactly `text`.
///
/// # Panics
///
/// Panics if the resulting revision is invalid.
#[must_use]
pub fn revision_with_text(text: &str) -> TranscriptRevision {
    revision_from_segments(
        AlignmentOrigin::Native,
        vec![TranscriptSegment::timed(0, 0, 1_000, text)],
    )
}

/// Build a revision from explicit segments.
///
/// # Panics
///
/// Panics if the segments violate a revision invariant.
#[must_use]
pub fn revision_from_segments(
    origin: AlignmentOrigin,
    segments: Vec<TranscriptSegment>,
) -> TranscriptRevision {
    TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: RecordingId::new(),
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"fixture-media"),
        engine: EngineId::new("whisper.cpp"),
        model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
        locale: None,
        alignment_origin: origin,
        status: RevisionStatus::Complete,
        normalization_profile: "fixture".to_string(),
        chunking_strategy: "fixture".to_string(),
        duration_ms: Some(60_000),
        created_at: Timestamp::from_millis(1_700_000_000_000),
        segments,
    })
    .expect("fixture revision is valid")
}
