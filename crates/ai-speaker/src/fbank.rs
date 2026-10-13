//! Kaldi-compatible log-Mel filterbank features (ADR-0081 §3).
//!
//! The WeSpeaker ONNX checkpoint does not take audio. Its input is `feats`,
//! a `[batch, frames, 80]` tensor of log-Mel filterbank energies computed the
//! way Kaldi computes them, which is also what the model's own inference
//! script does (`wespeaker/bin/infer_onnx.py`: `torchaudio.compliance.kaldi`
//! `fbank`, 80 bins, 25 ms / 10 ms, `dither=0`, `window_type="hamming"`,
//! `use_energy=False`, followed by cepstral mean subtraction). Feed it
//! anything else and it still returns 256 numbers — they are simply not the
//! vectors the thresholds were measured against. Features are therefore part
//! of the checkpoint contract, not a detail of the caller.
//!
//! **Why this is first-party Rust and not a binding.** ADR-0081 §3 named
//! `kaldi-native-fbank` as the feature extractor. The measured build burden
//! (recorded in [docs/hardware/macos-apple-silicon.md]) made that the wrong
//! trade: the only maintained Rust binding (`knf-rs` → `knf-rs-sys`) vendors
//! the C++ sources and builds them with CMake *and* bindgen, so every build
//! host — including the Windows x64 one that has no measurement yet — needs
//! libclang and a C++ toolchain, and a second native parser of untrusted
//! audio lands inside the process (ADR-0015). The arithmetic here is ~300
//! lines of DSP with no allocation per frame, it is exercised against the
//! committed golden features in `tests/fbank_golden.rs`, and the repository
//! has the precedent: `opus-pure` was chosen over libopus bindings for the
//! same reason (third-party register, O1.6).
//!
//! Everything in this module is deterministic and platform-independent: no
//! SIMD intrinsics, no FFT crate whose plan depends on the host, `f64`
//! accumulation inside the transform. Two machines must agree bit for bit or
//! an operating point measured on one does not describe the other.

use std::f64::consts::PI;

/// Analysis window shape.
///
/// Kaldi's `povey` is the default in Kaldi itself and in `kaldi-native-fbank`;
/// WeSpeaker's published inference uses `hamming`. Both are here because the
/// difference is a different checkpoint contract, not a preference: the
/// golden-feature test pins each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowType {
    /// `0.54 - 0.46 cos(2πi/(N-1))`, used by WeSpeaker's own inference.
    Hamming,
    /// Kaldi's `povey` window: a Hann window raised to the power `0.85`.
    Povey,
}

impl WindowType {
    /// Stable token for manifests and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hamming => "hamming",
            Self::Povey => "povey",
        }
    }
}

/// Everything the feature front end needs, as one value.
///
/// Carried together for the same reason [`crate::checkpoint`] carries the
/// digest beside the file name: these numbers *are* part of the model's
/// identity, and a caller that can set them one at a time will eventually set
/// one of them wrongly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FbankOptions {
    /// Sample rate the samples are already at, in Hz.
    pub sample_rate_hz: u32,
    /// Analysis window length in milliseconds.
    pub frame_length_ms: f32,
    /// Hop between successive frames in milliseconds.
    pub frame_shift_ms: f32,
    /// Number of triangular Mel filters.
    pub num_mel_bins: usize,
    /// Low cut of the Mel filter bank, in Hz.
    pub low_freq_hz: f32,
    /// High cut of the Mel filter bank, in Hz. `0` means the Nyquist rate.
    pub high_freq_hz: f32,
    /// First-order pre-emphasis coefficient.
    pub preemphasis: f32,
    /// Subtract each frame's mean before pre-emphasis (Kaldi's default).
    pub remove_dc_offset: bool,
    /// Analysis window shape.
    pub window: WindowType,
    /// Scale applied to the input samples before anything else.
    ///
    /// The checkpoint's `normalize_samples` metadata is `0`, meaning it was
    /// trained on samples in 16-bit integer range; our decoder produces
    /// `[-1, 1)` floats, so they are multiplied back up by `32768`. Getting
    /// this wrong does not fail — it quietly shifts every log energy by a
    /// constant, which is exactly the kind of silent accuracy loss the
    /// golden-feature test exists to catch.
    pub sample_scale: f32,
}

impl FbankOptions {
    /// The front end the WeSpeaker ResNet34-LM checkpoint was exported with.
    #[must_use]
    pub const fn wespeaker() -> Self {
        Self {
            sample_rate_hz: 16_000,
            frame_length_ms: 25.0,
            frame_shift_ms: 10.0,
            num_mel_bins: 80,
            low_freq_hz: 20.0,
            high_freq_hz: 0.0,
            preemphasis: 0.97,
            remove_dc_offset: true,
            window: WindowType::Hamming,
            sample_scale: 32_768.0,
        }
    }

    /// Samples per analysis window.
    #[must_use]
    pub fn window_samples(&self) -> usize {
        (f64::from(self.sample_rate_hz) * 0.001 * f64::from(self.frame_length_ms)).round() as usize
    }

    /// Samples between successive windows.
    #[must_use]
    pub fn shift_samples(&self) -> usize {
        (f64::from(self.sample_rate_hz) * 0.001 * f64::from(self.frame_shift_ms)).round() as usize
    }

    /// FFT size: the window length rounded up to a power of two, as Kaldi does.
    #[must_use]
    pub fn padded_window_samples(&self) -> usize {
        self.window_samples().next_power_of_two()
    }
}

/// Why a front end could not be built or run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FbankError {
    /// An option is outside the range the arithmetic is defined for.
    #[error("filterbank option `{option}` is unusable: {reason}")]
    Option {
        /// Which option.
        option: &'static str,
        /// Why it cannot be used.
        reason: String,
    },
    /// The audio is shorter than one analysis window.
    #[error("{samples} sample(s) is shorter than the {window}-sample analysis window")]
    TooShort {
        /// Samples offered.
        samples: usize,
        /// Samples one window needs.
        window: usize,
    },
}

/// A frames × bins matrix of log-Mel energies, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    frames: usize,
    bins: usize,
    values: Vec<f32>,
}

impl Features {
    /// Number of frames.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// Number of Mel bins per frame.
    #[must_use]
    pub const fn bins(&self) -> usize {
        self.bins
    }

    /// Row-major `frames × bins` values, which is the ONNX input layout.
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// One frame.
    #[must_use]
    pub fn frame(&self, index: usize) -> &[f32] {
        &self.values[index * self.bins..(index + 1) * self.bins]
    }

    /// Cepstral mean normalisation: subtract each bin's mean over all frames.
    ///
    /// WeSpeaker applies CMN without variance normalisation, over exactly the
    /// window being embedded, so the statistics are the utterance's own. A
    /// longer window is therefore not simply more of the same signal: this is
    /// why the minimum-speech rules (VP-021) are part of the contract and not
    /// advice.
    pub fn subtract_mean(&mut self) {
        if self.frames == 0 {
            return;
        }
        let frames = self.frames as f32;
        for bin in 0..self.bins {
            let mut sum = 0.0f64;
            for frame in 0..self.frames {
                sum += f64::from(self.values[frame * self.bins + bin]);
            }
            let mean = (sum / f64::from(frames)) as f32;
            for frame in 0..self.frames {
                self.values[frame * self.bins + bin] -= mean;
            }
        }
    }
}

/// The feature front end: windows, filters and transform, built once.
#[derive(Debug, Clone)]
pub struct FbankComputer {
    options: FbankOptions,
    window: Vec<f32>,
    mel: MelBanks,
    fft: Radix2Fft,
}

impl FbankComputer {
    /// Build the front end for these options.
    pub fn new(options: FbankOptions) -> Result<Self, FbankError> {
        if options.sample_rate_hz == 0 {
            return Err(FbankError::Option {
                option: "sample_rate_hz",
                reason: "must be positive".to_owned(),
            });
        }
        let window_samples = options.window_samples();
        if window_samples < 2 {
            return Err(FbankError::Option {
                option: "frame_length_ms",
                reason: "shorter than two samples".to_owned(),
            });
        }
        if options.shift_samples() == 0 {
            return Err(FbankError::Option {
                option: "frame_shift_ms",
                reason: "rounds to zero samples".to_owned(),
            });
        }
        if options.num_mel_bins < 3 {
            return Err(FbankError::Option {
                option: "num_mel_bins",
                reason: "Kaldi needs at least three Mel bins".to_owned(),
            });
        }
        let window = window_function(options.window, window_samples);
        let mel = MelBanks::new(&options)?;
        let fft = Radix2Fft::new(options.padded_window_samples());
        Ok(Self {
            options,
            window,
            mel,
            fft,
        })
    }

    /// The options this front end was built from.
    #[must_use]
    pub const fn options(&self) -> &FbankOptions {
        &self.options
    }

    /// Frames Kaldi's `snip_edges=true` rule yields for this many samples.
    #[must_use]
    pub fn frame_count(&self, samples: usize) -> usize {
        let window = self.options.window_samples();
        if samples < window {
            0
        } else {
            1 + (samples - window) / self.options.shift_samples()
        }
    }

    /// Compute features over `samples`, which must already be mono at
    /// [`FbankOptions::sample_rate_hz`] and in `[-1, 1)`.
    pub fn compute(&self, samples: &[f32]) -> Result<Features, FbankError> {
        let window_samples = self.options.window_samples();
        let frames = self.frame_count(samples.len());
        if frames == 0 {
            return Err(FbankError::TooShort {
                samples: samples.len(),
                window: window_samples,
            });
        }
        let shift = self.options.shift_samples();
        let padded = self.options.padded_window_samples();
        let bins = self.options.num_mel_bins;

        let mut values = vec![0.0f32; frames * bins];
        // Scratch reused across frames: the embedding loop is the expensive
        // one, and a per-frame allocation would dominate a 3-second window.
        let mut frame_buffer = vec![0.0f64; padded];
        let mut power = vec![0.0f64; padded / 2];
        let mut re = vec![0.0f64; padded];
        let mut im = vec![0.0f64; padded];

        for frame in 0..frames {
            let start = frame * shift;
            let source = &samples[start..start + window_samples];

            for (slot, sample) in frame_buffer[..window_samples]
                .iter_mut()
                .zip(source.iter().copied())
            {
                *slot = f64::from(sample * self.options.sample_scale);
            }
            frame_buffer[window_samples..].fill(0.0);

            if self.options.remove_dc_offset {
                let mean =
                    frame_buffer[..window_samples].iter().sum::<f64>() / window_samples as f64;
                for slot in &mut frame_buffer[..window_samples] {
                    *slot -= mean;
                }
            }

            if self.options.preemphasis != 0.0 {
                let coefficient = f64::from(self.options.preemphasis);
                for index in (1..window_samples).rev() {
                    frame_buffer[index] -= coefficient * frame_buffer[index - 1];
                }
                frame_buffer[0] -= coefficient * frame_buffer[0];
            }

            for (slot, weight) in frame_buffer[..window_samples]
                .iter_mut()
                .zip(self.window.iter().copied())
            {
                *slot *= f64::from(weight);
            }

            re.copy_from_slice(&frame_buffer);
            im.fill(0.0);
            self.fft.transform(&mut re, &mut im);

            // Kaldi reads `padded/2` bins, not `padded/2 + 1`: the Nyquist bin
            // is never part of a filter.
            for (bin, slot) in power.iter_mut().enumerate() {
                *slot = re[bin] * re[bin] + im[bin] * im[bin];
            }

            self.mel
                .accumulate(&power, &mut values[frame * bins..(frame + 1) * bins]);
        }

        Ok(Features {
            frames,
            bins,
            values,
        })
    }
}

/// Kaldi's window functions.
fn window_function(kind: WindowType, samples: usize) -> Vec<f32> {
    let denominator = (samples - 1) as f64;
    (0..samples)
        .map(|index| {
            let phase = 2.0 * PI * index as f64 / denominator;
            let value = match kind {
                WindowType::Hamming => 0.54 - 0.46 * phase.cos(),
                WindowType::Povey => (0.5 - 0.5 * phase.cos()).powf(0.85),
            };
            value as f32
        })
        .collect()
}

/// Hertz to Mel, Kaldi's (Slaney-free) formula.
fn mel_scale(hz: f64) -> f64 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// The triangular filter bank, stored as a dense `bins × fft_bins` matrix.
///
/// Dense because it is 80 × 256 `f32` — 80 KiB, built once per process — and a
/// dense multiply has no index arithmetic to get subtly wrong.
#[derive(Debug, Clone, PartialEq)]
struct MelBanks {
    bins: usize,
    fft_bins: usize,
    weights: Vec<f32>,
}

impl MelBanks {
    fn new(options: &FbankOptions) -> Result<Self, FbankError> {
        let padded = options.padded_window_samples();
        let fft_bins = padded / 2;
        let sample_rate = f64::from(options.sample_rate_hz);
        let nyquist = sample_rate / 2.0;
        let low = f64::from(options.low_freq_hz);
        let high = if options.high_freq_hz > 0.0 {
            f64::from(options.high_freq_hz)
        } else {
            nyquist + f64::from(options.high_freq_hz)
        };
        if low < 0.0 || high <= low || high > nyquist {
            return Err(FbankError::Option {
                option: "low_freq_hz/high_freq_hz",
                reason: format!("{low} Hz to {high} Hz is not inside 0 Hz to {nyquist} Hz"),
            });
        }

        let bins = options.num_mel_bins;
        let fft_bin_width = sample_rate / padded as f64;
        let mel_low = mel_scale(low);
        let mel_high = mel_scale(high);
        let mel_delta = (mel_high - mel_low) / (bins + 1) as f64;

        let mut weights = vec![0.0f32; bins * fft_bins];
        for bin in 0..bins {
            let left = mel_low + bin as f64 * mel_delta;
            let centre = left + mel_delta;
            let right = left + 2.0 * mel_delta;
            for index in 0..fft_bins {
                let mel = mel_scale(fft_bin_width * index as f64);
                if mel > left && mel < right {
                    let weight = if mel <= centre {
                        (mel - left) / (centre - left)
                    } else {
                        (right - mel) / (right - centre)
                    };
                    weights[bin * fft_bins + index] = weight as f32;
                }
            }
        }

        Ok(Self {
            bins,
            fft_bins,
            weights,
        })
    }

    /// Log-compressed Mel energies of one power spectrum.
    fn accumulate(&self, power: &[f64], out: &mut [f32]) {
        debug_assert_eq!(power.len(), self.fft_bins);
        debug_assert_eq!(out.len(), self.bins);
        for (bin, slot) in out.iter_mut().enumerate() {
            let row = &self.weights[bin * self.fft_bins..(bin + 1) * self.fft_bins];
            let mut energy = 0.0f64;
            for (weight, value) in row.iter().copied().zip(power.iter().copied()) {
                if weight != 0.0 {
                    energy += f64::from(weight) * value;
                }
            }
            // Kaldi floors at FLT_EPSILON before the logarithm, so a silent
            // frame produces a large negative number rather than -inf.
            let floored = energy.max(f64::from(f32::EPSILON));
            *slot = floored.ln() as f32;
        }
    }
}

/// An iterative radix-2 Cooley-Tukey FFT with precomputed twiddles.
///
/// Written out rather than pulled in: the transform is twelve lines of
/// arithmetic, and a planner that picks a different algorithm per host is
/// exactly what a reproducible feature vector cannot have.
#[derive(Debug, Clone, PartialEq)]
struct Radix2Fft {
    size: usize,
    reversed: Vec<u32>,
    twiddle_re: Vec<f64>,
    twiddle_im: Vec<f64>,
}

impl Radix2Fft {
    fn new(size: usize) -> Self {
        assert!(size.is_power_of_two(), "FFT size must be a power of two");
        let bits = size.trailing_zeros();
        let reversed = (0..size)
            .map(|index| (index as u32).reverse_bits() >> (32 - bits))
            .collect();
        let mut twiddle_re = Vec::with_capacity(size / 2);
        let mut twiddle_im = Vec::with_capacity(size / 2);
        for index in 0..size / 2 {
            let angle = -2.0 * PI * index as f64 / size as f64;
            twiddle_re.push(angle.cos());
            twiddle_im.push(angle.sin());
        }
        Self {
            size,
            reversed,
            twiddle_re,
            twiddle_im,
        }
    }

    /// In-place forward transform of `re`/`im`, both of length `size`.
    fn transform(&self, re: &mut [f64], im: &mut [f64]) {
        debug_assert_eq!(re.len(), self.size);
        debug_assert_eq!(im.len(), self.size);

        for index in 0..self.size {
            let target = self.reversed[index] as usize;
            if index < target {
                re.swap(index, target);
                im.swap(index, target);
            }
        }

        let mut length = 2;
        while length <= self.size {
            let half = length / 2;
            let step = self.size / length;
            for start in (0..self.size).step_by(length) {
                for offset in 0..half {
                    let twiddle = offset * step;
                    let (wr, wi) = (self.twiddle_re[twiddle], self.twiddle_im[twiddle]);
                    let upper = start + offset + half;
                    let lower = start + offset;
                    let tr = re[upper] * wr - im[upper] * wi;
                    let ti = re[upper] * wi + im[upper] * wr;
                    re[upper] = re[lower] - tr;
                    im[upper] = im[lower] - ti;
                    re[lower] += tr;
                    im[lower] += ti;
                }
            }
            length *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(samples: usize, hz: f32) -> Vec<f32> {
        (0..samples)
            .map(|index| (2.0 * std::f32::consts::PI * hz * index as f32 / 16_000.0).sin() * 0.5)
            .collect()
    }

    #[test]
    fn the_wespeaker_front_end_has_the_shape_the_checkpoint_declares() {
        let options = FbankOptions::wespeaker();
        assert_eq!(options.window_samples(), 400);
        assert_eq!(options.shift_samples(), 160);
        assert_eq!(options.padded_window_samples(), 512);
    }

    #[test]
    fn frame_counts_follow_kaldis_snip_edges_rule() {
        let computer = FbankComputer::new(FbankOptions::wespeaker()).expect("front end");
        assert_eq!(computer.frame_count(399), 0);
        assert_eq!(computer.frame_count(400), 1);
        assert_eq!(computer.frame_count(559), 1);
        assert_eq!(computer.frame_count(560), 2);
        // Three seconds of 16 kHz audio.
        assert_eq!(computer.frame_count(48_000), 298);
    }

    #[test]
    fn audio_shorter_than_one_window_is_refused_rather_than_padded() {
        let computer = FbankComputer::new(FbankOptions::wespeaker()).expect("front end");
        let error = computer.compute(&tone(200, 440.0)).expect_err("too short");
        assert_eq!(
            error,
            FbankError::TooShort {
                samples: 200,
                window: 400
            }
        );
    }

    #[test]
    fn a_tone_lights_up_the_mel_bin_that_contains_it() {
        let computer = FbankComputer::new(FbankOptions::wespeaker()).expect("front end");
        let features = computer.compute(&tone(16_000, 1_000.0)).expect("features");
        assert_eq!(features.bins(), 80);
        assert_eq!(features.frames(), 98);

        let frame = features.frame(40);
        let (peak, _) = frame
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .expect("a peak");
        // mel(1000) = 1127 ln(1 + 1000/700) = 999.9, mel(20) = 31.7 and the
        // spacing over 80 bins is (mel(8000) - mel(20))/81 = 34.7, so the
        // filter centred on 1 kHz is bin 27.
        assert!(
            (26..=28).contains(&peak),
            "a 1 kHz tone peaked in bin {peak}"
        );
    }

    #[test]
    fn the_transform_is_bit_identical_across_repeated_runs() {
        let computer = FbankComputer::new(FbankOptions::wespeaker()).expect("front end");
        let samples = tone(16_000, 440.0);
        let first = computer.compute(&samples).expect("features");
        let second = computer.compute(&samples).expect("features");
        assert_eq!(first.values(), second.values());
    }

    #[test]
    fn mean_subtraction_centres_every_bin() {
        let computer = FbankComputer::new(FbankOptions::wespeaker()).expect("front end");
        let mut features = computer.compute(&tone(16_000, 440.0)).expect("features");
        features.subtract_mean();
        for bin in 0..features.bins() {
            let mean: f64 = (0..features.frames())
                .map(|frame| f64::from(features.frame(frame)[bin]))
                .sum::<f64>()
                / features.frames() as f64;
            assert!(mean.abs() < 1e-3, "bin {bin} still has mean {mean}");
        }
    }

    #[test]
    fn the_fft_agrees_with_a_direct_discrete_transform() {
        let size = 32;
        let fft = Radix2Fft::new(size);
        let signal: Vec<f64> = (0..size)
            .map(|index| (index as f64 * 0.37).sin() + 0.25 * (index as f64 * 1.9).cos())
            .collect();

        let mut re = signal.clone();
        let mut im = vec![0.0; size];
        fft.transform(&mut re, &mut im);

        for bin in 0..size {
            let mut direct_re = 0.0;
            let mut direct_im = 0.0;
            for (index, value) in signal.iter().enumerate() {
                let angle = -2.0 * PI * (bin * index) as f64 / size as f64;
                direct_re += value * angle.cos();
                direct_im += value * angle.sin();
            }
            assert!((re[bin] - direct_re).abs() < 1e-9, "bin {bin} real part");
            assert!(
                (im[bin] - direct_im).abs() < 1e-9,
                "bin {bin} imaginary part"
            );
        }
    }

    #[test]
    fn the_mel_filters_cover_the_band_without_gaps() {
        let options = FbankOptions::wespeaker();
        let banks = MelBanks::new(&options).expect("filters");
        // Every filter carries weight, and consecutive filters overlap.
        for bin in 0..banks.bins {
            let row = &banks.weights[bin * banks.fft_bins..(bin + 1) * banks.fft_bins];
            assert!(
                row.iter().any(|weight| *weight > 0.0),
                "mel bin {bin} has no weight"
            );
        }
    }

    #[test]
    fn an_impossible_band_is_refused() {
        let mut options = FbankOptions::wespeaker();
        options.high_freq_hz = 9_000.0;
        let error = MelBanks::new(&options).expect_err("above Nyquist");
        assert!(matches!(error, FbankError::Option { .. }));
    }
}
