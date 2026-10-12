//! Context sizing and chronological chunking (spec 9.4, tasks I1.3 and H2.3).
//!
//! The hard rule of spec 9.4 is that we never truncate unseen transcript tail
//! content and then call the result a full summary. That is enforced here by
//! making coverage an explicit value: [`ChunkPlan::coverage`] is either
//! [`Coverage::Full`] or it names the segment ranges nobody read.
//!
//! Context length is treated as evidence, not as a guess. A model that has not
//! reported a verified context window gets [`ChunkError::ContextLengthUnverified`]
//! under the default policy. Engines that genuinely cannot report one — Apple
//! Foundation Models reports no context window at all — may opt into
//! [`UnknownContext::Conservative`], and the resulting budget is labelled
//! [`ContextOrigin::Conservative`] so the provenance record shows that the size
//! was assumed rather than measured.
//!
//! Planning is O(segments) and allocates one [`Chunk`] per window: the caller
//! streams chunks and never materializes the whole transcript as prompts, so
//! peak memory tracks the active window (spec 17.5).

use std::ops::Range;

use plaudon_ai_contracts::capabilities::TextModelCapabilities;
use plaudon_domain::transcript::TranscriptRevision;
use serde::{Deserialize, Serialize};

/// Characters per token assumed when estimating prompt size.
///
/// Deliberately pessimistic: four characters per token is the common English
/// rule of thumb, and we use three so a mixed-script transcript does not
/// silently overflow. Over-estimating costs an extra chunk; under-estimating
/// costs a failed request.
pub const CHARS_PER_TOKEN: u64 = 3;

/// Where the context size in a [`ContextBudget`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum ContextOrigin {
    /// The engine reported a context window and the capability payload is current.
    Verified,
    /// The engine reports no context window; a conservative bound was assumed.
    Conservative {
        /// Why this bound was chosen, recorded in output provenance.
        basis: ConservativeBasis,
    },
}

/// Why a conservative context bound was used instead of a measured one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConservativeBasis {
    /// The engine exposes no context window at all, so the policy supplied one.
    EngineReportsNoContextWindow,
    /// The provider rejected a request as too long, revealing a real ceiling.
    ProviderReportedOverflow,
}

impl ContextOrigin {
    /// Whether the size was measured rather than assumed.
    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// What to do when the selected model does not report a context window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UnknownContext {
    /// Refuse to plan. The default, and the only acceptable behaviour for a
    /// cloud model whose catalogue entry simply was not probed.
    #[default]
    FailClosed,
    /// Assume `tokens` and label the budget conservative.
    Conservative {
        /// Assumed context window, in tokens.
        tokens: u64,
    },
}

/// Caller-chosen chunking parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkPolicy {
    /// Tokens reserved for the model's own output.
    pub reserved_output_tokens: u64,
    /// Tokens reserved for instructions, fences and anchors metadata.
    pub prompt_overhead_tokens: u64,
    /// Segments repeated from the previous chunk to preserve continuity.
    pub overlap_segments: u32,
    /// What to do when the context window is unknown.
    pub unknown_context: UnknownContext,
}

impl ChunkPolicy {
    /// The shipped default: fail closed, 1024 output tokens, 1024 overhead,
    /// two segments of overlap.
    pub const DEFAULT: Self = Self {
        reserved_output_tokens: 1_024,
        prompt_overhead_tokens: 1_024,
        overlap_segments: 2,
        unknown_context: UnknownContext::FailClosed,
    };
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A resolved per-request token budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Total context window, in tokens.
    pub context_tokens: u64,
    /// Tokens reserved for output.
    pub reserved_output_tokens: u64,
    /// Tokens reserved for instructions and metadata.
    pub prompt_overhead_tokens: u64,
    /// Segments repeated from the previous chunk.
    pub overlap_segments: u32,
    /// Whether the context size was measured or assumed.
    pub origin: ContextOrigin,
}

impl ContextBudget {
    /// Derive a budget from probed capabilities.
    ///
    /// Fails closed when the model never reported a context window and the
    /// policy does not permit a conservative assumption.
    pub fn from_capabilities(
        caps: &TextModelCapabilities,
        policy: &ChunkPolicy,
    ) -> Result<Self, ChunkError> {
        let (context_tokens, origin) = match caps.max_context_tokens.get() {
            Some(tokens) if *tokens > 0 => (*tokens, ContextOrigin::Verified),
            _ => match policy.unknown_context {
                UnknownContext::FailClosed => {
                    return Err(ChunkError::ContextLengthUnverified);
                }
                UnknownContext::Conservative { tokens } if tokens > 0 => (
                    tokens,
                    ContextOrigin::Conservative {
                        basis: ConservativeBasis::EngineReportsNoContextWindow,
                    },
                ),
                UnknownContext::Conservative { .. } => {
                    return Err(ChunkError::ContextLengthUnverified)
                }
            },
        };

        // An engine that reports an output ceiling below the policy's reserve
        // cannot honour the reserve; the smaller of the two is the truth.
        let reserved_output_tokens = caps
            .max_output_tokens
            .get()
            .map_or(policy.reserved_output_tokens, |max| {
                policy.reserved_output_tokens.min(*max)
            });

        let budget = Self {
            context_tokens,
            reserved_output_tokens,
            prompt_overhead_tokens: policy.prompt_overhead_tokens,
            overlap_segments: policy.overlap_segments,
            origin,
        };
        if budget.transcript_tokens() == 0 {
            return Err(ChunkError::NoRoomForTranscript {
                context_tokens,
                reserved: reserved_output_tokens + policy.prompt_overhead_tokens,
            });
        }
        Ok(budget)
    }

    /// Tokens available for transcript text after output and overhead reserves.
    #[must_use]
    pub const fn transcript_tokens(&self) -> u64 {
        self.context_tokens
            .saturating_sub(self.reserved_output_tokens)
            .saturating_sub(self.prompt_overhead_tokens)
    }

    /// A budget re-derived from a context size the provider revealed by
    /// rejecting an overlong request.
    ///
    /// Spec 9.4 / H2.3: a context overflow is handled by re-chunking, never by
    /// dropping the tail. The result is labelled conservative because the
    /// number came from an error path, not from a capability probe.
    #[must_use]
    pub fn shrunk(&self, observed_context_tokens: u64) -> Self {
        Self {
            context_tokens: observed_context_tokens.min(self.context_tokens),
            reserved_output_tokens: self.reserved_output_tokens,
            prompt_overhead_tokens: self.prompt_overhead_tokens,
            overlap_segments: self.overlap_segments,
            origin: ContextOrigin::Conservative {
                basis: ConservativeBasis::ProviderReportedOverflow,
            },
        }
    }
}

/// The size of one transcript segment, in characters.
///
/// Planning deliberately takes extents rather than segments so it can run
/// against any transcript representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentExtent {
    /// Zero-based segment index.
    pub seq_index: u32,
    /// Length of the segment text in Unicode scalars.
    pub chars: u32,
}

impl SegmentExtent {
    /// Estimated prompt tokens for this segment, including its index prefix.
    #[must_use]
    pub const fn estimated_tokens(&self) -> u64 {
        // +8 tokens for the per-segment framing the prompt assembler adds.
        (self.chars as u64).div_ceil(CHARS_PER_TOKEN) + 8
    }
}

/// One planned chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Zero-based chunk index, in chronological order.
    pub index: u32,
    /// Half-open range of segment indices this chunk contains.
    pub segments: Range<u32>,
    /// How many leading segments of `segments` are overlap from the previous chunk.
    pub overlap_prefix: u32,
    /// Estimated prompt tokens for the transcript part of this chunk.
    pub estimated_tokens: u64,
}

impl Chunk {
    /// Segment indices this chunk is responsible for, excluding overlap.
    #[must_use]
    pub const fn owned_segments(&self) -> Range<u32> {
        (self.segments.start + self.overlap_prefix)..self.segments.end
    }
}

/// Whether the plan reads the whole transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum Coverage {
    /// Every segment is read by at least one chunk.
    Full,
    /// Some segments are not read. The ranges are named, never hidden.
    Incomplete {
        /// Half-open segment ranges no chunk contains.
        uncovered: Vec<Range<u32>>,
    },
}

impl Coverage {
    /// Whether the plan covers the whole transcript.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }
}

/// A chronological chunk plan with its coverage accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkPlan {
    /// Chunks in chronological order.
    pub chunks: Vec<Chunk>,
    /// Coverage of the planned chunks against the transcript.
    pub coverage: Coverage,
    /// Budget the plan was built for.
    pub budget: ContextBudget,
}

/// Why a chunk plan could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChunkError {
    /// The model never reported a verified context window.
    #[error("the selected model has no verified context length")]
    ContextLengthUnverified,
    /// The transcript has no segments.
    #[error("the transcript has no segments")]
    EmptyTranscript,
    /// Output and overhead reserves consume the whole context window.
    #[error(
        "a context window of {context_tokens} tokens leaves no room after reserving {reserved}"
    )]
    NoRoomForTranscript {
        /// Context window of the model.
        context_tokens: u64,
        /// Tokens reserved for output and overhead.
        reserved: u64,
    },
    /// A single segment does not fit in the budget even alone.
    #[error("segment {seq_index} alone exceeds the per-request budget")]
    SegmentExceedsBudget {
        /// Index of the oversized segment.
        seq_index: u32,
    },
}

/// Plan chronological chunks over `segments` within `budget`.
///
/// Segments are taken in the order given. Each chunk repeats up to
/// `budget.overlap_segments` trailing segments of its predecessor so a
/// sentence split across a boundary is still summarizable.
pub fn plan_chunks(
    segments: &[SegmentExtent],
    budget: &ContextBudget,
) -> Result<ChunkPlan, ChunkError> {
    if segments.is_empty() {
        return Err(ChunkError::EmptyTranscript);
    }
    let capacity = budget.transcript_tokens();
    if capacity == 0 {
        return Err(ChunkError::NoRoomForTranscript {
            context_tokens: budget.context_tokens,
            reserved: budget.reserved_output_tokens + budget.prompt_overhead_tokens,
        });
    }
    for extent in segments {
        if extent.estimated_tokens() > capacity {
            return Err(ChunkError::SegmentExceedsBudget {
                seq_index: extent.seq_index,
            });
        }
    }

    let first_index = segments[0].seq_index;
    let last_index = segments[segments.len() - 1].seq_index;
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut position = 0_usize;

    while position < segments.len() {
        let overlap_prefix = chunks.last().map_or(0, |previous: &Chunk| {
            let available = position;
            let wanted = budget.overlap_segments as usize;
            // Never re-read a whole previous chunk: that would not make
            // progress if the window is small.
            let previous_len = (previous.segments.end - previous.segments.start) as usize;
            wanted.min(available).min(previous_len - 1)
        });
        let start = position - overlap_prefix;
        let mut used = 0_u64;
        let mut end = start;
        while end < segments.len() {
            let next = segments[end].estimated_tokens();
            if used + next > capacity && end > position {
                break;
            }
            if used + next > capacity && end == position && overlap_prefix == 0 {
                break;
            }
            if used + next > capacity {
                // The overlap itself does not fit; drop it and retry from the
                // real position rather than making no progress.
                break;
            }
            used += next;
            end += 1;
        }
        if end <= position {
            // Overlap consumed the window: plan this chunk without overlap.
            let mut used_without_overlap = 0_u64;
            let mut end_without_overlap = position;
            while end_without_overlap < segments.len() {
                let next = segments[end_without_overlap].estimated_tokens();
                if used_without_overlap + next > capacity && end_without_overlap > position {
                    break;
                }
                used_without_overlap += next;
                end_without_overlap += 1;
            }
            chunks.push(Chunk {
                index: u32::try_from(chunks.len()).unwrap_or(u32::MAX),
                segments: segments[position].seq_index
                    ..segments[end_without_overlap - 1].seq_index + 1,
                overlap_prefix: 0,
                estimated_tokens: used_without_overlap,
            });
            position = end_without_overlap;
            continue;
        }

        chunks.push(Chunk {
            index: u32::try_from(chunks.len()).unwrap_or(u32::MAX),
            segments: segments[start].seq_index..segments[end - 1].seq_index + 1,
            overlap_prefix: u32::try_from(overlap_prefix).unwrap_or(0),
            estimated_tokens: used,
        });
        position = end;
    }

    let coverage = coverage_of(&chunks, first_index, last_index);
    Ok(ChunkPlan {
        chunks,
        coverage,
        budget: *budget,
    })
}

/// Plan chunks directly over a transcript revision.
///
/// Thin adapter kept next to [`plan_chunks`] so the pipeline never has to
/// build extents by hand.
pub fn plan_chunks_for_revision(
    revision: &TranscriptRevision,
    budget: &ContextBudget,
) -> Result<ChunkPlan, ChunkError> {
    let extents: Vec<SegmentExtent> = revision
        .segments()
        .iter()
        .map(|segment| SegmentExtent {
            seq_index: segment.seq_index,
            chars: u32::try_from(segment.text.chars().count()).unwrap_or(u32::MAX),
        })
        .collect();
    plan_chunks(&extents, budget)
}

fn coverage_of(chunks: &[Chunk], first_index: u32, last_index: u32) -> Coverage {
    let mut uncovered = Vec::new();
    let mut cursor = first_index;
    for chunk in chunks {
        if chunk.segments.start > cursor {
            uncovered.push(cursor..chunk.segments.start);
        }
        cursor = cursor.max(chunk.segments.end);
    }
    if cursor <= last_index {
        uncovered.push(cursor..last_index + 1);
    }
    if uncovered.is_empty() {
        Coverage::Full
    } else {
        Coverage::Incomplete { uncovered }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
    use plaudon_domain::model::{EngineId, ModelIdentity};
    use proptest::prelude::*;

    fn caps(context: Option<u64>, output: Option<u64>) -> TextModelCapabilities {
        let engine = EngineId::new("test-engine");
        let mut caps =
            TextModelCapabilities::unknown(engine.clone(), ModelIdentity::new(engine, "m"));
        caps.status = CapabilityStatus::Supported;
        caps.offline = Tri::Yes;
        caps.max_context_tokens = context.map_or(Known::Unknown, Known::Known);
        caps.max_output_tokens = output.map_or(Known::Unknown, Known::Known);
        caps
    }

    fn budget(context: u64) -> ContextBudget {
        ContextBudget {
            context_tokens: context,
            reserved_output_tokens: 10,
            prompt_overhead_tokens: 10,
            overlap_segments: 1,
            origin: ContextOrigin::Verified,
        }
    }

    fn extents(count: u32, chars: u32) -> Vec<SegmentExtent> {
        (0..count)
            .map(|seq_index| SegmentExtent { seq_index, chars })
            .collect()
    }

    #[test]
    fn an_unverified_context_length_fails_closed() {
        let error = ContextBudget::from_capabilities(&caps(None, Some(512)), &ChunkPolicy::DEFAULT)
            .unwrap_err();
        assert_eq!(error, ChunkError::ContextLengthUnverified);
    }

    #[test]
    fn an_opted_in_conservative_bound_is_labelled_not_hidden() {
        let policy = ChunkPolicy {
            unknown_context: UnknownContext::Conservative { tokens: 4_096 },
            ..ChunkPolicy::DEFAULT
        };
        let resolved = ContextBudget::from_capabilities(&caps(None, None), &policy).unwrap();
        assert_eq!(resolved.context_tokens, 4_096);
        assert!(!resolved.origin.is_verified());
        assert!(matches!(
            resolved.origin,
            ContextOrigin::Conservative { .. }
        ));
    }

    #[test]
    fn a_verified_context_length_is_labelled_verified() {
        let resolved = ContextBudget::from_capabilities(
            &caps(Some(8_192), Some(2_048)),
            &ChunkPolicy::DEFAULT,
        )
        .unwrap();
        assert_eq!(resolved.context_tokens, 8_192);
        assert_eq!(resolved.reserved_output_tokens, 1_024);
        assert!(resolved.origin.is_verified());
    }

    #[test]
    fn an_output_ceiling_below_the_reserve_wins() {
        let resolved =
            ContextBudget::from_capabilities(&caps(Some(8_192), Some(256)), &ChunkPolicy::DEFAULT)
                .unwrap();
        assert_eq!(resolved.reserved_output_tokens, 256);
    }

    #[test]
    fn a_context_window_consumed_by_reserves_is_rejected() {
        let error = ContextBudget::from_capabilities(
            &caps(Some(1_024), Some(1_024)),
            &ChunkPolicy::DEFAULT,
        )
        .unwrap_err();
        assert!(matches!(error, ChunkError::NoRoomForTranscript { .. }));
    }

    #[test]
    fn an_empty_transcript_is_an_error_not_an_empty_summary() {
        assert_eq!(
            plan_chunks(&[], &budget(1_000)).unwrap_err(),
            ChunkError::EmptyTranscript
        );
    }

    #[test]
    fn a_short_transcript_is_one_full_coverage_chunk() {
        let plan = plan_chunks(&extents(4, 30), &budget(1_000)).unwrap();
        assert_eq!(plan.chunks.len(), 1);
        assert_eq!(plan.chunks[0].segments, 0..4);
        assert!(plan.coverage.is_full());
    }

    #[test]
    fn a_long_transcript_is_split_chronologically_with_overlap() {
        let plan = plan_chunks(&extents(40, 90), &budget(200)).unwrap();
        assert!(plan.chunks.len() > 1);
        assert!(plan.coverage.is_full());
        for pair in plan.chunks.windows(2) {
            assert!(
                pair[0].segments.start <= pair[1].segments.start,
                "chunks must stay chronological"
            );
            assert_eq!(
                pair[1].overlap_prefix, 1,
                "the configured overlap is applied"
            );
            assert!(
                pair[1].segments.start < pair[0].segments.end,
                "overlap repeats the tail of the previous chunk"
            );
        }
    }

    #[test]
    fn a_single_oversized_segment_is_rejected_rather_than_truncated() {
        let error = plan_chunks(&extents(1, 100_000), &budget(200)).unwrap_err();
        assert_eq!(error, ChunkError::SegmentExceedsBudget { seq_index: 0 });
    }

    #[test]
    fn an_owned_range_excludes_the_overlap_prefix() {
        let plan = plan_chunks(&extents(40, 90), &budget(200)).unwrap();
        let second = &plan.chunks[1];
        assert_eq!(second.owned_segments().start, second.segments.start + 1);
    }

    #[test]
    fn a_shrunk_budget_is_smaller_and_labelled_conservative() {
        let shrunk = budget(8_000).shrunk(2_000);
        assert_eq!(shrunk.context_tokens, 2_000);
        assert!(matches!(shrunk.origin, ContextOrigin::Conservative { .. }));
        assert_eq!(budget(8_000).shrunk(9_000).context_tokens, 8_000);
    }

    #[test]
    fn coverage_reports_the_exact_gap_when_a_chunk_is_missing() {
        let chunks = vec![
            Chunk {
                index: 0,
                segments: 0..3,
                overlap_prefix: 0,
                estimated_tokens: 1,
            },
            Chunk {
                index: 1,
                segments: 7..10,
                overlap_prefix: 0,
                estimated_tokens: 1,
            },
        ];
        match coverage_of(&chunks, 0, 11) {
            Coverage::Incomplete { uncovered } => assert_eq!(uncovered, vec![3..7, 10..12]),
            Coverage::Full => panic!("a gap must not report full coverage"),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn every_segment_is_covered_and_order_is_preserved(
            count in 1_u32..200,
            chars in 1_u32..400,
            context in 160_u64..4_000,
            overlap in 0_u32..4,
        ) {
            let mut budget = budget(context);
            budget.overlap_segments = overlap;
            let segments = extents(count, chars);
            let Ok(plan) = plan_chunks(&segments, &budget) else {
                // The only permitted failure is a segment that cannot fit.
                let single = SegmentExtent {
                    seq_index: 0,
                    chars,
                };
                prop_assert!(single.estimated_tokens() > budget.transcript_tokens());
                return Ok(());
            };

            prop_assert!(plan.coverage.is_full(), "coverage must be total: {:?}", plan.coverage);

            let mut covered = vec![false; count as usize];
            let mut previous_start = 0_u32;
            for chunk in &plan.chunks {
                prop_assert!(chunk.segments.start >= previous_start, "chunks must be chronological");
                prop_assert!(chunk.segments.end > chunk.segments.start, "no empty chunk");
                prop_assert!(chunk.estimated_tokens <= budget.transcript_tokens());
                previous_start = chunk.segments.start;
                for index in chunk.segments.clone() {
                    covered[index as usize] = true;
                }
            }
            prop_assert!(covered.iter().all(|seen| *seen), "a segment was never read");
        }

        #[test]
        fn planning_makes_progress_for_any_overlap_setting(
            count in 2_u32..80,
            overlap in 0_u32..16,
        ) {
            let mut budget = budget(400);
            budget.overlap_segments = overlap;
            let plan = plan_chunks(&extents(count, 120), &budget).unwrap();
            prop_assert!(plan.chunks.len() <= count as usize * 2, "planning must terminate compactly");
            for chunk in &plan.chunks {
                prop_assert!(chunk.owned_segments().end > chunk.owned_segments().start);
            }
        }
    }
}
