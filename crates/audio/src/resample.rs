//! Deterministic sample-rate conversion.
//!
//! §8.1 requires a *deterministic* canonical PCM derivative, and F1.2's
//! acceptance criterion is that the digest is reproducible "across runs and
//! across macOS/Windows for the same input and profile". That rules out any
//! resampler whose output depends on SIMD width, thread count, or a vendor
//! library version.
//!
//! So we implement the conversion ourselves: a windowed-sinc polyphase FIR
//! with a rational `L/M` ratio, evaluated in `f64` in a fixed order. Rust does
//! not contract `a * b + c` into an FMA, and IEEE-754 `f64` addition and
//! multiplication are exactly specified, so the result is bit-identical on
//! every target we support.
//!
//! When `L == M` the resampler is bypassed entirely, so a 16 kHz source
//! destined for a 16 kHz canonical profile is copied, not filtered.

/// Taps per polyphase branch. 32 gives roughly 100 dB of stopband rejection
/// with the Kaiser window below, which is well past what a 16 kHz speech
/// derivative can show.
const TAPS_PER_PHASE: usize = 32;

/// Kaiser window shape parameter. β = 8.6 corresponds to about −90 dB
/// sidelobes.
const KAISER_BETA: f64 = 8.6;

/// Streaming rational resampler for one interleaved multi-channel signal.
#[derive(Debug)]
pub struct Resampler {
    /// Interpolation factor (output side of the reduced ratio).
    l: usize,
    /// Decimation factor (input side of the reduced ratio).
    m: usize,
    channels: usize,
    /// Prototype low-pass filter, length `TAPS_PER_PHASE * l`.
    taps: Vec<f64>,
    /// Half the prototype filter length, in upsampled samples.
    center: usize,
    /// Per-channel sample history, most recent last. `None` when bypassing.
    buffer: Vec<Vec<f64>>,
    /// Absolute index of `buffer[c][0]` in the input stream.
    base: i64,
    /// Index of the next output sample to produce.
    output_index: u64,
    /// Real input frames fed so far, excluding flush padding. The output is
    /// capped at `ceil(input_frames_seen * L / M)` so the filter tail cannot
    /// append phantom frames past the end of the source.
    input_frames_seen: u64,
    bypass: bool,
}

impl Resampler {
    /// Build a resampler converting `from_hz` to `to_hz` for `channels`
    /// interleaved channels.
    #[must_use]
    pub fn new(from_hz: u32, to_hz: u32, channels: u16) -> Self {
        assert!(from_hz > 0 && to_hz > 0, "sample rates must be positive");
        assert!(channels > 0, "channel count must be positive");
        let divisor = gcd(u64::from(from_hz), u64::from(to_hz));
        let l = (u64::from(to_hz) / divisor) as usize;
        let m = (u64::from(from_hz) / divisor) as usize;
        let bypass = l == 1 && m == 1;

        let (taps, center) = if bypass {
            (Vec::new(), 0)
        } else {
            design_prototype(l, m)
        };

        let channels = usize::from(channels);
        // Pre-pad with `TAPS_PER_PHASE - 1` zeros so the very first output can
        // be produced; `base` therefore starts negative.
        let prepad = TAPS_PER_PHASE - 1;
        let buffer = if bypass {
            vec![Vec::new(); channels]
        } else {
            vec![vec![0.0; prepad]; channels]
        };

        Self {
            l,
            m,
            channels,
            taps,
            center,
            buffer,
            base: -(prepad as i64),
            output_index: 0,
            input_frames_seen: 0,
            bypass,
        }
    }

    /// Whether this resampler is a pass-through.
    #[must_use]
    pub const fn is_bypass(&self) -> bool {
        self.bypass
    }

    /// Feed interleaved input frames, returning whatever interleaved output
    /// frames became available.
    ///
    /// Memory use is bounded by `channels * TAPS_PER_PHASE` of retained
    /// history plus the caller's block, never by total stream length.
    pub fn process(&mut self, interleaved: &[f64]) -> Vec<f64> {
        if self.bypass {
            return interleaved.to_vec();
        }
        debug_assert_eq!(interleaved.len() % self.channels, 0);

        self.input_frames_seen += (interleaved.len() / self.channels) as u64;
        for (index, sample) in interleaved.iter().enumerate() {
            self.buffer[index % self.channels].push(*sample);
        }
        self.drain(false)
    }

    /// Flush the filter tail. Call once after the final [`Resampler::process`].
    pub fn finish(&mut self) -> Vec<f64> {
        if self.bypass {
            return Vec::new();
        }
        // Push enough zeros that every remaining real sample can reach the
        // centre of the filter.
        for channel in &mut self.buffer {
            channel.extend(std::iter::repeat_n(0.0, TAPS_PER_PHASE));
        }
        self.drain(true)
    }

    fn drain(&mut self, flushing: bool) -> Vec<f64> {
        let available_end = self.base + self.buffer[0].len() as i64; // exclusive
        let mut out = Vec::new();

        let output_limit = self.predict_output_frames(self.input_frames_seen);
        loop {
            if self.output_index >= output_limit {
                break;
            }
            let upsampled = self.output_index * self.m as u64 + self.center as u64;
            let phase = (upsampled % self.l as u64) as usize;
            let q = (upsampled / self.l as u64) as i64;

            // Need x[q - (TAPS_PER_PHASE - 1) ..= q].
            if q >= available_end {
                break;
            }
            let oldest_needed = q - (TAPS_PER_PHASE as i64 - 1);
            if oldest_needed < self.base {
                // Should not happen: we pre-padded. Guard anyway.
                break;
            }

            for channel in 0..self.channels {
                let samples = &self.buffer[channel];
                let mut accumulator = 0.0f64;
                for j in 0..TAPS_PER_PHASE {
                    let tap = self.taps[phase + j * self.l];
                    let index = (q - j as i64 - self.base) as usize;
                    accumulator += tap * samples[index];
                }
                out.push(accumulator);
            }
            self.output_index += 1;
        }

        // Discard history that no future output can reach.
        let next_upsampled = self.output_index * self.m as u64 + self.center as u64;
        let next_q = (next_upsampled / self.l as u64) as i64;
        let keep_from = next_q - (TAPS_PER_PHASE as i64 - 1);
        if keep_from > self.base {
            let drop = (keep_from - self.base) as usize;
            for channel in &mut self.buffer {
                let drop = drop.min(channel.len());
                channel.drain(..drop);
            }
            self.base += drop as i64;
        }

        if flushing {
            for channel in &mut self.buffer {
                channel.clear();
            }
        }
        out
    }

    /// Exact output frame count for a given input frame count.
    ///
    /// Used by disk preflight (§14.4) before a single byte is written.
    #[must_use]
    pub fn predict_output_frames(&self, input_frames: u64) -> u64 {
        if self.bypass {
            return input_frames;
        }
        // ceil(input * L / M)
        input_frames
            .saturating_mul(self.l as u64)
            .div_ceil(self.m as u64)
    }
}

/// Design the prototype low-pass filter for an `L/M` conversion.
///
/// Returns the taps and the group delay in upsampled samples.
fn design_prototype(l: usize, m: usize) -> (Vec<f64>, usize) {
    let length = TAPS_PER_PHASE * l;
    // Cutoff in cycles per upsampled sample: the stricter of the
    // interpolation and decimation Nyquist limits, with a small guard band so
    // the transition region does not fold.
    let cutoff = 0.5 / (l.max(m) as f64) * 0.92;
    let center = (length - 1) / 2;
    let i0_beta = bessel_i0(KAISER_BETA);

    let mut taps = Vec::with_capacity(length);
    for n in 0..length {
        let offset = n as f64 - center as f64;
        let ideal = 2.0 * cutoff * sinc(2.0 * cutoff * offset);
        let ratio = (2.0 * n as f64 / (length - 1) as f64) - 1.0;
        let window = bessel_i0(KAISER_BETA * (1.0 - ratio * ratio).max(0.0).sqrt()) / i0_beta;
        taps.push(ideal * window * l as f64);
    }
    (taps, center)
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let pi_x = std::f64::consts::PI * x;
        pi_x.sin() / pi_x
    }
}

/// Zeroth-order modified Bessel function of the first kind.
///
/// Fixed 40-term series: the term ratio falls off factorially, so this is both
/// converged and bit-reproducible.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut sum = 1.0f64;
    let mut term = 1.0f64;
    for k in 1..40u32 {
        term *= (half / f64::from(k)) * (half / f64::from(k));
        sum += term;
    }
    sum
}

const fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn tone(sample_rate: u32, frequency: f64, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|n| (TAU * frequency * n as f64 / f64::from(sample_rate)).sin())
            .collect()
    }

    #[test]
    fn equal_rates_bypass_and_copy_exactly() {
        let mut resampler = Resampler::new(16_000, 16_000, 1);
        assert!(resampler.is_bypass());
        let input = tone(16_000, 440.0, 100);
        let output = resampler.process(&input);
        assert_eq!(output, input);
        assert_eq!(resampler.finish().len(), 0);
    }

    #[test]
    fn output_length_matches_the_ratio() {
        let mut resampler = Resampler::new(44_100, 16_000, 1);
        let input = tone(44_100, 440.0, 44_100);
        let mut output = resampler.process(&input);
        output.extend(resampler.finish());
        // 44 100 input frames at 44.1 kHz ⇒ one second ⇒ 16 000 output frames,
        // ± one frame of filter boundary.
        assert!(
            (output.len() as i64 - 16_000).abs() <= 1,
            "got {} frames",
            output.len()
        );
    }

    #[test]
    fn block_size_does_not_change_the_result() {
        let input = tone(44_100, 440.0, 20_000);
        let reference = {
            let mut resampler = Resampler::new(44_100, 16_000, 1);
            let mut out = resampler.process(&input);
            out.extend(resampler.finish());
            out
        };
        for block in [1usize, 7, 256, 4096, 19_999] {
            let mut resampler = Resampler::new(44_100, 16_000, 1);
            let mut out = Vec::new();
            for chunk in input.chunks(block) {
                out.extend(resampler.process(chunk));
            }
            out.extend(resampler.finish());
            assert_eq!(out.len(), reference.len(), "block size {block}");
            for (index, (a, b)) in out.iter().zip(reference.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-12,
                    "block {block} sample {index}: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn repeated_runs_are_bit_identical() {
        let input = tone(48_000, 1000.0, 10_000);
        let run = || {
            let mut resampler = Resampler::new(48_000, 16_000, 1);
            let mut out = resampler.process(&input);
            out.extend(resampler.finish());
            out
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn channels_stay_independent() {
        let frames = 8_000;
        let left = tone(48_000, 400.0, frames);
        let right = tone(48_000, 1200.0, frames);
        let mut interleaved = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            interleaved.push(left[index]);
            interleaved.push(right[index]);
        }

        let mut stereo = Resampler::new(48_000, 16_000, 2);
        let mut stereo_out = stereo.process(&interleaved);
        stereo_out.extend(stereo.finish());

        let mut mono_left = Resampler::new(48_000, 16_000, 1);
        let mut left_out = mono_left.process(&left);
        left_out.extend(mono_left.finish());

        let deinterleaved: Vec<f64> = stereo_out.iter().step_by(2).copied().collect();
        assert_eq!(deinterleaved.len(), left_out.len());
        for (a, b) in deinterleaved.iter().zip(left_out.iter()) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn passband_tone_survives_downsampling() {
        // A 440 Hz tone is far inside the 8 kHz Nyquist of the 16 kHz target,
        // so its amplitude must be preserved.
        let frames = 44_100;
        let input = tone(44_100, 440.0, frames);
        let mut resampler = Resampler::new(44_100, 16_000, 1);
        let mut output = resampler.process(&input);
        output.extend(resampler.finish());

        // Ignore filter ramp-up/down regions.
        let body = &output[2_000..output.len() - 2_000];
        let peak = body.iter().fold(0.0f64, |acc, value| acc.max(value.abs()));
        assert!((peak - 1.0).abs() < 0.03, "peak amplitude was {peak}");
    }

    #[test]
    fn above_nyquist_tone_is_attenuated_not_aliased_through() {
        // 15 kHz cannot exist at a 16 kHz output rate; the anti-alias filter
        // must suppress it rather than fold it into the audible band.
        let frames = 48_000;
        let input = tone(48_000, 15_000.0, frames);
        let mut resampler = Resampler::new(48_000, 16_000, 1);
        let mut output = resampler.process(&input);
        output.extend(resampler.finish());

        let body = &output[2_000..output.len() - 2_000];
        let peak = body.iter().fold(0.0f64, |acc, value| acc.max(value.abs()));
        assert!(peak < 0.01, "aliased energy leaked through: peak {peak}");
    }

    #[test]
    fn predicted_output_frames_match_reality() {
        for (from, to) in [(44_100u32, 16_000u32), (48_000, 16_000), (8_000, 16_000)] {
            let input = tone(from, 300.0, from as usize);
            let mut resampler = Resampler::new(from, to, 1);
            let predicted = resampler.predict_output_frames(input.len() as u64);
            let mut output = resampler.process(&input);
            output.extend(resampler.finish());
            let actual = output.len() as u64;
            assert!(
                predicted.abs_diff(actual) <= 1,
                "{from}->{to}: predicted {predicted}, got {actual}"
            );
        }
    }

    #[test]
    fn upsampling_works_too() {
        let input = tone(8_000, 300.0, 8_000);
        let mut resampler = Resampler::new(8_000, 16_000, 1);
        let mut output = resampler.process(&input);
        output.extend(resampler.finish());
        assert!((output.len() as i64 - 16_000).abs() <= 1);
        let body = &output[1_000..output.len() - 1_000];
        let peak = body.iter().fold(0.0f64, |acc, value| acc.max(value.abs()));
        assert!((peak - 1.0).abs() < 0.03, "peak amplitude was {peak}");
    }

    #[test]
    fn history_stays_bounded_over_a_long_stream() {
        let mut resampler = Resampler::new(44_100, 16_000, 2);
        let block = vec![0.25f64; 4096];
        for _ in 0..500 {
            let _ = resampler.process(&block);
            for channel in &resampler.buffer {
                assert!(
                    channel.len() <= 4096 + TAPS_PER_PHASE * 2,
                    "history grew to {}",
                    channel.len()
                );
            }
        }
    }
}
