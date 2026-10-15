//! Failure paths of the single-writer coordinator (SPEC v1.3 §6.3A, §13.1).

mod common;

use common::{enqueue, fixture};

use plaudon_domain::error::AppErrorCode;
use plaudon_jobs::error::JobError;
use plaudon_jobs::writer::{BoxFut, Database, WriteCoordinator};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Sqlite, Transaction};

/// A write that does nothing, used to prove the coordinator's own failure
/// paths rather than a statement's.
fn noop_write(_conn: PoolConnection<Sqlite>) -> BoxFut<'static, Result<(), JobError>> {
    Box::pin(async { Ok(()) })
}

/// A transaction body that does nothing and commits.
fn noop_transaction<'a>(
    _tx: &'a mut Transaction<'static, Sqlite>,
) -> BoxFut<'a, Result<(), JobError>> {
    Box::pin(async { Ok(()) })
}

#[tokio::test]
async fn opening_a_database_under_a_regular_file_is_a_storage_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"regular file").expect("blocker");

    let error = Database::open(blocker.join("app.sqlite"), 2)
        .await
        .expect_err("must fail");

    assert!(
        matches!(&error, JobError::Storage { context, .. } if *context == "open write pool"),
        "{error:?}"
    );
    assert_eq!(error.code(), AppErrorCode::StorageUnavailable);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn a_reader_pool_always_has_at_least_one_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("app.sqlite");
    let db = Database::open(&path, 0).await.expect("open");

    assert_eq!(db.path(), path);
    let one: i64 = sqlx::query_scalar("SELECT 1")
        .fetch_one(db.reader().pool())
        .await
        .expect("read");
    assert_eq!(one, 1);
    db.close().await;
}

#[tokio::test]
async fn a_closed_pool_reports_a_storage_failure_instead_of_hanging() {
    let f = fixture().await;
    f.store.database().close().await;

    let error = f
        .store
        .database()
        .writer()
        .submit(|mut conn| {
            Box::pin(async move {
                sqlx::query("SELECT 1")
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("unreachable", e))?;
                Ok(())
            })
        })
        .await
        .expect_err("must fail");
    assert!(
        matches!(&error, JobError::Storage { context, .. }
            if *context == "acquire write connection"),
        "{error:?}"
    );

    let error = f
        .store
        .database()
        .writer()
        .transaction(noop_transaction)
        .await
        .expect_err("must fail");
    assert!(
        matches!(&error, JobError::Storage { context, .. } if *context == "begin transaction"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_transaction_that_returns_an_error_rolls_back_everything_it_wrote() {
    let f = fixture().await;
    let recording = plaudon_domain::ids::RecordingId::new();
    let id = recording.to_string();

    let error = f
        .store
        .database()
        .writer()
        .transaction(move |tx| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO tombstones (id, entity_type, entity_id, reason, created_at)
                     VALUES (?1, 'recording', ?1, 'rolled back', 1)",
                )
                .bind(&id)
                .execute(&mut **tx)
                .await
                .map_err(|e| JobError::storage("harness insert", e))?;
                Err::<(), JobError>(JobError::CorruptRecord("deliberate".to_string()))
            })
        })
        .await
        .expect_err("must surface the closure's error");

    assert!(
        matches!(&error, JobError::CorruptRecord(d) if d == "deliberate"),
        "{error:?}"
    );
    assert!(
        !f.store.is_tombstoned(recording).await.expect("check"),
        "a rolled back transaction leaves nothing observable"
    );
}

#[tokio::test]
async fn the_coordinator_counts_every_completed_write() {
    let f = fixture().await;
    let before = f.store.database().writer().completed_writes();

    enqueue(&f.store, 'a').await;

    let after = f.store.database().writer().completed_writes();
    assert!(after > before, "{before} -> {after}");
    assert!(format!("{:?}", f.store.database().writer()).contains("completed_writes"));
    assert!(format!("{:?}", f.store.database()).contains("Database"));
}

#[test]
fn a_coordinator_whose_task_is_gone_reports_the_writer_as_closed() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (coordinator, pool) = runtime.block_on(async {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        (WriteCoordinator::spawn(pool.clone(), 0), pool)
    });

    // Dropping the runtime drops the writer task, and with it the receiving
    // half of the work queue.
    drop(runtime);

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let error = runtime
        .block_on(coordinator.submit(noop_write))
        .expect_err("must fail");
    assert!(matches!(error, JobError::WriterClosed), "{error:?}");
    assert_eq!(error.code(), AppErrorCode::StorageUnavailable);
    assert!(error.is_retryable());

    let error = runtime
        .block_on(coordinator.transaction(noop_transaction))
        .expect_err("must fail");
    assert!(matches!(error, JobError::WriterClosed), "{error:?}");

    runtime.block_on(async move { pool.close().await });
}
