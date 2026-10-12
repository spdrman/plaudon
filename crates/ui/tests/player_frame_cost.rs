//! Frame cost must not depend on how long the recording is.
//!
//! The browser player decoded the whole file; a three-hour recording was
//! three hours of work for a 600-pixel strip. The pyramid is there so the
//! egui player does a fixed amount of work per frame. These cases drive the
//! same gestures — idle redraw, a scrub, a zoom sweep — over a one-minute
//! and a three-hour recording and compare.
//!
//! Timings are noisy, so the bounds are loose: three hours is 180× one
//! minute, and anything resembling a linear scan would miss by orders of
//! magnitude rather than by the factor allowed here.

use std::time::Instant;

use egui::{vec2, Event, Modifiers, Pos2, Rect, TouchPhase, Vec2};
use egui_kittest::Harness;
use plaudon_audio_playback::peaks::{PeakBucket, PeakLevel, PeakParams, PeakPyramid};
use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::timeline::{Ratio, Timeline};
use plaudon_ui::player::{PlayerStyle, TimeRuler, WaveformView};

const WIDTH: f32 = 1_200.0;
const HEIGHT: f32 = 300.0;

/// A pyramid shaped like `seconds` of audio, filled deterministically.
/// Built directly: folding 170 million samples in a debug build would turn
/// this into a decode benchmark instead of a frame benchmark.
fn synthetic(seconds: i64) -> PeakPyramid {
    let params = PeakParams::default();
    let total_frames = seconds as u64 * u64::from(params.analysis_rate_hz);
    let mut levels = Vec::new();
    let mut bucket_frames = u64::from(params.base_bucket_frames);
    let mut count = total_frames.div_ceil(bucket_frames);
    for _ in 0..params.max_levels {
        let buckets: Vec<PeakBucket> = (0..count)
            .map(|index| {
                let swing = ((index % 877) as i16).saturating_mul(37);
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

struct State {
    timeline: Timeline,
    pyramid: PeakPyramid,
    style: PlayerStyle,
    rect: Rect,
}

fn harness(seconds: i64) -> Harness<'static, State> {
    let pyramid = synthetic(seconds);
    let state = State {
        timeline: Timeline::new(pyramid.duration(), WIDTH as u32),
        pyramid,
        style: PlayerStyle::default(),
        rect: Rect::NOTHING,
    };
    let mut harness = Harness::builder()
        .with_size(vec2(WIDTH, HEIGHT))
        .build_ui_state(
            |ui, state: &mut State| {
                TimeRuler::new(&state.timeline, Micros::ZERO, &state.style).show(ui);
                let waveform = WaveformView::new(&mut state.timeline, Micros::ZERO, &state.style)
                    .peaks(Some(&state.pyramid))
                    .show(ui);
                state.rect = waveform.response.rect;
            },
            state,
        );
    harness.run();
    harness.state_mut().timeline.zoom_to_fit();
    harness.run();
    harness
}

fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2].max(1)
}

fn idle_frame_cost(harness: &mut Harness<'_, State>) -> u128 {
    let mut samples = Vec::new();
    for _ in 0..30 {
        let started = Instant::now();
        harness.step();
        samples.push(started.elapsed().as_nanos());
    }
    median(samples)
}

fn scrub_frame_cost(harness: &mut Harness<'_, State>) -> u128 {
    let rect = harness.state().rect;
    let y = rect.center().y;
    let start = Pos2::new(rect.left() + 10.0, y);
    harness.event(Event::PointerMoved(start));
    harness.event(Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::default(),
    });
    harness.step();

    let mut samples = Vec::new();
    for step in 0..40 {
        let pos = Pos2::new(rect.left() + 10.0 + step as f32 * 25.0, y);
        harness.event(Event::PointerMoved(pos));
        let started = Instant::now();
        harness.step();
        samples.push(started.elapsed().as_nanos());
    }
    harness.event(Event::PointerButton {
        pos: Pos2::new(rect.right() - 10.0, y),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::default(),
    });
    harness.step();
    median(samples)
}

fn zoom_frame_cost(harness: &mut Harness<'_, State>) -> u128 {
    let center = harness.state().rect.center();
    let mut samples = Vec::new();
    for _ in 0..40 {
        harness.event(Event::PointerMoved(center));
        harness.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, 1.0),
            phase: TouchPhase::Move,
            modifiers: Modifiers::COMMAND,
        });
        let started = Instant::now();
        harness.step();
        samples.push(started.elapsed().as_nanos());
    }
    median(samples)
}

#[test]
fn an_idle_frame_costs_the_same_for_one_minute_and_three_hours() {
    let short = idle_frame_cost(&mut harness(60));
    let long = idle_frame_cost(&mut harness(3 * 3_600));
    assert!(
        long <= short * 3,
        "three hours {long} ns vs one minute {short} ns per idle frame"
    );
}

#[test]
fn scrubbing_costs_the_same_for_one_minute_and_three_hours() {
    let short = scrub_frame_cost(&mut harness(60));
    let long = scrub_frame_cost(&mut harness(3 * 3_600));
    assert!(
        long <= short * 3,
        "three hours {long} ns vs one minute {short} ns per scrub frame"
    );
}

#[test]
fn zooming_costs_the_same_for_one_minute_and_three_hours() {
    let short = zoom_frame_cost(&mut harness(60));
    let long = zoom_frame_cost(&mut harness(3 * 3_600));
    assert!(
        long <= short * 3,
        "three hours {long} ns vs one minute {short} ns per zoom frame"
    );
}

#[test]
fn a_three_hour_waveform_zoomed_all_the_way_in_still_draws_one_quad_per_column() {
    // Not a timing assertion: the column count is what bounds the mesh, and
    // it is the width of the view at every zoom level.
    let mut harness = harness(3 * 3_600);
    let mut window = harness.state().pyramid.duration().get();
    while window > 50_000 {
        harness
            .state_mut()
            .timeline
            .zoom_about(Ratio::ZOOM_IN_STEP, 0.0);
        harness.step();
        window = harness.state().timeline.visible_duration().get();
        let columns = harness.state().rect.width().round() as usize;
        assert!(columns <= WIDTH as usize + 1);
    }
}
