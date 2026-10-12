//! Content sniffing by magic bytes (§8.1, F1.1).
//!
//! Classification is by bytes, never by file extension. The scan reads a
//! bounded prefix so a hostile file cannot make us walk gigabytes looking for
//! a signature.

use plaudon_ai_contracts::MediaFormat;

/// Bytes examined when classifying a container. Anything that needs more than
/// this to identify itself is `Unknown`.
pub const SNIFF_WINDOW_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SniffedContainer {
    Wav,
    Mp3,
    Mp4Aac,
    Ogg,
    Opus,
    Flac,
    Caf,
    Aiff,
    Unknown,
}

impl SniffedContainer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SniffedContainer::Wav => "wav",
            SniffedContainer::Mp3 => "mp3",
            SniffedContainer::Mp4Aac => "mp4_aac",
            SniffedContainer::Ogg => "ogg",
            SniffedContainer::Opus => "opus",
            SniffedContainer::Flac => "flac",
            SniffedContainer::Caf => "caf",
            SniffedContainer::Aiff => "aiff",
            SniffedContainer::Unknown => "unknown",
        }
    }

    /// File extensions that legitimately carry this container.
    #[must_use]
    pub const fn canonical_extensions(self) -> &'static [&'static str] {
        match self {
            SniffedContainer::Wav => &["wav", "wave"],
            SniffedContainer::Mp3 => &["mp3"],
            SniffedContainer::Mp4Aac => &["m4a", "mp4", "m4b", "aac", "mov"],
            SniffedContainer::Ogg => &["ogg", "oga"],
            SniffedContainer::Opus => &["opus", "ogg", "oga"],
            SniffedContainer::Flac => &["flac"],
            SniffedContainer::Caf => &["caf"],
            SniffedContainer::Aiff => &["aiff", "aif", "aifc"],
            SniffedContainer::Unknown => &[],
        }
    }

    /// Container name used in a [`MediaFormat`] capability pair.
    ///
    /// `None` for containers we can name but have no capability vocabulary
    /// for yet; such a file is still probed, it simply cannot be matched
    /// against an engine's declared formats.
    #[must_use]
    pub const fn capability_container(self) -> Option<&'static str> {
        match self {
            SniffedContainer::Wav => Some("wav"),
            SniffedContainer::Mp3 => Some("mp3"),
            SniffedContainer::Mp4Aac => Some("mp4"),
            SniffedContainer::Ogg | SniffedContainer::Opus => Some("ogg"),
            SniffedContainer::Flac => Some("flac"),
            SniffedContainer::Caf => Some("caf"),
            SniffedContainer::Aiff => Some("aiff"),
            SniffedContainer::Unknown => None,
        }
    }

    /// The capability format pair for this container and a probed codec.
    #[must_use]
    pub fn media_format(self, codec: &str) -> Option<MediaFormat> {
        Some(MediaFormat::new(self.capability_container()?, codec))
    }

    /// Symphonia's format hint string for this container.
    #[must_use]
    pub const fn hint_extension(self) -> Option<&'static str> {
        match self {
            SniffedContainer::Wav => Some("wav"),
            SniffedContainer::Mp3 => Some("mp3"),
            SniffedContainer::Mp4Aac => Some("m4a"),
            SniffedContainer::Ogg | SniffedContainer::Opus => Some("ogg"),
            SniffedContainer::Flac => Some("flac"),
            SniffedContainer::Caf => Some("caf"),
            SniffedContainer::Aiff => Some("aiff"),
            SniffedContainer::Unknown => None,
        }
    }
}

/// Classify a byte prefix. Only the first [`SNIFF_WINDOW_BYTES`] are examined.
#[must_use]
pub fn sniff_container(bytes: &[u8]) -> SniffedContainer {
    let window = &bytes[..bytes.len().min(SNIFF_WINDOW_BYTES)];

    if window.len() >= 12 && &window[0..4] == b"RIFF" && &window[8..12] == b"WAVE" {
        return SniffedContainer::Wav;
    }
    if window.len() >= 12 && &window[0..4] == b"RIFX" && &window[8..12] == b"WAVE" {
        return SniffedContainer::Wav;
    }
    if window.len() >= 4 && &window[0..4] == b"fLaC" {
        return SniffedContainer::Flac;
    }
    if window.len() >= 4 && &window[0..4] == b"caff" {
        return SniffedContainer::Caf;
    }
    if window.len() >= 12 && &window[0..4] == b"FORM" && matches!(&window[8..12], b"AIFF" | b"AIFC")
    {
        return SniffedContainer::Aiff;
    }
    if window.len() >= 4 && &window[0..4] == b"OggS" {
        // Distinguish Opus from Vorbis by the codec identification header that
        // begins the first logical page payload.
        if find_subsequence(window, b"OpusHead").is_some() {
            return SniffedContainer::Opus;
        }
        return SniffedContainer::Ogg;
    }
    if window.len() >= 12 && &window[4..8] == b"ftyp" {
        return SniffedContainer::Mp4Aac;
    }
    if let Some(container) = sniff_mpeg_audio(window) {
        return container;
    }
    SniffedContainer::Unknown
}

/// MPEG audio needs more care than a 4-byte magic: an ID3v2 tag may precede
/// the first frame, and a bare `0xFF 0xE?` pair occurs constantly in binary
/// data. We require a plausible frame header *and* a second header at the
/// distance the first one predicts.
fn sniff_mpeg_audio(window: &[u8]) -> Option<SniffedContainer> {
    let mut cursor = 0usize;
    if window.len() >= 10 && &window[0..3] == b"ID3" {
        // ID3v2 size is a 28-bit synchsafe integer in bytes 6..10.
        let size = ((u32::from(window[6]) & 0x7f) << 21)
            | ((u32::from(window[7]) & 0x7f) << 14)
            | ((u32::from(window[8]) & 0x7f) << 7)
            | (u32::from(window[9]) & 0x7f);
        cursor = 10usize.saturating_add(size as usize);
        if cursor >= window.len() {
            // Tag claims to extend past our window: it is an ID3-tagged audio
            // file by construction, but we cannot confirm the frame. Treat as
            // MP3 only if the tag is well formed and the file is larger.
            return Some(SniffedContainer::Mp3);
        }
    }

    // Scan a bounded distance for a syncword.
    let scan_end = window.len().min(cursor + 8192);
    let mut index = cursor;
    while index + 4 <= scan_end {
        if let Some(frame_len) = mpeg_frame_length(&window[index..]) {
            let next = index + frame_len;
            if next + 4 <= window.len() {
                if mpeg_frame_length(&window[next..]).is_some() {
                    return Some(SniffedContainer::Mp3);
                }
            } else if index > 0 || cursor > 0 {
                // Single frame at/near EOF of our window after a valid tag.
                return Some(SniffedContainer::Mp3);
            }
        }
        index += 1;
    }
    None
}

const MPEG1_LAYER3_BITRATES: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const MPEG2_LAYER3_BITRATES: [u32; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];

/// Length in bytes of the MPEG audio frame starting at `bytes`, or `None` when
/// `bytes` does not begin a plausible Layer III frame.
fn mpeg_frame_length(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 4 {
        return None;
    }
    if bytes[0] != 0xFF || (bytes[1] & 0xE0) != 0xE0 {
        return None;
    }
    let version_bits = (bytes[1] >> 3) & 0x03;
    let layer_bits = (bytes[1] >> 1) & 0x03;
    // 0b01 is a reserved MPEG version; layer bits 0b01 == Layer III.
    if version_bits == 0b01 || layer_bits != 0b01 {
        return None;
    }
    let bitrate_index = ((bytes[2] >> 4) & 0x0F) as usize;
    let sample_rate_index = ((bytes[2] >> 2) & 0x03) as usize;
    if bitrate_index == 0 || bitrate_index == 15 || sample_rate_index == 3 {
        return None;
    }
    let padding = usize::from((bytes[2] >> 1) & 0x01);

    let (bitrate_kbps, sample_rate, samples_per_frame) = match version_bits {
        // MPEG Version 1
        0b11 => (
            MPEG1_LAYER3_BITRATES[bitrate_index],
            [44_100u32, 48_000, 32_000][sample_rate_index],
            1152usize,
        ),
        // MPEG Version 2
        0b10 => (
            MPEG2_LAYER3_BITRATES[bitrate_index],
            [22_050u32, 24_000, 16_000][sample_rate_index],
            576usize,
        ),
        // MPEG Version 2.5
        0b00 => (
            MPEG2_LAYER3_BITRATES[bitrate_index],
            [11_025u32, 12_000, 8_000][sample_rate_index],
            576usize,
        ),
        _ => return None,
    };
    if bitrate_kbps == 0 {
        return None;
    }
    let length =
        (samples_per_frame / 8) * (bitrate_kbps as usize * 1000) / sample_rate as usize + padding;
    (length >= 4).then_some(length)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Whether `extension` is a legitimate extension for `container`.
///
/// Comparison is ASCII case-insensitive. An unknown container matches nothing.
#[must_use]
pub fn extension_matches(container: SniffedContainer, extension: &str) -> bool {
    let lower = extension.trim_start_matches('.').to_ascii_lowercase();
    container
        .canonical_extensions()
        .iter()
        .any(|candidate| *candidate == lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_header() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&36u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes
    }

    #[test]
    fn empty_and_short_inputs_are_unknown_not_panicking() {
        assert_eq!(sniff_container(&[]), SniffedContainer::Unknown);
        assert_eq!(sniff_container(b"R"), SniffedContainer::Unknown);
        assert_eq!(sniff_container(b"RIFF"), SniffedContainer::Unknown);
        assert_eq!(sniff_container(b"OggS"), SniffedContainer::Ogg);
    }

    #[test]
    fn recognises_each_container() {
        assert_eq!(sniff_container(&wav_header()), SniffedContainer::Wav);
        assert_eq!(sniff_container(b"fLaC\0\0\0\x22"), SniffedContainer::Flac);
        assert_eq!(
            sniff_container(b"\0\0\0\x20ftypM4A \0\0\0\0"),
            SniffedContainer::Mp4Aac
        );
        assert_eq!(sniff_container(b"caffsomething"), SniffedContainer::Caf);
        assert_eq!(
            sniff_container(b"FORM\0\0\0\x12AIFFCOMM"),
            SniffedContainer::Aiff
        );
    }

    #[test]
    fn opus_is_distinguished_from_vorbis_in_ogg() {
        let mut opus = b"OggS\0\x02".to_vec();
        opus.extend_from_slice(&[0u8; 20]);
        opus.extend_from_slice(b"OpusHead");
        assert_eq!(sniff_container(&opus), SniffedContainer::Opus);

        let mut vorbis = b"OggS\0\x02".to_vec();
        vorbis.extend_from_slice(&[0u8; 20]);
        vorbis.extend_from_slice(b"\x01vorbis");
        assert_eq!(sniff_container(&vorbis), SniffedContainer::Ogg);
    }

    #[test]
    fn single_ff_byte_run_is_not_mp3() {
        // A plain run of 0xFF bytes must not be classified as audio.
        let bytes = vec![0xFFu8; 4096];
        assert_eq!(sniff_container(&bytes), SniffedContainer::Unknown);
    }

    #[test]
    fn mp3_requires_two_consecutive_frames() {
        // 128 kbps, 44.1 kHz, MPEG1 Layer III, no padding ⇒ 417-byte frames.
        let header = [0xFFu8, 0xFB, 0x90, 0x00];
        let frame_len = mpeg_frame_length(&header).unwrap();
        assert_eq!(frame_len, 417);

        let mut single = header.to_vec();
        single.extend(std::iter::repeat_n(0u8, frame_len - 4));
        assert_eq!(sniff_container(&single), SniffedContainer::Unknown);

        let mut double = single.clone();
        double.extend_from_slice(&header);
        double.extend(std::iter::repeat_n(0u8, frame_len - 4));
        assert_eq!(sniff_container(&double), SniffedContainer::Mp3);
    }

    #[test]
    fn id3_tagged_mp3_is_recognised() {
        let header = [0xFFu8, 0xFB, 0x90, 0x00];
        let frame_len = mpeg_frame_length(&header).unwrap();
        let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x0a".to_vec();
        bytes.extend(std::iter::repeat_n(0u8, 10)); // tag body of 10 bytes
        for _ in 0..2 {
            bytes.extend_from_slice(&header);
            bytes.extend(std::iter::repeat_n(0u8, frame_len - 4));
        }
        assert_eq!(sniff_container(&bytes), SniffedContainer::Mp3);
    }

    #[test]
    fn reserved_mpeg_version_and_layer_are_rejected() {
        // Version bits 01 are reserved: 0b111_01_01_1 == 0xEB.
        assert_eq!(mpeg_frame_length(&[0xFF, 0xEB, 0x90, 0x00]), None);
        assert_eq!(mpeg_frame_length(&[0xFF, 0xFF, 0x90, 0x00]), None); // layer I
        assert_eq!(mpeg_frame_length(&[0xFF, 0xFB, 0x0C, 0x00]), None); // free bitrate
        assert_eq!(mpeg_frame_length(&[0xFF, 0xFB, 0xF0, 0x00]), None); // bad bitrate
        assert_eq!(mpeg_frame_length(&[0xFF, 0xFB, 0x9C, 0x00]), None); // reserved rate
    }

    #[test]
    fn sniffing_is_bounded_to_the_window() {
        // A signature beyond the window is not found.
        let mut bytes = vec![0u8; SNIFF_WINDOW_BYTES + 16];
        bytes[SNIFF_WINDOW_BYTES..SNIFF_WINDOW_BYTES + 4].copy_from_slice(b"fLaC");
        assert_eq!(sniff_container(&bytes), SniffedContainer::Unknown);
    }

    #[test]
    fn extension_matching_is_case_insensitive_and_closed() {
        assert!(extension_matches(SniffedContainer::Wav, "WAV"));
        assert!(extension_matches(SniffedContainer::Wav, ".wav"));
        assert!(!extension_matches(SniffedContainer::Wav, "mp3"));
        assert!(!extension_matches(SniffedContainer::Unknown, "wav"));
    }
}
