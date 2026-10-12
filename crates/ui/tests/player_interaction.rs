//! Interaction tests for the player, driven through `egui_kittest`.
//!
//! Every case is a user action — a click at a position, a drag, a wheel
//! notch, a key — and an assertion about the command the player emits or the
//! view state it leaves behind. Nothing here renders to a GPU, so the suite
//! runs anywhere; the frame-cost suite is separate.
//!
//! Commands accumulate in the fixture rather than being cleared per frame:
//! `Harness::run` keeps stepping until nothing asks for a repaint, so the
//! frame that handles an event is rarely the last one.

use egui::{vec2, Event, Key, Modifiers, PointerButton, Pos2, Rect, TouchPhase, Vec2};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use plaudon_audio_playback::engine::{Speed, TransportState, Volume};
use plaudon_audio_playback::peaks::{PeakBuilder, PeakParams, PeakPyramid};
use plaudon_audio_playback::regions::{SpeakerRegion, SpeakerRegions};
use plaudon_audio_playback::sync::{PlaybackSnapshot, PlayerCommand};
use plaudon_audio_playback::time::{Micros, TimeSpan};
use plaudon_audio_playback::timeline::{Ratio, Timeline};
use plaudon_domain::ids::SpeakerId;
use plaudon_ui::player::{PlayerStyle, TimeRuler, TransportBar, WaveformView};

const WIDTH: f32 = 600.0;
const HEIGHT: f32 = 320.0;
/// One minute, so a point is a known number of microseconds.
const DURATION: Micros = Micros::new(60_000_000);

struct Fixture {
    timeline: Timeline,
    pyramid: PeakPyramid,
    regions: Option<SpeakerRegions>,
    snapshot: PlaybackSnapshot,
    commands: Vec<PlayerCommand>,
    waveform_rect: Rect,
    style: PlayerStyle,
}

impl Fixture {
    fn new(duration: Micros) -> Self {
        Self {
            timeline: Timeline::new(duration, WIDTH as u32),
            pyramid: tone_pyramid(duration),
            regions: None,
            snapshot: PlaybackSnapshot {
                position: Micros::ZERO,
                duration: Some(duration),
                state: TransportState::Paused,
                speed: Speed::NORMAL,
                volume: Volume::FULL,
                revision: 0,
            },
            commands: Vec::new(),
            waveform_rect: Rect::NOTHING,
            style: PlayerStyle::default(),
        }
    }
}

/// A pyramid of the right duration with audible content.
fn tone_pyramid(duration: Micros) -> PeakPyramid {
    let params = PeakParams::default();
    let frames = duration.to_frames(params.analysis_rate_hz).max(0) as usize;
    let mut builder = PeakBuilder::new(params);
    let block: Vec<f32> = (0..1_024)
        .map(|index| ((index % 64) as f32 / 32.0) - 1.0)
        .collect();
    let mut written = 0usize;
    while written < frames {
        let take = block.len().min(frames - written);
        builder.push_samples(&block[..take]);
        written += take;
    }
    builder.finish()
}

fn harness(fixture: Fixture) -> Harness<'static, Fixture> {
    let mut harness = Harness::builder()
        .with_size(vec2(WIDTH, HEIGHT))
        .build_ui_state(
            |ui, state: &mut Fixture| {
                let ruler =
                    TimeRuler::new(&state.timeline, state.snapshot.position, &state.style).show(ui);
                state.commands.extend(ruler.commands);

                let regions = state.regions.as_ref();
                let waveform =
                    WaveformView::new(&mut state.timeline, state.snapshot.position, &state.style)
                        .peaks(Some(&state.pyramid))
                        .regions(regions)
                        .show(ui);
                state.waveform_rect = waveform.response.rect;
                state.commands.extend(waveform.commands);

                state
                    .commands
                    .extend(TransportBar::new(state.snapshot).show(ui).commands);
                state
                    .commands
                    .extend(plaudon_ui::player::keyboard_commands(ui.ctx()));
                if let Some(command) =
                    plaudon_ui::player::speed_shortcut(ui.ctx(), state.snapshot.speed)
                {
                    state.commands.push(command);
                }
            },
            fixture,
        );
    harness.run();
    // The first frame is what tells the timeline how wide the view really
    // is; fit the recording to that width so a click at a fraction of the
    // view is a click at the same fraction of the recording.
    harness.state_mut().timeline.zoom_to_fit();
    harness.run();
    harness.state_mut().commands.clear();
    harness
}

/// Run to quiescence and take everything the player asked for.
fn settle(harness: &mut Harness<'_, Fixture>) -> Vec<PlayerCommand> {
    harness.run();
    std::mem::take(&mut harness.state_mut().commands)
}

fn click_at(harness: &Harness<'_, Fixture>, pos: Pos2) {
    harness.event(Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        });
    }
}

fn waveform_rect(harness: &Harness<'_, Fixture>) -> Rect {
    harness.state().waveform_rect
}

fn seek_command(commands: &[PlayerCommand]) -> Option<Micros> {
    commands.iter().find_map(|command| match command {
        PlayerCommand::Seek(position) => Some(*position),
        _ => None,
    })
}

/// Two points' worth of time: the tolerance for "the click landed here".
fn point_tolerance(harness: &Harness<'_, Fixture>) -> i64 {
    harness
        .state()
        .timeline
        .scale()
        .micros_for_points(2)
        .get()
        .max(1)
}

#[test]
fn clicking_the_waveform_seeks_to_the_clicked_time() {
    let mut harness = harness(Fixture::new(DURATION));
    let rect = waveform_rect(&harness);
    click_at(
        &harness,
        Pos2::new(rect.left() + rect.width() * 0.25, rect.center().y),
    );
    let tolerance = point_tolerance(&harness);
    let commands = settle(&mut harness);

    let seek = seek_command(&commands).expect("a seek command");
    let expected = DURATION.get() / 4;
    assert!(
        (seek.get() - expected).abs() <= tolerance,
        "seek {seek} expected about {expected}"
    );
}

#[test]
fn clicking_the_ruler_seeks_too() {
    let mut harness = harness(Fixture::new(DURATION));
    let rect = waveform_rect(&harness);
    // The ruler sits directly above the waveform.
    let ruler_y = rect.top() - 4.0;
    click_at(
        &harness,
        Pos2::new(rect.left() + rect.width() * 0.5, ruler_y),
    );
    let tolerance = point_tolerance(&harness);
    let commands = settle(&mut harness);

    let seek = seek_command(&commands).expect("a seek command");
    let expected = DURATION.get() / 2;
    assert!((seek.get() - expected).abs() <= tolerance, "seek {seek}");
}

#[test]
fn dragging_the_waveform_scrubs_continuously() {
    let mut harness = harness(Fixture::new(DURATION));
    let rect = waveform_rect(&harness);
    let start = Pos2::new(rect.left() + 50.0, rect.center().y);

    harness.event(Event::PointerMoved(start));
    harness.event(Event::PointerButton {
        pos: start,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::default(),
    });
    let _ = settle(&mut harness);

    let mut seen = Vec::new();
    for step in 1..=4 {
        let pos = Pos2::new(start.x + 60.0 * step as f32, start.y);
        harness.event(Event::PointerMoved(pos));
        let commands = settle(&mut harness);
        if let Some(seek) = seek_command(&commands) {
            seen.push(seek);
        }
    }
    harness.event(Event::PointerButton {
        pos: Pos2::new(start.x + 240.0, start.y),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::default(),
    });
    let _ = settle(&mut harness);

    assert!(
        seen.len() >= 3,
        "expected a seek per drag step, got {seen:?}"
    );
    assert!(
        seen.windows(2).all(|pair| pair[1] > pair[0]),
        "scrub did not advance monotonically: {seen:?}"
    );
}

#[test]
fn the_waveform_reports_itself_as_a_slider_with_a_position_value() {
    let mut harness = harness(Fixture::new(DURATION));
    harness.state_mut().snapshot.position = Micros::from_secs(30);
    harness.run();
    let node = harness.get_by_label_contains(" of ").accesskit_node();
    assert_eq!(node.role(), egui::accesskit::Role::Slider);
    assert!(
        node.label().is_some_and(|label| label == "0:30 of 1:00"),
        "label was {:?}",
        node.label()
    );
}

#[test]
fn ctrl_wheel_zooms_about_the_pointer_and_plain_wheel_pans() {
    let mut harness = harness(Fixture::new(DURATION));
    let center = waveform_rect(&harness).center();
    let before = harness.state().timeline.scale();

    harness.event(Event::PointerMoved(center));
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: Vec2::new(0.0, 3.0),
        phase: TouchPhase::Move,
        modifiers: Modifiers::COMMAND,
    });
    let _ = settle(&mut harness);
    let zoomed = harness.state().timeline.scale();
    assert!(
        zoomed < before,
        "{zoomed:?} should be finer than {before:?}"
    );

    let scrolled_before = harness.state().timeline.scroll();
    harness.event(Event::PointerMoved(center));
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: Vec2::new(0.0, -3.0),
        phase: TouchPhase::Move,
        modifiers: Modifiers::NONE,
    });
    let _ = settle(&mut harness);
    assert!(
        harness.state().timeline.scroll() > scrolled_before,
        "a plain wheel should pan forward"
    );
    assert_eq!(
        harness.state().timeline.scale(),
        zoomed,
        "a plain wheel must not change the zoom"
    );
}

#[test]
fn keyboard_on_the_focused_waveform_skips_and_zooms() {
    let mut harness = harness(Fixture::new(DURATION));
    click_at(&harness, waveform_rect(&harness).center());
    let _ = settle(&mut harness);

    harness.key_press(Key::ArrowRight);
    let commands = settle(&mut harness);
    assert!(
        commands.contains(&PlayerCommand::Skip(Micros::from_secs(5))),
        "{commands:?}"
    );

    harness.key_press(Key::ArrowLeft);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Skip(Micros::from_secs(-5))));

    harness.key_press(Key::PageDown);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Skip(Micros::from_secs(30))));

    harness.key_press(Key::PageUp);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Skip(Micros::from_secs(-30))));

    harness.key_press(Key::End);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Seek(DURATION)));

    harness.key_press(Key::Home);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Seek(Micros::ZERO)));

    let before = harness.state().timeline.scale();
    harness.key_press(Key::Plus);
    let _ = settle(&mut harness);
    assert!(harness.state().timeline.scale() < before);
    harness.key_press(Key::Num0);
    let _ = settle(&mut harness);
    assert_eq!(harness.state().timeline.scale(), before);
}

#[test]
fn the_transport_buttons_emit_the_react_players_commands() {
    let mut harness = harness(Fixture::new(DURATION));

    harness.get_by_label("Play").click();
    let commands = settle(&mut harness);
    assert!(
        commands.contains(&PlayerCommand::TogglePlay),
        "{commands:?}"
    );

    harness.get_by_label("Back 10 s").click();
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Skip(Micros::from_secs(-10))));

    harness.get_by_label("Forward 10 s").click();
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Skip(Micros::from_secs(10))));

    harness.get_by_label("1.5×").click();
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::SetSpeed(Speed::ONE_AND_A_HALF)));
}

#[test]
fn the_play_button_becomes_pause_while_playing() {
    let mut harness = harness(Fixture::new(DURATION));
    assert!(harness.query_by_label("Pause").is_none());
    harness.state_mut().snapshot.state = TransportState::Playing;
    harness.run();
    assert!(harness.query_by_label("Play").is_none());
    harness.get_by_label("Pause").click();
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::TogglePlay));
}

#[test]
fn window_shortcuts_match_the_react_player() {
    let mut harness = harness(Fixture::new(DURATION));

    harness.key_press(Key::Space);
    let commands = settle(&mut harness);
    assert!(
        commands.contains(&PlayerCommand::TogglePlay),
        "{commands:?}"
    );

    harness.key_press(Key::J);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Skip(Micros::from_secs(-10))));

    harness.key_press(Key::L);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Skip(Micros::from_secs(10))));

    harness.key_press(Key::K);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::Pause));

    harness.key_press(Key::Period);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::SetSpeed(Speed::ONE_AND_A_QUARTER)));

    harness.key_press(Key::Comma);
    let commands = settle(&mut harness);
    assert!(commands.contains(&PlayerCommand::SetSpeed(Speed::THREE_QUARTERS)));
}

#[test]
fn an_unknown_duration_disables_the_skips_and_says_so() {
    let mut fixture = Fixture::new(DURATION);
    fixture.snapshot.duration = None;
    let harness = harness(fixture);

    assert!(harness
        .query_by_label_contains("duration unknown")
        .is_some());
    let back = harness.get_by_label("Back 10 s").accesskit_node();
    assert!(back.is_disabled(), "Back 10 s must be disabled");
    let forward = harness.get_by_label("Forward 10 s").accesskit_node();
    assert!(forward.is_disabled(), "Forward 10 s must be disabled");
    // Play stays available: a source can play without stating a length.
    assert!(!harness.get_by_label("Play").accesskit_node().is_disabled());
}

#[test]
fn the_volume_slider_reports_a_change() {
    let mut harness = harness(Fixture::new(DURATION));
    let rect = harness.get_by_label("Volume").rect();
    click_at(
        &harness,
        Pos2::new(rect.left() + rect.width() * 0.25, rect.center().y),
    );
    let commands = settle(&mut harness);
    let volume = commands
        .iter()
        .find_map(|command| match command {
            PlayerCommand::SetVolume(volume) => Some(*volume),
            _ => None,
        })
        .expect("a volume command");
    assert!(volume < Volume::FULL, "{volume:?}");
}

#[test]
fn speaker_regions_are_drawn_without_disturbing_the_waveform() {
    let mut fixture = Fixture::new(DURATION);
    fixture.regions = Some(SpeakerRegions::from_turns(vec![
        SpeakerRegion::new(
            Some(SpeakerId::new()),
            "Ada",
            TimeSpan::new(Micros::ZERO, Micros::from_secs(20)),
        ),
        SpeakerRegion::new(
            Some(SpeakerId::new()),
            "Grace",
            TimeSpan::new(Micros::from_secs(20), Micros::from_secs(60)),
        ),
    ]));
    let mut harness = harness(fixture);
    let rect = waveform_rect(&harness);
    // The band adds height, and click-to-seek still lands on the right time.
    assert!(rect.height() > PlayerStyle::default().waveform_height);
    click_at(
        &harness,
        Pos2::new(rect.left() + rect.width() * 0.5, rect.center().y),
    );
    let tolerance = point_tolerance(&harness);
    let commands = settle(&mut harness);
    let seek = seek_command(&commands).expect("a seek");
    assert!((seek.get() - DURATION.get() / 2).abs() <= tolerance);
}

#[test]
fn zoomed_in_clicks_still_land_on_the_time_under_the_pointer() {
    let mut harness = harness(Fixture::new(DURATION));
    // Zoom well in and scroll into the middle of the recording.
    for _ in 0..18 {
        harness
            .state_mut()
            .timeline
            .zoom_about(Ratio::ZOOM_IN_STEP, 0.0);
    }
    harness
        .state_mut()
        .timeline
        .center_on(Micros::from_secs(42));
    harness.run();
    harness.state_mut().commands.clear();

    let rect = waveform_rect(&harness);
    let point = rect.left() + rect.width() * 0.75;
    let expected = harness.state().timeline.time_at_point(point - rect.left());
    let tolerance = point_tolerance(&harness);
    click_at(&harness, Pos2::new(point, rect.center().y));
    let commands = settle(&mut harness);

    let seek = seek_command(&commands).expect("a seek");
    assert!(
        (seek.get() - expected.get()).abs() <= tolerance,
        "seek {seek} expected {expected}"
    );
}

#[test]
fn a_recording_with_no_peaks_yet_still_renders_and_seeks() {
    let mut fixture = Fixture::new(DURATION);
    fixture.pyramid = PeakBuilder::new(PeakParams::default()).finish();
    let mut harness = harness(fixture);
    let rect = waveform_rect(&harness);
    click_at(
        &harness,
        Pos2::new(rect.left() + rect.width() * 0.1, rect.center().y),
    );
    let commands = settle(&mut harness);
    assert!(seek_command(&commands).is_some());
}

#[test]
fn a_zero_height_viewport_does_not_panic() {
    let mut harness = Harness::builder()
        .with_size(vec2(10.0, 10.0))
        .build_ui_state(
            |ui, state: &mut Fixture| {
                WaveformView::new(&mut state.timeline, Micros::ZERO, &state.style)
                    .peaks(Some(&state.pyramid))
                    .height(0.0)
                    .show(ui);
            },
            Fixture::new(DURATION),
        );
    harness.run();
}
