//! Random-access PCM sources for playback.
//!
//! Playback does not get to decode the recording into memory: three hours of
//! 48 kHz stereo `f32` is four gigabytes. It also must not re-open the file
//! on every seek. A [`PcmSource`] is therefore a cursor: it decodes forward
//! cheaply and seeks when the mixer jumps, and the mixer thread owns it so
//! neither the UI nor the device callback ever touches a decoder.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::time::Micros;

/// Why a source could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The file could not be opened.
    #[error("cannot open {path}: {source}")]
    Open {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Symphonia could not make sense of the container or the codec. An Ogg
    /// Opus device derivative lands here: no Opus decoder is compiled in.
    #[error("cannot decode {path}: {detail}")]
    Unsupported {
        /// File involved.
        path: PathBuf,
        /// What Symphonia said.
        detail: String,
    },
}

/// A seekable stream of interleaved `f32` frames.
pub trait PcmSource: Send + std::fmt::Debug {
    /// Frames per second.
    fn sample_rate(&self) -> u32;
    /// Interleaved channel count.
    fn channels(&self) -> u16;
    /// Total frames, when the container states a duration.
    fn total_frames(&self) -> Option<u64>;
    /// Fill `out` with interleaved frames starting at `frame`.
    ///
    /// Returns the number of *frames* written; a short read means the end of
    /// the stream. `out.len()` must be a multiple of [`PcmSource::channels`].
    fn read(&mut self, frame: u64, out: &mut [f32]) -> usize;

    /// Duration, when known.
    fn duration(&self) -> Option<Micros> {
        self.total_frames()
            .map(|frames| Micros::from_frames(frames as i64, self.sample_rate()))
    }
}

/// An in-memory source. Tests and short clips.
#[derive(Debug, Clone)]
pub struct MemoryPcmSource {
    sample_rate: u32,
    channels: u16,
    samples: Vec<f32>,
}

impl MemoryPcmSource {
    /// Wrap interleaved samples.
    #[must_use]
    pub fn new(sample_rate: u32, channels: u16, samples: Vec<f32>) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            channels: channels.max(1),
            samples,
        }
    }

    /// A mono sine of `frames` frames at `hz`, amplitude `amplitude`.
    #[must_use]
    pub fn sine(sample_rate: u32, frames: usize, hz: f64, amplitude: f32) -> Self {
        let rate = sample_rate.max(1);
        let samples = (0..frames)
            .map(|index| {
                let phase = index as f64 * hz * std::f64::consts::TAU / f64::from(rate);
                (phase.sin() as f32) * amplitude
            })
            .collect();
        Self::new(rate, 1, samples)
    }

    /// The interleaved samples.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

impl PcmSource for MemoryPcmSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn total_frames(&self) -> Option<u64> {
        Some(self.samples.len() as u64 / u64::from(self.channels))
    }

    fn read(&mut self, frame: u64, out: &mut [f32]) -> usize {
        let channels = usize::from(self.channels);
        let start = (frame as usize).saturating_mul(channels);
        if start >= self.samples.len() {
            return 0;
        }
        let available = self.samples.len() - start;
        let wanted = out.len().min(available) / channels * channels;
        out[..wanted].copy_from_slice(&self.samples[start..start + wanted]);
        wanted / channels
    }
}

/// A Symphonia-backed file source: decode forward, seek on a jump.
pub struct SymphoniaPcmSource {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    total_frames: Option<u64>,
    /// Decoded frames waiting to be handed out, interleaved.
    buffer: Vec<f32>,
    /// Absolute frame index of `buffer[0]`.
    buffer_start: u64,
    /// Frames already handed out from the front of `buffer`.
    buffer_consumed: usize,
    /// Scratch for decoding through a gap, kept so seeking allocates once.
    skip_buffer: Vec<f32>,
    exhausted: bool,
}

impl std::fmt::Debug for SymphoniaPcmSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymphoniaPcmSource")
            .field("path", &self.path)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("total_frames", &self.total_frames)
            .finish_non_exhaustive()
    }
}

impl SymphoniaPcmSource {
    /// Open `path` for playback.
    pub fn open(path: &Path) -> Result<Self, SourceError> {
        let file = File::open(path).map_err(|source| SourceError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
            hint.with_extension(extension);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions {
                    enable_gapless: true,
                    ..FormatOptions::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|error| SourceError::Unsupported {
                path: path.to_path_buf(),
                detail: error.to_string(),
            })?;
        let format = probed.format;
        let track = format
            .default_track()
            .or_else(|| format.tracks().first())
            .ok_or_else(|| SourceError::Unsupported {
                path: path.to_path_buf(),
                detail: "container declares no audio track".to_owned(),
            })?;
        let track_id = track.id;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(0);
        let channels = track
            .codec_params
            .channels
            .map_or(0, |layout| layout.count() as u16);
        let total_frames = track.codec_params.n_frames;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|error| SourceError::Unsupported {
                path: path.to_path_buf(),
                detail: error.to_string(),
            })?;
        if sample_rate == 0 {
            return Err(SourceError::Unsupported {
                path: path.to_path_buf(),
                detail: "source states no sample rate".to_owned(),
            });
        }
        Ok(Self {
            path: path.to_path_buf(),
            format,
            decoder,
            track_id,
            sample_rate,
            channels: channels.max(1),
            total_frames,
            buffer: Vec::new(),
            buffer_start: 0,
            buffer_consumed: 0,
            skip_buffer: Vec::new(),
            exhausted: false,
        })
    }

    /// The file this source reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn cursor(&self) -> u64 {
        self.buffer_start + self.buffer_consumed as u64
    }

    fn seek_to(&mut self, frame: u64) {
        let time = symphonia::core::units::Time::from(frame as f64 / f64::from(self.sample_rate));
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        );
        self.decoder.reset();
        self.buffer.clear();
        self.buffer_consumed = 0;
        self.exhausted = false;
        self.buffer_start = match seeked {
            Ok(seeked) => seeked.actual_ts,
            // A container that cannot seek still plays from where it is; the
            // clock stays authoritative and the caller hears a jump, which is
            // better than silence.
            Err(_) => frame,
        };
    }

    /// Decode one more packet into the buffer. Returns false at end of stream.
    fn decode_more(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.exhausted = true;
                    return false;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(_) => {
                    self.exhausted = true;
                    return false;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let frames = decoded.frames();
                    if frames == 0 {
                        continue;
                    }
                    self.channels = (spec.channels.count() as u16).max(1);
                    let mut sample_buffer = SampleBuffer::<f32>::new(frames as u64, spec);
                    sample_buffer.copy_interleaved_ref(decoded);
                    // Drop the already-consumed prefix before appending, so
                    // the buffer stays one packet wide however long we play.
                    if self.buffer_consumed > 0 {
                        let channels = usize::from(self.channels);
                        let drop_samples = self.buffer_consumed * channels;
                        if drop_samples <= self.buffer.len() {
                            self.buffer.drain(..drop_samples);
                            self.buffer_start += self.buffer_consumed as u64;
                            self.buffer_consumed = 0;
                        } else {
                            self.buffer.clear();
                            self.buffer_consumed = 0;
                        }
                    }
                    self.buffer.extend_from_slice(sample_buffer.samples());
                    return true;
                }
                // A malformed packet is skipped; the next one usually
                // decodes, which is better than ending playback.
                Err(SymphoniaError::DecodeError(_)) => (),
                Err(_) => {
                    self.exhausted = true;
                    return false;
                }
            }
        }
    }
}

impl PcmSource for SymphoniaPcmSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn total_frames(&self) -> Option<u64> {
        self.total_frames
    }

    fn read(&mut self, frame: u64, out: &mut [f32]) -> usize {
        if out.len() < usize::from(self.channels) {
            return 0;
        }
        self.position_at(frame);
        self.read_contiguous(out)
    }
}

impl SymphoniaPcmSource {
    /// Put the cursor exactly on `frame`.
    ///
    /// A container seek lands on a packet boundary, not on the frame asked
    /// for — `actual_ts` says where it really went — so the gap is always
    /// decoded through afterwards. Without that, a click at 30 s plays from
    /// 29.952 s and the waveform and the audio disagree.
    fn position_at(&mut self, frame: u64) {
        if self.cursor() == frame {
            return;
        }
        let forward_gap = frame.checked_sub(self.cursor());
        // A short forward skip decodes faster than it seeks, and avoids the
        // imprecision of a seek entirely.
        if forward_gap.is_none_or(|gap| gap > u64::from(self.sample_rate)) {
            self.seek_to(frame);
        }
        if self.cursor() > frame {
            // The seek overshot; the only way back is from the start.
            self.seek_to(0);
        }
        self.skip_to(frame);
    }

    fn skip_to(&mut self, frame: u64) {
        let channels = usize::from(self.channels);
        let mut scratch = std::mem::take(&mut self.skip_buffer);
        while self.cursor() < frame {
            let gap = (frame - self.cursor()).min(1024) as usize;
            let wanted = gap * channels;
            if scratch.len() < wanted {
                scratch.resize(wanted, 0.0);
            }
            if self.read_contiguous(&mut scratch[..wanted]) == 0 {
                break;
            }
        }
        self.skip_buffer = scratch;
    }

    fn read_contiguous(&mut self, out: &mut [f32]) -> usize {
        let channels = usize::from(self.channels);
        let mut written = 0usize;
        while written * channels < out.len() {
            let available = self
                .buffer
                .len()
                .saturating_sub(self.buffer_consumed * channels);
            if available == 0 {
                if self.exhausted || !self.decode_more() {
                    break;
                }
                continue;
            }
            let wanted = (out.len() - written * channels).min(available) / channels * channels;
            if wanted == 0 {
                break;
            }
            let from = self.buffer_consumed * channels;
            out[written * channels..written * channels + wanted]
                .copy_from_slice(&self.buffer[from..from + wanted]);
            self.buffer_consumed += wanted / channels;
            written += wanted / channels;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn a_memory_source_reads_from_any_frame() {
        let mut source = MemoryPcmSource::new(8_000, 2, (0..20).map(|i| i as f32).collect());
        assert_eq!(source.total_frames(), Some(10));
        let mut out = [0.0f32; 4];
        assert_eq!(source.read(3, &mut out), 2);
        assert_eq!(out, [6.0, 7.0, 8.0, 9.0]);
        assert_eq!(source.read(10, &mut out), 0);
        assert_eq!(source.duration(), Micros::from_frames(10, 8_000).into());
    }

    fn write_wav(path: &Path, frames: u32, rate: u32) {
        let data_bytes = frames * 2;
        let mut file = std::fs::File::create(path).expect("create");
        file.write_all(b"RIFF").expect("write");
        file.write_all(&(36 + data_bytes).to_le_bytes())
            .expect("write");
        file.write_all(b"WAVEfmt ").expect("write");
        file.write_all(&16u32.to_le_bytes()).expect("write");
        file.write_all(&1u16.to_le_bytes()).expect("write");
        file.write_all(&1u16.to_le_bytes()).expect("write");
        file.write_all(&rate.to_le_bytes()).expect("write");
        file.write_all(&(rate * 2).to_le_bytes()).expect("write");
        file.write_all(&2u16.to_le_bytes()).expect("write");
        file.write_all(&16u16.to_le_bytes()).expect("write");
        file.write_all(b"data").expect("write");
        file.write_all(&data_bytes.to_le_bytes()).expect("write");
        for frame in 0..frames {
            // A ramp, so every frame is identifiable after a seek.
            let sample = ((frame % 1000) as i16).wrapping_mul(32);
            file.write_all(&sample.to_le_bytes()).expect("write");
        }
        file.flush().expect("flush");
    }

    #[test]
    fn a_wav_source_reads_sequentially_and_after_a_seek() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ramp.wav");
        write_wav(&path, 48_000, 16_000);
        let mut source = SymphoniaPcmSource::open(&path).expect("open");
        assert_eq!(source.sample_rate(), 16_000);
        assert_eq!(source.channels(), 1);

        let mut head = [0.0f32; 8];
        assert_eq!(source.read(0, &mut head), 8);
        let expected = |frame: u32| f32::from(((frame % 1000) as i16).wrapping_mul(32)) / 32_768.0;
        for (index, value) in head.iter().enumerate() {
            assert!((value - expected(index as u32)).abs() < 1e-4, "{index}");
        }

        let mut later = [0.0f32; 8];
        assert_eq!(source.read(30_000, &mut later), 8);
        for (index, value) in later.iter().enumerate() {
            assert!(
                (value - expected(30_000 + index as u32)).abs() < 1e-4,
                "frame {index}: {value}"
            );
        }

        // Backwards seek.
        let mut back = [0.0f32; 4];
        assert_eq!(source.read(100, &mut back), 4);
        for (index, value) in back.iter().enumerate() {
            assert!(
                (value - expected(100 + index as u32)).abs() < 1e-4,
                "{index}"
            );
        }
    }

    #[test]
    fn reading_past_the_end_returns_short() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("short.wav");
        write_wav(&path, 100, 16_000);
        let mut source = SymphoniaPcmSource::open(&path).expect("open");
        let mut out = [0.0f32; 256];
        let got = source.read(0, &mut out);
        assert_eq!(got, 100);
        assert_eq!(source.read(100, &mut out), 0);
    }

    #[test]
    fn an_ogg_opus_recording_is_refused_with_a_reason() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/audio/valid/tone-mono-16k.opus");
        let error =
            SymphoniaPcmSource::open(&fixture).expect_err("symphonia 0.5 has no Opus decoder");
        assert!(matches!(error, SourceError::Unsupported { .. }), "{error}");

        let vorbis = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/audio/valid/tone-mono-16k.ogg");
        let source = SymphoniaPcmSource::open(&vorbis).expect("ogg vorbis plays");
        assert!(source.sample_rate() > 0);
    }

    #[test]
    fn a_file_that_is_not_audio_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"still not audio").expect("write");
        assert!(matches!(
            SymphoniaPcmSource::open(&path),
            Err(SourceError::Unsupported { .. })
        ));
        assert!(matches!(
            SymphoniaPcmSource::open(Path::new("/nope/missing.wav")),
            Err(SourceError::Open { .. })
        ));
    }
}
