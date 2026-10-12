//! Opus decoding (RFC 6716 packets, RFC 7845 Ogg encapsulation) — task O1.6.
//!
//! Symphonia demuxes Ogg Opus and identifies the track, but ships no Opus
//! decoder for any feature set (`docs/spikes/O1.2-audio-playback-and-opus.md`
//! §2). PLAUD device `.ASR` sidecars are remuxed to Ogg Opus by
//! `plaudon_plaud_protocol::remux_to_ogg_opus`, so without a decoder the
//! device path cannot be played, cannot produce a waveform and cannot be
//! transcribed. This module closes that gap with the pure-Rust
//! [`opus_pure`] crate (BSD-3-Clause, no dependencies, no build script, no C
//! toolchain and no system library on any platform we ship).
//!
//! Two layers live here:
//!
//! * [`OpusPacketDecoder`] — a container-independent packet decoder with the
//!   hostile-input contract this repository requires: every length is checked
//!   before it is trusted, every buffer is allocated once at construction from
//!   a fixed cap, and every failure is a typed error rather than a panic or a
//!   silent zero-fill.
//! * [`OpusAudioDecoder`] — the Symphonia [`Decoder`] implementation that
//!   registers the packet decoder into [`crate::codecs::registry`], so
//!   `decode_to_canonical_pcm`, `plaudon_audio_playback::build_peaks`,
//!   `SymphoniaPcmSource` and the STT preprocessing path all gain Opus
//!   support through the seam they already use.
//!
//! ## Threat model
//!
//! The bytes are untrusted (five acquisition routes, all hostile domains).
//! ADR-0015 requires untrusted *native* decoders to run out of process,
//! because a C codec's memory safety is unknowable from the Rust around it.
//! This decoder is Rust: no FFI and no C build, with the dependency's only
//! `unsafe` in SIMD kernels that its own test suite pins against their scalar
//! definitions. The in-process posture is therefore the one the rest of the
//! Symphonia stack already has, not a weakening of it, and ADR-0015's helper
//! remains the route for any native codec. The caps below bound work and
//! memory regardless of what the file claims:
//!
//! | Bound | Value | Why |
//! | --- | --- | --- |
//! | [`MAX_PACKET_BYTES`] | 65 536 | RFC 6716 §3.2.5 allows at most 48 frames of 1275 bytes; anything larger is forged framing |
//! | [`MAX_FRAME_SAMPLES_PER_CHANNEL`] | 5 760 | 120 ms at 48 kHz, the longest Opus frame |
//! | [`MAX_CHANNELS`] | 8 | RFC 7845 §5.1.1 channel mapping family 1 |
//!
//! Decoding allocates nothing per packet: the interleaved scratch buffer is
//! sized once from those caps.

use std::fmt;

use opus_pure::{ChannelLayout, Error as OpusCrateError, OpusDecoder, OpusMSDecoder};
use symphonia::core::audio::SignalSpec;
use symphonia::core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal};
use symphonia::core::codecs::{
    CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_OPUS,
};
use symphonia::core::errors::{decode_error, unsupported_error, Result as SymphoniaResult};
use symphonia::core::formats::Packet;
use symphonia::core::support_codec;

/// Rate every Opus stream is decoded at. Opus is defined at 48 kHz; the rate
/// in `OpusHead` describes the *original* input and is informational only
/// (RFC 7845 §5.1). Symphonia reports the track as 48 kHz for the same
/// reason, and `crate::resample` converts to the canonical profile.
pub const DECODE_RATE_HZ: u32 = 48_000;

/// Largest packet we will hand to the decoder.
///
/// RFC 6716 §3.2.5: a code-3 packet carries at most 48 frames, and a frame is
/// at most 1275 bytes, so 61 200 bytes plus framing is the true ceiling. The
/// cap is rounded up to 64 KiB and enforced before any work is done.
pub const MAX_PACKET_BYTES: usize = 65_536;

/// Longest Opus frame, per channel: 120 ms at 48 kHz.
pub const MAX_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;

/// Largest channel count RFC 7845 §5.1.1 defines a mapping for.
pub const MAX_CHANNELS: usize = 8;

/// Minimum size of an `OpusHead` identification header (RFC 7845 §5.1).
const OPUS_HEAD_MIN_BYTES: usize = 19;

/// `OpusHead` magic signature.
const OPUS_HEAD_MAGIC: &[u8; 8] = b"OpusHead";

/// Why an Opus stream or packet was refused.
///
/// Every variant is a *refusal*: the decoder never guesses, never pads with
/// silence and never returns a partially decoded frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpusDecodeError {
    /// The `OpusHead` identification header is absent, short or inconsistent.
    #[error("malformed Opus identification header: {0}")]
    MalformedHeader(&'static str),
    /// The header is well formed but describes a stream shape this build does
    /// not decode (a reserved channel mapping family, for example).
    #[error("unsupported Opus stream: {0}")]
    UnsupportedStream(&'static str),
    /// A packet was larger than [`MAX_PACKET_BYTES`].
    #[error("Opus packet is {length} bytes, over the {MAX_PACKET_BYTES}-byte limit")]
    PacketTooLarge {
        /// Size of the offending packet.
        length: usize,
    },
    /// A zero-length packet. In a file this is forged framing, not the
    /// deliberate loss signal an RTP receiver would conceal.
    #[error("Opus packet is empty")]
    EmptyPacket,
    /// The packet's table of contents or frame lengths do not parse.
    #[error("malformed Opus packet")]
    MalformedPacket,
    /// The packet declares more samples than [`MAX_FRAME_SAMPLES_PER_CHANNEL`].
    #[error("Opus packet declares {samples} samples per channel, over the {MAX_FRAME_SAMPLES_PER_CHANNEL} limit")]
    FrameTooLong {
        /// Declared samples per channel.
        samples: usize,
    },
    /// The decoder reached a state it does not implement. Reported rather
    /// than approximated.
    #[error("Opus decoder internal error")]
    Internal,
}

impl OpusDecodeError {
    /// Stable short reason, used when the error has to cross Symphonia's
    /// `&'static str` error boundary.
    #[must_use]
    pub const fn detail(&self) -> &'static str {
        match self {
            Self::MalformedHeader(_) => "opus: malformed identification header",
            Self::UnsupportedStream(_) => "opus: unsupported stream shape",
            Self::PacketTooLarge { .. } => "opus: packet over the size limit",
            Self::EmptyPacket => "opus: empty packet",
            Self::MalformedPacket => "opus: malformed packet",
            Self::FrameTooLong { .. } => "opus: frame over the length limit",
            Self::Internal => "opus: internal decoder error",
        }
    }
}

/// How the coded streams map onto output channels (RFC 7845 §5.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMapping {
    /// Family 0: one mono stream, or one stereo-coupled stream.
    Rtp,
    /// Family 1: `streams` Opus streams, the first `coupled` of them stereo,
    /// demultiplexed onto output channels through `mapping`.
    Vorbis {
        /// Total coded streams.
        streams: u8,
        /// Stereo-coupled streams, always the first ones.
        coupled: u8,
        /// `table[out_channel]` indexes the interleaved decoded channels;
        /// `255` means "output silence". Only the first `channels` entries
        /// are meaningful.
        table: [u8; MAX_CHANNELS],
    },
}

/// The shape of an Opus stream, established from its `OpusHead` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusStreamParams {
    channels: u8,
    pre_skip: u16,
    input_rate_hz: u32,
    mapping: ChannelMapping,
}

impl OpusStreamParams {
    /// A plain family-0 stream: mono or stereo, no pre-skip.
    ///
    /// Used when a container states the channel count but carries no
    /// `OpusHead` (no container we accept does today, so this is the honest
    /// fallback rather than a guess at a mapping table).
    pub fn rtp(channels: u8) -> Result<Self, OpusDecodeError> {
        if channels == 0 || channels > 2 {
            return Err(OpusDecodeError::UnsupportedStream(
                "channel mapping family 0 defines 1 or 2 channels",
            ));
        }
        Ok(Self {
            channels,
            pre_skip: 0,
            input_rate_hz: DECODE_RATE_HZ,
            mapping: ChannelMapping::Rtp,
        })
    }

    /// Output channel count, 1..=[`MAX_CHANNELS`].
    #[must_use]
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    /// Samples at 48 kHz the container must discard from the start of the
    /// stream (RFC 7845 §4.2). Symphonia applies this as the track delay when
    /// gapless playback is enabled, so [`OpusPacketDecoder`] deliberately does
    /// not trim: trimming in both places would cut the recording twice.
    #[must_use]
    pub const fn pre_skip(&self) -> u16 {
        self.pre_skip
    }

    /// The sample rate of the audio before it was encoded. Informational:
    /// Opus always decodes at [`DECODE_RATE_HZ`].
    #[must_use]
    pub const fn input_rate_hz(&self) -> u32 {
        self.input_rate_hz
    }

    /// The channel mapping.
    #[must_use]
    pub const fn mapping(&self) -> ChannelMapping {
        self.mapping
    }
}

/// Parse an `OpusHead` identification header.
///
/// Every field is validated against RFC 7845 §5.1 before it is used, because
/// the header arrives from the file: a forged channel count, a reserved
/// mapping family, a stream count that overflows the decoded-channel space or
/// a mapping entry pointing past the decoded channels are all refused here,
/// before a decoder exists and before a buffer is sized.
pub fn parse_opus_head(bytes: &[u8]) -> Result<OpusStreamParams, OpusDecodeError> {
    if bytes.len() < OPUS_HEAD_MIN_BYTES {
        return Err(OpusDecodeError::MalformedHeader(
            "shorter than the 19-byte minimum",
        ));
    }
    if &bytes[..8] != OPUS_HEAD_MAGIC {
        return Err(OpusDecodeError::MalformedHeader("missing OpusHead magic"));
    }
    // The major version is the upper nibble; RFC 7845 §5.1 says a decoder
    // must reject anything above major version 0.
    if bytes[8] >> 4 != 0 {
        return Err(OpusDecodeError::UnsupportedStream(
            "encapsulation version above 0",
        ));
    }
    let channels = bytes[9];
    if channels == 0 {
        return Err(OpusDecodeError::MalformedHeader("zero channels"));
    }
    if usize::from(channels) > MAX_CHANNELS {
        return Err(OpusDecodeError::UnsupportedStream(
            "more channels than RFC 7845 defines a mapping for",
        ));
    }
    let pre_skip = u16::from_le_bytes([bytes[10], bytes[11]]);
    let input_rate_hz = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    // bytes[16..18] is the output gain, which we do not apply: applying it
    // would make our PCM differ from the libopus reference the differential
    // test compares against, and no file we handle sets it.
    let family = bytes[18];

    let mapping = match family {
        0 => {
            if channels > 2 {
                return Err(OpusDecodeError::MalformedHeader(
                    "channel mapping family 0 defines 1 or 2 channels",
                ));
            }
            ChannelMapping::Rtp
        }
        1 => {
            let table_start = OPUS_HEAD_MIN_BYTES + 2;
            let needed = table_start + usize::from(channels);
            if bytes.len() < needed {
                return Err(OpusDecodeError::MalformedHeader(
                    "channel mapping table is truncated",
                ));
            }
            let streams = bytes[OPUS_HEAD_MIN_BYTES];
            let coupled = bytes[OPUS_HEAD_MIN_BYTES + 1];
            if streams == 0 {
                return Err(OpusDecodeError::MalformedHeader("zero coded streams"));
            }
            if coupled > streams {
                return Err(OpusDecodeError::MalformedHeader(
                    "more coupled streams than streams",
                ));
            }
            // Every coupled stream decodes two channels, every other one.
            let decoded_channels = usize::from(streams) + usize::from(coupled);
            if decoded_channels > 2 * MAX_CHANNELS {
                return Err(OpusDecodeError::UnsupportedStream(
                    "more coded streams than the channel cap allows",
                ));
            }
            let mut table = [255u8; MAX_CHANNELS];
            for (slot, byte) in table
                .iter_mut()
                .zip(&bytes[table_start..table_start + usize::from(channels)])
            {
                if *byte != 255 && usize::from(*byte) >= decoded_channels {
                    return Err(OpusDecodeError::MalformedHeader(
                        "channel mapping points past the decoded channels",
                    ));
                }
                *slot = *byte;
            }
            ChannelMapping::Vorbis {
                streams,
                coupled,
                table,
            }
        }
        _ => {
            return Err(OpusDecodeError::UnsupportedStream(
                "reserved channel mapping family",
            ))
        }
    };

    Ok(OpusStreamParams {
        channels,
        pre_skip,
        input_rate_hz,
        mapping,
    })
}

enum Inner {
    Single(Box<OpusDecoder>),
    Multi(Box<OpusMSDecoder>),
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Single(_) => f.write_str("Inner::Single"),
            Self::Multi(_) => f.write_str("Inner::Multi"),
        }
    }
}

/// A container-independent Opus packet decoder.
///
/// Decodes one RFC 6716 packet per call into interleaved `f32` at
/// [`DECODE_RATE_HZ`]. The output buffer is allocated once, from the caps, so
/// a hostile stream cannot drive allocation at all.
#[derive(Debug)]
pub struct OpusPacketDecoder {
    inner: Inner,
    params: OpusStreamParams,
    /// Interleaved output, `MAX_FRAME_SAMPLES_PER_CHANNEL * channels` long.
    scratch: Vec<f32>,
    /// Samples per channel produced by the last successful decode.
    produced: usize,
}

impl OpusPacketDecoder {
    /// Build a decoder for a stream whose header has already been parsed.
    pub fn new(params: OpusStreamParams) -> Result<Self, OpusDecodeError> {
        let channels = usize::from(params.channels);
        let rate = DECODE_RATE_HZ as i32;
        let inner = match params.mapping {
            ChannelMapping::Rtp => Inner::Single(Box::new(
                OpusDecoder::new(rate, channels).map_err(|error| map_opus_error(&error))?,
            )),
            ChannelMapping::Vorbis {
                streams,
                coupled,
                table,
            } => {
                // The decoder takes a mapping *family* and derives the standard
                // RFC 7845 §5.1.1 layout from it. A file is free to carry a
                // different table; rather than decode it against the wrong
                // layout, the two are compared and a non-standard one is
                // refused by name.
                let standard =
                    ChannelLayout::surround(channels, 1).map_err(|error| map_opus_error(&error))?;
                if standard.nb_streams != usize::from(streams)
                    || standard.nb_coupled_streams != usize::from(coupled)
                    || standard.mapping != table[..channels]
                {
                    return Err(OpusDecodeError::UnsupportedStream(
                        "channel mapping table is not the standard family 1 layout",
                    ));
                }
                Inner::Multi(Box::new(
                    OpusMSDecoder::new(rate, channels, 1)
                        .map_err(|error| map_opus_error(&error))?,
                ))
            }
        };
        Ok(Self {
            inner,
            params,
            scratch: vec![0.0; MAX_FRAME_SAMPLES_PER_CHANNEL * channels],
            produced: 0,
        })
    }

    /// Build a decoder straight from an `OpusHead` header.
    pub fn from_opus_head(header: &[u8]) -> Result<Self, OpusDecodeError> {
        Self::new(parse_opus_head(header)?)
    }

    /// The stream this decoder was built for.
    #[must_use]
    pub const fn params(&self) -> &OpusStreamParams {
        &self.params
    }

    /// Output channel count.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.params.channels as usize
    }

    /// Output sample rate. Always [`DECODE_RATE_HZ`].
    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        DECODE_RATE_HZ
    }

    /// Discard decoder state after a seek or a discontinuity.
    ///
    /// `reset_state` is the dependency's `OPUS_RESET_STATE`: it clears the
    /// overlap-add, pitch and resampler history that would otherwise bleed
    /// pre-seek audio into the first packet after the jump.
    pub fn reset(&mut self) {
        let reset = match &mut self.inner {
            Inner::Single(decoder) => decoder.reset_state(),
            Inner::Multi(decoder) => decoder.reset_state(),
        };
        // The call is infallible in practice; a failure would mean the
        // decoder kept state, so the honest response is to rebuild it.
        if reset.is_err() {
            if let Ok(fresh) = Self::new(self.params) {
                *self = fresh;
            }
        }
        self.produced = 0;
    }

    /// Decode one packet, returning interleaved samples.
    ///
    /// The returned slice is `samples_per_channel * channels` long and borrows
    /// the decoder's single buffer, so it is valid until the next call. Values
    /// are nominally in `-1.0..=1.0`; like libopus, the codec's ringing can
    /// carry a sample mastered near full scale slightly past it, and the
    /// canonical PCM conversion clamps rather than wrapping.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], OpusDecodeError> {
        self.produced = 0;
        if packet.is_empty() {
            return Err(OpusDecodeError::EmptyPacket);
        }
        if packet.len() > MAX_PACKET_BYTES {
            return Err(OpusDecodeError::PacketTooLarge {
                length: packet.len(),
            });
        }

        let channels = self.channels();
        let decoded = match &mut self.inner {
            Inner::Single(decoder) => {
                decoder.decode(packet, MAX_FRAME_SAMPLES_PER_CHANNEL, &mut self.scratch)
            }
            Inner::Multi(decoder) => {
                decoder.decode(packet, MAX_FRAME_SAMPLES_PER_CHANNEL, &mut self.scratch)
            }
        }
        .map_err(|error| map_opus_error(&error))?;

        // The decoder reports what it wrote. Treat that as a claim: a count
        // past the buffer would mean reading scratch that was never written,
        // so it is refused rather than clamped silently.
        if decoded > MAX_FRAME_SAMPLES_PER_CHANNEL {
            return Err(OpusDecodeError::FrameTooLong { samples: decoded });
        }
        if decoded == 0 {
            return Err(OpusDecodeError::MalformedPacket);
        }
        self.produced = decoded;
        Ok(&self.scratch[..decoded * channels])
    }

    /// Samples per channel produced by the last successful [`Self::decode`].
    #[must_use]
    pub const fn last_frame_samples(&self) -> usize {
        self.produced
    }
}

/// Map the dependency's error onto ours. `BufferTooSmall` cannot happen for a
/// well-formed packet — the scratch is sized to the RFC maximum — so it means
/// the packet asked for a longer frame than Opus defines.
fn map_opus_error(error: &OpusCrateError) -> OpusDecodeError {
    match error {
        OpusCrateError::InvalidPacket(_) => OpusDecodeError::MalformedPacket,
        OpusCrateError::BufferTooSmall { needed, .. } => {
            OpusDecodeError::FrameTooLong { samples: *needed }
        }
        OpusCrateError::InvalidArgument(what) => OpusDecodeError::UnsupportedStream(what),
        OpusCrateError::InvalidStream(what) => OpusDecodeError::MalformedHeader(what),
        // `opus_pure::Error` is `#[non_exhaustive]`: a variant added upstream
        // must fail closed as an internal error, never be decoded past.
        OpusCrateError::Io(_) | OpusCrateError::Internal(_) => OpusDecodeError::Internal,
        _ => OpusDecodeError::Internal,
    }
}

/// The channel layout Symphonia uses for an Opus stream of `count` channels,
/// in the order RFC 7845 §5.1.1 defines (which is Vorbis order).
fn layout_for(count: u8) -> Option<Channels> {
    Some(match count {
        1 => Channels::FRONT_LEFT,
        2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
        3 => Channels::FRONT_LEFT | Channels::FRONT_CENTRE | Channels::FRONT_RIGHT,
        4 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        }
        5 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_CENTRE
                | Channels::FRONT_RIGHT
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        }
        6 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_CENTRE
                | Channels::FRONT_RIGHT
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
                | Channels::LFE1
        }
        7 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_CENTRE
                | Channels::FRONT_RIGHT
                | Channels::SIDE_LEFT
                | Channels::SIDE_RIGHT
                | Channels::REAR_CENTRE
                | Channels::LFE1
        }
        8 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_CENTRE
                | Channels::FRONT_RIGHT
                | Channels::SIDE_LEFT
                | Channels::SIDE_RIGHT
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
                | Channels::LFE1
        }
        _ => return None,
    })
}

/// Symphonia's `Decoder` for `CODEC_TYPE_OPUS`.
///
/// Registered by [`crate::codecs::registry`], which is the registry every
/// decode path in this workspace uses, so Opus support arrives everywhere at
/// once instead of being bolted onto one caller.
pub struct OpusAudioDecoder {
    params: CodecParameters,
    inner: OpusPacketDecoder,
    buf: AudioBuffer<f32>,
}

impl fmt::Debug for OpusAudioDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `buf` is one packet of PCM; printing it would be noise, so it is
        // summarised by its length rather than omitted silently.
        f.debug_struct("OpusAudioDecoder")
            .field("params", &self.params)
            .field("inner", &self.inner)
            .field("buffered_frames", &self.buf.frames())
            .finish()
    }
}

impl OpusAudioDecoder {
    fn decode_into_buffer(&mut self, packet: &Packet) -> SymphoniaResult<()> {
        let channels = self.inner.channels();
        let pcm = self
            .inner
            .decode(packet.buf())
            .map_err(|error| symphonia::core::errors::Error::DecodeError(error.detail()))?;
        let frames = pcm.len() / channels.max(1);

        self.buf.clear();
        self.buf.render_reserved(Some(frames));
        for channel in 0..channels {
            let plane = self.buf.chan_mut(channel);
            for (frame, slot) in plane.iter_mut().enumerate().take(frames) {
                *slot = pcm[frame * channels + channel];
            }
        }

        // Symphonia's Ogg reader derives its own start/end delays from the
        // granule positions and marks them on the packet; for Ogg Opus it
        // finds the *end* trim (the final granule states the real sample
        // count) but not the start one, because RFC 7845 §4.2 states the
        // pre-skip in `OpusHead` rather than in the granule arithmetic the
        // reader does. Both trims are applied here, and the start trim is
        // computed from the packet's own timestamp so that it is applied
        // exactly once: if the container already trimmed this packet's head,
        // `trim_start` accounts for it, and after a seek past the pre-skip
        // the subtraction is zero.
        self.buf
            .trim(packet.trim_start() as usize, packet.trim_end() as usize);
        let consumed = packet.ts().saturating_add(u64::from(packet.trim_start()));
        let pre_skip = u64::from(self.inner.params().pre_skip());
        let head = pre_skip.saturating_sub(consumed) as usize;
        if head > 0 {
            self.buf.trim(head, 0);
        }
        Ok(())
    }
}

impl Decoder for OpusAudioDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> SymphoniaResult<Self> {
        if params.codec != CODEC_TYPE_OPUS {
            return unsupported_error("opus: not an Opus track");
        }
        // Ogg, Matroska and MP4 all carry `OpusHead` verbatim as the codec's
        // extra data; it is the only source of the channel mapping, so a
        // stream without it is decoded as plain mono/stereo only when the
        // container states the channel count itself.
        let stream = match params.extra_data.as_deref() {
            Some(extra) => parse_opus_head(extra),
            None => match params.channels.map(Channels::count) {
                Some(count) => u8::try_from(count)
                    .map_err(|_| OpusDecodeError::UnsupportedStream("channel count out of range"))
                    .and_then(OpusStreamParams::rtp),
                None => Err(OpusDecodeError::MalformedHeader(
                    "no OpusHead and no declared channel count",
                )),
            },
        };
        let stream = stream.map_err(|error| match error {
            OpusDecodeError::UnsupportedStream(_) => {
                symphonia::core::errors::Error::Unsupported(error.detail())
            }
            other => symphonia::core::errors::Error::DecodeError(other.detail()),
        })?;

        let Some(layout) = layout_for(stream.channels()) else {
            return unsupported_error("opus: unsupported channel count");
        };
        // A container that disagrees with `OpusHead` about the channel count
        // is not reconciled in the file's favour: the two must agree, or the
        // PCM would be interleaved against the wrong layout.
        if let Some(declared) = params.channels {
            if declared.count() != usize::from(stream.channels()) {
                return decode_error("opus: container and OpusHead disagree on channel count");
            }
        }

        let inner = OpusPacketDecoder::new(stream)
            .map_err(|error| symphonia::core::errors::Error::DecodeError(error.detail()))?;
        let spec = SignalSpec::new(DECODE_RATE_HZ, layout);
        let buf = AudioBuffer::new(MAX_FRAME_SAMPLES_PER_CHANNEL as u64, spec);

        let mut params = params.clone();
        params
            .for_codec(CODEC_TYPE_OPUS)
            .with_sample_rate(DECODE_RATE_HZ)
            .with_channels(layout);

        Ok(Self { params, inner, buf })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_OPUS, "opus", "Opus")]
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> SymphoniaResult<AudioBufferRef<'_>> {
        match self.decode_into_buffer(packet) {
            Ok(()) => Ok(self.buf.as_audio_buffer_ref()),
            Err(error) => {
                // The trait requires an empty buffer after a failure, so a
                // caller that ignores the error cannot read stale audio.
                self.buf.clear();
                Err(error)
            }
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}

/// Fixtures shared between this module's tests and `crate::codecs`'.
#[cfg(test)]
pub(crate) mod tests_support {
    /// A minimal family-0 mono `OpusHead`: version 1, 1 channel, 312-sample
    /// pre-skip, 48 kHz input rate, no gain, RTP mapping.
    pub(crate) const OPUS_HEAD_MONO: &[u8] = &[
        b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 1, 0x38, 0x01, 0x80, 0xBB, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal family-0 `OpusHead`, as every file we have carries it.
    fn opus_head(channels: u8, pre_skip: u16) -> Vec<u8> {
        let mut head = Vec::from(*OPUS_HEAD_MAGIC);
        head.push(1); // version
        head.push(channels);
        head.extend_from_slice(&pre_skip.to_le_bytes());
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes()); // output gain
        head.push(0); // mapping family
        head
    }

    #[test]
    fn a_family_zero_header_parses_into_a_stereo_stream() {
        let params = parse_opus_head(&opus_head(2, 312)).expect("parse");
        assert_eq!(params.channels(), 2);
        assert_eq!(params.pre_skip(), 312);
        assert_eq!(params.input_rate_hz(), 48_000);
        assert_eq!(params.mapping(), ChannelMapping::Rtp);
    }

    #[test]
    fn a_family_one_header_parses_its_mapping_table() {
        // The standard RFC 7845 §5.1.1 quadraphonic layout: two coupled
        // streams, channels in Vorbis order.
        let mut head = opus_head(4, 312);
        head[18] = 1; // mapping family 1
        head.push(2); // streams
        head.push(2); // coupled
        head.extend_from_slice(&[0, 1, 2, 3]);
        let params = parse_opus_head(&head).expect("parse");
        assert_eq!(params.channels(), 4);
        match params.mapping() {
            ChannelMapping::Vorbis {
                streams,
                coupled,
                table,
            } => {
                assert_eq!((streams, coupled), (2, 2));
                assert_eq!(&table[..4], &[0, 1, 2, 3]);
            }
            other @ ChannelMapping::Rtp => panic!("expected a family-1 mapping, got {other:?}"),
        }
        let decoder = OpusPacketDecoder::new(params).expect("multistream decoder");
        assert_eq!(decoder.channels(), 4);
    }

    #[test]
    fn a_non_standard_family_one_layout_is_refused_rather_than_guessed() {
        // Same four channels, but three streams with one coupled pair. The
        // decoder derives the layout from the family, so decoding this
        // against the standard table would silently reorder the audio.
        let mut head = opus_head(4, 312);
        head[18] = 1;
        head.push(3); // streams
        head.push(1); // coupled
        head.extend_from_slice(&[0, 1, 2, 3]);
        let params = parse_opus_head(&head).expect("the header itself is well formed");
        let error =
            OpusPacketDecoder::new(params).expect_err("a non-standard layout must be refused");
        assert_eq!(
            error,
            OpusDecodeError::UnsupportedStream(
                "channel mapping table is not the standard family 1 layout"
            )
        );
    }

    #[test]
    fn hostile_headers_are_refused_with_a_typed_error() {
        let cases: Vec<(Vec<u8>, OpusDecodeError)> = vec![
            (
                Vec::new(),
                OpusDecodeError::MalformedHeader("shorter than the 19-byte minimum"),
            ),
            (
                b"NotOpus!............".to_vec(),
                OpusDecodeError::MalformedHeader("missing OpusHead magic"),
            ),
            (
                {
                    let mut head = opus_head(1, 0);
                    head[8] = 0x20; // major version 2
                    head
                },
                OpusDecodeError::UnsupportedStream("encapsulation version above 0"),
            ),
            (
                opus_head(0, 0),
                OpusDecodeError::MalformedHeader("zero channels"),
            ),
            (
                opus_head(255, 0),
                OpusDecodeError::UnsupportedStream(
                    "more channels than RFC 7845 defines a mapping for",
                ),
            ),
            (
                opus_head(3, 0),
                OpusDecodeError::MalformedHeader(
                    "channel mapping family 0 defines 1 or 2 channels",
                ),
            ),
            (
                {
                    let mut head = opus_head(2, 0);
                    head[18] = 255; // reserved family
                    head
                },
                OpusDecodeError::UnsupportedStream("reserved channel mapping family"),
            ),
            (
                {
                    let mut head = opus_head(2, 0);
                    head[18] = 1; // family 1 with no mapping table behind it
                    head
                },
                OpusDecodeError::MalformedHeader("channel mapping table is truncated"),
            ),
            (
                {
                    let mut head = opus_head(2, 0);
                    head[18] = 1;
                    head.extend_from_slice(&[0, 0, 0, 1]); // zero streams
                    head
                },
                OpusDecodeError::MalformedHeader("zero coded streams"),
            ),
            (
                {
                    let mut head = opus_head(2, 0);
                    head[18] = 1;
                    head.extend_from_slice(&[1, 2, 0, 1]); // coupled > streams
                    head
                },
                OpusDecodeError::MalformedHeader("more coupled streams than streams"),
            ),
            (
                {
                    let mut head = opus_head(2, 0);
                    head[18] = 1;
                    head.extend_from_slice(&[1, 0, 0, 9]); // mapping past the decoded channels
                    head
                },
                OpusDecodeError::MalformedHeader(
                    "channel mapping points past the decoded channels",
                ),
            ),
        ];
        for (bytes, expected) in cases {
            assert_eq!(parse_opus_head(&bytes), Err(expected), "{bytes:02x?}");
        }
    }

    #[test]
    fn an_empty_or_oversized_packet_is_refused_without_decoding() {
        let mut decoder =
            OpusPacketDecoder::from_opus_head(&opus_head(1, 0)).expect("build decoder");
        assert_eq!(decoder.decode(&[]), Err(OpusDecodeError::EmptyPacket));
        let huge = vec![0u8; MAX_PACKET_BYTES + 1];
        assert_eq!(
            decoder.decode(&huge),
            Err(OpusDecodeError::PacketTooLarge {
                length: MAX_PACKET_BYTES + 1
            })
        );
        assert_eq!(decoder.last_frame_samples(), 0);
    }

    #[test]
    fn a_decoder_reports_its_shape() {
        let decoder = OpusPacketDecoder::from_opus_head(&opus_head(2, 312)).expect("build");
        assert_eq!(decoder.channels(), 2);
        assert_eq!(decoder.sample_rate_hz(), 48_000);
        assert_eq!(decoder.params().pre_skip(), 312);
    }

    #[test]
    fn every_defined_channel_count_has_a_layout() {
        for count in 1..=8u8 {
            let layout = layout_for(count).unwrap_or_else(|| panic!("no layout for {count}"));
            assert_eq!(layout.count(), usize::from(count), "layout for {count}");
        }
        assert!(layout_for(9).is_none());
    }
}
