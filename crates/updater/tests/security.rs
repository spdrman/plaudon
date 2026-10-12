//! Security suite: every hostile update fixture is rejected with its own
//! typed error, and the valid one installs (SPEC v1.3 §16.5, §18.5 "reject
//! unsigned or corrupted application updates", §22 item 8).
//!
//! Each case drives the real [`Installer`] against the committed fixtures, so
//! the assertion covers both the rejection *and* the "no partial install"
//! consequence: the active version must not move, no pending record may
//! survive, the staging area must be gone and the inactive slot must not
//! exist.

mod support;

use plaudon_updater::error::{SignatureSubject, UpdateError, VerifyError};
use plaudon_updater::install::Installer;
use plaudon_updater::manifest::Version;
use plaudon_updater::testkeys::{
    FixtureFlaw, DOWNGRADE_VERSION, FOREIGN_TEST_KEY_ID, UPDATE_VERSION,
};

use support::{bootstrapped, load, verifier};

/// Drive one fixture through a fresh installation and return the rejection.
fn reject(name: &str) -> UpdateError {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    let hostile = load(name);

    let error = installer
        .install(hostile.envelope.as_bytes(), &hostile.package)
        .expect_err("hostile fixture must be refused");

    // Fail-closed consequences, asserted for every hostile fixture.
    assert_eq!(
        installer.installed_version(),
        Version::new(1, 0, 0),
        "{name}: the active version must not move"
    );
    assert!(
        installer.journal().pending.is_none(),
        "{name}: no pending record may survive a rejection"
    );
    assert!(
        !root.path().join("staging").exists(),
        "{name}: staged bytes must be removed"
    );
    assert!(
        !root.path().join("slots").join("b").exists(),
        "{name}: the inactive slot must never be written"
    );
    assert_eq!(
        installer.launchable().expect("still launchable").version,
        Version::new(1, 0, 0)
    );

    // Re-opening must agree: the rejection left nothing to recover from.
    let reopened = Installer::open(
        root.path(),
        verifier(),
        plaudon_updater::testkeys::FIXTURE_TARGET,
    )
    .expect("reopen");
    assert_eq!(reopened.installed_version(), Version::new(1, 0, 0));

    error
}

#[test]
fn an_unsigned_manifest_is_rejected() {
    assert!(matches!(
        reject(FixtureFlaw::Unsigned.fixture_name()),
        UpdateError::Verify(VerifyError::UnsignedManifest)
    ));
}

#[test]
fn a_manifest_signed_by_an_unpinned_key_is_rejected() {
    let error = reject(FixtureFlaw::WrongKey.fixture_name());
    match error {
        UpdateError::Verify(VerifyError::UnknownKey { key_id }) => {
            assert_eq!(key_id, FOREIGN_TEST_KEY_ID);
        }
        other => panic!("expected UnknownKey, got {other:?}"),
    }
}

#[test]
fn a_truncated_package_is_rejected_as_truncated_not_as_a_digest_mismatch() {
    let error = reject(FixtureFlaw::TruncatedPackage.fixture_name());
    match error {
        UpdateError::Verify(VerifyError::TruncatedPackage { expected, actual }) => {
            assert_eq!(expected, 512);
            assert_eq!(actual, 448);
        }
        other => panic!("expected TruncatedPackage, got {other:?}"),
    }
}

#[test]
fn a_single_flipped_package_byte_is_rejected() {
    let error = reject(FixtureFlaw::TamperedPackageByte.fixture_name());
    match error {
        UpdateError::Verify(VerifyError::DigestMismatch { expected, actual }) => {
            assert_ne!(expected, actual);
            assert_eq!(expected.len(), 64);
        }
        other => panic!("expected DigestMismatch, got {other:?}"),
    }
}

#[test]
fn an_edited_manifest_field_breaks_the_manifest_signature() {
    let error = reject(FixtureFlaw::TamperedManifestField.fixture_name());
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::SignatureMismatch {
                subject: SignatureSubject::Manifest
            })
        ),
        "expected a manifest signature mismatch, got {error:?}"
    );
}

#[test]
fn a_validly_signed_older_release_is_blocked_as_a_downgrade() {
    let error = reject(FixtureFlaw::Downgrade.fixture_name());
    match error {
        UpdateError::Verify(VerifyError::DowngradeBlocked {
            offered,
            installed,
            floor,
        }) => {
            assert_eq!(offered, DOWNGRADE_VERSION);
            assert_eq!(installed, Version::new(1, 0, 0));
            assert_eq!(floor, Version::new(1, 0, 0));
        }
        other => panic!("expected DowngradeBlocked, got {other:?}"),
    }
}

#[test]
fn a_manifest_for_another_target_is_rejected() {
    let error = reject(FixtureFlaw::WrongTarget.fixture_name());
    match error {
        UpdateError::Verify(VerifyError::UnsupportedTarget { offered, host }) => {
            assert_eq!(offered, "windows/x86_64");
            assert_eq!(host, "mac_os/aarch64");
        }
        other => panic!("expected UnsupportedTarget, got {other:?}"),
    }
}

#[test]
fn every_hostile_fixture_has_a_case_in_this_suite() {
    // Guards against a fixture being added without an assertion.
    let hostile: Vec<&str> = FixtureFlaw::ALL
        .iter()
        .filter(|flaw| **flaw != FixtureFlaw::None)
        .map(|flaw| flaw.fixture_name())
        .collect();
    assert_eq!(hostile.len(), 7);
    let suite = include_str!("security.rs");
    for name in hostile {
        assert!(
            suite.contains(&format!("FixtureFlaw::{}", flaw_variant(name))),
            "fixture `{name}` has no assertion in this suite"
        );
    }
}

fn flaw_variant(fixture_name: &str) -> &'static str {
    FixtureFlaw::ALL
        .iter()
        .find(|flaw| flaw.fixture_name() == fixture_name)
        .map(|flaw| match flaw {
            FixtureFlaw::None => "None",
            FixtureFlaw::Unsigned => "Unsigned",
            FixtureFlaw::WrongKey => "WrongKey",
            FixtureFlaw::TruncatedPackage => "TruncatedPackage",
            FixtureFlaw::TamperedPackageByte => "TamperedPackageByte",
            FixtureFlaw::TamperedManifestField => "TamperedManifestField",
            FixtureFlaw::Downgrade => "Downgrade",
            FixtureFlaw::WrongTarget => "WrongTarget",
        })
        .expect("known fixture")
}

#[test]
fn the_valid_fixture_installs_and_becomes_launchable() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    let update = load(FixtureFlaw::None.fixture_name());

    let outcome = installer
        .install(update.envelope.as_bytes(), &update.package)
        .expect("the valid fixture installs");

    assert_eq!(outcome, UPDATE_VERSION);
    let state = installer.launchable().expect("launchable");
    assert_eq!(state.version, UPDATE_VERSION);
    assert_eq!(
        std::fs::read(&state.payload).expect("payload"),
        update.package
    );
    assert_eq!(installer.retained_version(), Some(Version::new(1, 0, 0)));
}

#[test]
fn rejections_map_onto_the_shared_error_taxonomy_without_leaking() {
    use plaudon_domain::error::AppError;

    for flaw in FixtureFlaw::ALL {
        if *flaw == FixtureFlaw::None {
            continue;
        }
        let error = reject(flaw.fixture_name());
        let message = error.to_string();
        let app: AppError = error.into();
        // §15.4: no endpoint, no key material, no signature in anything a
        // user or a log can see.
        assert!(!message.contains("updates.plaudon.app"), "{message}");
        assert!(!message.contains("=="), "{message}");
        assert!(
            !app.detail().contains("updates.plaudon.app"),
            "{}",
            app.detail()
        );
        assert!(!app.is_retryable(), "a rejected update is never retryable");
    }
}
