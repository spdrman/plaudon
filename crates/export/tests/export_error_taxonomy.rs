//! The stable failure taxonomy and the pure helpers around it (spec 15.4).
//!
//! An export failure reaches the frontend as a code plus the error's canonical
//! message, so both halves are pinned here: every [`ExportError`] variant maps
//! to exactly one [`AppErrorCode`], and every [`ArchiveRejection`] explains
//! itself without leaking a password, a key or transcript text.

use plaudon_domain::error::AppErrorCode;
use plaudon_export::archive::crypto::KdfProfile;
use plaudon_export::archive::manifest::{
    host_os, in_known_namespace, Compression, Manifest, ManifestEntry, ProtectionKind, NAMESPACES,
};
use plaudon_export::archive::voiceprofiles::{
    embedding_shape, EmbeddingShape, EnrollmentRow, ExemplarRow, VoiceProfileDocument,
    BLOB_RUN_CHARS, MAX_EMBEDDING_DIM, MAX_RETENTION_MONTHS, VOICEPROFILE_SCHEMA,
};
use plaudon_export::bundle;
use plaudon_export::captions::{self, CaptionFormat};
use plaudon_export::error::{ArchiveRejection, CaptionRefusal, ExportError};
use plaudon_export::render;
use plaudon_persistence::PersistenceError;

// ---------------------------------------------------------------------------
// Rejections
// ---------------------------------------------------------------------------

fn every_rejection() -> Vec<ArchiveRejection> {
    vec![
        ArchiveRejection::NotAnArchive,
        ArchiveRejection::UnsupportedFormatVersion {
            found: 2,
            supported: 1,
        },
        ArchiveRejection::UnsupportedManifestSchema {
            found: "recording-archive-manifest/9".to_owned(),
        },
        ArchiveRejection::Malformed("a frame header is truncated".to_owned()),
        ArchiveRejection::PathRejected {
            path: "records/../x".to_owned(),
            reason: "traversal".to_owned(),
        },
        ArchiveRejection::DuplicatePath("records/a.json".to_owned()),
        ArchiveRejection::SizeCeilingExceeded("entry_too_large".to_owned()),
        ArchiveRejection::DigestMismatch {
            path: "records/a.json".to_owned(),
        },
        ArchiveRejection::IntegrityDigestMismatch,
        ArchiveRejection::UnsupportedEntryKind {
            path: "records/a".to_owned(),
            kind: "symlink".to_owned(),
        },
        ArchiveRejection::KdfParametersRejected("time cost 0 is out of range".to_owned()),
    ]
}

#[test]
fn every_rejection_explains_itself_distinctly_and_echoes_only_its_own_detail() {
    let mut explanations: Vec<String> = every_rejection()
        .iter()
        .map(ArchiveRejection::explanation)
        .collect();
    assert_eq!(explanations.len(), 11);
    for explanation in &explanations {
        assert_ne!(explanation, &String::new());
        assert!(explanation.starts_with(|c: char| c.is_ascii_uppercase()));
        assert!(explanation.ends_with('.') || explanation.ends_with(|c: char| c.is_alphanumeric()));
    }
    explanations.sort();
    explanations.dedup();
    assert_eq!(explanations.len(), 11, "each variant explains itself");

    // A rejection renders the detail it was constructed with and nothing it
    // was not: the detail is produced by this crate, never by the archive.
    let path = ArchiveRejection::PathRejected {
        path: "records/../x".to_owned(),
        reason: "traversal".to_owned(),
    };
    let rendered = path.explanation();
    assert!(rendered.contains("records/../x"));
    assert!(rendered.contains("traversal"));
    assert!(ArchiveRejection::IntegrityDigestMismatch
        .explanation()
        .contains("entry list"));
}

#[test]
fn a_version_rejection_names_both_versions_and_compares_by_value() {
    let found = ArchiveRejection::UnsupportedFormatVersion {
        found: 7,
        supported: 1,
    };
    assert!(found.explanation().contains('7'));
    assert_eq!(found.clone(), found);
    assert_ne!(
        found,
        ArchiveRejection::UnsupportedFormatVersion {
            found: 8,
            supported: 1,
        }
    );
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

#[test]
fn every_failure_maps_to_its_stable_application_code() {
    let cases: Vec<(ExportError, AppErrorCode)> = vec![
        (
            ExportError::DestinationRejected("no folder".to_owned()),
            AppErrorCode::ExportDestinationRejected,
        ),
        (
            ExportError::CaptionsUnavailable(CaptionRefusal::NoAlignment),
            AppErrorCode::CaptionsUnavailable,
        ),
        (
            ExportError::SanitizationUnsupported("opus".to_owned()),
            AppErrorCode::UnsupportedCodec,
        ),
        (
            ExportError::ArchiveRejected(ArchiveRejection::NotAnArchive),
            AppErrorCode::ArchiveRejected,
        ),
        (
            ExportError::DecryptionFailed,
            AppErrorCode::ArchiveDecryptionFailed,
        ),
        (
            ExportError::PasswordRequired,
            AppErrorCode::ArchiveDecryptionFailed,
        ),
        (
            ExportError::SecretExclusionFailed("api key".to_owned()),
            AppErrorCode::SecretExclusionFailed,
        ),
        (
            ExportError::VoiceProfilesRequireEncryption,
            AppErrorCode::SecretExclusionFailed,
        ),
        (ExportError::TargetLibraryNotEmpty, AppErrorCode::Conflict),
        (
            ExportError::RestoreConflict("recording r1".to_owned()),
            AppErrorCode::Conflict,
        ),
        (ExportError::IntegrityFailed, AppErrorCode::IntegrityFailed),
        (ExportError::RecordNotFound, AppErrorCode::RecordNotFound),
        (
            ExportError::ConfirmationRequired,
            AppErrorCode::ConfirmationRequired,
        ),
        (
            ExportError::Persistence(PersistenceError::RecordNotFound),
            AppErrorCode::RecordNotFound,
        ),
        (
            ExportError::Persistence(PersistenceError::StorageFull),
            AppErrorCode::StorageFull,
        ),
        (
            ExportError::Io(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "no space",
            )),
            AppErrorCode::StorageFull,
        ),
        (
            ExportError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            )),
            AppErrorCode::StorageUnavailable,
        ),
    ];

    for (error, expected) in cases {
        assert_eq!(error.code(), expected, "{error:?}");
        // The envelope the frontend receives is built from the code and the
        // canonical message, and never from a `Debug` rendering.
        let app: plaudon_domain::error::AppError = error.into();
        assert_eq!(app.code(), expected);
        assert_ne!(app.detail(), "");
    }
}

#[test]
fn io_and_serialization_failures_convert_without_losing_their_kind() {
    let io: ExportError = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
    assert!(matches!(io, ExportError::Io(_)));
    assert_eq!(io.code(), AppErrorCode::StorageUnavailable);
    assert_eq!(io.to_string(), "filesystem operation failed");

    let serde_error = serde_json::from_str::<Manifest>("{").unwrap_err();
    let serialization: ExportError = serde_error.into();
    assert!(matches!(serialization, ExportError::Serialization(_)));
    assert_eq!(serialization.code(), AppErrorCode::InternalError);
    assert_eq!(
        serialization.to_string(),
        "export document could not be serialized"
    );
}

#[test]
fn a_caption_refusal_says_which_half_of_the_rule_failed() {
    assert_ne!(
        CaptionRefusal::NoAlignment.explanation(),
        CaptionRefusal::NoSegments.explanation()
    );
    assert!(CaptionRefusal::NoAlignment
        .explanation()
        .contains("do not invent"));
    assert!(CaptionRefusal::NoSegments
        .explanation()
        .contains("segments"));
    assert_eq!(
        ExportError::CaptionsUnavailable(CaptionRefusal::NoSegments).to_string(),
        CaptionRefusal::NoSegments.explanation()
    );
}

// ---------------------------------------------------------------------------
// Cue time arithmetic
// ---------------------------------------------------------------------------

#[test]
fn cue_times_render_and_parse_at_their_boundaries() {
    for format in [CaptionFormat::Srt, CaptionFormat::Vtt] {
        for ms in [0u64, 1, 999, 1_000, 59_999, 60_000, 3_599_999, 3_600_000] {
            let text = captions::format_cue_time(ms, format);
            assert_eq!(captions::parse_cue_time(&text), Some(ms), "{text}");
        }
    }
    assert_eq!(
        captions::format_cue_time(3_723_456, CaptionFormat::Srt),
        "01:02:03,456"
    );
    assert_eq!(
        captions::format_cue_time(3_723_456, CaptionFormat::Vtt),
        "01:02:03.456"
    );
    assert_eq!(CaptionFormat::Srt.extension(), "srt");
    assert_eq!(CaptionFormat::Vtt.extension(), "vtt");
}

#[test]
fn a_malformed_cue_time_parses_to_nothing_rather_than_to_a_guess() {
    for text in [
        "",
        "00:00:00",        // no decimal separator
        "00:00,000",       // too few clock fields
        "00:00:00:00,000", // too many clock fields
        "00:60:00,000",    // minutes out of range
        "00:00:60,000",    // seconds out of range
        "00:00:00,12",     // millisecond field too short
        "00:00:00,1234",   // millisecond field too long
        "aa:00:00,000",    // non-numeric hours
        "00:bb:00,000",    // non-numeric minutes
        "00:00:cc,000",    // non-numeric seconds
        "00:00:00,abc",    // non-numeric milliseconds
    ] {
        assert_eq!(captions::parse_cue_time(text), None, "{text:?}");
    }
}

#[test]
fn offsets_render_with_millisecond_precision_at_their_boundaries() {
    assert_eq!(render::format_offset(0), "00:00:00.000");
    assert_eq!(render::format_offset(1), "00:00:00.001");
    assert_eq!(render::format_offset(59_999), "00:00:59.999");
    assert_eq!(render::format_offset(3_600_000), "01:00:00.000");
    assert_eq!(render::format_offset(359_999_999), "99:59:59.999");
}

#[test]
fn control_characters_are_stripped_from_structure_but_newlines_survive() {
    assert_eq!(render::strip_controls("plain"), "plain");
    assert_eq!(render::strip_controls("a\u{0}b\u{7}c"), "abc");
    assert_eq!(
        render::strip_controls("line\nnext\tcell"),
        "line\nnext\tcell"
    );
    assert_eq!(render::strip_controls(""), "");
}

// ---------------------------------------------------------------------------
// Manifest vocabulary
// ---------------------------------------------------------------------------

#[test]
fn the_protection_byte_round_trips_and_rejects_anything_else() {
    assert_eq!(ProtectionKind::Plaintext.code(), 0);
    assert_eq!(ProtectionKind::Encrypted.code(), 1);
    assert_eq!(
        ProtectionKind::from_code(0),
        Some(ProtectionKind::Plaintext)
    );
    assert_eq!(
        ProtectionKind::from_code(1),
        Some(ProtectionKind::Encrypted)
    );
    for code in [2u8, 7, 255] {
        assert_eq!(ProtectionKind::from_code(code), None, "{code}");
    }
}

#[test]
fn compression_names_are_stable_and_namespaces_are_prefix_exact() {
    assert_eq!(Compression::Store.as_str(), "store");
    assert_eq!(Compression::Deflate.as_str(), "deflate");

    for namespace in NAMESPACES {
        assert!(in_known_namespace(&format!("{namespace}thing.json")));
    }
    for outside in ["jobs/1.json", "manifest.json", "record/x.json", "", "media"] {
        assert!(!in_known_namespace(outside), "{outside}");
    }
    assert!(["macos", "windows", "linux", "unknown"].contains(&host_os()));
}

#[test]
fn media_entries_are_selected_by_namespace_not_by_extension() {
    let entry = |path: &str| ManifestEntry {
        path: path.to_owned(),
        kind: "file".to_owned(),
        sha256: "a".repeat(64),
        byte_length: 1,
        stored_length: 1,
        compression: Compression::Store,
        schema_version: None,
    };
    let entries = vec![
        entry("records/a.json"),
        entry("media/aa.wav"),
        entry("transcripts/t.wav"),
        entry("media/bb.bin"),
    ];
    let digest = Manifest::compute_integrity_digest(&entries);
    assert_eq!(digest.len(), 64);

    let manifest = serde_json::from_value::<Manifest>(serde_json::json!({
        "schema_version": "recording-archive-manifest/1",
        "format_version": 1,
        "created_at": "1970-01-01T00:00:00Z",
        "app": { "name": "plaudon", "version": "0" },
        "source_os": "linux",
        "entries": entries,
        "totals": { "entry_count": 4, "byte_length": 4 },
        "limits": { "max_entry_bytes": 1, "max_total_bytes": 1, "max_entries": 1 },
        "integrity_digest": digest,
        "integrity_note": "note",
        "nonportable_settings": [],
    }))
    .unwrap();

    let media: Vec<&str> = manifest
        .media_entries()
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(media, vec!["media/aa.wav", "media/bb.bin"]);
}

#[test]
fn key_derivation_profiles_are_ordered_by_cost() {
    let (backup_m, backup_t, backup_p) = KdfProfile::Backup.parameters();
    let (sensitive_m, sensitive_t, _) = KdfProfile::Sensitive.parameters();
    let (test_m, test_t, test_p) = KdfProfile::Test.parameters();

    assert_eq!((backup_m, backup_t, backup_p), (65_536, 3, 4));
    assert!(sensitive_m > backup_m && sensitive_t > backup_t);
    assert!(test_m < backup_m, "the test profile is the cheap one");
    assert_eq!((test_t, test_p), (1, 1));
}

// ---------------------------------------------------------------------------
// Voice-profile document validation (VP-078, VP-130)
// ---------------------------------------------------------------------------

fn exemplar() -> ExemplarRow {
    ExemplarRow {
        id: "exemplar-1".to_owned(),
        recording_id: None,
        cluster_key: None,
        source_start_ms: Some(0),
        source_end_ms: Some(2_000),
        speech_ms: 2_000,
        model_id: "ecapa".to_owned(),
        model_version: "1".to_owned(),
        model_sha256: "b".repeat(64),
        embedding_dim: 192,
        embedding_cipher_b64: "AAAA".to_owned(),
        embedding_nonce_b64: "BBBB".to_owned(),
        created_at: 1,
    }
}

fn profile() -> VoiceProfileDocument {
    VoiceProfileDocument {
        schema_version: VOICEPROFILE_SCHEMA.to_owned(),
        person_id: "person-1".to_owned(),
        enrollment: EnrollmentRow {
            consent_version: "voice-consent/1".to_owned(),
            consent_note: None,
            retention_months: 6,
            last_matched_at: None,
            status: "active".to_owned(),
            expired_at: None,
            created_at: 1,
        },
        exemplars: vec![exemplar()],
    }
}

#[test]
fn a_well_formed_voice_profile_validates_at_both_retention_bounds() {
    profile().validate().unwrap();

    for months in [1, MAX_RETENTION_MONTHS] {
        let mut document = profile();
        document.enrollment.retention_months = months;
        document.validate().unwrap();
    }
    for status in ["active", "expired"] {
        let mut document = profile();
        document.enrollment.status = status.to_owned();
        document.validate().unwrap();
    }
    assert_eq!(
        VoiceProfileDocument::entry_path("person-1"),
        "voiceprofiles/person-1.json"
    );
}

/// One violation applied to an otherwise valid voice-profile document.
type Mutation = Box<dyn Fn(&mut VoiceProfileDocument)>;

#[test]
fn every_voice_profile_rule_refuses_its_own_violation() {
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "schema",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.schema_version = "archive-voiceprofile/2".to_owned();
            }),
        ),
        (
            "empty person",
            Box::new(|d: &mut VoiceProfileDocument| d.person_id.clear()),
        ),
        (
            "oversized person",
            Box::new(|d: &mut VoiceProfileDocument| d.person_id = "p".repeat(65)),
        ),
        (
            "retention zero",
            Box::new(|d: &mut VoiceProfileDocument| d.enrollment.retention_months = 0),
        ),
        (
            "retention beyond the ceiling",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.enrollment.retention_months = MAX_RETENTION_MONTHS + 1;
            }),
        ),
        (
            "empty consent version",
            Box::new(|d: &mut VoiceProfileDocument| d.enrollment.consent_version.clear()),
        ),
        (
            "oversized consent version",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.enrollment.consent_version = "v".repeat(65);
            }),
        ),
        (
            "status",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.enrollment.status = "pending".to_owned();
            }),
        ),
        (
            "empty exemplar id",
            Box::new(|d: &mut VoiceProfileDocument| d.exemplars[0].id.clear()),
        ),
        (
            "oversized exemplar id",
            Box::new(|d: &mut VoiceProfileDocument| d.exemplars[0].id = "e".repeat(65)),
        ),
        (
            "zero dimensions",
            Box::new(|d: &mut VoiceProfileDocument| d.exemplars[0].embedding_dim = 0),
        ),
        (
            "too many dimensions",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.exemplars[0].embedding_dim = MAX_EMBEDDING_DIM as u32 + 1;
            }),
        ),
        (
            "speech below the minimum",
            Box::new(|d: &mut VoiceProfileDocument| d.exemplars[0].speech_ms = 999),
        ),
        (
            "model digest length",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.exemplars[0].model_sha256 = "b".repeat(63);
            }),
        ),
        (
            "model digest alphabet",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.exemplars[0].model_sha256 = "z".repeat(64);
            }),
        ),
        (
            "inverted span",
            Box::new(|d: &mut VoiceProfileDocument| {
                d.exemplars[0].source_start_ms = Some(2_000);
                d.exemplars[0].source_end_ms = Some(1_000);
            }),
        ),
        (
            "missing ciphertext",
            Box::new(|d: &mut VoiceProfileDocument| d.exemplars[0].embedding_cipher_b64.clear()),
        ),
        (
            "missing nonce",
            Box::new(|d: &mut VoiceProfileDocument| d.exemplars[0].embedding_nonce_b64.clear()),
        ),
    ];

    for (name, mutate) in cases {
        let mut document = profile();
        mutate(&mut document);
        let reason = document
            .validate()
            .expect_err(&format!("{name} should be refused"));
        assert!(!reason.is_empty(), "{name}");
        // A refusal never quotes the value it refused: these fields sit next to
        // biometric material.
        assert!(!reason.contains("AAAA"), "{name}: {reason}");
        assert!(!reason.contains("person-1"), "{name}: {reason}");
    }
}

#[test]
fn an_embedding_is_recognised_by_its_field_name_or_by_its_shape() {
    assert_eq!(
        embedding_shape(Some("embedding_cipher_b64"), "x"),
        Some(EmbeddingShape::NamedField)
    );
    assert_eq!(
        embedding_shape(Some("speaker_vector"), "x"),
        Some(EmbeddingShape::NamedField)
    );
    assert_eq!(
        embedding_shape(None, &"A".repeat(BLOB_RUN_CHARS)),
        Some(EmbeddingShape::BinaryBlob)
    );
    // One character short of the bound, and a long value whose runs are each
    // short, are both ordinary values.
    assert_eq!(embedding_shape(None, &"A".repeat(BLOB_RUN_CHARS - 1)), None);
    let prose = "word ".repeat(BLOB_RUN_CHARS);
    assert_eq!(embedding_shape(None, &prose), None);
    assert_eq!(embedding_shape(Some("title"), "Weekly sync"), None);
    assert_eq!(EmbeddingShape::NamedField.as_str(), "voice_embedding_field");
    assert_eq!(EmbeddingShape::BinaryBlob.as_str(), "voice_embedding_blob");
}

// ---------------------------------------------------------------------------
// Honest labels (spec 7.9, 8.6A, 23)
// ---------------------------------------------------------------------------

#[test]
fn every_fidelity_class_has_its_own_source_relative_label() {
    let known = [
        "original_device_recording",
        "compressed_device_transfer",
        "vendor_cloud_export",
        "user_supplied_file",
        "local_derivative",
    ];
    let mut labels: Vec<&str> = known
        .iter()
        .map(|class| bundle::quality_label(class))
        .collect();
    for label in &labels {
        assert_ne!(*label, "");
    }
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), known.len(), "no two classes share a label");

    // A transfer is never presented as the original it was derived from.
    assert!(
        bundle::quality_label("compressed_device_transfer").contains("not the original device WAV")
    );
    // An unknown class claims nothing.
    assert_eq!(
        bundle::quality_label("something_new"),
        "Fidelity not established"
    );
    assert_eq!(bundle::quality_label(""), "Fidelity not established");
}

#[test]
fn alignment_labels_are_the_three_honest_values_and_nothing_else() {
    assert_eq!(bundle::alignment_label("native"), "native");
    assert_eq!(bundle::alignment_label("derived"), "derived (approximate)");
    assert_eq!(bundle::alignment_label("none"), "unavailable");
    // Anything unrecognised is treated as "no times", never as native.
    assert_eq!(bundle::alignment_label("forced"), "unavailable");
    assert_eq!(bundle::alignment_label(""), "unavailable");
}

#[test]
fn an_unconfirmed_name_is_never_rendered_as_identity() {
    assert!(bundle::speaker_display("Speaker 1", Some("Dana"), true).starts_with("Dana"));
    for name in [Some("Dana"), Some(""), None] {
        let display = bundle::speaker_display("Speaker 1", name, false);
        assert!(display.starts_with("Speaker 1"), "{display}");
        assert!(!display.contains("Dana"), "{display}");
    }
    // A confirmed-but-empty name falls back to the cluster label too.
    let empty = bundle::speaker_display("Speaker 2", Some(""), true);
    assert!(empty.starts_with("Speaker 2"), "{empty}");
}
