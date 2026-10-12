//! Content hashing for release artifacts (spec 24: every evidence entry
//! carries a SHA-256).

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Lowercase hexadecimal SHA-256 of a byte slice.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Lowercase hexadecimal SHA-256 of a file, streamed so a multi-gigabyte
/// installer never has to be resident in memory.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_hashes_to_the_published_sha256_of_the_empty_string() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn file_and_byte_hashing_agree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("artifact.bin");
        let payload: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &payload).expect("write");
        assert_eq!(sha256_file(&path).expect("hash"), sha256_bytes(&payload));
    }
}
