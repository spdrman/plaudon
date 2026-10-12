//! Deterministic synthetic fixture generation.
//!
//! Every fixture in the test suite is generated from these functions rather
//! than checked in as a binary blob, so the corpus is reproducible, licence-
//! clean (§17.1: "we will not include confidential real client conversations
//! in the public repository") and diff-reviewable.
//!
//! The generators are deliberately dependency-free: the RIFF writer below is
//! a few dozen lines, and depending on an encoder here would make fixtures
//! vary with that encoder's version.

use std::f64::consts::TAU;
use std::io::Write;
use std::path::Path;

/// Write a canonical 16-bit PCM RIFF/WAVE file.
///
/// Samples are interleaved; `samples.len()` must be a multiple of `channels`.
///
/// # Panics
/// Panics if the file cannot be written or the sample count is not a whole
/// number of frames. This is test-support code; a broken fixture must fail
/// loudly.
pub fn write_wav(path: &Path, sample_rate_hz: u32, channels: u16, samples: &[i16]) {
    assert!(channels > 0, "channel count must be positive");
    assert!(
        samples.len().is_multiple_of(channels as usize),
        "sample buffer is not a whole number of frames"
    );

    let bits_per_sample: u16 = 16;
    let block_align = channels * bits_per_sample / 8;
    let byte_rate = sample_rate_hz * u32::from(block_align);
    let data_bytes = (samples.len() * 2) as u32;

    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }

    let mut file = std::fs::File::create(path).expect("create fixture");
    file.write_all(&out).expect("write fixture");
    file.sync_all().expect("sync fixture");
}

/// Deterministic sine tone, interleaved across `channels`.
///
/// Each channel gets a distinct frequency (440 Hz × (channel + 1)) so that a
/// blind stereo downmix is detectable in the output.
#[must_use]
pub fn sine_samples(sample_rate_hz: u32, channels: u16, duration_ms: u64) -> Vec<i16> {
    let frames = (u64::from(sample_rate_hz) * duration_ms / 1000) as usize;
    let mut samples = Vec::with_capacity(frames * channels as usize);
    for frame in 0..frames {
        for channel in 0..channels {
            let frequency = 440.0 * f64::from(channel + 1);
            let phase = TAU * frequency * (frame as f64) / f64::from(sample_rate_hz);
            let amplitude = 0.5 * phase.sin();
            samples.push((amplitude * f64::from(i16::MAX)).round() as i16);
        }
    }
    samples
}

/// Write a deterministic sine-tone WAV fixture.
pub fn write_sine_wav(path: &Path, sample_rate_hz: u32, channels: u16, duration_ms: u64) {
    let samples = sine_samples(sample_rate_hz, channels, duration_ms);
    write_wav(path, sample_rate_hz, channels, &samples);
}

/// Speech-like fixture: alternating bursts of band-limited noise and true
/// digital silence, for silence detection and hallucination tests.
///
/// `pattern` lists `(is_speech, duration_ms)` segments in order.
#[must_use]
pub fn speech_pattern_samples(
    sample_rate_hz: u32,
    channels: u16,
    pattern: &[(bool, u64)],
) -> Vec<i16> {
    // A tiny deterministic LCG keeps the fixture reproducible on every host
    // without pulling in an RNG crate.
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as i64 - (1 << 30)) as f64 / f64::from(1u32 << 30)
    };

    let mut samples = Vec::new();
    for &(is_speech, duration_ms) in pattern {
        let frames = (u64::from(sample_rate_hz) * duration_ms / 1000) as usize;
        for _ in 0..frames {
            let value = if is_speech {
                (next() * 0.45 * f64::from(i16::MAX)).round() as i16
            } else {
                0
            };
            for _ in 0..channels {
                samples.push(value);
            }
        }
    }
    samples
}

/// Write a speech/silence pattern fixture.
pub fn write_speech_pattern_wav(
    path: &Path,
    sample_rate_hz: u32,
    channels: u16,
    pattern: &[(bool, u64)],
) {
    let samples = speech_pattern_samples(sample_rate_hz, channels, pattern);
    write_wav(path, sample_rate_hz, channels, &samples);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_generation_is_deterministic() {
        let first = sine_samples(8_000, 1, 100);
        let second = sine_samples(8_000, 1, 100);
        assert_eq!(first, second);
        assert_eq!(first.len(), 800);
    }

    #[test]
    fn channels_carry_distinct_content() {
        let stereo = sine_samples(8_000, 2, 100);
        let left: Vec<i16> = stereo.iter().step_by(2).copied().collect();
        let right: Vec<i16> = stereo.iter().skip(1).step_by(2).copied().collect();
        assert_ne!(left, right, "stereo fixture must not be dual-mono");
    }

    #[test]
    fn written_wav_has_the_declared_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.wav");
        write_sine_wav(&path, 16_000, 1, 250);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 4000 * 2);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
    }

    #[test]
    fn silence_segments_are_exactly_zero() {
        let samples = speech_pattern_samples(8_000, 1, &[(false, 100), (true, 100)]);
        assert!(samples[..800].iter().all(|sample| *sample == 0));
        assert!(samples[800..].iter().any(|sample| *sample != 0));
    }

    #[test]
    fn speech_pattern_is_deterministic() {
        let pattern = [(true, 50), (false, 50), (true, 50)];
        assert_eq!(
            speech_pattern_samples(8_000, 1, &pattern),
            speech_pattern_samples(8_000, 1, &pattern)
        );
    }
}
