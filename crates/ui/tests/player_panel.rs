//! The assembled player, end to end: a real media file, the real worker
//! pool, the real engine — only the sound card is fake.
//!
//! This is the test that proves the pieces are actually wired: the worker
//! decodes a file and wakes the view, the view draws a waveform from the
//! resulting pyramid, a click on it reaches the engine through the handle,
//! and the engine's integer clock comes back out in the next frame.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use plaudon_audio_playback::engine::{
    FakeAudioOut, FakeDevice, Player, PlayerConfig, PlayerEngine, TransportState,
};
use plaudon_audio_playback::source::SymphoniaPcmSource;
use plaudon_audio_playback::sync::PlayerHandle;
use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::worker::PeakRequest;
use plaudon_ui::player::{PeakSource, PeakStatus, PlayerPanel, PlayerStyle};

const RATE: u32 = 16_000;
const SECONDS: u32 = 8;

fn write_wav(path: &Path) {
    let frames = RATE * SECONDS;
    let data_bytes = frames * 2;
    let mut file = std::fs::File::create(path).expect("create wav");
    file.write_all(b"RIFF").expect("write");
    file.write_all(&(36 + data_bytes).to_le_bytes())
        .expect("write");
    file.write_all(b"WAVEfmt ").expect("write");
    file.write_all(&16u32.to_le_bytes()).expect("write");
    file.write_all(&1u16.to_le_bytes()).expect("write");
    file.write_all(&1u16.to_le_bytes()).expect("write");
    file.write_all(&RATE.to_le_bytes()).expect("write");
    file.write_all(&(RATE * 2).to_le_bytes()).expect("write");
    file.write_all(&2u16.to_le_bytes()).expect("write");
    file.write_all(&16u16.to_le_bytes()).expect("write");
    file.write_all(b"data").expect("write");
    file.write_all(&data_bytes.to_le_bytes()).expect("write");
    for frame in 0..frames {
        let phase = f64::from(frame) * 240.0 * std::f64::consts::TAU / f64::from(RATE);
        let sample = (phase.sin() * 0.6 * f64::from(i16::MAX)) as i16;
        file.write_all(&sample.to_le_bytes()).expect("write");
    }
    file.flush().expect("flush");
}

struct State {
    panel: PlayerPanel,
    handle: PlayerHandle,
}

#[test]
fn the_panel_decodes_draws_and_seeks_a_real_recording() {
    let dir = tempfile::tempdir().expect("tempdir");
    let media = dir.path().join("meeting.wav");
    write_wav(&media);

    let out = FakeAudioOut::new(RATE, 1);
    let device: FakeDevice = out.device();
    let mut player = Player::with_config(Box::new(out), PlayerConfig::deterministic());
    player.load(Box::new(
        SymphoniaPcmSource::open(&media).expect("open media"),
    ));
    let handle = player.handle();

    let context = egui::Context::default();
    let peaks = PeakSource::new(&context, Some(dir.path().join("peak-cache")));
    let mut panel = PlayerPanel::new(peaks, PlayerStyle::default());
    panel.peaks.request(PeakRequest::new(&media));

    let mut harness = Harness::builder()
        .with_size(vec2(640.0, 260.0))
        .build_ui_state(
            |ui, state: &mut State| {
                let snapshot = state.handle.snapshot();
                state.panel.show(ui, snapshot, &state.handle);
            },
            State { panel, handle },
        );

    // Wait for the worker: the pyramid arrives on another thread and wakes
    // the view through the waker the source installed.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        harness.run();
        player.pump();
        if matches!(harness.state().panel.peaks.status(), PeakStatus::Ready) {
            break;
        }
        assert!(Instant::now() < deadline, "the peak worker never finished");
        std::thread::sleep(Duration::from_millis(10));
    }

    let pyramid = harness
        .state()
        .panel
        .peaks
        .pyramid()
        .expect("a pyramid once ready");
    assert_eq!(pyramid.duration().as_millis(), i64::from(SECONDS) * 1_000);
    assert!(
        pyramid.levels()[0]
            .buckets()
            .iter()
            .any(|bucket| bucket.amplitude() > 0.5),
        "the decoded tone should be audible in the peaks"
    );

    // The transport reflects the engine.
    assert!(harness.query_by_label_contains("0:00 / 0:08").is_some());

    // Press play through the UI.
    harness.get_by_label("Play").click();
    harness.run();
    player.pump();
    assert_eq!(player.state(), TransportState::Playing);

    // Half a second of audio, and the clock follows.
    device.render(RATE as usize / 2);
    player.pump();
    harness.run();
    assert_eq!(player.position(), Micros::from_millis(500));
    assert!(harness.query_by_label_contains("0:00 / 0:08").is_some());

    // Click three quarters of the way along the waveform and the engine
    // moves there.
    harness.state_mut().panel.timeline.zoom_to_fit();
    harness.run();
    let node_rect = harness.get_by_label_contains(" of ").rect();
    let target = egui::Pos2::new(
        node_rect.left() + node_rect.width() * 0.75,
        node_rect.center().y,
    );
    harness.event(egui::Event::PointerMoved(target));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    harness.run();
    player.pump();

    let expected = Micros::from_millis(i64::from(SECONDS) * 750);
    let tolerance = Micros::from_millis(200);
    assert!(
        (player.position() - expected).abs() <= tolerance,
        "clicked for {expected}, engine went to {}",
        player.position()
    );

    // And the audio really is playing from there.
    device.capture(true);
    device.render(256);
    assert!(
        device.captured().iter().any(|sample| sample.abs() > 0.1),
        "no audio after the seek"
    );
}

#[test]
fn a_file_that_cannot_be_decoded_reports_a_failure_rather_than_an_empty_waveform() {
    let dir = tempfile::tempdir().expect("tempdir");
    let media = dir.path().join("notes.txt");
    std::fs::write(&media, b"this is not audio").expect("write");

    let context = egui::Context::default();
    let mut peaks = PeakSource::new(&context, None);
    peaks.request(PeakRequest::new(&media));

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        peaks.poll();
        if matches!(peaks.status(), PeakStatus::Failed(_)) {
            break;
        }
        assert!(Instant::now() < deadline, "the worker never reported");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(peaks.pyramid().is_none());
}

#[test]
fn the_panel_follows_the_playhead_while_playing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let media = dir.path().join("meeting.wav");
    write_wav(&media);

    let out = FakeAudioOut::new(RATE, 1);
    let device = out.device();
    let mut player = Player::with_config(Box::new(out), PlayerConfig::deterministic());
    player.load(Box::new(
        SymphoniaPcmSource::open(&media).expect("open media"),
    ));
    let handle = player.handle();

    let context = egui::Context::default();
    let mut panel = PlayerPanel::new(PeakSource::new(&context, None), PlayerStyle::default());
    // Zoom in so the playhead can leave the view.
    panel
        .timeline
        .set_duration(Micros::from_secs(i64::from(SECONDS)));

    let mut harness = Harness::builder()
        .with_size(vec2(640.0, 260.0))
        .build_ui_state(
            |ui, state: &mut State| {
                let snapshot = state.handle.snapshot();
                state.panel.show(ui, snapshot, &state.handle);
            },
            State { panel, handle },
        );
    harness.run();
    for _ in 0..40 {
        harness
            .state_mut()
            .panel
            .timeline
            .zoom_about(plaudon_audio_playback::timeline::Ratio::ZOOM_IN_STEP, 0.0);
    }
    harness.run();

    player.play().expect("play");
    device.render(RATE as usize * 4);
    player.pump();
    harness.run();

    let timeline = harness.state().panel.timeline;
    assert!(
        timeline.visible().contains(player.position()),
        "playhead {} outside {:?}",
        player.position(),
        timeline.visible()
    );
}
