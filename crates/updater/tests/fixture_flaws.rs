//! The hostile-fixture generator produces the deviation it advertises
//! (SPEC v1.3 §18.5).
//!
//! Every suite in this crate trusts [`plaudon_updater::testkeys`] to build an
//! artifact that is wrong in exactly one named way. If the generator silently
//! produced a *valid* artifact for a flaw, every negative test would pass
//! vacuously, so each deviation is asserted here against the real verifier.

use plaudon_updater::error::{SignatureSubject, VerifyError};
use plaudon_updater::manifest::{SignedManifestEnvelope, Version};
use plaudon_updater::testkeys::{
    build_artifact, package_bytes, package_digest, test_trusted_keys, FixtureFlaw, FIXTURE_TARGET,
    FOREIGN_TARGET, FOREIGN_TEST_KEY_ID, TEST_KEY_ID,
};
use plaudon_updater::verify::{sha256, Ed25519Verifier, UpdateVerifier};

const V2: Version = Version::new(1, 1, 0);
const FLOOR: Version = Version::new(1, 0, 0);

fn verifier() -> Ed25519Verifier {
    Ed25519Verifier::new(test_trusted_keys())
}

#[test]
fn package_bytes_are_deterministic_and_version_derived() {
    assert_eq!(package_bytes(V2).len(), 512);
    assert_eq!(package_bytes(V2), package_bytes(V2));
    assert_ne!(package_bytes(V2), package_bytes(FLOOR));
    assert_eq!(package_digest(V2), sha256(&package_bytes(V2)));
    assert!(
        package_bytes(V2).starts_with(b"plaudon-test-only application payload v1.1.0\n"),
        "the payload names the release it belongs to"
    );
}

#[test]
fn every_flaw_has_its_own_name_and_the_set_is_exhaustive() {
    assert_eq!(FixtureFlaw::ALL.len(), 8);
    let mut names: Vec<&str> = FixtureFlaw::ALL
        .iter()
        .map(|flaw| flaw.fixture_name())
        .collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), FixtureFlaw::ALL.len());
    assert_eq!(FixtureFlaw::None.fixture_name(), "valid");
}

#[test]
fn the_clean_fixture_is_the_only_one_that_verifies_end_to_end() {
    for flaw in FixtureFlaw::ALL {
        let artifact = build_artifact(*flaw, V2, FLOOR);
        let outcome = verifier()
            .verify_manifest(artifact.envelope.as_bytes())
            .and_then(|verified| {
                verified.document().check_target(FIXTURE_TARGET)?;
                verifier().verify_package(&verified, &artifact.package)
            });
        // The downgrade fixture is cryptographically perfect; it is the
        // version policy, not verification, that refuses it.
        let sound = matches!(flaw, FixtureFlaw::None | FixtureFlaw::Downgrade);
        assert_eq!(
            outcome.is_ok(),
            sound,
            "{} verified as {outcome:?}",
            flaw.fixture_name()
        );
    }
}

#[test]
fn the_unsigned_fixture_carries_no_signature_member() {
    let artifact = build_artifact(FixtureFlaw::Unsigned, V2, FLOOR);
    assert!(!artifact.envelope.contains("manifest_signature"));
    let envelope =
        SignedManifestEnvelope::parse(artifact.envelope.as_bytes()).expect("still an envelope");
    assert_eq!(envelope.signing_key_id, TEST_KEY_ID);
    assert_eq!(
        envelope.signature().expect_err("unsigned"),
        VerifyError::UnsignedManifest
    );
}

#[test]
fn the_wrong_key_fixture_names_the_foreign_key_id() {
    let artifact = build_artifact(FixtureFlaw::WrongKey, V2, FLOOR);
    let envelope = SignedManifestEnvelope::parse(artifact.envelope.as_bytes()).expect("envelope");
    assert_eq!(envelope.signing_key_id, FOREIGN_TEST_KEY_ID);
    assert!(
        envelope.signature().is_ok(),
        "the fixture is signed, just not by a pinned key"
    );
    assert_eq!(
        verifier()
            .verify_manifest(artifact.envelope.as_bytes())
            .expect_err("unpinned"),
        VerifyError::UnknownKey {
            key_id: FOREIGN_TEST_KEY_ID.to_owned()
        }
    );
}

#[test]
fn the_truncated_fixture_is_short_by_exactly_sixty_four_bytes() {
    let artifact = build_artifact(FixtureFlaw::TruncatedPackage, V2, FLOOR);
    let honest = package_bytes(V2);
    assert_eq!(artifact.package.len(), honest.len() - 64);
    assert_eq!(artifact.package, honest[..honest.len() - 64]);
}

#[test]
fn the_tampered_byte_fixture_keeps_the_declared_length() {
    let artifact = build_artifact(FixtureFlaw::TamperedPackageByte, V2, FLOOR);
    let honest = package_bytes(V2);
    assert_eq!(artifact.package.len(), honest.len());
    let differing = artifact
        .package
        .iter()
        .zip(&honest)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(differing, 1, "exactly one byte must differ");
}

#[test]
fn the_wrong_target_fixture_targets_a_platform_we_do_not_build_for() {
    let artifact = build_artifact(FixtureFlaw::WrongTarget, V2, FLOOR);
    let verified = verifier()
        .verify_manifest(artifact.envelope.as_bytes())
        .expect("validly signed");
    assert_eq!(verified.document().target, FOREIGN_TARGET);
    assert!(verified.document().check_target(FIXTURE_TARGET).is_err());
}

#[test]
fn the_tampered_manifest_fixture_edits_the_digest_whatever_its_last_nibble_is() {
    // The generator flips the digest's last hex nibble, and the flip has two
    // cases: a trailing `0` becomes `1`, anything else becomes `0`. Pick one
    // release of each kind so neither case can rot unnoticed.
    let ends_with_zero = (0..512u32)
        .map(|patch| Version::new(1, 1, patch))
        .find(|version| package_digest(*version).to_hex().ends_with('0'))
        .expect("some release digest ends with 0");
    let ends_otherwise = (0..512u32)
        .map(|patch| Version::new(1, 1, patch))
        .find(|version| !package_digest(*version).to_hex().ends_with('0'))
        .expect("some release digest does not end with 0");
    assert_ne!(ends_with_zero, ends_otherwise);

    for (version, expected_last) in [(ends_with_zero, '1'), (ends_otherwise, '0')] {
        let honest = package_digest(version).to_hex();
        let artifact = build_artifact(FixtureFlaw::TamperedManifestField, version, FLOOR);
        let published = format!("{}{expected_last}", &honest[..honest.len() - 1]);

        assert!(
            artifact.envelope.contains(&published),
            "{version}: the published digest must be the flipped one"
        );
        assert!(
            !artifact.envelope.contains(&honest),
            "{version}: the honest digest must be gone"
        );
        // And the edit is what the signature catches.
        assert_eq!(
            verifier()
                .verify_manifest(artifact.envelope.as_bytes())
                .expect_err("the signature covers the unedited bytes"),
            VerifyError::SignatureMismatch {
                subject: SignatureSubject::Manifest
            }
        );
    }
}

#[test]
fn the_downgrade_fixture_is_validly_signed_but_older() {
    let old = Version::new(0, 9, 0);
    let artifact = build_artifact(FixtureFlaw::Downgrade, old, old);
    let verified = verifier()
        .verify_manifest(artifact.envelope.as_bytes())
        .expect("a downgrade is a correctly signed artifact");
    assert_eq!(verified.version(), old);
    verifier()
        .verify_package(&verified, &artifact.package)
        .expect("its package is intact too");
    // Only the version policy refuses it.
    assert!(plaudon_updater::verify::check_forward_progress(
        verified.document(),
        Version::new(1, 0, 0),
        Version::new(1, 0, 0),
    )
    .is_err());
}
