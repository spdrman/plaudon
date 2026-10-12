//! Pitch-preserving speed control (WSOLA).
//!
//! `<audio playbackRate>` preserves pitch; resampling does not. A transcript
//! reviewer running a meeting at 1.5× wants the voices to sound the same, so
//! simple resampling is not an option and filmcraft's answer — two commercial
//! pitch-shift stages from an unpublished crate — is not available to us.
//!
//! What is implemented here is WSOLA: overlap-add with a Hann window at 50 %
//! overlap, where each analysis segment is nudged within a small search
//! window to the offset that best continues the previous one, which is what
//! keeps periodic speech from developing a buzz. At 1× the stretcher is a
//! bypass and returns the input sample for sample, so normal playback costs
//! nothing and is bit-exact.
//!
//! The nominal input position is kept as an integer numerator over the speed
//! denominator, so a two-hour playback at 1.25× accumulates no drift.

use crate::engine::Speed;

/// Analysis window, as a fraction of a second.
const WINDOW_SECONDS: f64 = 0.040;
/// Search radius, as a fraction of a second.
const SEARCH_SECONDS: f64 = 0.005;
/// Offsets are tried every `SEARCH_STEP` frames; one-frame resolution buys
/// nothing audible and costs the search loop a factor of two.
const SEARCH_STEP: usize = 2;

/// Overlap-add time-scaler over interleaved `f32` frames.
#[derive(Debug)]
pub struct TimeStretcher {
    channels: usize,
    /// Analysis/synthesis window in frames; always `2 * hop`.
    frame: usize,
    hop: usize,
    search: usize,
    corr_len: usize,
    speed: Speed,
    window: Vec<f32>,
    /// Pending input, interleaved, starting at absolute frame `input_start`.
    input: Vec<f32>,
    input_start: u64,
    /// Nominal input position × `speed.den()`, so the advance is exact.
    position_scaled: u128,
    /// Overlap-add accumulator, `frame` frames wide.
    acc: Vec<f32>,
    /// Natural continuation of the last chosen segment, `corr_len` frames.
    template: Vec<f32>,
    /// Ready output samples, interleaved.
    out: Vec<f32>,
    out_read: usize,
    first_step: bool,
    drained: bool,
}

impl TimeStretcher {
    /// A stretcher for `channels` interleaved channels at `sample_rate`.
    #[must_use]
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        let channels = channels.max(1);
        let rate = f64::from(sample_rate.max(1));
        let hop = ((rate * WINDOW_SECONDS / 2.0).round() as usize).max(32);
        let frame = hop * 2;
        let search = ((rate * SEARCH_SECONDS).round() as usize).max(SEARCH_STEP);
        let corr_len = (hop / 4).max(16);
        let window = (0..frame)
            .map(|index| {
                let phase = std::f64::consts::TAU * index as f64 / frame as f64;
                (0.5 - 0.5 * phase.cos()) as f32
            })
            .collect();
        Self {
            channels,
            frame,
            hop,
            search,
            corr_len,
            speed: Speed::NORMAL,
            window,
            input: Vec::new(),
            input_start: 0,
            position_scaled: 0,
            acc: vec![0.0; frame * channels],
            template: Vec::new(),
            out: Vec::new(),
            out_read: 0,
            first_step: true,
            drained: false,
        }
    }

    /// Current speed.
    #[must_use]
    pub const fn speed(&self) -> Speed {
        self.speed
    }

    /// Set the speed and re-anchor the position so no frame is skipped.
    pub fn set_speed(&mut self, speed: Speed) {
        if speed == self.speed {
            return;
        }
        let position = self.position();
        self.speed = speed;
        self.position_scaled = u128::from(position) * u128::from(speed.den());
    }

    /// Whether the stretcher is passing audio through untouched.
    #[must_use]
    pub fn is_bypass(&self) -> bool {
        self.speed == Speed::NORMAL
    }

    /// Restart at absolute input frame `frame`, discarding every buffer.
    pub fn reset(&mut self, frame: u64) {
        self.input.clear();
        self.input_start = frame;
        self.position_scaled = u128::from(frame) * u128::from(self.speed.den());
        self.acc.fill(0.0);
        self.template.clear();
        self.out.clear();
        self.out_read = 0;
        self.first_step = true;
        self.drained = false;
    }

    /// Nominal absolute input frame the next segment is taken from.
    #[must_use]
    pub fn position(&self) -> u64 {
        (self.position_scaled / u128::from(self.speed.den())) as u64
    }

    /// The next contiguous input the stretcher wants: absolute start frame
    /// and a sensible number of frames to supply.
    #[must_use]
    pub fn wanted(&self) -> (u64, usize) {
        (self.input_end(), self.frame + self.search * 2)
    }

    /// Tell the stretcher the source has no more frames; the tail is flushed.
    pub fn mark_drained(&mut self) {
        self.drained = true;
    }

    /// Supply input frames. They must start where [`TimeStretcher::wanted`]
    /// asked; anything else is dropped rather than silently misaligned.
    pub fn feed(&mut self, start_frame: u64, samples: &[f32]) {
        if start_frame != self.input_end() {
            if self.input.is_empty() {
                self.input_start = start_frame;
            } else {
                return;
            }
        }
        self.input.extend_from_slice(samples);
        self.trim();
    }

    /// Emit stretched frames into `out`, returning the frames written.
    ///
    /// Returns zero when more input is needed; the caller then reads
    /// [`TimeStretcher::wanted`] and calls [`TimeStretcher::feed`].
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        if self.is_bypass() {
            return self.pull_bypass(out);
        }
        let mut written = 0usize;
        while written * self.channels < out.len() {
            if self.out_read * self.channels >= self.out.len() {
                self.out.clear();
                self.out_read = 0;
                if !self.step() {
                    break;
                }
            }
            let available = self.out.len() - self.out_read * self.channels;
            let wanted = (out.len() - written * self.channels).min(available) / self.channels
                * self.channels;
            if wanted == 0 {
                break;
            }
            let from = self.out_read * self.channels;
            out[written * self.channels..written * self.channels + wanted]
                .copy_from_slice(&self.out[from..from + wanted]);
            self.out_read += wanted / self.channels;
            written += wanted / self.channels;
        }
        written
    }

    fn pull_bypass(&mut self, out: &mut [f32]) -> usize {
        let position = self.position();
        if position < self.input_start {
            return 0;
        }
        let offset = (position - self.input_start) as usize * self.channels;
        if offset >= self.input.len() {
            return 0;
        }
        let available = self.input.len() - offset;
        let wanted = out.len().min(available) / self.channels * self.channels;
        out[..wanted].copy_from_slice(&self.input[offset..offset + wanted]);
        let frames = wanted / self.channels;
        self.position_scaled += u128::from(frames as u64) * u128::from(self.speed.num());
        self.trim();
        frames
    }

    fn input_end(&self) -> u64 {
        self.input_start + (self.input.len() / self.channels) as u64
    }

    fn trim(&mut self) {
        let keep_from = self.position().saturating_sub(self.search as u64);
        if keep_from <= self.input_start {
            return;
        }
        let drop_frames = (keep_from - self.input_start) as usize;
        let drop_samples = drop_frames * self.channels;
        if drop_samples >= self.input.len() {
            self.input.clear();
            self.input_start = keep_from;
        } else {
            self.input.drain(..drop_samples);
            self.input_start = keep_from;
        }
    }

    /// Produce one `hop` of output. Returns false when input is short.
    fn step(&mut self) -> bool {
        let position = self.position();
        let lowest = position.saturating_sub(self.search as u64);
        if lowest < self.input_start {
            return false;
        }
        let required_end = position + (self.frame + self.search) as u64;
        if self.input_end() < required_end && !self.drained {
            return false;
        }
        if self.input_end() <= position + self.hop as u64 {
            return false;
        }

        let chosen = self.best_offset(position);
        let base = (chosen - self.input_start) as usize * self.channels;
        for index in 0..self.frame {
            let weight = if self.first_step && index < self.hop {
                1.0
            } else {
                self.window[index]
            };
            for channel in 0..self.channels {
                let at = base + index * self.channels + channel;
                let sample = self.input.get(at).copied().unwrap_or(0.0);
                self.acc[index * self.channels + channel] += weight * sample;
            }
        }

        let emit = self.hop * self.channels;
        self.out.extend_from_slice(&self.acc[..emit]);
        self.acc.copy_within(emit.., 0);
        let tail = self.acc.len() - emit;
        self.acc[tail..].fill(0.0);

        self.template.clear();
        let template_base = base + self.hop * self.channels;
        for offset in 0..self.corr_len * self.channels {
            self.template.push(
                self.input
                    .get(template_base + offset)
                    .copied()
                    .unwrap_or(0.0),
            );
        }

        self.position_scaled += u128::from(self.hop as u64) * u128::from(self.speed.num());
        self.first_step = false;
        true
    }

    /// The absolute frame the next segment should start at: the nominal
    /// position, nudged to whichever nearby offset best continues the last
    /// segment.
    fn best_offset(&self, position: u64) -> u64 {
        if self.template.is_empty() {
            return position;
        }
        let mut best = position;
        let mut best_score = f32::NEG_INFINITY;
        let lowest = position
            .saturating_sub(self.search as u64)
            .max(self.input_start);
        let highest = position + self.search as u64;
        let mut candidate = lowest;
        while candidate <= highest {
            let base = (candidate - self.input_start) as usize * self.channels;
            if base + self.corr_len * self.channels > self.input.len() {
                break;
            }
            let mut score = 0.0f32;
            for offset in 0..self.corr_len * self.channels {
                score += self.template[offset] * self.input[base + offset];
            }
            if score > best_score {
                best_score = score;
                best = candidate;
            }
            candidate += SEARCH_STEP as u64;
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the stretcher from a slice until it has produced `want` frames
    /// or the input runs out, the way the mixer does.
    fn run(stretcher: &mut TimeStretcher, input: &[f32], channels: usize, want: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; want * channels];
        let mut produced = 0usize;
        loop {
            let got = stretcher.pull(&mut out[produced * channels..]);
            produced += got;
            if produced >= want {
                break;
            }
            let (start, count) = stretcher.wanted();
            let from = start as usize * channels;
            if from >= input.len() {
                stretcher.mark_drained();
                if got == 0 {
                    break;
                }
                continue;
            }
            let to = (from + count * channels).min(input.len());
            stretcher.feed(start, &input[from..to]);
        }
        out.truncate(produced * channels);
        out
    }

    fn sine(frames: usize, hz: f64, rate: u32) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                let phase = index as f64 * hz * std::f64::consts::TAU / f64::from(rate);
                (phase.sin() * 0.8) as f32
            })
            .collect()
    }

    /// Zero crossings per second: a direct read of the dominant pitch.
    fn crossing_rate(samples: &[f32], rate: u32) -> f64 {
        let crossings = samples
            .windows(2)
            .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
            .count();
        crossings as f64 * f64::from(rate) / samples.len() as f64
    }

    #[test]
    fn normal_speed_is_a_bit_exact_bypass() {
        let rate = 16_000;
        let input = sine(8_000, 220.0, rate);
        let mut stretcher = TimeStretcher::new(1, rate);
        assert!(stretcher.is_bypass());
        let out = run(&mut stretcher, &input, 1, 4_000);
        assert_eq!(out.len(), 4_000);
        assert_eq!(&out[..], &input[..4_000]);
    }

    #[test]
    fn output_length_tracks_the_speed_ratio() {
        let rate = 16_000;
        let input = sine(64_000, 220.0, rate);
        for speed in [
            Speed::HALF,
            Speed::THREE_QUARTERS,
            Speed::ONE_AND_A_HALF,
            Speed::DOUBLE,
        ] {
            let mut stretcher = TimeStretcher::new(1, rate);
            stretcher.set_speed(speed);
            let want = 64_000 * usize::try_from(speed.den()).unwrap()
                / usize::try_from(speed.num()).unwrap();
            let out = run(&mut stretcher, &input, 1, want);
            let tolerance = 2_000;
            assert!(
                out.len() + tolerance >= want,
                "speed {speed:?}: produced {} of {want}",
                out.len()
            );
        }
    }

    #[test]
    fn pitch_survives_the_stretch() {
        let rate = 16_000;
        let hz = 300.0;
        let input = sine(64_000, hz, rate);
        let reference = crossing_rate(&input, rate);
        for speed in [Speed::HALF, Speed::ONE_AND_A_HALF, Speed::DOUBLE] {
            let mut stretcher = TimeStretcher::new(1, rate);
            stretcher.set_speed(speed);
            let out = run(&mut stretcher, &input, 1, 24_000);
            assert!(out.len() > 16_000, "speed {speed:?} produced {}", out.len());
            // Ignore the first window, which ramps in.
            let measured = crossing_rate(&out[2_000..], rate);
            let drift = (measured - reference).abs() / reference;
            assert!(
                drift < 0.08,
                "speed {speed:?}: {measured:.1} Hz crossings vs {reference:.1}"
            );
        }
    }

    #[test]
    fn a_resampler_would_have_moved_the_pitch_and_this_does_not() {
        // Guards the property the test above asserts: at 2x a resampling
        // implementation doubles the crossing rate. Anything within 8 % of
        // the original therefore cannot be resampling.
        let rate = 16_000;
        let input = sine(64_000, 300.0, rate);
        let resampled: Vec<f32> = (0..32_000).map(|index| input[index * 2]).collect();
        let reference = crossing_rate(&input, rate);
        let moved = crossing_rate(&resampled, rate);
        assert!(moved > reference * 1.8, "{moved} vs {reference}");
    }

    #[test]
    fn stereo_channels_stay_aligned() {
        let rate = 16_000;
        let left = sine(32_000, 200.0, rate);
        let right = sine(32_000, 400.0, rate);
        let input: Vec<f32> = left
            .iter()
            .zip(&right)
            .flat_map(|(l, r)| [*l, *r])
            .collect();
        let mut stretcher = TimeStretcher::new(2, rate);
        stretcher.set_speed(Speed::ONE_AND_A_HALF);
        let out = run(&mut stretcher, &input, 2, 8_000);
        assert_eq!(out.len() % 2, 0);
        let left_out: Vec<f32> = out.iter().step_by(2).copied().collect();
        let right_out: Vec<f32> = out.iter().skip(1).step_by(2).copied().collect();
        let left_rate = crossing_rate(&left_out[1_000..], rate);
        let right_rate = crossing_rate(&right_out[1_000..], rate);
        assert!(right_rate > left_rate * 1.6, "{right_rate} vs {left_rate}");
    }

    #[test]
    fn a_seek_resets_every_buffer() {
        let rate = 16_000;
        let input = sine(64_000, 220.0, rate);
        let mut stretcher = TimeStretcher::new(1, rate);
        stretcher.set_speed(Speed::ONE_AND_A_HALF);
        let _ = run(&mut stretcher, &input, 1, 4_000);
        stretcher.reset(32_000);
        assert_eq!(stretcher.position(), 32_000);
        let (start, _) = stretcher.wanted();
        assert_eq!(start, 32_000);
    }

    #[test]
    fn changing_speed_mid_stream_keeps_the_input_position() {
        let rate = 16_000;
        let input = sine(64_000, 220.0, rate);
        let mut stretcher = TimeStretcher::new(1, rate);
        stretcher.set_speed(Speed::DOUBLE);
        let _ = run(&mut stretcher, &input, 1, 4_000);
        let before = stretcher.position();
        stretcher.set_speed(Speed::HALF);
        assert_eq!(stretcher.position(), before);
    }
}
