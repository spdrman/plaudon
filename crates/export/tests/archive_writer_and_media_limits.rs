//! Backup creation and media export at their edges (spec 13.6, 13.8, 7.8).
//!
//! Creation is the half of "archive accounting" that happens before a file
//! exists: a blob that changed under the writer, a staging area that cannot be
//! made, a declared length that does not match what streamed. None of those may
//! leave a `.recording-archive` behind, so every test asserts the destination
//! directory afterwards as well as the error.

use std::path::Path;

use plaudon_domain::media::MediaIdentity;
use plaudon_domain::time::Timestamp;
use plaudon_export::archive::crypto::KdfProfile;
use plaudon_export::archive::writer::{ArchiveBuilder, Protection, STAGING_PREFIX};
use plaudon_export::archive::{reader, ProtectionKind, DEFAULT_LIMITS};
use plaudon_export::destination::ExportDestination;
use plaudon_export::error::{ArchiveRejection, ExportError};
use plaudon_export::media::{self, SanitizableContainer};
use sha2::{Digest as _, Sha256};

const AT: Timestamp = Timestamp::from_millis(1_700_000_000_000);

fn builder(dir: &Path) -> ArchiveBuilder {
    let staging = dir.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    ArchiveBuilder::new(&staging, AT).unwrap()
}

fn destination(dir: &Path, name: &str) -> ExportDestination {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).unwrap();
    ExportDestination::new(&path).unwrap()
}

fn archives_in(directory: &Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".recording-archive")
        })
        .count()
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

#[test]
fn a_staging_base_that_is_not_a_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();

    let error = ArchiveBuilder::new(&file, AT).unwrap_err();
    assert!(
        matches!(error, ExportError::DestinationRejected(_)),
        "unexpected {error:?}"
    );
    assert!(STAGING_PREFIX.starts_with("plaudon-"));
}

#[test]
fn a_finished_builder_leaves_no_staging_directory_behind() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    {
        let mut builder = ArchiveBuilder::new(&staging, AT).unwrap();
        assert_eq!(builder.entry_count(), 0);
        builder
            .add_document("records/a.json", "archive-record/1", b"{}")
            .unwrap();
        assert_eq!(builder.entry_count(), 1);
        let out = destination(dir.path(), "out");
        let artifact = builder
            .finish(&out, "a.recording-archive", &Protection::Plaintext)
            .unwrap();
        assert_eq!(artifact.protection, ProtectionKind::Plaintext);
        assert_eq!(artifact.manifest.totals.entry_count, 1);
        assert_eq!(artifact.manifest.totals.byte_length, 2);
        assert!(artifact.file_bytes > 0);
        assert_eq!(
            std::fs::metadata(&artifact.path).unwrap().len(),
            artifact.file_bytes
        );
    }
    assert_eq!(
        std::fs::read_dir(&staging).unwrap().count(),
        0,
        "the private staging directory is removed with the builder"
    );
}

// ---------------------------------------------------------------------------
// Entry admission
// ---------------------------------------------------------------------------

#[test]
fn a_media_entry_beyond_the_per_entry_ceiling_is_refused_from_its_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = builder(dir.path());
    let source = dir.path().join("blob.wav");
    std::fs::write(&source, b"short").unwrap();

    let error = builder
        .add_media(
            "media/aa.wav",
            &source,
            &"a".repeat(64),
            DEFAULT_LIMITS.max_entry_bytes + 1,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        ExportError::ArchiveRejected(ArchiveRejection::SizeCeilingExceeded(_))
    ));
    assert_eq!(builder.entry_count(), 0);
}

#[test]
fn media_and_documents_share_one_duplicate_rule_and_one_namespace_rule() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = builder(dir.path());
    let source = dir.path().join("blob.wav");
    std::fs::write(&source, b"short").unwrap();

    builder
        .add_media("media/aa.wav", &source, &"a".repeat(64), 5)
        .unwrap();
    assert!(matches!(
        builder
            .add_media("media/AA.WAV", &source, &"a".repeat(64), 5)
            .unwrap_err(),
        ExportError::ArchiveRejected(ArchiveRejection::DuplicatePath(_))
    ));
    assert!(matches!(
        builder
            .add_media("blobs/aa.wav", &source, &"a".repeat(64), 5)
            .unwrap_err(),
        ExportError::ArchiveRejected(ArchiveRejection::PathRejected { .. })
    ));
    // A document cannot take a path a media entry already holds either.
    assert!(matches!(
        builder
            .add_document("media/aa.wav", "archive-record/1", b"{}")
            .unwrap_err(),
        ExportError::ArchiveRejected(ArchiveRejection::DuplicatePath(_))
    ));
    assert_eq!(builder.entry_count(), 1);
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

#[test]
fn a_library_blob_that_changed_between_planning_and_emission_fails_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("blob.wav");
    let bytes = b"RIFF....WAVE the planned bytes";
    std::fs::write(&source, bytes).unwrap();
    let digest = hex::encode(Sha256::digest(bytes));

    let mut builder = builder(dir.path());
    builder
        .add_media(
            &format!("media/{digest}.wav"),
            &source,
            &digest,
            bytes.len() as u64,
        )
        .unwrap();

    // Same length, different content: only the recomputed digest catches it.
    std::fs::write(&source, b"RIFF....WAVE the swapped bytes").unwrap();

    let out = destination(dir.path(), "out");
    let error = builder
        .finish(&out, "a.recording-archive", &Protection::Plaintext)
        .unwrap_err();

    assert!(matches!(error, ExportError::IntegrityFailed));
    assert_eq!(archives_in(out.directory()), 0, "no archive was committed");
}

#[test]
fn a_blob_whose_length_disagrees_with_its_declaration_fails_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("blob.wav");
    std::fs::write(&source, b"twelve bytes").unwrap();

    let mut builder = builder(dir.path());
    builder
        .add_media("media/aa.wav", &source, &"a".repeat(64), 999)
        .unwrap();

    let out = destination(dir.path(), "out");
    let error = builder
        .finish(&out, "a.recording-archive", &Protection::Plaintext)
        .unwrap_err();

    assert!(matches!(error, ExportError::IntegrityFailed));
    assert_eq!(archives_in(out.directory()), 0);
}

#[test]
fn a_blob_that_vanished_before_emission_fails_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("blob.wav");
    std::fs::write(&source, b"bytes").unwrap();

    let mut builder = builder(dir.path());
    builder
        .add_media("media/aa.wav", &source, &"a".repeat(64), 5)
        .unwrap();
    std::fs::remove_file(&source).unwrap();

    let out = destination(dir.path(), "out");
    let error = builder
        .finish(&out, "a.recording-archive", &Protection::Plaintext)
        .unwrap_err();

    assert!(matches!(error, ExportError::Io(_)), "unexpected {error:?}");
    assert_eq!(archives_in(out.directory()), 0);
}

#[test]
fn an_archive_with_no_entries_at_all_is_still_a_valid_container() {
    let dir = tempfile::tempdir().unwrap();
    let builder = builder(dir.path());
    let out = destination(dir.path(), "out");

    let artifact = builder
        .finish(&out, "empty.recording-archive", &Protection::Plaintext)
        .unwrap();
    assert_eq!(artifact.manifest.totals.entry_count, 0);

    let report = reader::inspect(&artifact.path, None, &dir.path().join("probe")).unwrap();
    assert!(report.payloads_verified);
    assert_eq!(report.manifest.entries.len(), 0);
}

#[test]
fn an_encrypted_archive_is_unreadable_without_the_password_it_was_written_with() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = builder(dir.path());
    builder
        .add_document("records/a.json", "archive-record/1", b"{\"a\":1}")
        .unwrap();
    let out = destination(dir.path(), "out");

    let artifact = builder
        .finish(
            &out,
            "sealed.recording-archive",
            &Protection::Encrypted {
                password: "a passphrase".to_owned(),
                profile: KdfProfile::Test,
            },
        )
        .unwrap();
    assert_eq!(artifact.protection, ProtectionKind::Encrypted);

    let probe = dir.path().join("probe");
    assert!(matches!(
        reader::inspect(&artifact.path, None, &probe).unwrap_err(),
        ExportError::PasswordRequired
    ));
    assert!(matches!(
        reader::inspect(&artifact.path, Some("a passphras"), &probe).unwrap_err(),
        ExportError::DecryptionFailed
    ));
    let report = reader::inspect(&artifact.path, Some("a passphrase"), &probe).unwrap();
    assert!(report.encrypted);
    assert!(report.payloads_verified);
}

#[test]
fn a_destination_whose_target_name_is_a_directory_refuses_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let builder = builder(dir.path());
    let out = destination(dir.path(), "out");
    std::fs::create_dir(out.directory().join("taken.recording-archive")).unwrap();

    let error = builder
        .finish(&out, "taken.recording-archive", &Protection::Plaintext)
        .unwrap_err();
    assert!(
        matches!(error, ExportError::DestinationRejected(_)),
        "unexpected {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Media sanitization
// ---------------------------------------------------------------------------

fn wav(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    for (id, payload) in chunks {
        body.extend_from_slice(*id);
        body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        body.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn fmt_chunk() -> Vec<u8> {
    vec![
        1, 0, 1, 0, 0x44, 0xAC, 0, 0, 0x88, 0x58, 0x01, 0, 2, 0, 16, 0,
    ]
}

#[test]
fn a_container_we_cannot_strip_is_detected_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();

    let ogg = dir.path().join("a.ogg");
    std::fs::write(&ogg, b"OggS\0\x02 and the rest").unwrap();
    assert!(matches!(
        SanitizableContainer::detect(&ogg).unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));

    let tiny = dir.path().join("tiny.bin");
    std::fs::write(&tiny, b"RI").unwrap();
    assert!(matches!(
        SanitizableContainer::detect(&tiny).unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));

    let empty = dir.path().join("empty.bin");
    std::fs::write(&empty, b"").unwrap();
    assert!(matches!(
        SanitizableContainer::detect(&empty).unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));

    assert!(matches!(
        SanitizableContainer::detect(&dir.path().join("absent.wav")).unwrap_err(),
        ExportError::Io(_)
    ));
}

#[test]
fn both_sanitizable_containers_are_detected_by_their_magic() {
    let dir = tempfile::tempdir().unwrap();

    let wav_path = dir.path().join("a.wav");
    std::fs::write(&wav_path, wav(&[(b"fmt ", fmt_chunk())])).unwrap();
    assert_eq!(
        SanitizableContainer::detect(&wav_path).unwrap(),
        SanitizableContainer::Wav
    );

    let tagged = dir.path().join("tagged.mp3");
    let mut bytes = b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
    std::fs::write(&tagged, &bytes).unwrap();
    assert_eq!(
        SanitizableContainer::detect(&tagged).unwrap(),
        SanitizableContainer::Mp3
    );

    let bare = dir.path().join("bare.mp3");
    std::fs::write(&bare, [0xFFu8, 0xFB, 0x90, 0x00]).unwrap();
    assert_eq!(
        SanitizableContainer::detect(&bare).unwrap(),
        SanitizableContainer::Mp3
    );
}

#[test]
fn a_wav_without_audio_data_is_refused_rather_than_emptied() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");

    let no_data = dir.path().join("no-data.wav");
    std::fs::write(&no_data, wav(&[(b"fmt ", fmt_chunk())])).unwrap();
    assert!(matches!(
        media::export_sanitized(&no_data, &out, "a.wav").unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));

    // A header with no chunks at all is the same refusal.
    let header_only = dir.path().join("header.wav");
    std::fs::write(&header_only, wav(&[])).unwrap();
    assert!(matches!(
        media::export_sanitized(&header_only, &out, "b.wav").unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));

    // A file that stops inside a chunk header is damage, not a container we
    // can rewrite.
    let truncated = dir.path().join("truncated.wav");
    let mut bytes = wav(&[(b"fmt ", fmt_chunk())]);
    bytes.extend_from_slice(b"dat");
    std::fs::write(&truncated, &bytes).unwrap();
    assert!(matches!(
        media::export_sanitized(&truncated, &out, "c.wav").unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));

    assert_eq!(std::fs::read_dir(out.directory()).unwrap().count(), 0);
}

#[test]
fn a_wav_keeps_its_structural_chunks_and_loses_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");
    let data: Vec<u8> = (0u16..64).flat_map(u16::to_le_bytes).collect();
    let source = dir.path().join("meeting.wav");
    let original = wav(&[
        (b"fmt ", fmt_chunk()),
        (b"LIST", b"INFOISFTSERIAL-0001".to_vec()),
        (b"fact", 64u32.to_le_bytes().to_vec()),
        (b"data", data.clone()),
        (b"id3 ", b"a trailing tag".to_vec()),
    ]);
    std::fs::write(&source, &original).unwrap();

    let report = media::export_sanitized(&source, &out, "meeting.wav").unwrap();
    assert!(!report.is_original);
    assert_eq!(
        report.stripped_metadata,
        vec!["LIST".to_owned(), "id3".to_owned()]
    );
    assert!(report.label.contains("not the original file"));

    let produced = std::fs::read(&report.path).unwrap();
    assert_eq!(report.byte_length, produced.len() as u64);
    assert_eq!(report.sha256, hex::encode(Sha256::digest(&produced)));
    assert!(produced.starts_with(b"RIFF"));
    assert!(!produced.windows(4).any(|w| w == b"LIST"));
    assert!(produced.windows(4).any(|w| w == b"fact"));
    // The audio samples survive byte for byte.
    assert!(produced
        .windows(data.len())
        .any(|window| window == data.as_slice()));
    // The stored original is untouched.
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn an_mp3_loses_both_id3_tags_and_keeps_its_frames() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");

    let frames: Vec<u8> = (0..256u32).map(|i| (i % 251) as u8).collect();
    let tag_body = vec![b'T'; 32];
    let mut source_bytes = Vec::new();
    source_bytes.extend_from_slice(b"ID3\x03\x00\x00");
    // Synchsafe length of the tag body.
    source_bytes.extend_from_slice(&[0, 0, 0, tag_body.len() as u8]);
    source_bytes.extend_from_slice(&tag_body);
    source_bytes.extend_from_slice(&frames);
    source_bytes.extend_from_slice(b"TAG");
    source_bytes.extend_from_slice(&[0u8; 125]);

    let source = dir.path().join("talk.mp3");
    std::fs::write(&source, &source_bytes).unwrap();

    let report = media::export_sanitized(&source, &out, "talk.mp3").unwrap();
    assert_eq!(
        report.stripped_metadata,
        vec!["ID3v2".to_owned(), "ID3v1".to_owned()]
    );
    assert_eq!(std::fs::read(&report.path).unwrap(), frames);
    assert_eq!(report.byte_length, frames.len() as u64);

    // An mp3 with no tags at all is copied whole and reports nothing stripped.
    let bare = dir.path().join("bare.mp3");
    let mut bare_bytes = vec![0xFFu8, 0xFB, 0x90, 0x00];
    bare_bytes.extend_from_slice(&frames);
    std::fs::write(&bare, &bare_bytes).unwrap();
    let bare_report = media::export_sanitized(&bare, &out, "bare.mp3").unwrap();
    assert_eq!(bare_report.stripped_metadata, Vec::<String>::new());
    assert_eq!(std::fs::read(&bare_report.path).unwrap(), bare_bytes);
}

#[test]
fn an_id3_tag_longer_than_the_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");
    let source = dir.path().join("lying.mp3");
    let mut bytes = b"ID3\x03\x00\x00".to_vec();
    // A synchsafe length far beyond the file's size.
    bytes.extend_from_slice(&[0x7f, 0x7f, 0x7f, 0x7f]);
    bytes.extend_from_slice(&[0u8; 32]);
    std::fs::write(&source, &bytes).unwrap();

    assert!(matches!(
        media::export_sanitized(&source, &out, "lying.mp3").unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));
    assert_eq!(std::fs::read_dir(out.directory()).unwrap().count(), 0);
}

#[test]
fn an_original_that_does_not_match_its_identity_is_removed_rather_than_kept() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");
    let source = dir.path().join("blob.wav");
    std::fs::write(&source, b"the real bytes").unwrap();

    let wrong = MediaIdentity::of_bytes(b"different bytes entirely", "wav");
    let error =
        media::export_original(&source, &wrong, &out, "meeting.wav", "Original").unwrap_err();
    assert!(matches!(error, ExportError::IntegrityFailed));
    assert_eq!(std::fs::read_dir(out.directory()).unwrap().count(), 0);

    // A missing source is a storage failure, not an integrity failure.
    let identity = MediaIdentity::of_bytes(b"the real bytes", "wav");
    let missing = media::export_original(
        &dir.path().join("absent.wav"),
        &identity,
        &out,
        "meeting.wav",
        "Original",
    )
    .unwrap_err();
    assert!(matches!(missing, ExportError::Io(_)));

    let report =
        media::export_original(&source, &identity, &out, "meeting.wav", "Original").unwrap();
    assert!(report.is_original);
    assert_eq!(report.byte_length, 14);
    assert_eq!(report.stripped_metadata, Vec::<String>::new());
    assert_eq!(std::fs::read(&report.path).unwrap(), b"the real bytes");
}

#[test]
fn an_original_whose_declared_length_is_wrong_is_removed_even_when_its_digest_matches() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");
    let source = dir.path().join("blob.wav");
    std::fs::write(&source, b"the real bytes").unwrap();

    // The digest of the copied bytes agrees; the length the asset row declares
    // does not. The two facts have to agree or the export is not the asset it
    // claims to be.
    let mut identity = MediaIdentity::of_bytes(b"the real bytes", "wav");
    identity.byte_length += 1;

    let error =
        media::export_original(&source, &identity, &out, "meeting.wav", "Original").unwrap_err();
    assert!(matches!(error, ExportError::IntegrityFailed));
    assert_eq!(std::fs::read_dir(out.directory()).unwrap().count(), 0);
}

#[test]
fn a_riff_file_that_is_not_wave_is_not_sanitized_as_a_wav() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");

    // RIFF, twelve readable bytes, but the form type is not WAVE: we do not
    // know which chunks carry audio, so we refuse instead of rewriting it.
    let avi = dir.path().join("clip.avi");
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&64u32.to_le_bytes());
    bytes.extend_from_slice(b"AVI LIST\0\0\0\0");
    std::fs::write(&avi, &bytes).unwrap();

    assert!(matches!(
        SanitizableContainer::detect(&avi).unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));
    assert!(matches!(
        media::export_sanitized(&avi, &out, "clip.avi").unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));
    assert_eq!(std::fs::read_dir(out.directory()).unwrap().count(), 0);
}

#[test]
fn a_leading_sync_byte_without_an_mpeg_sync_word_is_not_sanitized_as_an_mp3() {
    let dir = tempfile::tempdir().unwrap();

    // 0xFF alone is not an MPEG frame: the following three bits have to be set
    // as well, or any binary blob starting with a 0xFF byte would be treated
    // as audio we can strip.
    let blob = dir.path().join("blob.bin");
    std::fs::write(&blob, [0xFFu8, 0x0F, 0x90, 0x00, 0x11]).unwrap();

    assert!(matches!(
        SanitizableContainer::detect(&blob).unwrap_err(),
        ExportError::SanitizationUnsupported(_)
    ));
}

#[test]
fn a_wav_chunk_that_runs_past_the_end_of_the_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");

    // A `data` chunk declaring a kilobyte with eight bytes behind it. The
    // chunk list still scans, so the refusal has to come from the copy.
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&1_060u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&fmt_chunk());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&1_024u32.to_le_bytes());
    bytes.extend_from_slice(&[7u8; 8]);
    let source = dir.path().join("truncated.wav");
    std::fs::write(&source, &bytes).unwrap();

    let error = media::export_sanitized(&source, &out, "truncated.wav").unwrap_err();
    assert!(matches!(error, ExportError::SanitizationUnsupported(_)));
    assert_eq!(
        std::fs::read_dir(out.directory()).unwrap().count(),
        0,
        "a partial derivative is never left behind"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        bytes,
        "the source is read-only"
    );
}

#[test]
fn an_odd_length_audio_chunk_keeps_its_riff_pad_byte() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");
    // RIFF requires a pad byte after an odd-length chunk. Dropping it would
    // move every following chunk by one byte and make the derivative unreadable.
    let original = wav(&[(b"fmt ", fmt_chunk()), (b"data", vec![1, 2, 3])]);
    let source = dir.path().join("odd.wav");
    std::fs::write(&source, &original).unwrap();

    let report = media::export_sanitized(&source, &out, "odd.wav").unwrap();

    let produced = std::fs::read(&report.path).unwrap();
    assert_eq!(report.stripped_metadata, Vec::<String>::new());
    assert_eq!(produced, original, "nothing to strip, nothing to change");
    assert_eq!(report.byte_length, 48);
    assert_eq!(produced[produced.len() - 1], 0, "the pad byte survives");
}

#[test]
fn a_file_too_short_to_carry_an_id3_header_is_copied_without_claiming_a_strip() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");
    // Three bytes: enough to be detected as MPEG audio, too few to hold either
    // an ID3v2 header (ten bytes) or an ID3v1 trailer (128).
    let source = dir.path().join("stub.mp3");
    std::fs::write(&source, b"ID3").unwrap();

    let report = media::export_sanitized(&source, &out, "stub.mp3").unwrap();

    assert_eq!(report.stripped_metadata, Vec::<String>::new());
    assert_eq!(report.byte_length, 3);
    assert_eq!(std::fs::read(&report.path).unwrap(), b"ID3");
}

#[test]
fn an_id3_tag_past_the_metadata_ceiling_is_refused_even_when_the_file_is_long_enough() {
    let dir = tempfile::tempdir().unwrap();
    let out = destination(dir.path(), "out");

    // One byte past the 8 MiB we are willing to skip over, inside a file long
    // enough for the length to be self-consistent: the ceiling, not the file
    // size, is what refuses this.
    let declared: u64 = 8 * 1024 * 1024 + 1;
    let mut header = b"ID3\x03\x00\x00".to_vec();
    header.extend_from_slice(&synchsafe(declared));

    let source = dir.path().join("fat-tag.mp3");
    {
        let mut file = std::fs::File::create(&source).unwrap();
        std::io::Write::write_all(&mut file, &header).unwrap();
        file.set_len(9 * 1024 * 1024).unwrap();
    }

    let error = media::export_sanitized(&source, &out, "fat-tag.mp3").unwrap_err();
    assert!(matches!(error, ExportError::SanitizationUnsupported(_)));
    assert_eq!(std::fs::read_dir(out.directory()).unwrap().count(), 0);
}

/// ID3v2 lengths are seven bits per byte, with the high bit always clear.
fn synchsafe(size: u64) -> [u8; 4] {
    [
        u8::try_from((size >> 21) & 0x7f).unwrap(),
        u8::try_from((size >> 14) & 0x7f).unwrap(),
        u8::try_from((size >> 7) & 0x7f).unwrap(),
        u8::try_from(size & 0x7f).unwrap(),
    ]
}

#[test]
fn declared_entry_lengths_that_exceed_the_archive_total_are_refused_from_the_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = builder(dir.path());
    // The blob is never opened: the ceilings are enforced from what the caller
    // declares, before a single byte is staged or read.
    let blob = dir.path().join("library-blob.wav");
    let digest = "a".repeat(64);

    let per_entry = DEFAULT_LIMITS.max_entry_bytes;
    let allowed = DEFAULT_LIMITS.max_total_bytes / per_entry;
    for index in 0..allowed {
        builder
            .add_media(&format!("media/{index:04}.wav"), &blob, &digest, per_entry)
            .unwrap();
    }
    assert_eq!(u64::try_from(builder.entry_count()).unwrap(), allowed);

    let error = builder
        .add_media("media/one-too-many.wav", &blob, &digest, per_entry)
        .unwrap_err();
    match error {
        ExportError::ArchiveRejected(ArchiveRejection::SizeCeilingExceeded(detail)) => {
            assert_eq!(detail, "archive total is too large");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        u64::try_from(builder.entry_count()).unwrap(),
        allowed,
        "the refused entry is not staged"
    );
}
