//! Forward update then rollback, with a recording verifier proving that the
//! identical verification path ran on **both** directions
//! (SPEC v1.3 §16.5 "rollback must not weaken signature checking",
//! §22 item 8).

mod support;

use std::sync::{Arc, Mutex};

use plaudon_updater::error::{UpdateError, VerifyError};
use plaudon_updater::install::Installer;
use plaudon_updater::manifest::{Version, VerifiedManifest};
use plaudon_updater::testkeys::{
    test_trusted_keys, FixtureFlaw, BASE_VERSION, FIXTURE_TARGET, UPDATE_VERSION,
};
use plaudon_updater::verify::{Ed25519Verifier, UpdateVerifier};

use support::{bootstrapped, load};

/// One observed verification call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Manifest { version: Option<Version> },
    Package { version: Version, bytes: usize },
}

/// Wraps the real verifier and records every call, without weakening any
/// check: the decision always comes from [`Ed25519Verifier`].
#[derive(Debug)]
struct RecordingVerifier {
    inner: Ed25519Verifier,
    calls: Mutex<Vec<Call>>,
}

impl RecordingVerifier {
    fn new() -> Self {
        Self {
            inner: Ed25519Verifier::new(test_trusted_keys()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("not poisoned").clone()
    }
}

impl UpdateVerifier for RecordingVerifier {
    fn verify_manifest(&self, envelope_bytes: &[u8]) -> Result<VerifiedManifest, VerifyError> {
        let outcome = self.inner.verify_manifest(envelope_bytes);
        self.calls.lock().expect("not poisoned").push(Call::Manifest {
            version: outcome.as_ref().ok().map(VerifiedManifest::version),
        });
        outcome
    }

    fn verify_package(
        &self,
        manifest: &VerifiedManifest,
        package: &[u8],
    ) -> Result<(), VerifyError> {
        self.calls.lock().expect("not poisoned").push(Call::Package {
            version: manifest.version(),
            bytes: package.len(),
        });
        self.inner.verify_package(manifest, package)
    }
}

#[test]
fn a_forward_update_and_a_rollback_both_run_the_full_verification_path() {
    let root = tempfile::tempdir().expect("tempdir");
    let recorder = Arc::new(RecordingVerifier::new());
    let verifier: Arc<dyn UpdateVerifier> = recorder.clone();

    // ---- bootstrap -------------------------------------------------------
    let mut installer = bootstrapped(root.path(), verifier.clone());
    assert_eq!(
        recorder.calls(),
        vec![
            Call::Manifest {
                version: Some(BASE_VERSION)
            },
            Call::Package {
                version: BASE_VERSION,
                bytes: 512
            },
        ],
        "the shipped build is verified like any other artifact"
    );

    // ---- forward ---------------------------------------------------------
    let update = load(FixtureFlaw::None.fixture_name());
    assert_eq!(
        installer
            .install(update.envelope.as_bytes(), &update.package)
            .expect("install"),
        UPDATE_VERSION
    );
    let after_forward = recorder.calls();
    assert_eq!(
        after_forward[2..],
        [
            Call::Manifest {
                version: Some(UPDATE_VERSION)
            },
            Call::Package {
                version: UPDATE_VERSION,
                bytes: 512
            },
        ],
        "the forward path verifies manifest then package"
    );

    // ---- rollback --------------------------------------------------------
    assert_eq!(installer.rollback().expect("rollback"), BASE_VERSION);
    let after_rollback = recorder.calls();
    assert_eq!(
        after_rollback[4..],
        [
            Call::Manifest {
                version: Some(BASE_VERSION)
            },
            Call::Package {
                version: BASE_VERSION,
                bytes: 512
            },
        ],
        "rollback re-runs the identical manifest-then-package verification"
    );

    // Both directions performed exactly the same kind and number of checks.
    let forward: Vec<std::mem::Discriminant<Call>> = after_forward[2..]
        .iter()
        .map(std::mem::discriminant)
        .collect();
    let backward: Vec<std::mem::Discriminant<Call>> = after_rollback[4..]
        .iter()
        .map(std::mem::discriminant)
        .collect();
    assert_eq!(forward, backward, "rollback must not skip a check");

    assert_eq!(installer.installed_version(), BASE_VERSION);
    assert_eq!(
        installer.launchable().expect("launchable").version,
        BASE_VERSION
    );
}

#[test]
fn rollback_rejects_a_tampered_retained_artifact_with_the_same_strictness() {
    let root = tempfile::tempdir().expect("tempdir");
    let recorder = Arc::new(RecordingVerifier::new());
    let verifier: Arc<dyn UpdateVerifier> = recorder.clone();
    let mut installer = bootstrapped(root.path(), verifier);
    let update = load(FixtureFlaw::None.fixture_name());
    installer
        .install(update.envelope.as_bytes(), &update.package)
        .expect("install");

    // Replace the retained baseline manifest with the tampered-field fixture
    // of the same release line: the bytes on disk are no longer authentic.
    let slot = installer.journal().previous.expect("retained").slot;
    let tampered = load(FixtureFlaw::TamperedManifestField.fixture_name());
    std::fs::write(
        root.path()
            .join("slots")
            .join(slot.as_str())
            .join("manifest.json"),
        tampered.envelope.as_bytes(),
    )
    .expect("write");

    let error = installer.rollback().expect_err("rollback must re-verify");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::SignatureMismatch { .. })
        ),
        "expected a signature mismatch, got {error:?}"
    );
    assert_eq!(
        installer.installed_version(),
        UPDATE_VERSION,
        "a refused rollback must not move the active slot"
    );
    assert!(recorder
        .calls()
        .iter()
        .any(|call| matches!(call, Call::Manifest { version: None })));

    // And a relaunch still comes up on the new version.
    let reopened = Installer::open(root.path(), Arc::new(Ed25519Verifier::new(test_trusted_keys())), FIXTURE_TARGET)
        .expect("reopen");
    assert_eq!(
        reopened.launchable().expect("launchable").version,
        UPDATE_VERSION
    );
}
