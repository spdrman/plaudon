//! Shared loading of the committed fixtures in `tests/fixtures/updater/`.
//!
//! Each integration test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_updater::install::Installer;
use plaudon_updater::testkeys::{test_trusted_keys, Artifact, BASE_FIXTURE_NAME, FIXTURE_TARGET};
use plaudon_updater::verify::{Ed25519Verifier, UpdateVerifier};

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
