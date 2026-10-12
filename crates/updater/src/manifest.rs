//! The signed update manifest (SPEC v1.3 §2.4, §16.5).
//!
//! Tauri 2 ships a signed update mechanism; §2.4 commits us to inspecting and
//! enforcing update validation ourselves rather than trusting the framework
//! default. The on-disk/over-the-wire shape is a two-level *envelope*:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "manifest": { "version": "1.4.0", "...": "..." },
//!   "signing_key_id": "plaudon-release-2026a",
//!   "manifest_signature": "<base64 ed25519>"
//! }
//! ```
//!
//! The detached manifest signature covers the **exact bytes of the `manifest`
//! member as they appear in the document**, not a re-serialization of the
//! parsed value. That is why [`SignedManifestEnvelope`] keeps the member as a
//! [`RawValue`]: a canonicalization step would be a second, silently
//! divergent parser, and any difference between "bytes we verified" and
//! "bytes we parsed" is exactly the hole §16.5 forbids.
//!
//! The matching JSON Schema is `schemas/update-manifest.v1.schema.json`.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use crate::error::VerifyError;

/// Schema version of the envelope understood by this build.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Length in bytes of an ed25519 detached signature.
pub const SIGNATURE_LEN: usize = 64;

/// Length in bytes of a SHA-256 digest.
pub const DIGEST_LEN: usize = 32;

/// A three-component release version (`major.minor.patch`).
///
/// Ordering is numeric per component, so `0.10.0 > 0.9.9`. Pre-release and
/// build metadata are deliberately unsupported: §16.5 wants update decisions
/// to be reviewable by inspection, and a partial semver implementation whose
/// ordering rules differ from the published ones is worse than none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major component.
    pub major: u32,
    /// Minor component.
    pub minor: u32,
    /// Patch component.
    pub patch: u32,
}

impl Version {
    /// Construct a version from its components.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Failure to parse a [`Version`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a major.minor.patch version")]
pub struct VersionParseError(String);

impl FromStr for Version {
    type Err = VersionParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let reject = || VersionParseError(text.to_owned());
        let mut parts = text.split('.');
        let mut next = || -> Result<u32, VersionParseError> {
            let part = parts.next().ok_or_else(reject)?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(reject());
            }
            // Reject `01` so that one version has exactly one spelling.
            if part.len() > 1 && part.starts_with('0') {
                return Err(reject());
            }
            part.parse::<u32>().map_err(|_| reject())
        };
        let major = next()?;
        let minor = next()?;
        let patch = next()?;
        if parts.next().is_some() {
            return Err(reject());
        }
        Ok(Self::new(major, minor, patch))
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// Operating-system family an update package targets (§2.4, §22 item 17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Apple macOS, notarized artifact.
    MacOs,
    /// Microsoft Windows, signed installer.
    Windows,
    /// Linux; not a supported V1 release target, but representable so a
    /// foreign manifest is rejected by name rather than by parse failure.
    Linux,
}

impl Platform {
    /// Platform of the running build.
    #[must_use]
    pub const fn host() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::Linux
        }
    }

    /// Stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "mac_os",
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

/// CPU architecture an update package targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    /// 64-bit ARM (Apple silicon, Windows on ARM).
    Aarch64,
    /// 64-bit x86.
    X86_64,
}

impl Arch {
    /// Architecture of the running build.
    #[must_use]
    pub const fn host() -> Self {
        #[cfg(target_arch = "aarch64")]
        {
            Self::Aarch64
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            Self::X86_64
        }
    }

    /// Stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }
}

/// The platform/architecture pair a package is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Target {
    /// Operating-system family.
    pub platform: Platform,
    /// CPU architecture.
    pub arch: Arch,
}

impl Target {
    /// Construct a target.
    #[must_use]
    pub const fn new(platform: Platform, arch: Arch) -> Self {
        Self { platform, arch }
    }

    /// Target of the running build (§22 item 17: we never claim support for a
    /// cell we did not build).
    #[must_use]
    pub const fn host() -> Self {
        Self::new(Platform::host(), Arch::host())
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.platform.as_str(), self.arch.as_str())
    }
}

/// A lowercase-hex SHA-256 digest of exactly [`DIGEST_LEN`] bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; DIGEST_LEN]);

impl Sha256Digest {
    /// Wrap raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    /// Raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    /// Lowercase hex rendering, the only form that appears in manifests.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

/// Failure to parse a [`Sha256Digest`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a 64-character lowercase hex SHA-256 digest")]
pub struct DigestParseError;

impl FromStr for Sha256Digest {
    type Err = DigestParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() != DIGEST_LEN * 2
            || !text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(DigestParseError);
        }
        let mut bytes = [0u8; DIGEST_LEN];
        hex::decode_to_slice(text, &mut bytes).map_err(|_| DigestParseError)?;
        Ok(Self(bytes))
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// A detached ed25519 signature, base64 (standard alphabet, padded) on the
/// wire.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DetachedSignature([u8; SIGNATURE_LEN]);

impl DetachedSignature {
    /// Wrap raw signature bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SIGNATURE_LEN]) -> Self {
        Self(bytes)
    }

    /// Raw signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SIGNATURE_LEN] {
        &self.0
    }

    /// Base64 rendering, the only form that appears in manifests.
    #[must_use]
    pub fn to_base64(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(self.0)
    }
}

/// Signatures are never printed in full: §15.4 forbids raw credential-shaped
/// material in anything a user or a log can see.
impl fmt::Debug for DetachedSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DetachedSignature(<64 bytes>)")
    }
}

/// Failure to parse a [`DetachedSignature`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a base64-encoded 64-byte ed25519 signature")]
pub struct SignatureParseError;

impl FromStr for DetachedSignature {
    type Err = SignatureParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|_| SignatureParseError)?;
        let bytes: [u8; SIGNATURE_LEN] = raw.try_into().map_err(|_| SignatureParseError)?;
        Ok(Self(bytes))
    }
}

impl Serialize for DetachedSignature {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for DetachedSignature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// The inner, signed update document (§2.4, §16.5).
///
/// Every field participates in the manifest signature, so none of them can be
/// edited in transit without producing [`VerifyError::SignatureMismatch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateManifest {
    /// Version this package installs.
    pub version: Version,
    /// Lowest version the installation is allowed to run after any install or
    /// rollback decision made with this manifest. Rollback honours this floor
    /// unless the artifact is the retained, previously verified one
    /// (§16.5: "rollback must not weaken signature checking").
    pub minimum_supported_version: Version,
    /// Platform/architecture this package is built for.
    pub target: Target,
    /// Where the package is published. Carried for the transport layer only;
    /// it is never dereferenced in Local-only Mode (§16.2A, §22 item 15).
    pub package_url: String,
    /// SHA-256 of the package bytes.
    pub package_sha256: Sha256Digest,
    /// Exact byte length of the package, so a short read is reported as
    /// [`VerifyError::TruncatedPackage`] rather than a digest mismatch.
    pub package_size: u64,
    /// Detached ed25519 signature over the package bytes, by the same key
    /// that signed the manifest.
    pub package_signature: DetachedSignature,
}

impl UpdateManifest {
    /// Reject a manifest whose target is not this build's (§22 item 17).
    pub fn check_target(&self, host: Target) -> Result<(), VerifyError> {
        if self.target == host {
            return Ok(());
        }
        Err(VerifyError::UnsupportedTarget {
            offered: self.target.to_string(),
            host: host.to_string(),
        })
    }

    /// Structural invariants that the signature cannot express.
    ///
    /// A correctly signed but self-contradictory manifest (a release below
    /// its own floor, an empty URL, a zero-length package) is malformed, not
    /// merely unsatisfiable.
    pub fn check_well_formed(&self) -> Result<(), VerifyError> {
        if self.version < self.minimum_supported_version {
            return Err(VerifyError::MalformedManifest(format!(
                "version {} is below its own minimum_supported_version {}",
                self.version, self.minimum_supported_version
            )));
        }
        if self.package_url.is_empty() {
            return Err(VerifyError::MalformedManifest(
                "package_url is empty".to_owned(),
            ));
        }
        if self.package_size == 0 {
            return Err(VerifyError::MalformedManifest(
                "package_size is zero".to_owned(),
            ));
        }
        Ok(())
    }
}

/// The transported envelope: an opaque manifest document plus the detached
/// signature over its exact bytes.
///
/// Deserialize with [`SignedManifestEnvelope::parse`] so that the borrowed
/// `manifest` member keeps pointing at the original buffer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifestEnvelope<'a> {
    /// Envelope schema version; this build understands
    /// [`MANIFEST_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The signed document, exactly as received.
    #[serde(borrow)]
    pub manifest: &'a RawValue,
    /// Id of the pinned key that signed `manifest` and the package.
    pub signing_key_id: String,
    /// Detached ed25519 signature over `manifest`'s bytes. Absent for an
    /// unsigned manifest, which is rejected with
    /// [`VerifyError::UnsignedManifest`].
    #[serde(default)]
    pub manifest_signature: Option<String>,
}

impl<'a> SignedManifestEnvelope<'a> {
    /// Parse an envelope out of raw bytes without copying the signed region.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, VerifyError> {
        let envelope: Self = serde_json::from_slice(bytes)
            .map_err(|e| VerifyError::MalformedManifest(e.to_string()))?;
        if envelope.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(VerifyError::MalformedManifest(format!(
                "unsupported envelope schema_version {}",
                envelope.schema_version
            )));
        }
        if envelope.signing_key_id.is_empty() {
            return Err(VerifyError::MalformedManifest(
                "signing_key_id is empty".to_owned(),
            ));
        }
        Ok(envelope)
    }

    /// The exact bytes covered by [`Self::manifest_signature`].
    #[must_use]
    pub fn signed_bytes(&self) -> &'a [u8] {
        self.manifest.get().as_bytes()
    }

    /// Parse the inner document.
    pub fn document(&self) -> Result<UpdateManifest, VerifyError> {
        let manifest: UpdateManifest = serde_json::from_str(self.manifest.get())
            .map_err(|e| VerifyError::MalformedManifest(e.to_string()))?;
        manifest.check_well_formed()?;
        Ok(manifest)
    }

    /// The detached signature, or [`VerifyError::UnsignedManifest`] when the
    /// envelope carries none. An empty string counts as none: §18.5 requires
    /// us to reject unsigned updates, and "present but empty" is unsigned.
    pub fn signature(&self) -> Result<DetachedSignature, VerifyError> {
        let text = self
            .manifest_signature
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(VerifyError::UnsignedManifest)?;
        text.parse().map_err(|_| {
            VerifyError::MalformedManifest(
                "manifest_signature is not a base64 ed25519 signature".to_owned(),
            )
        })
    }
}

/// A manifest whose envelope signature verified against a pinned key.
///
/// Only [`crate::verify`] can mint this type, so holding one is proof that
/// §16.5 verification ran. It is deliberately not `Deserialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedManifest {
    document: UpdateManifest,
    signing_key_id: String,
}

impl VerifiedManifest {
    /// Construct the proof token. Crate-internal on purpose.
    pub(crate) fn new(document: UpdateManifest, signing_key_id: String) -> Self {
        Self {
            document,
            signing_key_id,
        }
    }

    /// The verified document.
    #[must_use]
    pub const fn document(&self) -> &UpdateManifest {
        &self.document
    }

    /// Id of the pinned key whose signature verified.
    #[must_use]
    pub fn signing_key_id(&self) -> &str {
        &self.signing_key_id
    }

    /// Version this manifest installs.
    #[must_use]
    pub const fn version(&self) -> Version {
        self.document.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_numerically_not_lexically() {
        let nine: Version = "0.9.9".parse().expect("parses");
        let ten: Version = "0.10.0".parse().expect("parses");
        assert!(ten > nine, "0.10.0 must outrank 0.9.9");
    }

    #[test]
    fn versions_have_exactly_one_spelling() {
        for bad in [
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-beta",
            "v1.2.3",
            "",
            "1.2.x",
        ] {
            assert!(
                bad.parse::<Version>().is_err(),
                "`{bad}` must not parse as a version"
            );
        }
        assert_eq!(
            "1.2.3".parse::<Version>().expect("parses").to_string(),
            "1.2.3"
        );
    }

    #[test]
    fn digests_reject_uppercase_and_short_hex() {
        assert!("ab".repeat(32).parse::<Sha256Digest>().is_ok());
        assert!("AB".repeat(32).parse::<Sha256Digest>().is_err());
        assert!("ab".repeat(31).parse::<Sha256Digest>().is_err());
    }

    #[test]
    fn signature_debug_never_prints_bytes() {
        let signature = DetachedSignature::from_bytes([7u8; SIGNATURE_LEN]);
        let rendered = format!("{signature:?}");
        assert_eq!(rendered, "DetachedSignature(<64 bytes>)");
        assert!(!rendered.contains('7'), "signature bytes must not leak");
    }

    #[test]
    fn signed_bytes_are_the_exact_source_region() {
        let raw = br#"{"schema_version":1,"manifest":{"a":  1},"signing_key_id":"k","manifest_signature":"x"}"#;
        let envelope = SignedManifestEnvelope::parse(raw).expect("parses");
        assert_eq!(envelope.signed_bytes(), br#"{"a":  1}"#);
    }

    #[test]
    fn a_missing_or_empty_signature_is_unsigned() {
        let missing = br#"{"schema_version":1,"manifest":{},"signing_key_id":"k"}"#;
        let empty =
            br#"{"schema_version":1,"manifest":{},"signing_key_id":"k","manifest_signature":""}"#;
        for raw in [missing.as_slice(), empty.as_slice()] {
            let envelope = SignedManifestEnvelope::parse(raw).expect("parses");
            assert_eq!(
                envelope.signature().expect_err("must be unsigned"),
                VerifyError::UnsignedManifest
            );
        }
    }
}
