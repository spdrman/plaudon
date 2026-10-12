//! Single-writer SQLite coordinator (SPEC v1.3 §6.3A, §13.1).
//!
//! Every mutating statement the job engine makes funnels through one
//! serialized writer task that owns the only write connection it has. Readers
//! use a separate pool and run concurrently against WAL snapshots.
//!
//! The engine is not alone on the file: the desktop application keeps its own
//! pool over the same library, so the write lock *is* contended. Every
//! transaction here therefore starts with `BEGIN IMMEDIATE`
//! ([`plaudon_persistence::begin_write`]), which takes that lock up front
//! where SQLite's busy handler applies. A plain `BEGIN` would read first and
//! fail the later upgrade with `SQLITE_BUSY` — without consulting the busy
//! handler — as soon as the other pool committed in between.
//!
//! The coordinator lives in `plaudon-jobs` rather than `plaudon-persistence`
//! because the post-commit event dispatch it feeds (§14.5) is owned by the job
//! engine, and `plaudon-persistence` must not depend on `plaudon-jobs`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::sync::{mpsc, oneshot};

use crate::error::JobError;

/// Boxed future returned by a write closure.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A unit of serialized write work.
type WriteTask = Box<dyn FnOnce(SqlitePool) -> BoxFut<'static, ()> + Send>;

/// Handle to the single writer task.
#[derive(Clone)]
pub struct WriteCoordinator {
    tx: mpsc::Sender<WriteTask>,
    completed: Arc<AtomicU64>,
}

impl std::fmt::Debug for WriteCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteCoordinator")
            .field("completed_writes", &self.completed_writes())
            .finish()
    }
}

impl WriteCoordinator {
    /// Spawn the writer task on the current Tokio runtime.
    ///
    /// `pool` must be configured with exactly one connection; [`Database::open`]
    /// does this.
    pub fn spawn(pool: SqlitePool, queue_depth: usize) -> Self {
        let (tx, mut rx) = mpsc::channel::<WriteTask>(queue_depth.max(1));
        let completed = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&completed);
        tokio::spawn(async move {
            // Tasks are awaited one at a time: writes are totally ordered.
            while let Some(task) = rx.recv().await {
                task(pool.clone()).await;
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        Self { tx, completed }
    }

    /// Number of completed write operations. Used by tests to prove that every
    /// write went through this coordinator.
    pub fn completed_writes(&self) -> u64 {
        self.completed.load(Ordering::Relaxed)
    }

    /// Run `f` on the single write connection, awaiting its result.
    pub async fn submit<T, F>(&self, f: F) -> Result<T, JobError>
    where
        T: Send + 'static,
        F: FnOnce(PoolConnection<Sqlite>) -> BoxFut<'static, Result<T, JobError>> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let task: WriteTask = Box::new(move |pool: SqlitePool| {
            Box::pin(async move {
                let result = match pool.acquire().await {
                    Ok(conn) => f(conn).await,
                    Err(error) => Err(JobError::storage("acquire write connection", error)),
                };
                // A dropped receiver means the caller gave up; nothing to do.
                let _ = reply_tx.send(result);
            })
        });
        self.tx
            .send(task)
            .await
            .map_err(|_| JobError::WriterClosed)?;
        reply_rx.await.map_err(|_| JobError::WriterClosed)?
    }

    /// Run `f` inside a single transaction on the write connection.
    ///
    /// The transaction commits when `f` returns `Ok` and rolls back on `Err`.
    /// Nothing observable escapes a rolled back transaction, including the
    /// event sequence counter.
    pub async fn transaction<T, F>(&self, f: F) -> Result<T, JobError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut Transaction<'static, Sqlite>) -> BoxFut<'a, Result<T, JobError>>
            + Send
            + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let task: WriteTask = Box::new(move |pool: SqlitePool| {
            Box::pin(async move {
                let result = run_transaction(pool, f).await;
                let _ = reply_tx.send(result);
            })
        });
        self.tx
            .send(task)
            .await
            .map_err(|_| JobError::WriterClosed)?;
        reply_rx.await.map_err(|_| JobError::WriterClosed)?
    }
}

async fn run_transaction<T, F>(pool: SqlitePool, f: F) -> Result<T, JobError>
where
    F: for<'a> FnOnce(&'a mut Transaction<'static, Sqlite>) -> BoxFut<'a, Result<T, JobError>>,
{
    // `BEGIN IMMEDIATE`, not `BEGIN`: the application's own pool writes the
    // same library file, and a deferred transaction that reads before it
    // writes cannot take the write lock afterwards — SQLite fails the upgrade
    // with `SQLITE_BUSY` without consulting the busy handler as soon as
    // anyone committed since the read snapshot. Taking the lock up front is
    // what makes `busy_timeout` absorb the contention.
    let mut tx = plaudon_persistence::begin_write(&pool)
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
}

/// Concurrent read-only access. Readers never block the writer under WAL.
#[derive(Clone, Debug)]
pub struct ReadPool(SqlitePool);

impl ReadPool {
    /// Borrow the underlying pool for query execution.
    pub fn pool(&self) -> &SqlitePool {
        &self.0
    }
}

/// An opened SQLite database with a single writer and a reader pool.
#[derive(Clone, Debug)]
pub struct Database {
    writer: WriteCoordinator,
    reader: ReadPool,
    write_pool: SqlitePool,
    path: std::path::PathBuf,
}

impl Database {
    /// Open (creating if absent) the database at `path`.
    ///
    /// Applies the §13.1 storage policy: WAL journaling, enforced foreign keys,
    /// `synchronous = FULL` on the writer so a commit is durable before we
    /// report it, and a busy timeout as defence in depth.
    pub async fn open(path: impl AsRef<Path>, max_readers: u32) -> Result<Self, JobError> {
        let base = SqliteConnectOptions::new()
            .filename(path.as_ref())
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
            .connect_with(base.synchronous(SqliteSynchronous::Normal))
            .await
            .map_err(|e| JobError::storage("open read pool", e))?;

        Ok(Self {
            writer: WriteCoordinator::spawn(write_pool.clone(), 256),
            reader: ReadPool(read_pool),
            write_pool,
            path: path.as_ref().to_path_buf(),
        })
    }

    /// Filesystem path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
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
