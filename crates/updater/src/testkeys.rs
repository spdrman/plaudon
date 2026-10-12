//! TEST-ONLY signing keys and the fixture generator (SPEC v1.3 §18.5).
//!
//! # These keys are not release keys
//!
//! Both seeds below are committed in plain sight and are spelled
//! `plaudon-test-only-...` precisely so that no reader, grep or audit can
//! mistake them for release material. They are **never** members of
//! [`crate::config::EMBEDDED_TRUSTED_KEYS`]; a unit test in this module
//! asserts that, so a copy-paste accident that pinned a test key into the
//! shipping trust store fails the build.
//!
//! # Why this is a normal module and not `#[cfg(test)]`
//!
//! Three consumers need the identical signing logic: the inline unit tests in
//! [`crate::install`], the integration/security suites under `tests/`, and
//! the committed fixture generator (`tests/fixture_generator.rs`). A
//! `#[cfg(test)]` module is invisible to the latter two, and three copies of
//! a signing routine is exactly the kind of divergence §16.5 punishes. The
//! module is therefore compiled always and kept inert by the trust-store
//! assertion.

use ed25519_dalek::{Signer as _, SigningKey};

use crate::config::TrustedKeyEntry;
use crate::manifest::{
    DetachedSignature, Sha256Digest, Target, UpdateManifest, Version, MANIFEST_SCHEMA_VERSION,
};
use crate::verify::{sha256, TrustedKeys};

/// Key id of the test signing key. Mirrors the shape of a real key id but is
/// unmistakably test material.
pub const TEST_KEY_ID: &str = "plaudon-test-only-2026a";

/// Key id of a second, *untrusted* test key, used for the wrong-key fixture.
pub const FOREIGN_TEST_KEY_ID: &str = "plaudon-test-only-foreign";

/// Seed of the test signing key. 32 bytes, deterministic, test-only.
pub const TEST_KEY_SEED: &[u8; 32] = b"plaudon-test-only-signing-key-01";

/// Seed of the untrusted test key. 32 bytes, deterministic, test-only.
pub const FOREIGN_TEST_KEY_SEED: &[u8; 32] = b"plaudon-test-only-WRONG-key-0002";

/// Base64 public half of [`TEST_KEY_SEED`], committed so the pinned test
/// trust store can be a `const`.
pub const TEST_KEY_PUBLIC_BASE64: &str = "N1S4PrVs2bGye8rUzg379T1PG/chFXbPcS7sdEnq/2c=";

/// Base64 public half of [`FOREIGN_TEST_KEY_SEED`]. Present for assertions
/// only; it is deliberately absent from [`TEST_TRUSTED_KEYS`].
pub const FOREIGN_TEST_KEY_PUBLIC_BASE64: &str = "IDTQv5YWQBYH/pNv2W2WUbvQvMYKXhiqbjzeBK+5djM=";

/// The pinned trust store used by the fixture suites: exactly one key.
pub const TEST_TRUSTED_KEYS: &[TrustedKeyEntry] = &[TrustedKeyEntry {
    id: TEST_KEY_ID,
    public_key_base64: TEST_KEY_PUBLIC_BASE64,
}];

/// The test signing key.
#[must_use]
pub fn test_signing_key() -> SigningKey {
    SigningKey::from_bytes(TEST_KEY_SEED)
}

/// The untrusted signing key used to produce the wrong-key fixture.
#[must_use]
pub fn foreign_signing_key() -> SigningKey {
    SigningKey::from_bytes(FOREIGN_TEST_KEY_SEED)
}

/// A [`TrustedKeys`] set pinning only [`TEST_KEY_ID`].
///
/// # Panics
///
/// Panics if [`TEST_KEY_PUBLIC_BASE64`] stops matching [`TEST_KEY_SEED`].
#[must_use]
pub fn test_trusted_keys() -> TrustedKeys {
    TrustedKeys::from_entries(TEST_TRUSTED_KEYS).expect("test trust store is valid")
}

/// How a fixture deviates from a well-formed, validly signed artifact.
///
/// Each variant corresponds to one committed hostile fixture and to one
/// expected [`crate::error::VerifyError`] (§18.5 "reject unsigned or
/// corrupted application updates").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureFlaw {
    /// No deviation; this is the artifact that must install.
    None,
    /// The envelope carries no `manifest_signature`.
    Unsigned,
    /// Signed correctly, but by a key that is not pinned.
    WrongKey,
    /// The package is short by the given number of trailing bytes.
    TruncatedPackage,
    /// One package byte is flipped; the length still matches.
    TamperedPackageByte,
    /// A manifest field is edited after signing.
    TamperedManifestField,
    /// Validly signed, but older than the installed version.
    Downgrade,
    /// Validly signed, but built for another platform/architecture.
    WrongTarget,
}

impl FixtureFlaw {
    /// Every flaw, so the suites can iterate exhaustively.
    pub const ALL: &'static [Self] = &[
        Self::None,
        Self::Unsigned,
        Self::WrongKey,
        Self::TruncatedPackage,
        Self::TamperedPackageByte,
        Self::TamperedManifestField,
        Self::Downgrade,
        Self::WrongTarget,
    ];

    /// Fixture base name, used for both `<name>.manifest.json` and
    /// `<name>.package.bin`.
    #[must_use]
    pub const fn fixture_name(self) -> &'static str {
        match self {
            Self::None => "valid",
            Self::Unsigned => "unsigned",
            Self::WrongKey => "wrong-key",
            Self::TruncatedPackage => "truncated-package",
            Self::TamperedPackageByte => "tampered-package-byte",
            Self::TamperedManifestField => "tampered-manifest-field",
            Self::Downgrade => "downgrade",
            Self::WrongTarget => "wrong-target",
        }
    }
}

/// A generated artifact: the envelope document and the package bytes that
/// accompany it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Pretty-printed signed envelope, exactly as committed.
    pub envelope: String,
    /// Package payload bytes, exactly as committed.
    pub package: Vec<u8>,
}

/// Deterministic package body for a version, so fixtures are reproducible
/// byte-for-byte and a regenerated tree diffs cleanly.
#[must_use]
pub fn package_bytes(version: Version) -> Vec<u8> {
    let mut bytes = format!("plaudon-test-only application payload v{version}\n").into_bytes();
    // Pad to a fixed length with a version-derived pattern so truncation and
    // byte-flip fixtures have room to differ well away from the header.
    let filler = u8::try_from(version.minor % 251).unwrap_or(0).wrapping_add(1);
    bytes.resize(512, filler);
    bytes
}

/// The reference target every fixture except the wrong-target one uses.
///
/// Pinned rather than taken from the host so that the wrong-target fixture is
/// wrong on every machine the suite runs on (§22 item 17).
pub const FIXTURE_TARGET: Target = Target::new(
    crate::manifest::Platform::MacOs,
    crate::manifest::Arch::Aarch64,
);

/// A target no fixture is built for.
pub const FOREIGN_TARGET: Target = Target::new(
    crate::manifest::Platform::Windows,
    crate::manifest::Arch::X86_64,
);

/// Build one artifact with the given deviation.
///
/// `version` is the release the manifest installs and `floor` its
/// `minimum_supported_version`.
#[must_use]
pub fn build_artifact(flaw: FixtureFlaw, version: Version, floor: Version) -> Artifact {
    let key = match flaw {
        FixtureFlaw::WrongKey => foreign_signing_key(),
        _ => test_signing_key(),
    };
    let key_id = match flaw {
        FixtureFlaw::WrongKey => FOREIGN_TEST_KEY_ID,
        _ => TEST_KEY_ID,
    };
    let target = match flaw {
        FixtureFlaw::WrongTarget => FOREIGN_TARGET,
        _ => FIXTURE_TARGET,
    };

    // The package the manifest *describes* is always the intact one: a
    // hostile artifact is a correct manifest paired with wrong bytes.
    let honest_package = package_bytes(version);
    let manifest = UpdateManifest {
        version,
        minimum_supported_version: floor,
        target,
        package_url: format!("https://updates.plaudon.app/stable/plaudon-{version}.pkg"),
        package_sha256: sha256(&honest_package),
        package_size: honest_package.len() as u64,
        package_signature: DetachedSignature::from_bytes(
            key.sign(&honest_package).to_bytes(),
        ),
    };

    let mut manifest_json =
        serde_json::to_string_pretty(&manifest).expect("manifest serializes");
    let signature = key.sign(manifest_json.as_bytes());

    if flaw == FixtureFlaw::TamperedManifestField {
        // Edit a signed field after signing: the published digest now names
        // a different package than the signature covers.
        let honest_digest = manifest.package_sha256.to_hex();
        let tampered_digest = flip_last_hex_nibble(&honest_digest);
        manifest_json = manifest_json.replace(&honest_digest, &tampered_digest);
        assert!(
            manifest_json.contains(&tampered_digest),
            "the tampered-manifest fixture must actually differ"
        );
    }

    let signature_field = if flaw == FixtureFlaw::Unsigned {
        String::new()
    } else {
        use base64::Engine as _;
        format!(
            ",\n  \"manifest_signature\": \"{}\"",
            base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
        )
    };

    let indented = manifest_json.replace('\n', "\n  ");
    let envelope = format!(
        "{{\n  \"schema_version\": {MANIFEST_SCHEMA_VERSION},\n  \"manifest\": {indented},\n  \"signing_key_id\": \"{key_id}\"{signature_field}\n}}\n"
    );

    let package = match flaw {
        FixtureFlaw::TruncatedPackage => honest_package[..honest_package.len() - 64].to_vec(),
        FixtureFlaw::TamperedPackageByte => {
            let mut bytes = honest_package;
            let last = bytes.len() - 1;
            bytes[last] ^= 0x01;
            bytes
        }
        _ => honest_package,
    };

    Artifact { envelope, package }
}

fn flip_last_hex_nibble(hex_text: &str) -> String {
    let mut chars: Vec<char> = hex_text.chars().collect();
    let last = chars.len() - 1;
    chars[last] = if chars[last] == '0' { '1' } else { '0' };
    chars.into_iter().collect()
}

/// SHA-256 of a fixture package, for assertions that do not want to re-derive
/// the digest by hand.
#[must_use]
pub fn package_digest(version: Version) -> Sha256Digest {
    sha256(&package_bytes(version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EMBEDDED_TRUSTED_KEYS;

    #[test]
    fn no_test_key_is_ever_pinned_for_release() {
        for entry in EMBEDDED_TRUSTED_KEYS {
            assert_ne!(entry.id, TEST_KEY_ID);
            assert_ne!(entry.id, FOREIGN_TEST_KEY_ID);
            assert_ne!(entry.public_key_base64, TEST_KEY_PUBLIC_BASE64);
            assert_ne!(entry.public_key_base64, FOREIGN_TEST_KEY_PUBLIC_BASE64);
            assert!(
                !entry.id.contains("test-only"),
                "a release pin must not be test material"
            );
        }
    }

    #[test]
    fn the_committed_public_keys_match_their_committed_seeds() {
        use base64::Engine as _;
        let encode = |key: &SigningKey| {
            base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes())
        };
        assert_eq!(encode(&test_signing_key()), TEST_KEY_PUBLIC_BASE64);
        assert_eq!(encode(&foreign_signing_key()), FOREIGN_TEST_KEY_PUBLIC_BASE64);
    }

    #[test]
    fn every_flaw_has_a_distinct_fixture_name() {
        let mut names: Vec<&str> = FixtureFlaw::ALL.iter().map(|f| f.fixture_name()).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total);
        assert_eq!(total, 8);
    }
}
