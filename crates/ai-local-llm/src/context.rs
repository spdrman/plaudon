//! Context sizing and section planning for a local model (spec 9.4, 12).
//!
//! Spec 9.4 forbids "arbitrarily truncating unseen transcript tail content and
//! claiming the result is a full summary". The plan produced here therefore
//! carries an explicit [`Coverage`], and a caller that ignores it still cannot
//! claim full coverage, because the claim lives in the data rather than in the
//! caller's head.
//!
//! The types mirror `plaudon_app_core::chunking` field for field, and
//! [`SectionPlanner`] exists so that the shared chunker can be substituted for
//! [`LocalSectionPlanner`] without touching the engine: the engine only ever
//! sees the trait.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Average characters per token used when a runtime does not expose a
/// tokenizer. Deliberately pessimistic: under-counting tokens is what causes a
/// context overflow mid-generation.
pub const CHARS_PER_TOKEN: f64 = 3.0;

/// One segment's size, independent of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentExtent {
    /// Segment position in the revision.
    pub seq_index: u32,
    /// Character count of the segment text.
    pub chars: u32,
}

/// Where a context length came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum ContextOrigin {
    /// Read from the model file's own metadata.
    Verified,
    /// Assumed, with the basis recorded.
    Conservative {
        /// What the assumption rests on.
        basis: String,
    },
}

/// The budget a plan is built against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Total context window in tokens.
    pub context_tokens: u64,
    /// Tokens held back for the model's own output.
    pub reserved_output_tokens: u64,
    /// Tokens consumed by instructions, rules and fences.
    pub prompt_overhead_tokens: u64,
    /// Segments repeated at the head of each section, for continuity.
    pub overlap_segments: u32,
    /// Where `context_tokens` came from.
    pub origin: ContextOrigin,
}

impl ContextBudget {
    /// A budget from a verified context length.
    #[must_use]
    pub const fn verified(context_tokens: u64) -> Self {
        Self {
            context_tokens,
            reserved_output_tokens: 1_024,
            prompt_overhead_tokens: 512,
            overlap_segments: 1,
            origin: ContextOrigin::Verified,
        }
    }

    /// Tokens available for transcript content in one section.
    #[must_use]
    pub const fn input_token_allowance(&self) -> u64 {
        self.context_tokens
            .saturating_sub(self.reserved_output_tokens)
            .saturating_sub(self.prompt_overhead_tokens)
    }

    /// The same budget with a smaller observed context, for the re-plan after
    /// a runtime reports the window was narrower than advertised.
    #[must_use]
    pub fn shrunk(&self, observed_context_tokens: u64) -> Self {
        Self {
            context_tokens: observed_context_tokens.min(self.context_tokens),
            origin: ContextOrigin::Conservative {
                basis: "runtime reported a smaller window than the model metadata".to_string(),
            },
            ..self.clone()
        }
    }
}

/// One planned section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// Position of this section in the plan.
    pub index: u32,
    /// Half-open range of `seq_index` values, chronological.
    pub segments: Range<u32>,
    /// How many of the leading segments are repeated from the previous
    /// section, so a consumer can avoid double-counting them.
    pub overlap_prefix: u32,
    /// Estimated input tokens for this section.
    pub estimated_tokens: u64,
}

/// Whether the plan covers the whole transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum Coverage {
    /// Every segment appears in at least one section.
    Full,
    /// Some segments could not be placed.
    Incomplete {
        /// Ranges of `seq_index` values that no section covers.
        uncovered: Vec<Range<u32>>,
    },
}

/// A complete sectioning of a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionPlan {
    /// The sections, in chronological order.
    pub sections: Vec<Section>,
    /// Whether the plan covers everything.
    pub coverage: Coverage,
    /// Budget the plan was built against.
    pub budget: ContextBudget,
}

impl SectionPlan {
    /// Whether every segment is covered.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.coverage, Coverage::Full)
    }
}

/// Why a plan could not be produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// There is nothing to summarize.
    #[error("the transcript has no segments")]
    EmptyTranscript,
    /// One segment alone exceeds the whole input allowance.
    #[error("segment {seq_index} alone exceeds the model's input allowance")]
    SegmentExceedsBudget {
        /// The offending segment.
        seq_index: u32,
    },
    /// The budget leaves no room for input at all.
    #[error("the context window leaves no room for transcript content")]
    NoInputAllowance,
}

/// Something that can divide a transcript into context-sized sections.
///
/// The engine depends on this trait rather than on a concrete chunker, so the
/// workspace-wide chunker can replace [`LocalSectionPlanner`] by implementing
/// it.
pub trait SectionPlanner: std::fmt::Debug {
    /// Plan sections for the given segments and budget.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError`] rather than silently dropping content.
    fn plan(
        &self,
        segments: &[SegmentExtent],
        budget: &ContextBudget,
    ) -> Result<SectionPlan, PlanError>;
}

/// Estimated token count for a character count.
#[must_use]
pub fn estimate_tokens(chars: u32) -> u64 {
    (f64::from(chars) / CHARS_PER_TOKEN).ceil() as u64
}

/// A greedy chronological planner with a fixed overlap.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalSectionPlanner;

impl SectionPlanner for LocalSectionPlanner {
    fn plan(
        &self,
        segments: &[SegmentExtent],
        budget: &ContextBudget,
    ) -> Result<SectionPlan, PlanError> {
        if segments.is_empty() {
            return Err(PlanError::EmptyTranscript);
        }
        let allowance = budget.input_token_allowance();
        if allowance == 0 {
            return Err(PlanError::NoInputAllowance);
        }
        for segment in segments {
            if estimate_tokens(segment.chars) > allowance {
                return Err(PlanError::SegmentExceedsBudget {
                    seq_index: segment.seq_index,
                });
            }
        }

        let mut sections: Vec<Section> = Vec::new();
        let mut cursor = 0usize;
        let overlap = budget.overlap_segments as usize;

        while cursor < segments.len() {
            let start_cursor = if sections.is_empty() {
                cursor
            } else {
                cursor.saturating_sub(overlap)
            };
            let overlap_prefix = u32::try_from(cursor - start_cursor).unwrap_or(0);

            let mut tokens = 0u64;
            let mut end_cursor = start_cursor;
            while end_cursor < segments.len() {
                let next = tokens + estimate_tokens(segments[end_cursor].chars);
                if next > allowance && end_cursor > cursor {
                    break;
                }
                if next > allowance && end_cursor == cursor {
                    // A single segment that fits the allowance on its own,
                    // checked above, always enters here at least once.
                    break;
                }
                tokens = next;
                end_cursor += 1;
            }

            if end_cursor <= cursor {
                // No forward progress is possible without dropping content.
                let uncovered =
                    vec![segments[cursor].seq_index..segments[segments.len() - 1].seq_index + 1];
                return Ok(SectionPlan {
                    sections,
                    coverage: Coverage::Incomplete { uncovered },
                    budget: budget.clone(),
                });
            }

            sections.push(Section {
                index: u32::try_from(sections.len()).unwrap_or(u32::MAX),
                segments: segments[start_cursor].seq_index..segments[end_cursor - 1].seq_index + 1,
                overlap_prefix,
                estimated_tokens: tokens,
            });
            cursor = end_cursor;
        }

        Ok(SectionPlan {
            sections,
            coverage: Coverage::Full,
            budget: budget.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(count: u32, chars: u32) -> Vec<SegmentExtent> {
        (0..count)
            .map(|seq_index| SegmentExtent { seq_index, chars })
            .collect()
    }

    #[test]
    fn a_short_transcript_becomes_one_fully_covering_section() {
        let plan = LocalSectionPlanner
            .plan(&segments(5, 100), &ContextBudget::verified(32_768))
            .unwrap();
        assert_eq!(plan.sections.len(), 1);
        assert_eq!(plan.sections[0].segments, 0..5);
        assert!(plan.is_complete());
    }

    #[test]
    fn a_long_transcript_is_split_and_still_fully_covered() {
        let budget = ContextBudget {
            context_tokens: 4_096,
            reserved_output_tokens: 1_024,
            prompt_overhead_tokens: 512,
            overlap_segments: 1,
            origin: ContextOrigin::Verified,
        };
        let plan = LocalSectionPlanner
            .plan(&segments(200, 300), &budget)
            .unwrap();
        assert!(plan.sections.len() > 1);
        assert!(plan.is_complete());

        // The union of the sections, minus overlap prefixes, is the whole
        // transcript with nothing repeated and nothing missing.
        let mut expected = 0u32;
        for section in &plan.sections {
            assert_eq!(section.segments.start + section.overlap_prefix, expected);
            expected = section.segments.end;
        }
        assert_eq!(expected, 200);
    }

    #[test]
    fn every_section_stays_inside_the_input_allowance() {
        let budget = ContextBudget {
            context_tokens: 4_096,
            reserved_output_tokens: 1_024,
            prompt_overhead_tokens: 512,
            overlap_segments: 2,
            origin: ContextOrigin::Verified,
        };
        let plan = LocalSectionPlanner
            .plan(&segments(120, 250), &budget)
            .unwrap();
        for section in &plan.sections {
            assert!(
                section.estimated_tokens <= budget.input_token_allowance(),
                "{section:?} exceeds {}",
                budget.input_token_allowance()
            );
        }
    }

    #[test]
    fn sections_overlap_by_the_configured_number_of_segments() {
        let budget = ContextBudget {
            context_tokens: 2_048,
            reserved_output_tokens: 512,
            prompt_overhead_tokens: 256,
            overlap_segments: 2,
            origin: ContextOrigin::Verified,
        };
        let plan = LocalSectionPlanner
            .plan(&segments(100, 200), &budget)
            .unwrap();
        assert!(plan.sections.len() > 1);
        assert_eq!(plan.sections[0].overlap_prefix, 0);
        assert_eq!(plan.sections[1].overlap_prefix, 2);
    }

    #[test]
    fn an_empty_transcript_is_an_error_not_an_empty_summary() {
        assert_eq!(
            LocalSectionPlanner
                .plan(&[], &ContextBudget::verified(8_192))
                .unwrap_err(),
            PlanError::EmptyTranscript
        );
    }

    #[test]
    fn a_segment_larger_than_the_window_is_reported_rather_than_truncated() {
        let budget = ContextBudget::verified(2_048);
        let huge = vec![SegmentExtent {
            seq_index: 7,
            chars: 1_000_000,
        }];
        assert_eq!(
            LocalSectionPlanner.plan(&huge, &budget).unwrap_err(),
            PlanError::SegmentExceedsBudget { seq_index: 7 }
        );
    }

    #[test]
    fn a_window_with_no_room_for_input_is_refused() {
        let budget = ContextBudget {
            context_tokens: 1_000,
            reserved_output_tokens: 900,
            prompt_overhead_tokens: 200,
            overlap_segments: 0,
            origin: ContextOrigin::Verified,
        };
        assert_eq!(
            LocalSectionPlanner
                .plan(&segments(3, 10), &budget)
                .unwrap_err(),
            PlanError::NoInputAllowance
        );
    }

    #[test]
    fn shrinking_a_budget_records_that_the_number_is_now_conservative() {
        let budget = ContextBudget::verified(32_768);
        let shrunk = budget.shrunk(8_192);
        assert_eq!(shrunk.context_tokens, 8_192);
        assert!(matches!(shrunk.origin, ContextOrigin::Conservative { .. }));
        // Shrinking never enlarges.
        assert_eq!(budget.shrunk(99_999).context_tokens, 32_768);
    }

    #[test]
    fn token_estimation_is_pessimistic_and_rounds_up() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(3), 1);
        assert_eq!(estimate_tokens(4), 2);
    }
}
