//! whisper.cpp model file inspection.
//!
//! Reading the ggml header is how this adapter answers "is this really a
//! whisper model, and what can it do?" without loading gigabytes of weights.
//! That matters for §8.3: `supports_language_detection` must be *observed*,
//! and the only cheap observation available is the vocabulary size — the same
//! test whisper.cpp itself uses to decide whether a model is multilingual.

use std::io::Read;
use std::path::Path;

/// ggml file magic, read as a little-endian `u32`.
const GGML_MAGIC: u32 = 0x6767_6D6C;

/// Vocabulary size at or above which a whisper model is multilingual.
///
/// English-only models ship 51 864 tokens; multilingual models add the
/// language tokens. whisper.cpp applies exactly this rule.
const MULTILINGUAL_VOCAB_THRESHOLD: u32 = 51_865;

/// Header fields of a whisper ggml model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WhisperModelHeader {
    /// Token vocabulary size.
    pub n_vocab: u32,
    /// Audio encoder context length.
    pub n_audio_ctx: u32,
    /// Audio encoder width.
    pub n_audio_state: u32,
    /// Audio encoder depth.
    pub n_audio_layer: u32,
    /// Text decoder context length.
    pub n_text_ctx: u32,
    /// Text decoder width.
    pub n_text_state: u32,
    /// Text decoder depth.
    pub n_text_layer: u32,
    /// Mel filterbank count.
    pub n_mels: u32,
    /// Quantisation type tag.
    pub ftype: i32,
    /// Size of the weight file.
    pub byte_length: u64,
}

impl WhisperModelHeader {
    /// Whether the model can transcribe languages other than English, and
    /// therefore whether language detection is meaningful.
    #[must_use]
    pub const fn is_multilingual(&self) -> bool {
        self.n_vocab >= MULTILINGUAL_VOCAB_THRESHOLD
    }

    /// Coarse size label used in benchmark documents and the model manager.
    #[must_use]
    pub const fn size_label(&self) -> &'static str {
        match self.n_audio_state {
            0..=384 => "tiny",
            385..=512 => "base",
            513..=768 => "small",
            769..=1024 => "medium",
            _ => "large",
        }
    }
}

/// Why a model file could not be read as a whisper model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelProbeError {
    /// The file does not exist or cannot be opened.
    #[error("model file is unreadable at {path}: {detail}")]
    Unreadable {
        /// Path that was tried.
        path: String,
        /// Non-secret description.
        detail: String,
    },
    /// The file is too short to contain a header.
    #[error(
        "model file is truncated: {observed} bytes is shorter than the {required} byte header"
    )]
    Truncated {
        /// Bytes available.
        observed: u64,
        /// Bytes needed.
        required: u64,
    },
    /// The magic number is wrong, so this is not a ggml model at all.
    #[error(
        "file is not a whisper ggml model (magic {observed:#010x}, expected {expected:#010x})"
    )]
    NotAWhisperModel {
        /// Magic that was read.
        observed: u32,
        /// Magic that was expected.
        expected: u32,
    },
    /// A header field is implausible.
    #[error("model header is implausible: {0}")]
    ImplausibleHeader(String),
}

/// Bytes of header we read: magic plus eleven `i32` hyper-parameters.
const HEADER_BYTES: usize = 4 + 11 * 4;

/// Read and validate a whisper model header.
pub fn read_model_header(path: &Path) -> Result<WhisperModelHeader, ModelProbeError> {
    let metadata = std::fs::metadata(path).map_err(|source| ModelProbeError::Unreadable {
        path: path.display().to_string(),
        detail: source.to_string(),
    })?;
    let byte_length = metadata.len();
    if byte_length < HEADER_BYTES as u64 {
        return Err(ModelProbeError::Truncated {
            observed: byte_length,
            required: HEADER_BYTES as u64,
        });
    }

    let mut file = std::fs::File::open(path).map_err(|source| ModelProbeError::Unreadable {
        path: path.display().to_string(),
        detail: source.to_string(),
    })?;
    let mut header = [0u8; HEADER_BYTES];
    file.read_exact(&mut header)
        .map_err(|source| ModelProbeError::Unreadable {
            path: path.display().to_string(),
            detail: source.to_string(),
        })?;

    let word = |index: usize| -> u32 {
        let offset = index * 4;
        u32::from_le_bytes([
            header[offset],
            header[offset + 1],
            header[offset + 2],
            header[offset + 3],
        ])
    };

    let magic = word(0);
    if magic != GGML_MAGIC {
        return Err(ModelProbeError::NotAWhisperModel {
            observed: magic,
            expected: GGML_MAGIC,
        });
    }

    let parsed = WhisperModelHeader {
        n_vocab: word(1),
        n_audio_ctx: word(2),
        n_audio_state: word(3),
        // word(4) is n_audio_head, which we do not surface.
        n_audio_layer: word(5),
        n_text_ctx: word(6),
        n_text_state: word(7),
        // word(8) is n_text_head.
        n_text_layer: word(9),
        n_mels: word(10),
        #[allow(clippy::cast_possible_wrap)]
        ftype: word(11) as i32,
        byte_length,
    };

    // A forged header could claim absurd dimensions; whisper.cpp would then
    // try to allocate from them. Refuse before the loader sees the file.
    if parsed.n_vocab == 0 || parsed.n_vocab > 1_000_000 {
        return Err(ModelProbeError::ImplausibleHeader(format!(
            "vocabulary size {}",
            parsed.n_vocab
        )));
    }
    if parsed.n_audio_state == 0 || parsed.n_audio_state > 65_536 {
        return Err(ModelProbeError::ImplausibleHeader(format!(
            "audio state width {}",
            parsed.n_audio_state
        )));
    }
    if parsed.n_mels == 0 || parsed.n_mels > 512 {
        return Err(ModelProbeError::ImplausibleHeader(format!(
            "mel band count {}",
            parsed.n_mels
        )));
    }
    if parsed.n_text_layer == 0 || parsed.n_text_layer > 256 {
        return Err(ModelProbeError::ImplausibleHeader(format!(
            "text layer count {}",
            parsed.n_text_layer
        )));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(mutate: impl FnOnce(&mut [u32; 12])) -> Vec<u8> {
        // Field order matches whisper.cpp's loader: magic, n_vocab,
        // n_audio_ctx, n_audio_state, n_audio_head, n_audio_layer,
        // n_text_ctx, n_text_state, n_text_head, n_text_layer, n_mels, ftype.
        let mut words: [u32; 12] = [GGML_MAGIC, 51_864, 1_500, 384, 6, 4, 448, 384, 6, 4, 80, 1];
        mutate(&mut words);
        let mut bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        bytes.extend_from_slice(&[0u8; 256]);
        bytes
    }

    fn write(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model.bin");
        std::fs::write(&path, bytes).expect("write");
        (dir, path)
    }

    #[test]
    fn an_english_only_tiny_model_is_parsed() {
        let (_dir, path) = write(&header_bytes(|_| {}));
        let header = read_model_header(&path).expect("parse");
        assert_eq!(header.n_vocab, 51_864);
        assert!(!header.is_multilingual());
        assert_eq!(header.size_label(), "tiny");
        assert_eq!(header.n_mels, 80);
    }

    #[test]
    fn a_multilingual_model_is_detected_by_vocabulary_size() {
        let (_dir, path) = write(&header_bytes(|words| words[1] = 51_865));
        assert!(read_model_header(&path).expect("parse").is_multilingual());

        // large-v3 widened the vocabulary again; the rule still holds.
        let (_dir, path) = write(&header_bytes(|words| words[1] = 51_866));
        assert!(read_model_header(&path).expect("parse").is_multilingual());
    }

    #[test]
    fn size_labels_track_the_encoder_width() {
        for (width, label) in [
            (384u32, "tiny"),
            (512, "base"),
            (768, "small"),
            (1024, "medium"),
            (1280, "large"),
        ] {
            let (_dir, path) = write(&header_bytes(|words| words[3] = width));
            assert_eq!(read_model_header(&path).expect("parse").size_label(), label);
        }
    }

    #[test]
    fn a_non_model_file_is_refused() {
        let (_dir, path) = write(&vec![0x41u8; 4096]);
        assert!(matches!(
            read_model_header(&path),
            Err(ModelProbeError::NotAWhisperModel { .. })
        ));
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let (_dir, path) = write(&[0u8; 8]);
        assert!(matches!(
            read_model_header(&path),
            Err(ModelProbeError::Truncated { .. })
        ));
    }

    #[test]
    fn a_missing_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            read_model_header(&dir.path().join("absent.bin")),
            Err(ModelProbeError::Unreadable { .. })
        ));
    }

    #[test]
    fn implausible_dimensions_are_refused_before_the_loader_sees_them() {
        for (index, value, what) in [
            (1usize, 0u32, "zero vocabulary"),
            (1, 10_000_000, "absurd vocabulary"),
            (3, 0, "zero encoder width"),
            (3, 1_000_000, "absurd encoder width"),
            (10, 0, "zero mel bands"),
            (10, 10_000, "absurd mel bands"),
            (9, 0, "zero text layers"),
            (9, 100_000, "absurd text layers"),
        ] {
            let (_dir, path) = write(&header_bytes(|words| words[index] = value));
            assert!(
                matches!(
                    read_model_header(&path),
                    Err(ModelProbeError::ImplausibleHeader(_))
                ),
                "{what} was accepted"
            );
        }
    }

    #[test]
    fn parsing_is_deterministic() {
        let (_dir, path) = write(&header_bytes(|_| {}));
        assert_eq!(read_model_header(&path), read_model_header(&path));
    }
}
