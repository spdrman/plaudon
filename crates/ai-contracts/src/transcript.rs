//! Transcript draft contracts and the timestamp-truthfulness model (§8.5, §8.6A).
//!
//! The central invariant of this module: **a time that was never measured is
//! absent, not zero.** `Option<SourceTime>` is used everywhere a timestamp may
//! be missing, and [`AlignmentOrigin::None`] forbids times outright.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where a segment's timestamps came from (§8.6A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentOrigin {
    /// Timestamps reported directly by the engine for this audio.
    Native,
    /// Timestamps produced by a separate alignment pass over engine text.
    /// Must be labelled as derived in data and UI payloads.
    Derived,
    /// Engine produced text only. No seek times may exist.
    None,
}

impl AlignmentOrigin {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AlignmentOrigin::Native => "native",
            AlignmentOrigin::Derived => "derived",
            AlignmentOrigin::None => "none",
        }
    }

    /// Whether timestamps are permitted to exist at all under this origin.
    #[inline]
    #[must_use]
    pub const fn permits_times(self) -> bool {
        !matches!(self, AlignmentOrigin::None)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "native" => Some(AlignmentOrigin::Native),
            "derived" => Some(AlignmentOrigin::Derived),
            "none" => Some(AlignmentOrigin::None),
            _ => None,
        }
    }
}

/// Declared precision of a source time (§8.6A: "engine-provided precision").
///
/// Ordered coarsest-first so that `max`/comparison means "no better than".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimePrecision {
    /// Coarse: segment-level only, typically whole or half seconds.
    Segment,
    /// Word-level alignment reported by the engine.
    Word,
    /// Exact sample index known.
    Sample,
}

/// A time offset into the *source* media, carried as a sample offset plus the
/// sample rate it was measured against, so no rounding happens before storage
/// (§8.2: "capture source sample offset and uncertainty, not just formatted
/// wall-clock times").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceTime {
    /// Absolute offset from the start of the source media, in samples at
    /// `sample_rate_hz`.
    pub sample_offset: u64,
    pub sample_rate_hz: u32,
    pub precision: TimePrecision,
    /// Half-width of the uncertainty interval in milliseconds, when the engine
    /// or the reconciliation pass can state one.
    pub uncertainty_ms: Option<u32>,
}

impl SourceTime {
    #[must_use]
    pub const fn new(sample_offset: u64, sample_rate_hz: u32, precision: TimePrecision) -> Self {
        Self {
            sample_offset,
            sample_rate_hz,
            precision,
            uncertainty_ms: None,
        }
    }

    #[must_use]
    pub const fn with_uncertainty_ms(mut self, uncertainty_ms: u32) -> Self {
        self.uncertainty_ms = Some(uncertainty_ms);
        self
    }

    /// Milliseconds from the start of the source media, rounded down.
    ///
    /// This is a *presentation* conversion; storage keeps the sample offset.
    #[must_use]
    pub const fn as_millis(&self) -> u64 {
        if self.sample_rate_hz == 0 {
            return 0;
        }
        (self.sample_offset * 1000) / self.sample_rate_hz as u64
    }

    /// Shift by an absolute chunk offset expressed in samples at the same rate.
    #[must_use]
    pub const fn shifted_by_samples(mut self, samples: u64) -> Self {
        self.sample_offset += samples;
        self
    }
}

/// Word-level alignment inside a segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordAlignment {
    pub text: String,
    pub start: SourceTime,
    pub end: SourceTime,
    pub confidence: Option<Confidence>,
}

/// An engine-specific confidence value.
///
/// §8.5: "Confidence is an engine-specific field and must not be compared
/// across vendors without calibration." The scale label travels with the value
/// so no code can silently compare two different scales.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Confidence {
    pub value: f32,
    /// Opaque engine-specific scale identifier, e.g. `whisper.avg_logprob`.
    pub scale: String,
}

impl Confidence {
    pub fn new(value: f32, scale: impl Into<String>) -> Self {
        Self {
            value,
            scale: scale.into(),
        }
    }

    /// Comparable only within one scale.
    #[must_use]
    pub fn comparable_with(&self, other: &Confidence) -> bool {
        self.scale == other.scale
    }
}

impl Eq for Confidence {}

/// One ordered segment of an engine result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptSegmentDraft {
    /// Absent when the revision's alignment origin is [`AlignmentOrigin::None`].
    pub start: Option<SourceTime>,
    pub end: Option<SourceTime>,
    /// Normalised segment text exactly as produced; never edited by correction
    /// passes (those create a new revision, §8.5).
    pub text: String,
    pub confidence: Option<Confidence>,
    pub speaker_id: Option<String>,
    pub words: Vec<WordAlignment>,
}

impl TranscriptSegmentDraft {
    /// A text-only segment: the only constructor that is valid under
    /// [`AlignmentOrigin::None`].
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            start: None,
            end: None,
            text: text.into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
        }
    }

    pub fn timed(start: SourceTime, end: SourceTime, text: impl Into<String>) -> Self {
        Self {
            start: Some(start),
            end: Some(end),
            text: text.into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
        }
    }

    #[must_use]
    pub fn has_times(&self) -> bool {
        self.start.is_some() || self.end.is_some() || !self.words.is_empty()
    }

    /// Coarsest precision claimed anywhere in this segment, if any.
    #[must_use]
    pub fn claimed_precision(&self) -> Option<TimePrecision> {
        self.start
            .iter()
            .chain(self.end.iter())
            .map(|t| t.precision)
            .chain(
                self.words
                    .iter()
                    .flat_map(|w| [w.start.precision, w.end.precision]),
            )
            .max()
    }
}

/// How the pipeline split the source before transcription (§8.2, persisted per
/// §8.5 "chunking strategy").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChunkingStrategy {
    /// Audio fitted inside every engine limit; a single chunk was used.
    SingleChunk,
    Fixed {
        chunk_samples: u64,
    },
    Overlap {
        chunk_samples: u64,
        overlap_samples: u64,
    },
    SilenceAligned {
        target_chunk_samples: u64,
        overlap_samples: u64,
        silence_threshold_dbfs: i16,
    },
}

/// Deterministic record of the canonicalisation applied before transcription
/// (§8.5 "normalization profile").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizationProfile {
    pub sample_rate_hz: u32,
    pub channel_policy: String,
    pub sample_format: String,
    /// SHA-256 (hex) of the canonical PCM bytes handed to the engine.
    pub pcm_sha256: String,
}

/// A machine transcription result, before it becomes a persisted revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptDraft {
    /// Applies to the whole draft; individual segments may not exceed it.
    pub alignment_origin: AlignmentOrigin,
    pub segments: Vec<TranscriptSegmentDraft>,
    /// BCP-47 tag when the engine reports one. Absent is not "en".
    pub language: Option<String>,
    pub engine_id: String,
    pub model_id: String,
    pub model_version: Option<String>,
    /// Engine parameters that affect output, recorded verbatim for
    /// repeatability (§8.5, F2.1).
    pub engine_params: BTreeMap<String, String>,
    pub chunking_strategy: ChunkingStrategy,
    pub normalization_profile: Option<NormalizationProfile>,
    /// Non-destructive quality flags (hallucination suspicion, overlap
    /// ambiguity). Never used to remove text.
    pub annotations: Vec<TranscriptAnnotation>,
    /// Backend actually used by a local engine, e.g. `metal`/`cpu` (F2.5).
    pub backend: Option<String>,
}

impl TranscriptDraft {
    pub fn text_only(
        engine_id: impl Into<String>,
        model_id: impl Into<String>,
        segments: Vec<TranscriptSegmentDraft>,
    ) -> Self {
        Self {
            alignment_origin: AlignmentOrigin::None,
            segments,
            language: None,
            engine_id: engine_id.into(),
            model_id: model_id.into(),
            model_version: None,
            engine_params: BTreeMap::new(),
            chunking_strategy: ChunkingStrategy::SingleChunk,
            normalization_profile: None,
            annotations: Vec::new(),
            backend: None,
        }
    }

    /// Concatenated segment text, used for diagnostics and equality fixtures.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A non-destructive observation attached to a draft or revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptAnnotation {
    pub kind: AnnotationKind,
    /// Human-readable, non-secret explanation.
    pub reason: String,
    /// Index into [`TranscriptDraft::segments`].
    pub segment_index: usize,
    pub start: Option<SourceTime>,
    pub end: Option<SourceTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    /// Same phrase repeated consecutively well beyond natural speech.
    SuspectedRepetitionLoop,
    /// Text claimed over a region measured as non-speech.
    SuspectedNonSpeech,
    /// Engine confidence far below its own distribution for this run.
    LowConfidence,
    /// Overlap reconciliation could not decide between two candidate texts.
    AmbiguousOverlap,
}

impl AnnotationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AnnotationKind::SuspectedRepetitionLoop => "suspected_repetition_loop",
            AnnotationKind::SuspectedNonSpeech => "suspected_non_speech",
            AnnotationKind::LowConfidence => "low_confidence",
            AnnotationKind::AmbiguousOverlap => "ambiguous_overlap",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_origin_forbids_times() {
        assert!(!AlignmentOrigin::None.permits_times());
        assert!(AlignmentOrigin::Native.permits_times());
        assert!(AlignmentOrigin::Derived.permits_times());
    }

    #[test]
    fn text_only_segment_has_no_times() {
        let segment = TranscriptSegmentDraft::text_only("hello");
        assert!(!segment.has_times());
        assert_eq!(segment.claimed_precision(), None);
    }

    #[test]
    fn source_time_keeps_sample_offset_not_rounded_millis() {
        let time = SourceTime::new(16_001, 16_000, TimePrecision::Sample);
        assert_eq!(time.sample_offset, 16_001);
        assert_eq!(time.as_millis(), 1000);
    }

    #[test]
    fn precision_orders_coarsest_first() {
        assert!(TimePrecision::Segment < TimePrecision::Word);
        assert!(TimePrecision::Word < TimePrecision::Sample);
    }

    #[test]
    fn confidence_is_not_comparable_across_scales() {
        let a = Confidence::new(-0.2, "whisper.avg_logprob");
        let b = Confidence::new(0.9, "apple.confidence");
        assert!(!a.comparable_with(&b));
        assert!(a.comparable_with(&Confidence::new(-0.9, "whisper.avg_logprob")));
    }

    #[test]
    fn alignment_origin_serialises_snake_case() {
        assert_eq!(
            serde_json::to_string(&AlignmentOrigin::Derived).unwrap(),
            "\"derived\""
        );
        assert_eq!(AlignmentOrigin::parse("none"), Some(AlignmentOrigin::None));
        assert_eq!(AlignmentOrigin::parse("guessed"), None);
    }

    #[test]
    fn absent_times_serialise_as_null_never_zero() {
        let segment = TranscriptSegmentDraft::text_only("hi");
        let json = serde_json::to_value(&segment).unwrap();
        assert!(json["start"].is_null());
        assert!(json["end"].is_null());
    }
}
