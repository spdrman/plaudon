//! The §16.5 verification path, driven from its failing side.
//!
//! Each rule in `verify.rs` gets both answers: a pinned key and an unpinned
//! one, a package one byte short and one byte long, a digest that matches and
//! one that does not, a package signature by the manifest's own key and one
//! by a foreign key, and the version policy at each side of its boundary.
//! The order of the checks is itself part of the contract (§18.5 wants the
//! *actionable* failure reported), so the suite pins which rejection wins
//! when two faults are present at once.

mod support;

use plaudon_updater::config::{TrustedKeyEntry, EMBEDDED_TRUSTED_KEYS, RELEASE_SIGNING_KEY_ID};
use plaudon_updater::error::{SignatureSubject, VerifyError};
use plaudon_updater::manifest::{Target, UpdateManifest, Version};
use plaudon_updater::testkeys::{
    foreign_signing_key, package_bytes, test_signing_key, test_trusted_keys, FixtureFlaw,
    FIXTURE_TARGET, FOREIGN_TARGET, FOREIGN_TEST_KEY_ID, FOREIGN_TEST_KEY_PUBLIC_BASE64,
    TEST_KEY_ID, TEST_KEY_PUBLIC_BASE64,
};
use plaudon_updater::verify::{
    check_forward_progress, check_version_policy, sha256, Ed25519Verifier, TrustedKeys,
    UpdateVerifier,
};

use support::{embed, envelope_with, load, manifest_for, sign_base64, signed_envelope};

const V1: Version = Version::new(1, 0, 0);
const V2: Version = Version::new(1, 1, 0);

fn pinned() -> Ed25519Verifier {
    Ed25519Verifier::new(test_trusted_keys())
}

/// A verifier that pins only the *foreign* test key.
fn foreign_pinned() -> Ed25519Verifier {
    Ed25519Verifier::new(
        TrustedKeys::from_entries(&[TrustedKeyEntry {
            id: FOREIGN_TEST_KEY_ID,
            public_key_base64: FOREIGN_TEST_KEY_PUBLIC_BASE64,
        }])
        .expect("the foreign test key is a valid ed25519 key"),
    )
}

fn reference_manifest(version: Version, package: &[u8]) -> UpdateManifest {
    manifest_for(version, V1, FIXTURE_TARGET, package, &test_signing_key())
}

// ---- The pinned key set -------------------------------------------------

#[test]
fn the_embedded_key_set_pins_exactly_the_reviewed_release_key() {
    let keys = TrustedKeys::embedded();
    assert_eq!(keys.len(), EMBEDDED_TRUSTED_KEYS.len());
    let pins_something = !keys.is_empty();
    assert!(pins_something, "the shipped build must pin a key");
    assert_eq!(keys.key_ids(), vec![RELEASE_SIGNING_KEY_ID]);
    assert!(keys.get(RELEASE_SIGNING_KEY_ID).is_ok());
    assert_eq!(
        keys.get(TEST_KEY_ID).expect_err("test keys are not pinned"),
        VerifyError::UnknownKey {
            key_id: TEST_KEY_ID.to_owned()
        }
    );
    // `Ed25519Verifier::embedded` must expose that same set.
    assert_eq!(Ed25519Verifier::embedded().keys().key_ids(), keys.key_ids());
}

#[test]
fn an_empty_key_set_rejects_every_artifact() {
    let empty = TrustedKeys::default();
    let pins_nothing = empty.is_empty();
    assert!(pins_nothing, "a default trust store trusts no one");
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.key_ids(), Vec::<&str>::new());
    assert_eq!(
        empty.get(TEST_KEY_ID).expect_err("nothing is pinned"),
        VerifyError::UnknownKey {
            key_id: TEST_KEY_ID.to_owned()
        }
    );

    let valid = load(FixtureFlaw::None.fixture_name());
    let error = Ed25519Verifier::new(empty)
        .verify_manifest(valid.envelope.as_bytes())
        .expect_err("an empty trust store trusts nothing");
    assert_eq!(
        error,
        VerifyError::UnknownKey {
            key_id: TEST_KEY_ID.to_owned()
        }
    );
}

#[test]
fn a_pinned_entry_that_is_not_an_ed25519_public_key_is_refused() {
    for (label, encoded) in [
        ("not base64", "not base64 at all!!"),
        ("too short", "AAAA"),
        ("too long", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        ("empty", ""),
        // 32 bytes of 0xff: the right length, correctly encoded, but not a
        // point on the curve, so decompression is what rejects it.
        (
            "not a curve point",
            "//////////////////////////////////////////w=",
        ),
    ] {
        let entries = [TrustedKeyEntry {
            id: "bogus-key",
            public_key_base64: encoded,
        }];
        let error = TrustedKeys::from_entries(&entries)
            .expect_err("a pin that is not a key must fail loudly");
        assert!(
            matches!(&error, VerifyError::MalformedManifest(detail)
                if detail.contains("bogus-key")),
            "{label} gave {error:?}"
        );
    }
    // A well-formed pin set builds, and keeps both entries addressable.
    let both = TrustedKeys::from_entries(&[
        TrustedKeyEntry {
            id: TEST_KEY_ID,
            public_key_base64: TEST_KEY_PUBLIC_BASE64,
        },
        TrustedKeyEntry {
            id: FOREIGN_TEST_KEY_ID,
            public_key_base64: FOREIGN_TEST_KEY_PUBLIC_BASE64,
        },
    ])
    .expect("two valid pins");
    assert_eq!(both.len(), 2);
    assert_eq!(
        both.key_ids(),
        vec![TEST_KEY_ID, FOREIGN_TEST_KEY_ID],
        "key ids are reported sorted"
    );
}

// ---- Manifest verification ----------------------------------------------

#[test]
fn a_manifest_signed_over_different_bytes_fails_as_a_signature_mismatch() {
    let package = package_bytes(V2);
    let manifest = reference_manifest(V2, &package);
    let embedded = embed(&manifest);
    // Sign a *different* document, then publish this one: the key id is
    // pinned, the signature parses, and only the ed25519 check can catch it.
    let decoy = reference_manifest(V1, &package_bytes(V1));
    let signature = sign_base64(&test_signing_key(), embed(&decoy).as_bytes());
    let text = envelope_with(1, &embedded, TEST_KEY_ID, Some(&signature));

    assert_eq!(
        pinned()
            .verify_manifest(text.as_bytes())
            .expect_err("the signature covers other bytes"),
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Manifest
        }
    );
}

#[test]
fn an_unsigned_manifest_is_reported_as_unsigned_even_when_the_key_is_unpinned() {
    let package = package_bytes(V2);
    let manifest = reference_manifest(V2, &package);
    let text = envelope_with(1, &embed(&manifest), "nobody-knows-this-key", None);
    assert_eq!(
        pinned()
            .verify_manifest(text.as_bytes())
            .expect_err("unsigned wins over unknown key"),
        VerifyError::UnsignedManifest,
        "§18.5 names the unsigned case explicitly, so it is the reported one"
    );
}

#[test]
fn a_signature_that_verifies_still_has_to_describe_a_well_formed_release() {
    // Signed correctly by the pinned key, but self-contradictory: the
    // document check runs *after* the signature check and still rejects.
    let package = package_bytes(V2);
    let mut manifest = reference_manifest(V2, &package);
    manifest.package_size = 0;
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let error = pinned()
        .verify_manifest(text.as_bytes())
        .expect_err("a zero-size package is malformed");
    assert!(
        matches!(&error, VerifyError::MalformedManifest(detail)
            if detail.contains("package_size")),
        "got {error:?}"
    );
}

#[test]
fn a_verified_manifest_carries_the_key_that_signed_it() {
    let package = package_bytes(V2);
    let manifest = reference_manifest(V2, &package);
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned().verify_manifest(text.as_bytes()).expect("verifies");

    assert_eq!(verified.signing_key_id(), TEST_KEY_ID);
    assert_eq!(verified.version(), V2);
    assert_eq!(verified.document(), &manifest);
    assert_eq!(verified.clone(), verified);
}

// ---- Package verification -----------------------------------------------

#[test]
fn a_package_one_byte_short_is_truncated_not_corrupt() {
    let package = package_bytes(V2);
    let manifest = reference_manifest(V2, &package);
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned().verify_manifest(text.as_bytes()).expect("verifies");

    let short = &package[..package.len() - 1];
    assert_eq!(
        pinned()
            .verify_package(&verified, short)
            .expect_err("one byte short"),
        VerifyError::TruncatedPackage {
            expected: package.len() as u64,
            actual: short.len() as u64,
        }
    );
    // The degenerate case: nothing arrived at all.
    assert_eq!(
        pinned()
            .verify_package(&verified, b"")
            .expect_err("nothing arrived"),
        VerifyError::TruncatedPackage {
            expected: package.len() as u64,
            actual: 0,
        }
    );
    // And the boundary: the exact length verifies.
    pinned()
        .verify_package(&verified, &package)
        .expect("the declared length verifies");
}

#[test]
fn a_package_one_byte_long_is_a_digest_mismatch() {
    let package = package_bytes(V2);
    let manifest = reference_manifest(V2, &package);
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned().verify_manifest(text.as_bytes()).expect("verifies");

    let mut long = package.clone();
    long.push(0);
    let error = pinned()
        .verify_package(&verified, &long)
        .expect_err("one byte too many");
    assert!(
        matches!(&error, VerifyError::DigestMismatch { expected, actual }
            if *expected == manifest.package_sha256.to_hex() && *actual == sha256(&long).to_hex()),
        "got {error:?}"
    );
}

#[test]
fn a_same_length_substitution_is_a_digest_mismatch() {
    let package = package_bytes(V2);
    let manifest = reference_manifest(V2, &package);
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned().verify_manifest(text.as_bytes()).expect("verifies");

    let mut hostile = package.clone();
    hostile[0] ^= 0x80;
    let error = pinned()
        .verify_package(&verified, &hostile)
        .expect_err("a flipped bit must be caught");
    assert!(
        matches!(&error, VerifyError::DigestMismatch { actual, .. }
            if *actual == sha256(&hostile).to_hex()),
        "got {error:?}"
    );
}

#[test]
fn a_package_signed_by_another_key_is_rejected_even_when_its_digest_matches() {
    // The manifest is signed by the pinned key, and names the honest digest
    // and length — but the package signature inside it is by the foreign
    // key. Only the package signature check can catch this.
    let package = package_bytes(V2);
    let manifest = manifest_for(V2, V1, FIXTURE_TARGET, &package, &foreign_signing_key());
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned()
        .verify_manifest(text.as_bytes())
        .expect("the manifest itself is validly signed");

    assert_eq!(
        pinned()
            .verify_package(&verified, &package)
            .expect_err("the package signature is foreign"),
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Package
        }
    );
}

#[test]
fn a_package_cannot_be_validated_against_a_key_the_verifier_does_not_pin() {
    // A manifest verified by one trust store, handed to a verifier pinning a
    // different key: the package step re-resolves the key id and refuses.
    let package = package_bytes(V2);
    let manifest = reference_manifest(V2, &package);
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned().verify_manifest(text.as_bytes()).expect("verifies");

    assert_eq!(
        foreign_pinned()
            .verify_package(&verified, &package)
            .expect_err("the signing key is not pinned here"),
        VerifyError::UnknownKey {
            key_id: TEST_KEY_ID.to_owned()
        }
    );
}

#[test]
fn a_one_byte_package_verifies_end_to_end() {
    // Smallest legal package: the length boundary must not be an off-by-one.
    let package = vec![0x42u8];
    let manifest = manifest_for(V2, V1, FIXTURE_TARGET, &package, &test_signing_key());
    assert_eq!(manifest.package_size, 1);
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned().verify_manifest(text.as_bytes()).expect("verifies");
    pinned()
        .verify_package(&verified, &package)
        .expect("a one-byte package is verifiable");
}

#[test]
fn sha256_matches_published_vectors() {
    assert_eq!(
        sha256(b"").to_hex(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc").to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(sha256(b"abc"), sha256(b"abd"));
}

// ---- Version policy -----------------------------------------------------

fn manifest_at(version: Version, floor: Version) -> UpdateManifest {
    manifest_for(version, floor, FIXTURE_TARGET, b"x", &test_signing_key())
}

#[test]
fn the_floor_is_inclusive_on_its_own_value() {
    let installed = Version::new(1, 2, 0);
    let floor = Version::new(1, 2, 0);
    // Exactly at the floor: allowed.
    check_version_policy(&manifest_at(floor, V1), installed, floor, false)
        .expect("a release standing on the floor is permitted");
    // One patch below the floor: refused.
    let below = Version::new(1, 1, 9);
    let error = check_version_policy(&manifest_at(below, V1), installed, floor, false)
        .expect_err("below the floor");
    assert_eq!(
        error,
        VerifyError::DowngradeBlocked {
            offered: below,
            installed,
            floor,
        }
    );
}

#[test]
fn a_manifest_raises_the_effective_floor_with_its_own_minimum() {
    let installed = Version::new(1, 0, 0);
    let floor = Version::new(1, 0, 0);
    // The manifest declares a floor above its own version is malformed, so
    // the interesting case is a manifest whose floor is above the
    // installation's: the stricter of the two wins.
    let offered = manifest_at(Version::new(1, 5, 0), Version::new(1, 4, 0));
    check_version_policy(&offered, installed, floor, false).expect("above both floors");

    let old = manifest_at(Version::new(1, 1, 0), Version::new(1, 1, 0));
    check_version_policy(&old, installed, floor, false).expect("above both floors");

    // A high installation floor blocks an otherwise acceptable release.
    let error = check_version_policy(&old, installed, Version::new(1, 2, 0), false)
        .expect_err("the installation floor is higher");
    assert_eq!(
        error,
        VerifyError::DowngradeBlocked {
            offered: Version::new(1, 1, 0),
            installed,
            floor: Version::new(1, 2, 0),
        }
    );
}

#[test]
fn only_the_retained_artifact_may_go_below_the_floor() {
    let installed = Version::new(1, 3, 0);
    let floor = Version::new(1, 2, 0);
    let old = manifest_at(Version::new(1, 1, 0), Version::new(1, 1, 0));
    assert!(check_version_policy(&old, installed, floor, false).is_err());
    check_version_policy(&old, installed, floor, true)
        .expect("the retained artifact is always allowed back");
}

#[test]
fn forward_progress_requires_a_strictly_newer_release() {
    let installed = Version::new(1, 2, 0);
    let floor = Version::new(1, 0, 0);

    // One patch newer: the smallest acceptable step.
    check_forward_progress(&manifest_at(Version::new(1, 2, 1), floor), installed, floor)
        .expect("one patch newer is forward progress");
    // Exactly the installed version: not progress.
    let error = check_forward_progress(&manifest_at(installed, floor), installed, floor)
        .expect_err("reinstalling the same version is not an update");
    assert_eq!(
        error,
        VerifyError::DowngradeBlocked {
            offered: installed,
            installed,
            floor,
        }
    );
    // One patch older but still above the floor: not progress either.
    let older = Version::new(1, 1, 9);
    assert!(
        check_forward_progress(&manifest_at(older, floor), installed, floor).is_err(),
        "an older release is never forward progress"
    );
    // Below the floor: refused by the floor rule before the progress rule.
    let error = check_forward_progress(
        &manifest_at(Version::new(0, 9, 0), Version::new(0, 9, 0)),
        installed,
        Version::new(1, 0, 0),
    )
    .expect_err("below the floor");
    assert_eq!(
        error,
        VerifyError::DowngradeBlocked {
            offered: Version::new(0, 9, 0),
            installed,
            floor: Version::new(1, 0, 0),
        }
    );
}

#[test]
fn the_fixture_target_and_the_foreign_target_never_coincide() {
    assert_ne!(FIXTURE_TARGET, FOREIGN_TARGET);
    let package = package_bytes(V2);
    let manifest = manifest_for(V2, V1, FOREIGN_TARGET, &package, &test_signing_key());
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let verified = pinned()
        .verify_manifest(text.as_bytes())
        .expect("a foreign-target manifest is still validly signed");
    // Signature verification says nothing about targeting: the target check
    // is a separate rule, and it is the one that refuses.
    assert_eq!(
        verified
            .document()
            .check_target(FIXTURE_TARGET)
            .expect_err("foreign target"),
        VerifyError::UnsupportedTarget {
            offered: FOREIGN_TARGET.to_string(),
            host: FIXTURE_TARGET.to_string(),
        }
    );
    assert!(Target::host().to_string().contains('/'));
}
