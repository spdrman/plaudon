//! The `.ASR` Opus sidecar: framing validation, measurement and remux
//! (spec §7.7, §7.9, App. B.3, reference-accuracy notes).
//!
//! # What the device gives us
//!
//! The legacy BLE path transfers a compressed `.ASR` Opus sidecar, "the
//! original **transferred bytes** but not full-fidelity original WAV". Spec
//! §7.7 therefore requires us to "store raw bytes separately from any
//! generated WAV/MP3 […], validate opus framing and decoded sample count and
//! duration where possible […] and cap allocation against forged lengths".
//!
//! # What this module does, and does not, do
//!
//! * [`AsrScanner`] is a push parser over the raw sidecar. It validates every
//!   Opus packet's table-of-contents byte and frame packing, and measures the
//!   encoded duration from the frame descriptors. This is a measurement of the
//!   bitstream, not a decode.
//! * [`remux_to_ogg_opus`] rewraps the same packets in a standard Ogg Opus
//!   container so the bytes are playable by ordinary tools. A remux is a
//!   **derived conversion**: it is bit-exact in the coded domain and is never
//!   described as an original WAV.
//! * There is **no PCM decoder here**. Spike A1.4 recorded that this build
//!   bundles no Opus decoder, so a decoded WAV derivative is not available and
//!   is reported as unavailable rather than fabricated.
//!
//! # Sidecar framing
//!
//! Upstream notes published the GATT framing but not the sidecar layout, so
//! the length-prefixed packet stream below is **this build's** definition for
//! the named profile: `| u16 BE packet length | packet bytes |`, repeated. It
//! is validated against our own fixtures and simulator, and the compatibility
//! cells that would rely on it stay `not_evaluated` until hardware says
//! otherwise.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// Opus operates on a 48 kHz timebase regardless of the encoded bandwidth.
pub const OPUS_TIMEBASE_HZ: u32 = 48_000;

/// Samples the decoder discards at the start of a stream, at 48 kHz.
pub const OPUS_PRE_SKIP: u16 = 3_840;

/// Sample rate the legacy sidecar is documented to decode to (spec §2.5A).
pub const ASR_DECODED_SAMPLE_RATE_HZ: u32 = 16_000;

/// Largest Opus packet this build accepts, in bytes.
///
/// RFC 6716 bounds a packet at 1275 bytes per frame with at most 48 frames;
/// the cap here is generous but finite, so a forged length prefix cannot drive
/// allocation.
pub const MAX_PACKET_BYTES: usize = 8 * 1024;

/// Largest encoded duration one packet may describe, in microseconds.
pub const MAX_PACKET_DURATION_US: u32 = 120_000;

/// Why a sidecar was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AsrError {
    /// A length prefix exceeds the packet cap.
    #[error("a packet declares {declared} bytes, over the {MAX_PACKET_BYTES} byte cap")]
    PacketTooLarge {
        /// Length that was declared.
        declared: usize,
    },
    /// A packet declares zero bytes.
    #[error("a packet declares a length of zero")]
    EmptyPacket,
    /// The stream ended inside a packet.
    #[error("the sidecar ended inside a packet")]
    Truncated,
    /// A packet's table-of-contents byte describes an impossible frame layout.
    #[error("a packet declares an invalid frame layout")]
    InvalidFraming,
    /// A packet describes more than the Opus maximum of 120 ms.
    #[error("a packet declares {declared_us} microseconds, over the Opus maximum")]
    PacketTooLong {
        /// Duration the packet declared.
        declared_us: u32,
    },
    /// The sidecar contains no packets at all.
    #[error("the sidecar contains no Opus packets")]
    NoPackets,
    /// Reading or writing failed.
    #[error("the sidecar could not be read or written")]
    Io,
}

/// What the scanner measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrSummary {
    /// Opus packets observed.
    pub packet_count: u64,
    /// Encoded duration, derived from the frame descriptors.
    pub duration_ms: u64,
    /// Coded sample count on the 48 kHz Opus timebase.
    pub samples_48k: u64,
    /// Channel count declared by the first packet.
    pub channels: u8,
    /// Bytes consumed, excluding length prefixes.
    pub packet_bytes: u64,
}

impl AsrSummary {
    /// Decoded sample count at the sidecar's documented output rate.
    ///
    /// Derived arithmetically from the coded duration. It is a cross-check
    /// against a device-declared duration, not a decoder result.
    #[must_use]
    pub const fn derived_samples_at_output_rate(&self) -> u64 {
        self.samples_48k * ASR_DECODED_SAMPLE_RATE_HZ as u64 / OPUS_TIMEBASE_HZ as u64
    }
}

/// Frame duration, in microseconds, for an Opus configuration number.
const fn frame_duration_us(config: u8) -> u32 {
    // RFC 6716 §3.1.
    const SILK: [u32; 4] = [10_000, 20_000, 40_000, 60_000];
    const HYBRID: [u32; 2] = [10_000, 20_000];
    const CELT: [u32; 4] = [2_500, 5_000, 10_000, 20_000];
    if config < 12 {
        SILK[(config & 0x03) as usize]
    } else if config < 16 {
        HYBRID[(config & 0x01) as usize]
    } else {
        CELT[(config & 0x03) as usize]
    }
}

/// Encoded duration of one packet, in microseconds.
///
/// Validates the frame-count code, including the code-3 frame count, and
/// rejects anything outside the Opus maximum of 120 ms.
pub fn packet_duration_us(packet: &[u8]) -> Result<u32, AsrError> {
    let toc = *packet.first().ok_or(AsrError::EmptyPacket)?;
    let config = toc >> 3;
    let frames = match toc & 0x03 {
        0 => 1u32,
        1 | 2 => 2,
        _ => {
            let count_byte = *packet.get(1).ok_or(AsrError::InvalidFraming)?;
            let frames = u32::from(count_byte & 0x3F);
            if frames == 0 || frames > 48 {
                return Err(AsrError::InvalidFraming);
            }
            frames
        }
    };
    let duration = frames
        .checked_mul(frame_duration_us(config))
        .ok_or(AsrError::InvalidFraming)?;
    if duration > MAX_PACKET_DURATION_US {
        return Err(AsrError::PacketTooLong {
            declared_us: duration,
        });
    }
    Ok(duration)
}

/// Channel count declared by a packet's table-of-contents byte.
#[must_use]
pub fn packet_channels(packet: &[u8]) -> u8 {
    match packet.first() {
        Some(toc) if toc & 0x04 != 0 => 2,
        _ => 1,
    }
}

/// A bounded push parser over a `.ASR` sidecar.
///
/// Feed it the bytes as they arrive; it holds at most one packet plus a
/// length prefix, so validation runs during the transfer without buffering
/// the recording.
#[derive(Debug)]
pub struct AsrScanner {
    buffer: Vec<u8>,
    packet_count: u64,
    packet_bytes: u64,
    duration_us: u64,
    channels: Option<u8>,
    failed: Option<AsrError>,
}

impl Default for AsrScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl AsrScanner {
    /// A scanner that has seen nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            packet_count: 0,
            packet_bytes: 0,
            duration_us: 0,
            channels: None,
            failed: None,
        }
    }

    /// Feed bytes. Returns the first framing error encountered, and stays
    /// failed afterwards.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), AsrError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        self.buffer.extend_from_slice(bytes);
        loop {
            if self.buffer.len() < 2 {
                return Ok(());
            }
            let declared = usize::from(u16::from_be_bytes([self.buffer[0], self.buffer[1]]));
            if declared == 0 {
                return Err(self.fail(AsrError::EmptyPacket));
            }
            if declared > MAX_PACKET_BYTES {
                return Err(self.fail(AsrError::PacketTooLarge { declared }));
            }
            if self.buffer.len() < 2 + declared {
                return Ok(());
            }
            let packet = &self.buffer[2..2 + declared];
            let duration = match packet_duration_us(packet) {
                Ok(duration) => duration,
                Err(error) => return Err(self.fail(error)),
            };
            if self.channels.is_none() {
                self.channels = Some(packet_channels(packet));
            }
            self.packet_count += 1;
            self.packet_bytes += declared as u64;
            self.duration_us += u64::from(duration);
            self.buffer.drain(..2 + declared);
        }
    }

    /// Finish, requiring that the stream ended on a packet boundary.
    pub fn finish(self) -> Result<AsrSummary, AsrError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.buffer.is_empty() {
            return Err(AsrError::Truncated);
        }
        if self.packet_count == 0 {
            return Err(AsrError::NoPackets);
        }
        Ok(AsrSummary {
            packet_count: self.packet_count,
            duration_ms: self.duration_us / 1_000,
            samples_48k: self.duration_us * u64::from(OPUS_TIMEBASE_HZ) / 1_000_000,
            channels: self.channels.unwrap_or(1),
            packet_bytes: self.packet_bytes,
        })
    }

    /// Bytes currently held while waiting for the rest of a packet.
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    fn fail(&mut self, error: AsrError) -> AsrError {
        self.failed = Some(error.clone());
        error
    }
}

/// Validate a complete sidecar held in memory.
pub fn scan(bytes: &[u8]) -> Result<AsrSummary, AsrError> {
    let mut scanner = AsrScanner::new();
    scanner.push(bytes)?;
    scanner.finish()
}

// --------------------------------------------------------------- Ogg remux

/// CRC-32 used by the Ogg container: polynomial `0x04C11DB7`, not reflected,
/// zero initial value, no final inversion.
fn ogg_crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Writes an Ogg Opus stream one packet at a time.
#[derive(Debug)]
struct OggOpusWriter<W: Write> {
    sink: W,
    serial: u32,
    sequence: u32,
    segments: Vec<u8>,
    body: Vec<u8>,
    granule: u64,
    page_granule: u64,
    first_page: bool,
}

impl<W: Write> OggOpusWriter<W> {
    /// Ogg allows at most 255 lacing values per page.
    const MAX_SEGMENTS: usize = 255;

    fn new(sink: W, serial: u32) -> Self {
        Self {
            sink,
            serial,
            sequence: 0,
            segments: Vec::new(),
            body: Vec::new(),
            granule: 0,
            page_granule: 0,
            first_page: true,
        }
    }

    fn write_page(&mut self, header_type: u8, granule: u64, last: bool) -> Result<(), AsrError> {
        let mut page = Vec::with_capacity(27 + self.segments.len() + self.body.len());
        page.extend_from_slice(b"OggS");
        page.push(0); // stream structure version
        page.push(header_type | u8::from(last) << 2);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&self.serial.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes()); // checksum placeholder
        page.push(u8::try_from(self.segments.len()).map_err(|_| AsrError::InvalidFraming)?);
        page.extend_from_slice(&self.segments);
        page.extend_from_slice(&self.body);

        let checksum = ogg_crc32(&page);
        page[22..26].copy_from_slice(&checksum.to_le_bytes());
        self.sink.write_all(&page).map_err(|_| AsrError::Io)?;

        self.sequence += 1;
        self.segments.clear();
        self.body.clear();
        Ok(())
    }

    /// Append one packet's lacing values and bytes to the current page.
    fn stage_packet(&mut self, packet: &[u8]) -> Result<(), AsrError> {
        let mut remaining = packet.len();
        let mut written = 0usize;
        loop {
            let lace = remaining.min(255);
            self.segments.push(u8::try_from(lace).unwrap_or(255));
            self.body
                .extend_from_slice(&packet[written..written + lace]);
            written += lace;
            if lace < 255 {
                break;
            }
            remaining -= lace;
            if remaining == 0 {
                // A packet whose length is a multiple of 255 needs a final
                // zero lacing value to mark its end.
                self.segments.push(0);
                break;
            }
        }
        Ok(())
    }

    fn write_header_pages(&mut self, channels: u8, input_rate: u32) -> Result<(), AsrError> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(channels);
        head.extend_from_slice(&OPUS_PRE_SKIP.to_le_bytes());
        head.extend_from_slice(&input_rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family 0
        self.stage_packet(&head)?;
        self.write_page(0x02, 0, false)?;

        let vendor = b"plaudon";
        let mut tags = Vec::new();
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&u32::try_from(vendor.len()).unwrap_or(0).to_le_bytes());
        tags.extend_from_slice(vendor);
        // One comment recording that this is a derived conversion.
        let comment = b"ENCODER=plaudon remux of a device .ASR sidecar (derived conversion)";
        tags.extend_from_slice(&1u32.to_le_bytes());
        tags.extend_from_slice(&u32::try_from(comment.len()).unwrap_or(0).to_le_bytes());
        tags.extend_from_slice(comment);
        self.stage_packet(&tags)?;
        self.write_page(0x00, 0, false)?;
        self.first_page = false;
        Ok(())
    }

    fn write_audio_packet(&mut self, packet: &[u8], duration_us: u32) -> Result<(), AsrError> {
        // A packet may need up to ceil(len/255)+1 lacing values; flush first
        // when the page cannot hold them.
        let needed = packet.len() / 255 + 2;
        if self.segments.len() + needed > Self::MAX_SEGMENTS {
            let granule = self.page_granule;
            self.write_page(0x00, granule, false)?;
        }
        self.stage_packet(packet)?;
        self.granule += u64::from(duration_us) * u64::from(OPUS_TIMEBASE_HZ) / 1_000_000;
        self.page_granule = self.granule + u64::from(OPUS_PRE_SKIP);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AsrError> {
        let granule = self.page_granule;
        self.write_page(0x00, granule, true)
    }
}

/// Remux a `.ASR` sidecar into a standard Ogg Opus stream.
///
/// The coded packets are copied unchanged, so the result is bit-exact in the
/// coded domain. It is a derived conversion, never an original recording:
/// callers label it `QualityClass::DerivedConversion`.
pub fn remux_to_ogg_opus<R: Read, W: Write>(
    source: &mut R,
    sink: &mut W,
    serial: u32,
) -> Result<AsrSummary, AsrError> {
    let mut writer = OggOpusWriter::new(sink, serial);
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut scanner = AsrScanner::new();
    let mut started = false;
    let mut eof = false;

    loop {
        if !eof {
            let read = source.read(&mut chunk).map_err(|_| AsrError::Io)?;
            if read == 0 {
                eof = true;
            } else {
                buffer.extend_from_slice(&chunk[..read]);
            }
        }

        while buffer.len() >= 2 {
            let declared = usize::from(u16::from_be_bytes([buffer[0], buffer[1]]));
            if declared == 0 {
                return Err(AsrError::EmptyPacket);
            }
            if declared > MAX_PACKET_BYTES {
                return Err(AsrError::PacketTooLarge { declared });
            }
            if buffer.len() < 2 + declared {
                break;
            }
            let packet: Vec<u8> = buffer[2..2 + declared].to_vec();
            scanner.push(&buffer[..2 + declared])?;
            buffer.drain(..2 + declared);

            let duration = packet_duration_us(&packet)?;
            if !started {
                writer.write_header_pages(packet_channels(&packet), ASR_DECODED_SAMPLE_RATE_HZ)?;
                started = true;
            }
            writer.write_audio_packet(&packet, duration)?;
        }

        if eof {
            break;
        }
    }

    if !buffer.is_empty() {
        return Err(AsrError::Truncated);
    }
    if !started {
        return Err(AsrError::NoPackets);
    }
    writer.finish()?;
    scanner.finish()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// A 20 ms mono SILK-wideband packet: config 9, code 0.
    ///
    /// RFC 6716 §3.1: configs 8-11 are SILK wideband at 10, 20, 40 and 60 ms.
    fn packet_20ms(payload: usize) -> Vec<u8> {
        let mut packet = vec![9 << 3];
        packet.extend(std::iter::repeat_n(0x55u8, payload));
        packet
    }

    fn sidecar(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for packet in packets {
            out.extend_from_slice(&u16::try_from(packet.len()).unwrap().to_be_bytes());
            out.extend_from_slice(packet);
        }
        out
    }

    #[test]
    fn a_well_formed_sidecar_measures_its_own_duration() {
        let bytes = sidecar(&vec![packet_20ms(40); 50]);
        let summary = scan(&bytes).unwrap();
        assert_eq!(summary.packet_count, 50);
        assert_eq!(summary.duration_ms, 1_000);
        assert_eq!(summary.samples_48k, 48_000);
        assert_eq!(summary.derived_samples_at_output_rate(), 16_000);
        assert_eq!(summary.channels, 1);
        assert_eq!(summary.packet_bytes, 50 * 41);
    }

    #[test]
    fn every_opus_frame_size_is_accounted_for() {
        for (config, expected_us) in [
            (0u8, 10_000u32),
            (3, 60_000),
            (11, 60_000),
            (12, 10_000),
            (15, 20_000),
            (16, 2_500),
            (31, 20_000),
        ] {
            let packet = vec![config << 3, 0x00];
            assert_eq!(packet_duration_us(&packet).unwrap(), expected_us);
        }
    }

    #[test]
    fn multi_frame_packets_are_counted_correctly() {
        // Code 1: two frames of config 9 (20 ms each).
        let two = vec![(9 << 3) | 1, 0x00];
        assert_eq!(packet_duration_us(&two).unwrap(), 40_000);

        // Code 3: explicit frame count of three, config 16 (2.5 ms each).
        let three = vec![(16 << 3) | 3, 3, 0x00];
        assert_eq!(packet_duration_us(&three).unwrap(), 7_500);
    }

    #[test]
    fn a_packet_longer_than_the_opus_maximum_is_refused() {
        // Code 3 with 48 frames of 60 ms would be 2.88 seconds.
        let packet = vec![(3 << 3) | 3, 48, 0x00];
        assert_eq!(
            packet_duration_us(&packet),
            Err(AsrError::PacketTooLong {
                declared_us: 2_880_000
            })
        );
    }

    #[test]
    fn a_zero_frame_count_is_refused() {
        assert_eq!(
            packet_duration_us(&[(10 << 3) | 3, 0, 0]),
            Err(AsrError::InvalidFraming)
        );
        assert_eq!(
            packet_duration_us(&[(10 << 3) | 3]),
            Err(AsrError::InvalidFraming)
        );
    }

    #[test]
    fn a_forged_length_prefix_cannot_drive_allocation() {
        let mut bytes = u16::try_from(MAX_PACKET_BYTES + 1).unwrap().to_be_bytes().to_vec();
        bytes.push(0x50);
        assert_eq!(
            scan(&bytes),
            Err(AsrError::PacketTooLarge {
                declared: MAX_PACKET_BYTES + 1
            })
        );

        let mut scanner = AsrScanner::new();
        // A length that fits the cap but never arrives holds at most the cap.
        scanner.push(&[0x0F, 0xFF]).unwrap();
        scanner.push(&[0x55; 64]).unwrap();
        assert!(scanner.buffered_bytes() <= MAX_PACKET_BYTES + 2);
        assert_eq!(scanner.finish(), Err(AsrError::Truncated));
    }

    #[test]
    fn a_truncated_sidecar_is_never_summarised() {
        let mut bytes = sidecar(&[packet_20ms(40)]);
        bytes.truncate(bytes.len() - 5);
        assert_eq!(scan(&bytes), Err(AsrError::Truncated));
    }

    #[test]
    fn an_empty_sidecar_is_refused() {
        assert_eq!(scan(&[]), Err(AsrError::NoPackets));
        assert_eq!(scan(&[0x00, 0x00]), Err(AsrError::EmptyPacket));
    }

    #[test]
    fn the_scanner_accepts_bytes_split_at_any_boundary() {
        let bytes = sidecar(&vec![packet_20ms(7); 9]);
        for split in 1..bytes.len() {
            let mut scanner = AsrScanner::new();
            scanner.push(&bytes[..split]).unwrap();
            scanner.push(&bytes[split..]).unwrap();
            assert_eq!(scanner.finish().unwrap().packet_count, 9);
        }
    }

    #[test]
    fn a_remux_produces_a_standard_ogg_opus_stream() {
        let bytes = sidecar(&vec![packet_20ms(30); 120]);
        let mut out = Vec::new();
        let summary = remux_to_ogg_opus(&mut bytes.as_slice(), &mut out, 0x1234_5678).unwrap();

        assert_eq!(summary.packet_count, 120);
        assert_eq!(summary.duration_ms, 2_400);
        assert_eq!(&out[..4], b"OggS");
        assert!(
            find(&out, b"OpusHead").is_some(),
            "the identification header is required"
        );
        assert!(find(&out, b"OpusTags").is_some());
        assert!(
            find(&out, b"derived conversion").is_some(),
            "the derivative labels itself"
        );

        // Every page carries a valid checksum, which is how a player decides
        // the stream is intact.
        assert!(pages(&out) > 2);
    }

    #[test]
    fn a_remuxed_stream_never_claims_to_be_wav() {
        let bytes = sidecar(&vec![packet_20ms(30); 4]);
        let mut out = Vec::new();
        remux_to_ogg_opus(&mut bytes.as_slice(), &mut out, 1).unwrap();
        assert!(find(&out, b"RIFF").is_none());
        assert!(find(&out, b"WAVE").is_none());
    }

    #[test]
    fn a_malformed_sidecar_is_not_remuxed() {
        let mut bytes = sidecar(&[packet_20ms(16)]);
        bytes.truncate(bytes.len() - 2);
        let mut out = Vec::new();
        assert_eq!(
            remux_to_ogg_opus(&mut bytes.as_slice(), &mut out, 1),
            Err(AsrError::Truncated)
        );
    }

    /// Walk the pages, verifying each checksum, and return the page count.
    fn pages(stream: &[u8]) -> usize {
        let mut at = 0usize;
        let mut count = 0usize;
        while at + 27 <= stream.len() {
            assert_eq!(&stream[at..at + 4], b"OggS", "page {count} is misaligned");
            let segment_count = usize::from(stream[at + 26]);
            let header_len = 27 + segment_count;
            assert!(at + header_len <= stream.len());
            let body_len: usize = stream[at + 27..at + header_len]
                .iter()
                .map(|lace| usize::from(*lace))
                .sum();
            let page_len = header_len + body_len;
            let mut page = stream[at..at + page_len].to_vec();
            let declared = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
            page[22..26].copy_from_slice(&0u32.to_le_bytes());
            assert_eq!(ogg_crc32(&page), declared, "page {count} checksum");
            at += page_len;
            count += 1;
        }
        assert_eq!(at, stream.len(), "the stream ends on a page boundary");
        count
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// Scanning arbitrary bytes never panics and never buffers without bound.
        #[test]
        fn scanning_arbitrary_bytes_is_total(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let mut scanner = AsrScanner::new();
            let _ = scanner.push(&bytes);
            prop_assert!(scanner.buffered_bytes() <= MAX_PACKET_BYTES + 2 + bytes.len());
            let _ = scanner.finish();
        }

        /// A valid sidecar always remuxes, and the remux always measures the
        /// same duration the scanner measured.
        #[test]
        fn remux_preserves_the_measured_duration(
            packet_count in 1usize..40,
            payload in 1usize..60,
        ) {
            let bytes = sidecar(&vec![packet_20ms(payload); packet_count]);
            let scanned = scan(&bytes).unwrap();
            let mut out = Vec::new();
            let remuxed = remux_to_ogg_opus(&mut bytes.as_slice(), &mut out, 7).unwrap();
            prop_assert_eq!(scanned, remuxed);
            prop_assert!(out.len() > bytes.len() / 2);
        }
    }
}
