//! Property tests for the integer timeline.
//!
//! The arithmetic is the part of the player that a unit test is least likely
//! to catch out: the failures are drift and off-by-one at zoom levels nobody
//! thought to write down. These generate the zoom levels instead.

use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::timeline::{Ratio, Scale, Timeline};
use proptest::prelude::*;

/// A duration from one second to six hours.
fn duration() -> impl Strategy<Value = Micros> {
    (1_000_000i64..21_600_000_000).prop_map(Micros::new)
}

fn viewport() -> impl Strategy<Value = u32> {
    80u32..4_000
}

/// A sequence of zoom notches and pans.
#[derive(Debug, Clone, Copy)]
enum Gesture {
    ZoomIn(f32),
    ZoomOut(f32),
    Pan(f32),
}

fn gesture() -> impl Strategy<Value = Gesture> {
    prop_oneof![
        (0.0f32..4_000.0).prop_map(Gesture::ZoomIn),
        (0.0f32..4_000.0).prop_map(Gesture::ZoomOut),
        (-500.0f32..500.0).prop_map(Gesture::Pan),
    ]
}

fn apply(timeline: &mut Timeline, gesture: Gesture) {
    match gesture {
        Gesture::ZoomIn(anchor) => timeline.zoom_about(Ratio::ZOOM_IN_STEP, anchor),
        Gesture::ZoomOut(anchor) => timeline.zoom_about(Ratio::ZOOM_OUT_STEP, anchor),
        Gesture::Pan(points) => timeline.scroll_by_points(points),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Scroll stays inside the recording however the view is driven.
    #[test]
    fn scroll_is_always_inside_the_recording(
        total in duration(),
        width in viewport(),
        gestures in prop::collection::vec(gesture(), 0..40),
    ) {
        let mut timeline = Timeline::new(total, width);
        for gesture in gestures {
            apply(&mut timeline, gesture);
            prop_assert!(timeline.scroll() >= Micros::ZERO);
            prop_assert!(timeline.scroll() <= timeline.max_scroll());
        }
    }

    /// The mapping is monotonic: a pixel further right is never earlier.
    #[test]
    fn the_pixel_to_time_mapping_is_monotonic(
        total in duration(),
        width in viewport(),
        gestures in prop::collection::vec(gesture(), 0..20),
        points in prop::collection::vec(0.0f32..4_000.0, 2..12),
    ) {
        let mut timeline = Timeline::new(total, width);
        for gesture in gestures {
            apply(&mut timeline, gesture);
        }
        let mut sorted = points;
        sorted.sort_by(f32::total_cmp);
        for pair in sorted.windows(2) {
            prop_assert!(timeline.time_at_point(pair[0]) <= timeline.time_at_point(pair[1]));
        }
    }

    /// Time at the left edge is exactly the scroll offset — no rounding slop
    /// at the one position the ruler and the waveform must agree on.
    #[test]
    fn the_left_edge_is_exactly_the_scroll_offset(
        total in duration(),
        width in viewport(),
        gestures in prop::collection::vec(gesture(), 0..20),
    ) {
        let mut timeline = Timeline::new(total, width);
        for gesture in gestures {
            apply(&mut timeline, gesture);
            prop_assert_eq!(timeline.time_at_point(0.0), timeline.scroll());
            prop_assert_eq!(timeline.point_of_time(timeline.scroll()), 0.0);
        }
    }

    /// Pixel → time → pixel lands within one pixel at every zoom level.
    #[test]
    fn the_mapping_round_trips_within_a_pixel(
        total in duration(),
        width in viewport(),
        gestures in prop::collection::vec(gesture(), 0..30),
        point in 0.0f32..4_000.0,
    ) {
        let mut timeline = Timeline::new(total, width);
        for gesture in gestures {
            apply(&mut timeline, gesture);
        }
        let time = timeline.time_at_point(point);
        let back = timeline.point_of_time(time);
        prop_assert!((back - point).abs() <= 1.0, "{point} -> {time} -> {back}");
    }

    /// Zooming holds the anchor time under the anchor pixel, to within one
    /// pixel of the new scale — unless the clamp at an edge moved the view.
    #[test]
    fn zoom_holds_its_anchor(
        total in duration(),
        width in viewport(),
        anchor in 0.0f32..2_000.0,
        inward in any::<bool>(),
        rounds in 1usize..12,
    ) {
        let mut timeline = Timeline::new(total, width);
        let ratio = if inward { Ratio::ZOOM_IN_STEP } else { Ratio::ZOOM_OUT_STEP };
        for _ in 0..rounds {
            let before = timeline.time_at_point(anchor);
            let scrolled_before = timeline.scroll();
            timeline.zoom_about(ratio, anchor);
            let after = timeline.time_at_point(anchor);
            let clamped = timeline.scroll() == Micros::ZERO
                || timeline.scroll() == timeline.max_scroll()
                || scrolled_before == Micros::ZERO
                || scrolled_before == timeline.max_scroll();
            if !clamped {
                let tolerance = timeline.scale().micros_for_points(1);
                prop_assert!(
                    (after - before).abs() <= tolerance,
                    "{before} -> {after}, tolerance {tolerance}"
                );
            }
        }
    }

    /// Panning out and back by the same pixel count returns to the same
    /// microsecond. A float scroll accumulator fails this.
    #[test]
    fn panning_is_reversible(
        total in duration(),
        width in viewport(),
        zooms in 0usize..20,
        step in 1.0f32..40.0,
        count in 1usize..200,
    ) {
        let mut timeline = Timeline::new(total, width);
        for _ in 0..zooms {
            timeline.zoom_about(Ratio::ZOOM_IN_STEP, 0.0);
        }
        // Park at the start and measure one step, so the walk can be sized
        // to finish before the far clamp: a clamped walk is not reversible
        // and says nothing about accumulation.
        timeline.set_scroll(Micros::ZERO);
        timeline.scroll_by_points(step);
        let per_step = timeline.scroll().get();
        prop_assume!(per_step > 0);
        prop_assume!(per_step.checked_mul(count as i64 + 2).is_some());
        let start = Micros::new(per_step);
        timeline.set_scroll(start);
        prop_assume!(
            start.get() + per_step * count as i64 <= timeline.max_scroll().get()
        );

        for _ in 0..count {
            timeline.scroll_by_points(step);
        }
        prop_assert_eq!(timeline.scroll().get(), start.get() + per_step * count as i64);
        for _ in 0..count {
            timeline.scroll_by_points(-step);
        }
        prop_assert_eq!(timeline.scroll(), start);
    }

    /// The visible span is exactly the viewport width at the current scale.
    #[test]
    fn the_visible_span_matches_the_viewport(
        total in duration(),
        width in viewport(),
        gestures in prop::collection::vec(gesture(), 0..20),
    ) {
        let mut timeline = Timeline::new(total, width);
        for gesture in gestures {
            apply(&mut timeline, gesture);
            let span = timeline.visible();
            prop_assert_eq!(span.start(), timeline.scroll());
            prop_assert_eq!(span.duration(), timeline.visible_duration());
            prop_assert_eq!(
                span.duration(),
                timeline.scale().micros_for_points(i64::from(width))
            );
        }
    }

    /// Scale never escapes its bounds, whatever ratio arrives.
    #[test]
    fn the_scale_stays_within_its_bounds(
        total in duration(),
        width in viewport(),
        factors in prop::collection::vec(0.001f32..1_000.0, 1..30),
    ) {
        let mut timeline = Timeline::new(total, width);
        for factor in factors {
            timeline.zoom_about(Ratio::from_f32(factor), 10.0);
            prop_assert!(timeline.scale() >= Scale::MIN);
            prop_assert!(timeline.scale() <= Scale::MAX);
        }
    }

    /// Revealing a time always leaves it visible, and asking twice is a
    /// no-op — the property auto-follow depends on.
    #[test]
    fn revealing_a_time_makes_it_visible(
        total in duration(),
        width in viewport(),
        zooms in 0usize..24,
        fraction in 0.0f64..1.0,
    ) {
        let mut timeline = Timeline::new(total, width);
        for _ in 0..zooms {
            timeline.zoom_about(Ratio::ZOOM_IN_STEP, 0.0);
        }
        let target = Micros::new((total.get() as f64 * fraction) as i64);
        timeline.scroll_to_reveal(target, 4.0);
        let visible = timeline.visible();
        prop_assert!(
            visible.contains(target) || target >= visible.end().saturating_sub(Micros::new(1)),
            "target {target} not in {visible:?}"
        );
        prop_assert!(!timeline.scroll_to_reveal(target, 4.0));
    }
}
