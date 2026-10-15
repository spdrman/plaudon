//! Spec 18.9 archive accounting: what a `.recording-archive` reader does with
//! a container that is damaged, forged or simply not ours.
//!
//! Every test here forges the container by hand rather than asking
//! [`plaudon_export::archive::ArchiveBuilder`] for one, because the interesting
//! inputs are exactly the ones our writer will never produce: a wrong magic, a
//! future format version, a manifest that disagrees with the frames behind it,
//! a deflate stream that expands past its declared length. The assertion is
//! always the typed rejection the UI shows, never a string match on a message.

use std::io::Write as _;
use std::path::Path;

use flate2::write::DeflateEncoder;
use flate2::Compression as FlateLevel;
use plaudon_export::archive::container;
use plaudon_export::archive::crypto::{
    EncryptingWriter, EncryptionHeader, KdfProfile, CIPHER_NAME,
};
use plaudon_export::archive::manifest::{
    host_os, ArchiveHeader, Compression, Manifest, ManifestEntry, ManifestTotals, ProtectionKind,
    FORMAT_VERSION, INTEGRITY_NOTE, MAGIC, MANIFEST_PATH, MANIFEST_SCHEMA_VERSION,
    MAX_HEADER_BYTES, MAX_MANIFEST_BYTES, NONPORTABLE_SETTINGS,
};
use plaudon_export::archive::reader::{self, EntrySink, VerifyingSink};
use plaudon_export::archive::DEFAULT_LIMITS;
use plaudon_export::bundle::AppStamp;
use plaudon_export::error::{ArchiveRejection, ExportError, ExportResult};
use sha2::{Digest as _, Sha256};

const PASSWORD: &str = "correct horse battery staple";

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = DeflateEncoder::new(&mut out, FlateLevel::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap();
    }
    out
}

/// A `store`d entry whose declared facts all agree with `payload`.
fn store_entry(path: &str, payload: &[u8]) -> ManifestEntry {
    ManifestEntry {
        path: path.to_owned(),
        kind: "file".to_owned(),
        sha256: sha_hex(payload),
        byte_length: payload.len() as u64,
        stored_length: payload.len() as u64,
        compression: Compression::Store,
        schema_version: None,
    }
}

fn manifest_of(entries: Vec<ManifestEntry>) -> Manifest {
    let integrity_digest = Manifest::compute_integrity_digest(&entries);
    Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
        format_version: FORMAT_VERSION,
        created_at: "1970-01-01T00:00:00Z".to_owned(),
        app: AppStamp::default(),
        source_os: host_os().to_owned(),
        totals: ManifestTotals {
            entry_count: entries.len() as u64,
            byte_length: entries
                .iter()
                .fold(0u64, |total, entry| total.wrapping_add(entry.byte_length)),
        },
        limits: DEFAULT_LIMITS,
        entries,
        integrity_digest,
        integrity_note: INTEGRITY_NOTE.to_owned(),
        nonportable_settings: NONPORTABLE_SETTINGS
            .iter()
            .map(|note| (*note).to_owned())
            .collect(),
    }
}

/// The manifest frame, stored verbatim so a test can corrupt the bytes without
/// fighting a compressor.
fn manifest_frame(manifest: &Manifest) -> (ManifestEntry, Vec<u8>) {
    let json = serde_json::to_vec(manifest).unwrap();
    let mut entry = store_entry(MANIFEST_PATH, &json);
    entry.schema_version = Some(MANIFEST_SCHEMA_VERSION.to_owned());
    (entry, json)
}

fn body(frames: &[(ManifestEntry, Vec<u8>)], terminate: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for (entry, payload) in frames {
        container::write_frame_header(&mut out, entry).unwrap();
        out.extend_from_slice(payload);
    }
    if terminate {
        container::write_terminator(&mut out).unwrap();
    }
    out
}

fn plaintext_header() -> ArchiveHeader {
    ArchiveHeader {
        format_version: FORMAT_VERSION,
        manifest_schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
        protection: "plaintext".to_owned(),
        encryption: None,
    }
}

fn encrypted_header() -> ArchiveHeader {
    ArchiveHeader {
        format_version: FORMAT_VERSION,
        manifest_schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
        protection: CIPHER_NAME.to_owned(),
        encryption: Some(EncryptionHeader::generate(KdfProfile::Test)),
    }
}

/// Assemble a preamble byte by byte, so the reader's structural checks can be
/// driven with values our writer refuses to emit.
fn raw_preamble(
    version: u16,
    protection: u8,
    reserved: u8,
    header_json: &[u8],
    declared_length: Option<u32>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.push(protection);
    out.push(reserved);
    let length = declared_length.unwrap_or(header_json.len() as u32);
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(header_json);
    out
}

fn write_plain(path: &Path, header: &ArchiveHeader, body_bytes: &[u8]) {
    let mut file = std::fs::File::create(path).unwrap();
    container::write_preamble(&mut file, ProtectionKind::Plaintext, header).unwrap();
    file.write_all(body_bytes).unwrap();
}

fn write_encrypted(path: &Path, header: &ArchiveHeader, password: &str, body_bytes: &[u8]) {
    let mut file = std::fs::File::create(path).unwrap();
    let header_bytes =
        container::write_preamble(&mut file, ProtectionKind::Encrypted, header).unwrap();
    let mut writer = EncryptingWriter::new(
        &mut file,
        header.encryption.as_ref().unwrap(),
        &header_bytes,
        password,
    )
    .unwrap();
    writer.write_all(body_bytes).unwrap();
    writer.finish().unwrap();
}

/// A plaintext archive carrying exactly `entries`, each with its real payload.
fn plain_archive(path: &Path, entries: Vec<(ManifestEntry, Vec<u8>)>) {
    let manifest = manifest_of(entries.iter().map(|(entry, _)| entry.clone()).collect());
    let mut frames = vec![manifest_frame(&manifest)];
    frames.extend(entries);
    write_plain(path, &plaintext_header(), &body(&frames, true));
}

fn rejection(error: &ExportError) -> &ArchiveRejection {
    match error {
        ExportError::ArchiveRejected(rejection) => rejection,
        other => panic!("expected an archive rejection, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Preamble
// ---------------------------------------------------------------------------

#[test]
fn a_file_that_is_not_an_archive_is_refused_by_its_magic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("holiday.zip");
    std::fs::write(&path, b"PK\x03\x04 this is a zip, not a backup").unwrap();

    let error = reader::inspect(&path, None, dir.path()).unwrap_err();
    assert!(matches!(rejection(&error), ArchiveRejection::NotAnArchive));
    assert_eq!(
        error.code(),
        plaudon_domain::error::AppErrorCode::ArchiveRejected
    );
}

#[test]
fn an_empty_file_is_refused_rather_than_read_as_an_empty_archive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.recording-archive");
    std::fs::write(&path, b"").unwrap();

    let error = reader::read_manifest(&path, None).unwrap_err();
    assert!(matches!(rejection(&error), ArchiveRejection::NotAnArchive));
}

#[test]
fn a_file_that_stops_inside_its_preamble_is_malformed() {
    let dir = tempfile::tempdir().unwrap();

    // Magic, then nothing: the fixed eight bytes never arrive.
    let truncated = dir.path().join("short.recording-archive");
    std::fs::write(&truncated, MAGIC).unwrap();
    assert!(matches!(
        rejection(&reader::read_manifest(&truncated, None).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));

    // A declared header length the file does not carry.
    let clipped = dir.path().join("clipped.recording-archive");
    let header = serde_json::to_vec(&plaintext_header()).unwrap();
    let mut bytes = raw_preamble(
        FORMAT_VERSION,
        0,
        0,
        &header,
        Some(header.len() as u32 + 64),
    );
    bytes.truncate(bytes.len() - 4);
    std::fs::write(&clipped, &bytes).unwrap();
    assert!(matches!(
        rejection(&reader::read_manifest(&clipped, None).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn a_future_container_version_is_refused_rather_than_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.recording-archive");
    let header = serde_json::to_vec(&plaintext_header()).unwrap();
    std::fs::write(&path, raw_preamble(FORMAT_VERSION + 1, 0, 0, &header, None)).unwrap();

    let error = reader::read_manifest(&path, None).unwrap_err();
    let ArchiveRejection::UnsupportedFormatVersion { found, supported } = rejection(&error) else {
        panic!("expected an unsupported format version, got {error:?}");
    };
    assert_eq!(*found, u32::from(FORMAT_VERSION) + 1);
    assert_eq!(*supported, u32::from(FORMAT_VERSION));
}

#[test]
fn the_authenticated_header_must_agree_with_the_fixed_preamble_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.recording-archive");
    let mut header = plaintext_header();
    header.format_version = FORMAT_VERSION + 7;
    let json = serde_json::to_vec(&header).unwrap();
    std::fs::write(&path, raw_preamble(FORMAT_VERSION, 0, 0, &json, None)).unwrap();

    assert!(matches!(
        rejection(&reader::read_manifest(&path, None).unwrap_err()),
        ArchiveRejection::UnsupportedFormatVersion { .. }
    ));
}

#[test]
fn unknown_structural_preamble_bytes_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let header = serde_json::to_vec(&plaintext_header()).unwrap();

    for (name, bytes) in [
        // Protection code 7 is neither plaintext (0) nor encrypted (1).
        (
            "protection",
            raw_preamble(FORMAT_VERSION, 7, 0, &header, None),
        ),
        // The reserved byte is the extension point; a non-zero value means the
        // writer used a feature this build does not implement.
        (
            "reserved",
            raw_preamble(FORMAT_VERSION, 0, 1, &header, None),
        ),
        // A zero-length header carries no authenticated metadata at all.
        (
            "zero header",
            raw_preamble(FORMAT_VERSION, 0, 0, &header, Some(0)),
        ),
        (
            "huge header",
            raw_preamble(FORMAT_VERSION, 0, 0, &header, Some(MAX_HEADER_BYTES + 1)),
        ),
        // Header bytes that are not archive metadata at all.
        (
            "header json",
            raw_preamble(FORMAT_VERSION, 0, 0, b"<xml/>", None),
        ),
    ] {
        let path = dir.path().join(format!("{name}.recording-archive"));
        std::fs::write(&path, bytes).unwrap();
        assert!(
            matches!(
                rejection(&reader::read_manifest(&path, None).unwrap_err()),
                ArchiveRejection::Malformed(_)
            ),
            "{name} should be malformed"
        );
    }
}

#[test]
fn protection_and_its_parameters_must_agree() {
    let dir = tempfile::tempdir().unwrap();

    // Claims encryption, carries no parameters.
    let bare = dir.path().join("bare.recording-archive");
    let plain = serde_json::to_vec(&plaintext_header()).unwrap();
    std::fs::write(&bare, raw_preamble(FORMAT_VERSION, 1, 0, &plain, None)).unwrap();
    assert!(matches!(
        rejection(&reader::read_manifest(&bare, Some(PASSWORD)).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));

    // Claims plaintext, carries key-derivation parameters.
    let dressed = dir.path().join("dressed.recording-archive");
    let encrypted = serde_json::to_vec(&encrypted_header()).unwrap();
    std::fs::write(
        &dressed,
        raw_preamble(FORMAT_VERSION, 0, 0, &encrypted, None),
    )
    .unwrap();
    assert!(matches!(
        rejection(&reader::read_manifest(&dressed, None).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

// ---------------------------------------------------------------------------
// Encryption
// ---------------------------------------------------------------------------

#[test]
fn an_encrypted_archive_asks_for_its_password_before_anything_else() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.recording-archive");
    let header = encrypted_header();
    let manifest = manifest_of(Vec::new());
    write_encrypted(
        &path,
        &header,
        PASSWORD,
        &body(&[manifest_frame(&manifest)], true),
    );

    assert!(matches!(
        reader::read_manifest(&path, None).unwrap_err(),
        ExportError::PasswordRequired
    ));
    let (read, encrypted) = reader::read_manifest(&path, Some(PASSWORD)).unwrap();
    assert!(encrypted);
    assert_eq!(read.totals.entry_count, 0);
}

#[test]
fn a_wrong_password_and_a_tampered_body_are_both_decryption_failures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sealed.recording-archive");
    let header = encrypted_header();
    let payload = br#"{"schema_version":"archive-source/1"}"#.to_vec();
    let entry = store_entry("records/a.json", &payload);
    let manifest = manifest_of(vec![entry.clone()]);
    let frames = vec![manifest_frame(&manifest), (entry, payload)];
    write_encrypted(&path, &header, PASSWORD, &body(&frames, true));

    assert!(matches!(
        reader::inspect(&path, Some("not the password"), dir.path()).unwrap_err(),
        ExportError::DecryptionFailed
    ));

    // Flipping one ciphertext bit is indistinguishable from a wrong password,
    // which is exactly what authenticated encryption promises.
    let tampered = dir.path().join("tampered.recording-archive");
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&tampered, &bytes).unwrap();
    assert!(matches!(
        reader::inspect(&tampered, Some(PASSWORD), dir.path()).unwrap_err(),
        ExportError::DecryptionFailed
    ));
}

#[test]
fn a_truncated_encrypted_body_is_reported_as_damage_not_as_a_bad_password() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sealed.recording-archive");
    let mut header = encrypted_header();
    header.encryption.as_mut().unwrap().chunk_bytes = 64;
    let payload = vec![b'x'; 400];
    let entry = store_entry("records/a.json", &payload);
    let manifest = manifest_of(vec![entry.clone()]);
    let frames = vec![manifest_frame(&manifest), (entry, payload)];
    write_encrypted(&path, &header, PASSWORD, &body(&frames, true));

    let truncated = dir.path().join("cut.recording-archive");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&truncated, &bytes[..bytes.len() - 90]).unwrap();

    let error = reader::inspect(&truncated, Some(PASSWORD), dir.path()).unwrap_err();
    assert!(matches!(rejection(&error), ArchiveRejection::Malformed(_)));
}

#[test]
fn abusive_key_derivation_parameters_are_refused_before_any_work_happens() {
    let dir = tempfile::tempdir().unwrap();

    for (name, mutate) in [
        (
            "time cost",
            Box::new(|header: &mut EncryptionHeader| header.kdf.t_cost = 0)
                as Box<dyn Fn(&mut EncryptionHeader)>,
        ),
        (
            "memory cost",
            Box::new(|header: &mut EncryptionHeader| header.kdf.m_cost_kib = u32::MAX),
        ),
        (
            "salt",
            Box::new(|header: &mut EncryptionHeader| header.kdf.salt_hex = "zz".to_owned()),
        ),
        (
            "nonce prefix",
            Box::new(|header: &mut EncryptionHeader| header.nonce_prefix_hex = "00".to_owned()),
        ),
        (
            "chunk size",
            Box::new(|header: &mut EncryptionHeader| header.chunk_bytes = 0),
        ),
        (
            "cipher",
            Box::new(|header: &mut EncryptionHeader| header.cipher = "rot13".to_owned()),
        ),
    ] {
        let mut header = encrypted_header();
        mutate(header.encryption.as_mut().unwrap());
        let json = serde_json::to_vec(&header).unwrap();
        let path = dir.path().join(format!("{name}.recording-archive"));
        let mut bytes = raw_preamble(FORMAT_VERSION, 1, 0, &json, None);
        bytes.extend_from_slice(&[0u8; 64]);
        std::fs::write(&path, bytes).unwrap();

        assert!(
            matches!(
                rejection(&reader::read_manifest(&path, Some(PASSWORD)).unwrap_err()),
                ArchiveRejection::KdfParametersRejected(_)
            ),
            "{name} should be refused"
        );
    }
}

// ---------------------------------------------------------------------------
// Manifest frame
// ---------------------------------------------------------------------------

#[test]
fn an_archive_without_a_manifest_frame_is_malformed() {
    let dir = tempfile::tempdir().unwrap();

    // Terminator straight after the preamble: no manifest at all.
    let empty = dir.path().join("no-manifest.recording-archive");
    write_plain(&empty, &plaintext_header(), &body(&[], true));
    assert!(matches!(
        rejection(&reader::read_manifest(&empty, None).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));

    // No terminator either: the body simply stops.
    let stops = dir.path().join("stops.recording-archive");
    write_plain(&stops, &plaintext_header(), &[]);
    assert!(matches!(
        rejection(&reader::read_manifest(&stops, None).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn the_first_frame_must_be_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wrong-first.recording-archive");
    let payload = b"{}".to_vec();
    let entry = store_entry("records/a.json", &payload);
    write_plain(&path, &plaintext_header(), &body(&[(entry, payload)], true));

    assert!(matches!(
        rejection(&reader::read_manifest(&path, None).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn a_manifest_frame_larger_than_the_ceiling_is_refused_before_it_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge-manifest.recording-archive");
    let manifest = manifest_of(Vec::new());
    let (mut entry, json) = manifest_frame(&manifest);
    entry.byte_length = MAX_MANIFEST_BYTES + 1;
    entry.stored_length = MAX_MANIFEST_BYTES + 1;
    write_plain(&path, &plaintext_header(), &body(&[(entry, json)], true));

    assert!(matches!(
        rejection(&reader::read_manifest(&path, None).unwrap_err()),
        ArchiveRejection::SizeCeilingExceeded(_)
    ));
}

#[test]
fn a_manifest_whose_bytes_do_not_match_its_frame_digest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad-digest.recording-archive");
    let manifest = manifest_of(Vec::new());
    let (mut entry, json) = manifest_frame(&manifest);
    entry.sha256 = "0".repeat(64);
    write_plain(&path, &plaintext_header(), &body(&[(entry, json)], true));

    let error = reader::read_manifest(&path, None).unwrap_err();
    let ArchiveRejection::DigestMismatch { path: entry_path } = rejection(&error) else {
        panic!("expected a digest mismatch, got {error:?}");
    };
    assert_eq!(entry_path, MANIFEST_PATH);
}

#[test]
fn a_manifest_that_is_not_archive_metadata_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-json.recording-archive");
    let json = b"this is not a manifest".to_vec();
    let mut entry = store_entry(MANIFEST_PATH, &json);
    entry.schema_version = Some(MANIFEST_SCHEMA_VERSION.to_owned());
    write_plain(&path, &plaintext_header(), &body(&[(entry, json)], true));

    assert!(matches!(
        rejection(&reader::read_manifest(&path, None).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn a_manifest_that_disagrees_with_itself_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let payload = b"{}".to_vec();
    let base = store_entry("records/a.json", &payload);

    let mut edited_entry = manifest_of(vec![base.clone()]);
    edited_entry.entries[0].sha256 = "9".repeat(64);

    let mut wrong_count = manifest_of(vec![base.clone()]);
    wrong_count.totals.entry_count = 4;

    let mut wrong_bytes = manifest_of(vec![base.clone()]);
    wrong_bytes.totals.byte_length = 999;

    let mut forged_digest = manifest_of(vec![base.clone()]);
    forged_digest.integrity_digest = "f".repeat(64);

    for (name, manifest) in [
        ("edited entry", edited_entry),
        ("entry count", wrong_count),
        ("byte total", wrong_bytes),
        ("integrity digest", forged_digest),
    ] {
        let path = dir.path().join(format!("{name}.recording-archive"));
        write_plain(
            &path,
            &plaintext_header(),
            &body(&[manifest_frame(&manifest)], true),
        );
        assert!(
            matches!(
                rejection(&reader::read_manifest(&path, None).unwrap_err()),
                ArchiveRejection::IntegrityDigestMismatch
            ),
            "{name} should fail the self-consistency check"
        );
    }
}

#[test]
fn a_manifest_declaring_an_unknown_schema_or_format_is_refused() {
    let dir = tempfile::tempdir().unwrap();

    let mut schema = manifest_of(Vec::new());
    schema.schema_version = "recording-archive-manifest/99".to_owned();
    let path = dir.path().join("schema.recording-archive");
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&schema)], true),
    );
    let error = reader::read_manifest(&path, None).unwrap_err();
    let ArchiveRejection::UnsupportedManifestSchema { found } = rejection(&error) else {
        panic!("expected an unsupported schema, got {error:?}");
    };
    assert_eq!(found, "recording-archive-manifest/99");

    let mut version = manifest_of(Vec::new());
    version.format_version = FORMAT_VERSION + 3;
    let path = dir.path().join("version.recording-archive");
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&version)], true),
    );
    assert!(matches!(
        rejection(&reader::read_manifest(&path, None).unwrap_err()),
        ArchiveRejection::UnsupportedFormatVersion { .. }
    ));
}

#[test]
fn declared_entry_lengths_that_overflow_are_refused_rather_than_wrapped() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = store_entry("records/a.json", b"{}");
    a.byte_length = u64::MAX;
    let mut b = store_entry("records/b.json", b"{}");
    b.byte_length = u64::MAX;
    let manifest = manifest_of(vec![a, b]);

    let path = dir.path().join("overflow.recording-archive");
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest)], true),
    );
    assert!(matches!(
        rejection(&reader::read_manifest(&path, None).unwrap_err()),
        ArchiveRejection::SizeCeilingExceeded(_)
    ));
}

// ---------------------------------------------------------------------------
// Entry policy, with no filesystem access at all
// ---------------------------------------------------------------------------

#[test]
fn an_entry_that_is_not_a_regular_file_is_never_materialized() {
    let mut entry = store_entry("records/a.json", b"{}");
    entry.kind = "symlink".to_owned();
    let manifest = manifest_of(vec![entry]);

    let error =
        reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err();
    let ArchiveRejection::UnsupportedEntryKind { path, kind } = rejection(&error) else {
        panic!("expected an unsupported entry kind, got {error:?}");
    };
    assert_eq!(path, "records/a.json");
    assert_eq!(kind, "symlink");
}

#[test]
fn an_entry_outside_the_archive_namespaces_is_refused() {
    for path in ["secrets/key.json", "manifest.json", "../etc/passwd"] {
        let manifest = manifest_of(vec![store_entry(path, b"{}")]);
        let error =
            reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err();
        let ArchiveRejection::PathRejected { path: refused, .. } = rejection(&error) else {
            panic!("expected a path rejection for {path}, got {error:?}");
        };
        assert_eq!(refused, path);
    }
}

#[test]
fn a_traversal_inside_a_namespace_cannot_escape_the_destination() {
    let manifest = manifest_of(vec![store_entry("records/../../etc/passwd", b"{}")]);
    let error =
        reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err();
    assert!(matches!(
        rejection(&error),
        ArchiveRejection::PathRejected { .. }
    ));
}

#[test]
fn an_entry_without_a_usable_digest_is_refused() {
    for digest in [String::new(), "a".repeat(63), "z".repeat(64)] {
        let mut entry = store_entry("records/a.json", b"{}");
        entry.sha256 = digest.clone();
        let manifest = manifest_of(vec![entry]);
        assert!(
            matches!(
                rejection(
                    &reader::validate_manifest_entries(&manifest, Path::new("/nonexistent"))
                        .unwrap_err()
                ),
                ArchiveRejection::Malformed(_)
            ),
            "digest {digest:?} should be refused"
        );
    }
}

#[test]
fn a_stored_entry_declaring_two_different_lengths_is_refused() {
    let mut entry = store_entry("records/a.json", b"{}");
    entry.stored_length = 99;
    let manifest = manifest_of(vec![entry]);

    assert!(matches!(
        rejection(
            &reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err()
        ),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn two_entries_colliding_under_case_folding_are_refused() {
    let manifest = manifest_of(vec![
        store_entry("records/Meeting.json", b"{}"),
        store_entry("records/meeting.json", b"{}"),
    ]);

    let error =
        reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err();
    assert!(matches!(
        rejection(&error),
        ArchiveRejection::DuplicatePath(_)
    ));
}

#[test]
fn size_ceilings_are_enforced_from_the_declared_manifest_alone() {
    // One entry larger than the per-entry ceiling.
    let mut huge = store_entry("records/a.json", b"{}");
    huge.byte_length = DEFAULT_LIMITS.max_entry_bytes + 1;
    huge.stored_length = huge.byte_length;
    let manifest = manifest_of(vec![huge]);
    assert!(matches!(
        rejection(
            &reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err()
        ),
        ArchiveRejection::SizeCeilingExceeded(_)
    ));

    // A declared ratio that only a decompression bomb produces.
    let mut bomb = store_entry("records/a.json", b"{}");
    bomb.compression = Compression::Deflate;
    bomb.byte_length = 64 * 1024 * 1024;
    bomb.stored_length = 1024;
    let manifest = manifest_of(vec![bomb]);
    assert!(matches!(
        rejection(
            &reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err()
        ),
        ArchiveRejection::SizeCeilingExceeded(_)
    ));
}

#[test]
fn a_media_entry_whose_name_disagrees_with_its_digest_is_refused() {
    let payload = b"RIFF....WAVE".to_vec();
    let mut entry = store_entry(&format!("media/{}.wav", "b".repeat(64)), &payload);
    entry.sha256 = sha_hex(&payload);
    let manifest = manifest_of(vec![entry]);

    let error =
        reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap_err();
    let ArchiveRejection::PathRejected { reason, .. } = rejection(&error) else {
        panic!("expected a path rejection, got {error:?}");
    };
    assert!(reason.contains("digest"), "unexpected reason {reason}");
}

#[test]
fn a_media_entry_named_for_its_digest_passes_the_policy() {
    let payload = b"RIFF....WAVE".to_vec();
    let digest = sha_hex(&payload);
    let entry = store_entry(&format!("media/{digest}.wav"), &payload);
    let manifest = manifest_of(vec![entry]);

    reader::validate_manifest_entries(&manifest, Path::new("/nonexistent")).unwrap();
}

// ---------------------------------------------------------------------------
// Body framing and payloads
// ---------------------------------------------------------------------------

#[test]
fn a_frame_header_that_disagrees_with_the_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mismatch.recording-archive");
    let payload = b"{}".to_vec();
    let declared = store_entry("records/a.json", &payload);
    let manifest = manifest_of(vec![declared.clone()]);

    // The frame claims a different schema version from the manifest entry.
    let mut frame = declared;
    frame.schema_version = Some("archive-record/2".to_owned());
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest), (frame, payload)], true),
    );

    let error = reader::inspect(&path, None, dir.path()).unwrap_err();
    assert!(matches!(rejection(&error), ArchiveRejection::Malformed(_)));
}

#[test]
fn a_body_that_ends_before_the_manifest_does_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short-body.recording-archive");
    let payload = b"{}".to_vec();
    let entry = store_entry("records/a.json", &payload);
    let manifest = manifest_of(vec![entry]);
    // The manifest promises one entry; the body carries only the terminator.
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest)], true),
    );

    assert!(matches!(
        rejection(&reader::inspect(&path, None, dir.path()).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn a_body_carrying_entries_the_manifest_does_not_list_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extra.recording-archive");
    let payload = b"{}".to_vec();
    let listed = store_entry("records/a.json", &payload);
    let smuggled = store_entry("records/b.json", &payload);
    let manifest = manifest_of(vec![listed.clone()]);

    write_plain(
        &path,
        &plaintext_header(),
        &body(
            &[
                manifest_frame(&manifest),
                (listed, payload.clone()),
                (smuggled, payload),
            ],
            true,
        ),
    );

    assert!(matches!(
        rejection(&reader::inspect(&path, None, dir.path()).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn a_body_without_a_terminator_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unterminated.recording-archive");
    let payload = b"{}".to_vec();
    let entry = store_entry("records/a.json", &payload);
    let manifest = manifest_of(vec![entry.clone()]);
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest), (entry, payload)], false),
    );

    assert!(matches!(
        rejection(&reader::inspect(&path, None, dir.path()).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

#[test]
fn a_payload_that_does_not_match_its_digest_names_the_entry_that_failed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("swapped.recording-archive");
    let declared = b"{\"a\":1}".to_vec();
    let entry = store_entry("records/a.json", &declared);
    let manifest = manifest_of(vec![entry.clone()]);
    // Same length, different bytes: only the digest catches this.
    let actual = b"{\"a\":2}".to_vec();
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest), (entry, actual)], true),
    );

    let error = reader::inspect(&path, None, dir.path()).unwrap_err();
    let ArchiveRejection::DigestMismatch { path: failed } = rejection(&error) else {
        panic!("expected a digest mismatch, got {error:?}");
    };
    assert_eq!(failed, "records/a.json");
}

#[test]
fn a_payload_shorter_than_its_declared_length_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short-payload.recording-archive");
    let real = b"{\"a\":1}".to_vec();
    let compressed = deflate(&real);
    let mut entry = store_entry("records/a.json", &real);
    entry.compression = Compression::Deflate;
    entry.stored_length = compressed.len() as u64;
    // The manifest claims more plaintext than the deflate stream produces.
    entry.byte_length = real.len() as u64 + 16;
    let manifest = manifest_of(vec![entry.clone()]);

    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest), (entry, compressed)], true),
    );

    assert!(matches!(
        rejection(&reader::inspect(&path, None, dir.path()).unwrap_err()),
        ArchiveRejection::DigestMismatch { .. }
    ));
}

#[test]
fn a_deflate_stream_that_expands_past_its_declared_length_is_abandoned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bomb.recording-archive");
    let real = vec![0u8; 512 * 1024];
    let compressed = deflate(&real);
    let mut entry = store_entry("records/a.json", &real);
    entry.compression = Compression::Deflate;
    entry.stored_length = compressed.len() as u64;
    // Declared small enough that the declared ratio passes the policy check,
    // so the hard stop inside the decompressor is the thing under test.
    entry.byte_length = 64;
    let manifest = manifest_of(vec![entry.clone()]);

    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest), (entry, compressed)], true),
    );

    let error = reader::inspect(&path, None, dir.path()).unwrap_err();
    let ArchiveRejection::SizeCeilingExceeded(detail) = rejection(&error) else {
        panic!("expected a size ceiling rejection, got {error:?}");
    };
    assert!(detail.contains("expands beyond"), "unexpected {detail}");
}

#[test]
fn a_frame_header_longer_than_the_ceiling_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fat-frame.recording-archive");
    let manifest = manifest_of(Vec::new());
    let mut bytes = body(&[manifest_frame(&manifest)], false);
    // A second frame declaring a header far past the ceiling.
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    write_plain(&path, &plaintext_header(), &bytes);

    assert!(matches!(
        rejection(&reader::inspect(&path, None, dir.path()).unwrap_err()),
        ArchiveRejection::Malformed(_)
    ));
}

// ---------------------------------------------------------------------------
// Boundaries and the happy side, so the failure assertions mean something
// ---------------------------------------------------------------------------

#[test]
fn an_archive_with_no_entries_inspects_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.recording-archive");
    plain_archive(&path, Vec::new());

    let report = reader::inspect(&path, None, dir.path()).unwrap();
    assert!(!report.encrypted);
    assert_eq!(report.manifest.totals.entry_count, 0);
    assert_eq!(report.total_bytes(), 0);
    assert_eq!(report.entries_by_namespace, Vec::new());
    assert!(report.payloads_verified);
    assert_ne!(report.nonportable_settings, Vec::<String>::new());
}

#[test]
fn a_single_zero_byte_entry_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.recording-archive");
    let payload = Vec::new();
    plain_archive(
        &path,
        vec![(store_entry("records/a.json", &payload), payload)],
    );

    let report = reader::inspect(&path, None, dir.path()).unwrap();
    assert_eq!(report.manifest.totals.entry_count, 1);
    assert_eq!(report.total_bytes(), 0);
    assert_eq!(report.entries_by_namespace, vec![("records".to_owned(), 1)]);
}

#[test]
fn namespace_counts_are_grouped_and_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many.recording-archive");
    let payload = b"{}".to_vec();
    let media = vec![1u8, 2, 3];
    let media_digest = sha_hex(&media);
    plain_archive(
        &path,
        vec![
            (store_entry("transcripts/t.json", &payload), payload.clone()),
            (store_entry("records/a.json", &payload), payload.clone()),
            (store_entry("records/b.json", &payload), payload.clone()),
            (
                store_entry(&format!("media/{media_digest}.wav"), &media),
                media,
            ),
        ],
    );

    let report = reader::inspect(&path, None, dir.path()).unwrap();
    assert_eq!(
        report.entries_by_namespace,
        vec![
            ("media".to_owned(), 1),
            ("records".to_owned(), 2),
            ("transcripts".to_owned(), 1),
        ]
    );
    assert_eq!(report.total_bytes(), payload.len() as u64 * 3 + 3);
}

#[test]
fn a_deflated_entry_streams_its_plaintext_to_the_sink() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deflated.recording-archive");
    let real = br#"{"schema_version":"archive-record/1"}"#.repeat(40);
    let compressed = deflate(&real);
    let mut entry = store_entry("records/a.json", &real);
    entry.compression = Compression::Deflate;
    entry.stored_length = compressed.len() as u64;
    let manifest = manifest_of(vec![entry.clone()]);
    write_plain(
        &path,
        &plaintext_header(),
        &body(&[manifest_frame(&manifest), (entry, compressed)], true),
    );

    #[derive(Debug, Default)]
    struct Collect {
        bytes: Vec<u8>,
    }
    impl EntrySink for Collect {
        fn begin(&mut self, _entry: &ManifestEntry) -> ExportResult<()> {
            Ok(())
        }
        fn chunk(&mut self, bytes: &[u8]) -> ExportResult<()> {
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
        fn commit(&mut self, _entry: &ManifestEntry) -> ExportResult<()> {
            Ok(())
        }
    }

    let mut sink = Collect::default();
    let (manifest, encrypted) = reader::stream(&path, None, dir.path(), &mut sink).unwrap();
    assert!(!encrypted);
    assert_eq!(manifest.entries.len(), 1);
    assert_eq!(sink.bytes, real);
}

#[test]
fn a_sink_failure_stops_the_stream_at_the_entry_that_failed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sink.recording-archive");
    let payload = b"{\"a\":1}".to_vec();
    plain_archive(
        &path,
        vec![
            (store_entry("records/a.json", &payload), payload.clone()),
            (store_entry("records/b.json", &payload), payload),
        ],
    );

    /// Refuses at the stage named by `fail_on`, so each hook's failure path is
    /// exercised separately.
    #[derive(Debug)]
    struct Failing {
        fail_on: &'static str,
        committed: usize,
    }
    impl EntrySink for Failing {
        fn begin(&mut self, _entry: &ManifestEntry) -> ExportResult<()> {
            if self.fail_on == "begin" {
                return Err(ExportError::IntegrityFailed);
            }
            Ok(())
        }
        fn chunk(&mut self, _bytes: &[u8]) -> ExportResult<()> {
            if self.fail_on == "chunk" {
                return Err(ExportError::IntegrityFailed);
            }
            Ok(())
        }
        fn commit(&mut self, _entry: &ManifestEntry) -> ExportResult<()> {
            if self.fail_on == "commit" {
                return Err(ExportError::IntegrityFailed);
            }
            self.committed += 1;
            Ok(())
        }
    }

    for stage in ["begin", "chunk", "commit"] {
        let mut sink = Failing {
            fail_on: stage,
            committed: 0,
        };
        let error = reader::stream(&path, None, dir.path(), &mut sink).unwrap_err();
        assert!(
            matches!(error, ExportError::IntegrityFailed),
            "{stage} should propagate, got {error:?}"
        );
        assert_eq!(sink.committed, 0, "{stage} must not commit an entry");
    }

    let mut sink = VerifyingSink::default();
    reader::stream(&path, None, dir.path(), &mut sink).unwrap();
    assert_eq!(sink.seen, vec!["records/a.json", "records/b.json"]);
}

#[test]
fn an_encrypted_archive_round_trips_through_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sealed.recording-archive");
    let header = encrypted_header();
    let payload = br#"{"schema_version":"archive-record/1"}"#.to_vec();
    let entry = store_entry("records/a.json", &payload);
    let manifest = manifest_of(vec![entry.clone()]);
    write_encrypted(
        &path,
        &header,
        PASSWORD,
        &body(&[manifest_frame(&manifest), (entry, payload)], true),
    );

    let report = reader::inspect(&path, Some(PASSWORD), dir.path()).unwrap();
    assert!(report.encrypted);
    assert!(report.payloads_verified);
    assert_eq!(report.manifest.totals.entry_count, 1);
}

#[test]
fn digesting_a_file_reports_its_bytes_and_refuses_a_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.bin");
    std::fs::write(&path, b"abc").unwrap();

    let (digest, length) = reader::digest_file(&path).unwrap();
    assert_eq!(digest, sha_hex(b"abc"));
    assert_eq!(length, 3);

    let missing = dir.path().join("gone.bin");
    assert!(matches!(
        reader::digest_file(&missing).unwrap_err(),
        ExportError::Io(_)
    ));
}

#[test]
fn opening_an_archive_that_is_not_there_is_a_storage_failure_not_a_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let error = reader::inspect(
        &dir.path().join("absent.recording-archive"),
        None,
        dir.path(),
    )
    .unwrap_err();

    assert!(matches!(error, ExportError::Io(_)));
    assert_eq!(
        error.code(),
        plaudon_domain::error::AppErrorCode::StorageUnavailable
    );
}
