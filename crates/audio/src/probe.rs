//! Container/codec probing and metadata extraction (§8.1, F1.1).
//!
//! Built on Symphonia, the permissively licensed pure-Rust decoder the spec
//! names. Every value the probe cannot establish is reported as
//! [`Known::Unknown`] rather than guessed.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use plaudon_ai_contracts::Known;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::AudioError;
use crate::sniff::{sniff_container, SniffedContainer, SNIFF_WINDOW_BYTES};

/// How the duration was established, so the UI never presents an estimate as
/// a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationSource {
    /// Declared by a container/stream header (e.g. WAV data size, Xing frame
    /// count, MP4 `mvhd`).
    ContainerHeader,
    /// Counted by walking every packet without decoding.
    PacketScan,
}

/// Everything media validation establishes about a file's shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MediaProfile {
    pub container: SniffedContainer,
    /// Symphonia short codec name, e.g. `pcm_s16le`, `mp3`, `aac`.
    pub codec: Known<String>,
    pub sample_rate_hz: Known<u32>,
    pub channels: Known<u16>,
    pub duration_ms: Known<u64>,
    pub duration_source: Option<DurationSource>,
    /// Frames per channel.
    pub frames: Known<u64>,
    pub bitrate_bps: Known<u64>,
    pub bits_per_sample: Known<u32>,
    pub byte_length: u64,
}

impl MediaProfile {
    /// Channel count as declared, never collapsed. §8.1: "we will not collapse
    /// stereo channels blindly when they may represent separate participants".
    #[must_use]
    pub fn is_multichannel(&self) -> bool {
        self.channels.get().is_some_and(|channels| *channels > 1)
    }
}

/// Upper bound on packets walked during a duration scan. A one-hour MP3 is
/// roughly 138 000 packets; this cap is three orders of magnitude above that
/// and exists purely so a hostile file cannot spin forever.
const MAX_SCAN_PACKETS: u64 = 100_000_000;

/// Probe a file on disk.
///
/// The container is determined by sniffing the byte prefix; the sniffed
/// container (not the extension) seeds Symphonia's hint.
pub fn probe_path(path: &Path) -> Result<MediaProfile, AudioError> {
    let byte_length = std::fs::metadata(path)
        .map_err(|source| AudioError::io(path, source))?
        .len();
    if byte_length == 0 {
        return Err(AudioError::ZeroLength {
            path: path.to_path_buf(),
        });
    }

    let mut prefix = vec![0u8; SNIFF_WINDOW_BYTES.min(byte_length as usize)];
    {
        let mut file = File::open(path).map_err(|source| AudioError::io(path, source))?;
        let mut filled = 0usize;
        while filled < prefix.len() {
            match file.read(&mut prefix[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(source) => return Err(AudioError::io(path, source)),
            }
        }
        prefix.truncate(filled);
    }
    let container = sniff_container(&prefix);
    if container == SniffedContainer::Unknown {
        return Err(AudioError::UnrecognisedContainer);
    }

    let file = File::open(path).map_err(|source| AudioError::io(path, source))?;
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    probe_stream(stream, container, byte_length)
}

/// Probe an in-memory buffer. Used by fuzz targets and fixtures.
pub fn probe_bytes(bytes: Vec<u8>) -> Result<MediaProfile, AudioError> {
    let byte_length = bytes.len() as u64;
    if byte_length == 0 {
        return Err(AudioError::ZeroLength {
            path: "<memory>".into(),
        });
    }
    let container = sniff_container(&bytes);
    if container == SniffedContainer::Unknown {
        return Err(AudioError::UnrecognisedContainer);
    }
    let stream = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes)),
        MediaSourceStreamOptions::default(),
    );
    probe_stream(stream, container, byte_length)
}

fn probe_stream(
    stream: MediaSourceStream,
    container: SniffedContainer,
    byte_length: u64,
) -> Result<MediaProfile, AudioError> {
    let mut hint = Hint::new();
    if let Some(extension) = container.hint_extension() {
        hint.with_extension(extension);
    }

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
        .map_err(|error| map_probe_error(error, container))?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .or_else(|| format.tracks().first())
        .ok_or_else(|| AudioError::MalformedHeader("container declares no audio track".into()))?;
    let track_id = track.id;
    let params = track.codec_params.clone();

    let codec_name = codec_short_name(&params);
    let sample_rate_hz: Known<u32> = params.sample_rate.into();
    let channels: Known<u16> = params
        .channels
        .map(|channels| channels.count() as u16)
        .into();
    let bits_per_sample: Known<u32> = params.bits_per_sample.into();

    let (frames, duration_source) = resolve_frames(&params, &mut *format, track_id)?;

    let duration_ms = match (&frames, &sample_rate_hz) {
        (Known::Known(frames), Known::Known(rate)) if *rate > 0 => {
            Known::Known((*frames * 1000) / u64::from(*rate))
        }
        _ => Known::Unknown,
    };

    let bitrate_bps = match &duration_ms {
        Known::Known(ms) if *ms > 0 => Known::Known((byte_length * 8 * 1000) / ms),
        _ => Known::Unknown,
    };
    // An uncompressed stream's declared length is checkable against the bytes
    // that actually exist. A WAV whose `data` chunk header promises an hour
    // but whose file stops after a minute is truncated, and Symphonia will
    // happily report the promised duration from the header alone.
    if let (
        Some(codec),
        Some(declared_frames),
        Some(declared_channels),
        Some(declared_bits),
        Some(DurationSource::ContainerHeader),
    ) = (
        codec_name.get(),
        frames.get(),
        channels.get(),
        bits_per_sample.get(),
        duration_source,
    ) {
        if codec.starts_with("pcm") {
            let required = declared_frames
                .saturating_mul(u64::from(*declared_channels))
                .saturating_mul(u64::from(declared_bits.div_ceil(8)));
            if required > byte_length {
                return Err(AudioError::Truncated {
                    detail: format!(
                        "header declares {declared_frames} frames needing {required} bytes of \
                         audio data, but the file is only {byte_length} bytes"
                    ),
                });
            }
        }
    }

    Ok(MediaProfile {
        container,
        codec: codec_name,
        sample_rate_hz,
        channels,
        duration_ms,
        duration_source,
        frames,
        bitrate_bps,
        bits_per_sample,
        byte_length,
    })
}

fn resolve_frames(
    params: &CodecParameters,
    format: &mut dyn FormatReader,
    track_id: u32,
) -> Result<(Known<u64>, Option<DurationSource>), AudioError> {
    if let Some(n_frames) = params.n_frames {
        if n_frames > 0 {
            return Ok((
                Known::Known(n_frames),
                Some(DurationSource::ContainerHeader),
            ));
        }
    }

    // Walk packets without decoding. This is exact and allocates nothing
    // beyond one packet at a time.
    let mut frames = 0u64;
    let mut packets = 0u64;
    loop {
        match format.next_packet() {
            Ok(packet) => {
                if packet.track_id() != track_id {
                    continue;
                }
                frames = frames.saturating_add(packet.dur());
                packets += 1;
                if packets > MAX_SCAN_PACKETS {
                    return Ok((Known::Unknown, None));
                }
            }
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                // Symphonia signals clean end-of-stream this way.
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(error) => return Err(map_probe_error(error, SniffedContainer::Unknown)),
        }
    }

    if frames == 0 {
        return Ok((Known::Unknown, None));
    }
    Ok((Known::Known(frames), Some(DurationSource::PacketScan)))
}

fn codec_short_name(params: &CodecParameters) -> Known<String> {
    symphonia::default::get_codecs()
        .get_codec(params.codec)
        .map(|descriptor| descriptor.short_name.to_string())
        .into()
}

pub(crate) fn map_probe_error(error: SymphoniaError, container: SniffedContainer) -> AudioError {
    match error {
        SymphoniaError::Unsupported(reason) => AudioError::UnsupportedCodec {
            codec: container.as_str().to_string(),
            reason: format!(
                "{reason}; no bundled decoder handles this stream and no optional external \
                 runtime is installed"
            ),
        },
        SymphoniaError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            AudioError::Truncated {
                detail: "stream ends before the container's declared data".into(),
            }
        }
        SymphoniaError::IoError(io) => AudioError::Io {
            path: "<stream>".into(),
            source: io,
        },
        SymphoniaError::DecodeError(reason) => AudioError::MalformedHeader(reason.to_string()),
        SymphoniaError::SeekError(_) => {
            AudioError::MalformedHeader("stream does not support the required seek".into())
        }
        SymphoniaError::LimitError(reason) => AudioError::AllocationLimitExceeded {
            requested: 0,
            limit: 0,
        }
        .tagged(reason),
        SymphoniaError::ResetRequired => {
            AudioError::MalformedHeader("stream requires a decoder reset mid-probe".into())
        }
    }
}

trait Tagged {
    fn tagged(self, detail: &str) -> AudioError;
}

impl Tagged for AudioError {
    fn tagged(self, detail: &str) -> AudioError {
        match self {
            AudioError::AllocationLimitExceeded { .. } => {
                AudioError::MalformedHeader(format!("decoder limit reached: {detail}"))
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn probes_a_mono_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        testing::write_sine_wav(&path, 16_000, 1, 2000);

        let profile = probe_path(&path).unwrap();
        assert_eq!(profile.container, SniffedContainer::Wav);
        assert_eq!(profile.sample_rate_hz, Known::Known(16_000));
        assert_eq!(profile.channels, Known::Known(1));
        assert_eq!(profile.duration_ms, Known::Known(2000));
        assert_eq!(profile.frames, Known::Known(32_000));
        assert!(!profile.is_multichannel());
    }

    #[test]
    fn preserves_stereo_channel_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        testing::write_sine_wav(&path, 44_100, 2, 1000);

        let profile = probe_path(&path).unwrap();
        assert_eq!(profile.channels, Known::Known(2));
        assert!(profile.is_multichannel());
        assert_eq!(profile.sample_rate_hz, Known::Known(44_100));
    }

    #[test]
    fn empty_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.wav");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(probe_path(&path).unwrap_err().code(), "MEDIA_EMPTY");
    }

    #[test]
    fn non_audio_bytes_are_unrecognised() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"this is plainly not audio, repeatedly, at length.").unwrap();
        assert_eq!(probe_path(&path).unwrap_err().code(), "MEDIA_UNRECOGNISED");
    }

    #[test]
    fn truncated_wav_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        testing::write_sine_wav(&path, 16_000, 1, 2000);
        let bytes = std::fs::read(&path).unwrap();
        let truncated = dir.path().join("truncated.wav");
        std::fs::write(&truncated, &bytes[..bytes.len() / 3]).unwrap();

        let profile = probe_path(&truncated);
        match profile {
            // The RIFF header declares more data than exists.
            Err(error) => assert!(
                matches!(error.code(), "MEDIA_TRUNCATED" | "MEDIA_MALFORMED_HEADER"),
                "unexpected error: {error}"
            ),
            Ok(profile) => {
                // If Symphonia tolerates it, the frame count must reflect the
                // bytes that actually exist, never the declared length.
                let frames = *profile.frames.get().unwrap();
                assert!(frames < 32_000, "truncated file reported full length");
            }
        }
    }

    #[test]
    fn probing_is_deterministic_for_identical_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        testing::write_sine_wav(&path, 16_000, 1, 500);
        let first = probe_path(&path).unwrap();
        let second = probe_path(&path).unwrap();
        assert_eq!(first, second);
    }
}
