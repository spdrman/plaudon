//! Truthfulness guard for engine results (spec §8.6A).
//!
//! Every [`TranscriptDraft`] returned by an adapter passes through
//! [`validate_draft`] before it may become a revision. The guard enforces
//! three things the spec states as absolutes:
//!
//! 1. A text-only result (`alignment_origin = none`) exposes **no** seek times.
//! 2. An engine may not claim `native` alignment while declaring it does not
//!    produce segments.
//! 3. No transformation may raise a segment's claimed precision or invent a
//!    time that was not there before.

use crate::capabilities::SttCapabilities;
use crate::transcript::{AlignmentOrigin, TimePrecision, TranscriptDraft, TranscriptSegmentDraft};

/// A specific way a draft breaks the §8.6A contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TruthfulnessViolation {
    #[error("segment {segment_index} carries a {field} time but alignment origin is `none`")]
    TimeWithoutOrigin {
        segment_index: usize,
        field: &'static str,
    },

    #[error("segment {segment_index} carries word alignment but alignment origin is `none`")]
    WordsWithoutOrigin { segment_index: usize },

    #[error(
        "draft claims alignment origin `{origin}` but engine `{engine_id}` does not advertise \
         segment timestamps"
    )]
    OriginUnsupportedByEngine {
        origin: &'static str,
        engine_id: String,
    },

    #[error(
        "segment {segment_index} claims word-level precision but engine `{engine_id}` does not \
         advertise word timestamps"
    )]
    WordPrecisionUnsupportedByEngine {
        segment_index: usize,
        engine_id: String,
    },

    #[error("segment {segment_index} has end before start")]
    InvertedRange { segment_index: usize },

    #[error("segment {segment_index} starts before segment {previous_index} ends")]
    NonMonotonicTimeline {
        segment_index: usize,
        previous_index: usize,
    },

    #[error("segment {segment_index} has a start time but no end time (or the reverse)")]
    HalfOpenRange { segment_index: usize },

    #[error("segment {segment_index} word `{word_index}` falls outside its segment range")]
    WordOutsideSegment {
        segment_index: usize,
        word_index: usize,
    },

    #[error("segment {segment_index} mixes sample rates ({a} vs {b}) on the source timeline")]
    InconsistentSampleRate {
        segment_index: usize,
        a: u32,
        b: u32,
    },

    #[error("alignment origin is `{origin}` but no segment carries a time")]
    OriginWithoutTimes { origin: &'static str },

    #[error("annotation {annotation_index} references segment {segment_index}, which does not exist")]
    DanglingAnnotation {
        annotation_index: usize,
        segment_index: usize,
    },
}

/// Validate a draft against the §8.6A contract.
///
/// `capabilities` is the payload the producing engine advertises; passing
/// `None` validates only the intrinsic invariants (used when re-validating a
/// draft that has already been detached from its engine).
pub fn validate_draft(
    draft: &TranscriptDraft,
    capabilities: Option<&SttCapabilities>,
) -> Result<(), TruthfulnessViolation> {
    let origin = draft.alignment_origin;

    if origin.permits_times() {
        if let Some(caps) = capabilities {
            if !caps.supports_segments.satisfies() {
                return Err(TruthfulnessViolation::OriginUnsupportedByEngine {
                    origin: origin.as_str(),
                    engine_id: caps.engine_id.clone(),
                });
            }
        }
        // An origin that promises times must deliver at least one, otherwise it
        // is a text-only result wearing a timeline label.
        if !draft.segments.is_empty() && !draft.segments.iter().any(|s| s.has_times()) {
            return Err(TruthfulnessViolation::OriginWithoutTimes {
                origin: origin.as_str(),
            });
        }
    }

    let mut previous_end: Option<(usize, u64, u32)> = None;

    for (index, segment) in draft.segments.iter().enumerate() {
        if origin == AlignmentOrigin::None {
            if segment.start.is_some() {
                return Err(TruthfulnessViolation::TimeWithoutOrigin {
                    segment_index: index,
                    field: "start",
                });
            }
            if segment.end.is_some() {
                return Err(TruthfulnessViolation::TimeWithoutOrigin {
                    segment_index: index,
                    field: "end",
                });
            }
            if !segment.words.is_empty() {
                return Err(TruthfulnessViolation::WordsWithoutOrigin {
                    segment_index: index,
                });
            }
            continue;
        }

        validate_timed_segment(index, segment, capabilities, &mut previous_end)?;
    }

    for (annotation_index, annotation) in draft.annotations.iter().enumerate() {
        if annotation.segment_index >= draft.segments.len() {
            return Err(TruthfulnessViolation::DanglingAnnotation {
                annotation_index,
                segment_index: annotation.segment_index,
            });
        }
    }

    Ok(())
}

fn validate_timed_segment(
    index: usize,
    segment: &TranscriptSegmentDraft,
    capabilities: Option<&SttCapabilities>,
    previous_end: &mut Option<(usize, u64, u32)>,
) -> Result<(), TruthfulnessViolation> {
    let (start, end) = match (segment.start, segment.end) {
        (Some(start), Some(end)) => (start, end),
        (None, None) => {
            if segment.words.is_empty() {
                return Ok(());
            }
            return Err(TruthfulnessViolation::HalfOpenRange {
                segment_index: index,
            });
        }
        _ => {
            return Err(TruthfulnessViolation::HalfOpenRange {
                segment_index: index,
            })
        }
    };

    if start.sample_rate_hz != end.sample_rate_hz {
        return Err(TruthfulnessViolation::InconsistentSampleRate {
            segment_index: index,
            a: start.sample_rate_hz,
            b: end.sample_rate_hz,
        });
    }

    if end.sample_offset < start.sample_offset {
        return Err(TruthfulnessViolation::InvertedRange {
            segment_index: index,
        });
    }

    if let Some((previous_index, previous_offset, previous_rate)) = *previous_end {
        if previous_rate == start.sample_rate_hz && start.sample_offset < previous_offset {
            return Err(TruthfulnessViolation::NonMonotonicTimeline {
                segment_index: index,
                previous_index,
            });
        }
    }
    *previous_end = Some((index, end.sample_offset, end.sample_rate_hz));

    if !segment.words.is_empty() {
        if let Some(caps) = capabilities {
            if !caps.supports_word_timestamps.satisfies() {
                return Err(TruthfulnessViolation::WordPrecisionUnsupportedByEngine {
                    segment_index: index,
                    engine_id: caps.engine_id.clone(),
                });
            }
        }
        for (word_index, word) in segment.words.iter().enumerate() {
            if word.start.sample_rate_hz != start.sample_rate_hz
                || word.end.sample_rate_hz != start.sample_rate_hz
            {
                return Err(TruthfulnessViolation::InconsistentSampleRate {
                    segment_index: index,
                    a: start.sample_rate_hz,
                    b: word.start.sample_rate_hz,
                });
            }
            if word.start.sample_offset < start.sample_offset
                || word.end.sample_offset > end.sample_offset
                || word.end.sample_offset < word.start.sample_offset
            {
                return Err(TruthfulnessViolation::WordOutsideSegment {
                    segment_index: index,
                    word_index,
                });
            }
        }
    }

    Ok(())
}

/// Assert that a transformation did not raise any claim.
///
/// Used wherever a draft is rewritten (overlap reconciliation, normalisation).
/// A transformation may *lower* precision or *widen* uncertainty; it may never
/// do the reverse, and it may never add a time to a segment that had none.
pub fn assert_no_claim_inflation(
    before: &TranscriptDraft,
    after: &TranscriptDraft,
) -> Result<(), ClaimInflation> {
    if !before.alignment_origin.permits_times() && after.alignment_origin.permits_times() {
        return Err(ClaimInflation::OriginUpgraded {
            before: before.alignment_origin.as_str(),
            after: after.alignment_origin.as_str(),
        });
    }
    if before.alignment_origin == AlignmentOrigin::Derived
        && after.alignment_origin == AlignmentOrigin::Native
    {
        return Err(ClaimInflation::OriginUpgraded {
            before: before.alignment_origin.as_str(),
            after: after.alignment_origin.as_str(),
        });
    }

    let before_precision = max_precision(before);
    let after_precision = max_precision(after);
    match (before_precision, after_precision) {
        (None, Some(_)) => Err(ClaimInflation::TimesInvented),
        (Some(before_precision), Some(after_precision)) if after_precision > before_precision => {
            Err(ClaimInflation::PrecisionRaised {
                before: before_precision,
                after: after_precision,
            })
        }
        _ => Ok(()),
    }
}

fn max_precision(draft: &TranscriptDraft) -> Option<TimePrecision> {
    draft
        .segments
        .iter()
        .filter_map(TranscriptSegmentDraft::claimed_precision)
        .max()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimInflation {
    #[error("alignment origin upgraded from `{before}` to `{after}`")]
    OriginUpgraded {
        before: &'static str,
        after: &'static str,
    },
    #[error("transformation invented timestamps where none existed")]
    TimesInvented,
    #[error("claimed precision raised from {before:?} to {after:?}")]
    PrecisionRaised {
        before: TimePrecision,
        after: TimePrecision,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::SttCapabilities;
    use crate::tri::Tri;
    use crate::transcript::{SourceTime, WordAlignment};

    fn timed_caps() -> SttCapabilities {
        let mut caps = SttCapabilities::unknown("test-engine");
        caps.supports_segments = Tri::Yes;
        caps.supports_word_timestamps = Tri::Yes;
        caps
    }

    fn time(offset: u64) -> SourceTime {
        SourceTime::new(offset, 16_000, TimePrecision::Segment)
    }

    fn draft_with(origin: AlignmentOrigin, segments: Vec<TranscriptSegmentDraft>) -> TranscriptDraft {
        let mut draft = TranscriptDraft::text_only("test-engine", "m", segments);
        draft.alignment_origin = origin;
        draft
    }

    #[test]
    fn text_only_draft_is_valid() {
        let draft = draft_with(
            AlignmentOrigin::None,
            vec![TranscriptSegmentDraft::text_only("hello world")],
        );
        assert_eq!(validate_draft(&draft, None), Ok(()));
    }

    #[test]
    fn none_origin_with_start_time_is_rejected() {
        let mut segment = TranscriptSegmentDraft::text_only("hello");
        segment.start = Some(time(0));
        let draft = draft_with(AlignmentOrigin::None, vec![segment]);
        assert_eq!(
            validate_draft(&draft, None),
            Err(TruthfulnessViolation::TimeWithoutOrigin {
                segment_index: 0,
                field: "start"
            })
        );
    }

    #[test]
    fn none_origin_with_words_is_rejected() {
        let mut segment = TranscriptSegmentDraft::text_only("hello");
        segment.words.push(WordAlignment {
            text: "hello".into(),
            start: time(0),
            end: time(100),
            confidence: None,
        });
        let draft = draft_with(AlignmentOrigin::None, vec![segment]);
        assert_eq!(
            validate_draft(&draft, None),
            Err(TruthfulnessViolation::WordsWithoutOrigin { segment_index: 0 })
        );
    }

    #[test]
    fn native_origin_requires_engine_segment_support() {
        let mut caps = SttCapabilities::unknown("text-only-engine");
        caps.supports_segments = Tri::No;
        let draft = draft_with(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(time(0), time(100), "hi")],
        );
        assert!(matches!(
            validate_draft(&draft, Some(&caps)),
            Err(TruthfulnessViolation::OriginUnsupportedByEngine { .. })
        ));
    }

    #[test]
    fn unknown_segment_support_also_fails_closed() {
        // Tri::Unknown must not satisfy the requirement.
        let caps = SttCapabilities::unknown("unprobed-engine");
        let draft = draft_with(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(time(0), time(100), "hi")],
        );
        assert!(validate_draft(&draft, Some(&caps)).is_err());
    }

    #[test]
    fn inverted_and_non_monotonic_ranges_are_rejected() {
        let caps = timed_caps();
        let inverted = draft_with(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(time(100), time(50), "hi")],
        );
        assert_eq!(
            validate_draft(&inverted, Some(&caps)),
            Err(TruthfulnessViolation::InvertedRange { segment_index: 0 })
        );

        let overlapping = draft_with(
            AlignmentOrigin::Native,
            vec![
                TranscriptSegmentDraft::timed(time(0), time(100), "a"),
                TranscriptSegmentDraft::timed(time(50), time(150), "b"),
            ],
        );
        assert_eq!(
            validate_draft(&overlapping, Some(&caps)),
            Err(TruthfulnessViolation::NonMonotonicTimeline {
                segment_index: 1,
                previous_index: 0
            })
        );
    }

    #[test]
    fn half_open_range_is_rejected() {
        let caps = timed_caps();
        let mut segment = TranscriptSegmentDraft::text_only("hi");
        segment.start = Some(time(0));
        let draft = draft_with(AlignmentOrigin::Native, vec![segment]);
        assert_eq!(
            validate_draft(&draft, Some(&caps)),
            Err(TruthfulnessViolation::HalfOpenRange { segment_index: 0 })
        );
    }

    #[test]
    fn word_outside_segment_is_rejected() {
        let caps = timed_caps();
        let mut segment = TranscriptSegmentDraft::timed(time(100), time(200), "hi");
        segment.words.push(WordAlignment {
            text: "hi".into(),
            start: time(50),
            end: time(150),
            confidence: None,
        });
        let draft = draft_with(AlignmentOrigin::Native, vec![segment]);
        assert_eq!(
            validate_draft(&draft, Some(&caps)),
            Err(TruthfulnessViolation::WordOutsideSegment {
                segment_index: 0,
                word_index: 0
            })
        );
    }

    #[test]
    fn dangling_annotation_is_rejected() {
        use crate::transcript::{AnnotationKind, TranscriptAnnotation};
        let mut draft = draft_with(
            AlignmentOrigin::None,
            vec![TranscriptSegmentDraft::text_only("hi")],
        );
        draft.annotations.push(TranscriptAnnotation {
            kind: AnnotationKind::LowConfidence,
            reason: "x".into(),
            segment_index: 9,
            start: None,
            end: None,
        });
        assert_eq!(
            validate_draft(&draft, None),
            Err(TruthfulnessViolation::DanglingAnnotation {
                annotation_index: 0,
                segment_index: 9
            })
        );
    }

    #[test]
    fn claim_inflation_blocks_invented_times() {
        let before = draft_with(
            AlignmentOrigin::None,
            vec![TranscriptSegmentDraft::text_only("hi")],
        );
        let after = draft_with(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(time(0), time(10), "hi")],
        );
        assert!(matches!(
            assert_no_claim_inflation(&before, &after),
            Err(ClaimInflation::OriginUpgraded { .. })
        ));
    }

    #[test]
    fn claim_inflation_blocks_derived_to_native() {
        let before = draft_with(
            AlignmentOrigin::Derived,
            vec![TranscriptSegmentDraft::timed(time(0), time(10), "hi")],
        );
        let after = draft_with(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(time(0), time(10), "hi")],
        );
        assert!(matches!(
            assert_no_claim_inflation(&before, &after),
            Err(ClaimInflation::OriginUpgraded { .. })
        ));
    }

    #[test]
    fn claim_inflation_blocks_precision_upgrade() {
        let coarse = draft_with(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(time(0), time(10), "hi")],
        );
        let finer = draft_with(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(
                SourceTime::new(0, 16_000, TimePrecision::Sample),
                SourceTime::new(10, 16_000, TimePrecision::Sample),
                "hi",
            )],
        );
        assert!(matches!(
            assert_no_claim_inflation(&coarse, &finer),
            Err(ClaimInflation::PrecisionRaised { .. })
        ));
        // Lowering a claim is always allowed.
        assert_eq!(assert_no_claim_inflation(&finer, &coarse), Ok(()));
    }
}
