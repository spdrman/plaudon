//! P16: the three duration answers for an Ogg Opus recording agree.
//!
//! A recording's length is computed three times by three different code
//! paths — the playback cursor, `plaudon_audio`'s probe, and the peak
//! pyramid — and a user sees all three at once: the transport's total, the
//! library row's duration, and the width of the waveform. Opus is the case
//! where they can silently disagree, because an Ogg Opus granule position
//! counts the pre-skip samples RFC 7845 §4.2 requires the decoder to discard.
//! If one path subtracted the pre-skip and another did not, the playhead
//! would reach the end of the waveform before the clock reached the total.

use std::path::{Path, PathBuf};

use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::Known;
use plaudon_audio_playback::generate::build_peaks;
use plaudon_audio_playback::peaks::PeakParams;
use plaudon_audio_playback::source::{PcmSource, SymphoniaPcmSource};
use plaudon_domain::ids::JobId;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Every fixture `scripts/make-audio-fixtures.sh` encodes with libopus is one
/// second of a sine tone; they differ only in the rate libopus resampled from
/// on the way in and in the channel count.
const FIXTURES: &[(&str, u16)] = &[
    ("tone-mono-8k.opus", 1),
    ("tone-mono-16k.opus", 1),
    ("tone-mono-48k.opus", 1),
    ("tone-stereo-48k.opus", 2),
];

/// The nominal content of every fixture: `duration=1` in the generator.
const NOMINAL_MS: i64 = 1_000;
/// Opus always decodes at 48 kHz whatever the source rate was.
const OPUS_RATE: u32 = 48_000;

/// Tolerance between the three answers: one Opus frame.
///
/// The encoder pads the final packet out to a whole 20 ms frame and records
/// the padding in the granule position, so the container can overstate the
/// content by up to one frame. The playback source and the peak pyramid both
/// decode through Symphonia with gapless trimming on and so land on the exact
/// content length; `probe_path` deliberately leaves gapless off — it reports
/// what the container declares, minus only the pre-skip — so it is allowed to
/// read up to one frame long. Anything beyond that is a real disagreement.
const TOLERANCE_MS: i64 = 20;

fn fixture_path(name: &str) -> PathBuf {
    // Canonicalised: the peak path's guard refuses a `..` component outright,
    // which would make this test pass for the wrong reason.
    std::fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/audio/valid")
            .join(name),
    )
    .expect("committed fixture")
}

/// What the container itself declares: the granule-derived frame count with
/// no gapless trimming, and the pre-skip the header asks for.
fn container_claim(path: &Path) -> (u64, u32) {
    let file = std::fs::File::open(path).expect("open fixture");
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("opus");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: false,
                ..FormatOptions::default()
            },
            &MetadataOptions::default(),
        )
        .expect("probe fixture");
    let params = &probed
        .format
        .default_track()
        .expect("a default track")
        .codec_params;
    (
        params.n_frames.expect("a granule-derived frame count"),
        params.delay.expect("an Opus pre-skip"),
    )
}

#[test]
fn the_player_the_probe_and_the_pyramid_agree_on_an_opus_recordings_length() {
    for &(name, channels) in FIXTURES {
        let path = fixture_path(name);
        let (raw_frames, pre_skip) = container_claim(&path);
        assert!(pre_skip > 0, "{name}: libopus always writes a pre-skip");

        // (a) The playback cursor.
        let source = SymphoniaPcmSource::open(&path).expect("opus opens for playback");
        assert_eq!(source.sample_rate(), OPUS_RATE, "{name}");
        assert_eq!(source.channels(), channels, "{name}");
        let played_frames = source.total_frames().expect("a known length");
        assert!(
            played_frames < raw_frames,
            "{name}: {played_frames} frames reported from a {raw_frames}-frame granule count, \
             so the {pre_skip}-frame pre-skip was not removed"
        );
        assert_eq!(
            played_frames,
            u64::from(OPUS_RATE) * NOMINAL_MS as u64 / 1_000,
            "{name}: one second of tone"
        );
        let played_ms = source.duration().expect("a known duration").as_millis();
        assert_eq!(played_ms, NOMINAL_MS, "{name}");

        // (b) `plaudon_audio`'s probe, the number the library screen shows.
        let profile = plaudon_audio::probe::probe_path(&path).expect("opus probes");
        assert_eq!(profile.sample_rate_hz, Known::Known(OPUS_RATE), "{name}");
        assert_eq!(profile.channels, Known::Known(channels), "{name}");
        let probed_ms = match profile.duration_ms {
            Known::Known(ms) => i64::try_from(ms).expect("a sane duration"),
            Known::Unknown => panic!("{name}: the probe states no duration"),
        };
        let probed_frames = match profile.frames {
            Known::Known(frames) => frames,
            Known::Unknown => panic!("{name}: the probe states no frame count"),
        };
        assert!(
            probed_frames < raw_frames,
            "{name}: the probe kept the {pre_skip}-frame pre-skip"
        );

        // (c) The waveform the user clicks on.
        let ctx = JobContext::new(JobId::new());
        let built = build_peaks(&path, PeakParams::default(), &ctx).expect("opus builds peaks");
        let peaks_ms = built.pyramid.duration().as_millis();
        let loudest = built.pyramid.levels()[0]
            .buckets()
            .iter()
            .map(|bucket| bucket.amplitude())
            .fold(0.0f32, f32::max);
        assert!(loudest > 0.05, "{name}: the waveform is flat: {loudest}");

        for (label, value) in [("probe", probed_ms), ("peaks", peaks_ms)] {
            assert!(
                (value - played_ms).abs() <= TOLERANCE_MS,
                "{name}: {label} says {value} ms, playback says {played_ms} ms"
            );
            assert!(
                (value - NOMINAL_MS).abs() <= TOLERANCE_MS,
                "{name}: {label} says {value} ms for one second of tone"
            );
        }
    }
}
