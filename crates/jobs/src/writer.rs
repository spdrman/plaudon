//! Single-writer SQLite coordinator (SPEC v1.3 §6.3A, §13.1).
//!
//! Every mutating statement in the application funnels through one serialized
//! writer task that owns the only write connection. Readers use a separate
//! pool and run concurrently against WAL snapshots. Because exactly one
//! connection ever holds the write lock, `SQLITE_BUSY` cannot arise from our
//! own concurrency.
//!
//! The coordinator lives in `plaudon-jobs` rather than `plaudon-persistence`
//! because the post-commit event dispatch it feeds (§14.5) is owned by the job
//! engine, and `plaudon-persistence` must not depend on `plaudon-jobs`. See
//! `docs/adr/0030-single-writer-coordinator.md`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};
use tokio::sync::{mpsc, oneshot};

use crate::error::JobError;

/// Boxed future returned by a write closure.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A unit of serialized write work.
type WriteTask = Box<dyn for<'a> FnOnce(&'a mut SqliteConnection) -> BoxFut<'a, ()> + Send>;

/// Handle to the single writer task.
#[derive(Clone)]
pub struct WriteCoordinator {
    tx: mpsc::Sender<WriteTask>,
    inner: Arc<WriterInner>,
}

struct WriterInner {
    /// Number of write operations completed; used by tests to prove that all
    /// writes went through this coordinator.
    completed: std::sync::atomic::AtomicU64,
}

impl WriteCoordinator {
    /// Spawn the writer task on the current Tokio runtime.
    ///
    /// `pool` must be configured with exactly one connection; `Database::open`
    /// does this.
    pub fn spawn(pool: SqlitePool, queue_depth: usize) -> Self {
        let (tx, mut rx) = mpsc::channel::<WriteTask>(queue_depth.max(1));
        let inner = Arc::new(WriterInner {
            completed: std::sync::atomic::AtomicU64::new(0),
        });
        let counter = Arc::clone(&inner);
        tokio::spawn(async move {
            // One connection, held for the lifetime of the writer. Nothing else
            // in the process may acquire a write connection.
            let mut conn = match pool.acquire().await {
                Ok(conn) => conn,
                Err(error) => {
                    tracing::error!(%error, "single-writer coordinator could not acquire its connection");
                    return;
                }
            };
            while let Some(task) = rx.recv().await {
                task(&mut conn).await;
                counter
                    .completed
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        Self { tx, inner }
    }

    /// Number of completed write operations.
    pub fn completed_writes(&self) -> u64 {
        self.inner
            .completed
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Run `f` on the single write connection, awaiting its result.
    pub async fn submit<T, F>(&self, f: F) -> Result<T, JobError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut SqliteConnection) -> BoxFut<'a, Result<T, JobError>>
            + Send
            + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let task: WriteTask = Box::new(move |conn| {
            Box::pin(async move {
                let result = f(conn).await;
                // Receiver dropped means the caller gave up; nothing to do.
                let _ = reply_tx.send(result);
            })
        });
        self.tx.send(task).await.map_err(|_| JobError::WriterClosed)?;
        reply_rx.await.map_err(|_| JobError::WriterClosed)?
    }

    /// Run `f` inside a single transaction on the write connection.
    ///
    /// The transaction is committed when `f` returns `Ok`, and rolled back on
    /// `Err` or panic-free early return. Nothing observable escapes a rolled
    /// back transaction.
    pub async fn transaction<T, F>(&self, f: F) -> Result<T, JobError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut Transaction<'static, Sqlite>) -> BoxFut<'a, Result<T, JobError>>
            + Send
            + 'static,
    {
        self.submit(move |conn| {
            Box::pin(async move {
                let mut tx = <&mut SqliteConnection as sqlx::Acquire>::begin(conn)
                    .await
                    .map_err(|e| JobError::storage("begin transaction", e))?;
                match f(&mut tx).await {
                    Ok(value) => {
                        crate::fault::trip("tx:before_commit");
                        tx.commit()
                            .await
                            .map_err(|e| JobError::storage("commit transaction", e))?;
                        crate::fault::trip("tx:after_commit");
                        Ok(value)
                    }
                    Err(error) => {
                        tx.rollback()
                            .await
                            .map_err(|e| JobError::storage("rollback transaction", e))?;
                        Err(error)
                    }
                }
            })
        })
        .await
    }
}

/// Concurrent read-only access. Readers never block the writer under WAL.
#[derive(Clone)]
pub struct ReadPool(SqlitePool);

impl ReadPool {
    /// Borrow the underlying pool for query execution.
    pub fn pool(&self) -> &SqlitePool {
        &self.0
    }
}

/// An opened SQLite database with a single writer and a reader pool.
#[derive(Clone)]
pub struct Database {
    writer: WriteCoordinator,
    reader: ReadPool,
    write_pool: SqlitePool,
}

impl Database {
    /// Open (creating if absent) the database at `path`.
    ///
    /// Applies the §13.1 storage policy: WAL journaling, enforced foreign keys,
    /// `synchronous = FULL` on the writer so a commit is durable before we
    /// report it, and a busy timeout as defence in depth.
    pub async fn open(path: impl AsRef<Path>, max_readers: u32) -> Result<Self, JobError> {
        let path = path.as_ref();
        let base = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(10));

        let write_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .connect_with(base.clone().synchronous(SqliteSynchronous::Full))
            .await
            .map_err(|e| JobError::storage("open write pool", e))?;

        let read_pool = SqlitePoolOptions::new()
            .max_connections(max_readers.max(1))
            .connect_with(base.read_only(false).synchronous(SqliteSynchronous::Normal))
            .await
            .map_err(|e| JobError::storage("open read pool", e))?;

        Ok(Self {
            writer: WriteCoordinator::spawn(write_pool.clone(), 256),
            reader: ReadPool(read_pool),
            write_pool,
        })
    }

    /// The single-writer handle.
    pub fn writer(&self) -> &WriteCoordinator {
        &self.writer
    }

    /// The concurrent reader pool.
    pub fn reader(&self) -> &ReadPool {
        &self.reader
    }

    /// Close both pools, flushing WAL state.
    pub async fn close(&self) {
        self.write_pool.close().await;
        self.reader.0.close().await;
    }
}
