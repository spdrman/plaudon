//! The manifest wire contract: one spelling per value, and every structural
//! deviation rejected by name (SPEC v1.3 §2.4, §16.5, §18.5).
//!
//! §16.5 makes the signed region byte-exact, so every parsing rule here is
//! load-bearing: a value with two accepted spellings is a value an attacker
//! can re-spell between the bytes we verify and the bytes we act on. The
//! suite therefore drives the boundaries — empty, one component short, one
//! component long, one byte short, one case wrong — rather than the happy
//! shape only.

mod support;

use plaudon_updater::error::VerifyError;
use plaudon_updater::manifest::{
    Arch, DetachedSignature, Platform, Sha256Digest, SignedManifestEnvelope, Target,
    UpdateManifest, Version, DIGEST_LEN, MANIFEST_SCHEMA_VERSION, SIGNATURE_LEN,
};
use plaudon_updater::testkeys::{
    package_bytes, test_signing_key, FIXTURE_TARGET, FOREIGN_TARGET, TEST_KEY_ID,
};

use support::{embed, envelope_with, manifest_for, sign_base64, signed_envelope};

const V1: Version = Version::new(1, 0, 0);

fn valid_manifest() -> UpdateManifest {
    let package = package_bytes(V1);
    manifest_for(V1, V1, FIXTURE_TARGET, &package, &test_signing_key())
}

// ---- Version ------------------------------------------------------------

#[test]
fn versions_parse_only_in_their_canonical_spelling() {
    for good in ["0.0.0", "1.2.3", "0.10.0", "4294967295.0.1"] {
        let parsed: Version = good.parse().expect("canonical version parses");
        assert_eq!(parsed.to_string(), good);
    }
    for bad in [
        "",               // empty
        "1",              // one component
        "1.2",            // two components
        "1.2.3.4",        // four components
        "1.2.",           // empty trailing component
        ".2.3",           // empty leading component
        "01.2.3",         // leading zero, two spellings for one version
        "1.02.3",         // leading zero in the middle
        "1.2.03",         // leading zero at the end
        "1.2.3-beta",     // pre-release
        "1.2.3+build",    // build metadata
        "v1.2.3",         // prefixed
        "1.2.x",          // non-digit
        "1.2.-3",         // sign
        "1.2.٣",          // non-ASCII digit
        "4294967296.0.0", // overflows u32
        " 1.2.3",         // leading space
    ] {
        assert!(
            bad.parse::<Version>().is_err(),
            "`{bad}` must not parse as a version"
        );
    }
}

#[test]
fn versions_order_by_component_not_by_text() {
    assert!(Version::new(0, 10, 0) > Version::new(0, 9, 9));
    assert!(Version::new(1, 0, 0) > Version::new(0, 999, 999));
    assert!(Version::new(1, 2, 4) > Version::new(1, 2, 3));
    assert_eq!(Version::new(1, 2, 3), Version::new(1, 2, 3));
    assert_eq!(
        Version::new(1, 2, 3).max(Version::new(1, 3, 0)),
        Version::new(1, 3, 0)
    );
}

#[test]
fn versions_round_trip_through_json_and_reject_foreign_shapes() {
    let encoded = serde_json::to_string(&Version::new(1, 2, 3)).expect("encodes");
    assert_eq!(encoded, "\"1.2.3\"");
    assert_eq!(
        serde_json::from_str::<Version>(&encoded).expect("decodes"),
        Version::new(1, 2, 3)
    );
    // A number is not a version, and neither is a malformed string.
    assert!(serde_json::from_str::<Version>("123").is_err());
    assert!(serde_json::from_str::<Version>("\"1.2\"").is_err());
    assert!(serde_json::from_str::<Version>("null").is_err());
}

// ---- Sha256Digest -------------------------------------------------------

#[test]
fn digests_accept_only_64_lowercase_hex_characters() {
    let digest: Sha256Digest = "ab".repeat(32).parse().expect("canonical digest parses");
    assert_eq!(digest.to_hex(), "ab".repeat(32));
    assert_eq!(digest.to_string(), "ab".repeat(32));
    assert_eq!(digest.as_bytes().len(), DIGEST_LEN);

    for bad in [
        String::new(),                    // empty
        "ab".repeat(31),                  // one byte short
        format!("{}a", "ab".repeat(32)),  // one nibble long
        "AB".repeat(32),                  // uppercase
        "zz".repeat(32),                  // lowercase but not hex
        format!("{}  ", "ab".repeat(31)), // padded with spaces
    ] {
        assert!(
            bad.parse::<Sha256Digest>().is_err(),
            "`{bad}` must not parse as a digest"
        );
    }
}

#[test]
fn digests_round_trip_through_json() {
    let digest = Sha256Digest::from_bytes([0u8; DIGEST_LEN]);
    let encoded = serde_json::to_string(&digest).expect("encodes");
    assert_eq!(encoded, format!("\"{}\"", "00".repeat(32)));
    assert_eq!(
        serde_json::from_str::<Sha256Digest>(&encoded).expect("decodes"),
        digest
    );
    assert!(serde_json::from_str::<Sha256Digest>("\"nope\"").is_err());
    assert!(serde_json::from_str::<Sha256Digest>("17").is_err());
}

// ---- DetachedSignature --------------------------------------------------

#[test]
fn signatures_accept_only_base64_of_exactly_64_bytes() {
    let signature = DetachedSignature::from_bytes([3u8; SIGNATURE_LEN]);
    let encoded = signature.to_base64();
    assert_eq!(
        encoded.parse::<DetachedSignature>().expect("round trips"),
        signature
    );
    assert_eq!(signature.as_bytes(), &[3u8; SIGNATURE_LEN]);

    for bad in [
        String::new(),                 // empty
        "!!!not base64!!!".to_owned(), // not base64 at all
        encoded[..40].to_owned(),      // well-formed base64, 30 bytes
        // Well-formed base64 of 65 bytes: one byte too many.
        format!("{}=", "A".repeat(87)),
        // Well-formed base64 of 66 bytes, unpadded.
        "A".repeat(88),
    ] {
        assert!(
            bad.parse::<DetachedSignature>().is_err(),
            "`{bad}` must not parse as a signature"
        );
    }
}

#[test]
fn signatures_round_trip_through_json_without_printing_bytes() {
    let signature = DetachedSignature::from_bytes([9u8; SIGNATURE_LEN]);
    let encoded = serde_json::to_string(&signature).expect("encodes");
    assert_eq!(encoded, format!("\"{}\"", signature.to_base64()));
    assert_eq!(
        serde_json::from_str::<DetachedSignature>(&encoded).expect("decodes"),
        signature
    );
    assert!(serde_json::from_str::<DetachedSignature>("\"short\"").is_err());
    assert_eq!(
        format!("{signature:?}"),
        "DetachedSignature(<64 bytes>)",
        "a signature must never render its bytes"
    );
}

// ---- Target -------------------------------------------------------------

#[test]
fn every_platform_and_arch_has_one_stable_wire_spelling() {
    assert_eq!(Platform::MacOs.as_str(), "mac_os");
    assert_eq!(Platform::Windows.as_str(), "windows");
    assert_eq!(Platform::Linux.as_str(), "linux");
    assert_eq!(Arch::Aarch64.as_str(), "aarch64");
    assert_eq!(Arch::X86_64.as_str(), "x86_64");

    for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
        for arch in [Arch::Aarch64, Arch::X86_64] {
            let target = Target::new(platform, arch);
            assert_eq!(
                target.to_string(),
                format!("{}/{}", platform.as_str(), arch.as_str())
            );
            let encoded = serde_json::to_string(&target).expect("encodes");
            assert_eq!(
                serde_json::from_str::<Target>(&encoded).expect("decodes"),
                target
            );
        }
    }

    // The host pair is one of the representable cells, never something else.
    let host = Target::host();
    assert_eq!(host.platform, Platform::host());
    assert_eq!(host.arch, Arch::host());
    assert!(serde_json::from_str::<Platform>("\"solaris\"").is_err());
    assert!(serde_json::from_str::<Arch>("\"riscv64\"").is_err());
}

#[test]
fn a_manifest_accepts_its_own_target_and_rejects_every_other() {
    let manifest = valid_manifest();
    assert_eq!(manifest.target, FIXTURE_TARGET);
    manifest
        .check_target(FIXTURE_TARGET)
        .expect("the matching target is accepted");

    let error = manifest
        .check_target(FOREIGN_TARGET)
        .expect_err("a foreign target is refused");
    assert_eq!(
        error,
        VerifyError::UnsupportedTarget {
            offered: FIXTURE_TARGET.to_string(),
            host: FOREIGN_TARGET.to_string(),
        }
    );

    // Same platform, other architecture: still foreign.
    let other_arch = Target::new(FIXTURE_TARGET.platform, Arch::X86_64);
    assert!(manifest.check_target(other_arch).is_err());
    // Same architecture, other platform: still foreign.
    let other_platform = Target::new(Platform::Linux, FIXTURE_TARGET.arch);
    assert!(manifest.check_target(other_platform).is_err());
}

// ---- Structural invariants ----------------------------------------------

#[test]
fn a_well_formed_manifest_passes_every_structural_check() {
    valid_manifest()
        .check_well_formed()
        .expect("the reference manifest is well formed");

    // Boundary: version exactly equal to its own floor is allowed.
    let mut at_floor = valid_manifest();
    at_floor.minimum_supported_version = at_floor.version;
    at_floor
        .check_well_formed()
        .expect("a release may sit exactly on its own floor");

    // Boundary: a single byte of package is enough.
    let mut minimal = valid_manifest();
    minimal.package_size = 1;
    minimal
        .check_well_formed()
        .expect("a one-byte package is representable");
}

#[test]
fn a_manifest_below_its_own_floor_is_malformed() {
    let mut manifest = valid_manifest();
    manifest.version = Version::new(1, 0, 0);
    manifest.minimum_supported_version = Version::new(1, 0, 1);
    let error = manifest
        .check_well_formed()
        .expect_err("self-contradictory");
    assert!(
        matches!(&error, VerifyError::MalformedManifest(detail)
            if detail.contains("below its own minimum_supported_version")),
        "got {error:?}"
    );
    assert_eq!(
        error.code(),
        plaudon_domain::error::AppErrorCode::MalformedOutput
    );
}

#[test]
fn an_empty_package_url_is_malformed() {
    let mut manifest = valid_manifest();
    manifest.package_url = String::new();
    let error = manifest.check_well_formed().expect_err("empty url");
    assert!(
        matches!(&error, VerifyError::MalformedManifest(detail) if detail.contains("package_url")),
        "got {error:?}"
    );
}

#[test]
fn a_zero_length_package_is_malformed() {
    let mut manifest = valid_manifest();
    manifest.package_size = 0;
    let error = manifest.check_well_formed().expect_err("zero size");
    assert!(
        matches!(&error, VerifyError::MalformedManifest(detail) if detail.contains("package_size")),
        "got {error:?}"
    );
}

// ---- Envelope parsing ---------------------------------------------------

#[test]
fn an_envelope_round_trips_its_exact_signed_region() {
    let manifest = valid_manifest();
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let envelope = SignedManifestEnvelope::parse(text.as_bytes()).expect("parses");

    assert_eq!(envelope.schema_version, MANIFEST_SCHEMA_VERSION);
    assert_eq!(envelope.signing_key_id, TEST_KEY_ID);
    assert_eq!(envelope.signed_bytes(), embed(&manifest).as_bytes());
    assert_eq!(envelope.document().expect("document"), manifest);
    assert!(envelope.signature().is_ok());
}

#[test]
fn the_signed_region_keeps_insignificant_whitespace() {
    // Two JSON documents that parse equal but differ byte-for-byte must have
    // different signed regions, otherwise the signature covers less than the
    // bytes we act on (§16.5).
    let spaced = br#"{"schema_version":1,"manifest":{"a":  1},"signing_key_id":"k"}"#;
    let tight = br#"{"schema_version":1,"manifest":{"a":1},"signing_key_id":"k"}"#;
    let spaced = SignedManifestEnvelope::parse(spaced).expect("parses");
    let tight = SignedManifestEnvelope::parse(tight).expect("parses");
    assert_eq!(spaced.signed_bytes(), br#"{"a":  1}"#);
    assert_eq!(tight.signed_bytes(), br#"{"a":1}"#);
    assert_ne!(spaced.signed_bytes(), tight.signed_bytes());
}

#[test]
fn bytes_that_are_not_an_envelope_are_malformed() {
    for bad in [
        b"".as_slice(),                                                             // empty
        b"not json".as_slice(),                                                     // not JSON
        b"[]".as_slice(),                                      // JSON, wrong shape
        b"{}".as_slice(),                                      // no members
        br#"{"schema_version":1}"#.as_slice(),                 // no manifest
        br#"{"manifest":{},"signing_key_id":"k"}"#.as_slice(), // no schema version
        br#"{"schema_version":1,"manifest":{}}"#.as_slice(),   // no key id
        br#"{"schema_version":"1","manifest":{},"signing_key_id":"k"}"#.as_slice(), // wrong type
        br#"{"schema_version":1,"manifest":{},"signing_key_id":"k","extra":true}"#.as_slice(),
        br#"{"schema_version":1,"manifest":{},"signing_key_id":"k"#.as_slice(), // truncated
    ] {
        let error = SignedManifestEnvelope::parse(bad).expect_err("must be refused");
        assert!(
            matches!(error, VerifyError::MalformedManifest(_)),
            "{} gave {error:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn an_unknown_envelope_schema_version_is_refused_by_number() {
    let manifest = valid_manifest();
    let embedded = embed(&manifest);
    let signature = sign_base64(&test_signing_key(), embedded.as_bytes());
    for version in [0, 2, u32::MAX] {
        let text = envelope_with(version, &embedded, TEST_KEY_ID, Some(&signature));
        let error = SignedManifestEnvelope::parse(text.as_bytes()).expect_err("must be refused");
        assert!(
            matches!(&error, VerifyError::MalformedManifest(detail)
                if detail.contains(&format!("schema_version {version}"))),
            "got {error:?}"
        );
    }
    // The supported version is the only one that passes.
    let text = envelope_with(
        MANIFEST_SCHEMA_VERSION,
        &embedded,
        TEST_KEY_ID,
        Some(&signature),
    );
    assert!(SignedManifestEnvelope::parse(text.as_bytes()).is_ok());
}

#[test]
fn an_empty_signing_key_id_is_refused_before_any_lookup() {
    let manifest = valid_manifest();
    let embedded = embed(&manifest);
    let signature = sign_base64(&test_signing_key(), embedded.as_bytes());
    let text = envelope_with(MANIFEST_SCHEMA_VERSION, &embedded, "", Some(&signature));
    let error = SignedManifestEnvelope::parse(text.as_bytes()).expect_err("must be refused");
    assert!(
        matches!(&error, VerifyError::MalformedManifest(detail)
            if detail.contains("signing_key_id is empty")),
        "got {error:?}"
    );
}

#[test]
fn a_missing_empty_or_unparsable_signature_is_reported_distinctly() {
    let manifest = valid_manifest();
    let embedded = embed(&manifest);

    let absent = envelope_with(MANIFEST_SCHEMA_VERSION, &embedded, TEST_KEY_ID, None);
    let empty = envelope_with(MANIFEST_SCHEMA_VERSION, &embedded, TEST_KEY_ID, Some(""));
    for text in [absent, empty] {
        let envelope = SignedManifestEnvelope::parse(text.as_bytes()).expect("parses");
        assert_eq!(
            envelope.signature().expect_err("unsigned"),
            VerifyError::UnsignedManifest
        );
    }

    // Present, non-empty, but not a 64-byte base64 signature: that is a
    // malformed manifest, not an unsigned one.
    for junk in ["not-base64!!", "AAAA", &"A".repeat(88)] {
        let text = envelope_with(MANIFEST_SCHEMA_VERSION, &embedded, TEST_KEY_ID, Some(junk));
        let envelope = SignedManifestEnvelope::parse(text.as_bytes()).expect("parses");
        let error = envelope.signature().expect_err("not a signature");
        assert!(
            matches!(&error, VerifyError::MalformedManifest(detail)
                if detail.contains("manifest_signature")),
            "`{junk}` gave {error:?}"
        );
    }
}

#[test]
fn an_inner_document_missing_a_required_field_is_malformed() {
    let manifest = valid_manifest();
    let full = embed(&manifest);
    let signature = sign_base64(&test_signing_key(), full.as_bytes());

    for missing in [
        "version",
        "minimum_supported_version",
        "target",
        "package_url",
        "package_sha256",
        "package_size",
        "package_signature",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&full).expect("inner json");
        value
            .as_object_mut()
            .expect("object")
            .remove(missing)
            .expect("field was present");
        let text = envelope_with(
            MANIFEST_SCHEMA_VERSION,
            &value.to_string(),
            TEST_KEY_ID,
            Some(&signature),
        );
        let envelope = SignedManifestEnvelope::parse(text.as_bytes()).expect("envelope parses");
        let error = envelope
            .document()
            .expect_err("a document missing a required field is malformed");
        assert!(
            matches!(&error, VerifyError::MalformedManifest(detail) if detail.contains(missing)),
            "dropping `{missing}` gave {error:?}"
        );
    }
}

#[test]
fn an_inner_document_with_an_unknown_field_is_malformed() {
    let manifest = valid_manifest();
    let mut value: serde_json::Value = serde_json::from_str(&embed(&manifest)).expect("inner json");
    value
        .as_object_mut()
        .expect("object")
        .insert("rollout_percentage".to_owned(), serde_json::json!(5));
    let text = envelope_with(
        MANIFEST_SCHEMA_VERSION,
        &value.to_string(),
        TEST_KEY_ID,
        Some(&"A".repeat(88)),
    );
    let envelope = SignedManifestEnvelope::parse(text.as_bytes()).expect("envelope parses");
    let error = envelope.document().expect_err("unknown field");
    assert!(
        matches!(&error, VerifyError::MalformedManifest(detail)
            if detail.contains("rollout_percentage")),
        "got {error:?}"
    );
}

#[test]
fn an_inner_document_that_is_not_an_object_is_malformed() {
    for inner in ["null", "42", "[]", "\"a string\""] {
        let text = envelope_with(MANIFEST_SCHEMA_VERSION, inner, TEST_KEY_ID, Some("sig"));
        let envelope = SignedManifestEnvelope::parse(text.as_bytes()).expect("envelope parses");
        assert!(
            matches!(
                envelope.document().expect_err("not a manifest"),
                VerifyError::MalformedManifest(_)
            ),
            "`{inner}` must not parse as a manifest document"
        );
    }
}

#[test]
fn a_structurally_valid_but_self_contradictory_document_is_malformed() {
    // `document()` runs `check_well_formed`, so a signed manifest that
    // contradicts itself never reaches the installer.
    let mut manifest = valid_manifest();
    manifest.package_size = 0;
    let text = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let envelope = SignedManifestEnvelope::parse(text.as_bytes()).expect("envelope parses");
    assert!(
        matches!(&envelope.document().expect_err("contradictory"),
            VerifyError::MalformedManifest(detail) if detail.contains("package_size")),
        "a zero-size package must not survive document()"
    );
}
