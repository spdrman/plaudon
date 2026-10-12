//! Chunk planning (spec §8.2 — task F1.3).
//!
//! > We will chunk long audio **only** when an engine's duration/size/memory
//! > limits demand it.
//!
//! So the planner's first job is to decide not to chunk. When it must, every
//! chunk carries its absolute sample offset and the size of the overlap it
//! shares with its predecessor, which is what makes
//! [`crate::reconcile`] able to merge rather than concatenate.

use plaudon_ai_contracts::transcript::ChunkingStrategy;
use plaudon_ai_contracts::{Known, SpeechActivity};

use crate::error::AudioError;

/// The limits an engine declares (§8.3), expressed for one canonical PCM
/// derivative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineLimits {
    pub max_file_bytes: Known<u64>,
    pub max_duration_ms: Known<u64>,
    /// Bytes one frame of the canonical derivative occupies
    /// (`channels * bytes_per_sample`).
    pub bytes_per_frame: u64,
}

impl EngineLimits {
    /// Limits for an engine that declares none: a single chunk is always used.
    #[must_use]
    pub const fn unlimited(bytes_per_frame: u64) -> Self {
        Self {
            max_file_bytes: Known::Unknown,
            max_duration_ms: Known::Unknown,
            bytes_per_frame,
        }
    }

    /// Largest chunk, in frames, that satisfies every declared limit.
    ///
    /// `None` means no limit constrains us.
    ///
    /// Note the asymmetry with capability *gating*: there, an unknown limit
    /// fails closed. Here an unknown limit means "this engine did not say it
    /// cannot take the whole file", and §8.2 forbids gratuitous splitting. The
    /// engine was already admitted by the selection policy at this point.
    #[must_use]
    pub fn max_chunk_frames(&self, sample_rate_hz: u32) -> Option<u64> {
        let mut limit: Option<u64> = None;
        if let Some(max_bytes) = self.max_file_bytes.get() {
            if let Some(frames) = max_bytes.checked_div(self.bytes_per_frame) {
                limit = Some(frames);
            }
        }
        if let Some(max_duration_ms) = self.max_duration_ms.get() {
            let frames = (u64::from(sample_rate_hz) * max_duration_ms) / 1000;
            limit = Some(limit.map_or(frames, |current| current.min(frames)));
        }
        limit.filter(|frames| *frames > 0)
    }
}

/// How boundaries are chosen when chunking is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryStrategy {
    /// Hard cuts with no shared audio. Cheapest, but a word straddling the cut
    /// is lost to both chunks.
    Fixed,
    /// Chunks share `overlap_samples` of audio so the reconciler can match and
    /// deduplicate the straddling words.
    Overlap { overlap_samples: u64 },
    /// Like `Overlap`, but each boundary is nudged into the quietest point
    /// within `search_samples`, so the cut usually lands between utterances.
    SilenceAligned {
        overlap_samples: u64,
        search_samples: u64,
        threshold_dbfs: i16,
    },
}

impl BoundaryStrategy {
    /// 5 s of overlap at the given rate: comfortably longer than any single
    /// spoken word, short enough to cost little.
    #[must_use]
    pub const fn default_overlap(sample_rate_hz: u32) -> Self {
        BoundaryStrategy::Overlap {
            overlap_samples: sample_rate_hz as u64 * 5,
        }
    }

    #[must_use]
    pub const fn overlap_samples(&self) -> u64 {
        match self {
            BoundaryStrategy::Fixed => 0,
            BoundaryStrategy::Overlap { overlap_samples }
            | BoundaryStrategy::SilenceAligned {
                overlap_samples, ..
            } => *overlap_samples,
        }
    }
}

/// One planned chunk on the absolute source timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSpec {
    pub index: usize,
    /// Absolute offset of this chunk's first sample in the canonical PCM.
    pub start_sample: u64,
    /// Absolute offset one past this chunk's last sample.
    pub end_sample: u64,
    /// How much of this chunk's head is also present in the previous chunk's
    /// tail. Zero for chunk 0 and for [`BoundaryStrategy::Fixed`].
    pub overlap_prefix_samples: u64,
}

impl ChunkSpec {
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.end_sample - self.start_sample
    }

    /// Absolute sample offset at which this chunk's *unique* content starts.
    #[must_use]
    pub const fn unique_start_sample(&self) -> u64 {
        self.start_sample + self.overlap_prefix_samples
    }
}

/// A complete plan plus the strategy record to persist with the revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkPlan {
    pub chunks: Vec<ChunkSpec>,
    pub strategy: ChunkingStrategy,
    pub sample_rate_hz: u32,
    pub total_samples: u64,
}

impl ChunkPlan {
    /// True when the plan is the no-op single chunk.
    #[must_use]
    pub fn is_single_chunk(&self) -> bool {
        matches!(self.strategy, ChunkingStrategy::SingleChunk)
    }

    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

/// Plan how to split `total_samples` of canonical PCM for an engine.
///
/// `activity` is only consulted by [`BoundaryStrategy::SilenceAligned`].
pub fn plan_chunks(
    total_samples: u64,
    sample_rate_hz: u32,
    limits: &EngineLimits,
    strategy: &BoundaryStrategy,
    activity: Option<&SpeechActivity>,
) -> Result<ChunkPlan, AudioError> {
    if total_samples == 0 {
        return Err(AudioError::Truncated {
            detail: "cannot plan chunks for zero-length audio".into(),
        });
    }
    if sample_rate_hz == 0 {
        return Err(AudioError::UnsupportedConversion(
            "sample rate must be positive".into(),
        ));
    }

    let Some(max_chunk_frames) = limits.max_chunk_frames(sample_rate_hz) else {
        return Ok(single_chunk(total_samples, sample_rate_hz));
    };
    if total_samples <= max_chunk_frames {
        return Ok(single_chunk(total_samples, sample_rate_hz));
    }

    let overlap = strategy.overlap_samples().min(max_chunk_frames / 2);
    let stride = max_chunk_frames
        .checked_sub(overlap)
        .filter(|stride| *stride > 0)
        .ok_or_else(|| {
            AudioError::UnsupportedConversion(
                "engine limit is smaller than the configured overlap".into(),
            )
        })?;

    let mut chunks: Vec<ChunkSpec> = Vec::new();
    let mut start = 0u64;
    let mut index = 0usize;
    while start < total_samples {
        let nominal_end = (start + max_chunk_frames).min(total_samples);
        let end = match strategy {
            BoundaryStrategy::SilenceAligned { search_samples, .. }
                if nominal_end < total_samples =>
            {
                align_to_silence(
                    nominal_end,
                    *search_samples,
                    max_chunk_frames,
                    start,
                    activity,
                )
            }
            _ => nominal_end,
        };
        let overlap_prefix = if index == 0 { 0 } else { overlap };
        chunks.push(ChunkSpec {
            index,
            start_sample: start,
            end_sample: end,
            overlap_prefix_samples: overlap_prefix.min(end - start),
        });
        if end >= total_samples {
            break;
        }
        // The next chunk begins `overlap` before this one ended, so the shared
        // region is exactly `overlap` samples wide.
        start = end.saturating_sub(overlap);
        // Guard against a silence alignment that failed to advance.
        if start <= chunks[index].start_sample {
            start = chunks[index].start_sample + stride;
        }
        index += 1;
    }

    let recorded = match strategy {
        BoundaryStrategy::Fixed => ChunkingStrategy::Fixed {
            chunk_samples: max_chunk_frames,
        },
        BoundaryStrategy::Overlap { .. } => ChunkingStrategy::Overlap {
            chunk_samples: max_chunk_frames,
            overlap_samples: overlap,
        },
        BoundaryStrategy::SilenceAligned { threshold_dbfs, .. } => {
            ChunkingStrategy::SilenceAligned {
                target_chunk_samples: max_chunk_frames,
                overlap_samples: overlap,
                silence_threshold_dbfs: *threshold_dbfs,
            }
        }
    };

    Ok(ChunkPlan {
        chunks,
        strategy: recorded,
        sample_rate_hz,
        total_samples,
    })
}

fn single_chunk(total_samples: u64, sample_rate_hz: u32) -> ChunkPlan {
    ChunkPlan {
        chunks: vec![ChunkSpec {
            index: 0,
            start_sample: 0,
            end_sample: total_samples,
            overlap_prefix_samples: 0,
        }],
        strategy: ChunkingStrategy::SingleChunk,
        sample_rate_hz,
        total_samples,
    }
}

/// Move a boundary into the nearest measured non-speech region.
///
/// Falls back to the nominal boundary when no silence is within reach, which
/// is why [`BoundaryStrategy::SilenceAligned`] still carries an overlap.
fn align_to_silence(
    nominal_end: u64,
    search_samples: u64,
    max_chunk_frames: u64,
    chunk_start: u64,
    activity: Option<&SpeechActivity>,
) -> u64 {
    let Some(activity) = activity else {
        return nominal_end;
    };
    let lower = nominal_end
        .saturating_sub(search_samples)
        .max(chunk_start + max_chunk_frames / 4);
    let upper = nominal_end;
    if lower >= upper {
        return nominal_end;
    }

    // Candidate boundaries are the start of each measured silence gap inside
    // the search window, preferring the one closest to the nominal end.
    let mut best: Option<u64> = None;
    let intervals = activity.speech_intervals();
    for window in intervals.windows(2) {
        let gap_start = window[0].1;
        let gap_end = window[1].0;
        if gap_end <= lower || gap_start >= upper {
            continue;
        }
        let midpoint = gap_start + (gap_end - gap_start) / 2;
        let candidate = midpoint.clamp(lower, upper);
        best = Some(match best {
            Some(current) if current.abs_diff(nominal_end) <= candidate.abs_diff(nominal_end) => {
                current
            }
            _ => candidate,
        });
    }
    // Trailing silence after the last speech interval.
    if let Some(last) = intervals.last() {
        if last.1 > lower && last.1 < upper {
            let candidate = last.1;
            best = Some(match best {
                Some(current)
                    if current.abs_diff(nominal_end) <= candidate.abs_diff(nominal_end) =>
                {
                    current
                }
                _ => candidate,
            });
        }
    }
    best.unwrap_or(nominal_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn limits_ms(max_duration_ms: u64) -> EngineLimits {
        EngineLimits {
            max_file_bytes: Known::Unknown,
            max_duration_ms: Known::Known(max_duration_ms),
            bytes_per_frame: 4,
        }
    }

    #[test]
    fn audio_within_limits_is_one_chunk() {
        let plan = plan_chunks(
            u64::from(RATE) * 30,
            RATE,
            &limits_ms(60_000),
            &BoundaryStrategy::default_overlap(RATE),
            None,
        )
        .unwrap();
        assert!(plan.is_single_chunk());
        assert_eq!(plan.chunk_count(), 1);
        assert_eq!(plan.chunks[0].start_sample, 0);
        assert_eq!(plan.chunks[0].end_sample, u64::from(RATE) * 30);
        assert_eq!(plan.chunks[0].overlap_prefix_samples, 0);
        assert_eq!(plan.strategy, ChunkingStrategy::SingleChunk);
    }

    #[test]
    fn unknown_limits_never_split() {
        let plan = plan_chunks(
            u64::from(RATE) * 3_600,
            RATE,
            &EngineLimits::unlimited(4),
            &BoundaryStrategy::default_overlap(RATE),
            None,
        )
        .unwrap();
        assert!(plan.is_single_chunk());
    }

    #[test]
    fn byte_limit_drives_the_split() {
        let limits = EngineLimits {
            max_file_bytes: Known::Known(16_000 * 4 * 10), // 10 s at 4 bytes/frame
            max_duration_ms: Known::Unknown,
            bytes_per_frame: 4,
        };
        let plan = plan_chunks(
            u64::from(RATE) * 25,
            RATE,
            &limits,
            &BoundaryStrategy::Fixed,
            None,
        )
        .unwrap();
        assert!(!plan.is_single_chunk());
        assert!(plan.chunks.iter().all(|chunk| chunk.frames() <= 160_000));
    }

    #[test]
    fn the_stricter_of_two_limits_wins() {
        let limits = EngineLimits {
            max_file_bytes: Known::Known(16_000 * 4 * 30),
            max_duration_ms: Known::Known(10_000),
            bytes_per_frame: 4,
        };
        assert_eq!(limits.max_chunk_frames(RATE), Some(160_000));
    }

    #[test]
    fn every_chunk_carries_its_absolute_offset_and_covers_the_timeline() {
        let total = u64::from(RATE) * 125;
        let plan = plan_chunks(
            total,
            RATE,
            &limits_ms(30_000),
            &BoundaryStrategy::default_overlap(RATE),
            None,
        )
        .unwrap();

        assert!(plan.chunk_count() > 1);
        assert_eq!(plan.chunks[0].start_sample, 0);
        assert_eq!(plan.chunks.last().unwrap().end_sample, total);

        // Chunks are contiguous once the overlap is accounted for, with no
        // gaps: every source sample belongs to at least one chunk.
        let mut covered_to = 0u64;
        for chunk in &plan.chunks {
            assert!(
                chunk.start_sample <= covered_to,
                "gap before chunk {}: covered to {covered_to}, chunk starts at {}",
                chunk.index,
                chunk.start_sample
            );
            covered_to = covered_to.max(chunk.end_sample);
        }
        assert_eq!(covered_to, total);
    }

    #[test]
    fn overlap_width_matches_the_declared_strategy() {
        let total = u64::from(RATE) * 125;
        let plan = plan_chunks(
            total,
            RATE,
            &limits_ms(30_000),
            &BoundaryStrategy::Overlap {
                overlap_samples: u64::from(RATE) * 5,
            },
            None,
        )
        .unwrap();

        let ChunkingStrategy::Overlap {
            overlap_samples, ..
        } = plan.strategy
        else {
            panic!("expected an overlap strategy, got {:?}", plan.strategy);
        };
        assert_eq!(overlap_samples, u64::from(RATE) * 5);

        for pair in plan.chunks.windows(2) {
            let shared = pair[0].end_sample.saturating_sub(pair[1].start_sample);
            assert_eq!(shared, overlap_samples, "chunk {} boundary", pair[0].index);
            assert_eq!(pair[1].overlap_prefix_samples, overlap_samples);
        }
        assert_eq!(plan.chunks[0].overlap_prefix_samples, 0);
    }

    #[test]
    fn fixed_strategy_has_no_overlap() {
        let plan = plan_chunks(
            u64::from(RATE) * 100,
            RATE,
            &limits_ms(30_000),
            &BoundaryStrategy::Fixed,
            None,
        )
        .unwrap();
        assert!(plan
            .chunks
            .iter()
            .all(|chunk| chunk.overlap_prefix_samples == 0));
        for pair in plan.chunks.windows(2) {
            assert_eq!(pair[0].end_sample, pair[1].start_sample);
        }
    }

    #[test]
    fn overlap_is_capped_at_half_the_chunk() {
        let plan = plan_chunks(
            u64::from(RATE) * 100,
            RATE,
            &limits_ms(10_000),
            &BoundaryStrategy::Overlap {
                overlap_samples: u64::from(RATE) * 60,
            },
            None,
        )
        .unwrap();
        let ChunkingStrategy::Overlap {
            overlap_samples,
            chunk_samples,
        } = plan.strategy
        else {
            panic!("expected overlap strategy");
        };
        assert_eq!(overlap_samples, chunk_samples / 2);
        assert!(plan.chunk_count() > 1);
    }

    #[test]
    fn silence_alignment_moves_the_boundary_into_the_gap() {
        // 60 s total; speech from 0–28 s and 32–60 s, so the gap is 28–32 s.
        let total = u64::from(RATE) * 60;
        let activity = SpeechActivity::new(
            RATE,
            total,
            vec![(0, u64::from(RATE) * 28), (u64::from(RATE) * 32, total)],
        );
        let plan = plan_chunks(
            total,
            RATE,
            &limits_ms(30_000),
            &BoundaryStrategy::SilenceAligned {
                overlap_samples: u64::from(RATE) * 2,
                search_samples: u64::from(RATE) * 5,
                threshold_dbfs: -60,
            },
            Some(&activity),
        )
        .unwrap();

        // Nominal boundary is 30 s; the gap midpoint is 30 s as well here, so
        // assert it lands strictly inside the measured silence.
        let boundary = plan.chunks[0].end_sample;
        assert!(
            boundary >= u64::from(RATE) * 28 && boundary <= u64::from(RATE) * 32,
            "boundary at {boundary} is not inside the measured gap"
        );
        assert!(matches!(
            plan.strategy,
            ChunkingStrategy::SilenceAligned { .. }
        ));
    }

    #[test]
    fn silence_alignment_falls_back_when_there_is_no_gap() {
        let total = u64::from(RATE) * 60;
        let activity = SpeechActivity::new(RATE, total, vec![(0, total)]);
        let plan = plan_chunks(
            total,
            RATE,
            &limits_ms(30_000),
            &BoundaryStrategy::SilenceAligned {
                overlap_samples: u64::from(RATE) * 2,
                search_samples: u64::from(RATE) * 5,
                threshold_dbfs: -60,
            },
            Some(&activity),
        )
        .unwrap();
        assert_eq!(plan.chunks[0].end_sample, u64::from(RATE) * 30);
    }

    #[test]
    fn zero_length_audio_is_rejected() {
        let error =
            plan_chunks(0, RATE, &limits_ms(1_000), &BoundaryStrategy::Fixed, None).unwrap_err();
        assert_eq!(error.code(), "MEDIA_TRUNCATED");
    }

    #[test]
    fn planning_is_deterministic() {
        let plan = |()| {
            plan_chunks(
                u64::from(RATE) * 250,
                RATE,
                &limits_ms(30_000),
                &BoundaryStrategy::default_overlap(RATE),
                None,
            )
            .unwrap()
        };
        assert_eq!(plan(()), plan(()));
    }

    #[test]
    fn unique_start_skips_the_shared_prefix() {
        let chunk = ChunkSpec {
            index: 1,
            start_sample: 1_000,
            end_sample: 5_000,
            overlap_prefix_samples: 400,
        };
        assert_eq!(chunk.unique_start_sample(), 1_400);
        assert_eq!(chunk.frames(), 4_000);
    }
}
