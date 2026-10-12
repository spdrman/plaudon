//! Truthfulness guard for engine results (spec §8.6A — task F1.5).
//!
//! Every [`TranscriptDraft`] returned by an adapter passes through
//! [`validate_draft`] before it may become a revision. The guard enforces what
//! §8.6A states as absolutes:
//!
//! 1. A text-only result (`alignment_origin = none`) exposes **no** seek times.
//! 2. An engine may not claim `native` alignment while declaring it does not
//!    produce segments — `unknown` support does not count as support (§8.3).
//! 3. No transformation may raise a segment's claimed precision, upgrade its
//!    alignment origin, or invent a time that was not there before.

use plaudon_domain::transcript::AlignmentOrigin;

use crate::capabilities::SttCapabilities;
use crate::stt::{TranscriptDraft, TranscriptSegmentDraft};
use crate::transcript::TimePrecision;

/// A specific way a draft breaks the §8.6A contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TruthfulnessViolation {
    /// A text-only result carried a seek time.
    #[error("segment {segment_index} carries a {field} time but alignment origin is `none`")]
    TimeWithoutOrigin {
        /// Index of the offending segment.
        segment_index: usize,
        /// Which time was present.
        field: &'static str,
    },

    /// A text-only result carried word alignment.
    #[error("segment {segment_index} carries word alignment but alignment origin is `none`")]
    WordsWithoutOrigin {
        /// Index of the offending segment.
        segment_index: usize,
    },

    /// The engine does not declare segment timestamps but the draft has them.
    #[error(
        "draft claims alignment origin `{origin}` but engine `{engine_id}` does not advertise \
         segment timestamps"
    )]
    OriginUnsupportedByEngine {
        /// Claimed origin.
        origin: &'static str,
        /// Engine that produced the draft.
        engine_id: String,
    },

    /// Word-level alignment appeared from an engine that does not declare it.
    #[error(
        "segment {segment_index} claims word alignment but engine `{engine_id}` does not \
         advertise word timestamps"
    )]
    WordPrecisionUnsupportedByEngine {
        /// Index of the offending segment.
        segment_index: usize,
        /// Engine that produced the draft.
        engine_id: String,
    },

    /// A segment ends before it starts.
    #[error("segment {segment_index} has end before start")]
    InvertedRange {
        /// Index of the offending segment.
        segment_index: usize,
    },

    /// Segments are not in non-overlapping time order.
    #[error("segment {segment_index} starts before segment {previous_index} ends")]
    NonMonotonicTimeline {
        /// Index of the offending segment.
        segment_index: usize,
        /// Index of the segment it overlaps.
        previous_index: usize,
    },

    /// Exactly one of start/end is present.
    #[error("segment {segment_index} has a start time but no end time, or the reverse")]
    HalfOpenRange {
        /// Index of the offending segment.
        segment_index: usize,
    },

    /// A word alignment falls outside the segment that contains it.
    #[error("segment {segment_index} word {word_index} falls outside its segment range")]
    WordOutsideSegment {
        /// Index of the offending segment.
        segment_index: usize,
        /// Index of the offending word.
        word_index: usize,
    },

    /// Times within one segment are expressed at different sample rates.
    #[error("segment {segment_index} mixes sample rates ({a} vs {b}) on the source timeline")]
    InconsistentSampleRate {
        /// Index of the offending segment.
        segment_index: usize,
        /// First observed rate.
        a: u32,
        /// Second observed rate.
        b: u32,
    },

    /// The draft promises a timeline it does not deliver.
    #[error("alignment origin is `{origin}` but no segment carries a time")]
    OriginWithoutTimes {
        /// Claimed origin.
        origin: &'static str,
    },

    /// Segment indices are not a contiguous ascending run.
    #[error("segment at position {position} declares seq_index {seq_index}")]
    NonContiguousIndex {
        /// Position in the segment vector.
        position: usize,
        /// Index the segment declared.
        seq_index: u32,
    },

    /// An annotation points at a segment that does not exist.
    #[error("annotation {annotation_index} references missing segment {segment_index}")]
    DanglingAnnotation {
        /// Index of the offending annotation.
        annotation_index: usize,
        /// Segment it referenced.
        segment_index: usize,
    },
}

/// Validate a draft against the §8.6A contract.
///
/// `capabilities` is the payload the producing engine advertises. Passing
/// `None` validates only the intrinsic invariants, which is what a re-check of
/// a stored draft can do.
pub fn validate_draft(
    draft: &TranscriptDraft,
    capabilities: Option<&SttCapabilities>,
) -> Result<(), TruthfulnessViolation> {
    let origin = draft.alignment_origin;

    for (position, segment) in draft.segments.iter().enumerate() {
        if segment.seq_index as usize != position {
            return Err(TruthfulnessViolation::NonContiguousIndex {
                position,
                seq_index: segment.seq_index,
            });
        }
    }

    if origin.permits_times() {
        if let Some(caps) = capabilities {
            if !caps.supports_segments.satisfies() {
                return Err(TruthfulnessViolation::OriginUnsupportedByEngine {
                    origin: origin_label(origin),
                    engine_id: caps.engine_id.as_str().to_string(),
                });
            }
        }
        if !draft.segments.is_empty()
            && !draft.segments.iter().any(TranscriptSegmentDraft::has_times)
        {
            return Err(TruthfulnessViolation::OriginWithoutTimes {
                origin: origin_label(origin),
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

const fn origin_label(origin: AlignmentOrigin) -> &'static str {
    match origin {
        AlignmentOrigin::Native => "native",
        AlignmentOrigin::Derived => "derived",
        AlignmentOrigin::None => "none",
    }
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
                // A text-only segment inside an otherwise timed draft is
                // honest: the engine simply did not time this one.
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

    if segment.words.is_empty() {
        return Ok(());
    }
    if let Some(caps) = capabilities {
        if !caps.supports_word_timestamps.satisfies() {
            return Err(TruthfulnessViolation::WordPrecisionUnsupportedByEngine {
                segment_index: index,
                engine_id: caps.engine_id.as_str().to_string(),
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
    Ok(())
}

/// Ways a transformation can strengthen a claim it is not entitled to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimInflation {
    /// The alignment origin became a stronger claim.
    #[error("alignment origin upgraded from `{before}` to `{after}`")]
    OriginUpgraded {
        /// Origin before the transformation.
        before: &'static str,
        /// Origin after the transformation.
        after: &'static str,
    },
    /// Times appeared where none existed.
    #[error("transformation invented timestamps where none existed")]
    TimesInvented,
    /// A coarse claim became a fine one.
    #[error("claimed precision raised from {before:?} to {after:?}")]
    PrecisionRaised {
        /// Precision before.
        before: TimePrecision,
        /// Precision after.
        after: TimePrecision,
    },
    /// A stated uncertainty got smaller.
    #[error("stated uncertainty narrowed from {before} ms to {after} ms")]
    UncertaintyNarrowed {
        /// Uncertainty before.
        before: u32,
        /// Uncertainty after.
        after: u32,
    },
}

/// Assert that a transformation did not strengthen any claim.
///
/// Used wherever a draft is rewritten: overlap reconciliation, normalisation,
/// derived alignment. A transformation may *lower* precision or *widen*
/// uncertainty; it may never do the reverse.
pub fn assert_no_claim_inflation(
    before: &TranscriptDraft,
    after: &TranscriptDraft,
) -> Result<(), ClaimInflation> {
    if !before.alignment_origin.permits_times() && after.alignment_origin.permits_times() {
        return Err(ClaimInflation::OriginUpgraded {
            before: origin_label(before.alignment_origin),
            after: origin_label(after.alignment_origin),
        });
    }
    if before.alignment_origin == AlignmentOrigin::Derived
        && after.alignment_origin == AlignmentOrigin::Native
    {
        return Err(ClaimInflation::OriginUpgraded {
            before: origin_label(before.alignment_origin),
            after: origin_label(after.alignment_origin),
        });
    }

    match (max_precision(before), max_precision(after)) {
        (None, Some(_)) => return Err(ClaimInflation::TimesInvented),
        (Some(before_precision), Some(after_precision)) if after_precision > before_precision => {
            return Err(ClaimInflation::PrecisionRaised {
                before: before_precision,
                after: after_precision,
            })
        }
        _ => {}
    }

    if let (Some(before_uncertainty), Some(after_uncertainty)) =
        (max_uncertainty(before), max_uncertainty(after))
    {
        if after_uncertainty < before_uncertainty {
            return Err(ClaimInflation::UncertaintyNarrowed {
                before: before_uncertainty,
                after: after_uncertainty,
            });
        }
    }

    Ok(())
}

fn max_precision(draft: &TranscriptDraft) -> Option<TimePrecision> {
    draft
        .segments
        .iter()
        .filter_map(TranscriptSegmentDraft::claimed_precision)
        .max()
}

fn max_uncertainty(draft: &TranscriptDraft) -> Option<u32> {
    draft
        .segments
        .iter()
        .flat_map(|segment| [segment.start, segment.end])
        .flatten()
        .filter_map(|time| time.uncertainty_ms)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{DraftWordAlignment, SourceTime};
    use plaudon_domain::capability::Tri;
    use plaudon_domain::media::Sha256Digest;
    use plaudon_domain::model::{EngineId, ModelIdentity};

    fn engine() -> EngineId {
        EngineId::new("test-engine")
    }

    fn model() -> ModelIdentity {
        ModelIdentity::new(engine(), "m")
    }

    fn timed_caps() -> SttCapabilities {
        let mut caps = SttCapabilities::unknown(engine(), model());
        caps.supports_segments = Tri::Yes;
        caps.supports_word_timestamps = Tri::Yes;
        caps
    }

    fn time(offset: u64) -> SourceTime {
        SourceTime::new(offset, 16_000, TimePrecision::Segment)
    }

    fn draft(origin: AlignmentOrigin, segments: Vec<TranscriptSegmentDraft>) -> TranscriptDraft {
        let mut draft =
            TranscriptDraft::text_only(engine(), model(), Sha256Digest::of(b""), segments);
        draft.alignment_origin = origin;
        draft
    }

    #[test]
    fn text_only_draft_is_valid() {
        let draft = draft(
            AlignmentOrigin::None,
            vec![TranscriptSegmentDraft::text_only(0, "hello world")],
        );
        assert_eq!(validate_draft(&draft, None), Ok(()));
    }

    #[test]
    fn none_origin_with_a_start_time_is_rejected() {
        let mut segment = TranscriptSegmentDraft::text_only(0, "hello");
        segment.start = Some(time(0));
        let draft = draft(AlignmentOrigin::None, vec![segment]);
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
        let mut segment = TranscriptSegmentDraft::text_only(0, "hello");
        segment.words.push(DraftWordAlignment {
            text: "hello".into(),
            start: time(0),
            end: time(100),
            confidence: None,
        });
        let draft = draft(AlignmentOrigin::None, vec![segment]);
        assert_eq!(
            validate_draft(&draft, None),
            Err(TruthfulnessViolation::WordsWithoutOrigin { segment_index: 0 })
        );
    }

    #[test]
    fn native_origin_requires_declared_segment_support() {
        let mut caps = SttCapabilities::unknown(EngineId::new("text-only"), model());
        caps.supports_segments = Tri::No;
        let draft = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(0, time(0), time(100), "hi")],
        );
        assert!(matches!(
            validate_draft(&draft, Some(&caps)),
            Err(TruthfulnessViolation::OriginUnsupportedByEngine { .. })
        ));
    }

    #[test]
    fn unknown_segment_support_also_fails_closed() {
        let caps = SttCapabilities::unknown(engine(), model());
        let draft = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(0, time(0), time(100), "hi")],
        );
        assert!(validate_draft(&draft, Some(&caps)).is_err());
    }

    #[test]
    fn a_timed_origin_with_no_times_anywhere_is_rejected() {
        let draft = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::text_only(0, "hi")],
        );
        assert_eq!(
            validate_draft(&draft, Some(&timed_caps())),
            Err(TruthfulnessViolation::OriginWithoutTimes { origin: "native" })
        );
    }

    #[test]
    fn inverted_and_non_monotonic_ranges_are_rejected() {
        let caps = timed_caps();
        let inverted = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(0, time(100), time(50), "hi")],
        );
        assert_eq!(
            validate_draft(&inverted, Some(&caps)),
            Err(TruthfulnessViolation::InvertedRange { segment_index: 0 })
        );

        let overlapping = draft(
            AlignmentOrigin::Native,
            vec![
                TranscriptSegmentDraft::timed(0, time(0), time(100), "a"),
                TranscriptSegmentDraft::timed(1, time(50), time(150), "b"),
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
        let mut segment = TranscriptSegmentDraft::text_only(0, "hi");
        segment.start = Some(time(0));
        let draft = draft(AlignmentOrigin::Native, vec![segment]);
        assert_eq!(
            validate_draft(&draft, Some(&timed_caps())),
            Err(TruthfulnessViolation::HalfOpenRange { segment_index: 0 })
        );
    }

    #[test]
    fn word_outside_its_segment_is_rejected() {
        let mut segment = TranscriptSegmentDraft::timed(0, time(100), time(200), "hi");
        segment.words.push(DraftWordAlignment {
            text: "hi".into(),
            start: time(50),
            end: time(150),
            confidence: None,
        });
        let draft = draft(AlignmentOrigin::Native, vec![segment]);
        assert_eq!(
            validate_draft(&draft, Some(&timed_caps())),
            Err(TruthfulnessViolation::WordOutsideSegment {
                segment_index: 0,
                word_index: 0
            })
        );
    }

    #[test]
    fn word_alignment_from_an_engine_that_does_not_declare_it_is_rejected() {
        let mut caps = timed_caps();
        caps.supports_word_timestamps = Tri::Unknown;
        let mut segment = TranscriptSegmentDraft::timed(0, time(100), time(200), "hi");
        segment.words.push(DraftWordAlignment {
            text: "hi".into(),
            start: time(100),
            end: time(200),
            confidence: None,
        });
        let draft = draft(AlignmentOrigin::Native, vec![segment]);
        assert!(matches!(
            validate_draft(&draft, Some(&caps)),
            Err(TruthfulnessViolation::WordPrecisionUnsupportedByEngine { .. })
        ));
    }

    #[test]
    fn non_contiguous_segment_indices_are_rejected() {
        let draft = draft(
            AlignmentOrigin::None,
            vec![
                TranscriptSegmentDraft::text_only(0, "a"),
                TranscriptSegmentDraft::text_only(7, "b"),
            ],
        );
        assert_eq!(
            validate_draft(&draft, None),
            Err(TruthfulnessViolation::NonContiguousIndex {
                position: 1,
                seq_index: 7
            })
        );
    }

    #[test]
    fn dangling_annotation_is_rejected() {
        use crate::transcript::{AnnotationKind, TranscriptAnnotation};
        let mut value = draft(
            AlignmentOrigin::None,
            vec![TranscriptSegmentDraft::text_only(0, "hi")],
        );
        value.annotations.push(TranscriptAnnotation {
            kind: AnnotationKind::LowConfidence,
            reason: "x".into(),
            segment_index: 9,
            start: None,
            end: None,
        });
        assert_eq!(
            validate_draft(&value, None),
            Err(TruthfulnessViolation::DanglingAnnotation {
                annotation_index: 0,
                segment_index: 9
            })
        );
    }

    #[test]
    fn claim_inflation_blocks_invented_times() {
        let before = draft(
            AlignmentOrigin::None,
            vec![TranscriptSegmentDraft::text_only(0, "hi")],
        );
        let after = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(0, time(0), time(10), "hi")],
        );
        assert!(matches!(
            assert_no_claim_inflation(&before, &after),
            Err(ClaimInflation::OriginUpgraded { .. })
        ));
    }

    #[test]
    fn claim_inflation_blocks_derived_to_native() {
        let before = draft(
            AlignmentOrigin::Derived,
            vec![TranscriptSegmentDraft::timed(0, time(0), time(10), "hi")],
        );
        let after = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(0, time(0), time(10), "hi")],
        );
        assert!(matches!(
            assert_no_claim_inflation(&before, &after),
            Err(ClaimInflation::OriginUpgraded { .. })
        ));
    }

    #[test]
    fn claim_inflation_blocks_precision_upgrade_but_allows_downgrade() {
        let coarse = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(0, time(0), time(10), "hi")],
        );
        let finer = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(
                0,
                SourceTime::new(0, 16_000, TimePrecision::Sample),
                SourceTime::new(10, 16_000, TimePrecision::Sample),
                "hi",
            )],
        );
        assert!(matches!(
            assert_no_claim_inflation(&coarse, &finer),
            Err(ClaimInflation::PrecisionRaised { .. })
        ));
        assert_eq!(assert_no_claim_inflation(&finer, &coarse), Ok(()));
    }

    #[test]
    fn claim_inflation_blocks_narrowing_uncertainty() {
        let wide = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(
                0,
                time(0).with_uncertainty_ms(250),
                time(10).with_uncertainty_ms(250),
                "hi",
            )],
        );
        let narrow = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(
                0,
                time(0).with_uncertainty_ms(5),
                time(10).with_uncertainty_ms(5),
                "hi",
            )],
        );
        assert_eq!(
            assert_no_claim_inflation(&wide, &narrow),
            Err(ClaimInflation::UncertaintyNarrowed {
                before: 250,
                after: 5
            })
        );
        assert_eq!(assert_no_claim_inflation(&narrow, &wide), Ok(()));
    }

    #[test]
    fn identity_transformation_is_always_allowed() {
        let value = draft(
            AlignmentOrigin::Native,
            vec![TranscriptSegmentDraft::timed(0, time(0), time(10), "hi")],
        );
        assert_eq!(assert_no_claim_inflation(&value, &value), Ok(()));
    }
}
