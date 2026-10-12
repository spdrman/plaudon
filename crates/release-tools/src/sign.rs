//! Detached Ed25519 signing and verification for release manifests
//! (spec 16.5, 24).
//!
//! The release evidence manifest is signed so a reviewer can tell an archived
//! claim from a retyped one. The signing key never lives in the repository: it
//! is supplied through `PLAUDON_RELEASE_SIGNING_KEY` as base64 of the 32-byte
//! Ed25519 seed. When it is absent, signing fails loudly rather than emitting
//! an unsigned bundle that looks signed.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Environment variable carrying the base64 32-byte Ed25519 signing seed.
pub const SIGNING_KEY_ENV: &str = "PLAUDON_RELEASE_SIGNING_KEY";

/// Errors raised by signing or verification.
#[derive(Debug, thiserror::Error)]
pub enum SignError {
    /// No signing key was supplied, so nothing may be published.
    #[error("no release signing key: set {SIGNING_KEY_ENV} to the base64 Ed25519 seed")]
    MissingKey,
    /// The supplied key material is not a 32-byte base64 seed.
    #[error("malformed release signing key: {0}")]
    MalformedKey(String),
    /// The signature blob is not base64 of a 64-byte Ed25519 signature.
    #[error("malformed signature: {0}")]
    MalformedSignature(String),
    /// The signature does not verify against the signed bytes and key.
    #[error("signature does not verify for key id {key_id}")]
    SignatureMismatch {
        /// Key id named by the signature envelope.
        key_id: String,
    },
    /// The signature names a key the verifier does not trust.
    #[error("signature key id {key_id} is not in the trusted set")]
    UnknownKey {
        /// Key id named by the signature envelope.
        key_id: String,
    },
}

/// A detached signature plus the identity of the key that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetachedSignature {
    /// Signature algorithm; only `ed25519` exists today.
    pub algorithm: String,
    /// Stable identifier of the public key: the first 16 hex characters of the
    /// SHA-256 of the 32-byte public key.
    pub key_id: String,
    /// Base64 of the 64-byte detached signature.
    pub signature: String,
    /// Base64 of the 32-byte public key, so a bundle is self-describing and a
    /// reviewer can compare it against the published key out of band.
    pub public_key: String,
}

/// Derives the stable key id of a verifying key.
pub fn key_id(key: &VerifyingKey) -> String {
    crate::hash::sha256_bytes(key.as_bytes())[..16].to_owned()
}

/// Loads the signing key from the environment.
pub fn signing_key_from_env() -> Result<SigningKey, SignError> {
    let raw = std::env::var(SIGNING_KEY_ENV).map_err(|_| SignError::MissingKey)?;
    signing_key_from_base64(raw.trim())
}

/// Parses a base64 32-byte Ed25519 seed into a signing key.
pub fn signing_key_from_base64(seed: &str) -> Result<SigningKey, SignError> {
    let bytes = BASE64
        .decode(seed)
        .map_err(|e| SignError::MalformedKey(e.to_string()))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| SignError::MalformedKey("expected 32 seed bytes".to_owned()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Signs `bytes`, producing a self-describing detached signature.
pub fn sign(key: &SigningKey, bytes: &[u8]) -> DetachedSignature {
    let verifying = key.verifying_key();
    DetachedSignature {
        algorithm: "ed25519".to_owned(),
        key_id: key_id(&verifying),
        signature: BASE64.encode(key.sign(bytes).to_bytes()),
        public_key: BASE64.encode(verifying.as_bytes()),
    }
}

/// Verifies a detached signature over `bytes`.
///
/// `trusted` is the set of key ids the caller accepts. An empty set means
/// "trust on first use is not allowed": verification fails, because a bundle
/// that carries its own key and its own trust decision proves nothing.
pub fn verify(
    signature: &DetachedSignature,
    bytes: &[u8],
    trusted: &[String],
) -> Result<(), SignError> {
    if signature.algorithm != "ed25519" {
        return Err(SignError::MalformedSignature(format!(
            "unsupported algorithm {}",
            signature.algorithm
        )));
    }
    if !trusted.iter().any(|id| id == &signature.key_id) {
        return Err(SignError::UnknownKey {
            key_id: signature.key_id.clone(),
        });
    }
    let public = BASE64
        .decode(&signature.public_key)
        .map_err(|e| SignError::MalformedKey(e.to_string()))?;
    let public: [u8; 32] = public
        .try_into()
        .map_err(|_| SignError::MalformedKey("expected 32 public key bytes".to_owned()))?;
    let verifying =
        VerifyingKey::from_bytes(&public).map_err(|e| SignError::MalformedKey(e.to_string()))?;
    // The envelope cannot name one key and carry another.
    if key_id(&verifying) != signature.key_id {
        return Err(SignError::SignatureMismatch {
            key_id: signature.key_id.clone(),
        });
    }
    let raw = BASE64
        .decode(&signature.signature)
        .map_err(|e| SignError::MalformedSignature(e.to_string()))?;
    let raw: [u8; 64] = raw
        .try_into()
        .map_err(|_| SignError::MalformedSignature("expected 64 signature bytes".to_owned()))?;
    verifying
        .verify(bytes, &Signature::from_bytes(&raw))
        .map_err(|_| SignError::SignatureMismatch {
            key_id: signature.key_id.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
    }

    #[test]
    fn a_signature_verifies_against_its_own_key_id() {
        let key = test_key();
        let signature = sign(&key, b"evidence manifest");
        let trusted = vec![signature.key_id.clone()];
        verify(&signature, b"evidence manifest", &trusted).expect("verifies");
    }

    #[test]
    fn altered_bytes_fail_verification() {
        let key = test_key();
        let signature = sign(&key, b"evidence manifest");
        let trusted = vec![signature.key_id.clone()];
        let error = verify(&signature, b"evidence manifesT", &trusted).expect_err("must fail");
        assert!(matches!(error, SignError::SignatureMismatch { .. }));
    }

    #[test]
    fn an_untrusted_key_is_rejected_even_when_the_signature_is_valid() {
        let key = test_key();
        let signature = sign(&key, b"evidence manifest");
        let error = verify(&signature, b"evidence manifest", &[]).expect_err("must fail");
        assert!(matches!(error, SignError::UnknownKey { .. }));
    }

    #[test]
    fn an_envelope_naming_one_key_but_carrying_another_is_rejected() {
        let key = test_key();
        let other = SigningKey::from_bytes(&[9_u8; 32]);
        let mut signature = sign(&key, b"evidence manifest");
        signature.public_key = BASE64.encode(other.verifying_key().as_bytes());
        let trusted = vec![signature.key_id.clone()];
        let error = verify(&signature, b"evidence manifest", &trusted).expect_err("must fail");
        assert!(matches!(error, SignError::SignatureMismatch { .. }));
    }

    #[test]
    fn a_malformed_seed_is_reported_rather_than_truncated() {
        let error = signing_key_from_base64("not-base64!!").expect_err("must fail");
        assert!(matches!(error, SignError::MalformedKey(_)));
    }
}
