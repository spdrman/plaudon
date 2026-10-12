//! Media validation and content sniffing (spec §8.1, §7.4 — task F1.1).
//!
//! This is the gate every imported file passes before any pipeline work
//! happens. It treats the input as untrusted bytes: the path is hardened, the
//! container is decided by content, the declared size is checked against
//! reality, and a content hash is computed for deduplication. Nothing here
//! guesses — a mismatch is reported, never resolved in the file's favour.

use std::io::Read;
use std::path::Path;

use plaudon_ai_contracts::{Known, MediaFormat};
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use sha2::{Digest, Sha256};

use crate::error::AudioError;
use crate::pathguard::{resolve_media_path, PathPolicy, ResolvedMediaPath};
use crate::probe::{probe_path, MediaProfile};
use crate::sniff::{extension_matches, sniff_container, SniffedContainer, SNIFF_WINDOW_BYTES};

/// Bytes read per hashing iteration. Hashing a 16 GiB file therefore costs
/// 1 MiB of memory, not 16 GiB.
const HASH_CHUNK_BYTES: usize = 1024 * 1024;

/// What the caller claims about the file, if anything.
#[derive(Debug, Clone, Default)]
pub struct ValidationRequest {
    pub policy: PathPolicy,
    /// Size the source claims this file has. A cloud/device manifest provides
    /// one; manual import does not.
    pub declared_byte_length: Option<u64>,
}

impl ValidationRequest {
    #[must_use]
    pub fn manual_import() -> Self {
        Self {
            policy: PathPolicy::user_selected(),
            declared_byte_length: None,
        }
    }
}

/// A file that passed every validation rule.
#[derive(Debug, Clone)]
pub struct ValidatedMedia {
    path: ResolvedMediaPath,
    profile: MediaProfile,
    identity: MediaIdentity,
    display_name: String,
}

impl ValidatedMedia {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.path.path()
    }

    #[must_use]
    pub const fn resolved(&self) -> &ResolvedMediaPath {
        &self.path
    }

    #[must_use]
    pub const fn profile(&self) -> &MediaProfile {
        &self.profile
    }

    /// Content hash plus byte length and observed codec (§6.6
    /// `MediaIdentity`), the key used for deduplication decisions.
    #[must_use]
    pub const fn identity(&self) -> &MediaIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn sha256(&self) -> Sha256Digest {
        self.identity.sha256
    }

    /// Sanitised original file name, metadata only.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub const fn container(&self) -> SniffedContainer {
        self.profile.container
    }

    /// The capability format pair (§8.3 `supported_formats`) for this file.
    ///
    /// Both halves come from the bytes: the container from sniffing, the
    /// codec from the probe. An engine's declared formats are matched against
    /// this pair, never against the file extension.
    #[must_use]
    pub fn media_format(&self) -> Known<MediaFormat> {
        let Some(codec) = self.profile.codec.get() else {
            return Known::Unknown;
        };
        self.profile.container.media_format(codec).into()
    }
}

/// Validate an untrusted media file.
///
/// On success every value in [`MediaProfile`] has been established from the
/// bytes themselves.
pub fn validate_media(
    supplied_path: &Path,
    request: &ValidationRequest,
) -> Result<ValidatedMedia, AudioError> {
    let resolved = resolve_media_path(supplied_path, &request.policy)?;

    if let Some(declared) = request.declared_byte_length {
        if declared != resolved.byte_length() {
            return Err(AudioError::DeclaredSizeMismatch {
                declared,
                actual: resolved.byte_length(),
            });
        }
    }

    let (digest, prefix) = hash_and_prefix(resolved.path())?;

    let container = sniff_container(&prefix);
    if container == SniffedContainer::Unknown {
        return Err(AudioError::UnrecognisedContainer);
    }

    // Extension/content mismatch is fail-closed: the file is not decoded as
    // the extension claims, and the caller is told what it actually is.
    let extension = resolved.supplied_extension();
    if !extension.is_empty() && !extension_matches(container, &extension) {
        return Err(AudioError::mismatch(&extension, container));
    }

    let profile = probe_path(resolved.path())?;

    // The probe re-sniffs from a fresh read; disagreement would mean the file
    // changed under us (TOCTOU) and must not be imported.
    if profile.container != container {
        return Err(AudioError::PathRejected {
            reason: format!(
                "file content changed during validation: sniffed `{}` then `{}`",
                container.as_str(),
                profile.container.as_str()
            ),
        });
    }

    let Some(codec) = profile.codec.get().cloned() else {
        return Err(AudioError::UnsupportedCodec {
            codec: container.as_str().to_string(),
            reason: "no bundled decoder claims this stream; it would need an optional external \
                     runtime, which this build does not assume is installed"
                .to_string(),
        });
    };

    if profile.frames.get().is_none_or(|frames| *frames == 0) {
        return Err(AudioError::Truncated {
            detail: "container declares no decodable audio frames".to_string(),
        });
    }

    let display_name = resolved.sanitized_display_name();
    let identity = MediaIdentity::new(digest, resolved.byte_length(), codec);

    Ok(ValidatedMedia {
        path: resolved,
        profile,
        identity,
        display_name,
    })
}

/// Stream the file once, producing both its SHA-256 and its sniff prefix.
fn hash_and_prefix(path: &Path) -> Result<(Sha256Digest, Vec<u8>), AudioError> {
    let mut file = std::fs::File::open(path).map_err(|source| AudioError::io(path, source))?;
    let mut hasher = Sha256::new();
    let mut prefix = Vec::with_capacity(SNIFF_WINDOW_BYTES);
    let mut buffer = vec![0u8; HASH_CHUNK_BYTES];

    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| AudioError::io(path, source))?;
        if read == 0 {
            break;
        }
        if prefix.len() < SNIFF_WINDOW_BYTES {
            let wanted = (SNIFF_WINDOW_BYTES - prefix.len()).min(read);
            prefix.extend_from_slice(&buffer[..wanted]);
        }
        hasher.update(&buffer[..read]);
    }

    let digest: [u8; 32] = hasher.finalize().into();
    Ok((Sha256Digest::from_bytes(digest), prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use std::path::PathBuf;

    fn fixture_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn wav(dir: &Path, name: &str, rate: u32, channels: u16, ms: u64) -> PathBuf {
        let path = dir.join(name);
        testing::write_sine_wav(&path, rate, channels, ms);
        path
    }

    #[test]
    fn validates_a_wav_with_correct_metadata() {
        let dir = fixture_dir();
        let path = wav(dir.path(), "meeting.wav", 16_000, 1, 1500);
        let media = validate_media(&path, &ValidationRequest::manual_import()).unwrap();

        assert_eq!(media.container(), SniffedContainer::Wav);
        assert_eq!(media.profile().sample_rate_hz, Known::Known(16_000));
        assert_eq!(media.profile().channels, Known::Known(1));
        assert_eq!(media.profile().duration_ms, Known::Known(1500));
        assert_eq!(
            media.media_format(),
            Known::Known(MediaFormat::new("wav", "pcm_s16le"))
        );
        assert_eq!(media.display_name(), "meeting.wav");
        assert_eq!(media.identity().byte_length, media.profile().byte_length);
    }

    #[test]
    fn preserves_stereo_channel_count_without_downmix() {
        let dir = fixture_dir();
        let path = wav(dir.path(), "stereo.wav", 44_100, 2, 500);
        let media = validate_media(&path, &ValidationRequest::manual_import()).unwrap();
        assert_eq!(media.profile().channels, Known::Known(2));
        assert!(media.profile().is_multichannel());
    }

    #[test]
    fn hash_is_deterministic_and_content_addressed() {
        let dir = fixture_dir();
        let first = wav(dir.path(), "a.wav", 16_000, 1, 300);
        let second = dir.path().join("copy-with-different-name.wav");
        std::fs::copy(&first, &second).unwrap();

        let a = validate_media(&first, &ValidationRequest::manual_import()).unwrap();
        let b = validate_media(&second, &ValidationRequest::manual_import()).unwrap();
        assert_eq!(a.sha256(), b.sha256());

        let different = wav(dir.path(), "c.wav", 16_000, 1, 301);
        let c = validate_media(&different, &ValidationRequest::manual_import()).unwrap();
        assert_ne!(a.sha256(), c.sha256());
    }

    #[test]
    fn validation_is_idempotent() {
        let dir = fixture_dir();
        let path = wav(dir.path(), "a.wav", 16_000, 1, 300);
        let first = validate_media(&path, &ValidationRequest::manual_import()).unwrap();
        let second = validate_media(&path, &ValidationRequest::manual_import()).unwrap();
        assert_eq!(first.sha256(), second.sha256());
        assert_eq!(first.profile(), second.profile());
    }

    #[test]
    fn mp3_bytes_named_wav_are_classified_by_content_and_rejected() {
        let dir = fixture_dir();
        // Two valid MPEG Layer III frames: enough for the sniffer, and plainly
        // not a RIFF/WAVE file.
        let header = [0xFFu8, 0xFB, 0x90, 0x00];
        let mut bytes = Vec::new();
        for _ in 0..3 {
            bytes.extend_from_slice(&header);
            bytes.extend(std::iter::repeat_n(0u8, 417 - 4));
        }
        let path = dir.path().join("disguised.wav");
        std::fs::write(&path, &bytes).unwrap();

        let error = validate_media(&path, &ValidationRequest::manual_import()).unwrap_err();
        match error {
            AudioError::ExtensionContentMismatch { declared, sniffed } => {
                assert_eq!(declared, "wav");
                assert_eq!(sniffed, "mp3");
            }
            other => panic!("expected an extension/content mismatch, got {other}"),
        }
    }

    #[test]
    fn non_audio_file_is_rejected() {
        let dir = fixture_dir();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"Dear team, the meeting is cancelled. Regards.").unwrap();
        let error = validate_media(&path, &ValidationRequest::manual_import()).unwrap_err();
        assert_eq!(error.code(), "MEDIA_UNRECOGNISED");
    }

    #[test]
    fn empty_file_is_rejected() {
        let dir = fixture_dir();
        let path = dir.path().join("empty.wav");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            validate_media(&path, &ValidationRequest::manual_import())
                .unwrap_err()
                .code(),
            "MEDIA_EMPTY"
        );
    }

    #[test]
    fn truncated_file_is_rejected_not_partially_imported() {
        let dir = fixture_dir();
        let full = wav(dir.path(), "full.wav", 16_000, 1, 2000);
        let bytes = std::fs::read(&full).unwrap();
        let path = dir.path().join("truncated.wav");
        // Keep the 44-byte header (which declares 64 000 bytes of data) and
        // 100 bytes of samples.
        std::fs::write(&path, &bytes[..144]).unwrap();

        let result = validate_media(&path, &ValidationRequest::manual_import());
        match result {
            Err(error) => assert!(
                matches!(
                    error.code(),
                    "MEDIA_TRUNCATED" | "MEDIA_MALFORMED_HEADER" | "MEDIA_UNSUPPORTED_CODEC"
                ),
                "unexpected error: {error}"
            ),
            Ok(media) => {
                let frames = *media.profile().frames.get().unwrap();
                assert!(
                    frames < 32_000,
                    "truncated file must not report the declared length"
                );
            }
        }
    }

    #[test]
    fn declared_size_mismatch_is_rejected() {
        let dir = fixture_dir();
        let path = wav(dir.path(), "a.wav", 16_000, 1, 300);
        let actual = std::fs::metadata(&path).unwrap().len();
        let request = ValidationRequest {
            declared_byte_length: Some(actual + 1),
            ..ValidationRequest::manual_import()
        };
        let error = validate_media(&path, &request).unwrap_err();
        assert_eq!(error.code(), "MEDIA_DECLARED_SIZE_MISMATCH");
    }

    #[test]
    fn matching_declared_size_is_accepted() {
        let dir = fixture_dir();
        let path = wav(dir.path(), "a.wav", 16_000, 1, 300);
        let actual = std::fs::metadata(&path).unwrap().len();
        let request = ValidationRequest {
            declared_byte_length: Some(actual),
            ..ValidationRequest::manual_import()
        };
        assert!(validate_media(&path, &request).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_input_is_resolved_and_validated() {
        let dir = fixture_dir();
        let target = wav(dir.path(), "real.wav", 16_000, 1, 300);
        let link = dir.path().join("link.wav");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let media = validate_media(&link, &ValidationRequest::manual_import()).unwrap();
        assert!(media.resolved().followed_symlink());
        assert_eq!(media.path().file_name().unwrap(), "real.wav");
    }

    #[cfg(unix)]
    #[test]
    fn unresolvable_symlink_is_rejected() {
        let dir = fixture_dir();
        let link = dir.path().join("broken.wav");
        std::os::unix::fs::symlink(dir.path().join("gone.wav"), &link).unwrap();
        assert_eq!(
            validate_media(&link, &ValidationRequest::manual_import())
                .unwrap_err()
                .code(),
            "MEDIA_PATH_REJECTED"
        );
    }

    #[test]
    fn hostile_file_name_is_sanitized_for_display_only() {
        let dir = fixture_dir();
        let path = dir.path().join("..weird--name...wav");
        testing::write_sine_wav(&path, 16_000, 1, 200);
        let media = validate_media(&path, &ValidationRequest::manual_import()).unwrap();
        assert!(!media.display_name().contains('/'));
        assert!(!media.display_name().starts_with('.'));
        // The real path is untouched.
        assert_eq!(media.path().file_name().unwrap(), "..weird--name...wav");
    }

    #[test]
    fn extensionless_file_is_classified_by_content() {
        let dir = fixture_dir();
        let path = dir.path().join("recording");
        testing::write_sine_wav(&path, 16_000, 1, 200);
        let media = validate_media(&path, &ValidationRequest::manual_import()).unwrap();
        assert_eq!(media.container(), SniffedContainer::Wav);
    }
}
