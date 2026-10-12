//! Builders for the malicious `.recording-archive` corpus (spec 18.6A
//! "Portable backup" cluster, J2.3/J2.5).
//!
//! These write containers the product's own writer refuses to produce:
//! traversal paths, link entries, duplicate names, decompression bombs,
//! altered digests, unsupported versions and abusive KDF parameters. They are
//! deliberately built at the byte level so the restore path is tested against
//! a hostile *file*, not against a hostile in-memory struct.

use std::io::Write;
use std::path::Path;

use flate2::write::DeflateEncoder;
use plaudon_export::archive::container;
use plaudon_export::archive::crypto::{EncryptionHeader, KdfProfile};
use plaudon_export::archive::manifest::{
    ArchiveHeader, Compression, Manifest, ManifestEntry, ManifestLimits, ManifestTotals,
    ProtectionKind, FORMAT_VERSION, INTEGRITY_NOTE, MANIFEST_PATH, MANIFEST_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};

/// One hostile archive: a file name and the reason it must be refused.
#[derive(Debug, Clone, Copy)]
pub struct HostileCase {
    /// File name inside the corpus directory.
    pub file_name: &'static str,
    /// What the archive attempts.
    pub attack: &'static str,
}

/// Every case in the corpus.
pub const CASES: &[HostileCase] = &[
    HostileCase {
        file_name: "traversal.recording-archive",
        attack: "an entry path that escapes the destination with ..",
    },
    HostileCase {
        file_name: "absolute-path.recording-archive",
        attack: "an absolute entry path",
    },
    HostileCase {
        file_name: "windows-drive.recording-archive",
        attack: "a drive-qualified entry path",
    },
    HostileCase {
        file_name: "symlink-entry.recording-archive",
        attack: "an entry that declares itself a symbolic link",
    },
    HostileCase {
        file_name: "duplicate-path.recording-archive",
        attack: "two entries that collide when case is folded",
    },
    HostileCase {
        file_name: "decompression-bomb.recording-archive",
        attack: "a tiny deflate stream declaring gigabytes of output",
    },
    HostileCase {
        file_name: "altered-hash.recording-archive",
        attack: "a payload that does not match its recorded digest",
    },
    HostileCase {
        file_name: "integrity-digest.recording-archive",
        attack: "a manifest whose integrity digest does not cover its entries",
    },
    HostileCase {
        file_name: "unsupported-format.recording-archive",
        attack: "a container format version this build does not read",
    },
    HostileCase {
        file_name: "unsupported-schema.recording-archive",
        attack: "a manifest schema version this build does not implement",
    },
    HostileCase {
        file_name: "foreign-namespace.recording-archive",
        attack: "an entry outside the documented namespaces",
    },
    HostileCase {
        file_name: "media-digest-mismatch.recording-archive",
        attack: "a content-addressed media path that does not match its digest",
    },
    HostileCase {
        file_name: "kdf-resource-abuse.recording-archive",
        attack: "key-derivation parameters that would exhaust memory",
    },
    HostileCase {
        file_name: "not-an-archive.recording-archive",
        attack: "a file that is not a recording archive at all",
    },
];

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = DeflateEncoder::new(&mut out, flate2::Compression::default());
        encoder.write_all(bytes).expect("deflate");
        encoder.finish().expect("deflate finish");
    }
    out
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn entry(path: &str, payload: &[u8]) -> (ManifestEntry, Vec<u8>) {
    let stored = deflate(payload);
    (
        ManifestEntry {
            path: path.to_owned(),
            kind: "file".to_owned(),
            sha256: digest(payload),
            byte_length: payload.len() as u64,
            stored_length: stored.len() as u64,
            compression: Compression::Deflate,
            schema_version: Some("archive-record/1".to_owned()),
        },
        stored,
    )
}

fn manifest_for(entries: &[ManifestEntry]) -> Manifest {
    Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION.to_owned(),
        format_version: FORMAT_VERSION,
        created_at: "2026-10-11T00:00:00Z".to_owned(),
        app: plaudon_export::bundle::AppStamp::default(),
        source_os: "macos".to_owned(),
        totals: ManifestTotals {
            entry_count: entries.len() as u64,
            byte_length: entries.iter().map(|entry| entry.byte_length).sum(),
        },
        limits: ManifestLimits {
            max_entry_bytes: 16 * 1024 * 1024 * 1024,
            max_total_bytes: 1024 * 1024 * 1024 * 1024,
            max_entries: 1_000_000,
        },
        entries: entries.to_vec(),
        integrity_digest: Manifest::compute_integrity_digest(entries),
        integrity_note: INTEGRITY_NOTE.to_owned(),
        nonportable_settings: vec!["fixture".to_owned()],
    }
}

/// Write one hostile container.
///
/// `format_version_override` and `manifest_mutate` let a case break exactly
/// one rule while leaving everything else well formed.
fn write_container(
    path: &Path,
    manifest: &Manifest,
    payloads: &[(ManifestEntry, Vec<u8>)],
    format_version_override: Option<u16>,
    encryption: Option<EncryptionHeader>,
) {
    let mut body: Vec<u8> = Vec::new();
    let manifest_json = serde_json::to_vec(manifest).expect("manifest json");
    let compressed = deflate(&manifest_json);
    container::write_frame_header(
        &mut body,
        &ManifestEntry {
            path: MANIFEST_PATH.to_owned(),
            kind: "file".to_owned(),
            sha256: digest(&manifest_json),
            byte_length: manifest_json.len() as u64,
            stored_length: compressed.len() as u64,
            compression: Compression::Deflate,
            schema_version: Some(MANIFEST_SCHEMA_VERSION.to_owned()),
        },
    )
    .expect("manifest frame");
    body.extend_from_slice(&compressed);

    for (entry, stored) in payloads {
        container::write_frame_header(&mut body, entry).expect("frame");
        body.extend_from_slice(stored);
    }
    container::write_terminator(&mut body).expect("terminator");

    let header = ArchiveHeader {
        format_version: format_version_override.unwrap_or(FORMAT_VERSION),
        manifest_schema_version: manifest.schema_version.clone(),
        protection: if encryption.is_some() {
            plaudon_export::archive::crypto::CIPHER_NAME.to_owned()
        } else {
            "plaintext".to_owned()
        },
        encryption,
    };

    let mut file: Vec<u8> = Vec::new();
    // The preamble writer pins the supported version, so an "unsupported
    // version" case patches the two bytes afterwards.
    container::write_preamble(
        &mut file,
        if header.encryption.is_some() {
            ProtectionKind::Encrypted
        } else {
            ProtectionKind::Plaintext
        },
        &header,
    )
    .expect("preamble");
    if let Some(version) = format_version_override {
        file[8..10].copy_from_slice(&version.to_le_bytes());
    }
    file.extend_from_slice(&body);
    std::fs::write(path, &file).expect("write archive");
}

/// Generate the whole corpus into `directory`.
pub fn write_corpus(directory: &Path) {
    std::fs::create_dir_all(directory).expect("corpus directory");
    let record = br#"{"schema_version":"archive-record/1","recording":{"id":"01932b3a-4c5d-7e8f-9a0b-1c2d3e4f5061","captured_at":null,"duration_ms":null,"preferred_media_asset_id":null,"created_at":1,"deleted_at":null},"media_assets":[],"recording_media":[],"media_revisions":[],"source_items":[]}"#;

    // Traversal.
    let (mut hostile, stored) = entry("records/a.json", record);
    "records/../../escape.json".clone_into(&mut hostile.path);
    let manifest = manifest_for(std::slice::from_ref(&hostile));
    write_container(
        &directory.join("traversal.recording-archive"),
        &manifest,
        &[(hostile, stored)],
        None,
        None,
    );

    // Absolute path.
    let (mut hostile, stored) = entry("records/a.json", record);
    "/etc/passwd".clone_into(&mut hostile.path);
    let manifest = manifest_for(std::slice::from_ref(&hostile));
    write_container(
        &directory.join("absolute-path.recording-archive"),
        &manifest,
        &[(hostile, stored)],
        None,
        None,
    );

    // Drive-qualified path.
    let (mut hostile, stored) = entry("records/a.json", record);
    "C:\\Windows\\System32\\drivers\\etc\\hosts".clone_into(&mut hostile.path);
    let manifest = manifest_for(std::slice::from_ref(&hostile));
    write_container(
        &directory.join("windows-drive.recording-archive"),
        &manifest,
        &[(hostile, stored)],
        None,
        None,
    );

    // Link entry.
    let (mut hostile, stored) = entry("records/a.json", record);
    "symlink".clone_into(&mut hostile.kind);
    let manifest = manifest_for(std::slice::from_ref(&hostile));
    write_container(
        &directory.join("symlink-entry.recording-archive"),
        &manifest,
        &[(hostile, stored)],
        None,
        None,
    );

    // Duplicate path under case folding.
    let (first, first_stored) = entry("records/a.json", record);
    let (mut second, second_stored) = entry("records/A.JSON", record);
    second.sha256.clone_from(&first.sha256);
    let manifest = manifest_for(&[first.clone(), second.clone()]);
    write_container(
        &directory.join("duplicate-path.recording-archive"),
        &manifest,
        &[(first, first_stored), (second, second_stored)],
        None,
        None,
    );

    // Decompression bomb: a few kilobytes of deflate claiming 8 GiB.
    let zeros = vec![0u8; 1024 * 1024];
    let stored = deflate(&zeros);
    let bomb = ManifestEntry {
        path: "records/bomb.json".to_owned(),
        kind: "file".to_owned(),
        sha256: digest(&zeros),
        byte_length: 8 * 1024 * 1024 * 1024,
        stored_length: stored.len() as u64,
        compression: Compression::Deflate,
        schema_version: Some("archive-record/1".to_owned()),
    };
    let manifest = manifest_for(std::slice::from_ref(&bomb));
    write_container(
        &directory.join("decompression-bomb.recording-archive"),
        &manifest,
        &[(bomb, stored)],
        None,
        None,
    );

    // Altered payload.
    let (altered, _) = entry("records/a.json", record);
    let tampered = deflate(br#"{"schema_version":"archive-record/1","recording":{"id":"01932b3a-4c5d-7e8f-9a0b-1c2d3e4f5062","captured_at":null,"duration_ms":null,"preferred_media_asset_id":null,"created_at":9,"deleted_at":null},"media_assets":[],"recording_media":[],"media_revisions":[],"source_items":[]}"#);
    let mut altered_entry = altered.clone();
    altered_entry.stored_length = tampered.len() as u64;
    let manifest = manifest_for(std::slice::from_ref(&altered_entry));
    write_container(
        &directory.join("altered-hash.recording-archive"),
        &manifest,
        &[(altered_entry, tampered)],
        None,
        None,
    );

    // Manifest whose integrity digest does not cover its entries.
    let (plain, stored) = entry("records/a.json", record);
    let mut manifest = manifest_for(std::slice::from_ref(&plain));
    manifest.integrity_digest = "0".repeat(64);
    write_container(
        &directory.join("integrity-digest.recording-archive"),
        &manifest,
        &[(plain, stored)],
        None,
        None,
    );

    // Unsupported container version.
    let (plain, stored) = entry("records/a.json", record);
    let manifest = manifest_for(std::slice::from_ref(&plain));
    write_container(
        &directory.join("unsupported-format.recording-archive"),
        &manifest,
        &[(plain, stored)],
        Some(99),
        None,
    );

    // Unsupported manifest schema.
    let (plain, stored) = entry("records/a.json", record);
    let mut manifest = manifest_for(std::slice::from_ref(&plain));
    "recording-archive-manifest/99".clone_into(&mut manifest.schema_version);
    write_container(
        &directory.join("unsupported-schema.recording-archive"),
        &manifest,
        &[(plain, stored)],
        None,
        None,
    );

    // Entry outside the documented namespaces.
    let (mut foreign, stored) = entry("records/a.json", record);
    "jobs/pending.json".clone_into(&mut foreign.path);
    let manifest = manifest_for(std::slice::from_ref(&foreign));
    write_container(
        &directory.join("foreign-namespace.recording-archive"),
        &manifest,
        &[(foreign, stored)],
        None,
        None,
    );

    // Media path that does not match its digest.
    let audio = b"RIFF....WAVEfixture".to_vec();
    let stored = audio.clone();
    let media = ManifestEntry {
        path: format!("media/{}.wav", "b".repeat(64)),
        kind: "file".to_owned(),
        sha256: digest(&audio),
        byte_length: audio.len() as u64,
        stored_length: stored.len() as u64,
        compression: Compression::Store,
        schema_version: None,
    };
    let manifest = manifest_for(std::slice::from_ref(&media));
    write_container(
        &directory.join("media-digest-mismatch.recording-archive"),
        &manifest,
        &[(media, stored)],
        None,
        None,
    );

    // Abusive key-derivation parameters.
    let (plain, stored) = entry("records/a.json", record);
    let manifest = manifest_for(std::slice::from_ref(&plain));
    let mut encryption = EncryptionHeader::generate(KdfProfile::Test);
    encryption.kdf.m_cost_kib = 4 * 1024 * 1024;
    encryption.kdf.t_cost = 64;
    // Fixed salt and nonce prefix: the corpus is committed, so every byte of
    // it has to be reproducible by the generator.
    "00112233445566778899aabbccddeeff".clone_into(&mut encryption.kdf.salt_hex);
    "000102030405060708090a0b0c0d0e0f101112".clone_into(&mut encryption.nonce_prefix_hex);
    write_container(
        &directory.join("kdf-resource-abuse.recording-archive"),
        &manifest,
        &[(plain, stored)],
        None,
        Some(encryption),
    );

    // Not an archive at all.
    std::fs::write(
        directory.join("not-an-archive.recording-archive"),
        b"PK\x03\x04 this is a zip file pretending to be a recording archive",
    )
    .expect("write archive");
}
