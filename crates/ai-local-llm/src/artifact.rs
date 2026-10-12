//! The installed model artifact a local runtime executes (spec 12).
//!
//! Installation, progress, licence display and deletion belong to the model
//! manager in `plaudon_app_core::models`. What this module owns is narrower
//! and safety-critical: **verifying the exact bytes before they are fed to an
//! inference process**. A manifest checksum that is only checked at install
//! time proves nothing about the file on the disk today.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bytes read per hashing iteration. Large enough to be fast, small enough
/// that a multi-gigabyte weight file never lands in memory at once.
const HASH_CHUNK_BYTES: usize = 1024 * 1024;

/// A model file the user has installed, with the identity it must still have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelArtifact {
    /// Model identifier, matching the model manager's entry.
    pub model_id: String,
    /// Absolute path of the weight file.
    pub path: PathBuf,
    /// Expected SHA-256, lowercase hex, from the trusted manifest.
    pub expected_sha256: String,
    /// Expected size in bytes.
    pub expected_bytes: u64,
    /// Context length the runtime reports for this file.
    pub context_tokens: u64,
}

/// Why an artifact cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    /// The file is not where the registry says it is.
    #[error("the model file is missing")]
    Missing,
    /// The file could not be read.
    #[error("the model file could not be read")]
    Unreadable,
    /// The file size does not match the manifest.
    #[error("the model file size does not match the manifest")]
    SizeMismatch {
        /// Size the manifest declares.
        expected: u64,
        /// Size on disk.
        observed: u64,
    },
    /// The file contents do not match the manifest.
    #[error("the model file failed its integrity check")]
    ChecksumMismatch,
    /// The manifest entry itself is not usable.
    #[error("the model manifest entry is invalid: {0}")]
    InvalidManifest(&'static str),
}

/// Hash a file without loading it into memory.
///
/// # Errors
///
/// Returns [`ArtifactError::Missing`] or [`ArtifactError::Unreadable`].
pub fn sha256_file(path: &Path) -> Result<String, ArtifactError> {
    if !path.exists() {
        return Err(ArtifactError::Missing);
    }
    let mut file = std::fs::File::open(path).map_err(|_| ArtifactError::Unreadable)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ArtifactError::Unreadable)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

impl LocalModelArtifact {
    /// Verify the file on disk against the manifest.
    ///
    /// Size is checked first because it is cheap and catches a truncated
    /// download without hashing gigabytes.
    ///
    /// # Errors
    ///
    /// Returns the specific mismatch rather than a generic failure, so the
    /// model manager can offer the right remedy.
    pub fn verify(&self) -> Result<(), ArtifactError> {
        if self.expected_sha256.len() != 64
            || !self.expected_sha256.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(ArtifactError::InvalidManifest(
                "expected_sha256 is not a hex SHA-256",
            ));
        }
        if self.model_id.trim().is_empty() {
            return Err(ArtifactError::InvalidManifest("model_id is empty"));
        }
        if self.context_tokens == 0 {
            return Err(ArtifactError::InvalidManifest("context_tokens is zero"));
        }

        let metadata = std::fs::metadata(&self.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ArtifactError::Missing
            } else {
                ArtifactError::Unreadable
            }
        })?;
        if metadata.len() != self.expected_bytes {
            return Err(ArtifactError::SizeMismatch {
                expected: self.expected_bytes,
                observed: metadata.len(),
            });
        }
        let actual = sha256_file(&self.path)?;
        if !actual.eq_ignore_ascii_case(&self.expected_sha256) {
            return Err(ArtifactError::ChecksumMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("model.gguf");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn artifact(path: PathBuf, bytes: &[u8]) -> LocalModelArtifact {
        LocalModelArtifact {
            model_id: "fixture-3b".to_string(),
            path,
            expected_sha256: {
                let mut hasher = Sha256::new();
                hasher.update(bytes);
                hex::encode(hasher.finalize())
            },
            expected_bytes: bytes.len() as u64,
            context_tokens: 8_192,
        }
    }

    #[test]
    fn a_matching_file_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"weights";
        let path = write(dir.path(), bytes);
        assert!(artifact(path, bytes).verify().is_ok());
    }

    #[test]
    fn a_tampered_file_of_the_same_size_fails_the_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"weights";
        let path = write(dir.path(), bytes);
        let entry = artifact(path.clone(), bytes);
        std::fs::write(&path, b"weightS").unwrap();
        assert_eq!(entry.verify(), Err(ArtifactError::ChecksumMismatch));
    }

    #[test]
    fn a_truncated_file_fails_on_size_before_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"weights";
        let path = write(dir.path(), bytes);
        let entry = artifact(path.clone(), bytes);
        std::fs::write(&path, b"we").unwrap();
        assert_eq!(
            entry.verify(),
            Err(ArtifactError::SizeMismatch {
                expected: 7,
                observed: 2
            })
        );
    }

    #[test]
    fn a_missing_file_is_distinguished_from_a_corrupt_one() {
        let dir = tempfile::tempdir().unwrap();
        let entry = artifact(dir.path().join("absent.gguf"), b"weights");
        assert_eq!(entry.verify(), Err(ArtifactError::Missing));
    }

    #[test]
    fn an_invalid_manifest_entry_is_refused_before_any_io() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), b"weights");
        let mut entry = artifact(path, b"weights");
        entry.expected_sha256 = "not-a-hash".to_string();
        assert!(matches!(
            entry.verify(),
            Err(ArtifactError::InvalidManifest(_))
        ));
    }

    #[test]
    fn a_zero_context_entry_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), b"weights");
        let mut entry = artifact(path, b"weights");
        entry.context_tokens = 0;
        assert!(matches!(
            entry.verify(),
            Err(ArtifactError::InvalidManifest(_))
        ));
    }

    #[test]
    fn hashing_handles_a_file_larger_than_one_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![7u8; HASH_CHUNK_BYTES * 2 + 13];
        let path = write(dir.path(), &bytes);
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        assert_eq!(sha256_file(&path).unwrap(), hex::encode(hasher.finalize()));
    }
}
