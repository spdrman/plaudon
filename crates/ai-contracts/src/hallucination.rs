//! Non-destructive hallucination detection (spec §8.6A).
//!
//! > We will detect low-confidence/non-speech hallucinations using a measured
//! > evaluation corpus, not silently delete questionable text.
//!
//! Everything here produces [`TranscriptAnnotation`]s. Nothing here edits,
//! reorders or removes a single character of provider text — that is the whole
//! point. Whisper-family decoders are known to emit confident-looking text
//! over silence and to fall into repetition loops; we surface both rather than
//! pretending they did not happen.

use crate::transcript::{
    AnnotationKind, Confidence, SourceTime, TranscriptAnnotation, TranscriptDraft,
    TranscriptSegmentDraft,
};

/// Measured speech/non-speech map over the source timeline.
///
/// Produced by the audio pipeline's energy analysis (`plaudon_audio::silence`),
/// never by the engine itself: using the engine's own output to judge the
/// engine would be circular.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechActivity {
    sample_rate_hz: u32,
    /// Half-open `[start, end)` sample ranges measured as speech, sorted and
    /// non-overlapping.
    speech_intervals: Vec<(u64, u64)>,
    total_samples: u64,
}

impl SpeechActivity {
    /// `speech_intervals` is normalised: sorted, merged, and clamped to
    /// `total_samples`.
    #[must_use]
    pub fn new(sample_rate_hz: u32, total_samples: u64, speech_intervals: Vec<(u64, u64)>) -> Self {
        let mut intervals: Vec<(u64, u64)> = speech_intervals
            .into_iter()
            .map(|(start, end)| (start.min(total_samples), end.min(total_samples)))
            .filter(|(start, end)| end > start)
            .collect();
        intervals.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(intervals.len());
        for (start, end) in intervals {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        Self {
            sample_rate_hz,
            speech_intervals: merged,
            total_samples,
        }
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    #[must_use]
    pub const fn total_samples(&self) -> u64 {
        self.total_samples
    }

    #[must_use]
    pub fn speech_intervals(&self) -> &[(u64, u64)] {
        &self.speech_intervals
    }

    /// Fraction of `[start, end)` measured as speech, in `0.0..=1.0`.
    ///
    /// An empty or inverted range reports `1.0` ("nothing to suspect"), because
    /// an absent range is a timestamp problem, not a hallucination signal.
    #[must_use]
    pub fn speech_ratio(&self, start: u64, end: u64) -> f32 {
        if end <= start {
            return 1.0;
        }
        let span = end - start;
        let mut overlap = 0u64;
        for &(interval_start, interval_end) in &self.speech_intervals {
            if interval_end <= start {
                continue;
            }
            if interval_start >= end {
                break;
            }
            overlap += interval_end.min(end) - interval_start.max(start);
        }
        overlap as f32 / span as f32
    }
}

/// Tunable thresholds. Defaults are the documented starting point; §17.2/§17.6
/// require they be re-derived from the measured corpus before any quality
/// claim is published, and the values live in one place so that recalibration
/// is a data change, not a code change.
#[derive(Debug, Clone, PartialEq)]
pub struct HallucinationPolicy {
    /// A phrase repeated at least this many times consecutively (across
    /// segments or inside one segment) is flagged.
    pub max_consecutive_repeats: usize,
    /// A timed segment whose measured speech ratio is at or below this value
    /// is flagged as suspected non-speech.
    pub max_non_speech_ratio: f32,
    /// Minimum characters a repeated phrase must have before repetition is
    /// considered suspicious. Guards against flagging "yes. yes. yes.".
    pub min_repeat_phrase_chars: usize,
    /// Per-scale confidence floor. A segment whose confidence is below the
    /// floor *for its own scale* is flagged. Scales are never compared
    /// across engines (§8.5).
    pub confidence_floors: Vec<(String, f32)>,
}

impl Default for HallucinationPolicy {
    fn default() -> Self {
        Self {
            max_consecutive_repeats: 3,
            max_non_speech_ratio: 0.10,
            min_repeat_phrase_chars: 4,
            // whisper.cpp average log-probability: values at or below -1.0 are
            // the documented "very unsure" band used by whisper's own
            // `no_speech`/`logprob` filters.
            confidence_floors: vec![("whisper.avg_logprob".to_string(), -1.0)],
        }
    }
}

impl HallucinationPolicy {
    fn floor_for(&self, confidence: &Confidence) -> Option<f32> {
        self.confidence_floors
            .iter()
            .find(|(scale, _)| *scale == confidence.scale)
            .map(|(_, floor)| *floor)
    }
}

/// Run every detector and return the annotations to attach to the draft.
///
/// `activity` is optional: without a measured speech map we simply skip the
/// non-speech detector rather than guessing.
#[must_use]
pub fn detect_hallucinations(
    draft: &TranscriptDraft,
    activity: Option<&SpeechActivity>,
    policy: &HallucinationPolicy,
) -> Vec<TranscriptAnnotation> {
    let mut annotations = Vec::new();
    detect_cross_segment_repetition(draft, policy, &mut annotations);
    detect_intra_segment_repetition(draft, policy, &mut annotations);
    if let Some(activity) = activity {
        detect_non_speech(draft, activity, policy, &mut annotations);
    }
    detect_low_confidence(draft, policy, &mut annotations);
    annotations.sort_by_key(|annotation| (annotation.segment_index, annotation.kind.as_str()));
    annotations
}

fn normalise(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn detect_cross_segment_repetition(
    draft: &TranscriptDraft,
    policy: &HallucinationPolicy,
    out: &mut Vec<TranscriptAnnotation>,
) {
    let normalised: Vec<String> = draft.segments.iter().map(|s| normalise(&s.text)).collect();
    let mut run_start = 0usize;
    while run_start < normalised.len() {
        let mut run_end = run_start + 1;
        while run_end < normalised.len() && normalised[run_end] == normalised[run_start] {
            run_end += 1;
        }
        let run_length = run_end - run_start;
        if run_length >= policy.max_consecutive_repeats
            && normalised[run_start].len() >= policy.min_repeat_phrase_chars
        {
            for index in run_start..run_end {
                out.push(annotation(
                    draft,
                    index,
                    AnnotationKind::SuspectedRepetitionLoop,
                    format!(
                        "segment text repeats identically across {run_length} consecutive segments"
                    ),
                ));
            }
        }
        run_start = run_end;
    }
}

fn detect_intra_segment_repetition(
    draft: &TranscriptDraft,
    policy: &HallucinationPolicy,
    out: &mut Vec<TranscriptAnnotation>,
) {
    for (index, segment) in draft.segments.iter().enumerate() {
        let normalised = normalise(&segment.text);
        let words: Vec<&str> = normalised.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        // Look for a repeating unit of 1..=6 words tiling the segment.
        for unit in 1..=6usize.min(words.len()) {
            let pattern = &words[..unit];
            if pattern.join(" ").len() < policy.min_repeat_phrase_chars {
                continue;
            }
            let mut repeats = 0usize;
            let mut cursor = 0usize;
            while cursor + unit <= words.len() && &words[cursor..cursor + unit] == pattern {
                repeats += 1;
                cursor += unit;
            }
            if repeats >= policy.max_consecutive_repeats && cursor == words.len() {
                out.push(annotation(
                    draft,
                    index,
                    AnnotationKind::SuspectedRepetitionLoop,
                    format!("segment consists of the same {unit}-word phrase repeated {repeats} times"),
                ));
                break;
            }
        }
    }
}

fn detect_non_speech(
    draft: &TranscriptDraft,
    activity: &SpeechActivity,
    policy: &HallucinationPolicy,
    out: &mut Vec<TranscriptAnnotation>,
) {
    for (index, segment) in draft.segments.iter().enumerate() {
        if segment.text.trim().is_empty() {
            continue;
        }
        let (Some(start), Some(end)) = (segment.start, segment.end) else {
            continue;
        };
        if start.sample_rate_hz != activity.sample_rate_hz() {
            continue;
        }
        let ratio = activity.speech_ratio(start.sample_offset, end.sample_offset);
        if ratio <= policy.max_non_speech_ratio {
            out.push(annotation(
                draft,
                index,
                AnnotationKind::SuspectedNonSpeech,
                format!(
                    "text claimed over a region measured as {:.0}% speech",
                    ratio * 100.0
                ),
            ));
        }
    }
}

fn detect_low_confidence(
    draft: &TranscriptDraft,
    policy: &HallucinationPolicy,
    out: &mut Vec<TranscriptAnnotation>,
) {
    for (index, segment) in draft.segments.iter().enumerate() {
        let Some(confidence) = &segment.confidence else {
            continue;
        };
        let Some(floor) = policy.floor_for(confidence) else {
            continue;
        };
        if confidence.value <= floor {
            out.push(annotation(
                draft,
                index,
                AnnotationKind::LowConfidence,
                format!(
                    "confidence {:.3} on scale `{}` is at or below the calibrated floor {:.3}",
                    confidence.value, confidence.scale, floor
                ),
            ));
        }
    }
}

fn annotation(
    draft: &TranscriptDraft,
    segment_index: usize,
    kind: AnnotationKind,
    reason: String,
) -> TranscriptAnnotation {
    let segment: Option<&TranscriptSegmentDraft> = draft.segments.get(segment_index);
    TranscriptAnnotation {
        kind,
        reason,
        segment_index,
        start: segment.and_then(|s| s.start),
        end: segment.and_then(|s| s.end),
    }
}

/// Convenience: attach detected annotations to a draft without touching text.
pub fn annotate(
    draft: &mut TranscriptDraft,
    activity: Option<&SpeechActivity>,
    policy: &HallucinationPolicy,
) {
    let detected = detect_hallucinations(draft, activity, policy);
    draft.annotations.extend(detected);
}

/// Helper for tests and callers: the source-time range a draft covers.
#[must_use]
pub fn draft_span(draft: &TranscriptDraft) -> Option<(SourceTime, SourceTime)> {
    let start = draft.segments.iter().filter_map(|s| s.start).min_by_key(|t| t.sample_offset)?;
    let end = draft.segments.iter().filter_map(|s| s.end).max_by_key(|t| t.sample_offset)?;
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{AlignmentOrigin, TimePrecision};

    fn time(offset: u64) -> SourceTime {
        SourceTime::new(offset, 16_000, TimePrecision::Segment)
    }

    fn timed_draft(segments: Vec<TranscriptSegmentDraft>) -> TranscriptDraft {
        let mut draft = TranscriptDraft::text_only("whisper.cpp", "tiny.en", segments);
        draft.alignment_origin = AlignmentOrigin::Native;
        draft
    }

    #[test]
    fn repeated_segments_are_flagged_not_removed() {
        let draft = timed_draft(vec![
            TranscriptSegmentDraft::timed(time(0), time(16_000), "thank you."),
            TranscriptSegmentDraft::timed(time(16_000), time(32_000), "Thank you!"),
            TranscriptSegmentDraft::timed(time(32_000), time(48_000), "thank you"),
        ]);
        let annotations = detect_hallucinations(&draft, None, &HallucinationPolicy::default());
        assert_eq!(annotations.len(), 3);
        assert!(annotations
            .iter()
            .all(|a| a.kind == AnnotationKind::SuspectedRepetitionLoop));
        // Text is untouched.
        assert_eq!(draft.segments.len(), 3);
        assert_eq!(draft.segments[0].text, "thank you.");
    }

    #[test]
    fn two_repeats_are_not_flagged() {
        let draft = timed_draft(vec![
            TranscriptSegmentDraft::timed(time(0), time(16_000), "thank you."),
            TranscriptSegmentDraft::timed(time(16_000), time(32_000), "thank you."),
        ]);
        assert!(detect_hallucinations(&draft, None, &HallucinationPolicy::default()).is_empty());
    }

    #[test]
    fn intra_segment_loop_is_flagged() {
        let draft = timed_draft(vec![TranscriptSegmentDraft::timed(
            time(0),
            time(160_000),
            "subscribe to the channel subscribe to the channel subscribe to the channel subscribe to the channel",
        )]);
        let annotations = detect_hallucinations(&draft, None, &HallucinationPolicy::default());
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].kind, AnnotationKind::SuspectedRepetitionLoop);
    }

    #[test]
    fn text_over_measured_silence_is_flagged() {
        let draft = timed_draft(vec![TranscriptSegmentDraft::timed(
            time(0),
            time(16_000),
            "Thanks for watching!",
        )]);
        // Entire minute measured as non-speech.
        let activity = SpeechActivity::new(16_000, 960_000, vec![]);
        let annotations =
            detect_hallucinations(&draft, Some(&activity), &HallucinationPolicy::default());
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].kind, AnnotationKind::SuspectedNonSpeech);
        assert_eq!(annotations[0].segment_index, 0);
        assert_eq!(annotations[0].start, Some(time(0)));
    }

    #[test]
    fn text_over_measured_speech_is_not_flagged() {
        let draft = timed_draft(vec![TranscriptSegmentDraft::timed(
            time(0),
            time(16_000),
            "Thanks for watching!",
        )]);
        let activity = SpeechActivity::new(16_000, 960_000, vec![(0, 16_000)]);
        assert!(
            detect_hallucinations(&draft, Some(&activity), &HallucinationPolicy::default())
                .is_empty()
        );
    }

    #[test]
    fn low_confidence_uses_matching_scale_only() {
        let mut low = TranscriptSegmentDraft::timed(time(0), time(16_000), "mumble");
        low.confidence = Some(Confidence::new(-1.5, "whisper.avg_logprob"));
        let mut other_scale = TranscriptSegmentDraft::timed(time(16_000), time(32_000), "clear");
        other_scale.confidence = Some(Confidence::new(-1.5, "apple.confidence"));
        let draft = timed_draft(vec![low, other_scale]);
        let annotations = detect_hallucinations(&draft, None, &HallucinationPolicy::default());
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].segment_index, 0);
        assert_eq!(annotations[0].kind, AnnotationKind::LowConfidence);
    }

    #[test]
    fn speech_ratio_is_fraction_of_overlap() {
        let activity = SpeechActivity::new(16_000, 1000, vec![(0, 250), (500, 750)]);
        assert!((activity.speech_ratio(0, 1000) - 0.5).abs() < 1e-6);
        assert!((activity.speech_ratio(250, 500) - 0.0).abs() < 1e-6);
        assert!((activity.speech_ratio(0, 250) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn overlapping_intervals_are_merged() {
        let activity = SpeechActivity::new(16_000, 1000, vec![(500, 700), (0, 600)]);
        assert_eq!(activity.speech_intervals(), &[(0, 700)]);
    }

    #[test]
    fn annotate_appends_without_mutating_text() {
        let mut draft = timed_draft(vec![
            TranscriptSegmentDraft::timed(time(0), time(16_000), "thank you."),
            TranscriptSegmentDraft::timed(time(16_000), time(32_000), "thank you."),
            TranscriptSegmentDraft::timed(time(32_000), time(48_000), "thank you."),
        ]);
        let before: Vec<String> = draft.segments.iter().map(|s| s.text.clone()).collect();
        annotate(&mut draft, None, &HallucinationPolicy::default());
        let after: Vec<String> = draft.segments.iter().map(|s| s.text.clone()).collect();
        assert_eq!(before, after);
        assert_eq!(draft.annotations.len(), 3);
    }
}
