//! The egui shell opens a library the Tauri shell created, unchanged (O2.7).
//!
//! What is true only here: every fixture database is built the way the Tauri
//! shell built it — [`Database::open`] on `<data dir>/app.sqlite` followed by
//! [`MigrationRunner::run`] — and is then re-opened through
//! [`AppEngine::start`], which is the only path the egui binary has. So a
//! second shell that quietly used a different file name, a different archive
//! layout or a different migration history would fail here rather than in a
//! user's data directory.
//!
//! The data is real: a WAV is synthesised on disk and imported through
//! `AppCommand::ImportFiles`, so a recording row, a media asset row and an
//! archived blob genuinely exist before the engine is restarted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use plaudon_app::backend::store::DATABASE_FILE_NAME;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    DuplicatePolicy, GetRecordingRequest, ImportFilesRequest, ImportOutcome, ListRecordingsRequest,
    RecordingFilter, RecordingPage, RecordingSort, RecordingSortField, RecordingSummary,
    SortDirection,
};
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_domain::media::MediaIdentity;
use plaudon_persistence::archive::relative_path;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::{AppliedMigration, MigrationRunner, MIGRATIONS};
use sqlx::Row as _;

/// Long enough for the import worker to have committed, short enough that a
/// hung task fails the test instead of hanging it.
const GRACE: Duration = Duration::from_secs(10);

/* ------------------------------------------------------------- fixtures */

/// The exact bytes the Tauri shell would have been handed by a file picker.
fn wav(payload: usize) -> Vec<u8> {
    let data_len = u32::try_from(payload).expect("payload fits in a RIFF chunk");
    let mut bytes = Vec::with_capacity(44 + payload);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&32_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + payload, 0x11);
    bytes
}

/// A throwaway runtime for the direct-database steps, which are the ones the
/// Tauri shell performed before it ever built an engine.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds")
}

/// Create the library exactly as `apps/desktop/src-tauri` created it: open the
/// database file by the name `Store::open` uses, then run every embedded
/// migration. Nothing else is written, so the directory afterwards holds only
/// what the Tauri shell would have left behind.
fn create_tauri_library(data_dir: &Path) {
    runtime().block_on(async {
        let db = Database::open(data_dir.join(DATABASE_FILE_NAME))
            .await
            .expect("the Tauri-style database opens");
        MigrationRunner::new()
            .run(&db)
            .await
            .expect("the embedded migrations apply");
        db.close().await;
    });
}

fn start_engine(data_dir: &Path, approved_roots: Vec<PathBuf>) -> AppEngine {
    AppEngine::start(
        EngineConfig::new(data_dir.to_path_buf())
            .with_secrets(Arc::new(InMemorySecretStore::new()))
            .with_notifier(Arc::new(UnsupportedNotifier))
            .with_worker_threads(2)
            .with_approved_roots(approved_roots),
    )
    .expect("the engine opens the existing library")
}

fn call(engine: &AppEngine, command: AppCommand) -> AppResponse {
    engine
        .dispatcher()
        .call_blocking(command)
        .expect("the command succeeds")
}

fn list(engine: &AppEngine) -> RecordingPage {
    match call(
        engine,
        AppCommand::ListRecordings(ListRecordingsRequest {
            cursor: None,
            limit: 50,
            filter: RecordingFilter {
                query: None,
                import_status: Vec::new(),
                transcription_status: Vec::new(),
                sources: Vec::new(),
                tags: Vec::new(),
                date_from: None,
                date_to: None,
            },
            sort: RecordingSort {
                field: RecordingSortField::ImportedAt,
                direction: SortDirection::Desc,
            },
        }),
    ) {
        AppResponse::ListRecordings(page) => page,
        other => panic!("unexpected response: {other:?}"),
    }
}

/// A Tauri-created library that already holds one imported recording.
#[derive(Debug)]
struct TauriLibrary {
    data_dir: tempfile::TempDir,
    /// The picker directory, kept alive so the imported source file outlives
    /// the fixture and stays an approved import root.
    sources: tempfile::TempDir,
    audio: Vec<u8>,
    summary: RecordingSummary,
}

impl TauriLibrary {
    /// Roots the engine will accept imports from: the library itself, as the
    /// egui shell configures it, plus the picker directory.
    fn approved_roots(&self) -> Vec<PathBuf> {
        vec![
            self.data_dir.path().to_path_buf(),
            self.sources.path().to_path_buf(),
        ]
    }

    fn reopen(&self) -> AppEngine {
        start_engine(self.data_dir.path(), self.approved_roots())
    }
}

fn tauri_library_with_one_recording() -> TauriLibrary {
    let data_dir = tempfile::tempdir().expect("a temporary library directory");
    let sources = tempfile::tempdir().expect("a temporary picker directory");
    create_tauri_library(data_dir.path());

    let audio = wav(512);
    let source = sources.path().join("board-call.wav");
    std::fs::write(&source, &audio).expect("the source file is written");

    let engine = start_engine(
        data_dir.path(),
        vec![data_dir.path().to_path_buf(), sources.path().to_path_buf()],
    );
    let AppResponse::ImportFiles(imported) = call(
        &engine,
        AppCommand::ImportFiles(ImportFilesRequest {
            paths: vec![source.to_string_lossy().into_owned()],
            duplicate_policy: DuplicatePolicy::Ask,
        }),
    ) else {
        panic!("import_files answered with another response")
    };
    assert!(
        matches!(
            imported.results.as_slice(),
            [ImportOutcome::Accepted { .. }]
        ),
        "the fixture import was not accepted: {:?}",
        imported.results
    );

    let page = list(&engine);
    let summary = page
        .items
        .first()
        .cloned()
        .expect("the imported recording is listed before shutdown");
    engine.shutdown(GRACE);

    TauriLibrary {
        data_dir,
        sources,
        audio,
        summary,
    }
}

/* ------------------------------------------------- migration-history reads */

fn applied_migrations(data_dir: &Path) -> Vec<AppliedMigration> {
    runtime().block_on(async {
        let db = Database::open(data_dir.join(DATABASE_FILE_NAME))
            .await
            .expect("the library database opens for inspection");
        let rows = MigrationRunner::applied(&db)
            .await
            .expect("the migration history reads");
        db.close().await;
        rows
    })
}

/// The columns `schema_migrations` actually has, so the comparison below is
/// known to cover the whole row rather than a guessed subset.
fn history_table_columns(data_dir: &Path) -> Vec<String> {
    runtime().block_on(async {
        let db = Database::open(data_dir.join(DATABASE_FILE_NAME))
            .await
            .expect("the library database opens for inspection");
        let rows = sqlx::query("PRAGMA table_info(schema_migrations)")
            .fetch_all(db.pool())
            .await
            .expect("the history table is introspectable");
        db.close().await;
        rows.into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect()
    })
}

fn versions(rows: &[AppliedMigration]) -> Vec<i64> {
    rows.iter().map(|row| row.version).collect()
}

fn assert_strictly_ascending(rows: &[AppliedMigration]) {
    for pair in rows.windows(2) {
        assert!(
            pair[0].version < pair[1].version,
            "migration versions are not strictly ascending: {:?}",
            versions(rows)
        );
    }
}

/* ------------------------------------------------------ filesystem probing */

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("the library directory reads") {
            let entry = entry.expect("a directory entry reads");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Every file that is itself a SQLite library, which is how a second, parallel
/// library would show up. The `-wal` and `-shm` companions of `app.sqlite` have
/// their own extensions and are deliberately not counted.
fn sqlite_files(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = files_under(root)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "sqlite"))
        .collect();
    found.sort();
    found
}

/* ================================================================ nothing lost */

#[test]
fn a_library_the_tauri_shell_created_reopens_in_the_egui_shell_with_the_recording_intact() {
    let fixture = tauri_library_with_one_recording();
    let before = fixture.summary.clone();

    let engine = fixture.reopen();

    let page = list(&engine);
    assert_eq!(page.total, 1, "the re-opened library lost or gained rows");
    let after = page
        .items
        .first()
        .cloned()
        .expect("the recording survives the re-open");

    // Whole-row equality is the strongest honest claim: it covers the
    // recording id, the title, the digest and the byte length the ticket
    // names, and every other column of the summary besides.
    assert_eq!(
        after, before,
        "the re-opened library reports a different recording row"
    );
    assert_eq!(after.recording_id, before.recording_id);
    assert_eq!(after.title, before.title);
    assert_eq!(after.sha256, before.sha256);
    assert_eq!(after.byte_length, before.byte_length);
    assert_eq!(
        after.byte_length,
        fixture.audio.len() as u64,
        "the archived length is not the length of the imported file"
    );

    let AppResponse::GetRecording(detail) = call(
        &engine,
        AppCommand::GetRecording(GetRecordingRequest {
            recording_id: before.recording_id.to_string(),
        }),
    ) else {
        panic!("get_recording answered with another response")
    };
    assert_eq!(detail.summary, before, "get_recording no longer resolves");

    let media = engine
        .handle()
        .block_on(plaudon_app::media::resolve_media(
            &engine.ports().library,
            before.recording_id,
        ))
        .expect("the archived blob still resolves");
    assert_eq!(media.byte_length, before.byte_length);
    assert!(
        media.path.is_file(),
        "the archived blob is gone from disk: {}",
        media.path.display()
    );
    let on_disk = std::fs::read(&media.path).expect("the archived blob reads");
    assert_eq!(
        on_disk.len() as u64,
        before.byte_length,
        "the archived blob changed length across the re-open"
    );
    assert_eq!(
        on_disk, fixture.audio,
        "the archived blob is no longer the imported bytes"
    );

    engine.shutdown(GRACE);
}

/* ========================================================= forward-only only */

#[test]
fn reopening_a_migrated_library_applies_no_migration_and_rewrites_no_history_row() {
    let fixture = tauri_library_with_one_recording();
    let data_dir = fixture.data_dir.path();

    // Read the history schema rather than assuming it: the equality below is
    // only a "byte-identical row" claim if these are all the columns there are.
    let mut columns = history_table_columns(data_dir);
    columns.sort();
    assert_eq!(
        columns,
        vec![
            "applied_at".to_owned(),
            "checksum".to_owned(),
            "name".to_owned(),
            "version".to_owned(),
        ],
        "schema_migrations has columns this test does not compare"
    );

    let before = applied_migrations(data_dir);
    assert_eq!(
        versions(&before),
        MIGRATIONS.iter().map(|m| m.version).collect::<Vec<_>>(),
        "the fixture is not at the build's migration head"
    );
    assert_strictly_ascending(&before);

    fixture.reopen().shutdown(GRACE);

    let after = applied_migrations(data_dir);
    // Every column of every row, including `applied_at`: a re-application
    // would move it, a checksum rewrite would change `checksum`, and a removed
    // row would shorten the vector.
    assert_eq!(
        after, before,
        "the egui shell rewrote the migration history it inherited"
    );
    assert_strictly_ascending(&after);

    // `MigrationRunner` backs a non-empty library up before it upgrades it, so
    // the absence of a backup is independent evidence that nothing was applied.
    let backups: Vec<PathBuf> = files_under(data_dir)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".pre-migration-"))
        })
        .collect();
    assert!(
        backups.is_empty(),
        "an up-to-date library was treated as an upgrade: {backups:?}"
    );
}

/* ====================================================== an older library upgrades */

/// The `recordings` row written into the older fixture, read back column for
/// column. `recordings` is created by migration 1 and is never `ALTER`ed by any
/// later migration, so the row is comparable before and after the upgrade.
#[derive(Debug, PartialEq, Eq)]
struct LegacyRecording {
    id: String,
    title: String,
    captured_at: Option<i64>,
    duration_ms: Option<i64>,
    preferred_media_asset_id: Option<String>,
    annotation: Option<String>,
    created_at: i64,
    deleted_at: Option<i64>,
}

const LEGACY_ID: &str = "7a0f1d2c-9b84-4f3e-8a17-2c5d6e7f8091";
const LEGACY_TITLE: &str = "Board call, migrated from an older build";

fn read_legacy_recording(data_dir: &Path) -> LegacyRecording {
    runtime().block_on(async {
        let db = Database::open(data_dir.join(DATABASE_FILE_NAME))
            .await
            .expect("the older library opens for inspection");
        let row = sqlx::query(
            "SELECT id, title, captured_at, duration_ms, preferred_media_asset_id, \
             annotation, created_at, deleted_at FROM recordings WHERE id = ?1",
        )
        .bind(LEGACY_ID)
        .fetch_one(db.pool())
        .await
        .expect("the row written before the upgrade is still there");
        let parsed = LegacyRecording {
            id: row.get("id"),
            title: row.get("title"),
            captured_at: row.get("captured_at"),
            duration_ms: row.get("duration_ms"),
            preferred_media_asset_id: row.get("preferred_media_asset_id"),
            annotation: row.get("annotation"),
            created_at: row.get("created_at"),
            deleted_at: row.get("deleted_at"),
        };
        db.close().await;
        parsed
    })
}

#[test]
fn a_library_from_an_older_build_is_migrated_forward_and_keeps_the_rows_it_already_had() {
    // The largest honest prefix: every migration but the last. `recordings`
    // exists from migration 1 onwards, so a shorter prefix would prove strictly
    // less, and the full list would not be an older library at all.
    let prefix = MIGRATIONS.len() - 1;
    let head = MIGRATIONS[prefix - 1].version;
    let pending = MIGRATIONS[prefix].version;

    let data_dir = tempfile::tempdir().expect("a temporary library directory");
    runtime().block_on(async {
        let db = Database::open(data_dir.path().join(DATABASE_FILE_NAME))
            .await
            .expect("the older database opens");
        MigrationRunner::with_migrations(MIGRATIONS[..prefix].to_vec())
            .run(&db)
            .await
            .expect("the older migration prefix applies");
        sqlx::query(
            "INSERT INTO recordings (id, title, captured_at, duration_ms, \
             preferred_media_asset_id, annotation, created_at, deleted_at) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL)",
        )
        .bind(LEGACY_ID)
        .bind(LEGACY_TITLE)
        .bind(1_700_000_000_000i64)
        .bind(61_000i64)
        .bind("written by the older build")
        .bind(1_700_000_001_000i64)
        .execute(db.pool())
        .await
        .expect("a row the upgrade must preserve is written");
        db.close().await;
    });

    let before_rows = applied_migrations(data_dir.path());
    assert_eq!(
        before_rows.len(),
        prefix,
        "the fixture is not an older build"
    );
    let before_row = read_legacy_recording(data_dir.path());

    // The egui binary's own path, over a library it did not create.
    start_engine(data_dir.path(), vec![data_dir.path().to_path_buf()]).shutdown(GRACE);

    let after_rows = applied_migrations(data_dir.path());
    assert_eq!(
        &after_rows[..prefix],
        &before_rows[..],
        "the already-applied migrations were rewritten by the upgrade"
    );
    assert_eq!(
        versions(&after_rows),
        MIGRATIONS.iter().map(|m| m.version).collect::<Vec<_>>(),
        "the older library was not brought to this build's head"
    );
    assert_strictly_ascending(&after_rows);
    let appended = versions(&after_rows[prefix..]);
    assert_eq!(
        appended,
        vec![pending],
        "something other than the pending migration was applied"
    );
    assert!(
        appended.iter().all(|version| *version > head),
        "a migration was applied below the inherited head {head}"
    );

    assert_eq!(
        read_legacy_recording(data_dir.path()),
        before_row,
        "the upgrade changed a row the older build had written"
    );

    // Spec 13.3A: an upgrade of a non-empty library backs it up first, under
    // the inherited head's version.
    let backup = data_dir
        .path()
        .join(format!("{DATABASE_FILE_NAME}.pre-migration-{head}.bak"));
    assert!(
        backup.is_file(),
        "no pre-upgrade backup was taken at {}",
        backup.display()
    );
}

/* ================================================================== layout */

#[test]
fn the_egui_shell_reuses_the_tauri_file_names_and_builds_no_parallel_library() {
    let fixture = tauri_library_with_one_recording();
    let data_dir = fixture.data_dir.path();
    let database = data_dir.join(DATABASE_FILE_NAME);

    // What the Tauri-created fixture plus one import left behind.
    assert!(
        database.is_file(),
        "the Tauri-created database is not at {}",
        database.display()
    );
    assert_eq!(
        sqlite_files(data_dir),
        vec![database.clone()],
        "the fixture already holds more than one library"
    );
    assert!(
        data_dir.join("media/sha256").is_dir(),
        "the archive directory `media/sha256` is missing"
    );
    assert!(
        data_dir.join("temp/imports").is_dir(),
        "the archive staging directory `temp/imports` is missing"
    );

    let identity = MediaIdentity::new(
        fixture.summary.sha256,
        fixture.summary.byte_length,
        String::new(),
    );
    let blob = data_dir.join(relative_path(&identity, "wav"));
    assert!(
        blob.is_file(),
        "the imported blob is not at the content-addressed path {}",
        blob.display()
    );

    let engine = fixture.reopen();
    assert_eq!(
        engine.data_dir(),
        data_dir,
        "the engine adopted a different data directory"
    );

    let media = engine
        .handle()
        .block_on(plaudon_app::media::resolve_media(
            &engine.ports().library,
            fixture.summary.recording_id,
        ))
        .expect("the archived blob still resolves");
    assert_eq!(
        media.path, blob,
        "the re-opened engine resolves the blob somewhere else"
    );
    engine.shutdown(GRACE);

    // Still exactly one library, still the inherited one, and no upgrade
    // backup: nothing parallel was created beside the fixture.
    assert_eq!(
        sqlite_files(data_dir),
        vec![database],
        "the egui shell created a second library beside the Tauri one"
    );
    assert!(
        data_dir.join("media/sha256").is_dir() && data_dir.join("temp/imports").is_dir(),
        "the archive directories were replaced rather than reused"
    );
    assert!(blob.is_file(), "the archived blob moved across the re-open");
}
