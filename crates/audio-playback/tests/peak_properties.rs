//! Property tests for the peak pyramid.
//!
//! Three invariants matter and none of them is obvious from reading the fold:
//! a mip level says exactly what a scan of its children says; a query at any
//! zoom contains the answer a finer query gives for the same span; and a
//! query returns one bucket per requested column whatever the span is.

use plaudon_audio_playback::peaks::{PeakBucket, PeakBuilder, PeakParams, PeakPyramid};
use plaudon_audio_playback::time::{Micros, TimeSpan};
use proptest::prelude::*;

fn params() -> impl Strategy<Value = PeakParams> {
    (
        prop::sample::select(vec![8_000u32, 16_000, 44_100, 48_000]),
        prop::sample::select(vec![4u32, 16, 64, 256]),
        2u32..6,
        3u8..8,
    )
        .prop_map(
            |(analysis_rate_hz, base_bucket_frames, mip_factor, max_levels)| PeakParams {
                analysis_rate_hz,
                base_bucket_frames,
                mip_factor,
                max_levels,
            },
        )
}

fn samples() -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(-1.5f32..1.5, 0..6_000)
}

fn build(samples: &[f32], params: PeakParams) -> PeakPyramid {
    let mut builder = PeakBuilder::new(params);
    builder.push_samples(samples);
    builder.finish()
}

fn merge(buckets: &[PeakBucket]) -> PeakBucket {
    buckets
        .iter()
        .fold(PeakBucket::empty(), |acc, bucket| acc.merged(*bucket))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    /// Every bucket of level n+1 equals the merge of its children in level n.
    #[test]
    fn mip_levels_summarise_their_children_exactly(
        samples in samples(),
        params in params(),
    ) {
        let pyramid = build(&samples, params);
        for index in 1..pyramid.levels().len() {
            let coarse = &pyramid.levels()[index];
            let fine = &pyramid.levels()[index - 1];
            prop_assert_eq!(coarse.bucket_frames(), fine.bucket_frames() * u64::from(params.mip_factor));
            for (position, bucket) in coarse.buckets().iter().enumerate() {
                let first = position * params.mip_factor as usize;
                let last = (first + params.mip_factor as usize).min(fine.buckets().len());
                prop_assert!(first < fine.buckets().len());
                prop_assert_eq!(*bucket, merge(&fine.buckets()[first..last]));
            }
        }
    }

    /// Level 0 holds exactly the extremes of its slice of the input.
    #[test]
    fn level_zero_holds_the_true_extremes(samples in samples(), params in params()) {
        let pyramid = build(&samples, params);
        let width = params.base_bucket_frames as usize;
        for (index, bucket) in pyramid.levels()[0].buckets().iter().enumerate() {
            let from = index * width;
            let to = (from + width).min(samples.len());
            let slice = &samples[from..to];
            let low = slice.iter().copied().fold(f32::INFINITY, f32::min).clamp(-1.0, 1.0);
            let high = slice.iter().copied().fold(f32::NEG_INFINITY, f32::max).clamp(-1.0, 1.0);
            prop_assert!((bucket.min_f32() - low).abs() < 1e-3, "{bucket:?} vs {low}");
            prop_assert!((bucket.max_f32() - high).abs() < 1e-3, "{bucket:?} vs {high}");
        }
    }

    /// A column count is honoured exactly, for any span including spans that
    /// start before zero or end past the recording.
    #[test]
    fn a_query_returns_one_bucket_per_column(
        samples in samples(),
        params in params(),
        columns in 1u32..3_000,
        from_ms in -5_000i64..600_000,
        length_ms in 1i64..600_000,
    ) {
        let pyramid = build(&samples, params);
        let span = TimeSpan::new(
            Micros::from_millis(from_ms),
            Micros::from_millis(from_ms + length_ms),
        );
        prop_assert_eq!(pyramid.columns(span, columns).len(), columns as usize);
    }

    /// A coarse query contains the finer answer for the same span: a peak
    /// can never disappear as the user zooms out.
    ///
    /// Stated for column widths of at least one frame, which is every zoom a
    /// user can reach on real audio. Past that the columns-never-blank rule
    /// lets a collapsed column borrow its neighbour's bucket, as
    /// `columns_into` documents.
    #[test]
    fn zooming_out_never_loses_a_peak(
        samples in prop::collection::vec(-1.0f32..1.0, 500..6_000),
        params in params(),
        columns in 1u32..200,
    ) {
        let pyramid = build(&samples, params);
        if pyramid.total_frames() == 0 {
            return Ok(());
        }
        prop_assume!(
            u64::from(columns) * u64::from(params.mip_factor) <= pyramid.total_frames()
        );
        let span = TimeSpan::new(Micros::ZERO, pyramid.duration());
        let coarse = pyramid.columns(span, columns);
        let factor = params.mip_factor;
        let fine = pyramid.columns(span, columns * factor);
        for (index, bucket) in coarse.iter().enumerate() {
            for sub in 0..factor as usize {
                let inner = fine[index * factor as usize + sub];
                if inner == PeakBucket::SILENT {
                    continue;
                }
                prop_assert!(bucket.min <= inner.min, "column {index} sub {sub}");
                prop_assert!(bucket.max >= inner.max, "column {index} sub {sub}");
            }
        }
    }

    /// Buckets are always inside the sample range, however loud the input.
    #[test]
    fn buckets_never_leave_the_sample_range(
        samples in prop::collection::vec(-50.0f32..50.0, 0..3_000),
        params in params(),
    ) {
        let pyramid = build(&samples, params);
        for level in pyramid.levels() {
            for bucket in level.buckets() {
                prop_assert!(bucket.min_f32() >= -1.0);
                prop_assert!(bucket.max_f32() <= 1.0);
                prop_assert!(bucket.min <= bucket.max);
                prop_assert!((0.0..=1.0).contains(&bucket.amplitude()));
            }
        }
    }

    /// The level chosen for a query bounds how many buckets each column
    /// folds — the invariant that makes drawing cost depend on the width of
    /// the view and not on the length of the recording.
    #[test]
    fn the_chosen_level_bounds_the_work_per_column(
        samples in prop::collection::vec(-1.0f32..1.0, 2_000..6_000),
        params in params(),
        columns in 1u64..2_000,
    ) {
        let pyramid = build(&samples, params);
        let frames_per_column = pyramid.total_frames() / columns;
        let level = pyramid.level_for(frames_per_column);
        let bucket_frames = pyramid.levels()[level].bucket_frames();
        let touched = frames_per_column / bucket_frames + 1;
        let at_top = level + 1 == pyramid.levels().len();
        prop_assert!(
            touched <= u64::from(params.mip_factor) + 1 || at_top,
            "level {level}: {touched} buckets per column"
        );
    }

    /// Total frames always equals the samples pushed, so the duration the
    /// ruler draws matches the audio the player has.
    #[test]
    fn the_frame_count_is_the_sample_count(samples in samples(), params in params()) {
        let pyramid = build(&samples, params);
        prop_assert_eq!(pyramid.total_frames(), samples.len() as u64);
    }

    /// Feeding the builder bytes in arbitrary chunks gives the same pyramid
    /// as feeding it samples: the streaming decode splits where it likes.
    #[test]
    fn byte_chunking_never_changes_the_result(
        samples in prop::collection::vec(-1.0f32..1.0, 0..2_000),
        chunk in 1usize..37,
        params in params(),
    ) {
        let expected = build(&samples, params);
        let bytes: Vec<u8> = samples.iter().flat_map(|value| value.to_le_bytes()).collect();
        let mut builder = PeakBuilder::new(params);
        for slice in bytes.chunks(chunk) {
            builder.push_f32le(slice);
        }
        prop_assert_eq!(builder.finish(), expected);
    }
}
