//! Drawing cost must depend on the width of the view, not the length of the
//! recording.
//!
//! The React player decoded the whole file and drew at most 2,000 buckets; a
//! three-hour recording made the browser do three hours of work for a
//! 600-pixel strip. The pyramid exists so that does not happen. This test
//! pins it two ways: an exact count of the buckets a query touches (which is
//! deterministic and so can be asserted tightly), and a wall-clock ratio
//! (which is noisy and so is asserted loosely — one minute against three
//! hours is a 180× difference in length, and anything near linear would blow
//! through the bound by two orders of magnitude).

use std::time::Instant;

use plaudon_audio_playback::peaks::{PeakBucket, PeakLevel, PeakParams, PeakPyramid};
use plaudon_audio_playback::time::{Micros, TimeSpan};

/// Whether this process is running under coverage instrumentation.
///
/// `cargo llvm-cov` sets `LLVM_PROFILE_FILE` for every test process it
/// starts. Instrumentation adds a counter update to every region executed,
/// which multiplies the per-bucket constant of these queries by enough to
/// turn the loose wall-clock ratios below into noise: the scrub ratio
/// measured 11× under instrumentation and 1.2× without it on the same
/// machine. The bucket *counts* above are exact and stay asserted either
/// way; the clock-based claims are left to the uninstrumented test runs,
/// which is where they mean something.
fn instrumented() -> bool {
    std::env::var_os("LLVM_PROFILE_FILE").is_some()
}

/// A pyramid of the right *shape* for `seconds` of audio, filled with a
/// deterministic pattern. Built directly rather than decoded: the subject is
/// the query, and folding 170 million samples in a debug build would make
/// this a decode benchmark instead.
fn synthetic(seconds: i64, params: PeakParams) -> PeakPyramid {
    let total_frames = seconds as u64 * u64::from(params.analysis_rate_hz);
    let mut levels = Vec::new();
    let mut bucket_frames = u64::from(params.base_bucket_frames);
    let mut count = total_frames.div_ceil(bucket_frames);
    for _ in 0..params.max_levels {
        let buckets: Vec<PeakBucket> = (0..count)
            .map(|index| {
                let swing = ((index % 997) as i16).saturating_mul(32);
                PeakBucket {
                    min: -swing,
                    max: swing,
                }
            })
            .collect();
        levels.push(PeakLevel::from_parts(bucket_frames, buckets));
        if count <= 1 {
            break;
        }
        bucket_frames *= u64::from(params.mip_factor);
        count = count.div_ceil(u64::from(params.mip_factor));
    }
    PeakPyramid::from_levels(params, total_frames, levels)
}

/// Buckets a full-width query folds, counted the way `columns_into` does.
fn buckets_touched(pyramid: &PeakPyramid, span: TimeSpan, columns: u32) -> u64 {
    let rate = pyramid.params().analysis_rate_hz;
    let start = span.start().to_frames(rate);
    let end = span.end().to_frames(rate);
    let span_frames = (end - start).max(0) as u64;
    let level = pyramid.level_for(span_frames / u64::from(columns));
    let bucket_frames = pyramid.levels()[level].bucket_frames();
    let mut total = 0u64;
    for column in 0..u64::from(columns) {
        let from = start as u64 + column * span_frames / u64::from(columns);
        let to = start as u64 + (column + 1) * span_frames / u64::from(columns);
        let first = from / bucket_frames;
        let last = to.div_ceil(bucket_frames).max(first + 1);
        total += last - first;
    }
    total
}

fn median_nanos(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn time_full_width(pyramid: &PeakPyramid, columns: u32, rounds: usize) -> u128 {
    let span = TimeSpan::new(Micros::ZERO, pyramid.duration());
    let mut out = Vec::with_capacity(columns as usize);
    // Warm up: the first query pages the levels in.
    pyramid.columns_into(span, columns, &mut out);
    let mut samples = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        pyramid.columns_into(span, columns, &mut out);
        samples.push(started.elapsed().as_nanos().max(1));
    }
    median_nanos(samples)
}

#[test]
fn a_three_hour_query_touches_no_more_buckets_than_a_one_minute_one() {
    let params = PeakParams::default();
    let columns = 1_200u32;
    let short = synthetic(60, params);
    let long = synthetic(3 * 3_600, params);

    let short_touched = buckets_touched(
        &short,
        TimeSpan::new(Micros::ZERO, short.duration()),
        columns,
    );
    let long_touched =
        buckets_touched(&long, TimeSpan::new(Micros::ZERO, long.duration()), columns);

    let ceiling = u64::from(columns) * (u64::from(params.mip_factor) + 1);
    assert!(
        short_touched <= ceiling,
        "one minute touched {short_touched}, ceiling {ceiling}"
    );
    assert!(
        long_touched <= ceiling,
        "three hours touched {long_touched}, ceiling {ceiling}"
    );
    // Same order of work for a 180x longer recording.
    assert!(
        long_touched <= short_touched * 2,
        "three hours touched {long_touched} vs one minute {short_touched}"
    );
}

#[test]
fn full_width_query_time_is_independent_of_recording_length() {
    if instrumented() {
        eprintln!("skipped: wall-clock ratios are meaningless under coverage instrumentation");
        return;
    }
    let params = PeakParams::default();
    let columns = 1_200u32;
    let short = synthetic(60, params);
    let long = synthetic(3 * 3_600, params);

    let short_ns = time_full_width(&short, columns, 25);
    let long_ns = time_full_width(&long, columns, 25);

    assert!(
        long_ns <= short_ns * 4,
        "three hours {long_ns} ns vs one minute {short_ns} ns for {columns} columns"
    );
}

#[test]
fn scrubbing_cost_is_independent_of_recording_length() {
    if instrumented() {
        eprintln!("skipped: wall-clock ratios are meaningless under coverage instrumentation");
        return;
    }
    // A scrub is a sequence of queries over a moving window, which is what
    // the waveform does while the pointer is down.
    let params = PeakParams::default();
    let columns = 900u32;
    let short = synthetic(60, params);
    let long = synthetic(3 * 3_600, params);

    let scrub = |pyramid: &PeakPyramid| -> u128 {
        let total = pyramid.duration().get();
        let window = total / 20;
        let mut out = Vec::new();
        let mut samples = Vec::new();
        for step in 0..40i64 {
            let from = Micros::new(step * (total - window) / 40);
            let span = TimeSpan::from_len(from, Micros::new(window));
            let started = Instant::now();
            pyramid.columns_into(span, columns, &mut out);
            samples.push(started.elapsed().as_nanos().max(1));
        }
        median_nanos(samples)
    };

    let short_ns = scrub(&short);
    let long_ns = scrub(&long);
    assert!(
        long_ns <= short_ns * 4,
        "three hours {long_ns} ns vs one minute {short_ns} ns per scrub frame"
    );
}

#[test]
fn zooming_all_the_way_in_stays_cheap_on_a_three_hour_recording() {
    if instrumented() {
        eprintln!("skipped: wall-clock ratios are meaningless under coverage instrumentation");
        return;
    }
    let params = PeakParams::default();
    let columns = 1_200u32;
    let long = synthetic(3 * 3_600, params);
    let mut out = Vec::new();

    // From the whole recording down to a 50 ms window, one notch at a time.
    let mut window = long.duration().get();
    let mut worst = 0u128;
    while window > 50_000 {
        let span = TimeSpan::from_len(Micros::new(long.duration().get() / 3), Micros::new(window));
        let started = Instant::now();
        long.columns_into(span, columns, &mut out);
        worst = worst.max(started.elapsed().as_nanos());
        assert_eq!(out.len(), columns as usize);
        window = window * 4 / 5;
    }
    // Generous, because this runs in a debug build on shared CI hardware;
    // the point is that no zoom level degenerates into a full scan.
    assert!(
        worst < 50_000_000,
        "worst zoom step took {worst} ns for {columns} columns"
    );
}
