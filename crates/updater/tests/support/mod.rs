//! Shared loading of the committed fixtures in `tests/fixtures/updater/`.
//!
//! Each integration test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed25519_dalek::{Signer as _, SigningKey};
use plaudon_updater::install::Installer;
use plaudon_updater::manifest::{
    DetachedSignature, Target, UpdateManifest, Version, MANIFEST_SCHEMA_VERSION,
};
use plaudon_updater::testkeys::{test_trusted_keys, Artifact, BASE_FIXTURE_NAME, FIXTURE_TARGET};
use plaudon_updater::verify::{sha256, Ed25519Verifier, UpdateVerifier};

/// Absolute path of the repository root.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

/// Absolute path of the committed fixture directory.
pub fn fixtures_dir() -> PathBuf {
    repo_root().join("tests/fixtures/updater")
}

/// Load a committed fixture pair by base name.
pub fn load(name: &str) -> Artifact {
    let dir = fixtures_dir();
    let manifest_path = dir.join(format!("{name}.manifest.json"));
    let package_path = dir.join(format!("{name}.package.bin"));
    Artifact {
        envelope: std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display())),
        package: std::fs::read(&package_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", package_path.display())),
    }
}

/// A verifier pinned to the test trust store.
pub fn verifier() -> Arc<dyn UpdateVerifier> {
    Arc::new(Ed25519Verifier::new(test_trusted_keys()))
}

/// Bootstrap an installation from the committed baseline fixture.
pub fn bootstrapped(root: &Path, verifier: Arc<dyn UpdateVerifier>) -> Installer {
    let base = load(BASE_FIXTURE_NAME);
    Installer::bootstrap(
        root,
        verifier,
        FIXTURE_TARGET,
        base.envelope.as_bytes(),
        &base.package,
    )
    .expect("the committed baseline fixture verifies")
}

/// Render a manifest exactly as the fixture generator embeds it in an
/// envelope: pretty-printed, then re-indented by two spaces so the signed
/// region is byte-identical to what a committed fixture carries.
pub fn embed(manifest: &UpdateManifest) -> String {
    serde_json::to_string_pretty(manifest)
        .expect("manifest serializes")
        .replace('\n', "\n  ")
}

/// Base64 of an ed25519 signature over `bytes`.
pub fn sign_base64(key: &SigningKey, bytes: &[u8]) -> String {
    DetachedSignature::from_bytes(key.sign(bytes).to_bytes()).to_base64()
}

/// Assemble an envelope around an already-embedded manifest document.
///
/// Every part is a parameter because the negative-path suites need envelopes
/// the fixture generator deliberately does not produce: a foreign schema
/// version, an empty key id, a signature that is not base64, no signature at
/// all.
pub fn envelope_with(
    schema_version: u32,
    embedded: &str,
    key_id: &str,
    signature_base64: Option<&str>,
) -> String {
    let signature_field = match signature_base64 {
        Some(signature) => format!(",\n  \"manifest_signature\": \"{signature}\""),
        None => String::new(),
    };
    format!(
        "{{\n  \"schema_version\": {schema_version},\n  \"manifest\": {embedded},\n  \"signing_key_id\": \"{key_id}\"{signature_field}\n}}\n"
    )
}

/// Sign `manifest` with `key` and wrap it in a well-formed envelope.
pub fn signed_envelope(manifest: &UpdateManifest, key: &SigningKey, key_id: &str) -> String {
    let embedded = embed(manifest);
    let signature = sign_base64(key, embedded.as_bytes());
    envelope_with(MANIFEST_SCHEMA_VERSION, &embedded, key_id, Some(&signature))
}

/// A manifest describing `package` at `version`, signed field-by-field by
/// `key`. The returned manifest is well formed; callers degrade it.
pub fn manifest_for(
    version: Version,
    floor: Version,
    target: Target,
    package: &[u8],
    key: &SigningKey,
) -> UpdateManifest {
    UpdateManifest {
        version,
        minimum_supported_version: floor,
        target,
        package_url: format!("https://updates.plaudon.app/stable/plaudon-{version}.pkg"),
        package_sha256: sha256(package),
        package_size: package.len() as u64,
        package_signature: DetachedSignature::from_bytes(key.sign(package).to_bytes()),
    }
}
