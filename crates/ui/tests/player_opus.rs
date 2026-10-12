//! P16: the player panel plays an Ogg Opus recording.
//!
//! Ogg Opus is the shape every PLAUD device `.ASR` derivative is remuxed to,
//! so this is the file the panel meets most often. The assertions are the
//! three things a user would notice if the decoder were missing: the
//! transport would be dead, the readout would say the duration is unknown,
//! and the waveform would be a decode failure instead of a tone.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use plaudon_audio_playback::engine::{FakeAudioOut, Player, PlayerConfig, PlayerEngine};
use plaudon_audio_playback::source::{PcmSource, SymphoniaPcmSource};
use plaudon_audio_playback::sync::PlayerHandle;
use plaudon_audio_playback::worker::PeakRequest;
use plaudon_ui::player::{PeakSource, PeakStatus, PlayerPanel, PlayerStyle};

/// Opus always decodes at 48 kHz, whatever rate it was encoded from.
const RATE: u32 = 48_000;

struct State {
    panel: PlayerPanel,
    handle: PlayerHandle,
}

/// A committed Opus fixture, copied into `dir` so the test owns its media
/// path, and canonicalised because the peak worker's path guard refuses a
/// `..` component outright.
fn opus_media(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/audio/valid/tone-mono-48k.opus");
    let media = dir.join("meeting.opus");
    std::fs::copy(source, &media).expect("copy the committed fixture");
    std::fs::canonicalize(&media).expect("canonical media path")
}

#[test]
fn the_panel_plays_an_ogg_opus_recording_with_a_real_duration_and_waveform() {
    let dir = tempfile::tempdir().expect("tempdir");
    let media = opus_media(dir.path());

    let out = FakeAudioOut::new(RATE, 1);
    let device = out.device();
    let mut player = Player::with_config(Box::new(out), PlayerConfig::deterministic());
    let source = SymphoniaPcmSource::open(&media).expect("an Opus recording opens");
    // The pre-skip is already gone here: one second of tone, not one second
    // plus the samples RFC 7845 §4.2 has the decoder discard.
    assert_eq!(source.total_frames(), Some(u64::from(RATE)));
    player.load(Box::new(source));
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

    // The pyramid arrives from the worker thread, so drive frames until the
    // view has it rather than guessing at a sleep.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        harness.run();
        player.pump();
        match harness.state().panel.peaks.status() {
            PeakStatus::Ready => break,
            PeakStatus::Failed(detail) => panic!("Opus peaks failed: {detail}"),
            _ => (),
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
    assert_eq!(pyramid.duration().as_millis(), 1_000);
    assert!(
        pyramid.levels()[0]
            .buckets()
            .iter()
            .any(|bucket| bucket.amplitude() > 0.05),
        "the decoded Opus tone should be audible in the peaks"
    );

    // The readout states the fixture's real length, and nothing announces a
    // failure to decode it.
    assert!(
        harness.query_by_label_contains("0:00 / 0:01").is_some(),
        "the transport should show the Opus recording's duration"
    );
    assert!(
        harness
            .query_by_label_contains("duration unknown")
            .is_none(),
        "the duration is known"
    );
    for complaint in ["cannot be played", "cannot decode", "unsupported", "failed"] {
        assert!(
            harness.query_by_label_contains(complaint).is_none(),
            "the panel announced {complaint:?} for a playable Opus recording"
        );
    }

    // The transport is live: play works and the skips are enabled, which they
    // are not when the source never stated a length.
    assert!(!harness.get_by_label("Play").accesskit_node().is_disabled());
    assert!(!harness
        .get_by_label("Back 10 s")
        .accesskit_node()
        .is_disabled());
    assert!(!harness
        .get_by_label("Forward 10 s")
        .accesskit_node()
        .is_disabled());

    harness.get_by_label("Play").click();
    harness.run();
    player.pump();
    device.capture(true);
    device.render(RATE as usize / 10);
    player.pump();
    harness.run();
    assert!(
        device.captured().iter().any(|sample| sample.abs() > 0.01),
        "the Opus recording produced silence"
    );
}
