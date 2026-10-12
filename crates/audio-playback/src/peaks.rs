//! Multiresolution waveform peaks.
//!
//! A single `(min, max)` list, the shape filmcraft uses, re-folds the same
//! buckets on every frame and goes blocky at deep zoom. Here the fold happens
//! once, into a pyramid: level 0 holds one bucket per
//! [`PeakParams::base_bucket_frames`] analysis frames and each level above it
//! merges [`PeakParams::mip_factor`] buckets of the level below. A query then
//! picks the coarsest level that still resolves one screen column, so the cost
//! of drawing is a function of the number of visible columns and never of the
//! length of the recording.

use crate::time::{Micros, TimeSpan};

/// Current layout of the peak data. Any change to the fold, the sample scaling
/// or the level arithmetic must bump this; cached files that disagree are
/// discarded rather than reinterpreted.
pub const PEAK_FORMAT_VERSION: u32 = 1;

/// Version of the decode path feeding the builder. Bumped when the decoder,
/// the resampler or the canonical profile changes in a way that could move a
/// sample, because the cache is keyed on it.
pub const DECODER_VERSION: u32 = 1;

/// How the pyramid is cut. Part of the cache key: changing any field
/// invalidates every cached pyramid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeakParams {
    /// Rate the source is decoded to before folding.
    pub analysis_rate_hz: u32,
    /// Analysis frames per level-0 bucket.
    pub base_bucket_frames: u32,
    /// Buckets of level *n* merged into one bucket of level *n + 1*.
    pub mip_factor: u32,
    /// Hard ceiling on the number of levels.
    pub max_levels: u8,
}

impl Default for PeakParams {
    /// 16 kHz mono, 64 frames (4 ms) per base bucket, ×4 per level, 10 levels.
    ///
    /// 16 kHz is the rate `plaudon_audio`'s canonical profile already produces
    /// for speech, so peak building reuses a decode path that is tested and
    /// fast. 4 ms base resolution is 250 buckets a second — 1 KiB/s, 3.6 MiB
    /// for a three-hour recording, 4.8 MiB with every mip level — and ×4 over
    /// ten levels reaches 17 s per bucket, enough to draw a 24-hour file in
    /// one screen without leaving the pyramid.
    fn default() -> Self {
        Self {
            analysis_rate_hz: 16_000,
            base_bucket_frames: 64,
            mip_factor: 4,
            max_levels: 10,
        }
    }
}

impl PeakParams {
    /// Duration one level-0 bucket covers.
    #[must_use]
    pub fn base_bucket_duration(self) -> Micros {
        Micros::from_frames(i64::from(self.base_bucket_frames), self.analysis_rate_hz)
    }

    /// Every field, in the fixed order the cache key hashes them.
    #[must_use]
    pub fn key_bytes(self) -> [u8; 13] {
        let mut out = [0u8; 13];
        out[0..4].copy_from_slice(&self.analysis_rate_hz.to_le_bytes());
        out[4..8].copy_from_slice(&self.base_bucket_frames.to_le_bytes());
        out[8..12].copy_from_slice(&self.mip_factor.to_le_bytes());
        out[12] = self.max_levels;
        out
    }

    /// Reject parameters that cannot produce a usable pyramid.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.analysis_rate_hz > 0
            && self.base_bucket_frames > 0
            && self.mip_factor >= 2
            && self.max_levels >= 1
    }
}

/// The extremes of one bucket, as 16-bit fixed point of the `[-1, 1]` sample
/// range. Four bytes a bucket instead of eight, and the quantisation error is
/// a third of a screen pixel on a 200-point-tall waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeakBucket {
    /// Most negative sample, scaled by 32767.
    pub min: i16,
    /// Most positive sample, scaled by 32767.
    pub max: i16,
}

impl PeakBucket {
    /// A bucket holding pure silence.
    pub const SILENT: Self = Self { min: 0, max: 0 };

    /// The empty accumulator: any real sample widens it.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            min: i16::MAX,
            max: i16::MIN,
        }
    }

    /// Whether nothing has been folded in yet.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.min > self.max
    }

    /// Fold one sample in `[-1, 1]`.
    pub fn push_sample(&mut self, sample: f32) {
        let quantised = quantise(sample);
        if quantised < self.min {
            self.min = quantised;
        }
        if quantised > self.max {
            self.max = quantised;
        }
    }

    /// The bucket covering both inputs. Merging is exact and associative, so a
    /// mip level says precisely what a scan of its children would.
    #[must_use]
    pub fn merged(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// Lower extreme as a float in `[-1, 1]`.
    #[must_use]
    pub fn min_f32(self) -> f32 {
        if self.is_empty() {
            0.0
        } else {
            f32::from(self.min) / 32_767.0
        }
    }

    /// Upper extreme as a float in `[-1, 1]`.
    #[must_use]
    pub fn max_f32(self) -> f32 {
        if self.is_empty() {
            0.0
        } else {
            f32::from(self.max) / 32_767.0
        }
    }

    /// Largest absolute excursion, the rectified height a bar chart draws.
    #[must_use]
    pub fn amplitude(self) -> f32 {
        self.min_f32().abs().max(self.max_f32().abs())
    }
}

fn quantise(sample: f32) -> i16 {
    if sample.is_nan() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

/// One resolution of the pyramid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeakLevel {
    bucket_frames: u64,
    buckets: Vec<PeakBucket>,
}

impl PeakLevel {
    /// Assemble a level from a bucket size and its buckets. Used by the cache
    /// decoder; the normal path is [`PeakBuilder`].
    #[must_use]
    pub const fn from_parts(bucket_frames: u64, buckets: Vec<PeakBucket>) -> Self {
        Self {
            bucket_frames,
            buckets,
        }
    }

    /// Analysis frames covered by one bucket at this level.
    #[must_use]
    pub const fn bucket_frames(&self) -> u64 {
        self.bucket_frames
    }

    /// The buckets, in time order.
    #[must_use]
    pub fn buckets(&self) -> &[PeakBucket] {
        &self.buckets
    }
}

/// A complete peak pyramid for one recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeakPyramid {
    params: PeakParams,
    total_frames: u64,
    levels: Vec<PeakLevel>,
}

impl PeakPyramid {
    /// Assemble from already-built levels. Used by the cache decoder; the
    /// normal path is [`PeakBuilder`].
    #[must_use]
    pub fn from_levels(params: PeakParams, total_frames: u64, levels: Vec<PeakLevel>) -> Self {
        Self {
            params,
            total_frames,
            levels,
        }
    }

    /// Parameters this pyramid was built with.
    #[must_use]
    pub const fn params(&self) -> PeakParams {
        self.params
    }

    /// Analysis frames folded in.
    #[must_use]
    pub const fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// Duration covered, from the frame count and the analysis rate.
    #[must_use]
    pub fn duration(&self) -> Micros {
        Micros::from_frames(self.total_frames as i64, self.params.analysis_rate_hz)
    }

    /// The levels, finest first.
    #[must_use]
    pub fn levels(&self) -> &[PeakLevel] {
        &self.levels
    }

    /// Bytes of bucket data held in memory.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.levels
            .iter()
            .map(|level| level.buckets.len() * std::mem::size_of::<PeakBucket>())
            .sum()
    }

    /// The coarsest level whose buckets still fit inside one column of
    /// `frames_per_column` analysis frames.
    #[must_use]
    pub fn level_for(&self, frames_per_column: u64) -> usize {
        let mut chosen = 0usize;
        for (index, level) in self.levels.iter().enumerate() {
            if level.bucket_frames <= frames_per_column.max(1) {
                chosen = index;
            } else {
                break;
            }
        }
        chosen
    }

    /// Fold the pyramid into exactly `columns` buckets covering `span`.
    ///
    /// Cost is `O(columns × mip_factor)`: the chosen level guarantees that no
    /// column spans more than `mip_factor + 1` buckets, so a three-hour
    /// recording costs the same as a one-minute one at the same width.
    /// Columns outside the recorded range come back empty.
    ///
    /// Every column reports at least one bucket, so a waveform zoomed past
    /// one frame per column still draws a line instead of gaps. The price is
    /// that at that zoom neighbouring columns repeat a bucket, and a column
    /// whose range has collapsed shows the bucket at its position — so a
    /// peak can appear one column over from where a finer query puts it. At
    /// one frame per column or coarser, which is every zoom level a user can
    /// reach on real audio, each column is exactly the extremes of its own
    /// range.
    pub fn columns_into(&self, span: TimeSpan, columns: u32, out: &mut Vec<PeakBucket>) {
        out.clear();
        let columns = columns.max(1);
        out.reserve(columns as usize);
        let rate = self.params.analysis_rate_hz;
        let start_frame = span.start().to_frames(rate);
        let end_frame = span.end().to_frames(rate);
        let span_frames = (end_frame - start_frame).max(0) as u64;
        let frames_per_column = span_frames / u64::from(columns);
        let level_index = self.level_for(frames_per_column);
        let Some(level) = self.levels.get(level_index) else {
            out.resize(columns as usize, PeakBucket::SILENT);
            return;
        };
        let bucket_frames = level.bucket_frames.max(1);

        for column in 0..u64::from(columns) {
            let from = start_frame + (column * span_frames / u64::from(columns)) as i64;
            let to = start_frame + ((column + 1) * span_frames / u64::from(columns)) as i64;
            out.push(self.fold(level, bucket_frames, from, to.max(from + 1)));
        }
    }

    /// Allocating form of [`PeakPyramid::columns_into`].
    #[must_use]
    pub fn columns(&self, span: TimeSpan, columns: u32) -> Vec<PeakBucket> {
        let mut out = Vec::new();
        self.columns_into(span, columns, &mut out);
        out
    }

    fn fold(&self, level: &PeakLevel, bucket_frames: u64, from: i64, to: i64) -> PeakBucket {
        if to <= 0 || from >= self.total_frames as i64 {
            return PeakBucket::SILENT;
        }
        let first = (from.max(0) as u64) / bucket_frames;
        let last = ((to.max(1) as u64).div_ceil(bucket_frames)).max(first + 1);
        let slice_end = (last as usize).min(level.buckets.len());
        let Some(slice) = level.buckets.get(first as usize..slice_end) else {
            return PeakBucket::SILENT;
        };
        let mut acc = PeakBucket::empty();
        for bucket in slice {
            acc = acc.merged(*bucket);
        }
        if acc.is_empty() {
            PeakBucket::SILENT
        } else {
            acc
        }
    }
}

/// Folds decoded mono `f32` samples into a pyramid, incrementally.
///
/// Built to be driven from a streaming decode: every level is kept up to date
/// as samples arrive, so memory is the pyramid itself and nothing else, and a
/// cancelled build throws away a partial pyramid rather than a buffered copy
/// of the recording.
#[derive(Debug)]
pub struct PeakBuilder {
    params: PeakParams,
    total_frames: u64,
    /// Accumulator and frame count of the level-0 bucket being filled.
    base: PeakBucket,
    base_frames: u32,
    /// Finished buckets, per level.
    levels: Vec<Vec<PeakBucket>>,
    /// Accumulator and child count of the bucket being filled, per level above 0.
    pending: Vec<(PeakBucket, u32)>,
    /// Trailing bytes of an `f32` sample split across two writes.
    partial: [u8; 4],
    partial_len: usize,
}

impl PeakBuilder {
    /// A builder for `params`.
    ///
    /// Invalid parameters fall back to [`PeakParams::default`] rather than
    /// failing: a waveform is never worth refusing to show the recording.
    #[must_use]
    pub fn new(params: PeakParams) -> Self {
        let params = if params.is_valid() {
            params
        } else {
            PeakParams::default()
        };
        let level_count = params.max_levels as usize;
        Self {
            params,
            total_frames: 0,
            base: PeakBucket::empty(),
            base_frames: 0,
            levels: vec![Vec::new(); level_count],
            pending: vec![(PeakBucket::empty(), 0); level_count.saturating_sub(1)],
            partial: [0; 4],
            partial_len: 0,
        }
    }

    /// Analysis frames folded in so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.total_frames
    }

    /// Fold mono samples in `[-1, 1]`.
    pub fn push_samples(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.push_sample(sample);
        }
    }

    /// Fold little-endian `f32` PCM, tolerating writes split mid-sample.
    pub fn push_f32le(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        if self.partial_len > 0 {
            let need = 4 - self.partial_len;
            let take = need.min(rest.len());
            self.partial[self.partial_len..self.partial_len + take].copy_from_slice(&rest[..take]);
            self.partial_len += take;
            rest = &rest[take..];
            if self.partial_len < 4 {
                // Still short of one sample: keep carrying it. Falling
                // through here would overwrite `partial_len` with the (zero)
                // length of the empty tail and drop the carried bytes.
                return;
            }
            self.push_sample(f32::from_le_bytes(self.partial));
            self.partial_len = 0;
        }
        let whole = rest.len() / 4 * 4;
        for word in rest[..whole].as_chunks::<4>().0 {
            self.push_sample(f32::from_le_bytes(*word));
        }
        let tail = &rest[whole..];
        self.partial[..tail.len()].copy_from_slice(tail);
        self.partial_len = tail.len();
    }

    fn push_sample(&mut self, sample: f32) {
        self.base.push_sample(sample);
        self.base_frames += 1;
        self.total_frames += 1;
        if self.base_frames >= self.params.base_bucket_frames {
            let finished = self.base;
            self.base = PeakBucket::empty();
            self.base_frames = 0;
            self.emit(0, finished);
        }
    }

    fn emit(&mut self, level: usize, bucket: PeakBucket) {
        let Some(buckets) = self.levels.get_mut(level) else {
            return;
        };
        buckets.push(bucket);
        let Some(slot) = self.pending.get_mut(level) else {
            return;
        };
        slot.0 = slot.0.merged(bucket);
        slot.1 += 1;
        if slot.1 >= self.params.mip_factor {
            let promoted = slot.0;
            *slot = (PeakBucket::empty(), 0);
            self.emit(level + 1, promoted);
        }
    }

    /// Flush the partial buckets and produce the pyramid.
    #[must_use]
    pub fn finish(mut self) -> PeakPyramid {
        if self.base_frames > 0 {
            let finished = self.base;
            self.base = PeakBucket::empty();
            self.base_frames = 0;
            self.emit(0, finished);
        }
        for level in 0..self.pending.len() {
            let (bucket, count) = self.pending[level];
            if count > 0 {
                self.pending[level] = (PeakBucket::empty(), 0);
                self.emit(level + 1, bucket);
            }
        }

        let mut levels = Vec::with_capacity(self.levels.len());
        let mut bucket_frames = u64::from(self.params.base_bucket_frames);
        for buckets in self.levels {
            let empty = buckets.is_empty();
            levels.push(PeakLevel {
                bucket_frames,
                buckets,
            });
            bucket_frames = bucket_frames.saturating_mul(u64::from(self.params.mip_factor));
            if empty {
                break;
            }
        }
        // A level nobody can query is worse than no level: `level_for` would
        // pick it and every column would fold an empty slice. Drop the empty
        // tail first, then the levels a single-bucket level already covers.
        while levels.len() > 1 && levels.last().is_some_and(|l| l.buckets.is_empty()) {
            levels.pop();
        }
        while levels.len() > 1 && levels[levels.len() - 2].buckets.len() <= 1 {
            levels.pop();
        }
        PeakPyramid {
            params: self.params,
            total_frames: self.total_frames,
            levels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(samples: &[f32], params: PeakParams) -> PeakPyramid {
        let mut builder = PeakBuilder::new(params);
        builder.push_samples(samples);
        builder.finish()
    }

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| ((i % 200) as f32 / 100.0) - 1.0).collect()
    }

    #[test]
    fn every_level_exactly_summarises_the_level_below() {
        let params = PeakParams {
            base_bucket_frames: 4,
            mip_factor: 4,
            max_levels: 5,
            ..PeakParams::default()
        };
        let pyramid = build(&ramp(4_000), params);
        for index in 1..pyramid.levels().len() {
            let coarse = &pyramid.levels()[index];
            let fine = &pyramid.levels()[index - 1];
            for (bucket_index, bucket) in coarse.buckets().iter().enumerate() {
                let first = bucket_index * params.mip_factor as usize;
                let last = (first + params.mip_factor as usize).min(fine.buckets().len());
                let expected = fine.buckets()[first..last]
                    .iter()
                    .fold(PeakBucket::empty(), |acc, b| acc.merged(*b));
                assert_eq!(*bucket, expected, "level {index} bucket {bucket_index}");
            }
        }
    }

    #[test]
    fn a_coarse_query_contains_the_fine_answer() {
        let params = PeakParams {
            base_bucket_frames: 4,
            mip_factor: 4,
            max_levels: 6,
            analysis_rate_hz: 16_000,
        };
        let pyramid = build(&ramp(64_000), params);
        let span = TimeSpan::new(Micros::ZERO, pyramid.duration());
        let coarse = pyramid.columns(span, 50);
        let fine = pyramid.columns(span, 50 * params.mip_factor);
        for (column, bucket) in coarse.iter().enumerate() {
            for sub in 0..params.mip_factor as usize {
                let inner = fine[column * params.mip_factor as usize + sub];
                if inner.is_empty() || inner == PeakBucket::SILENT {
                    continue;
                }
                assert!(bucket.min <= inner.min, "column {column}");
                assert!(bucket.max >= inner.max, "column {column}");
            }
        }
    }

    #[test]
    fn a_query_always_returns_exactly_the_requested_column_count() {
        let pyramid = build(&ramp(10_000), PeakParams::default());
        for columns in [1u32, 7, 128, 1024, 4096] {
            let span = TimeSpan::new(Micros::ZERO, pyramid.duration());
            assert_eq!(pyramid.columns(span, columns).len(), columns as usize);
        }
    }

    #[test]
    fn a_query_past_the_end_is_silent_not_a_panic() {
        let pyramid = build(&ramp(1_000), PeakParams::default());
        let span = TimeSpan::new(Micros::from_secs(100), Micros::from_secs(200));
        assert!(pyramid
            .columns(span, 32)
            .iter()
            .all(|bucket| *bucket == PeakBucket::SILENT));
    }

    #[test]
    fn the_chosen_level_bounds_the_buckets_touched_per_column() {
        let params = PeakParams::default();
        let pyramid = build(&ramp(16_000 * 60), params);
        for columns in [100u32, 500, 2000] {
            let frames_per_column = pyramid.total_frames() / u64::from(columns);
            let level = pyramid.level_for(frames_per_column);
            let bucket_frames = pyramid.levels()[level].bucket_frames();
            let touched = frames_per_column / bucket_frames + 1;
            assert!(
                touched <= u64::from(params.mip_factor) + 1,
                "columns {columns}: {touched} buckets per column"
            );
        }
    }

    #[test]
    fn byte_fed_and_sample_fed_builders_agree_across_split_writes() {
        let samples = ramp(5_000);
        let expected = build(&samples, PeakParams::default());
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut builder = PeakBuilder::new(PeakParams::default());
        // Deliberately odd chunk sizes, so samples straddle writes.
        for chunk in bytes.chunks(7) {
            builder.push_f32le(chunk);
        }
        assert_eq!(builder.finish(), expected);
    }

    #[test]
    fn silence_quantises_to_a_silent_bucket_and_clipping_saturates() {
        let pyramid = build(&vec![0.0f32; 1_000], PeakParams::default());
        assert!(pyramid.levels()[0]
            .buckets()
            .iter()
            .all(|b| *b == PeakBucket::SILENT));

        let loud = build(&[2.0f32, -2.0, f32::NAN], PeakParams::default());
        let bucket = loud.levels()[0].buckets()[0];
        assert_eq!(bucket.max, 32_767);
        assert_eq!(bucket.min, -32_767);
    }

    #[test]
    fn an_empty_recording_produces_an_empty_but_queryable_pyramid() {
        let pyramid = build(&[], PeakParams::default());
        assert_eq!(pyramid.total_frames(), 0);
        assert_eq!(pyramid.duration(), Micros::ZERO);
        let span = TimeSpan::new(Micros::ZERO, Micros::from_secs(1));
        assert_eq!(pyramid.columns(span, 10).len(), 10);
    }
}
