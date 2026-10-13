//! Crash injection across the export, backup, restore and deletion paths
//! (spec 18.6A "Archive" and "Portable backup" clusters, J1.5, J2.5).
//!
//! Each case runs the real code in a child process and kills it with `abort`
//! at a chosen durable step, then checks the invariant from the parent:
//! nothing half-written is ever presented as finished, and the ordinary
//! recovery path finishes the job.

use std::path::{Path, PathBuf};
use std::process::Command;

use plaudon_app_core::deletion::DeletionService;
use plaudon_chaos_tests::ExportStep;
use plaudon_domain::time::Timestamp;
use plaudon_export::archive::restore::{ConflictPolicy, RestoreMode, RestoreOptions};
use plaudon_export::service::{BackupRequest, ExportService, RestoreArchiveRequest};
use plaudon_integration_tests::fakes::library;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::deletion::{DeletionRepository, DeletionState};

const NOW: Timestamp = Timestamp::from_millis(1_700_000_500_000);

fn run_child(
    library_root: &Path,
    work_dir: &Path,
    step: ExportStep,
    archive: Option<&Path>,
) -> Vec<String> {
    std::fs::create_dir_all(work_dir).expect("work dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_export-step-runner"));
    command.arg(library_root).arg(work_dir).arg(step.as_str());
    if let Some(archive) = archive {
        command.arg(archive);
    }
    let output = command.output().expect("spawn export step runner");
    assert!(
        !output.status.success(),
        "the child must abort, not exit cleanly ({step:?})"
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[tokio::test]
async fn a_crash_during_an_export_leaves_no_export_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    let seeded = library::seed(&root, false).await;
    seeded.db.close().await;

    let work = dir.path().join("exports");
    run_child(&root, &work, ExportStep::ExportPartialWrite, None);

    let names = entries(&work);
    assert!(
        !names.iter().any(|name| name == "transcript.txt"),
        "a partial export was committed: {names:?}"
    );
    for name in &names {
        assert!(
            name.starts_with(".plaudon-export-")
                && Path::new(name).extension().is_some_and(|ext| ext == "part"),
            "unexpected leftover {name}"
        );
    }
}

#[tokio::test]
async fn a_crash_during_a_backup_leaves_no_recording_archive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    let seeded = library::seed(&root, false).await;
    seeded.db.close().await;

    let work = dir.path().join("backups");
    run_child(&root, &work, ExportStep::BackupPartialWrite, None);

    let names = entries(&work);
    assert!(
        !names
            .iter()
            .any(|name| name.ends_with(".recording-archive")),
        "a partial archive was committed: {names:?}"
    );
}

/// Source library plus a plaintext archive of it.
async fn source_archive(dir: &Path) -> (PathBuf, library::SeededLibrary) {
    let source_root = dir.join("source-library");
    let seeded = library::seed(&source_root, false).await;
    let service = ExportService::new(
        seeded.db.clone(),
        seeded.archive.clone(),
        seeded.root.clone(),
    );
    let backups = dir.join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    let response = service
        .create_portable_backup(
            &BackupRequest {
                destination: backups,
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .expect("backup");
    (response.path, seeded)
}

#[tokio::test]
async fn a_crash_after_staging_leaves_the_target_library_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (archive_path, source) = source_archive(dir.path()).await;

    let target_root = dir.path().join("target-library");
    let (target_db, target_media) = library::empty_library(&target_root).await;
    target_db.close().await;

    run_child(
        &target_root,
        &dir.path().join("restore-work"),
        ExportStep::RestoreAfterStaging,
        Some(&archive_path),
    );

    let db = Database::open(target_root.join("app.sqlite"))
        .await
        .unwrap();
    let recordings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(recordings, 0, "staging must not touch the library");
    assert_eq!(target_media.list_blobs().unwrap(), Vec::new());

    // The ordinary restore still works afterwards.
    let service = ExportService::new(db.clone(), target_media.clone(), target_root.clone());
    let report = service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path,
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::NewLibrary,
                    on_conflict: ConflictPolicy::Fail,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .expect("restore after crash");
    assert_eq!(report.recordings_restored, 1);
    assert_eq!(
        target_media.list_blobs().unwrap(),
        source.archive.list_blobs().unwrap()
    );
    db.close().await;
    source.db.close().await;
}

#[tokio::test]
async fn a_crash_after_media_commit_resumes_by_reusing_the_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let (archive_path, source) = source_archive(dir.path()).await;

    let target_root = dir.path().join("target-library");
    let (target_db, target_media) = library::empty_library(&target_root).await;
    target_db.close().await;

    let committed = run_child(
        &target_root,
        &dir.path().join("restore-work"),
        ExportStep::RestoreAfterMediaCommit,
        Some(&archive_path),
    );
    assert_eq!(committed.len(), 2, "both blobs should have been committed");

    let db = Database::open(target_root.join("app.sqlite"))
        .await
        .unwrap();
    let recordings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(recordings, 0, "no row may exist without its transaction");
    assert_eq!(target_media.list_blobs().unwrap().len(), 2);

    let service = ExportService::new(db.clone(), target_media.clone(), target_root.clone());
    let report = service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path,
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::NewLibrary,
                    on_conflict: ConflictPolicy::Fail,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .expect("resume restore");
    assert_eq!(report.recordings_restored, 1);
    assert_eq!(
        report.media_reused, 2,
        "orphan blobs are reused, not rewritten"
    );
    assert_eq!(report.media_committed, 0);
    assert_eq!(
        target_media.list_blobs().unwrap(),
        source.archive.list_blobs().unwrap()
    );
    db.close().await;
    source.db.close().await;
}

#[tokio::test]
async fn a_crash_right_after_the_deletion_journal_is_finished_by_the_reconciler() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    let seeded = library::seed(&root, false).await;
    let recording_id = seeded.recording_id;
    seeded.db.close().await;

    let printed = run_child(
        &root,
        &dir.path().join("work"),
        ExportStep::DeleteAfterJournal,
        None,
    );
    assert_eq!(printed.len(), 1, "the journal id must be durable");

    let db = Database::open(root.join("app.sqlite")).await.unwrap();
    let media = MediaArchive::open(&root).unwrap();
    let journal = DeletionRepository::new(db.clone());
    let unfinished = journal.unfinished().await.unwrap();
    assert_eq!(unfinished.len(), 1);
    assert_eq!(unfinished[0].state, DeletionState::Planned);
    // Nothing was removed yet: the recording still looks intact.
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings WHERE id = ?1")
        .bind(recording_id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(media.list_blobs().unwrap().len(), 2);

    let service = DeletionService::new(db.clone(), media.clone());
    let receipts = service.reconcile(NOW).await.expect("reconcile");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].blobs_removed, 2);
    assert_eq!(media.list_blobs().unwrap(), Vec::new());
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(journal.unfinished().await.unwrap(), Vec::new());
    db.close().await;
}

#[tokio::test]
async fn a_crash_after_the_rows_are_gone_still_frees_the_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    let seeded = library::seed(&root, false).await;
    let recording_id = seeded.recording_id;
    seeded.db.close().await;

    run_child(
        &root,
        &dir.path().join("work"),
        ExportStep::DeleteAfterRows,
        None,
    );

    let db = Database::open(root.join("app.sqlite")).await.unwrap();
    let media = MediaArchive::open(&root).unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings WHERE id = ?1")
        .bind(recording_id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "the rows are gone");
    assert_eq!(
        media.list_blobs().unwrap().len(),
        2,
        "the blobs outlive the crash"
    );
    let journal = DeletionRepository::new(db.clone());
    let unfinished = journal.unfinished().await.unwrap();
    assert_eq!(unfinished.len(), 1);
    assert_eq!(unfinished[0].state, DeletionState::RowsRemoved);

    // A half-deleted recording is never presented as intact: the tombstone and
    // the suppression are already durable.
    let service = DeletionService::new(db.clone(), media.clone());
    assert!(service
        .is_media_suppressed(&seeded.original_identity)
        .await
        .unwrap());

    let receipts = service.reconcile(NOW).await.expect("reconcile");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].blobs_removed, 2);
    assert_eq!(media.list_blobs().unwrap(), Vec::new());
    assert_eq!(journal.unfinished().await.unwrap(), Vec::new());
    db.close().await;
}

#[test]
fn every_crash_point_has_a_case() {
    // A step added to the runner without a test here is a step nobody checks.
    let covered = [
        ExportStep::ExportPartialWrite,
        ExportStep::BackupPartialWrite,
        ExportStep::RestoreAfterStaging,
        ExportStep::RestoreAfterMediaCommit,
        ExportStep::DeleteAfterJournal,
        ExportStep::DeleteAfterRows,
    ];
    assert_eq!(covered.len(), ExportStep::ALL.len());
    for step in ExportStep::ALL {
        assert!(covered.contains(&step), "{step:?} has no chaos case");
        assert_eq!(ExportStep::parse(step.as_str()), Some(step));
    }
}
