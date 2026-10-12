//! Cryptographic verification of update manifests and packages
//! (SPEC v1.3 §16.5, §18.5, §22 item 8).
//!
//! The rules, in the order they are applied:
//!
//! 1. The envelope must parse and name a `signing_key_id`.
//! 2. It must carry a detached signature at all ([`VerifyError::UnsignedManifest`]).
//! 3. The key id must be in the pinned set ([`VerifyError::UnknownKey`]).
//! 4. ed25519 over the exact signed region must verify
//!    ([`VerifyError::SignatureMismatch`]).
//! 5. The package length must match, then its SHA-256, then its own detached
//!    ed25519 signature by the same key.
//!
//! Only after step 4 does a [`VerifiedManifest`] exist, and only after step 5
//! may any byte be written into an install slot. §16.5 requires that rollback
//! use *this same* path: [`crate::install`] therefore owns no verification
//! logic of its own and calls an [`UpdateVerifier`] on both directions.

use std::collections::BTreeMap;
use std::fmt;

use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use sha2::{Digest as _, Sha256};

use crate::config::{TrustedKeyEntry, EMBEDDED_TRUSTED_KEYS};
use crate::error::{SignatureSubject, VerifyError};
use crate::manifest::{Sha256Digest, SignedManifestEnvelope, UpdateManifest, VerifiedManifest};

/// The pinned public keys this build will accept signatures from (§16.5).
#[derive(Debug, Clone, Default)]
pub struct TrustedKeys {
    keys: BTreeMap<String, VerifyingKey>,
}

impl TrustedKeys {
    /// The key set compiled into this build
    /// ([`crate::config::EMBEDDED_TRUSTED_KEYS`]).
    ///
    /// # Panics
    ///
    /// Panics if a compiled-in key is not a valid ed25519 public key. That is
    /// a build-time defect, not a runtime condition: shipping with an
    /// unusable pin would silently disable update verification.
    #[must_use]
    pub fn embedded() -> Self {
        Self::from_entries(EMBEDDED_TRUSTED_KEYS)
            .expect("the compiled-in trusted key set must be valid")
    }

    /// Build a key set from literal entries. Used by the embedded set and by
    /// the fixture suite, which pins its own clearly-marked test keys.
    pub fn from_entries(entries: &[TrustedKeyEntry]) -> Result<Self, VerifyError> {
        let mut keys = BTreeMap::new();
        for entry in entries {
            let key = decode_verifying_key(entry.public_key_base64).ok_or_else(|| {
                VerifyError::MalformedManifest(format!(
                    "pinned key `{}` is not a valid ed25519 public key",
                    entry.id
                ))
            })?;
            keys.insert(entry.id.to_owned(), key);
        }
        Ok(Self { keys })
    }

    /// Look a key id up, rejecting anything unpinned (§16.5).
    pub fn get(&self, key_id: &str) -> Result<&VerifyingKey, VerifyError> {
        self.keys.get(key_id).ok_or_else(|| VerifyError::UnknownKey {
            key_id: key_id.to_owned(),
        })
    }

    /// Ids in the set, sorted. Shown in diagnostics; never the key bytes.
    #[must_use]
    pub fn key_ids(&self) -> Vec<&str> {
        self.keys.keys().map(String::as_str).collect()
    }

    /// Number of pinned keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the set is empty. An empty set rejects every update.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

fn decode_verifying_key(base64_text: &str) -> Option<VerifyingKey> {
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(base64_text)
        .ok()?;
    let bytes: [u8; 32] = raw.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// SHA-256 of a byte slice.
#[must_use]
pub fn sha256(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    Sha256Digest::from_bytes(digest.into())
}

/// The verification seam used by both install and rollback.
///
/// `install` and `rollback` take the *same* `&dyn UpdateVerifier`, which is
/// how §16.5's "rollback must not weaken signature checking" is enforced
/// structurally rather than by convention — and how the integration suite can
/// observe, with a recording decorator, that both directions verified.
pub trait UpdateVerifier: fmt::Debug + Send + Sync {
    /// Verify the detached manifest signature over `envelope_bytes` and
    /// return the proof token.
    fn verify_manifest(&self, envelope_bytes: &[u8]) -> Result<VerifiedManifest, VerifyError>;

    /// Verify package length, SHA-256 digest and detached package signature.
    fn verify_package(
        &self,
        manifest: &VerifiedManifest,
        package: &[u8],
    ) -> Result<(), VerifyError>;
}

/// The production verifier: pinned ed25519 keys plus SHA-256.
#[derive(Debug, Clone)]
pub struct Ed25519Verifier {
    keys: TrustedKeys,
}

impl Ed25519Verifier {
    /// Verifier pinned to an explicit key set.
    #[must_use]
    pub const fn new(keys: TrustedKeys) -> Self {
        Self { keys }
    }

    /// Verifier pinned to the compiled-in release keys (§16.5).
    #[must_use]
    pub fn embedded() -> Self {
        Self::new(TrustedKeys::embedded())
    }

    /// The pinned key set, for diagnostics and drift tests.
    #[must_use]
    pub const fn keys(&self) -> &TrustedKeys {
        &self.keys
    }
}

impl UpdateVerifier for Ed25519Verifier {
    fn verify_manifest(&self, envelope_bytes: &[u8]) -> Result<VerifiedManifest, VerifyError> {
        let envelope = SignedManifestEnvelope::parse(envelope_bytes)?;
        // Order matters: "unsigned" is reported as unsigned even when the key
        // id is also unknown, because that is the more actionable fact and
        // §18.5 names it explicitly.
        let signature_bytes = envelope.signature()?;
        let key = self.keys.get(&envelope.signing_key_id)?;
        let signature = Signature::from_bytes(signature_bytes.as_bytes());
        key.verify(envelope.signed_bytes(), &signature)
            .map_err(|_| VerifyError::SignatureMismatch {
                subject: SignatureSubject::Manifest,
            })?;
        let document = envelope.document()?;
        Ok(VerifiedManifest::new(document, envelope.signing_key_id))
    }

    fn verify_package(
        &self,
        manifest: &VerifiedManifest,
        package: &[u8],
    ) -> Result<(), VerifyError> {
        let document = manifest.document();
        let actual_len = package.len() as u64;
        if actual_len < document.package_size {
            return Err(VerifyError::TruncatedPackage {
                expected: document.package_size,
                actual: actual_len,
            });
        }
        let actual_digest = sha256(package);
        if actual_len != document.package_size || actual_digest != document.package_sha256 {
            return Err(VerifyError::DigestMismatch {
                expected: document.package_sha256.to_hex(),
                actual: actual_digest.to_hex(),
            });
        }
        // The signing key is the one that already authenticated the manifest,
        // so a package cannot be re-signed by a different pinned key.
        let key = self.keys.get(manifest.signing_key_id())?;
        let signature = Signature::from_bytes(document.package_signature.as_bytes());
        key.verify(package, &signature)
            .map_err(|_| VerifyError::SignatureMismatch {
                subject: SignatureSubject::Package,
            })
    }
}

/// Reject an artifact that would move the installation to a version it is not
/// allowed to run (§16.5).
///
/// `floor` is the highest `minimum_supported_version` the installation has
/// committed to. `allow_below_floor` is set **only** for the retained,
/// previously verified artifact: rolling back to the exact state we came from
/// is always permitted, while rolling "back" to any other artifact must not
/// become a downgrade hole.
pub fn check_version_policy(
    offered: &UpdateManifest,
    installed: crate::manifest::Version,
    floor: crate::manifest::Version,
    allow_below_floor: bool,
) -> Result<(), VerifyError> {
    let effective_floor = floor.max(offered.minimum_supported_version);
    if !allow_below_floor && offered.version < effective_floor {
        return Err(VerifyError::DowngradeBlocked {
            offered: offered.version,
            installed,
            floor: effective_floor,
        });
    }
    Ok(())
}

/// Reject a forward update that is not strictly newer than what is installed
/// (§16.5). Rollback uses [`check_version_policy`] alone.
pub fn check_forward_progress(
    offered: &UpdateManifest,
    installed: crate::manifest::Version,
    floor: crate::manifest::Version,
) -> Result<(), VerifyError> {
    check_version_policy(offered, installed, floor, false)?;
    if offered.version <= installed {
        return Err(VerifyError::DowngradeBlocked {
            offered: offered.version,
            installed,
            floor: floor.max(offered.minimum_supported_version),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Version;

    #[test]
    fn the_embedded_key_set_is_usable() {
        let keys = TrustedKeys::embedded();
        assert_eq!(keys.len(), 1);
        assert!(keys.get(crate::config::RELEASE_SIGNING_KEY_ID).is_ok());
    }

    #[test]
    fn an_unpinned_key_id_is_rejected_by_name() {
        let keys = TrustedKeys::embedded();
        assert_eq!(
            keys.get("attacker-key").expect_err("must reject"),
            VerifyError::UnknownKey {
                key_id: "attacker-key".to_owned()
            }
        );
    }

    #[test]
    fn a_pinned_key_that_is_not_a_curve_point_fails_loudly() {
        let bad = [TrustedKeyEntry {
            id: "bogus",
            public_key_base64: "not-base64!!",
        }];
        assert!(TrustedKeys::from_entries(&bad).is_err());
    }

    #[test]
    fn sha256_matches_the_published_empty_digest() {
        assert_eq!(
            sha256(b"").to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    fn manifest_at(version: &str, floor: &str) -> UpdateManifest {
        UpdateManifest {
            version: version.parse().expect("version"),
            minimum_supported_version: floor.parse().expect("floor"),
            target: crate::manifest::Target::host(),
            package_url: "https://example.invalid/p".to_owned(),
            package_sha256: sha256(b"x"),
            package_size: 1,
            package_signature: crate::manifest::DetachedSignature::from_bytes([0u8; 64]),
        }
    }

    #[test]
    fn forward_progress_requires_a_strictly_newer_version() {
        let installed = Version::new(1, 2, 0);
        let floor = Version::new(1, 0, 0);
        assert!(check_forward_progress(&manifest_at("1.3.0", "1.0.0"), installed, floor).is_ok());
        assert!(check_forward_progress(&manifest_at("1.2.0", "1.0.0"), installed, floor).is_err());
        assert!(check_forward_progress(&manifest_at("1.1.0", "1.0.0"), installed, floor).is_err());
    }

    #[test]
    fn the_floor_only_yields_for_the_retained_artifact() {
        let installed = Version::new(1, 3, 0);
        let floor = Version::new(1, 2, 0);
        let old = manifest_at("1.1.0", "1.1.0");
        assert!(check_version_policy(&old, installed, floor, false).is_err());
        assert!(check_version_policy(&old, installed, floor, true).is_ok());
    }
}
