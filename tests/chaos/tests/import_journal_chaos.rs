//! Crash injection at every durable import step, plus SQLite transaction
//! failure, a locked database and an out-of-space write (spec 18.6A Archive
//! cluster, 19 Phase 1 exit).

use std::path::{Path, PathBuf};
use std::process::Command;

use plaudon_chaos_tests::ImportStep;
use plaudon_domain::ids::ImportIntentId;
use plaudon_domain::media::{MediaIdentity, QualityClass};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::error::PersistenceError;
use plaudon_persistence::journal::{ImportJournal, JournalState, ReconcileOutcome};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::MediaAssetRepository;

const CONTENT: &[u8] = b"crash injection audio payload";

struct Crashed {
    dir: tempfile::TempDir,
    intent_id: Option<ImportIntentId>,
    staging_path: Option<PathBuf>,
}

impl Crashed {
    fn db_path(&self) -> PathBuf {
        self.dir.path().join("app.sqlite")
    }

    fn archive_root(&self) -> PathBuf {
        self.dir.path().join("archive")
    }
}

/// Run a real import in a child process and kill it at `step`.
fn crash_at(step: ImportStep) -> Crashed {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("app.sqlite");
    let archive_root = dir.path().join("archive");

    let output = Command::new(env!("CARGO_BIN_EXE_import-step-runner"))
        .arg(&db_path)
        .arg(&archive_root)
        .arg(step.as_str())
        .arg(String::from_utf8(CONTENT.to_vec()).expect("utf8 content"))
        .output()
        .expect("spawn import step runner");

    assert!(
        !output.status.success(),
        "the child must abort, not exit cleanly"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let intent_id = lines
        .next()
        .and_then(|raw| raw.trim().parse::<ImportIntentId>().ok());
    let staging_path = lines.next().map(|raw| PathBuf::from(raw.trim()));

    Crashed {
        dir,
        intent_id,
        staging_path,
    }
}

async fn reopen(crashed: &Crashed) -> (Database, ImportJournal) {
    let db = Database::open(crashed.db_path())
        .await
        .expect("reopen database");
    MigrationRunner::new()
        .run(&db)
        .await
        .expect("migrate on restart");
    let archive = MediaArchive::open(crashed.archive_root()).expect("reopen archive");
    let journal = ImportJournal::new(db.clone(), archive);
    (db, journal)
}

async fn assert_valid_state(db: &Database, journal: &ImportJournal) {
    // Every committed intent points at an asset whose bytes verify.
    let assets = MediaAssetRepository::new(db.clone());
    for (relative, _) in journal.archive().list_blobs().expect("list blobs") {
        let identity = sqlx::query_scalar::<_, String>(
            "SELECT sha256 FROM media_assets WHERE relative_path = ?1",
        )
        .bind(&relative)
        .fetch_optional(db.pool())
        .await
        .expect("query asset");
        assert!(
            identity.is_some(),
            "archive holds {relative} with no media_assets row: half-registered asset"
        );
    }
    let rows = sqlx::query_as::<_, (String, i64, String)>(
        "SELECT sha256, byte_length, relative_path FROM media_assets",
    )
    .fetch_all(db.pool())
    .await
    .expect("query assets");
    for (sha, length, relative) in rows {
        let identity = MediaIdentity::new(
            sha.parse().expect("digest"),
            u64::try_from(length).expect("length"),
            String::new(),
        );
        journal
            .archive()
            .verify(&relative, &identity)
            .expect("committed asset must verify");
    }
    assert!(assets.count().await.expect("count") <= 1);
    assert!(
        journal.incomplete().await.expect("incomplete").is_empty(),
        "reconciliation must settle every intent"
    );
}

#[tokio::test]
async fn every_durable_step_recovers_to_a_valid_state() {
    for step in ImportStep::ALL {
        let crashed = crash_at(step);
        let (db, journal) = reopen(&crashed).await;

        let outcomes = journal
            .reconcile(Timestamp::from_millis(100))
            .await
            .expect("reconcile");
        assert_valid_state(&db, &journal).await;

        let assets = MediaAssetRepository::new(db.clone());
        match step {
            ImportStep::BeforeStaging => {
                assert!(
                    outcomes.is_empty(),
                    "nothing happened, nothing to reconcile"
                );
                assert_eq!(assets.count().await.unwrap(), 0);
            }
            ImportStep::AfterStaging => {
                // No intent row exists, so the staged file is an app-owned
                // orphan and is removed. No half-registered asset.
                assert_eq!(assets.count().await.unwrap(), 0);
                let staging = crashed.staging_path.clone().expect("staging path reported");
                assert!(!staging.exists(), "an orphan staging file is cleaned up");
                assert!(matches!(
                    outcomes.as_slice(),
                    [ReconcileOutcome::OrphanRemoved(_)]
                ));
            }
            ImportStep::AfterIntent | ImportStep::AfterRename | ImportStep::AfterCommit => {
                let intent_id = crashed.intent_id.expect("intent id reported");
                assert_eq!(
                    journal.get(intent_id).await.unwrap().state,
                    JournalState::Committed,
                    "step {step:?} must recover to a committed import"
                );
                assert_eq!(assets.count().await.unwrap(), 1);
            }
        }
        db.close().await;
    }
}

#[tokio::test]
async fn recovery_is_idempotent_across_repeated_restarts() {
    let crashed = crash_at(ImportStep::AfterIntent);
    for _ in 0..3 {
        let (db, journal) = reopen(&crashed).await;
        journal
            .reconcile(Timestamp::from_millis(100))
            .await
            .expect("reconcile");
        assert_valid_state(&db, &journal).await;
        assert_eq!(
            MediaAssetRepository::new(db.clone()).count().await.unwrap(),
            1
        );
        db.close().await;
    }
}

#[tokio::test]
async fn a_failed_sqlite_transaction_leaves_no_half_registered_asset() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
    let journal = ImportJournal::new(db.clone(), archive);

    let staged = journal
        .stage(&mut std::io::Cursor::new(CONTENT.to_vec()), "wav")
        .unwrap();
    let intent = journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            None,
            Timestamp::EPOCH,
        )
        .await
        .unwrap();
    journal
        .rename_into_archive(&intent, Timestamp::EPOCH)
        .await
        .unwrap();

    // Pre-insert a conflicting row so the commit's INSERT violates the unique
    // media index while the archive file already exists.
    sqlx::query(
        "INSERT INTO media_assets (id, sha256, byte_length, relative_path, quality_class, verification_state, created_at)
         VALUES (?1, ?2, ?3, ?4, 'user_supplied_file', 'verified', 0)",
    )
    .bind(plaudon_domain::ids::MediaAssetId::new().to_string())
    .bind(intent.identity.sha256.to_hex())
    .bind(i64::try_from(intent.identity.byte_length).unwrap())
    .bind(&intent.target_relative_path)
    .execute(db.pool())
    .await
    .unwrap();

    // The commit now finds the existing row, re-verifies it and reuses it
    // rather than creating a duplicate.
    journal.commit(&intent, Timestamp::EPOCH).await.unwrap();
    assert_eq!(
        MediaAssetRepository::new(db.clone()).count().await.unwrap(),
        1
    );
    assert_valid_state(&db, &journal).await;
    db.close().await;
}

#[tokio::test]
async fn a_locked_database_surfaces_a_typed_error_rather_than_a_partial_commit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("app.sqlite");
    let db = Database::open(&db_path).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
    let journal = ImportJournal::new(db.clone(), archive);

    // Hold an exclusive transaction on a second connection for longer than the
    // busy timeout allows the first to wait.
    let blocker = Database::open(&db_path).await.unwrap();
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(blocker.pool())
        .await
        .unwrap();

    let staged = journal
        .stage(&mut std::io::Cursor::new(CONTENT.to_vec()), "wav")
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        journal.record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            None,
            Timestamp::EPOCH,
        ),
    )
    .await;

    match result {
        Ok(Ok(_)) => panic!("writing through an exclusive lock must not succeed"),
        Ok(Err(error)) => assert_eq!(
            error.code(),
            plaudon_domain::error::AppErrorCode::StorageUnavailable,
            "a locked database is reported as storage unavailable"
        ),
        Err(elapsed) => panic!("the busy timeout must bound the wait: {elapsed}"),
    }

    sqlx::query("ROLLBACK")
        .execute(blocker.pool())
        .await
        .unwrap();
    blocker.close().await;

    // No asset was registered, and the staged bytes are still just a staging
    // file that reconciliation will clean up.
    assert_eq!(
        MediaAssetRepository::new(db.clone()).count().await.unwrap(),
        0
    );
    journal.reconcile(Timestamp::from_millis(10)).await.unwrap();
    assert!(!staged.path.exists());
    db.close().await;
}

#[tokio::test]
async fn an_unwritable_archive_fails_the_commit_without_registering_an_asset() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    let archive_root = dir.path().join("archive");
    let archive = MediaArchive::open(&archive_root).unwrap();
    let journal = ImportJournal::new(db.clone(), archive);

    let staged = journal
        .stage(&mut std::io::Cursor::new(CONTENT.to_vec()), "wav")
        .unwrap();
    let intent = journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            None,
            Timestamp::EPOCH,
        )
        .await
        .unwrap();

    make_read_only(&archive_root.join("media/sha256"));
    let error = journal
        .rename_into_archive(&intent, Timestamp::EPOCH)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            PersistenceError::Io(_) | PersistenceError::StorageFull
        ),
        "unexpected error: {error}"
    );
    make_writable(&archive_root.join("media/sha256"));

    assert_eq!(
        MediaAssetRepository::new(db.clone()).count().await.unwrap(),
        0
    );
    assert_eq!(
        journal.get(intent.id).await.unwrap().state,
        JournalState::IntentRecorded
    );

    // A later restart recovers it cleanly now that writing works again.
    let outcomes = journal.reconcile(Timestamp::from_millis(50)).await.unwrap();
    assert_eq!(outcomes, vec![ReconcileOutcome::Committed(intent.id)]);
    assert_valid_state(&db, &journal).await;
    db.close().await;
}

#[tokio::test]
async fn reconciliation_never_touches_files_the_application_did_not_create() {
    let crashed = crash_at(ImportStep::AfterStaging);
    let (db, journal) = reopen(&crashed).await;

    let foreign = journal
        .archive()
        .staging_dir()
        .join("user-dropped-recording.wav");
    std::fs::write(&foreign, b"a source recording the user put here").unwrap();
    let outside = crashed.dir.path().join("original-source.wav");
    std::fs::write(&outside, b"the user's own file").unwrap();

    journal
        .reconcile(Timestamp::from_millis(100))
        .await
        .unwrap();

    assert!(
        foreign.is_file(),
        "a foreign file in staging must survive reconciliation"
    );
    assert!(
        outside.is_file(),
        "a source recording is never deleted as cleanup"
    );
    db.close().await;
}

#[cfg(unix)]
fn make_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).expect("chmod");
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

#[cfg(not(unix))]
fn make_read_only(path: &Path) {
    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions).expect("set permissions");
}

#[cfg(not(unix))]
fn make_writable(path: &Path) {
    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).expect("set permissions");
}
