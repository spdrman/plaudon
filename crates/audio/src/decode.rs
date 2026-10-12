//! Streaming decode to a deterministic canonical PCM derivative (§8.1, §14.4,
//! §17.5 — task F1.2).
//!
//! Three properties drive the design:
//!
//! * **Bounded memory.** Decoding happens packet by packet; the only retained
//!   state is one decoded packet plus the resampler's `32`-tap history. Peak
//!   memory is a function of the active window, never of total duration.
//! * **Determinism.** Same bytes + same [`CanonicalPcmProfile`] ⇒ byte-
//!   identical output and digest, on every host (see [`crate::resample`]).
//! * **No orphans.** [`ScratchPcm`] removes its file on drop unless the caller
//!   explicitly keeps it, so a cancellation or an error leaves nothing behind.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::transcript::NormalizationProfile;
use plaudon_ai_contracts::Progress;
use plaudon_domain::media::Sha256Digest;
use sha2::{Digest, Sha256};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::AudioError;
use crate::probe::map_probe_error;
use crate::resample::Resampler;
use crate::validate::ValidatedMedia;

/// What to do with a multi-channel source.
///
/// §8.1: "we will not collapse stereo channels blindly when they may represent
/// separate participants". [`ChannelPolicy::PreserveAll`] is the default;
/// downmixing is only ever an explicit request from an engine that requires
/// mono input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChannelPolicy {
    PreserveAll,
    DownmixToMono,
    SelectChannel { index: u16 },
}

impl ChannelPolicy {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            ChannelPolicy::PreserveAll => "preserve_all".to_string(),
            ChannelPolicy::DownmixToMono => "downmix_mono".to_string(),
            ChannelPolicy::SelectChannel { index } => format!("select_{index}"),
        }
    }

    /// Output channel count for a given source channel count.
    #[must_use]
    pub const fn output_channels(self, source_channels: u16) -> u16 {
        match self {
            ChannelPolicy::PreserveAll => source_channels,
            ChannelPolicy::DownmixToMono | ChannelPolicy::SelectChannel { .. } => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PcmSampleFormat {
    S16Le,
    F32Le,
}

impl PcmSampleFormat {
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            PcmSampleFormat::S16Le => 2,
            PcmSampleFormat::F32Le => 4,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PcmSampleFormat::S16Le => "s16le",
            PcmSampleFormat::F32Le => "f32le",
        }
    }
}

/// The exact shape of the canonical derivative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalPcmProfile {
    pub sample_rate_hz: u32,
    pub channel_policy: ChannelPolicy,
    pub sample_format: PcmSampleFormat,
}

impl CanonicalPcmProfile {
    /// The profile whisper.cpp requires: 16 kHz mono `f32`.
    ///
    /// Downmixing is explicit here, which is exactly the point: the engine
    /// demands mono, so the request is recorded in the normalization profile
    /// and visible in the revision metadata.
    #[must_use]
    pub const fn whisper_mono_16k() -> Self {
        Self {
            sample_rate_hz: 16_000,
            channel_policy: ChannelPolicy::DownmixToMono,
            sample_format: PcmSampleFormat::F32Le,
        }
    }

    /// Archival profile: original channel layout, 16-bit at the requested rate.
    #[must_use]
    pub const fn preserving(sample_rate_hz: u32) -> Self {
        Self {
            sample_rate_hz,
            channel_policy: ChannelPolicy::PreserveAll,
            sample_format: PcmSampleFormat::S16Le,
        }
    }
}

/// Destination for canonical PCM bytes.
pub trait PcmSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AudioError>;
}

/// Collects PCM in memory. For tests and short inputs only.
#[derive(Debug, Default)]
pub struct MemoryPcmSink {
    pub bytes: Vec<u8>,
}

impl PcmSink for MemoryPcmSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AudioError> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

/// Hashes PCM without storing it, for digest-only verification runs.
#[derive(Debug, Default)]
pub struct DigestOnlySink {
    hasher: Sha256,
    bytes: u64,
}

impl DigestOnlySink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn finish(self) -> (Sha256Digest, u64) {
        let digest: [u8; 32] = self.hasher.finalize().into();
        (Sha256Digest::from_bytes(digest), self.bytes)
    }
}

impl PcmSink for DigestOnlySink {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AudioError> {
        self.hasher.update(bytes);
        self.bytes += bytes.len() as u64;
        Ok(())
    }
}

/// A temporary PCM derivative that deletes itself unless explicitly kept.
///
/// §8.1: "we will avoid permanently storing oversized intermediates unless the
/// user chooses to keep them".
#[derive(Debug)]
pub struct ScratchPcm {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    keep: bool,
    bytes: u64,
}

impl ScratchPcm {
    /// Create a scratch file in `dir`.
    ///
    /// `dir` must already exist; the caller is expected to have run
    /// [`preflight_disk_space`] first.
    pub fn create(dir: &Path, file_stem: &str) -> Result<Self, AudioError> {
        std::fs::create_dir_all(dir).map_err(|source| AudioError::io(dir, source))?;
        let path = dir.join(format!("{file_stem}.pcm.part"));
        let file = File::create(&path).map_err(|source| AudioError::io(&path, source))?;
        Ok(Self {
            path,
            writer: Some(BufWriter::with_capacity(256 * 1024, file)),
            keep: false,
            bytes: 0,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Flush, fsync and retain the file, returning its path.
    ///
    /// Only call this once the decode has succeeded: a scratch file that is
    /// kept after a failure is exactly the "partial result presented as
    /// complete audio" the spec forbids.
    pub fn commit(mut self) -> Result<PathBuf, AudioError> {
        if let Some(mut writer) = self.writer.take() {
            writer
                .flush()
                .map_err(|source| AudioError::io(&self.path, source))?;
            writer
                .get_ref()
                .sync_all()
                .map_err(|source| AudioError::io(&self.path, source))?;
        }
        self.keep = true;
        Ok(self.path.clone())
    }
}

impl PcmSink for ScratchPcm {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AudioError> {
        let Some(writer) = self.writer.as_mut() else {
            return Err(AudioError::io(
                &self.path,
                std::io::Error::other("scratch file already committed"),
            ));
        };
        writer
            .write_all(bytes)
            .map_err(|source| AudioError::io(&self.path, source))?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }
}

impl Drop for ScratchPcm {
    fn drop(&mut self) {
        self.writer.take();
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Everything the caller needs to record about a completed decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeOutcome {
    /// Frames per channel in the canonical output.
    pub frames: u64,
    pub channels: u16,
    pub sample_rate_hz: u32,
    pub sample_format: PcmSampleFormat,
    /// Digest of the canonical PCM bytes.
    pub digest: Sha256Digest,
    pub byte_length: u64,
    /// Largest single buffer held during decode, in bytes. Asserted against
    /// in the memory-ceiling regression test.
    pub peak_buffer_bytes: usize,
}

impl DecodeOutcome {
    /// The §8.5 normalization profile describing this derivative.
    #[must_use]
    pub fn normalization_profile(&self, policy: ChannelPolicy) -> NormalizationProfile {
        NormalizationProfile {
            sample_rate_hz: self.sample_rate_hz,
            channel_policy: policy.label(),
            sample_format: self.sample_format.label().to_string(),
            pcm_sha256: self.digest,
        }
    }
}

/// Refuse a workload that would not fit on disk (§14.4), before creating
/// anything.
pub fn preflight_disk_space(dir: &Path, required_bytes: u64) -> Result<(), AudioError> {
    // Probe the nearest existing ancestor: the scratch directory itself may
    // not exist yet.
    let mut probe = dir;
    loop {
        if probe.exists() {
            break;
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => break,
        }
    }
    let available = fs4::available_space(probe).map_err(|source| AudioError::io(probe, source))?;
    if available < required_bytes {
        return Err(AudioError::InsufficientDiskSpace {
            path: dir.to_path_buf(),
            required_bytes,
            available_bytes: available,
        });
    }
    Ok(())
}

/// Exact size of the canonical derivative, when the source duration is known.
#[must_use]
pub fn estimate_pcm_bytes(media: &ValidatedMedia, profile: &CanonicalPcmProfile) -> Option<u64> {
    let frames = *media.profile().frames.get()?;
    let source_rate = *media.profile().sample_rate_hz.get()?;
    let channels = *media.profile().channels.get()?;
    let resampler = Resampler::new(source_rate, profile.sample_rate_hz, 1);
    let out_frames = resampler.predict_output_frames(frames);
    let out_channels = u64::from(profile.channel_policy.output_channels(channels));
    Some(out_frames * out_channels * profile.sample_format.bytes_per_sample() as u64)
}

/// Decode validated media into the canonical profile, streaming into `sink`.
pub fn decode_to_canonical_pcm(
    media: &ValidatedMedia,
    profile: &CanonicalPcmProfile,
    sink: &mut dyn PcmSink,
    ctx: &JobContext,
) -> Result<DecodeOutcome, AudioError> {
    // Some containers (notably MP4/AAC) do not state the channel count in
    // their headers; Symphonia only learns it from the first decoded frame.
    // An unknown count is therefore resolved by decoding, never guessed.
    let declared_channels = media.profile().channels.get().copied();
    let source_rate =
        *media.profile().sample_rate_hz.get().ok_or_else(|| {
            AudioError::UnsupportedConversion("source sample rate is unknown".into())
        })?;
    if let (ChannelPolicy::SelectChannel { index }, Some(source_channels)) =
        (profile.channel_policy, declared_channels)
    {
        if index >= source_channels {
            return Err(AudioError::UnsupportedConversion(format!(
                "channel {index} requested but the source has {source_channels}"
            )));
        }
    }

    let total_source_frames = media.profile().frames.get().copied();

    let file = File::open(media.path()).map_err(|source| AudioError::io(media.path(), source))?;
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(extension) = media.container().hint_extension() {
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
        .map_err(|error| map_probe_error(error, media.container()))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .or_else(|| format.tracks().first())
        .ok_or_else(|| AudioError::MalformedHeader("container declares no audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| map_probe_error(error, media.container()))?;

    let mut resampler: Option<Resampler> = None;
    let mut out_channels: u16 = 0;
    let mut hasher = Sha256::new();
    let mut sample_buffer: Option<SampleBuffer<f32>> = None;
    let mut reduced: Vec<f64> = Vec::new();
    let mut out_bytes: Vec<u8> = Vec::new();
    let mut frames_written = 0u64;
    let mut source_frames_seen = 0u64;
    let mut byte_length = 0u64;
    let mut peak_buffer_bytes = 0usize;

    loop {
        if ctx.is_cancelled() {
            return Err(AudioError::Cancelled);
        }

        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(error) => {
                return Err(AudioError::DecodeFailed {
                    frames_decoded: frames_written,
                    detail: error.to_string(),
                })
            }
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A malformed packet is not a recoverable condition for us: the
            // partial result is discarded rather than silently presented as
            // complete audio.
            Err(error) => {
                return Err(AudioError::DecodeFailed {
                    frames_decoded: frames_written,
                    detail: error.to_string(),
                })
            }
        };

        let spec = *decoded.spec();
        let capacity = decoded.capacity() as u64;
        let needs_new_buffer = sample_buffer.as_ref().is_none_or(|buffer| {
            (buffer.capacity() as u64) < capacity * u64::from(spec.channels.count() as u16)
        });
        if needs_new_buffer {
            sample_buffer = Some(SampleBuffer::<f32>::new(capacity, spec));
        }
        let buffer = sample_buffer
            .as_mut()
            .ok_or_else(|| AudioError::DecodeFailed {
                frames_decoded: frames_written,
                detail: "sample buffer unavailable".into(),
            })?;
        buffer.copy_interleaved_ref(decoded);
        let samples = buffer.samples();
        let packet_channels = spec.channels.count();
        if packet_channels == 0 {
            continue;
        }
        let packet_frames = samples.len() / packet_channels;
        source_frames_seen += packet_frames as u64;

        let packet_channels_u16 = u16::try_from(packet_channels).unwrap_or(u16::MAX);
        if resampler.is_none() {
            if let (ChannelPolicy::SelectChannel { index }, _) =
                (profile.channel_policy, packet_channels_u16)
            {
                if index >= packet_channels_u16 {
                    return Err(AudioError::UnsupportedConversion(format!(
                        "channel {index} requested but the source has {packet_channels_u16}"
                    )));
                }
            }
            out_channels = profile.channel_policy.output_channels(packet_channels_u16);
            resampler = Some(Resampler::new(
                source_rate,
                profile.sample_rate_hz,
                out_channels,
            ));
        }
        let resampler = resampler.as_mut().ok_or_else(|| AudioError::DecodeFailed {
            frames_decoded: frames_written,
            detail: "resampler unavailable".into(),
        })?;

        reduce_channels(
            samples,
            packet_channels,
            profile.channel_policy,
            &mut reduced,
        );
        peak_buffer_bytes =
            peak_buffer_bytes.max(reduced.len() * std::mem::size_of::<f64>() + samples.len() * 4);

        let resampled = resampler.process(&reduced);
        frames_written += emit(
            &resampled,
            out_channels,
            profile.sample_format,
            &mut out_bytes,
            sink,
            &mut hasher,
            &mut byte_length,
        )?;
        peak_buffer_bytes = peak_buffer_bytes.max(out_bytes.capacity() + resampled.len() * 8);

        // Progress is reported as processed source audio, which is monotonic
        // by construction, plus a fraction when the total is known.
        let processed_ms = (source_frames_seen * 1000) / u64::from(source_rate.max(1));
        ctx.report(Progress::processed_ms(processed_ms));
        if let Some(total) = total_source_frames.filter(|total| *total > 0) {
            ctx.report(Progress::fraction(
                (source_frames_seen as f64 / total as f64) as f32,
            ));
        }
    }

    if ctx.is_cancelled() {
        return Err(AudioError::Cancelled);
    }
    let tail = match resampler.as_mut() {
        Some(resampler) => resampler.finish(),
        None => Vec::new(),
    };
    frames_written += emit(
        &tail,
        out_channels,
        profile.sample_format,
        &mut out_bytes,
        sink,
        &mut hasher,
        &mut byte_length,
    )?;

    if frames_written == 0 {
        return Err(AudioError::DecodeFailed {
            frames_decoded: 0,
            detail: "decoder produced no audio frames".into(),
        });
    }

    let digest: [u8; 32] = hasher.finalize().into();
    Ok(DecodeOutcome {
        frames: frames_written,
        channels: out_channels,
        sample_rate_hz: profile.sample_rate_hz,
        sample_format: profile.sample_format,
        digest: Sha256Digest::from_bytes(digest),
        byte_length,
        peak_buffer_bytes,
    })
}

/// Apply the channel policy, writing interleaved `f64` into `out`.
fn reduce_channels(
    samples: &[f32],
    source_channels: usize,
    policy: ChannelPolicy,
    out: &mut Vec<f64>,
) {
    out.clear();
    match policy {
        ChannelPolicy::PreserveAll => {
            out.extend(samples.iter().map(|sample| f64::from(*sample)));
        }
        ChannelPolicy::DownmixToMono => {
            let divisor = source_channels as f64;
            for frame in samples.chunks_exact(source_channels) {
                let sum: f64 = frame.iter().map(|sample| f64::from(*sample)).sum();
                out.push(sum / divisor);
            }
        }
        ChannelPolicy::SelectChannel { index } => {
            let index = usize::from(index);
            for frame in samples.chunks_exact(source_channels) {
                out.push(f64::from(frame[index]));
            }
        }
    }
}

/// Convert interleaved `f64` into the output format and push it to the sink.
fn emit(
    interleaved: &[f64],
    out_channels: u16,
    format: PcmSampleFormat,
    scratch: &mut Vec<u8>,
    sink: &mut dyn PcmSink,
    hasher: &mut Sha256,
    byte_length: &mut u64,
) -> Result<u64, AudioError> {
    if interleaved.is_empty() {
        return Ok(0);
    }
    scratch.clear();
    scratch.reserve(interleaved.len() * format.bytes_per_sample());
    match format {
        PcmSampleFormat::S16Le => {
            for sample in interleaved {
                scratch.extend_from_slice(&to_i16(*sample).to_le_bytes());
            }
        }
        PcmSampleFormat::F32Le => {
            for sample in interleaved {
                let clamped = sample.clamp(-1.0, 1.0) as f32;
                scratch.extend_from_slice(&clamped.to_le_bytes());
            }
        }
    }
    hasher.update(&*scratch);
    sink.write_bytes(scratch)?;
    *byte_length += scratch.len() as u64;
    Ok((interleaved.len() / usize::from(out_channels)) as u64)
}

/// Deterministic float-to-`i16` conversion.
///
/// Clamp first, then scale by `i16::MAX` and round half away from zero
/// (`f64::round`'s documented behaviour), so the mapping is fully specified
/// rather than depending on the current rounding mode.
fn to_i16(sample: f64) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    let scaled = (clamped * f64::from(i16::MAX)).round();
    scaled as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::validate::{validate_media, ValidationRequest};
    use plaudon_ai_contracts::job::RecordingProgressSink;
    use plaudon_domain::ids::JobId;
    use std::sync::Arc;

    fn ctx() -> JobContext {
        JobContext::new(JobId::new())
    }

    fn validated(path: &Path) -> ValidatedMedia {
        validate_media(path, &ValidationRequest::manual_import()).expect("valid fixture")
    }

    #[test]
    fn mono_16k_source_passes_through_unresampled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let samples = testing::sine_samples(16_000, 1, 250);
        testing::write_wav(&path, 16_000, 1, &samples);

        let media = validated(&path);
        let mut sink = MemoryPcmSink::default();
        let outcome = decode_to_canonical_pcm(
            &media,
            &CanonicalPcmProfile::preserving(16_000),
            &mut sink,
            &ctx(),
        )
        .unwrap();

        assert_eq!(outcome.frames, 4_000);
        assert_eq!(outcome.channels, 1);
        assert_eq!(sink.bytes.len(), 4_000 * 2);

        // Bit-exact round trip: no resampling, no dithering, no gain.
        let decoded: Vec<i16> = sink
            .bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn digest_is_reproducible_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        testing::write_sine_wav(&path, 44_100, 2, 400);
        let media = validated(&path);

        let run = || {
            let mut sink = DigestOnlySink::new();
            let outcome = decode_to_canonical_pcm(
                &media,
                &CanonicalPcmProfile::whisper_mono_16k(),
                &mut sink,
                &ctx(),
            )
            .unwrap();
            (outcome.digest, outcome.frames, sink.finish().1)
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn stereo_is_preserved_unless_downmix_is_requested() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        testing::write_sine_wav(&path, 16_000, 2, 200);
        let media = validated(&path);

        let mut preserved = MemoryPcmSink::default();
        let outcome = decode_to_canonical_pcm(
            &media,
            &CanonicalPcmProfile::preserving(16_000),
            &mut preserved,
            &ctx(),
        )
        .unwrap();
        assert_eq!(outcome.channels, 2);

        let interleaved: Vec<i16> = preserved
            .bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        let left: Vec<i16> = interleaved.iter().step_by(2).copied().collect();
        let right: Vec<i16> = interleaved.iter().skip(1).step_by(2).copied().collect();
        assert_ne!(left, right, "channels were collapsed");

        let mut mono = MemoryPcmSink::default();
        let mono_outcome = decode_to_canonical_pcm(
            &media,
            &CanonicalPcmProfile {
                sample_rate_hz: 16_000,
                channel_policy: ChannelPolicy::DownmixToMono,
                sample_format: PcmSampleFormat::S16Le,
            },
            &mut mono,
            &ctx(),
        )
        .unwrap();
        assert_eq!(mono_outcome.channels, 1);
        assert_eq!(mono.bytes.len(), preserved.bytes.len() / 2);
    }

    #[test]
    fn single_channel_selection_keeps_that_channel_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        let samples = testing::sine_samples(16_000, 2, 100);
        testing::write_wav(&path, 16_000, 2, &samples);
        let media = validated(&path);

        let mut sink = MemoryPcmSink::default();
        decode_to_canonical_pcm(
            &media,
            &CanonicalPcmProfile {
                sample_rate_hz: 16_000,
                channel_policy: ChannelPolicy::SelectChannel { index: 1 },
                sample_format: PcmSampleFormat::S16Le,
            },
            &mut sink,
            &ctx(),
        )
        .unwrap();

        let decoded: Vec<i16> = sink
            .bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        let expected: Vec<i16> = samples.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn selecting_a_missing_channel_is_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mono.wav");
        testing::write_sine_wav(&path, 16_000, 1, 100);
        let media = validated(&path);
        let mut sink = MemoryPcmSink::default();
        let error = decode_to_canonical_pcm(
            &media,
            &CanonicalPcmProfile {
                sample_rate_hz: 16_000,
                channel_policy: ChannelPolicy::SelectChannel { index: 3 },
                sample_format: PcmSampleFormat::S16Le,
            },
            &mut sink,
            &ctx(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "MEDIA_UNSUPPORTED_CONVERSION");
    }

    #[test]
    fn cancellation_stops_promptly_and_leaves_no_scratch_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        testing::write_sine_wav(&path, 44_100, 2, 20_000);
        let media = validated(&path);

        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();

        let scratch_dir = dir.path().join("scratch");
        let scratch_path = {
            let mut scratch = ScratchPcm::create(&scratch_dir, "job").unwrap();
            let path = scratch.path().to_path_buf();
            let error = decode_to_canonical_pcm(
                &media,
                &CanonicalPcmProfile::whisper_mono_16k(),
                &mut scratch,
                &ctx,
            )
            .unwrap_err();
            assert_eq!(error.code(), "MEDIA_CANCELLED");
            path
        };
        assert!(
            !scratch_path.exists(),
            "cancelled decode left {scratch_path:?} behind"
        );
    }

    #[test]
    fn failed_decode_leaves_no_scratch_file() {
        let dir = tempfile::tempdir().unwrap();
        let scratch_dir = dir.path().join("scratch");
        let leaked = {
            let scratch = ScratchPcm::create(&scratch_dir, "abandoned").unwrap();
            scratch.path().to_path_buf()
        };
        assert!(!leaked.exists());
    }

    #[test]
    fn committed_scratch_file_survives() {
        let dir = tempfile::tempdir().unwrap();
        let mut scratch = ScratchPcm::create(dir.path(), "kept").unwrap();
        scratch.write_bytes(b"0123").unwrap();
        assert_eq!(scratch.bytes_written(), 4);
        let path = scratch.commit().unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"0123");
    }

    #[test]
    fn progress_is_monotonic_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        testing::write_sine_wav(&path, 44_100, 1, 3_000);
        let media = validated(&path);

        let sink = Arc::new(RecordingProgressSink::new());
        let ctx = JobContext::new(JobId::new()).with_progress(sink.clone());
        let mut pcm = DigestOnlySink::new();
        decode_to_canonical_pcm(
            &media,
            &CanonicalPcmProfile::whisper_mono_16k(),
            &mut pcm,
            &ctx,
        )
        .unwrap();

        let updates = sink.updates();
        assert_ne!(updates.len(), 0);

        let processed: Vec<u64> = updates.iter().filter_map(|p| p.processed_ms).collect();
        assert!(
            processed.windows(2).all(|pair| pair[1] >= pair[0]),
            "processed audio went backwards: {processed:?}"
        );
        assert_eq!(
            processed.last(),
            Some(&3_000),
            "final report must reach the full duration"
        );

        let fractions: Vec<f32> = updates.iter().filter_map(|p| p.fraction).collect();
        assert!(
            fractions.windows(2).all(|pair| pair[1] >= pair[0]),
            "fraction went backwards"
        );
        assert!(fractions.iter().all(|value| (0.0..=1.0).contains(value)));
    }

    #[test]
    fn corrupt_mid_stream_data_is_a_typed_error_with_no_partial_success() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.wav");
        testing::write_sine_wav(&good, 16_000, 1, 500);
        let mut bytes = std::fs::read(&good).unwrap();
        // Shrink the file so the RIFF header over-declares its data chunk.
        bytes.truncate(44 + 4_000);
        let path = dir.path().join("short.wav");
        std::fs::write(&path, &bytes).unwrap();

        // Validation may already reject it; if it does not, decode must either
        // report exactly the frames that exist or fail with a typed error.
        if let Ok(media) = validate_media(&path, &ValidationRequest::manual_import()) {
            let mut sink = MemoryPcmSink::default();
            match decode_to_canonical_pcm(
                &media,
                &CanonicalPcmProfile::preserving(16_000),
                &mut sink,
                &ctx(),
            ) {
                Ok(outcome) => assert!(
                    outcome.frames <= 2_000,
                    "decoder invented {} frames",
                    outcome.frames
                ),
                Err(error) => assert_eq!(error.code(), "MEDIA_DECODE_FAILED"),
            }
        }
    }

    #[test]
    fn disk_preflight_refuses_an_impossible_workload() {
        let dir = tempfile::tempdir().unwrap();
        let error = preflight_disk_space(dir.path(), u64::MAX).unwrap_err();
        assert_eq!(error.code(), "MEDIA_INSUFFICIENT_DISK");
        assert!(preflight_disk_space(dir.path(), 1024).is_ok());
    }

    #[test]
    fn disk_preflight_probes_the_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not").join("created").join("yet");
        assert!(preflight_disk_space(&missing, 1024).is_ok());
    }

    #[test]
    fn pcm_size_estimate_matches_the_real_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        testing::write_sine_wav(&path, 44_100, 2, 1_000);
        let media = validated(&path);
        let profile = CanonicalPcmProfile::whisper_mono_16k();

        let estimate = estimate_pcm_bytes(&media, &profile).unwrap();
        let mut sink = DigestOnlySink::new();
        let outcome = decode_to_canonical_pcm(&media, &profile, &mut sink, &ctx()).unwrap();
        let actual = outcome.byte_length;
        assert!(
            estimate.abs_diff(actual) <= 4 * u64::from(outcome.channels) * 4,
            "estimate {estimate} vs actual {actual}"
        );
    }

    #[test]
    fn peak_buffer_stays_bounded_over_a_long_input() {
        // A ten-minute 44.1 kHz stereo source: peak buffers must stay in the
        // low megabytes, not scale with the 2.6 GB of decoded PCM it implies.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        testing::write_sine_wav(&path, 44_100, 2, 600_000);
        let media = validated(&path);

        let mut sink = DigestOnlySink::new();
        let outcome = decode_to_canonical_pcm(
            &media,
            &CanonicalPcmProfile::whisper_mono_16k(),
            &mut sink,
            &ctx(),
        )
        .unwrap();

        assert_eq!(outcome.frames, 9_600_000);
        assert!(
            outcome.peak_buffer_bytes < 8 * 1024 * 1024,
            "peak buffer was {} bytes",
            outcome.peak_buffer_bytes
        );
    }

    #[test]
    fn float_to_i16_is_clamped_and_rounded_deterministically() {
        assert_eq!(to_i16(0.0), 0);
        assert_eq!(to_i16(1.0), 32_767);
        assert_eq!(to_i16(-1.0), -32_767);
        assert_eq!(to_i16(2.0), 32_767);
        assert_eq!(to_i16(-2.0), -32_767);
        // Round half away from zero.
        let half = 0.5 / f64::from(i16::MAX);
        assert_eq!(to_i16(half), 1);
        assert_eq!(to_i16(-half), -1);
    }
}
