//! Context-aware chunking and hierarchical summarization (spec §9.1, §9.4,
//! §11.4, §11.6 — task H2.3).
//!
//! §9.4 asks for chunks sized to the model's *verified* context length and
//! output budget, in chronological order, with section overlap, and forbids
//! truncating "unseen transcript tail content" while claiming a full summary.
//! §11.6 adds that long transcripts need hierarchical summarization with
//! intermediate evidence records on Apple FoundationModels. This module is the
//! shared planner both the Apple adapter and the generic text pipeline use.
//!
//! Three properties are load-bearing:
//!
//! 1. **Unknown context never silently becomes a number.** A model that
//!    reports no context window either fails closed or runs under an explicit
//!    conservative size that is labelled as such ([`ContextOrigin`]).
//! 2. **Coverage is accounted for, not assumed.** A [`ChunkPlan`] says whether
//!    the chunk ranges cover every supplied segment; [`ChunkPlan::is_complete`]
//!    and [`FinalSummary::is_complete`] are what gate a "complete summary"
//!    claim.
//! 3. **Nothing here performs I/O.** The planner is a pure function of segment
//!    sizes and a budget; the [`Summarization`] driver is a state machine the
//!    caller feeds model output into. It never calls a model itself, so the
//!    same driver serves any engine and replays deterministically.

use core::ops::Range;

use plaudon_ai_contracts::capabilities::TextModelCapabilities;
use plaudon_domain::capability::Known;

/// Characters assumed per token.
///
/// Two is deliberately pessimistic. English prose averages roughly four
/// characters per token, but transcripts are not always English: CJK scripts,
/// numbers and proper nouns tokenize at one to two characters per token. An
/// over-estimate only costs an extra chunk; an under-estimate overflows the
/// model's context window and fails the request (§9.4), so the ratio errs on
/// the side of more tokens.
pub const CHARS_PER_TOKEN: u64 = 2;

/// Tokens charged per segment on top of its text.
///
/// Each segment is rendered with a sequence label, speaker and timestamp
/// anchor (§9.6 requires source anchors), plus a separator. Eight tokens is a
/// round figure above that framing; it also guarantees every segment costs at
/// least one token so an empty-text segment can never make a chunk free.
pub const SEGMENT_FRAMING_TOKENS: u64 = 8;

/// `basis` label used when a conservative context size stands in for a
/// window the engine did not report (§11.4: Apple FoundationModels).
pub const UNREPORTED_CONTEXT_BASIS: &str = "engine reports no context window";

/// Conservative token estimate for one segment of `chars` characters.
///
/// See [`CHARS_PER_TOKEN`] and [`SEGMENT_FRAMING_TOKENS`] for why this errs
/// high.
#[must_use]
pub const fn estimate_segment_tokens(chars: u32) -> u64 {
    (chars as u64).div_ceil(CHARS_PER_TOKEN) + SEGMENT_FRAMING_TOKENS
}

/// Size of one transcript segment, as the planner sees it.
///
/// `seq_index` values must be strictly increasing across the slice handed to
/// [`plan_chunks`] and below `u32::MAX`. They need not be contiguous: a gap is
/// reported through [`Coverage::Incomplete`] rather than bridged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentExtent {
    /// Position of the segment in the transcript revision.
    pub seq_index: u32,
    /// Length of the segment text in characters.
    pub chars: u32,
}

/// What to do when the engine does not report a context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownContext {
    /// Refuse to plan. Unknown never satisfies (§9.4: "verified context length").
    FailClosed,
    /// Plan against a deliberately small, caller-chosen size and label the
    /// result as unverified.
    Conservative {
        /// Context window to assume, in tokens.
        tokens: u64,
    },
}

/// Where a [`ContextBudget`]'s context size came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextOrigin {
    /// Declared by the engine, or revealed by the model in an overflow error.
    Verified,
    /// Supplied by policy because the engine reported nothing.
    Conservative {
        /// Why a conservative size was used.
        basis: &'static str,
    },
}

/// Caller policy applied when deriving a [`ContextBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPolicy {
    /// Tokens held back for the model's answer.
    pub reserved_output_tokens: u64,
    /// Tokens consumed by the instructions and template around the transcript.
    pub prompt_overhead_tokens: u64,
    /// Segments repeated at the start of each chunk after the first.
    pub overlap_segments: u32,
    /// Behaviour when the context window is not reported.
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

/// Token budget a chunk plan is computed against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// Total context window, in tokens.
    pub context_tokens: u64,
    /// Tokens held back for the model's answer.
    pub reserved_output_tokens: u64,
    /// Tokens consumed by the instructions around the transcript.
    pub prompt_overhead_tokens: u64,
    /// Segments repeated at the start of each chunk after the first.
    pub overlap_segments: u32,
    /// Provenance of `context_tokens`.
    pub origin: ContextOrigin,
}

impl ContextBudget {
    /// Derive a budget from declared capabilities and caller policy.
    ///
    /// # Errors
    ///
    /// [`ChunkError::ContextLengthUnverified`] when the context window is
    /// unknown and the policy is [`UnknownContext::FailClosed`].
    pub fn from_capabilities(
        caps: &TextModelCapabilities,
        policy: &ChunkPolicy,
    ) -> Result<Self, ChunkError> {
        let (context_tokens, origin) = match (&caps.max_context_tokens, policy.unknown_context) {
            (Known::Known(tokens), _) => (*tokens, ContextOrigin::Verified),
            (Known::Unknown, UnknownContext::Conservative { tokens }) => (
                tokens,
                ContextOrigin::Conservative {
                    basis: UNREPORTED_CONTEXT_BASIS,
                },
            ),
            (Known::Unknown, UnknownContext::FailClosed) => {
                return Err(ChunkError::ContextLengthUnverified);
            }
        };
        Ok(Self {
            context_tokens,
            reserved_output_tokens: policy.reserved_output_tokens,
            prompt_overhead_tokens: policy.prompt_overhead_tokens,
            overlap_segments: policy.overlap_segments,
            origin,
        })
    }

    /// A budget with the context window the model itself reported in an
    /// overflow error.
    ///
    /// The model is the authority on its own limit, so the origin becomes
    /// [`ContextOrigin::Verified`]. The result never exceeds `self`: a larger
    /// observation (a mis-parsed or unrelated number) must not widen a budget
    /// that just failed.
    #[must_use]
    pub fn shrunk(&self, observed_context_tokens: u64) -> Self {
        Self {
            context_tokens: self.context_tokens.min(observed_context_tokens),
            origin: ContextOrigin::Verified,
            ..*self
        }
    }

    /// Tokens available for transcript text in one request.
    #[must_use]
    pub fn input_token_allowance(&self) -> u64 {
        self.context_tokens.saturating_sub(
            self.reserved_output_tokens
                .saturating_add(self.prompt_overhead_tokens),
        )
    }
}

/// One planned request's worth of segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Position of the chunk in the plan.
    pub index: u32,
    /// Half-open `seq_index` range, overlap prefix included.
    pub segments: Range<u32>,
    /// Number of leading segments repeated from the previous chunk.
    pub overlap_prefix: u32,
    /// Conservative token estimate for the whole range.
    pub estimated_tokens: u64,
}

/// Whether the chunk ranges cover every supplied segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// Every supplied segment is inside some chunk and nothing was bridged.
    Full,
    /// `seq_index` ranges missing from the input, in chronological order.
    Incomplete {
        /// The gaps.
        uncovered: Vec<Range<u32>>,
    },
}

/// Chronological chunk plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkPlan {
    /// Chunks in transcript order.
    pub chunks: Vec<Chunk>,
    /// Coverage accounting for the input.
    pub coverage: Coverage,
    /// Budget the plan was computed against.
    pub budget: ContextBudget,
}

impl ChunkPlan {
    /// Whether the plan may back a "complete summary" claim (§9.4).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self.coverage, Coverage::Full)
    }
}

/// Why a plan could not be produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChunkError {
    /// The context window is unknown and policy forbids guessing.
    #[error("model context length is not verified")]
    ContextLengthUnverified,
    /// There is nothing to chunk.
    #[error("transcript has no segments")]
    EmptyTranscript,
    /// One segment alone is larger than the input allowance. Also the result
    /// when the allowance is zero.
    #[error("segment {seq_index} exceeds the input token allowance")]
    SegmentExceedsBudget {
        /// The offending segment.
        seq_index: u32,
    },
}

/// Plan chronological chunks that each fit the input allowance.
///
/// Chunks are greedy: each takes as many new segments as fit after its overlap
/// prefix. The prefix is trimmed (down to nothing) when it would crowd out the
/// first new segment, so every chunk consumes at least one new segment and the
/// planner always terminates. Chunks never span a `seq_index` gap and the
/// overlap never reaches across one, so the ranges stay honest about what text
/// the model saw.
///
/// # Errors
///
/// [`ChunkError::EmptyTranscript`] for empty input;
/// [`ChunkError::SegmentExceedsBudget`] for the first segment that cannot fit
/// alone, which includes every segment when the allowance is zero.
pub fn plan_chunks(
    segments: &[SegmentExtent],
    budget: &ContextBudget,
) -> Result<ChunkPlan, ChunkError> {
    if segments.is_empty() {
        return Err(ChunkError::EmptyTranscript);
    }
    let allowance = budget.input_token_allowance();
    let cost = |i: usize| estimate_segment_tokens(segments[i].chars);
    // Checked up front so the error names the first oversized segment rather
    // than whichever one the greedy walk happens to reach.
    if let Some(s) = segments
        .iter()
        .find(|s| estimate_segment_tokens(s.chars) > allowance)
    {
        return Err(ChunkError::SegmentExceedsBudget {
            seq_index: s.seq_index,
        });
    }
    // Whether segment `i + 1` directly follows segment `i`.
    let adjacent =
        |i: usize| segments[i].seq_index.checked_add(1) == Some(segments[i + 1].seq_index);

    let n = segments.len();
    let overlap = budget.overlap_segments as usize;
    let mut chunks = Vec::new();
    let mut uncovered = Vec::new();
    let mut prev_end: Option<u32> = None;
    let mut next = 0;
    while next < n {
        let mut prefix = 0;
        let mut total = 0;
        while prefix < overlap && prefix < next && adjacent(next - prefix - 1) {
            prefix += 1;
            total += cost(next - prefix);
        }
        // Overlap is a convenience; forward progress is not.
        let first = cost(next);
        while prefix > 0 && total + first > allowance {
            total -= cost(next - prefix);
            prefix -= 1;
        }
        let mut end = next;
        while end < n && total + cost(end) <= allowance && (end == next || adjacent(end - 1)) {
            total += cost(end);
            end += 1;
        }
        let range =
            segments[next - prefix].seq_index..segments[end - 1].seq_index.saturating_add(1);
        let new_start = segments[next].seq_index;
        if let Some(pe) = prev_end.filter(|&pe| pe < new_start) {
            uncovered.push(pe..new_start);
        }
        prev_end = Some(range.end);
        chunks.push(Chunk {
            index: u32::try_from(chunks.len()).unwrap_or(u32::MAX),
            segments: range,
            overlap_prefix: prefix as u32,
            estimated_tokens: total,
        });
        next = end;
    }
    let coverage = if uncovered.is_empty() {
        Coverage::Full
    } else {
        Coverage::Incomplete { uncovered }
    };
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
///
/// # Errors
///
/// The errors of [`plan_chunks`].
pub fn plan_chunks_for_revision(
    revision: &plaudon_domain::transcript::TranscriptRevision,
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

// ---------------------------------------------------------------------------
// Hierarchical pipeline (§9.1, §9.4, §11.6)
// ---------------------------------------------------------------------------

/// Identity under which every intermediate record is produced, so a summary
/// can be reproduced or marked stale (§9.1, §9.6A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Immutable transcript revision the segments come from.
    pub input_revision: String,
    /// Version of the prompt/template that produced the output.
    pub prompt_version: String,
    /// Engine that ran the model.
    pub engine_id: String,
    /// Model that produced the output.
    pub model: String,
}

/// Intermediate evidence record: one model output over one chunk.
///
/// Level 0 summarizes transcript chunks; level `n > 0` summarizes the level
/// `n - 1` records, so a long transcript reduces in as many passes as it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionSummary {
    /// Reduction level; 0 is transcript text.
    pub level: u32,
    /// Index of the chunk in its level's plan.
    pub chunk_index: u32,
    /// Transcript `seq_index` ranges this record covers. Ranges that do not
    /// touch stay separate so a gap is never hidden by a merged record.
    pub covered: Vec<Range<u32>>,
    /// Leading segments (level 0) or records (higher levels) repeated from the
    /// previous chunk.
    pub overlap_prefix: u32,
    /// Identity of the producing pipeline step.
    pub provenance: Provenance,
    /// Plan generation, incremented by each re-chunk after an overflow.
    pub generation: u32,
    /// Context window the chunk was planned against.
    pub context_tokens: u64,
    /// Model output.
    pub text: String,
}

/// Audit entry for a re-chunk after a context overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RechunkRecord {
    /// Generation that began with this re-chunk.
    pub generation: u32,
    /// Context window of the budget that overflowed.
    pub from_context_tokens: u64,
    /// Context window the model reported.
    pub observed_context_tokens: u64,
    /// Context window the new plan uses.
    pub to_context_tokens: u64,
    /// Records discarded because they were produced under the old plan.
    pub discarded_records: u32,
}

/// Why the driver cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// Planning failed.
    #[error(transparent)]
    Chunk(#[from] ChunkError),
    /// The overflow report did not reveal a smaller context window, so
    /// re-planning would repeat the same request.
    #[error("observed context {observed} does not shrink the {current}-token budget")]
    OverflowNotInformative {
        /// Context window currently planned against.
        current: u64,
        /// Context window the model reported.
        observed: u64,
    },
    /// A consolidation pass would not reduce the number of records, so it
    /// would never terminate.
    #[error("consolidation of {inputs} records does not converge")]
    SynthesisDoesNotConverge {
        /// Records the pass started with.
        inputs: u32,
    },
    /// The summary is already complete; no step is pending.
    #[error("summarization already finished")]
    AlreadyFinished,
    /// The summary is not finished yet.
    #[error("summarization has pending steps")]
    NotFinished,
}

/// The finished hierarchical summary and everything needed to audit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalSummary {
    /// Final model output.
    pub text: String,
    /// Identity of the pipeline run.
    pub provenance: Provenance,
    /// Transcript coverage of the level-0 plan.
    pub coverage: Coverage,
    /// Accepted records per level, level 0 first; the last level holds the
    /// final record.
    pub levels: Vec<Vec<SectionSummary>>,
    /// Records discarded by re-chunking.
    pub superseded: Vec<SectionSummary>,
    /// Re-chunks that happened, in order.
    pub rechunks: Vec<RechunkRecord>,
}

impl FinalSummary {
    /// Whether this may be presented as a complete summary (§9.4).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self.coverage, Coverage::Full)
    }
}

/// Work the caller must perform next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step<'a> {
    /// Summarize this chunk of transcript text.
    Summarize {
        /// The chunk, with `seq_index` ranges into the transcript.
        chunk: &'a Chunk,
    },
    /// Consolidate these records. `is_final` marks the pass whose output is
    /// the final summary.
    Synthesize {
        /// Reduction level of the output (at least 1).
        level: u32,
        /// The chunk, with indices into the previous level's records.
        chunk: &'a Chunk,
        /// The records to consolidate, in order.
        inputs: &'a [SectionSummary],
        /// Whether this is the last pass.
        is_final: bool,
    },
    /// Nothing left to do; read [`Summarization::final_summary`].
    Done,
}

/// Pure driver for section-summary → final-synthesis.
///
/// The caller loops: ask [`Summarization::step`], run the model, feed the text
/// to [`Summarization::submit`], or on a context-overflow error call
/// [`Summarization::report_overflow`]. All state lives here, so a run replays
/// from the same inputs and model outputs.
#[derive(Debug, Clone)]
pub struct Summarization {
    segments: Vec<SegmentExtent>,
    provenance: Provenance,
    budget: ContextBudget,
    plan: ChunkPlan,
    transcript_coverage: Coverage,
    generation: u32,
    /// `levels[level]` holds accepted records of the pass in progress.
    levels: Vec<Vec<SectionSummary>>,
    superseded: Vec<SectionSummary>,
    rechunks: Vec<RechunkRecord>,
    finished: Option<FinalSummary>,
}

impl Summarization {
    /// Plan the transcript and start at level 0.
    ///
    /// # Errors
    ///
    /// Any [`ChunkError`] from [`plan_chunks`].
    pub fn start(
        segments: &[SegmentExtent],
        budget: ContextBudget,
        provenance: Provenance,
    ) -> Result<Self, PipelineError> {
        let plan = plan_chunks(segments, &budget)?;
        Ok(Self {
            segments: segments.to_vec(),
            provenance,
            budget,
            transcript_coverage: plan.coverage.clone(),
            plan,
            generation: 0,
            levels: vec![Vec::new()],
            superseded: Vec::new(),
            rechunks: Vec::new(),
            finished: None,
        })
    }

    fn level(&self) -> usize {
        self.levels.len() - 1
    }

    /// The next piece of work.
    #[must_use]
    pub fn step(&self) -> Step<'_> {
        if self.finished.is_some() {
            return Step::Done;
        }
        let level = self.level();
        let chunk = &self.plan.chunks[self.levels[level].len()];
        if level == 0 {
            return Step::Summarize { chunk };
        }
        let prev = &self.levels[level - 1];
        Step::Synthesize {
            level: level as u32,
            chunk,
            inputs: &prev[chunk.segments.start as usize..chunk.segments.end as usize],
            is_final: self.plan.chunks.len() == 1,
        }
    }

    /// Record the model output for the current step and advance.
    ///
    /// # Errors
    ///
    /// [`PipelineError::AlreadyFinished`] when no step is pending;
    /// [`PipelineError::SynthesisDoesNotConverge`] or a [`ChunkError`] when
    /// the next consolidation pass cannot be planned. On error nothing is
    /// recorded.
    pub fn submit(&mut self, text: String) -> Result<(), PipelineError> {
        if self.finished.is_some() {
            return Err(PipelineError::AlreadyFinished);
        }
        let level = self.level();
        let chunk = &self.plan.chunks[self.levels[level].len()];
        let covered = if level == 0 {
            vec![chunk.segments.clone()]
        } else {
            let prev = &self.levels[level - 1];
            coalesce(
                prev[chunk.segments.start as usize..chunk.segments.end as usize]
                    .iter()
                    .flat_map(|s| s.covered.iter().cloned()),
            )
        };
        let summary = SectionSummary {
            level: level as u32,
            chunk_index: chunk.index,
            covered,
            overlap_prefix: chunk.overlap_prefix,
            provenance: self.provenance.clone(),
            generation: self.generation,
            context_tokens: self.budget.context_tokens,
            text,
        };
        self.levels[level].push(summary);
        if let Err(e) = self.advance() {
            self.levels[level].pop();
            return Err(e);
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<(), PipelineError> {
        let level = self.level();
        let done = &self.levels[level];
        if done.len() < self.plan.chunks.len() {
            return Ok(());
        }
        if done.len() == 1 {
            self.finished = Some(FinalSummary {
                text: done[0].text.clone(),
                provenance: self.provenance.clone(),
                coverage: self.transcript_coverage.clone(),
                levels: self.levels.clone(),
                superseded: self.superseded.clone(),
                rechunks: self.rechunks.clone(),
            });
            return Ok(());
        }
        // Summaries are not transcript, so repeating them as overlap would
        // only bloat the pass and could stall the reduction.
        let extents: Vec<SegmentExtent> = done
            .iter()
            .enumerate()
            .map(|(i, s)| SegmentExtent {
                seq_index: i as u32,
                chars: u32::try_from(s.text.chars().count()).unwrap_or(u32::MAX),
            })
            .collect();
        let budget = ContextBudget {
            overlap_segments: 0,
            ..self.budget
        };
        let plan = plan_chunks(&extents, &budget)?;
        if plan.chunks.len() >= extents.len() {
            return Err(PipelineError::SynthesisDoesNotConverge {
                inputs: extents.len() as u32,
            });
        }
        self.plan = plan;
        self.levels.push(Vec::new());
        Ok(())
    }

    /// The model rejected a request as too large and revealed its context
    /// window. Re-plan the whole transcript against `budget.shrunk(observed)`.
    ///
    /// Everything produced under the old plan is moved to
    /// [`FinalSummary::superseded`] and a [`RechunkRecord`] is kept, so the
    /// final record shows the re-chunk happened (§9.1).
    ///
    /// # Errors
    ///
    /// [`PipelineError::OverflowNotInformative`] when `observed` is not below
    /// the current window; a [`ChunkError`] when the shrunk budget cannot hold
    /// even one segment. On error state is unchanged.
    pub fn report_overflow(&mut self, observed_context_tokens: u64) -> Result<(), PipelineError> {
        if self.finished.is_some() {
            return Err(PipelineError::AlreadyFinished);
        }
        if observed_context_tokens >= self.budget.context_tokens {
            return Err(PipelineError::OverflowNotInformative {
                current: self.budget.context_tokens,
                observed: observed_context_tokens,
            });
        }
        let budget = self.budget.shrunk(observed_context_tokens);
        let plan = plan_chunks(&self.segments, &budget)?;
        let discarded: Vec<SectionSummary> = self.levels.drain(..).flatten().collect();
        self.generation += 1;
        self.rechunks.push(RechunkRecord {
            generation: self.generation,
            from_context_tokens: self.budget.context_tokens,
            observed_context_tokens,
            to_context_tokens: budget.context_tokens,
            discarded_records: discarded.len() as u32,
        });
        self.superseded.extend(discarded);
        self.levels.push(Vec::new());
        self.transcript_coverage = plan.coverage.clone();
        self.budget = budget;
        self.plan = plan;
        Ok(())
    }

    /// The finished summary, once [`Step::Done`] is reached.
    #[must_use]
    pub fn final_summary(&self) -> Option<&FinalSummary> {
        self.finished.as_ref()
    }

    /// Consume the driver and return the finished summary.
    ///
    /// # Errors
    ///
    /// [`PipelineError::NotFinished`] while steps remain.
    pub fn into_final(self) -> Result<FinalSummary, PipelineError> {
        self.finished.ok_or(PipelineError::NotFinished)
    }

    /// Re-chunks performed so far.
    #[must_use]
    pub fn rechunks(&self) -> &[RechunkRecord] {
        &self.rechunks
    }
}

/// Merge touching or overlapping ranges, keeping real gaps apart.
fn coalesce(ranges: impl Iterator<Item = Range<u32>>) -> Vec<Range<u32>> {
    let mut out: Vec<Range<u32>> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_ai_contracts::capabilities::TextModelCapabilities;
    use plaudon_domain::model::{EngineId, ModelIdentity};
    use proptest::prelude::*;

    fn caps(max_context: Known<u64>) -> TextModelCapabilities {
        let engine = EngineId::new("apple-foundation");
        let mut c =
            TextModelCapabilities::unknown(engine.clone(), ModelIdentity::new(engine, "model"));
        c.max_context_tokens = max_context;
        c
    }

    fn policy(unknown: UnknownContext) -> ChunkPolicy {
        ChunkPolicy {
            reserved_output_tokens: 100,
            prompt_overhead_tokens: 50,
            overlap_segments: 1,
            unknown_context: unknown,
        }
    }

    fn budget(context: u64, overlap: u32) -> ContextBudget {
        ContextBudget {
            context_tokens: context,
            reserved_output_tokens: 0,
            prompt_overhead_tokens: 0,
            overlap_segments: overlap,
            origin: ContextOrigin::Verified,
        }
    }

    /// Segments `0..n`, each costing exactly `cost` tokens.
    fn uniform(n: u32, cost: u64) -> Vec<SegmentExtent> {
        let chars = ((cost - SEGMENT_FRAMING_TOKENS) * CHARS_PER_TOKEN) as u32;
        (0..n)
            .map(|seq_index| SegmentExtent { seq_index, chars })
            .collect()
    }

    #[test]
    fn known_context_is_verified() {
        let b = ContextBudget::from_capabilities(
            &caps(Known::Known(8000)),
            &policy(UnknownContext::FailClosed),
        )
        .unwrap();
        assert_eq!(b.context_tokens, 8000);
        assert_eq!(b.origin, ContextOrigin::Verified);
        assert_eq!(b.input_token_allowance(), 7850);
    }

    #[test]
    fn unknown_context_fails_closed() {
        let r = ContextBudget::from_capabilities(
            &caps(Known::Unknown),
            &policy(UnknownContext::FailClosed),
        );
        assert_eq!(r, Err(ChunkError::ContextLengthUnverified));
    }

    #[test]
    fn unknown_context_conservative_is_labelled() {
        let b = ContextBudget::from_capabilities(
            &caps(Known::Unknown),
            &policy(UnknownContext::Conservative { tokens: 4096 }),
        )
        .unwrap();
        assert_eq!(b.context_tokens, 4096);
        assert_eq!(
            b.origin,
            ContextOrigin::Conservative {
                basis: UNREPORTED_CONTEXT_BASIS
            }
        );
    }

    #[test]
    fn known_context_ignores_conservative_policy() {
        let b = ContextBudget::from_capabilities(
            &caps(Known::Known(1000)),
            &policy(UnknownContext::Conservative { tokens: 4096 }),
        )
        .unwrap();
        assert_eq!(
            (b.context_tokens, b.origin),
            (1000, ContextOrigin::Verified)
        );
    }

    #[test]
    fn allowance_saturates_at_zero() {
        let mut b = budget(100, 0);
        b.reserved_output_tokens = 80;
        b.prompt_overhead_tokens = 80;
        assert_eq!(b.input_token_allowance(), 0);
        b.reserved_output_tokens = u64::MAX;
        assert_eq!(b.input_token_allowance(), 0);
    }

    #[test]
    fn zero_allowance_is_rejected_at_plan_time() {
        let mut b = budget(100, 2);
        b.reserved_output_tokens = 100;
        let r = plan_chunks(&uniform(3, 10), &b);
        assert_eq!(r, Err(ChunkError::SegmentExceedsBudget { seq_index: 0 }));
    }

    #[test]
    fn empty_transcript_is_rejected() {
        assert_eq!(
            plan_chunks(&[], &budget(1000, 1)),
            Err(ChunkError::EmptyTranscript)
        );
    }

    #[test]
    fn oversized_segment_names_its_index() {
        let mut segs = uniform(4, 10);
        segs[2].chars = 10_000;
        assert_eq!(
            plan_chunks(&segs, &budget(100, 0)),
            Err(ChunkError::SegmentExceedsBudget { seq_index: 2 })
        );
    }

    #[test]
    fn estimate_is_conservative_and_never_free() {
        assert_eq!(estimate_segment_tokens(0), SEGMENT_FRAMING_TOKENS);
        assert_eq!(estimate_segment_tokens(1), SEGMENT_FRAMING_TOKENS + 1);
        // At least as many tokens as the ~4 chars/token English average.
        assert!(estimate_segment_tokens(4000) >= 1000);
    }

    #[test]
    fn single_chunk_when_everything_fits() {
        let plan = plan_chunks(&uniform(5, 10), &budget(50, 1)).unwrap();
        assert_eq!(plan.chunks.len(), 1);
        assert_eq!(plan.chunks[0].segments, 0..5);
        assert_eq!(plan.chunks[0].overlap_prefix, 0);
        assert!(plan.is_complete());
    }

    #[test]
    fn overlap_is_reincluded_after_first_chunk() {
        // Allowance 40 = four 10-token segments.
        let plan = plan_chunks(&uniform(10, 10), &budget(40, 1)).unwrap();
        let ranges: Vec<_> = plan.chunks.iter().map(|c| c.segments.clone()).collect();
        assert_eq!(ranges, vec![0..4, 3..7, 6..10]);
        let prefixes: Vec<_> = plan.chunks.iter().map(|c| c.overlap_prefix).collect();
        assert_eq!(prefixes, vec![0, 1, 1]);
        assert!(plan.chunks.iter().all(|c| c.estimated_tokens <= 40));
        assert!(plan.is_complete());
    }

    #[test]
    fn overlap_is_trimmed_to_keep_progress() {
        // Allowance 20: two segments. Overlap of 5 cannot fit; it shrinks to
        // one segment, then the chunk still takes one new segment.
        let plan = plan_chunks(&uniform(6, 10), &budget(20, 5)).unwrap();
        assert!(plan.chunks.iter().skip(1).all(|c| c.overlap_prefix == 1));
        assert_eq!(plan.chunks.last().unwrap().segments.end, 6);
        // Allowance equal to one segment forces zero overlap.
        let plan = plan_chunks(&uniform(4, 10), &budget(10, 3)).unwrap();
        assert_eq!(plan.chunks.len(), 4);
        assert!(plan.chunks.iter().all(|c| c.overlap_prefix == 0));
    }

    #[test]
    fn gaps_are_reported_not_bridged() {
        let segs: Vec<_> = [0, 1, 2, 5, 6, 9]
            .into_iter()
            .map(|seq_index| SegmentExtent {
                seq_index,
                chars: 2,
            })
            .collect();
        let plan = plan_chunks(&segs, &budget(10_000, 2)).unwrap();
        assert_eq!(
            plan.coverage,
            Coverage::Incomplete {
                uncovered: vec![3..5, 7..9]
            }
        );
        assert!(!plan.is_complete());
        let ranges: Vec<_> = plan.chunks.iter().map(|c| c.segments.clone()).collect();
        assert_eq!(ranges, vec![0..3, 5..7, 9..10]);
        assert!(plan.chunks.iter().all(|c| c.overlap_prefix == 0));
    }

    #[test]
    fn shrunk_is_monotone_and_verified() {
        let b = ContextBudget {
            origin: ContextOrigin::Conservative { basis: "x" },
            overlap_segments: 3,
            ..budget(4096, 0)
        };
        let s = b.shrunk(2000);
        assert_eq!(s.context_tokens, 2000);
        assert_eq!(s.origin, ContextOrigin::Verified);
        assert_eq!(s.overlap_segments, 3);
        assert_eq!(b.shrunk(9999).context_tokens, 4096);
        assert_eq!(b.shrunk(4096).context_tokens, 4096);
    }

    #[test]
    fn hundred_thousand_segments_plan_quickly() {
        let segs: Vec<_> = (0..100_000)
            .map(|seq_index| SegmentExtent {
                seq_index,
                chars: 120 + seq_index % 200,
            })
            .collect();
        let started = std::time::Instant::now();
        let plan = plan_chunks(&segs, &budget(4096, 2)).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(plan.is_complete());
        assert_eq!(plan.chunks.last().unwrap().segments.end, 100_000);
        // One Chunk per request, not one per segment.
        assert!(plan.chunks.len() < 20_000);
        assert!(plan.chunks.iter().all(|c| c.estimated_tokens <= 4096));
    }

    fn prov() -> Provenance {
        Provenance {
            input_revision: "rev-1".into(),
            prompt_version: "summary-v1".into(),
            engine_id: "apple-foundation".into(),
            model: "model".into(),
        }
    }

    /// Run the driver, answering each step with a fixed-size string.
    fn drive(s: &mut Summarization, reply_chars: usize) {
        while let Step::Summarize { .. } | Step::Synthesize { .. } = s.step() {
            s.submit("x".repeat(reply_chars)).unwrap();
        }
    }

    #[test]
    fn pipeline_single_chunk_is_final() {
        let mut s = Summarization::start(&uniform(3, 10), budget(100, 1), prov()).unwrap();
        assert!(matches!(s.step(), Step::Summarize { .. }));
        s.submit("summary".into()).unwrap();
        assert_eq!(s.step(), Step::Done);
        let f = s.into_final().unwrap();
        assert_eq!(f.text, "summary");
        assert!(f.is_complete());
        assert_eq!(f.levels.len(), 1);
        assert_eq!(f.levels[0][0].covered, vec![0..3]);
        assert_eq!(f.levels[0][0].provenance, prov());
    }

    #[test]
    fn pipeline_synthesizes_section_summaries() {
        let mut s = Summarization::start(&uniform(10, 10), budget(40, 1), prov()).unwrap();
        drive(&mut s, 5);
        let f = s.into_final().unwrap();
        assert!(f.levels.len() >= 2);
        let last = f.levels.last().unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].covered, vec![0..10]);
        assert!(f.is_complete());
        assert_eq!(f.rechunks.len(), 0);
    }

    #[test]
    fn pipeline_marks_final_pass() {
        let mut s = Summarization::start(&uniform(10, 10), budget(40, 0), prov()).unwrap();
        while let Step::Summarize { .. } = s.step() {
            s.submit("ok".into()).unwrap();
        }
        match s.step() {
            Step::Synthesize {
                level,
                inputs,
                is_final,
                ..
            } => {
                assert_eq!(level, 1);
                assert_eq!(inputs.len(), 3);
                assert!(is_final);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pipeline_rechunk_is_recorded() {
        let mut s = Summarization::start(&uniform(10, 10), budget(40, 0), prov()).unwrap();
        s.submit("a".into()).unwrap();
        s.report_overflow(20).unwrap();
        // Window 20 holds two segments.
        match s.step() {
            Step::Summarize { chunk } => assert_eq!(chunk.segments, 0..2),
            other => panic!("unexpected {other:?}"),
        }
        drive(&mut s, 1);
        let f = s.into_final().unwrap();
        assert_eq!(f.rechunks.len(), 1);
        let r = f.rechunks[0];
        assert_eq!(
            (
                r.generation,
                r.from_context_tokens,
                r.observed_context_tokens
            ),
            (1, 40, 20)
        );
        assert_eq!((r.to_context_tokens, r.discarded_records), (20, 1));
        assert_eq!(f.superseded.len(), 1);
        assert_eq!(f.superseded[0].generation, 0);
        assert!(f.levels[0]
            .iter()
            .all(|x| x.generation == 1 && x.context_tokens == 20));
    }

    #[test]
    fn pipeline_overflow_must_shrink() {
        let mut s = Summarization::start(&uniform(10, 10), budget(40, 0), prov()).unwrap();
        assert_eq!(
            s.report_overflow(40),
            Err(PipelineError::OverflowNotInformative {
                current: 40,
                observed: 40
            })
        );
        assert_eq!(
            s.report_overflow(5),
            Err(PipelineError::Chunk(ChunkError::SegmentExceedsBudget {
                seq_index: 0
            }))
        );
        assert_eq!(s.rechunks().len(), 0);
    }

    #[test]
    fn pipeline_reports_gaps_in_final_coverage() {
        let segs: Vec<_> = [0, 1, 4]
            .into_iter()
            .map(|seq_index| SegmentExtent {
                seq_index,
                chars: 2,
            })
            .collect();
        let mut s = Summarization::start(&segs, budget(1000, 0), prov()).unwrap();
        drive(&mut s, 1);
        let f = s.into_final().unwrap();
        assert!(!f.is_complete());
        assert_eq!(
            f.coverage,
            Coverage::Incomplete {
                uncovered: vec![Range { start: 2, end: 4 }],
            }
        );
    }

    #[test]
    fn pipeline_rejects_non_converging_synthesis() {
        // Allowance 10 with 8-token framing: replies of 2 chars cost 9, so no
        // two records fit together and the second level cannot reduce.
        let mut s = Summarization::start(&uniform(3, 10), budget(10, 0), prov()).unwrap();
        s.submit("xx".into()).unwrap();
        s.submit("xx".into()).unwrap();
        assert_eq!(
            s.submit("xx".into()),
            Err(PipelineError::SynthesisDoesNotConverge { inputs: 3 })
        );
        // The rejected record was not kept; the step is still pending.
        assert!(matches!(s.step(), Step::Summarize { .. }));
    }

    #[test]
    fn pipeline_submit_after_done_is_an_error() {
        let mut s = Summarization::start(&uniform(1, 10), budget(100, 0), prov()).unwrap();
        s.submit("a".into()).unwrap();
        assert_eq!(s.submit("b".into()), Err(PipelineError::AlreadyFinished));
        let s2 = Summarization::start(&uniform(1, 10), budget(100, 0), prov()).unwrap();
        assert_eq!(s2.into_final().unwrap_err(), PipelineError::NotFinished);
    }

    fn arb_input() -> impl Strategy<Value = (Vec<SegmentExtent>, ContextBudget)> {
        (
            proptest::collection::vec((1u32..4, 0u32..1500, any::<bool>()), 1..250),
            100u64..6000,
            0u64..40,
            0u64..40,
            0u32..6,
        )
            .prop_map(|(raw, context, reserved, overhead, overlap)| {
                let mut seq = 0u32;
                let segs = raw
                    .into_iter()
                    .map(|(step, chars, jump)| {
                        // Mostly contiguous; occasionally skip indices.
                        seq += if jump && step > 2 { step } else { 1 };
                        SegmentExtent {
                            seq_index: seq,
                            chars,
                        }
                    })
                    .collect();
                let b = ContextBudget {
                    context_tokens: context,
                    reserved_output_tokens: reserved,
                    prompt_overhead_tokens: overhead,
                    overlap_segments: overlap,
                    origin: ContextOrigin::Verified,
                };
                (segs, b)
            })
    }

    proptest! {
        #[test]
        fn plan_invariants((segs, b) in arb_input()) {
            let allowance = b.input_token_allowance();
            let oversized = segs
                .iter()
                .find(|s| estimate_segment_tokens(s.chars) > allowance);
            let plan = match plan_chunks(&segs, &b) {
                Err(ChunkError::SegmentExceedsBudget { seq_index }) => {
                    prop_assert_eq!(Some(seq_index), oversized.map(|s| s.seq_index));
                    return Ok(());
                }
                Err(e) => return Err(TestCaseError::fail(format!("{e}"))),
                Ok(p) => p,
            };
            prop_assert!(oversized.is_none());
            // Termination: at least one new segment per chunk.
            prop_assert!(plan.chunks.len() <= segs.len());

            let supplied: std::collections::BTreeSet<u32> =
                segs.iter().map(|s| s.seq_index).collect();
            let mut prev: Option<&Chunk> = None;
            for (i, c) in plan.chunks.iter().enumerate() {
                prop_assert_eq!(c.index as usize, i);
                prop_assert!(c.segments.start < c.segments.end);
                // Within allowance, and the estimate matches the contents.
                prop_assert!(c.estimated_tokens <= allowance);
                let inside: Vec<_> = segs
                    .iter()
                    .filter(|s| c.segments.contains(&s.seq_index))
                    .collect();
                prop_assert_eq!(
                    inside.iter().map(|s| estimate_segment_tokens(s.chars)).sum::<u64>(),
                    c.estimated_tokens
                );
                // Ranges never span a gap in the input.
                prop_assert_eq!(inside.len() as u32, c.segments.end - c.segments.start);
                // Overlap bounds.
                prop_assert!(c.overlap_prefix <= b.overlap_segments);
                prop_assert!(c.overlap_prefix < inside.len() as u32);
                if i == 0 {
                    prop_assert_eq!(c.overlap_prefix, 0);
                }
                // Chronological, strictly advancing.
                if let Some(p) = prev {
                    prop_assert!(c.segments.start > p.segments.start);
                    prop_assert!(c.segments.end > p.segments.end);
                    prop_assert!(c.segments.start + c.overlap_prefix >= p.segments.end);
                    if c.overlap_prefix > 0 {
                        prop_assert!(c.segments.start < p.segments.end);
                    }
                }
                prev = Some(c);
            }

            // Union invariant: every supplied seq is covered.
            for s in &segs {
                prop_assert!(plan.chunks.iter().any(|c| c.segments.contains(&s.seq_index)));
            }
            // Coverage reports exactly the missing indices between min and max.
            let (lo, hi) = (segs[0].seq_index, segs[segs.len() - 1].seq_index);
            let missing: Vec<u32> = (lo..=hi).filter(|q| !supplied.contains(q)).collect();
            match &plan.coverage {
                Coverage::Full => prop_assert!(missing.is_empty()),
                Coverage::Incomplete { uncovered } => {
                    let reported: Vec<u32> = uncovered.iter().flat_map(Clone::clone).collect();
                    prop_assert_eq!(reported, missing);
                }
            }
            prop_assert_eq!(plan.is_complete(), matches!(plan.coverage, Coverage::Full));
        }

        #[test]
        fn shrunk_never_increases(context in 0u64..100_000, observed in 0u64..200_000) {
            let b = budget(context, 1);
            let s = b.shrunk(observed);
            prop_assert!(s.context_tokens <= b.context_tokens);
            if observed < context {
                prop_assert!(s.context_tokens < b.context_tokens);
            }
            prop_assert_eq!(s.origin, ContextOrigin::Verified);
        }

        #[test]
        fn driver_terminates_and_covers((segs, b) in arb_input(), reply in 0usize..30) {
            let Ok(mut s) = Summarization::start(&segs, b, prov()) else {
                return Ok(());
            };
            let mut guard = 0;
            while !matches!(s.step(), Step::Done) {
                guard += 1;
                prop_assert!(guard <= segs.len() * 2 + 4);
                if s.submit("x".repeat(reply)).is_err() {
                    // Non-convergence is a legitimate, reported stop.
                    return Ok(());
                }
            }
            let f = s.into_final().unwrap();
            let last = f.levels.last().unwrap();
            prop_assert_eq!(last.len(), 1);
            let covered_total: u32 = last[0].covered.iter().map(|r| r.end - r.start).sum();
            let supplied = segs.len() as u32;
            prop_assert_eq!(covered_total, supplied);
        }
    }
}
