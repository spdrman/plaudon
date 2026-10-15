//! Rejection and recovery paths of the durable import journal (spec 13.3A).
//!
//! The unit tests inside `journal.rs` walk the six durable steps forwards.
//! These drive the other side: a double registration, an unknown intent, a row
//! the schema allows but this build cannot interpret, an archive entry that
//! cannot be verified, and a commit whose asset insert fails. Every rejection
//! asserts the specific error variant, that the journal state did not advance,
//! and that no asset was registered.

use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::{ImportIntentId, JobId, MediaAssetId, RecordingId};
use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::error::PersistenceError;
use plaudon_persistence::journal::{
    ImportJournal, JournalState, PreparedIntent, ReconcileOutcome, StagedFile,
};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::MediaAssetRepository;

const AT: Timestamp = Timestamp::EPOCH;

struct Harness {
    _dir: tempfile::TempDir,
    db: Database,
    journal: ImportJournal,
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
        MigrationRunner::new().run(&db).await.unwrap();
        let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
        let journal = ImportJournal::new(db.clone(), archive);
        Self {
            _dir: dir,
            db,
            journal,
        }
    }

    /// Stage `bytes` and record the intent, stopping at step 3.
    async fn prepared(&self, bytes: &[u8], class: QualityClass) -> PreparedIntent {
        let staged = self
            .journal
            .stage(&mut std::io::Cursor::new(bytes.to_vec()), "wav")
            .unwrap();
        self.journal
            .record_intent(&staged, "wav", class, None, None, AT)
            .await
            .unwrap()
    }

    async fn asset_count(&self) -> i64 {
        MediaAssetRepository::new(self.db.clone())
            .count()
            .await
            .unwrap()
    }

    async fn journal_rows(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM import_journal")
            .fetch_one(self.db.pool())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn registering_the_same_intent_twice_is_refused_without_a_second_row() {
    let h = Harness::new().await;
    let staged = h
        .journal
        .stage(&mut std::io::Cursor::new(b"audio".to_vec()), "wav")
        .unwrap();
    h.journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            None,
            AT,
        )
        .await
        .unwrap();

    let error = h
        .journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            None,
            AT,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, PersistenceError::Conflict),
        "unexpected error {error:?}"
    );
    assert_eq!(error.code(), AppErrorCode::Conflict);
    assert_eq!(h.journal_rows().await, 1);
    assert_eq!(h.asset_count().await, 0);
}

#[tokio::test]
async fn an_unknown_intent_is_reported_as_record_not_found() {
    let h = Harness::new().await;
    let unknown = ImportIntentId::new();

    assert!(matches!(
        h.journal.get(unknown).await,
        Err(PersistenceError::RecordNotFound)
    ));
    assert!(matches!(
        h.journal.abandon(unknown, "NO_SUCH_INTENT", AT).await,
        Err(PersistenceError::RecordNotFound)
    ));
    assert_eq!(h.journal_rows().await, 0);
    assert_eq!(h.asset_count().await, 0);
}

#[tokio::test]
async fn every_quality_class_round_trips_and_an_unreadable_one_degrades_to_unknown() {
    let h = Harness::new().await;
    for (index, class) in [
        QualityClass::OriginalDeviceRecording,
        QualityClass::CompressedDeviceTransfer,
        QualityClass::VendorCloudExport,
        QualityClass::UserSuppliedFile,
        QualityClass::LocalDerivative,
        QualityClass::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        let bytes = format!("payload {index}").into_bytes();
        let intent = h.prepared(&bytes, class).await;
        assert_eq!(h.journal.get(intent.id).await.unwrap().quality_class, class);
    }

    // A class this build does not know is read back as `Unknown` rather than
    // failing the whole startup reconciliation.
    let intent = h
        .prepared(b"martian payload", QualityClass::UserSuppliedFile)
        .await;
    sqlx::query("UPDATE import_journal SET quality_class = 'martian' WHERE id = ?1")
        .bind(intent.id.to_string())
        .execute(h.db.pool())
        .await
        .unwrap();
    assert_eq!(
        h.journal.get(intent.id).await.unwrap().quality_class,
        QualityClass::Unknown
    );
}

#[tokio::test]
async fn an_intent_carries_the_recording_and_job_it_belongs_to() {
    let h = Harness::new().await;
    let recording_id = RecordingId::new();
    let job_id = JobId::new();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1, 'Fixture', 0)")
        .bind(recording_id.to_string())
        .execute(h.db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, job_type, state, created_at) VALUES (?1, 'import', 'queued', 0)",
    )
    .bind(job_id.to_string())
    .execute(h.db.pool())
    .await
    .unwrap();

    let staged = h
        .journal
        .stage(&mut std::io::Cursor::new(b"owned audio".to_vec()), "wav")
        .unwrap();
    let intent = h
        .journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::OriginalDeviceRecording,
            Some(recording_id),
            Some(job_id),
            AT,
        )
        .await
        .unwrap();
    assert_eq!(intent.recording_id, Some(recording_id));
    assert_eq!(intent.job_id, Some(job_id));

    let reloaded = h.journal.get(intent.id).await.unwrap();
    assert_eq!(reloaded.recording_id, Some(recording_id));
    assert_eq!(reloaded.job_id, Some(job_id));
    assert_eq!(reloaded.media_asset_id, None);

    let asset_id = h.journal.commit(&intent, AT).await.unwrap();
    assert_eq!(
        h.journal.get(intent.id).await.unwrap().media_asset_id,
        Some(asset_id)
    );
}

#[tokio::test]
async fn a_journal_row_this_build_cannot_interpret_is_reported_not_guessed() {
    let h = Harness::new().await;
    let intent = h.prepared(b"audio", QualityClass::UserSuppliedFile).await;

    // A digest column the schema accepts (64 characters) but no build can
    // parse must not become a silently different identity.
    sqlx::query("UPDATE import_journal SET expected_sha256 = ?2 WHERE id = ?1")
        .bind(intent.id.to_string())
        .bind("z".repeat(64))
        .execute(h.db.pool())
        .await
        .unwrap();
    assert!(matches!(
        h.journal.get(intent.id).await,
        Err(PersistenceError::IntegrityFailed)
    ));
    assert!(matches!(
        h.journal.incomplete().await,
        Err(PersistenceError::IntegrityFailed)
    ));
    assert!(matches!(
        h.journal.reconcile(AT).await,
        Err(PersistenceError::IntegrityFailed)
    ));

    // An identifier that is not an intent id at all is a migration failure,
    // not a row we quietly skip.
    sqlx::query("UPDATE import_journal SET id = 'not-an-intent-id'")
        .execute(h.db.pool())
        .await
        .unwrap();
    assert!(matches!(
        h.journal.retained_intent_ids().await,
        Err(PersistenceError::Migration(_))
    ));
    assert_eq!(h.asset_count().await, 0);
}

#[tokio::test]
async fn a_byte_length_beyond_i64_is_clamped_when_the_intent_is_recorded() {
    let h = Harness::new().await;
    let intent_id = ImportIntentId::new();
    let staged = StagedFile {
        intent_id,
        path: h.journal.archive().staging_path(intent_id),
        identity: MediaIdentity::new(Sha256Digest::of(b"claimed"), u64::MAX, "wav"),
    };

    h.journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            None,
            AT,
        )
        .await
        .unwrap();

    let stored = sqlx::query_scalar::<_, i64>(
        "SELECT expected_byte_length FROM import_journal WHERE id = ?1",
    )
    .bind(intent_id.to_string())
    .fetch_one(h.db.pool())
    .await
    .unwrap();
    assert_eq!(stored, i64::MAX);
    assert_eq!(
        h.journal.get(intent_id).await.unwrap().identity.byte_length,
        u64::try_from(i64::MAX).unwrap()
    );
    assert_eq!(h.asset_count().await, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn an_intent_whose_archive_entry_is_a_symlink_is_deferred_not_committed() {
    let h = Harness::new().await;
    let intent = h.prepared(b"audio", QualityClass::UserSuppliedFile).await;

    // Something replaced the archive entry with a symlink. That is neither a
    // verified blob nor an integrity failure of our own bytes, so the intent
    // must be deferred to the next pass instead of committed or quarantined.
    let target = h
        .journal
        .archive()
        .absolute(&intent.target_relative_path)
        .unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&intent.staging_path, &target).unwrap();

    let outcomes = h.journal.reconcile(AT).await.unwrap();
    assert_eq!(
        outcomes,
        vec![ReconcileOutcome::Deferred {
            intent_id: intent.id,
            code: AppErrorCode::ArchivePathRejected,
        }]
    );
    assert_eq!(
        h.journal.get(intent.id).await.unwrap().state,
        JournalState::IntentRecorded,
        "a deferred intent keeps its state so the next pass can retry it"
    );
    assert_eq!(h.asset_count().await, 0);
    assert!(
        intent.staging_path.is_file(),
        "the staged bytes of a deferred intent are not swept as an orphan"
    );
}

#[tokio::test]
async fn abandoning_an_intent_removes_its_staging_file_and_registers_nothing() {
    let h = Harness::new().await;
    let intent = h.prepared(b"audio", QualityClass::UserSuppliedFile).await;
    assert!(intent.staging_path.is_file());

    h.journal
        .abandon(intent.id, "USER_CANCELLED", AT)
        .await
        .unwrap();

    assert!(!intent.staging_path.exists());
    let abandoned = h.journal.get(intent.id).await.unwrap();
    assert_eq!(abandoned.state, JournalState::Abandoned);
    assert!(abandoned.state.is_settled());
    assert_eq!(h.asset_count().await, 0);
    assert_eq!(h.journal.incomplete().await.unwrap(), Vec::new());

    // Abandoning twice is harmless: the staging file is already gone.
    h.journal
        .abandon(intent.id, "USER_CANCELLED", AT)
        .await
        .unwrap();
    assert_eq!(h.asset_count().await, 0);
}

#[tokio::test]
async fn a_duplicate_import_whose_staging_file_vanished_still_reuses_the_blob() {
    let h = Harness::new().await;
    let first = h
        .prepared(b"shared bytes", QualityClass::UserSuppliedFile)
        .await;
    h.journal.rename_into_archive(&first, AT).await.unwrap();
    let first_asset = h.journal.commit(&first, AT).await.unwrap();

    let second = h
        .prepared(b"shared bytes", QualityClass::UserSuppliedFile)
        .await;
    std::fs::remove_file(&second.staging_path).unwrap();

    h.journal.rename_into_archive(&second, AT).await.unwrap();
    assert_eq!(
        h.journal.get(second.id).await.unwrap().state,
        JournalState::Renamed
    );
    assert_eq!(h.journal.commit(&second, AT).await.unwrap(), first_asset);
    assert_eq!(h.asset_count().await, 1);
    assert_eq!(h.journal.archive().list_blobs().unwrap().len(), 1);
}

#[tokio::test]
async fn a_commit_that_would_reuse_a_corrupted_blob_fails_without_advancing_the_journal() {
    let h = Harness::new().await;
    let first = h
        .prepared(b"reusable bytes", QualityClass::UserSuppliedFile)
        .await;
    h.journal.rename_into_archive(&first, AT).await.unwrap();
    h.journal.commit(&first, AT).await.unwrap();

    // A second intent for the same bytes reaches the rename, and only then are
    // the archived bytes damaged. The commit must re-verify what it is about
    // to reuse.
    let second = h
        .prepared(b"reusable bytes", QualityClass::UserSuppliedFile)
        .await;
    h.journal.rename_into_archive(&second, AT).await.unwrap();
    std::fs::write(
        h.journal
            .archive()
            .absolute(&second.target_relative_path)
            .unwrap(),
        b"damaged",
    )
    .unwrap();

    let error = h.journal.commit(&second, AT).await.unwrap_err();
    assert!(
        matches!(error, PersistenceError::IntegrityFailed),
        "unexpected error {error:?}"
    );
    assert_eq!(
        h.journal.get(second.id).await.unwrap().state,
        JournalState::Renamed,
        "a failed commit must not mark the intent committed"
    );
    assert_eq!(h.asset_count().await, 1, "no second asset was registered");
}

#[tokio::test]
async fn a_failed_asset_insert_leaves_no_half_registered_asset() {
    let h = Harness::new().await;
    let intent = h.prepared(b"audio", QualityClass::UserSuppliedFile).await;
    h.journal.rename_into_archive(&intent, AT).await.unwrap();

    // An unrelated asset already owns the archive path this intent targets, so
    // the insert violates `ux_media_relative_path`.
    sqlx::query(
        "INSERT INTO media_assets
             (id, sha256, byte_length, relative_path, quality_class,
              verification_state, created_at)
         VALUES (?1, ?2, 99, ?3, 'unknown', 'verified', 0)",
    )
    .bind(MediaAssetId::new().to_string())
    .bind("a".repeat(64))
    .bind(&intent.target_relative_path)
    .execute(h.db.pool())
    .await
    .unwrap();

    let error = h.journal.commit(&intent, AT).await.unwrap_err();
    assert!(
        matches!(error, PersistenceError::Conflict),
        "unexpected error {error:?}"
    );
    assert_eq!(
        h.journal.get(intent.id).await.unwrap().state,
        JournalState::Renamed
    );
    assert_eq!(
        h.journal.get(intent.id).await.unwrap().media_asset_id,
        None,
        "a failed insert never attaches an asset to the intent"
    );
    assert_eq!(
        h.asset_count().await,
        1,
        "only the pre-existing row survives"
    );
    let ours = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_assets WHERE sha256 = ?1")
        .bind(intent.identity.sha256.to_hex())
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(ours, 0);
}

#[tokio::test]
async fn a_damaged_journal_row_is_refused_rather_than_misread() {
    let h = Harness::new().await;
    let intent = h.prepared(b"audio", QualityClass::UserSuppliedFile).await;

    // A row the schema would never accept today, as a damaged file or an
    // older build could still leave behind. Both writes go through one
    // connection, because the pragma that lets them in is per-connection.
    let mut conn = h.db.pool().acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE import_journal SET expected_byte_length = -1 WHERE id = ?1")
        .bind(intent.id.to_string())
        .execute(&mut *conn)
        .await
        .unwrap();

    // A negative length is not a length: it must read back as zero, so the
    // bytes on disk can never satisfy it.
    assert_eq!(
        h.journal.get(intent.id).await.unwrap().identity.byte_length,
        0
    );

    sqlx::query("UPDATE import_journal SET state = 'teleported' WHERE id = ?1")
        .bind(intent.id.to_string())
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    for outcome in [
        h.journal.get(intent.id).await.map(|intent| intent.state),
        h.journal.incomplete().await.map(|intents| intents[0].state),
    ] {
        assert!(
            matches!(outcome, Err(PersistenceError::Migration(_))),
            "a state no build understands must be refused: {outcome:?}"
        );
    }
    assert_eq!(h.asset_count().await, 0);
}

#[test]
fn journal_state_settlement_is_defined_for_every_variant() {
    let all = [
        JournalState::Staged,
        JournalState::Hashed,
        JournalState::Flushed,
        JournalState::IntentRecorded,
        JournalState::Renamed,
        JournalState::Committed,
        JournalState::Abandoned,
        JournalState::Quarantined,
    ];
    for state in all {
        assert_eq!(JournalState::parse(state.as_str()), Some(state));
    }

    let settled: Vec<JournalState> = all.into_iter().filter(|s| s.is_settled()).collect();
    assert_eq!(
        settled,
        vec![
            JournalState::Committed,
            JournalState::Abandoned,
            JournalState::Quarantined,
        ],
        "only these three states need no recovery"
    );
    for unsettled in [
        JournalState::Staged,
        JournalState::Hashed,
        JournalState::Flushed,
        JournalState::IntentRecorded,
        JournalState::Renamed,
    ] {
        assert!(!unsettled.is_settled(), "{unsettled:?}");
    }
    for unknown in ["", "COMMITTED", "renamed ", "teleported"] {
        assert_eq!(JournalState::parse(unknown), None, "{unknown:?}");
    }
}
