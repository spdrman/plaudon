//! Overlap-aware reconciliation of per-chunk engine results (§8.2 — F1.3).
//!
//! > We will merge words/segments using overlap-aware reconciliation rather
//! > than naive concatenation.
//!
//! Naive concatenation produces duplicated words at every boundary, because
//! the shared audio was transcribed twice. This module matches the duplicates
//! and keeps one copy, with three rules that come straight from §8.2/§8.6A:
//!
//! * A chunk that is missing is an **error**, not a gap to paper over.
//! * An ambiguous overlap keeps **both** candidate texts and records the
//!   uncertainty; it never drops content to make the timeline tidy.
//! * Nothing here may strengthen a claim: the merged draft is checked with
//!   [`plaudon_ai_contracts::assert_no_claim_inflation`] before it is
//!   returned.

use std::collections::BTreeMap;

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::transcript::{AnnotationKind, SourceTime, TranscriptAnnotation};
use plaudon_ai_contracts::{assert_no_claim_inflation, AlignmentOrigin};

use crate::chunk::{ChunkPlan, ChunkSpec};

/// One chunk's engine result, positioned on the absolute source timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkTranscript {
    chunk: ChunkSpec,
    draft: TranscriptDraft,
}

impl ChunkTranscript {
    /// Wrap a draft whose times are already absolute source offsets.
    ///
    /// This is what an adapter produces when it honours
    /// `SttInput::source_sample_offset`.
    #[must_use]
    pub const fn absolute(chunk: ChunkSpec, draft: TranscriptDraft) -> Self {
        Self { chunk, draft }
    }

    /// Wrap a draft whose times are relative to the start of its own chunk,
    /// shifting every time by the chunk's absolute offset.
    #[must_use]
    pub fn relative(chunk: ChunkSpec, mut draft: TranscriptDraft) -> Self {
        draft.segments = draft
            .segments
            .into_iter()
            .map(|segment| segment.shifted_by_samples(chunk.start_sample))
            .collect();
        for annotation in &mut draft.annotations {
            annotation.start = annotation
                .start
                .map(|time| time.shifted_by_samples(chunk.start_sample));
            annotation.end = annotation
                .end
                .map(|time| time.shifted_by_samples(chunk.start_sample));
        }
        Self { chunk, draft }
    }

    /// The planned chunk this result belongs to.
    #[must_use]
    pub const fn chunk(&self) -> &ChunkSpec {
        &self.chunk
    }

    /// The engine result.
    #[must_use]
    pub const fn draft(&self) -> &TranscriptDraft {
        &self.draft
    }
}

/// Why a set of chunk results cannot be merged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReconcileError {
    /// No chunk results were supplied.
    #[error("no chunk results were supplied")]
    Empty,
    /// A planned chunk has no result. Merging would silently lose audio.
    #[error("chunk {index} of {expected} has no transcript; refusing to merge a gap")]
    MissingChunk {
        /// Index of the chunk with no result.
        index: usize,
        /// Number of chunks the plan declared.
        expected: usize,
    },
    /// Two results claim the same chunk index.
    #[error("chunk {index} was supplied more than once")]
    DuplicateChunk {
        /// The repeated index.
        index: usize,
    },
    /// A result claims a chunk the plan does not contain.
    #[error("chunk {index} is not part of the plan")]
    UnplannedChunk {
        /// The unexpected index.
        index: usize,
    },
    /// Chunks disagree about whether they have a timeline at all.
    ///
    /// Fail closed: silently downgrading to text-only would discard real
    /// timestamps, and silently upgrading would invent them (§8.6A).
    #[error("chunks disagree on alignment origin: {first} vs {second}")]
    MixedAlignmentOrigins {
        /// Origin of the first chunk.
        first: &'static str,
        /// The conflicting origin.
        second: &'static str,
    },
    /// Chunks disagree about which engine or model produced them.
    #[error("chunk {index} was produced by a different engine or model than chunk 0")]
    MixedEngines {
        /// Index of the offending chunk.
        index: usize,
    },
    /// The merged draft failed the truthfulness contract.
    #[error("merged draft strengthened a claim: {0}")]
    ClaimInflation(String),
}

/// Maximum number of boundary segments compared when deduplicating.
///
/// A chunk overlap is seconds long, so a handful of segments on each side is
/// always enough; the cap keeps the comparison linear in practice.
const BOUNDARY_WINDOW_SEGMENTS: usize = 32;

/// Merge per-chunk results into one ordered timeline.
pub fn reconcile_chunks(
    plan: &ChunkPlan,
    results: Vec<ChunkTranscript>,
) -> Result<TranscriptDraft, ReconcileError> {
    if results.is_empty() {
        return Err(ReconcileError::Empty);
    }

    let mut by_index: BTreeMap<usize, ChunkTranscript> = BTreeMap::new();
    for result in results {
        let index = result.chunk.index;
        if index >= plan.chunks.len() {
            return Err(ReconcileError::UnplannedChunk { index });
        }
        if by_index.insert(index, result).is_some() {
            return Err(ReconcileError::DuplicateChunk { index });
        }
    }
    for index in 0..plan.chunks.len() {
        if !by_index.contains_key(&index) {
            return Err(ReconcileError::MissingChunk {
                index,
                expected: plan.chunks.len(),
            });
        }
    }

    let ordered: Vec<ChunkTranscript> = by_index.into_values().collect();
    let first = &ordered[0].draft;
    for (index, result) in ordered.iter().enumerate().skip(1) {
        if result.draft.alignment_origin != first.alignment_origin {
            return Err(ReconcileError::MixedAlignmentOrigins {
                first: origin_label(first.alignment_origin),
                second: origin_label(result.draft.alignment_origin),
            });
        }
        if result.draft.engine_id != first.engine_id || result.draft.model != first.model {
            return Err(ReconcileError::MixedEngines { index });
        }
    }

    // Single chunk: nothing to reconcile. Return it verbatim, only recording
    // the plan's strategy.
    if ordered.len() == 1 {
        let mut merged = ordered
            .into_iter()
            .next()
            .unwrap_or_else(|| unreachable!())
            .draft;
        merged.chunking_strategy = plan.strategy.clone();
        reindex(&mut merged);
        return Ok(merged);
    }

    let mut merged = first.clone();
    merged.segments = Vec::new();
    merged.annotations = Vec::new();
    merged.chunking_strategy = plan.strategy.clone();

    if first.alignment_origin == AlignmentOrigin::None {
        merge_text_only(&ordered, &mut merged);
    } else {
        merge_timed(&ordered, &mut merged);
    }

    reindex(&mut merged);

    // The merge may only weaken claims. Compare against the strongest claim
    // any input made.
    let strongest = ordered
        .iter()
        .map(|result| result.draft.clone())
        .reduce(|mut acc, next| {
            acc.segments.extend(next.segments);
            acc
        })
        .unwrap_or_else(|| merged.clone());
    assert_no_claim_inflation(&strongest, &merged)
        .map_err(|error| ReconcileError::ClaimInflation(error.to_string()))?;

    Ok(merged)
}

const fn origin_label(origin: AlignmentOrigin) -> &'static str {
    match origin {
        AlignmentOrigin::Native => "native",
        AlignmentOrigin::Derived => "derived",
        AlignmentOrigin::None => "none",
    }
}

fn reindex(draft: &mut TranscriptDraft) {
    for (index, segment) in draft.segments.iter_mut().enumerate() {
        segment.seq_index = u32::try_from(index).unwrap_or(u32::MAX);
    }
}

/// Normalised comparison key: case-folded, punctuation-stripped words.
fn key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text-only merge: no timeline exists, so duplicates can only be found by
/// matching the boundary text itself.
fn merge_text_only(ordered: &[ChunkTranscript], merged: &mut TranscriptDraft) {
    for (position, result) in ordered.iter().enumerate() {
        let incoming = &result.draft.segments;
        let skip = if position == 0 {
            0
        } else {
            duplicate_prefix_len(&merged.segments, incoming)
        };
        for segment in incoming.iter().skip(skip) {
            merged.segments.push(segment.clone());
        }
    }
}

/// How many leading segments of `incoming` repeat the tail of `accumulated`.
fn duplicate_prefix_len(
    accumulated: &[TranscriptSegmentDraft],
    incoming: &[TranscriptSegmentDraft],
) -> usize {
    let window = BOUNDARY_WINDOW_SEGMENTS
        .min(accumulated.len())
        .min(incoming.len());
    // Longest `n` such that the last `n` accumulated segments equal the first
    // `n` incoming segments.
    for n in (1..=window).rev() {
        let tail = &accumulated[accumulated.len() - n..];
        let head = &incoming[..n];
        if tail
            .iter()
            .zip(head.iter())
            .all(|(a, b)| key(&a.text) == key(&b.text))
        {
            return n;
        }
    }
    0
}

/// Timed merge: use the planned overlap regions to decide which copy of a
/// duplicated segment survives.
fn merge_timed(ordered: &[ChunkTranscript], merged: &mut TranscriptDraft) {
    // Kept segments, tagged with the chunk they came from.
    let mut kept: Vec<(usize, TranscriptSegmentDraft)> = Vec::new();
    let mut ambiguous: Vec<usize> = Vec::new();

    for (position, result) in ordered.iter().enumerate() {
        let chunk = &result.chunk;
        let previous = if position == 0 {
            None
        } else {
            Some(&ordered[position - 1])
        };

        for segment in &result.draft.segments {
            let Some(previous) = previous else {
                kept.push((chunk.index, segment.clone()));
                continue;
            };
            let overlap_start = chunk.start_sample;
            let overlap_end = previous.chunk.end_sample;
            if overlap_end <= overlap_start {
                kept.push((chunk.index, segment.clone()));
                continue;
            }

            let Some(center) = segment_center(segment) else {
                kept.push((chunk.index, segment.clone()));
                continue;
            };
            if center >= overlap_end {
                // Past the shared region entirely: unique content.
                kept.push((chunk.index, segment.clone()));
                continue;
            }

            // Inside the shared region. Did the previous chunk already say it?
            let duplicate_of = kept.iter().position(|(chunk_index, candidate)| {
                *chunk_index == previous.chunk.index
                    && key(&candidate.text) == key(&segment.text)
                    && segment_center(candidate).is_some_and(|other| {
                        other.abs_diff(center) <= (overlap_end - overlap_start)
                    })
            });

            let midpoint = overlap_start + (overlap_end - overlap_start) / 2;
            match duplicate_of {
                Some(index) if center < midpoint => {
                    // Earlier chunk had more leading context: keep its copy.
                    let _ = index;
                }
                Some(index) => {
                    // Later chunk had more trailing context: replace.
                    kept[index] = (chunk.index, segment.clone());
                }
                None => {
                    // Content in the shared region that the previous chunk did
                    // not produce, or produced differently. Keep it and record
                    // the uncertainty rather than discarding either reading.
                    kept.push((chunk.index, segment.clone()));
                    ambiguous.push(kept.len() - 1);
                }
            }
        }
    }

    // Record ambiguity against the surviving positions before sorting.
    let ambiguous_keys: Vec<(u64, String)> = ambiguous
        .iter()
        .filter_map(|index| kept.get(*index))
        .filter_map(|(_, segment)| {
            segment_center(segment).map(|center| (center, key(&segment.text)))
        })
        .collect();

    kept.sort_by_key(|(chunk_index, segment)| {
        (
            segment.start.map_or(u64::MAX, |time| time.sample_offset),
            segment.end.map_or(u64::MAX, |time| time.sample_offset),
            *chunk_index,
        )
    });

    let mut segments: Vec<TranscriptSegmentDraft> =
        kept.into_iter().map(|(_, segment)| segment).collect();

    // Enforce a monotonic, non-overlapping timeline. Overlaps that survived
    // deduplication are real disagreements: we clamp the later start, widen
    // its uncertainty by exactly the amount we moved it, and flag it.
    let mut clamped: Vec<usize> = Vec::new();
    let mut previous_end: Option<u64> = None;
    for (index, segment) in segments.iter_mut().enumerate() {
        let (Some(start), Some(end)) = (segment.start, segment.end) else {
            continue;
        };
        if let Some(previous_end) = previous_end {
            if start.sample_offset < previous_end {
                let shift_samples = previous_end - start.sample_offset;
                let shift_ms = u32::try_from(
                    (shift_samples.min(u64::from(u32::MAX)) * 1000)
                        / u64::from(start.sample_rate_hz.max(1)),
                )
                .unwrap_or(u32::MAX);
                let mut moved = start;
                moved.sample_offset = previous_end;
                segment.start = Some(moved.widened_to(shift_ms));
                if end.sample_offset < previous_end {
                    let mut moved_end = end;
                    moved_end.sample_offset = previous_end;
                    segment.end = Some(moved_end.widened_to(shift_ms));
                }
                clamped.push(index);
            }
        }
        previous_end = segment.end.map(|time| time.sample_offset);
    }

    for index in clamped {
        if let Some(segment) = segments.get(index) {
            merged.annotations.push(TranscriptAnnotation {
                kind: AnnotationKind::AmbiguousOverlap,
                reason: "segment start was clamped to the end of the preceding segment after \
                         overlap reconciliation; both readings were preserved"
                    .to_string(),
                segment_index: index,
                start: segment.start,
                end: segment.end,
            });
        }
    }

    for (center, text_key) in ambiguous_keys {
        if let Some(index) = segments.iter().position(|segment| {
            key(&segment.text) == text_key
                && segment_center(segment).is_some_and(|value| value.abs_diff(center) < 2)
        }) {
            let segment = &segments[index];
            merged.annotations.push(TranscriptAnnotation {
                kind: AnnotationKind::AmbiguousOverlap,
                reason: "text appeared in a chunk overlap without a matching reading from the \
                         neighbouring chunk; kept rather than dropped"
                    .to_string(),
                segment_index: index,
                start: segment.start,
                end: segment.end,
            });
        }
    }

    merged
        .annotations
        .sort_by_key(|annotation| (annotation.segment_index, annotation.kind));
    merged.annotations.dedup_by(|a, b| {
        a.segment_index == b.segment_index && a.kind == b.kind && a.reason == b.reason
    });
    merged.segments = segments;
}

fn segment_center(segment: &TranscriptSegmentDraft) -> Option<u64> {
    let start = segment.start?.sample_offset;
    let end = segment.end?.sample_offset;
    Some(start + (end.saturating_sub(start)) / 2)
}

/// Shift a whole draft onto the absolute source timeline.
///
/// Exposed for adapters that receive chunk-relative engine output and want to
/// position it before validation.
#[must_use]
pub fn shift_draft_by_samples(mut draft: TranscriptDraft, samples: u64) -> TranscriptDraft {
    draft.segments = draft
        .segments
        .into_iter()
        .map(|segment| segment.shifted_by_samples(samples))
        .collect();
    for annotation in &mut draft.annotations {
        annotation.start = annotation
            .start
            .map(|time| time.shifted_by_samples(samples));
        annotation.end = annotation.end.map(|time| time.shifted_by_samples(samples));
    }
    draft
}

/// Convenience for tests and adapters: build a source time at the plan's rate.
#[must_use]
pub fn at(plan: &ChunkPlan, sample_offset: u64) -> SourceTime {
    SourceTime::new(
        sample_offset,
        plan.sample_rate_hz,
        plaudon_ai_contracts::TimePrecision::Segment,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{plan_chunks, BoundaryStrategy, EngineLimits};
    use plaudon_ai_contracts::transcript::TimePrecision;
    use plaudon_ai_contracts::Known;
    use plaudon_domain::media::Sha256Digest;
    use plaudon_domain::model::{EngineId, ModelIdentity};

    const RATE: u32 = 16_000;

    fn engine() -> EngineId {
        EngineId::new("whisper.cpp")
    }

    fn model() -> ModelIdentity {
        ModelIdentity::new(engine(), "tiny.en")
    }

    fn time(offset: u64) -> SourceTime {
        SourceTime::new(offset, RATE, TimePrecision::Segment)
    }

    fn timed(index: u32, start: u64, end: u64, text: &str) -> TranscriptSegmentDraft {
        TranscriptSegmentDraft::timed(index, time(start), time(end), text)
    }

    fn native(segments: Vec<TranscriptSegmentDraft>) -> TranscriptDraft {
        TranscriptDraft::native(engine(), model(), Sha256Digest::of(b"src"), segments)
    }

    fn text_only(segments: Vec<TranscriptSegmentDraft>) -> TranscriptDraft {
        TranscriptDraft::text_only(engine(), model(), Sha256Digest::of(b"src"), segments)
    }

    /// Two 30 s chunks over a 55 s source with a 5 s overlap:
    /// chunk 0 covers [0, 30), chunk 1 covers [25, 55).
    fn two_chunk_plan() -> ChunkPlan {
        plan_chunks(
            u64::from(RATE) * 55,
            RATE,
            &EngineLimits {
                max_file_bytes: Known::Unknown,
                max_duration_ms: Known::Known(30_000),
                bytes_per_frame: 4,
            },
            &BoundaryStrategy::Overlap {
                overlap_samples: u64::from(RATE) * 5,
            },
            None,
        )
        .expect("plan")
    }

    fn seconds(value: u64) -> u64 {
        u64::from(RATE) * value
    }

    #[test]
    fn single_chunk_passes_through_unchanged() {
        let plan = plan_chunks(
            seconds(10),
            RATE,
            &EngineLimits::unlimited(4),
            &BoundaryStrategy::Fixed,
            None,
        )
        .unwrap();
        let draft = native(vec![timed(0, 0, seconds(5), "hello world")]);
        let merged = reconcile_chunks(
            &plan,
            vec![ChunkTranscript::absolute(plan.chunks[0], draft)],
        )
        .unwrap();
        assert_eq!(merged.segments.len(), 1);
        assert_eq!(merged.plain_text(), "hello world");
        assert!(plan.is_single_chunk());
    }

    #[test]
    fn merged_times_equal_chunk_offset_plus_in_chunk_time() {
        let plan = two_chunk_plan();
        assert_eq!(plan.chunks.len(), 2);
        assert_eq!(plan.chunks[1].start_sample, seconds(25));

        // Chunk 1's engine reported a segment 2 s into its own audio.
        let chunk_relative = native(vec![timed(0, seconds(2), seconds(4), "second chunk")]);
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, 0, seconds(1), "first chunk")]),
                ),
                ChunkTranscript::relative(plan.chunks[1], chunk_relative),
            ],
        )
        .unwrap();

        let second = &merged.segments[1];
        assert_eq!(second.start.unwrap().sample_offset, seconds(27));
        assert_eq!(second.end.unwrap().sample_offset, seconds(29));
    }

    #[test]
    fn duplicated_overlap_text_is_merged_once() {
        let plan = two_chunk_plan();
        // "the budget is approved" is spoken at 26-28 s, inside the 25-30 s
        // overlap, so both chunks transcribe it.
        let chunk0 = native(vec![
            timed(0, seconds(1), seconds(3), "good morning"),
            timed(1, seconds(26), seconds(28), "the budget is approved"),
        ]);
        let chunk1 = native(vec![
            timed(0, seconds(26), seconds(28), "The budget is approved."),
            timed(1, seconds(40), seconds(42), "any other business"),
        ]);

        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(plan.chunks[0], chunk0),
                ChunkTranscript::absolute(plan.chunks[1], chunk1),
            ],
        )
        .unwrap();

        assert_eq!(
            merged.segments.len(),
            3,
            "overlap text was not deduplicated"
        );
        assert_eq!(
            merged.plain_text(),
            "good morning the budget is approved any other business"
        );
    }

    #[test]
    fn merged_timeline_is_sorted_and_non_overlapping() {
        let plan = two_chunk_plan();
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![
                        timed(0, seconds(1), seconds(3), "a"),
                        timed(1, seconds(26), seconds(29), "shared"),
                    ]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    native(vec![
                        timed(0, seconds(26), seconds(29), "shared"),
                        timed(1, seconds(31), seconds(33), "b"),
                    ]),
                ),
            ],
        )
        .unwrap();

        let mut previous_end = 0u64;
        for segment in &merged.segments {
            let start = segment.start.unwrap().sample_offset;
            let end = segment.end.unwrap().sample_offset;
            assert!(start >= previous_end, "timeline overlaps at {start}");
            assert!(end >= start);
            previous_end = end;
        }
        assert_eq!(
            merged
                .segments
                .iter()
                .map(|s| s.seq_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn out_of_order_arrival_is_sorted_by_chunk_index() {
        let plan = two_chunk_plan();
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    native(vec![timed(0, seconds(40), seconds(42), "later")]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, seconds(1), seconds(2), "earlier")]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(merged.plain_text(), "earlier later");
    }

    #[test]
    fn a_missing_chunk_is_an_error_not_a_silent_gap() {
        let plan = two_chunk_plan();
        let error = reconcile_chunks(
            &plan,
            vec![ChunkTranscript::absolute(
                plan.chunks[0],
                native(vec![timed(0, 0, seconds(1), "only")]),
            )],
        )
        .unwrap_err();
        assert_eq!(
            error,
            ReconcileError::MissingChunk {
                index: 1,
                expected: 2
            }
        );
    }

    #[test]
    fn a_duplicate_chunk_is_rejected() {
        let plan = two_chunk_plan();
        let draft = native(vec![timed(0, 0, seconds(1), "x")]);
        let error = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(plan.chunks[0], draft.clone()),
                ChunkTranscript::absolute(plan.chunks[0], draft),
            ],
        )
        .unwrap_err();
        assert_eq!(error, ReconcileError::DuplicateChunk { index: 0 });
    }

    #[test]
    fn empty_input_is_rejected() {
        let plan = two_chunk_plan();
        assert_eq!(
            reconcile_chunks(&plan, Vec::new()).unwrap_err(),
            ReconcileError::Empty
        );
    }

    #[test]
    fn mixed_alignment_origins_fail_closed() {
        let plan = two_chunk_plan();
        let error = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, 0, seconds(1), "a")]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    text_only(vec![TranscriptSegmentDraft::text_only(0, "b")]),
                ),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ReconcileError::MixedAlignmentOrigins { .. }
        ));
    }

    #[test]
    fn mixed_engines_fail_closed() {
        let plan = two_chunk_plan();
        let other_engine = EngineId::new("apple.speech");
        let mut foreign = native(vec![timed(0, seconds(40), seconds(41), "b")]);
        foreign.engine_id = other_engine.clone();
        foreign.model = ModelIdentity::new(other_engine, "sr");

        let error = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, 0, seconds(1), "a")]),
                ),
                ChunkTranscript::absolute(plan.chunks[1], foreign),
            ],
        )
        .unwrap_err();
        assert_eq!(error, ReconcileError::MixedEngines { index: 1 });
    }

    #[test]
    fn ambiguous_overlap_keeps_both_readings_and_records_uncertainty() {
        let plan = two_chunk_plan();
        // The chunks disagree about what was said at 26-28 s.
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, seconds(26), seconds(28), "fifteen units")]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    native(vec![timed(0, seconds(26), seconds(28), "fifty units")]),
                ),
            ],
        )
        .unwrap();

        assert_eq!(
            merged.segments.len(),
            2,
            "a disagreement must not drop content"
        );
        let text = merged.plain_text();
        assert!(text.contains("fifteen units"), "{text}");
        assert!(text.contains("fifty units"), "{text}");
        assert!(
            merged
                .annotations
                .iter()
                .any(|annotation| annotation.kind == AnnotationKind::AmbiguousOverlap),
            "uncertainty was not recorded"
        );
    }

    #[test]
    fn clamping_widens_uncertainty_rather_than_narrowing_it() {
        let plan = two_chunk_plan();
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, seconds(26), seconds(29), "alpha")]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    native(vec![timed(0, seconds(27), seconds(30), "beta")]),
                ),
            ],
        )
        .unwrap();

        assert_eq!(merged.segments.len(), 2);
        let second = &merged.segments[1];
        assert!(
            second.start.unwrap().sample_offset >= merged.segments[0].end.unwrap().sample_offset
        );
        assert!(
            second.start.unwrap().uncertainty_ms.is_some(),
            "a clamped start must carry the uncertainty it gained"
        );
    }

    #[test]
    fn zero_overlap_plans_merge_by_concatenation() {
        let plan = plan_chunks(
            seconds(55),
            RATE,
            &EngineLimits {
                max_file_bytes: Known::Unknown,
                max_duration_ms: Known::Known(30_000),
                bytes_per_frame: 4,
            },
            &BoundaryStrategy::Fixed,
            None,
        )
        .unwrap();
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, 0, seconds(2), "one")]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    native(vec![timed(0, seconds(31), seconds(33), "two")]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(merged.plain_text(), "one two");
    }

    #[test]
    fn full_duplication_collapses_to_one_copy() {
        let plan = two_chunk_plan();
        let shared = vec![
            timed(0, seconds(26), seconds(27), "alpha"),
            timed(1, seconds(27), seconds(28), "beta"),
        ];
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(plan.chunks[0], native(shared.clone())),
                ChunkTranscript::absolute(plan.chunks[1], native(shared)),
            ],
        )
        .unwrap();
        assert_eq!(merged.plain_text(), "alpha beta");
        assert_eq!(merged.segments.len(), 2);
    }

    #[test]
    fn text_only_chunks_merge_without_inventing_times() {
        let plan = two_chunk_plan();
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    text_only(vec![
                        TranscriptSegmentDraft::text_only(0, "the budget"),
                        TranscriptSegmentDraft::text_only(1, "is approved"),
                    ]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    text_only(vec![
                        TranscriptSegmentDraft::text_only(0, "is approved"),
                        TranscriptSegmentDraft::text_only(1, "any other business"),
                    ]),
                ),
            ],
        )
        .unwrap();

        assert_eq!(merged.alignment_origin, AlignmentOrigin::None);
        assert!(merged.segments.iter().all(|segment| !segment.has_times()));
        assert_eq!(
            merged.plain_text(),
            "the budget is approved any other business"
        );
    }

    #[test]
    fn the_plan_strategy_is_recorded_on_the_merged_draft() {
        let plan = two_chunk_plan();
        let merged = reconcile_chunks(
            &plan,
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![timed(0, 0, seconds(1), "a")]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    native(vec![timed(0, seconds(40), seconds(41), "b")]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(merged.chunking_strategy, plan.strategy);
        assert_eq!(merged.chunking_strategy.label(), "overlap");
    }

    #[test]
    fn reconciliation_is_deterministic() {
        let plan = two_chunk_plan();
        let build = || {
            vec![
                ChunkTranscript::absolute(
                    plan.chunks[0],
                    native(vec![
                        timed(0, seconds(1), seconds(3), "a"),
                        timed(1, seconds(26), seconds(28), "shared"),
                    ]),
                ),
                ChunkTranscript::absolute(
                    plan.chunks[1],
                    native(vec![
                        timed(0, seconds(26), seconds(28), "shared"),
                        timed(1, seconds(40), seconds(42), "b"),
                    ]),
                ),
            ]
        };
        assert_eq!(
            reconcile_chunks(&plan, build()).unwrap(),
            reconcile_chunks(&plan, build()).unwrap()
        );
    }

    #[test]
    fn shifting_a_draft_moves_annotations_too() {
        let mut draft = native(vec![timed(0, 0, seconds(1), "a")]);
        draft.annotations.push(TranscriptAnnotation {
            kind: AnnotationKind::LowConfidence,
            reason: "x".into(),
            segment_index: 0,
            start: Some(time(0)),
            end: Some(time(seconds(1))),
        });
        let shifted = shift_draft_by_samples(draft, seconds(10));
        assert_eq!(
            shifted.segments[0].start.unwrap().sample_offset,
            seconds(10)
        );
        assert_eq!(
            shifted.annotations[0].start.unwrap().sample_offset,
            seconds(10)
        );
    }
}
