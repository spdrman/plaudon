//! A1.4: decoder coverage for WAV, MP3, M4A/AAC, Opus and FLAC.
//!
//! The harness probes a real file with Symphonia and reports what the decoder
//! actually did, with three rules that match spec 8.1:
//!
//! * the container comes from content, never from the filename;
//! * a declared length is a claim, so allocation is bounded by bytes read;
//! * a canonical PCM derivative is byte-deterministic for one input and
//!   profile, and never collapses channels blindly.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Bytes examined when sniffing a container.
pub const SNIFF_WINDOW_BYTES: usize = 64 * 1024;

/// Maximum decoded samples the harness will hold.
///
/// A forged header cannot drive allocation past this, because decoding stops
/// and reports the cap rather than reserving what the file claims.
pub const MAX_DECODED_SAMPLES: usize = 16 * 1024 * 1024;

/// A container recognized from content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SniffedContainer {
    /// RIFF/WAVE.
    Wav,
    /// MPEG audio with or without an ID3 tag.
    Mp3,
    /// ISO base media file format.
    Mp4,
    /// Ogg carrying Vorbis.
    OggVorbis,
    /// Ogg carrying Opus.
    OggOpus,
    /// Native FLAC.
    Flac,
    /// Core Audio Format.
    Caf,
    /// Nothing recognizable.
    Unknown,
}

impl SniffedContainer {
    /// Extensions conventionally used for this container.
    #[must_use]
    pub const fn accepted_extensions(self) -> &'static [&'static str] {
        match self {
            Self::Wav => &["wav", "wave"],
            Self::Mp3 => &["mp3"],
            Self::Mp4 => &["m4a", "mp4", "m4b", "aac"],
            Self::OggVorbis => &["ogg", "oga"],
            Self::OggOpus => &["opus", "ogg"],
            Self::Flac => &["flac"],
            Self::Caf => &["caf"],
            Self::Unknown => &[],
        }
    }
}

/// Identify a container from a bounded byte prefix.
#[must_use]
pub fn sniff_container(bytes: &[u8]) -> SniffedContainer {
    let window = &bytes[..bytes.len().min(SNIFF_WINDOW_BYTES)];
    if window.len() < 12 {
        return SniffedContainer::Unknown;
    }
    if &window[..4] == b"RIFF" && &window[8..12] == b"WAVE" {
        return SniffedContainer::Wav;
    }
    if &window[..4] == b"fLaC" {
        return SniffedContainer::Flac;
    }
    if &window[..4] == b"caff" {
        return SniffedContainer::Caf;
    }
    if &window[..4] == b"OggS" {
        return if find(window, b"OpusHead").is_some() {
            SniffedContainer::OggOpus
        } else {
            SniffedContainer::OggVorbis
        };
    }
    if &window[4..8] == b"ftyp" {
        return SniffedContainer::Mp4;
    }
    if &window[..3] == b"ID3" {
        return SniffedContainer::Mp3;
    }
    if window
        .windows(2)
        .take(2048)
        .any(|pair| pair[0] == 0xFF && (pair[1] & 0xE0) == 0xE0 && (pair[1] & 0x18) != 0x08)
    {
        return SniffedContainer::Mp3;
    }
    SniffedContainer::Unknown
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Whether a filename extension is consistent with the sniffed container.
#[must_use]
pub fn extension_matches(container: SniffedContainer, extension: &str) -> bool {
    let normalized = extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    container
        .accepted_extensions()
        .contains(&normalized.as_str())
}

/// Why a probe or decode failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum SpikeError {
    /// The file has no bytes.
    ZeroLength,
    /// The container was not recognized.
    UnrecognizedContainer,
    /// The extension disagrees with the content.
    ExtensionContentMismatch {
        /// Extension on the filename.
        declared: String,
        /// Container found in the bytes.
        sniffed: SniffedContainer,
    },
    /// The container is recognized but no installed decoder handles its codec.
    UnsupportedCodec {
        /// Container the bytes are in.
        container: SniffedContainer,
    },
    /// The stream ends before its structure is complete.
    Truncated,
    /// The header is structurally invalid.
    MalformedHeader,
    /// A length declared inside the file disagrees with the bytes present.
    ///
    /// Symphonia decodes a truncated RIFF stream happily and simply returns
    /// fewer samples, so the declared-size check of spec 8.1 has to be ours.
    DeclaredSizeMismatch {
        /// Byte count the file declares.
        declared: u64,
        /// Byte count actually present.
        actual: u64,
    },
    /// A declared sample rate is outside any physically plausible range.
    ///
    /// Symphonia reports the header value verbatim, so a forged rate reaches
    /// the caller unless we reject it here.
    ImplausibleSampleRate {
        /// Rate the file declares.
        declared: u32,
    },
    /// Decoding would exceed the configured sample budget.
    AllocationLimitExceeded,
    /// The file could not be read.
    Io,
}

/// What a decoder reported about a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaProbe {
    /// Container found in the bytes.
    pub container: SniffedContainer,
    /// Codec the demuxer named.
    pub codec: String,
    /// Sample rate, when the stream declares one.
    pub sample_rate_hz: Option<u32>,
    /// Channel count, when the stream declares one.
    pub channels: Option<u16>,
    /// Duration derived from the decoded frame count.
    pub duration_ms: Option<u64>,
    /// Frames actually decoded.
    pub decoded_frames: u64,
    /// Length of the file on disk.
    pub byte_length: u64,
}

/// A deterministic canonical PCM derivative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPcm {
    /// Interleaved 16-bit samples, channels preserved.
    pub samples: Vec<i16>,
    /// Channel count of the derivative. Equal to the source's.
    pub channels: u16,
    /// Sample rate of the derivative. Equal to the source's.
    pub sample_rate_hz: u32,
}

impl CanonicalPcm {
    /// Digest of the derivative, for determinism assertions.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.channels.to_le_bytes());
        hasher.update(self.sample_rate_hz.to_le_bytes());
        for sample in &self.samples {
            hasher.update(sample.to_le_bytes());
        }
        hex_encode(&hasher.finalize())
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Probe and fully decode a file, returning both the metadata and the
/// canonical derivative.
#[allow(clippy::too_many_lines)] // One linear pipeline: sniff, validate, demux, decode.
pub fn probe_and_decode(path: &Path) -> Result<(MediaProbe, CanonicalPcm), SpikeError> {
    let bytes = std::fs::read(path).map_err(|_| SpikeError::Io)?;
    if bytes.is_empty() {
        return Err(SpikeError::ZeroLength);
    }
    let container = sniff_container(&bytes);
    if container == SniffedContainer::Unknown {
        return Err(SpikeError::UnrecognizedContainer);
    }
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        if !extension_matches(container, extension) {
            return Err(SpikeError::ExtensionContentMismatch {
                declared: extension.to_ascii_lowercase(),
                sniffed: container,
            });
        }
    }

    let byte_length = bytes.len() as u64;
    if container == SniffedContainer::Wav {
        validate_riff_lengths(&bytes)?;
    }
    let source = Box::new(std::io::Cursor::new(bytes));
    let stream = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(map_symphonia)?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or(SpikeError::UnsupportedCodec { container })?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|_| SpikeError::UnsupportedCodec { container })?;

    let mut samples: Vec<i16> = Vec::new();
    let mut channels: Option<u16> = None;
    let mut sample_rate: Option<u32> = None;
    let mut frames: u64 = 0;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(error) => return Err(map_symphonia(error)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let frame = match decoder.decode(&packet) {
            Ok(frame) => frame,
            // A damaged packet mid-stream is skipped by design; a damaged
            // stream surfaces as a format error above.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(map_symphonia(error)),
        };
        let spec = *frame.spec();
        channels.get_or_insert(u16::try_from(spec.channels.count()).unwrap_or(u16::MAX));
        sample_rate.get_or_insert(spec.rate);

        let mut buffer = SampleBuffer::<i16>::new(frame.capacity() as u64, spec);
        buffer.copy_interleaved_ref(frame);
        if samples.len() + buffer.samples().len() > MAX_DECODED_SAMPLES {
            return Err(SpikeError::AllocationLimitExceeded);
        }
        samples.extend_from_slice(buffer.samples());
        frames += (buffer.samples().len() / spec.channels.count().max(1)) as u64;
    }

    if samples.is_empty() {
        return Err(SpikeError::Truncated);
    }

    let channels = channels.unwrap_or(1);
    let sample_rate_hz = sample_rate.unwrap_or(0);
    if !PLAUSIBLE_SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
        return Err(SpikeError::ImplausibleSampleRate {
            declared: sample_rate_hz,
        });
    }
    let duration_ms = if sample_rate_hz == 0 {
        None
    } else {
        Some(frames * 1000 / u64::from(sample_rate_hz))
    };

    Ok((
        MediaProbe {
            container,
            codec: format!("{:?}", codec_params.codec),
            sample_rate_hz: sample_rate,
            channels: Some(channels),
            duration_ms,
            decoded_frames: frames,
            byte_length,
        },
        CanonicalPcm {
            samples,
            channels,
            sample_rate_hz,
        },
    ))
}

/// Sample rates a real recording can plausibly carry.
///
/// Narrow enough to catch a forged header, wide enough to admit telephony at
/// one end and high-resolution capture at the other.
pub const PLAUSIBLE_SAMPLE_RATES_HZ: std::ops::RangeInclusive<u32> = 4_000..=768_000;

/// Check a RIFF/WAVE file's declared lengths against the bytes present.
///
/// The RIFF size field covers everything after the first eight bytes, and the
/// `data` chunk length covers the sample payload. Either one overrunning the
/// file is a truncation or a forgery, and both must fail before decoding.
fn validate_riff_lengths(bytes: &[u8]) -> Result<(), SpikeError> {
    let actual = bytes.len() as u64;
    if bytes.len() < 12 {
        return Err(SpikeError::Truncated);
    }
    let riff_size = u64::from(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
    let declared_total = riff_size + 8;
    if declared_total > actual {
        return Err(SpikeError::DeclaredSizeMismatch {
            declared: declared_total,
            actual,
        });
    }

    // Walk the chunk list rather than searching for the literal "data", so a
    // `data` sequence inside sample bytes cannot be mistaken for a header.
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]);
        let payload_start = offset + 8;
        let declared_end = payload_start as u64 + u64::from(size);
        if id == b"data" && declared_end > actual {
            return Err(SpikeError::DeclaredSizeMismatch {
                declared: declared_end,
                actual,
            });
        }
        // Chunks are word aligned.
        let advance =
            8usize + usize::try_from(size).unwrap_or(usize::MAX) + usize::from(size % 2 == 1);
        offset = match offset.checked_add(advance) {
            Some(next) if next > offset => next,
            _ => break,
        };
    }
    Ok(())
}

fn map_symphonia(error: SymphoniaError) -> SpikeError {
    match error {
        SymphoniaError::Unsupported(_) => SpikeError::UnsupportedCodec {
            container: SniffedContainer::Unknown,
        },
        SymphoniaError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            SpikeError::Truncated
        }
        SymphoniaError::IoError(_) => SpikeError::Io,
        SymphoniaError::DecodeError(_) | SymphoniaError::SeekError(_) => {
            SpikeError::MalformedHeader
        }
        SymphoniaError::LimitError(_) => SpikeError::AllocationLimitExceeded,
        SymphoniaError::ResetRequired => SpikeError::MalformedHeader,
    }
}

/// One row of the support matrix the spike produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupportRow {
    /// Fixture the row was produced from.
    pub fixture: String,
    /// Container found in the bytes.
    pub container: SniffedContainer,
    /// Whether Symphonia decoded it with the features we enable.
    pub decoded: bool,
    /// Codec name, when decoding succeeded.
    pub codec: Option<String>,
    /// Sample rate, when decoding succeeded.
    pub sample_rate_hz: Option<u32>,
    /// Channel count, when decoding succeeded.
    pub channels: Option<u16>,
    /// Duration in milliseconds, when decoding succeeded.
    pub duration_ms: Option<u64>,
    /// Failure, when decoding did not succeed.
    pub error: Option<SpikeError>,
}

/// Build a support row for one fixture.
#[must_use]
pub fn support_row(path: &Path) -> SupportRow {
    let fixture = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let container =
        std::fs::read(path).map_or(SniffedContainer::Unknown, |bytes| sniff_container(&bytes));
    match probe_and_decode(path) {
        Ok((probe, _)) => SupportRow {
            fixture,
            container: probe.container,
            decoded: true,
            codec: Some(probe.codec),
            sample_rate_hz: probe.sample_rate_hz,
            channels: probe.channels,
            duration_ms: probe.duration_ms,
            error: None,
        },
        Err(error) => SupportRow {
            fixture,
            container,
            decoded: false,
            codec: None,
            sample_rate_hz: None,
            channels: None,
            duration_ms: None,
            error: Some(error),
        },
    }
}
