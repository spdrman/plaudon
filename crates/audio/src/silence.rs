//! Energy-based speech/non-speech measurement.
//!
//! Two consumers:
//!
//! * [`crate::chunk`] uses it to place chunk boundaries in silence instead of
//!   mid-word (§8.2 "silence-aligned boundaries when supported").
//! * The hallucination detector uses it to decide whether an engine claimed
//!   text over a region that contains no speech (§8.6A). That check only
//!   means anything because this measurement is independent of the engine.
//!
//! This is a *speech activity* measurement, not a classifier: it answers
//! "does this region contain signal above the measured noise floor", and the
//! annotation it feeds is labelled "suspected", never "hallucinated".

use plaudon_ai_contracts::SpeechActivity;

/// Tunables for [`measure_speech_activity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SilenceOptions {
    /// Analysis window length.
    pub frame_ms: u32,
    /// Fixed floor in dBFS. A frame at or below this is non-speech regardless
    /// of the adaptive floor.
    pub absolute_floor_dbfs: i16,
    /// How far above the measured noise floor a frame must sit to count as
    /// speech. Makes the measurement work on quiet and loud recordings alike.
    pub above_noise_floor_db: i16,
    /// Runs of speech shorter than this are discarded as clicks.
    pub min_speech_ms: u32,
    /// Gaps shorter than this do not split a speech run.
    pub min_silence_ms: u32,
}

/// Where the adaptive threshold sits between the measured quiet and loud
/// populations. Below the midpoint, because speech energy varies far more
/// than a noise floor does, so a midpoint threshold clips quiet syllables.
const ADAPTIVE_THRESHOLD_FRACTION: f64 = 0.35;

impl Default for SilenceOptions {
    fn default() -> Self {
        Self {
            frame_ms: 20,
            absolute_floor_dbfs: -60,
            above_noise_floor_db: 8,
            min_speech_ms: 100,
            min_silence_ms: 300,
        }
    }
}

/// Measure speech activity over interleaved `f32` samples in `-1.0..=1.0`.
///
/// Channels are combined by taking the per-frame maximum absolute value: a
/// participant speaking on one channel of a stereo recording must not be
/// averaged into silence.
#[must_use]
pub fn measure_speech_activity(
    samples: &[f32],
    sample_rate_hz: u32,
    channels: u16,
    options: &SilenceOptions,
) -> SpeechActivity {
    assert!(channels > 0, "channel count must be positive");
    let channels = usize::from(channels);
    let total_frames = (samples.len() / channels) as u64;
    if total_frames == 0 || sample_rate_hz == 0 {
        return SpeechActivity::new(sample_rate_hz, total_frames, Vec::new());
    }

    let window_frames = ((u64::from(sample_rate_hz) * u64::from(options.frame_ms)) / 1000).max(1);
    let window_samples = (window_frames as usize) * channels;

    // Per-window RMS in dBFS.
    let mut levels: Vec<f64> = Vec::new();
    for window in samples.chunks(window_samples) {
        let frames_in_window = window.len() / channels;
        if frames_in_window == 0 {
            break;
        }
        let mut energy = 0.0f64;
        for frame in window.chunks_exact(channels) {
            // Loudest channel represents the frame.
            let peak = frame
                .iter()
                .fold(0.0f64, |acc, sample| acc.max(f64::from(*sample).abs()));
            energy += peak * peak;
        }
        let rms = (energy / frames_in_window as f64).sqrt();
        levels.push(if rms <= 0.0 {
            f64::from(options.absolute_floor_dbfs) - 40.0
        } else {
            20.0 * rms.log10()
        });
    }

    // A percentile pair tells us whether the recording actually contains two
    // populations. A continuously-speaking recording has no quiet population,
    // so "noise floor + delta" would reject all of it; a silent recording has
    // no loud population, so the same rule would accept all of it. We only
    // place an adaptive threshold when the two populations are separated.
    let quiet = percentile(&levels, 0.10);
    let loud = percentile(&levels, 0.90);
    let absolute_floor = f64::from(options.absolute_floor_dbfs);
    let threshold = if loud - quiet < f64::from(options.above_noise_floor_db) * 2.0 {
        absolute_floor
    } else {
        absolute_floor.max(quiet + (loud - quiet) * ADAPTIVE_THRESHOLD_FRACTION)
    };

    let mut flags: Vec<bool> = levels.iter().map(|level| *level > threshold).collect();

    let min_silence_windows =
        (u64::from(options.min_silence_ms) / u64::from(options.frame_ms).max(1)).max(1) as usize;
    let min_speech_windows =
        (u64::from(options.min_speech_ms) / u64::from(options.frame_ms).max(1)).max(1) as usize;

    bridge_short_runs(&mut flags, false, min_silence_windows);
    bridge_short_runs(&mut flags, true, min_speech_windows);

    let mut intervals: Vec<(u64, u64)> = Vec::new();
    let mut run_start: Option<usize> = None;
    for (index, is_speech) in flags.iter().enumerate() {
        match (is_speech, run_start) {
            (true, None) => run_start = Some(index),
            (false, Some(start)) => {
                intervals.push((
                    start as u64 * window_frames,
                    (index as u64 * window_frames).min(total_frames),
                ));
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = run_start {
        intervals.push((start as u64 * window_frames, total_frames));
    }

    SpeechActivity::new(sample_rate_hz, total_frames, intervals)
}

/// Replace runs of `value` shorter than `min_len` with `!value`.
fn bridge_short_runs(flags: &mut [bool], value: bool, min_len: usize) {
    let mut index = 0usize;
    while index < flags.len() {
        if flags[index] != value {
            index += 1;
            continue;
        }
        let start = index;
        while index < flags.len() && flags[index] == value {
            index += 1;
        }
        // Leading and trailing runs are kept: a recording may genuinely start
        // or end mid-speech.
        let interior = start > 0 && index < flags.len();
        if interior && index - start < min_len {
            for flag in &mut flags[start..index] {
                *flag = !value;
            }
        }
    }
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

/// Decode canonical `f32` PCM bytes back into samples.
///
/// Used by callers that already have a PCM derivative on disk and want to
/// measure it without decoding the source again.
#[must_use]
pub fn f32_samples_from_pcm_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|quad| f32::from_le_bytes(*quad))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn samples_from_pattern(sample_rate: u32, channels: u16, pattern: &[(bool, u64)]) -> Vec<f32> {
        testing::speech_pattern_samples(sample_rate, channels, pattern)
            .into_iter()
            .map(|sample| f32::from(sample) / f32::from(i16::MAX))
            .collect()
    }

    #[test]
    fn pure_silence_has_no_speech() {
        let samples = samples_from_pattern(16_000, 1, &[(false, 2_000)]);
        let activity = measure_speech_activity(&samples, 16_000, 1, &SilenceOptions::default());
        assert_eq!(activity.speech_samples(), 0);
        assert_eq!(activity.speech_intervals().len(), 0);
    }

    #[test]
    fn continuous_speech_is_entirely_speech() {
        let samples = samples_from_pattern(16_000, 1, &[(true, 2_000)]);
        let activity = measure_speech_activity(&samples, 16_000, 1, &SilenceOptions::default());
        let ratio = activity.speech_ratio(0, activity.total_samples());
        assert!(ratio > 0.95, "ratio was {ratio}");
    }

    #[test]
    fn a_gap_between_two_utterances_is_found() {
        // 1 s speech, 1 s silence, 1 s speech.
        let samples =
            samples_from_pattern(16_000, 1, &[(true, 1_000), (false, 1_000), (true, 1_000)]);
        let activity = measure_speech_activity(&samples, 16_000, 1, &SilenceOptions::default());
        assert_eq!(
            activity.speech_intervals().len(),
            2,
            "intervals: {:?}",
            activity.speech_intervals()
        );
        let gap_ratio = activity.speech_ratio(17_000, 31_000);
        assert!(gap_ratio < 0.05, "gap was {gap_ratio} speech");
    }

    #[test]
    fn short_gaps_do_not_split_an_utterance() {
        // 50 ms of silence inside speech is a breath, not a boundary.
        let samples = samples_from_pattern(16_000, 1, &[(true, 600), (false, 50), (true, 600)]);
        let activity = measure_speech_activity(&samples, 16_000, 1, &SilenceOptions::default());
        assert_eq!(activity.speech_intervals().len(), 1);
    }

    #[test]
    fn one_loud_channel_is_not_averaged_into_silence() {
        // Left channel speaks, right channel is silent.
        let frames = 16_000usize;
        let mono = samples_from_pattern(16_000, 1, &[(true, 1_000)]);
        let mut stereo = Vec::with_capacity(frames * 2);
        for sample in mono.iter().take(frames) {
            stereo.push(*sample);
            stereo.push(0.0);
        }
        let activity = measure_speech_activity(&stereo, 16_000, 2, &SilenceOptions::default());
        let ratio = activity.speech_ratio(0, activity.total_samples());
        assert!(ratio > 0.9, "single-channel speech was lost: {ratio}");
    }

    #[test]
    fn measurement_is_deterministic() {
        let samples = samples_from_pattern(16_000, 1, &[(true, 300), (false, 300), (true, 300)]);
        let first = measure_speech_activity(&samples, 16_000, 1, &SilenceOptions::default());
        let second = measure_speech_activity(&samples, 16_000, 1, &SilenceOptions::default());
        assert_eq!(first, second);
    }

    #[test]
    fn empty_input_is_handled() {
        let activity = measure_speech_activity(&[], 16_000, 1, &SilenceOptions::default());
        assert_eq!(activity.total_samples(), 0);
        assert_eq!(activity.speech_intervals().len(), 0);
    }

    #[test]
    fn pcm_bytes_round_trip_to_samples() {
        let samples = vec![0.0f32, 0.5, -0.25];
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(f32_samples_from_pcm_bytes(&bytes), samples);
    }
}
