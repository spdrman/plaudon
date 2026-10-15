//! Private staging directories and the §6.3B commit, on the paths a successful
//! import never takes (§16.5, §6.3B, §13.4).
//!
//! The inline tests in `src/fsguard/tests.rs` cover the happy stage-and-commit
//! and the reconciler. This suite covers the refusals: a staging base that
//! cannot be created, a name that does not fit once the `.part` suffix is
//! added, a prefix outside the reconciler's own vocabulary, and the three
//! shapes a commit target may have been turned into between validation and
//! rename — a symlink, a directory, and an existing file.

use std::io::Write;
use std::path::Path;

use plaudon_security::fsguard::{
    canonicalize_and_validate, is_restrictive, same_volume, set_restrictive_permissions, volume_id,
    PathPolicy, PathRejection, PrivateTempDir, ReconcileReport, VolumeId, STAGED_SUFFIX,
};

fn staged_bytes() -> &'static [u8] {
    b"RIFF----WAVEfmt staged-bytes"
}

/// A staging directory reports the prefix it was created with and a policy
/// whose only root is itself, so nothing validated through it can land outside.
#[test]
fn a_staging_directory_is_its_own_single_permitted_root() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = PrivateTempDir::create(Some(base.path()), "plaudon-import").expect("staging");

    assert_eq!(staging.prefix(), "plaudon-import");
    assert!(staging
        .path()
        .starts_with(std::fs::canonicalize(base.path()).expect("canonical base")));
    assert_eq!(is_restrictive(staging.path()), Some(true));

    let policy = staging.policy();
    assert_eq!(policy.roots.len(), 1);
    assert_eq!(policy.base(), Some(staging.path()));
    assert!(!policy.follow_symlinks, "staging never follows an escape");

    assert_eq!(
        canonicalize_and_validate(&policy, base.path())
            .unwrap_err()
            .reason(),
        PathRejection::OutsideRoot,
        "the parent of the staging directory is outside it"
    );

    let path = staging.path().to_path_buf();
    drop(staging);
    assert!(!path.exists(), "the staging directory is removed on drop");
}

/// The process prefix is a closed vocabulary; everything else is refused before
/// a directory is created or an entry is removed.
#[test]
fn a_hostile_staging_prefix_is_refused_by_both_entry_points() {
    let base = tempfile::tempdir().expect("temp base");

    let cases: [(&str, PathRejection); 6] = [
        ("", PathRejection::ComponentTooLong),
        (&"p".repeat(33), PathRejection::ComponentTooLong),
        ("../escape", PathRejection::ReservedCharacter),
        ("a/b", PathRejection::ReservedCharacter),
        ("a b", PathRejection::ReservedCharacter),
        ("a\0b", PathRejection::ReservedCharacter),
    ];
    for (prefix, expected) in cases {
        assert_eq!(
            PrivateTempDir::create(Some(base.path()), prefix)
                .unwrap_err()
                .reason(),
            expected,
            "create accepted {prefix:?}"
        );
        assert_eq!(
            PrivateTempDir::reconcile_abandoned(base.path(), prefix, 0, 0)
                .unwrap_err()
                .reason(),
            expected,
            "reconcile accepted {prefix:?}"
        );
    }

    assert!(PrivateTempDir::create(Some(base.path()), &"p".repeat(32)).is_ok());

    // A base that is an existing *file* cannot be turned into a directory.
    let file_base = base.path().join("not-a-directory");
    std::fs::write(&file_base, b"x").expect("write");
    assert_eq!(
        PrivateTempDir::create(Some(&file_base), "plaudon")
            .unwrap_err()
            .reason(),
        PathRejection::Io
    );

    // A missing base is simply nothing to reconcile, not an error.
    let report =
        PrivateTempDir::reconcile_abandoned(&base.path().join("never-existed"), "plaudon", 0, 0)
            .expect("a missing base is a no-op");
    assert_eq!(report, ReconcileReport::default());
}

/// A staged name must still fit in one path component once the `.part` suffix
/// is appended, and the refusal names the measured length.
#[test]
fn a_staged_name_that_does_not_fit_with_its_suffix_is_refused() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = PrivateTempDir::create(Some(base.path()), "plaudon").expect("staging");

    // 255 bytes passes `check_component` but not once `.part` is appended.
    let name = "n".repeat(255);
    let error = staging.stage_file(&name).unwrap_err();
    assert_eq!(error.reason(), PathRejection::ComponentTooLong);
    assert_eq!(
        error.detail(),
        Some(format!("{} bytes", 255 + STAGED_SUFFIX.len()).as_str())
    );

    // The name rules apply before the length rule.
    assert_eq!(
        staging.stage_file("a/b").unwrap_err().reason(),
        PathRejection::ReservedCharacter
    );
    assert_eq!(
        staging.stage_file("").unwrap_err().reason(),
        PathRejection::Empty
    );

    let fits = "f".repeat(255 - STAGED_SUFFIX.len());
    let staged = staging.stage_file(&fits).expect("a name that fits");
    assert!(staged
        .path()
        .as_path()
        .to_string_lossy()
        .ends_with(STAGED_SUFFIX));
}

/// Staging the same name twice is refused rather than reopening the first
/// file, because `O_CREAT|O_EXCL` is the half of the symlink-race defence that
/// lives in the staging directory.
#[test]
fn staging_the_same_name_twice_is_refused() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = PrivateTempDir::create(Some(base.path()), "plaudon").expect("staging");

    let first = staging.stage_file("input").expect("first stage");
    let error = staging.stage_file("input").unwrap_err();
    assert_eq!(error.reason(), PathRejection::Io);
    assert_eq!(error.detail(), Some("AlreadyExists"));
    drop(first);

    // Once the first staged file is gone the name is free again.
    assert!(staging.stage_file("input").is_ok());
}

/// A staged file can be written, checkpointed and abandoned; abandoning removes
/// the partial bytes immediately rather than at drop.
#[test]
fn a_staged_file_can_be_checkpointed_and_abandoned() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = PrivateTempDir::create(Some(base.path()), "plaudon").expect("staging");

    let mut staged = staging.stage_file("download").expect("stage");
    staged.write_all(staged_bytes()).expect("write");
    staged.flush().expect("flush");
    staged.sync().expect("checkpoint fsync");

    let partial = staged.path().as_path().to_path_buf();
    assert!(partial.exists());
    assert_eq!(
        std::fs::read(&partial).expect("read back"),
        staged_bytes(),
        "a checkpoint makes the bytes readable without committing"
    );
    assert!(staged.file_mut().is_some());

    staged.abandon().expect("abandon");
    assert!(!partial.exists(), "abandoning removes the partial file");
}

/// A commit replaces an existing regular file: that is the crash-recovery retry
/// and the content-addressed same-bytes case, and it is the point of a rename
/// commit rather than a failure.
#[test]
fn a_commit_replaces_an_existing_regular_file() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = PrivateTempDir::create(Some(base.path()), "plaudon").expect("staging");
    let destination = tempfile::tempdir_in(base.path()).expect("destination");

    let target_path = destination.path().join("media.wav");
    std::fs::write(&target_path, b"stale bytes").expect("pre-existing file");

    let policy = PathPolicy::for_roots([destination.path().to_path_buf()]);
    let target = canonicalize_and_validate(&policy, &target_path).expect("validated target");

    let mut staged = staging.stage_file("media").expect("stage");
    staged.write_all(staged_bytes()).expect("write");
    let committed = staged.commit_by_rename(&target).expect("commit");

    assert_eq!(committed.as_path(), target.as_path());
    assert_eq!(
        std::fs::read(committed.as_path()).expect("read"),
        staged_bytes()
    );
    assert_eq!(is_restrictive(committed.as_path()), Some(true));
    assert_eq!(
        std::fs::read_dir(staging.path())
            .expect("staging dir")
            .count(),
        0,
        "the staged file moved rather than being copied"
    );
}

/// A target that became a directory between validation and the rename is
/// refused instead of being renamed over.
#[test]
fn a_commit_onto_a_directory_is_refused() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = PrivateTempDir::create(Some(base.path()), "plaudon").expect("staging");
    let destination = tempfile::tempdir_in(base.path()).expect("destination");

    let target_path = destination.path().join("media.wav");
    let policy = PathPolicy::for_roots([destination.path().to_path_buf()]);
    let target = canonicalize_and_validate(&policy, &target_path).expect("validated target");

    // The planted directory appears after validation.
    std::fs::create_dir(&target_path).expect("plant a directory");

    let mut staged = staging.stage_file("media").expect("stage");
    staged.write_all(staged_bytes()).expect("write");
    let error = staged.commit_by_rename(&target).unwrap_err();
    assert_eq!(error.reason(), PathRejection::Io);
    assert_eq!(error.detail(), Some("IsADirectory"));
    assert!(
        target_path.is_dir(),
        "the planted directory was not replaced"
    );
}

/// A target that became a symlink between validation and the rename is refused,
/// and its destination is never written through (§13.4 symlink race).
#[cfg(unix)]
#[test]
fn a_commit_onto_a_planted_symlink_is_refused_and_its_target_is_untouched() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = PrivateTempDir::create(Some(base.path()), "plaudon").expect("staging");
    let destination = tempfile::tempdir_in(base.path()).expect("destination");

    let victim = base.path().join("victim.txt");
    std::fs::write(&victim, b"do not overwrite me").expect("victim");

    let target_path = destination.path().join("media.wav");
    let policy = PathPolicy::for_roots([destination.path().to_path_buf()]);
    let target = canonicalize_and_validate(&policy, &target_path).expect("validated target");

    std::os::unix::fs::symlink(&victim, &target_path).expect("plant a symlink");

    let mut staged = staging.stage_file("media").expect("stage");
    staged.write_all(staged_bytes()).expect("write");
    let error = staged.commit_by_rename(&target).unwrap_err();
    assert_eq!(error.reason(), PathRejection::SymlinkEscapesRoot);
    assert_eq!(
        std::fs::read(&victim).expect("victim still readable"),
        b"do not overwrite me",
        "the symlink target was written through"
    );
}

/// Restrictive permissions are never applied through a symlink, because that
/// would change the mode of whatever the link points at.
#[cfg(unix)]
#[test]
fn restrictive_permissions_are_never_applied_through_a_symlink() {
    let base = tempfile::tempdir().expect("temp base");
    let victim = base.path().join("victim.txt");
    std::fs::write(&victim, b"x").expect("victim");

    let link_path = base.path().join("link.txt");
    let policy = PathPolicy::for_roots([base.path().to_path_buf()]);
    let link = canonicalize_and_validate(&policy, &link_path).expect("validated");
    std::os::unix::fs::symlink(&victim, &link_path).expect("plant a symlink");

    assert_eq!(
        set_restrictive_permissions(&link).unwrap_err().reason(),
        PathRejection::SymlinkEscapesRoot
    );

    // A missing path cannot be inspected at all, which is reported as unknown
    // rather than as a reassuring "yes".
    assert_eq!(is_restrictive(&base.path().join("absent")), None);
}

/// Volume identity is readable for a real path, unreadable for a missing one,
/// and the comparison itself is exercisable with injected ids on a host that
/// has only one writable volume.
#[test]
fn volume_identity_is_readable_comparable_and_fails_closed_when_absent() {
    let base = tempfile::tempdir().expect("temp base");
    let here = volume_id(base.path()).expect("an existing path has a volume");
    let also_here = volume_id(base.path()).expect("stable");
    assert_eq!(here, also_here);
    assert!(same_volume(here, also_here));

    let error = volume_id(&base.path().join("absent")).unwrap_err();
    assert_eq!(error.reason(), PathRejection::Io);
    assert_eq!(error.detail(), Some("NotFound"));

    let a = VolumeId::from_raw(7);
    let b = VolumeId::from_raw(9);
    assert_eq!(a.as_u64(), 7);
    assert!(same_volume(a, VolumeId::from_raw(7)));
    assert!(!same_volume(a, b));
}

/// The reconciler counts every entry it refuses to touch, so a user folder that
/// happens to sit in the staging base is reported rather than silently skipped.
#[test]
fn the_reconciler_counts_what_it_leaves_alone() {
    let base = tempfile::tempdir().expect("temp base");
    let root = base.path();

    // Not our name shape.
    std::fs::create_dir(root.join("user-folder")).expect("user folder");
    std::fs::write(root.join("plaudon-1-deadbeef"), b"x").expect("wrong shape file");
    // Our prefix but a non-hex token.
    std::fs::create_dir(root.join(format!("plaudon-1-{}", "z".repeat(32)))).expect("bad token");
    // Our prefix but a non-numeric pid.
    std::fs::create_dir(root.join(format!("plaudon-pid-{}", "a".repeat(32)))).expect("bad pid");
    // Our prefix and a well-formed token, but no pid at all between them.
    std::fs::create_dir(root.join(format!("plaudon--{}", "c".repeat(32)))).expect("empty pid");
    // Our name shape, but a file rather than a directory.
    std::fs::write(root.join(format!("plaudon-2-{}", "b".repeat(32))), b"x").expect("file");

    let now = 2_000_000_000_i64;
    let report =
        PrivateTempDir::reconcile_abandoned(root, "plaudon", 3600, now).expect("reconcile runs");
    assert_eq!(report.removed, 0, "nothing here is ours to remove");
    assert!(report.skipped_foreign >= 5, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");

    assert!(root.join("user-folder").is_dir());
    assert!(root.join("plaudon-1-deadbeef").is_file());

    // A live staging directory of this process is ours and is left alone.
    let staging = PrivateTempDir::create(Some(root), "plaudon").expect("staging");
    let report =
        PrivateTempDir::reconcile_abandoned(root, "plaudon", 0, now).expect("reconcile runs");
    assert_eq!(report.skipped_own, 1, "{report:?}");
    assert_eq!(report.removed, 0, "{report:?}");
    assert!(staging.path().exists());
}

/// A validated staging path is still just a path: the guard is the `0o700` mode
/// and the single permitted root, which is what this asserts rather than any
/// secrecy of the random name.
#[test]
fn two_staging_directories_are_independent_roots() {
    let base = tempfile::tempdir().expect("temp base");
    let first = PrivateTempDir::create(Some(base.path()), "plaudon").expect("first");
    let second = PrivateTempDir::create(Some(base.path()), "plaudon").expect("second");
    assert_ne!(first.path(), second.path());

    let staged = first.stage_file("input").expect("stage");
    let path: &Path = staged.path().as_path();
    assert!(path.starts_with(first.path()));

    assert_eq!(
        canonicalize_and_validate(&second.policy(), path)
            .unwrap_err()
            .reason(),
        PathRejection::OutsideRoot,
        "one staging directory cannot reach into another"
    );
}

/// Set the mode of a directory we own, for the failure-injection tests below.
#[cfg(unix)]
fn chmod(dir: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("set mode");
}

/// Whether the mode just set actually denies this process. A build running as
/// root is not denied anything, and a permission test there would assert
/// nothing; those runs skip instead of reporting a false pass.
#[cfg(unix)]
fn mode_is_enforced(dir: &Path) -> bool {
    let probe = dir.join("mode-probe");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return false;
    }
    true
}

/// A staging base that exists but cannot be written to fails closed: the error
/// is the operating system's, not a silent fall back to some other directory.
#[cfg(unix)]
#[test]
fn a_staging_base_that_refuses_creation_fails_closed() {
    let base = tempfile::tempdir().expect("temp base");
    let locked = base.path().join("read-only-base");
    std::fs::create_dir(&locked).expect("locked base");
    chmod(&locked, 0o500);
    if !mode_is_enforced(&locked) {
        chmod(&locked, 0o700);
        return;
    }

    let error = PrivateTempDir::create(Some(&locked), "plaudon")
        .expect_err("a base that cannot be written to has no staging directory");
    let entries = std::fs::read_dir(&locked).expect("still listable").count();
    chmod(&locked, 0o700);

    assert_eq!(error.reason(), PathRejection::Io);
    assert_eq!(error.detail(), Some("PermissionDenied"));
    assert_eq!(entries, 0, "nothing was left behind in the locked base");
}

/// A reconciler base that is a file is an error. The one base shape that is
/// *not* an error is a base that does not exist yet, which is the first run on
/// a new machine.
#[test]
fn reconciling_a_base_that_is_not_a_directory_is_an_error_not_an_empty_report() {
    let base = tempfile::tempdir().expect("temp base");
    let file = base.path().join("staging-base");
    std::fs::write(&file, b"not a directory").expect("plant a file");

    let error = PrivateTempDir::reconcile_abandoned(&file, "plaudon", 3600, 2_000_000_000)
        .expect_err("a non-directory base is not 'nothing to reconcile'");
    assert_eq!(error.reason(), PathRejection::Io);
    assert_ne!(
        error.detail(),
        Some("NotFound"),
        "only a missing base reports success"
    );
    assert_eq!(
        std::fs::read(&file).expect("intact"),
        b"not a directory",
        "the file is never read as a directory nor removed"
    );

    let report = PrivateTempDir::reconcile_abandoned(
        &base.path().join("absent"),
        "plaudon",
        3600,
        2_000_000_000,
    )
    .expect("a base that does not exist yet is the first-run case");
    assert_eq!(report, ReconcileReport::default());
}

/// An entry whose name is not UTF-8 cannot be our staging directory — our names
/// are ASCII — so it is counted as foreign and left where it is.
///
/// Not macOS: APFS and HFS+ refuse a non-UTF-8 name with `EILSEQ`, so the case
/// cannot be planted there at all.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn an_entry_whose_name_is_not_utf8_is_foreign_and_left_alone() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let base = tempfile::tempdir().expect("temp base");
    let mut raw = b"plaudon-1-".to_vec();
    raw.extend_from_slice(&[0xff; 32]);
    let planted = base.path().join(OsString::from_vec(raw));
    std::fs::create_dir(&planted).expect("plant a non-UTF-8 entry");

    let report = PrivateTempDir::reconcile_abandoned(base.path(), "plaudon", 0, 2_000_000_000)
        .expect("reconcile runs");
    assert_eq!(report.skipped_foreign, 1, "{report:?}");
    assert_eq!(report.removed, 0, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    assert!(planted.is_dir(), "a name we cannot read is never removed");
}

/// An entry that matches our name shape but cannot be inspected is counted as a
/// failure, not removed on the strength of its name alone — the name is the
/// cheapest thing for someone else to forge.
#[cfg(unix)]
#[test]
fn an_entry_that_cannot_be_inspected_is_counted_as_failed() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = base.path().join("staging");
    std::fs::create_dir(&staging).expect("staging base");
    let abandoned = staging.join(format!("plaudon-1-{}", "a".repeat(32)));
    std::fs::create_dir(&abandoned).expect("abandoned directory");

    // Listable but not searchable: `read_dir` yields the entry, `lstat` on it
    // is refused.
    chmod(&staging, 0o400);
    if std::fs::symlink_metadata(&abandoned).is_ok() {
        chmod(&staging, 0o700);
        return;
    }

    let report = PrivateTempDir::reconcile_abandoned(&staging, "plaudon", 0, 2_000_000_000);
    chmod(&staging, 0o700);

    let report = report.expect("reconcile runs");
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(report.removed, 0, "{report:?}");
    assert_eq!(report.skipped_foreign, 0, "{report:?}");
    assert!(abandoned.is_dir(), "an uninspectable entry is left alone");
}

/// An abandoned directory the reconciler is not allowed to delete is reported
/// as a failure rather than as a removal, so a startup report cannot claim
/// space it did not reclaim.
#[cfg(unix)]
#[test]
fn an_abandoned_directory_that_cannot_be_removed_is_counted_as_failed() {
    let base = tempfile::tempdir().expect("temp base");
    let staging = base.path().join("staging");
    std::fs::create_dir(&staging).expect("staging base");
    let abandoned = staging.join(format!("plaudon-1-{}", "b".repeat(32)));
    std::fs::create_dir(&abandoned).expect("abandoned directory");
    std::fs::write(abandoned.join("import.part"), b"partial").expect("partial file");

    // Listable and searchable, but not writable: the entry can be identified
    // as ours and as abandoned, and then neither emptied nor unlinked.
    chmod(&abandoned, 0o500);
    chmod(&staging, 0o500);
    if !mode_is_enforced(&staging) {
        chmod(&staging, 0o700);
        chmod(&abandoned, 0o700);
        return;
    }

    let report = PrivateTempDir::reconcile_abandoned(&staging, "plaudon", 0, 2_000_000_000);
    chmod(&staging, 0o700);
    chmod(&abandoned, 0o700);

    let report = report.expect("reconcile runs");
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(report.removed, 0, "{report:?}");
    assert_eq!(report.skipped_recent, 0, "{report:?}");
    assert!(
        abandoned.join("import.part").is_file(),
        "the directory it could not remove is intact"
    );
}
