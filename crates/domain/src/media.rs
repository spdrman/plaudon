//! Media identity and quality classification (spec 6.3B, 7.9, 13.2).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::MediaAssetId;
use crate::time::Timestamp;

/// A SHA-256 digest of media bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest([u8; 32]);

/// Reason a digest string could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("sha256 digest must be 64 lowercase hexadecimal characters")]
pub struct Sha256ParseError;

impl Sha256Digest {
    /// Wrap raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Digest a byte slice.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }

    /// Raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// First two hex characters, used as the archive fan-out directory
    /// (spec 13.4 `media/sha256/<first-two-hex>/...`).
    #[must_use]
    pub fn hex_prefix(self) -> String {
        hex::encode(&self.0[..1])
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for Sha256Digest {
    type Err = Sha256ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(Sha256ParseError);
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).map_err(|_| Sha256ParseError)?;
        Ok(Self(out))
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Immutable identity of a media blob (spec 6.6).
///
/// Equality and hashing use the digest **and** the byte length. The observed
/// codec is provenance metadata: it records what a decoder reported about these
/// bytes and never participates in identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaIdentity {
    /// Digest of the exact acquired bytes.
    pub sha256: Sha256Digest,
    /// Length of the exact acquired bytes.
    pub byte_length: u64,
    /// Codec reported by our decoder for these bytes.
    pub observed_codec: String,
}

impl MediaIdentity {
    /// Construct an identity.
    #[must_use]
    pub fn new(sha256: Sha256Digest, byte_length: u64, observed_codec: impl Into<String>) -> Self {
        Self { sha256, byte_length, observed_codec: observed_codec.into() }
    }

    /// Compute the identity of an in-memory buffer.
    #[must_use]
    pub fn of_bytes(bytes: &[u8], observed_codec: impl Into<String>) -> Self {
        Self::new(Sha256Digest::of(bytes), bytes.len() as u64, observed_codec)
    }
}

impl PartialEq for MediaIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.sha256 == other.sha256 && self.byte_length == other.byte_length
    }
}

impl Eq for MediaIdentity {}

impl std::hash::Hash for MediaIdentity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.sha256.hash(state);
        self.byte_length.hash(state);
    }
}

/// Fidelity class of an acquired media candidate (spec 7.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityClass {
    /// The original high-fidelity recording as produced by the device.
    OriginalDeviceRecording,
    /// A compressed sidecar received over a device transfer (for example `.ASR`).
    CompressedDeviceTransfer,
    /// A copy obtained through a vendor cloud export.
    VendorCloudExport,
    /// A file supplied by the user.
    UserSuppliedFile,
    /// A derivative produced locally from another asset.
    LocalDerivative,
    /// Fidelity not established.
    #[serde(other)]
    Unknown,
}

impl QualityClass {
    /// Whether this class may be described to the user as an original.
    ///
    /// Spec 6.3(16): we never call an `.ASR` derivative the original stereo WAV.
    #[must_use]
    pub const fn is_original_fidelity(self) -> bool {
        matches!(self, Self::OriginalDeviceRecording)
    }
}

/// Integrity verification state of a stored asset (spec 13.2 `media_assets`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    /// Digest and byte length were re-checked against the stored row.
    Verified,
    /// Re-checking failed; the bytes must not be served.
    Failed,
    /// Bytes have not been re-read since they were written. Also the decoding
    /// of any state string this build does not recognize.
    #[serde(other)]
    Unverified,
}

/// A stored, immutable media blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAsset {
    /// Row identity.
    pub id: MediaAssetId,
    /// Content identity of the stored bytes.
    pub identity: MediaIdentity,
    /// Archive-relative path (spec 13.4).
    pub relative_path: String,
    /// Fidelity class of these bytes.
    pub quality_class: QualityClass,
    /// Integrity verification state.
    pub verification_state: VerificationState,
    /// When the integrity state was last established.
    pub verified_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_round_trips_through_hex() {
        let digest = Sha256Digest::of(b"hello");
        let text = digest.to_hex();
        assert_eq!(text.len(), 64);
        assert_eq!(text.parse::<Sha256Digest>().unwrap(), digest);
    }

    #[test]
    fn digest_parsing_rejects_uppercase_and_short_input() {
        assert!("ABC".parse::<Sha256Digest>().is_err());
        assert!(Sha256Digest::of(b"x").to_hex().to_uppercase().parse::<Sha256Digest>().is_err());
    }

    #[test]
    fn identity_requires_both_digest_and_byte_length() {
        let digest = Sha256Digest::of(b"same bytes");
        let a = MediaIdentity::new(digest, 10, "wav");
        let b = MediaIdentity::new(digest, 10, "mp3");
        let c = MediaIdentity::new(digest, 11, "wav");
        assert_eq!(a, b, "observed codec is provenance, not identity");
        assert_ne!(a, c, "a different byte length is a different identity");
    }

    #[test]
    fn compressed_device_transfer_is_never_called_an_original() {
        assert!(QualityClass::OriginalDeviceRecording.is_original_fidelity());
        assert!(!QualityClass::CompressedDeviceTransfer.is_original_fidelity());
        assert!(!QualityClass::Unknown.is_original_fidelity());
    }

    #[test]
    fn unknown_quality_and_verification_values_decode_conservatively() {
        let quality: QualityClass = serde_json::from_str("\"studio_master\"").unwrap();
        assert_eq!(quality, QualityClass::Unknown);
        let state: VerificationState = serde_json::from_str("\"probably_fine\"").unwrap();
        assert_eq!(state, VerificationState::Unverified);
    }

    #[test]
    fn archive_prefix_is_the_first_two_hex_characters() {
        let digest = Sha256Digest::from_bytes([0xab; 32]);
        assert_eq!(digest.hex_prefix(), "ab");
        assert!(digest.to_hex().starts_with(&digest.hex_prefix()));
    }
}
