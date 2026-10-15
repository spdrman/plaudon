//! Rejection, I/O-failure and boundary paths of the content-addressed archive
//! (spec 13.4, 6.3B).
//!
//! The unit tests inside `archive.rs` cover the happy path. These drive the
//! other side of every validation: hostile paths, blobs that are not regular
//! files, commits whose destination cannot be created, a destination volume
//! that cannot hold the archive, a copy that does not reproduce the bytes, and
//! readers that fail part way through. Every rejection asserts both the
//! specific error variant and that no blob was registered.

use std::io::Read;
use std::path::PathBuf;

use plaudon_domain::ids::ImportIntentId;
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use plaudon_persistence::archive::{
    relative_path, sanitize_extension, MediaArchive, ALLOWED_EXTENSIONS, FALLBACK_EXTENSION,
};
use plaudon_persistence::error::PersistenceError;

/// An archive rooted in a subdirectory, so a test can place files beside it
/// that are genuinely outside the archive root.
fn archive() -> (tempfile::TempDir, MediaArchive) {
    let dir = tempfile::tempdir().unwrap();
    let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
    (dir, archive)
}

/// Stage and commit `bytes`, returning the archive-relative path and identity.
fn commit_bytes(archive: &MediaArchive, bytes: &[u8]) -> (String, MediaIdentity) {
    let staging = archive.staging_path(ImportIntentId::new());
    let mut source = std::io::Cursor::new(bytes.to_vec());
    let identity = archive
        .stage_from_reader(&staging, &mut source, "wav")
        .unwrap();
    let relative = relative_path(&identity, "wav");
    archive.commit_staged(&staging, &relative).unwrap();
    (relative, identity)
}

/// The shard directory a blob lives in.
fn shard_of(archive: &MediaArchive, relative: &str) -> PathBuf {
    archive
        .absolute(relative)
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A reader that hands out `remaining` bytes and then fails, standing in for a
/// disconnected device or a truncated download.
#[derive(Debug)]
struct FailsAfter {
    remaining: usize,
}

impl Read for FailsAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the source device went away",
            ));
        }
        let take = self.remaining.min(buf.len());
        buf[..take].fill(b'a');
        self.remaining -= take;
        Ok(take)
    }
}

#[test]
fn absolute_names_the_reason_a_path_is_not_archive_relative() {
    let (_dir, archive) = archive();
    let cases = [
        ("", "empty path"),
        ("media/sha256/ab/x\0.wav", "NUL"),
        ("./media/sha256/ab/x.wav", "current-directory"),
        ("media/../../outside.wav", "escapes the archive root"),
        ("/etc/passwd", "not archive-relative"),
    ];
    for (hostile, fragment) in cases {
        match archive.absolute(hostile) {
            Err(PersistenceError::ArchivePathRejected(message)) => assert!(
                message.contains(fragment),
                "{hostile:?} was rejected with {message:?}, expected {fragment:?}"
            ),
            other => panic!("{hostile:?} was not rejected: {other:?}"),
        }
        assert!(
            matches!(
                archive.exists(hostile),
                Err(PersistenceError::ArchivePathRejected(_))
            ),
            "exists({hostile:?}) must refuse the same path"
        );
    }
    assert_eq!(archive.list_blobs().unwrap(), Vec::new());
}

#[test]
fn staging_paths_outside_the_staging_directory_are_refused_by_every_entry_point() {
    let (dir, archive) = archive();
    let elsewhere = [
        dir.path().join("loose.part"),
        archive.root().join("loose.part"),
        archive.root().join("media/sha256/loose.part"),
        archive.staging_dir().join("nested").join("deep.part"),
        PathBuf::from("bare.part"),
    ];
    for path in elsewhere {
        let mut source = std::io::Cursor::new(b"audio".to_vec());
        assert!(
            matches!(
                archive.stage_from_reader(&path, &mut source, "wav"),
                Err(PersistenceError::ArchivePathRejected(_))
            ),
            "staging accepted {path:?}"
        );
        assert!(
            matches!(
                archive.commit_staged(&path, "media/sha256/ab/blob.wav"),
                Err(PersistenceError::ArchivePathRejected(_))
            ),
            "committing accepted {path:?}"
        );
        assert!(!path.exists(), "{path:?} must never have been created");
    }
    assert_eq!(archive.list_blobs().unwrap(), Vec::new());
}

#[test]
fn verification_rejects_a_wrong_length_and_a_wrong_digest_independently() {
    let (_dir, archive) = archive();
    let (relative, identity) = commit_bytes(&archive, b"authentic bytes");

    // Right digest, wrong length: the length check alone must refuse.
    let wrong_length = MediaIdentity::new(identity.sha256, identity.byte_length + 1, "wav");
    assert!(matches!(
        archive.read_verified(&relative, &wrong_length),
        Err(PersistenceError::IntegrityFailed)
    ));
    assert!(matches!(
        archive.verify(&relative, &wrong_length),
        Err(PersistenceError::IntegrityFailed)
    ));

    // Right length, wrong digest: the digest check alone must refuse.
    let wrong_digest = MediaIdentity::new(
        Sha256Digest::of(b"a different payload entirely"),
        identity.byte_length,
        "wav",
    );
    assert!(matches!(
        archive.read_verified(&relative, &wrong_digest),
        Err(PersistenceError::IntegrityFailed)
    ));
    assert!(matches!(
        archive.verify(&relative, &wrong_digest),
        Err(PersistenceError::IntegrityFailed)
    ));

    // The genuine identity still reads.
    assert_eq!(
        archive.read_verified(&relative, &identity).unwrap(),
        b"authentic bytes"
    );
    archive.verify(&relative, &identity).unwrap();
}

#[test]
fn reads_refuse_a_missing_entry_and_an_entry_that_is_not_a_regular_file() {
    let (_dir, archive) = archive();
    let identity = MediaIdentity::of_bytes(b"never stored", "wav");
    let relative = relative_path(&identity, "wav");

    assert!(!archive.exists(&relative).unwrap());
    assert!(matches!(
        archive.read_verified(&relative, &identity),
        Err(PersistenceError::Io(_))
    ));
    assert!(matches!(
        archive.verify(&relative, &identity),
        Err(PersistenceError::Io(_))
    ));

    // A directory standing where a blob should be is a policy rejection, not
    // an integrity failure: nothing was read.
    std::fs::create_dir_all(archive.absolute(&relative).unwrap()).unwrap();
    for outcome in [
        archive.read_verified(&relative, &identity),
        archive.verify(&relative, &identity).map(|()| Vec::new()),
    ] {
        match outcome {
            Err(PersistenceError::ArchivePathRejected(message)) => assert!(
                message.contains("not a regular file"),
                "unexpected message {message:?}"
            ),
            other => panic!("a directory was not refused: {other:?}"),
        }
    }
    assert_eq!(archive.list_blobs().unwrap(), Vec::new());
}

#[cfg(unix)]
#[test]
fn a_symlinked_ancestor_cannot_smuggle_bytes_in_from_outside_the_root() {
    let (_dir, archive) = archive();
    let outside = tempfile::tempdir().unwrap();

    let identity = MediaIdentity::of_bytes(b"bytes from outside the root", "wav");
    let relative = relative_path(&identity, "wav");
    let file_name = relative.rsplit('/').next().unwrap().to_owned();
    std::fs::write(
        outside.path().join(&file_name),
        b"bytes from outside the root",
    )
    .unwrap();

    // The shard directory itself is the symlink, so the blob is a perfectly
    // ordinary regular file and only canonicalisation catches the escape.
    std::os::unix::fs::symlink(outside.path(), shard_of(&archive, &relative)).unwrap();

    match archive.read_verified(&relative, &identity) {
        Err(PersistenceError::ArchivePathRejected(message)) => assert!(
            message.contains("escapes the archive root"),
            "unexpected message {message:?}"
        ),
        other => panic!("a symlinked ancestor was not refused: {other:?}"),
    }
}

#[test]
fn orphan_detection_ignores_directories_foreign_names_and_live_intents() {
    let (_dir, archive) = archive();
    let live = ImportIntentId::new();
    let dead = ImportIntentId::new();
    std::fs::write(archive.staging_path(live), b"live").unwrap();
    std::fs::write(archive.staging_path(dead), b"dead").unwrap();
    let decoy_dir = archive.staging_dir().join("looks-like-ours.part");
    std::fs::create_dir_all(&decoy_dir).unwrap();
    let no_extension = archive.staging_dir().join("README");
    std::fs::write(&no_extension, b"user notes").unwrap();
    let foreign_part = archive.staging_dir().join("not-an-intent-id.part");
    std::fs::write(&foreign_part, b"someone else's").unwrap();

    assert_eq!(
        archive.orphan_staging_files(&[live]).unwrap(),
        vec![archive.staging_path(dead)]
    );
    assert!(decoy_dir.is_dir(), "a directory is never an orphan file");
    assert!(no_extension.is_file());
    assert!(foreign_part.is_file());

    // Boundary: with no live intents every app-owned file is an orphan, and
    // the result is sorted.
    let mut expected = vec![archive.staging_path(live), archive.staging_path(dead)];
    expected.sort();
    assert_eq!(archive.orphan_staging_files(&[]).unwrap(), expected);
}

#[test]
fn a_missing_staging_directory_reports_no_orphans() {
    let (_dir, archive) = archive();
    std::fs::remove_dir_all(archive.staging_dir()).unwrap();
    assert_eq!(
        archive.orphan_staging_files(&[]).unwrap(),
        Vec::<PathBuf>::new()
    );
}

#[test]
fn a_commit_whose_shard_cannot_be_created_keeps_the_staged_bytes() {
    let (_dir, archive) = archive();
    let staging = archive.staging_path(ImportIntentId::new());
    let mut source = std::io::Cursor::new(b"audio".to_vec());
    let identity = archive
        .stage_from_reader(&staging, &mut source, "wav")
        .unwrap();
    let relative = relative_path(&identity, "wav");

    // A regular file sits where the two-hex shard directory must go, so
    // `create_dir_all` fails with `ENOTDIR` for every user, root included.
    std::fs::write(shard_of(&archive, &relative), b"not a directory").unwrap();

    assert!(matches!(
        archive.commit_staged(&staging, &relative),
        Err(PersistenceError::Io(_))
    ));
    assert!(
        staging.is_file(),
        "the staged bytes survive so the import can be retried"
    );
    assert_eq!(archive.list_blobs().unwrap(), Vec::new());
    assert!(!archive.exists(&relative).unwrap());
}

#[test]
fn a_rename_failure_that_is_not_a_cross_volume_move_is_reported_as_io() {
    let (_dir, archive) = archive();
    let staging = archive.staging_path(ImportIntentId::new());
    let mut source = std::io::Cursor::new(b"audio".to_vec());
    let identity = archive
        .stage_from_reader(&staging, &mut source, "wav")
        .unwrap();
    let relative = relative_path(&identity, "wav");

    // The destination name is taken by a non-empty directory: the rename
    // fails, and the failure must not be reclassified as a cross-volume move.
    std::fs::create_dir_all(archive.absolute(&relative).unwrap().join("occupied")).unwrap();

    let error = archive.commit_staged(&staging, &relative).unwrap_err();
    assert!(
        matches!(error, PersistenceError::Io(_)),
        "unexpected error {error:?}"
    );
    assert!(staging.is_file(), "the staged bytes are not lost");
    assert_eq!(archive.list_blobs().unwrap(), Vec::new());
}

#[test]
fn relocation_refuses_a_destination_that_cannot_hold_the_blobs() {
    let (_dir, archive) = archive();
    let (relative, _) = commit_bytes(&archive, b"small blob");

    // A sparse blob larger than any real volume. The pre-flight space check
    // must refuse before a single byte is copied.
    let blob = std::fs::OpenOptions::new()
        .write(true)
        .open(archive.absolute(&relative).unwrap())
        .unwrap();
    blob.set_len(1 << 50).unwrap();
    drop(blob);

    let destination = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive.relocate_to(destination.path()),
        Err(PersistenceError::StorageFull)
    ));
    assert_eq!(
        MediaArchive::open(destination.path())
            .unwrap()
            .list_blobs()
            .unwrap(),
        Vec::new(),
        "a refused relocation copies nothing"
    );
}

#[cfg(unix)]
#[test]
fn a_blob_that_changes_under_the_copy_leaves_nothing_at_the_destination() {
    let (dir, archive) = archive();
    let (relative, _) = commit_bytes(&archive, b"bytes that will not survive the copy");

    // The destination entry is a symlink back to the source blob, so the copy
    // truncates the very file it was told to reproduce: the verification read
    // afterwards sees different bytes than the ones that were hashed.
    let destination = dir.path().join("moved");
    let target = destination.join(&relative);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(archive.absolute(&relative).unwrap(), &target).unwrap();

    assert!(matches!(
        archive.relocate_to(&destination),
        Err(PersistenceError::IntegrityFailed)
    ));
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "an unverified destination entry must be removed, not left to be read later"
    );
}

#[test]
fn listing_blobs_skips_stray_files_and_nested_directories() {
    let (_dir, archive) = archive();
    let (relative, identity) = commit_bytes(&archive, b"a real blob");
    std::fs::write(
        archive.root().join("media/sha256").join("stray.txt"),
        b"not a shard",
    )
    .unwrap();
    std::fs::create_dir_all(shard_of(&archive, &relative).join("nested")).unwrap();

    assert_eq!(
        archive.list_blobs().unwrap(),
        vec![(relative, identity.byte_length)]
    );
}

#[test]
fn an_archive_with_no_media_directory_lists_and_relocates_nothing() {
    let (_dir, archive) = archive();
    std::fs::remove_dir_all(archive.root().join("media/sha256")).unwrap();
    assert_eq!(archive.list_blobs().unwrap(), Vec::new());

    let destination = tempfile::tempdir().unwrap();
    let report = archive.relocate_to(destination.path()).unwrap();
    assert_eq!(report.blobs_moved, 0);
    assert_eq!(report.bytes_moved, 0);
    assert_eq!(report.new_root, destination.path());
}

#[test]
fn hashing_a_missing_file_fails_instead_of_returning_an_empty_digest() {
    let (dir, _archive) = archive();
    assert!(matches!(
        MediaArchive::digest_file(&dir.path().join("absent.wav")),
        Err(PersistenceError::Io(_))
    ));

    let empty = dir.path().join("empty.wav");
    std::fs::write(&empty, b"").unwrap();
    let (digest, length) = MediaArchive::digest_file(&empty).unwrap();
    assert_eq!(length, 0);
    assert_eq!(digest, Sha256Digest::of(b""));
}

#[test]
fn a_reader_that_fails_mid_stream_produces_no_blob() {
    let (_dir, archive) = archive();
    let staging = archive.staging_path(ImportIntentId::new());
    let mut source = FailsAfter { remaining: 1024 };

    let error = archive
        .stage_from_reader(&staging, &mut source, "wav")
        .unwrap_err();
    assert!(
        matches!(error, PersistenceError::Io(_)),
        "unexpected error {error:?}"
    );
    assert_eq!(archive.list_blobs().unwrap(), Vec::new());
    assert_eq!(
        archive.orphan_staging_files(&[]).unwrap(),
        vec![staging],
        "the partial file is left for the reconciler to sweep"
    );
}

#[test]
fn staging_handles_an_empty_reader_and_a_payload_larger_than_one_chunk() {
    let (_dir, archive) = archive();

    let (empty_relative, empty_identity) = commit_bytes(&archive, b"");
    assert_eq!(empty_identity.byte_length, 0);
    assert_eq!(empty_identity, MediaIdentity::of_bytes(b"", "wav"));
    assert_eq!(
        archive
            .read_verified(&empty_relative, &empty_identity)
            .unwrap(),
        Vec::<u8>::new()
    );

    // One byte past the 64 KiB read buffer: a second loop iteration plus a
    // short final read.
    let payload = vec![7u8; 64 * 1024 + 1];
    let (relative, identity) = commit_bytes(&archive, &payload);
    assert_eq!(identity.byte_length, payload.len() as u64);
    assert_eq!(
        archive.read_verified(&relative, &identity).unwrap(),
        payload
    );
}

#[test]
fn extension_sanitisation_folds_case_and_admits_nothing_else() {
    for allowed in ALLOWED_EXTENSIONS {
        assert_eq!(sanitize_extension(allowed), *allowed);
        assert_eq!(sanitize_extension(&allowed.to_ascii_uppercase()), *allowed);
    }
    assert_eq!(sanitize_extension("  .M4A  "), "m4a");
    assert_eq!(sanitize_extension("...wav"), "wav");
    for rejected in ["wav.exe", "", ".", "waves", "w av", "🎵"] {
        assert_eq!(
            sanitize_extension(rejected),
            FALLBACK_EXTENSION,
            "{rejected:?} must not survive"
        );
    }

    // Whatever the caller passes, the stored path stays inside the shard.
    let identity = MediaIdentity::of_bytes(b"x", "wav");
    let hostile = relative_path(&identity, "../../etc/passwd");
    assert_eq!(
        hostile.rsplit_once('.').map(|(_, extension)| extension),
        Some(FALLBACK_EXTENSION),
        "{hostile}"
    );
    assert!(hostile.starts_with("media/sha256/"), "{hostile}");
}
