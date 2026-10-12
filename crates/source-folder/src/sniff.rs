//! Media container sniffing (§8.1, App. A.4 "media-sniff sanity").
//!
//! The extension is a hint supplied by whoever created the file, so a
//! misleading `.wav` must not decide anything (§18.6A "USB media"). The
//! barrier therefore recognises the container from the leading bytes and uses
//! the *sniffed* extension when the archive path is derived.
//!
//! This is a sanity check, not a decoder: recognising a container says the
//! bytes begin like a media file, not that every frame is valid. Full
//! validation belongs to the audio pipeline.

/// Containers the watched folder accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaContainer {
    /// RIFF/WAVE.
    Wav,
    /// MPEG audio, with or without an ID3 header.
    Mp3,
    /// ISO base media (MP4, M4A).
    Mp4,
    /// Ogg.
    Ogg,
    /// Native FLAC.
    Flac,
    /// AIFF.
    Aiff,
    /// Matroska / WebM.
    Matroska,
}

impl MediaContainer {
    /// Conventional extension for the archive path (§13.4 sanitises these).
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Mp4 => "m4a",
            Self::Ogg => "ogg",
            Self::Flac => "flac",
            Self::Aiff => "aiff",
            Self::Matroska => "mka",
        }
    }

    /// Codec label recorded as the observed codec on the media asset.
    #[must_use]
    pub const fn codec_label(self) -> &'static str {
        match self {
            Self::Wav => "pcm",
            Self::Mp3 => "mp3",
            Self::Mp4 => "aac",
            Self::Ogg => "vorbis",
            Self::Flac => "flac",
            Self::Aiff => "pcm",
            Self::Matroska => "matroska",
        }
    }
}

/// Smallest leading slice that can decide any supported container.
pub const SNIFF_WINDOW_BYTES: usize = 16;

/// Recognise the container from a file's leading bytes.
///
/// Returns `None` for anything unrecognised, which the barrier treats as "not
/// importable yet" rather than "import it anyway".
#[must_use]
pub fn sniff(header: &[u8]) -> Option<MediaContainer> {
    if header.len() < 4 {
        return None;
    }
    if header.starts_with(b"RIFF") && header.len() >= 12 && &header[8..12] == b"WAVE" {
        return Some(MediaContainer::Wav);
    }
    if header.starts_with(b"FORM") && header.len() >= 12 && &header[8..12] == b"AIFF" {
        return Some(MediaContainer::Aiff);
    }
    if header.starts_with(b"OggS") {
        return Some(MediaContainer::Ogg);
    }
    if header.starts_with(b"fLaC") {
        return Some(MediaContainer::Flac);
    }
    if header.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(MediaContainer::Matroska);
    }
    if header.len() >= 8 && &header[4..8] == b"ftyp" {
        return Some(MediaContainer::Mp4);
    }
    if header.starts_with(b"ID3") {
        return Some(MediaContainer::Mp3);
    }
    // A bare MPEG audio frame: sync word plus a layer that is not "reserved".
    if header[0] == 0xFF && (header[1] & 0xE0) == 0xE0 && (header[1] & 0x06) != 0x00 {
        return Some(MediaContainer::Mp3);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_file_is_not_a_container() {
        assert_eq!(sniff(b"hello, this is not audio"), None);
    }

    #[test]
    fn a_wav_header_is_recognised_regardless_of_extension() {
        let mut header = Vec::from(*b"RIFF");
        header.extend_from_slice(&[0x24, 0x00, 0x00, 0x00]);
        header.extend_from_slice(b"WAVEfmt ");
        assert_eq!(sniff(&header), Some(MediaContainer::Wav));
    }

    #[test]
    fn an_isobmff_header_is_recognised_from_the_ftyp_box() {
        let header = b"\0\0\0\x20ftypM4A ";
        assert_eq!(sniff(header), Some(MediaContainer::Mp4));
    }

    #[test]
    fn a_truncated_header_is_not_guessed() {
        assert_eq!(sniff(b"RI"), None);
        assert_eq!(sniff(b"RIFF"), None);
    }
}
