//! Config drift tripwire (SPEC v1.3 §16.5, §16.1 "fake download redirects").
//!
//! The update endpoint and the pinned signing key live in exactly one place,
//! `crates/updater/src/config.rs`. This suite asserts their verbatim values,
//! so redirecting the update channel or swapping a key cannot reach `main`
//! without the diff showing a changed test expectation next to the changed
//! constant. Reviewers are expected to treat a change here as a security
//! review, not a chore.

use plaudon_updater::config::{
    EMBEDDED_TRUSTED_KEYS, RELEASE_SIGNING_KEY_BASE64, RELEASE_SIGNING_KEY_ID,
    UPDATE_MANIFEST_ENDPOINT,
};
use plaudon_updater::verify::TrustedKeys;

#[test]
fn the_update_endpoint_is_exactly_the_reviewed_value() {
    assert_eq!(
        UPDATE_MANIFEST_ENDPOINT,
        "https://updates.plaudon.app/stable/update-manifest.v1.json"
    );
}

#[test]
fn the_pinned_release_key_is_exactly_the_reviewed_value() {
    assert_eq!(RELEASE_SIGNING_KEY_ID, "plaudon-release-2026a");
    assert_eq!(
        RELEASE_SIGNING_KEY_BASE64,
        "55tblbXehVERBNYhjyBO9M9Ta3dV+zr6cv7NWwaMLEg="
    );
}

#[test]
fn the_trust_store_holds_exactly_the_pinned_release_key() {
    assert_eq!(EMBEDDED_TRUSTED_KEYS.len(), 1);
    assert_eq!(EMBEDDED_TRUSTED_KEYS[0].id, RELEASE_SIGNING_KEY_ID);
    assert_eq!(
        EMBEDDED_TRUSTED_KEYS[0].public_key_base64,
        RELEASE_SIGNING_KEY_BASE64
    );

    let keys = TrustedKeys::embedded();
    assert_eq!(keys.key_ids(), vec![RELEASE_SIGNING_KEY_ID]);
    assert!(keys.get(RELEASE_SIGNING_KEY_ID).is_ok());
}

#[test]
fn the_endpoint_is_https_and_carries_no_credential() {
    assert!(UPDATE_MANIFEST_ENDPOINT.starts_with("https://"));
    assert!(
        !UPDATE_MANIFEST_ENDPOINT.contains('@'),
        "an update URL must never embed credentials"
    );
    assert!(
        !UPDATE_MANIFEST_ENDPOINT.contains('?'),
        "an update URL must not carry query parameters that could identify a user"
    );
}

#[test]
fn no_other_source_file_hard_codes_the_endpoint_or_the_key() {
    // One place, enforced. `config.rs` defines them; the drift test and the
    // doc comment in `policy.rs` may mention the host, nothing else may.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&src).expect("src dir") {
        let path = entry.expect("entry").path();
        if path.file_name().is_some_and(|n| n == "config.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read source");
        assert!(
            !text.contains(RELEASE_SIGNING_KEY_BASE64),
            "{} duplicates the pinned key",
            path.display()
        );
        assert!(
            !text.contains(UPDATE_MANIFEST_ENDPOINT),
            "{} duplicates the update endpoint",
            path.display()
        );
    }
}
