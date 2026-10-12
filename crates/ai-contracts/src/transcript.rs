//! Source-time model and revision metadata for engine drafts (§8.2, §8.5,
//! §8.6A).
//!
//! Draft timestamps are **sample offsets**, not milliseconds. §8.2 requires
//! that we "capture source sample offset and uncertainty, not just formatted
//! wall-clock times", and chunk reconciliation (F1.3) depends on it: rounding
//! two chunk-local times to milliseconds before adding an absolute offset
//! loses exactly the precision needed to decide whether two overlapping
//! segments are the same words.
//!
//! Milliseconds appear only at the persistence and presentation boundary, via
//! [`SourceTime::as_source_time_ms`].

use serde::{Deserialize, Serialize};

use plaudon_domain::media::Sha256Digest;
use plaudon_domain::timerange::SourceTimeMs;
use plaudon_domain::transcript::Confidence;

/// Declared precision of a source time (§8.6A "engine-provided precision").
///
/// Ordered coarsest-first, so `a > b` reads as "a is a stronger claim than b".
/// [`crate::truthfulness::assert_no_claim_inflation`] relies on that ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimePrecision {
    /// Segment-level only, typically whole or half seconds.
    Segment,
    /// Word-level alignment reported by the engine.
    Word,
    /// Exact sample index known.
    Sample,
}

/// An offset into the source media, expressed in samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceTime {
    /// Absolute offset from the start of the source media, in samples at
    /// `sample_rate_hz`.
    pub sample_offset: u64,
    /// Rate `sample_offset` is expressed in.
    pub sample_rate_hz: u32,
    /// How precisely this offset is known.
    pub precision: TimePrecision,
    /// Half-width of the uncertainty interval in milliseconds, when the engine
    /// or the reconciliation pass can state one.
    pub uncertainty_ms: Option<u32>,
}

impl SourceTime {
    /// Construct an offset at a stated precision.
    #[must_use]
    pub const fn new(sample_offset: u64, sample_rate_hz: u32, precision: TimePrecision) -> Self {
        Self {
            sample_offset,
            sample_rate_hz,
            precision,
            uncertainty_ms: None,
        }
    }

    /// Build from a millisecond offset reported by an engine.
    ///
    /// Precision is [`TimePrecision::Segment`]: a millisecond figure from an
    /// engine is a rounded claim, not a sample index.
    #[must_use]
    pub const fn from_millis(millis: u64, sample_rate_hz: u32) -> Self {
        Self {
            sample_offset: (millis * sample_rate_hz as u64) / 1000,
            sample_rate_hz,
            precision: TimePrecision::Segment,
            uncertainty_ms: None,
        }
    }

    /// Attach a stated uncertainty.
    #[must_use]
    pub const fn with_uncertainty_ms(mut self, uncertainty_ms: u32) -> Self {
        self.uncertainty_ms = Some(uncertainty_ms);
        self
    }

    /// Widen the stated uncertainty.
    ///
    /// Narrowing is deliberately not offered: no transformation may strengthen
    /// a claim (§8.6A).
    #[must_use]
    pub fn widened_to(mut self, uncertainty_ms: u32) -> Self {
        self.uncertainty_ms = Some(self.uncertainty_ms.unwrap_or(0).max(uncertainty_ms));
        self
    }

    /// Milliseconds from the start of the source media, rounded down.
    #[must_use]
    pub const fn as_millis(&self) -> u64 {
        if self.sample_rate_hz == 0 {
            return 0;
        }
        (self.sample_offset * 1000) / self.sample_rate_hz as u64
    }

    /// The persistence/presentation form.
    #[must_use]
    pub const fn as_source_time_ms(&self) -> SourceTimeMs {
        SourceTimeMs::new(self.as_millis())
    }

    /// Shift onto the absolute source timeline by a chunk offset.
    #[must_use]
    pub const fn shifted_by_samples(mut self, samples: u64) -> Self {
        self.sample_offset += samples;
        self
    }
}

/// Word-level alignment inside a draft segment, at sample precision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftWordAlignment {
    /// The word as the engine normalized it.
    pub text: String,
    /// Start offset of the word.
    pub start: SourceTime,
    /// End offset of the word.
    pub end: SourceTime,
    /// Engine-scoped confidence, when reported.
    pub confidence: Option<Confidence>,
}

/// How the pipeline split the source before transcription (§8.2), persisted
/// with the revision (§8.5 "chunking strategy").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChunkingStrategy {
    /// Audio fitted inside every engine limit; one chunk was used.
    #[default]
    SingleChunk,
    /// Hard cuts with no shared audio.
    Fixed {
        /// Chunk length in samples.
        chunk_samples: u64,
    },
    /// Cuts with a shared region so straddling words can be reconciled.
    Overlap {
        /// Chunk length in samples.
        chunk_samples: u64,
        /// Shared region between neighbours, in samples.
        overlap_samples: u64,
    },
    /// Overlapping cuts nudged into measured silence.
    SilenceAligned {
        /// Nominal chunk length in samples.
        target_chunk_samples: u64,
        /// Shared region between neighbours, in samples.
        overlap_samples: u64,
        /// Level at or below which a frame counted as silence.
        silence_threshold_dbfs: i16,
    },
}

impl ChunkingStrategy {
    /// Stable short label persisted in `transcript_revisions.chunking_strategy`.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::SingleChunk => "single_chunk",
            Self::Fixed { .. } => "fixed",
            Self::Overlap { .. } => "overlap",
            Self::SilenceAligned { .. } => "silence_aligned",
        }
    }
}

/// Deterministic record of the canonicalisation applied before transcription
/// (§8.5 "normalization profile").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizationProfile {
    /// Canonical sample rate handed to the engine.
    pub sample_rate_hz: u32,
    /// Channel policy label, e.g. `preserve_all` or `downmix_mono`.
    pub channel_policy: String,
    /// Sample format label, e.g. `f32le`.
    pub sample_format: String,
    /// Digest of the canonical PCM bytes handed to the engine. Two runs that
    /// agree on this digest fed the engine identical audio.
    pub pcm_sha256: Sha256Digest,
}

impl NormalizationProfile {
    /// Stable short label persisted in
    /// `transcript_revisions.normalization_profile`.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{}hz/{}/{}",
            self.sample_rate_hz, self.channel_policy, self.sample_format
        )
    }
}

/// A non-destructive observation attached to a draft or revision.
///
/// Annotations never remove or edit text; they sit beside it (§8.6A).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptAnnotation {
    /// What was observed.
    pub kind: AnnotationKind,
    /// Human-readable, non-secret explanation shown next to the text.
    pub reason: String,
    /// Index of the segment the annotation refers to.
    pub segment_index: usize,
    /// Start of the annotated region, when the segment is timed.
    pub start: Option<SourceTime>,
    /// End of the annotated region, when the segment is timed.
    pub end: Option<SourceTime>,
}

/// Categories of non-destructive quality observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    /// The same phrase repeats far beyond natural speech.
    SuspectedRepetitionLoop,
    /// Text is claimed over a region measured as non-speech.
    SuspectedNonSpeech,
    /// Engine confidence is at or below the calibrated floor for its own scale.
    LowConfidence,
    /// Overlap reconciliation could not decide between two candidate texts;
    /// both were kept.
    AmbiguousOverlap,
}

impl AnnotationKind {
    /// Stable serialised label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SuspectedRepetitionLoop => "suspected_repetition_loop",
            Self::SuspectedNonSpeech => "suspected_non_speech",
            Self::LowConfidence => "low_confidence",
            Self::AmbiguousOverlap => "ambiguous_overlap",
        }
    }

    /// Parse a stable label.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "suspected_repetition_loop" => Some(Self::SuspectedRepetitionLoop),
            "suspected_non_speech" => Some(Self::SuspectedNonSpeech),
            "low_confidence" => Some(Self::LowConfidence),
            "ambiguous_overlap" => Some(Self::AmbiguousOverlap),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_time_keeps_the_sample_offset_milliseconds_would_lose() {
        let time = SourceTime::new(16_001, 16_000, TimePrecision::Sample);
        assert_eq!(time.sample_offset, 16_001);
        assert_eq!(time.as_millis(), 1000);
        assert_ne!(
            SourceTime::from_millis(1000, 16_000).sample_offset,
            time.sample_offset
        );
    }

    #[test]
    fn precision_orders_coarsest_first() {
        assert!(TimePrecision::Segment < TimePrecision::Word);
        assert!(TimePrecision::Word < TimePrecision::Sample);
    }

    #[test]
    fn uncertainty_only_widens() {
        let time = SourceTime::new(0, 16_000, TimePrecision::Segment).with_uncertainty_ms(50);
        assert_eq!(time.widened_to(10).uncertainty_ms, Some(50));
        assert_eq!(time.widened_to(200).uncertainty_ms, Some(200));
    }

    #[test]
    fn shifting_is_additive_on_the_sample_axis() {
        let time = SourceTime::new(100, 16_000, TimePrecision::Segment);
        assert_eq!(time.shifted_by_samples(1_000).sample_offset, 1_100);
        assert_eq!(time.shifted_by_samples(0), time);
    }

    #[test]
    fn zero_sample_rate_does_not_divide_by_zero() {
        assert_eq!(
            SourceTime::new(1_000, 0, TimePrecision::Segment).as_millis(),
            0
        );
    }

    #[test]
    fn chunking_strategy_labels_are_stable() {
        assert_eq!(ChunkingStrategy::SingleChunk.label(), "single_chunk");
        assert_eq!(
            ChunkingStrategy::Fixed { chunk_samples: 1 }.label(),
            "fixed"
        );
        assert_eq!(
            ChunkingStrategy::Overlap {
                chunk_samples: 1,
                overlap_samples: 1
            }
            .label(),
            "overlap"
        );
        assert_eq!(
            ChunkingStrategy::SilenceAligned {
                target_chunk_samples: 1,
                overlap_samples: 1,
                silence_threshold_dbfs: -60,
            }
            .label(),
            "silence_aligned"
        );
    }

    #[test]
    fn normalization_profile_label_is_deterministic() {
        let profile = NormalizationProfile {
            sample_rate_hz: 16_000,
            channel_policy: "downmix_mono".into(),
            sample_format: "f32le".into(),
            pcm_sha256: Sha256Digest::of(b""),
        };
        assert_eq!(profile.label(), "16000hz/downmix_mono/f32le");
    }

    #[test]
    fn annotation_kinds_round_trip_through_their_labels() {
        for kind in [
            AnnotationKind::SuspectedRepetitionLoop,
            AnnotationKind::SuspectedNonSpeech,
            AnnotationKind::LowConfidence,
            AnnotationKind::AmbiguousOverlap,
        ] {
            assert_eq!(AnnotationKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(AnnotationKind::parse("invented"), None);
    }

    #[test]
    fn source_time_round_trips_through_json() {
        let time = SourceTime::new(123, 48_000, TimePrecision::Word).with_uncertainty_ms(7);
        let json = serde_json::to_string(&time).unwrap();
        assert_eq!(serde_json::from_str::<SourceTime>(&json).unwrap(), time);
    }
}
