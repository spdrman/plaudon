//! Shared fixtures for the `plaudon-jobs` integration suites.
//!
//! This module is compiled into every test target that declares `mod common;`,
//! so a helper used by one suite is unused in another. The allow is scoped to
//! this harness module and never applies to shipping code.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use plaudon_domain::ids::JobId;
use plaudon_jobs::engine::{EngineConfig, JobEngine};
use plaudon_jobs::error::JobError;
use plaudon_jobs::events::EventSubscriber;
use plaudon_jobs::key::{ImportJobKey, SourceKind};
use plaudon_jobs::scheduler::{ResourceClass, SchedulerLimits};
use plaudon_jobs::state::{JobState, TransitionCause};
use plaudon_jobs::store::{EnqueueRequest, JobStore};
use plaudon_jobs::writer::Database;

/// A migrated store over a throwaway directory.
pub struct Fixture {
    /// Kept alive for the lifetime of the test; dropping it removes the tree.
    pub dir: tempfile::TempDir,
    pub db_path: PathBuf,
    pub staging: PathBuf,
    pub archive: PathBuf,
    pub store: JobStore,
}

/// Open a migrated store with a staging and an archive root beside it.
pub async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("app.sqlite");
    let archive = dir.path().join("library");
    let staging = dir.path().join("temp/imports");
    std::fs::create_dir_all(&staging).expect("staging dir");
    std::fs::create_dir_all(&archive).expect("archive dir");
    let store = JobStore::open_at(&db_path).await.expect("open store");
    Fixture {
        dir,
        db_path,
        staging,
        archive,
        store,
    }
}

/// An import request whose key is distinguished only by `seed`.
pub fn import_request(seed: char) -> EnqueueRequest {
    EnqueueRequest::new(
        "import",
        ImportJobKey::by_content(SourceKind::LocalFile, seed.to_string().repeat(64)),
        ResourceClass::LocalDisk,
    )
}

/// Enqueue a fresh import job keyed on `seed`.
pub async fn enqueue(store: &JobStore, seed: char) -> JobId {
    store
        .enqueue(import_request(seed))
        .await
        .expect("enqueue")
        .job_id()
}

/// Drive `job` through `states` in order, asserting every step is accepted.
pub async fn advance(store: &JobStore, job: JobId, states: &[JobState]) {
    for state in states {
        store
            .transition(job, *state, TransitionCause::system("harness"))
            .await
            .unwrap_or_else(|error| panic!("transition into {state} must succeed: {error}"));
    }
}

/// Run one statement on the single write connection, binding `binds` in order.
///
/// Used to plant deliberately corrupt rows so the read paths' integrity checks
/// are exercised against real SQLite data rather than a mock.
pub async fn raw_write(store: &JobStore, sql: &'static str, binds: Vec<String>) {
    store
        .database()
        .writer()
        .submit(move |mut conn| {
            Box::pin(async move {
                let mut query = sqlx::query(sql);
                for bind in binds {
                    query = query.bind(bind);
                }
                query
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("harness raw write", e))?;
                Ok(())
            })
        })
        .await
        .expect("raw write");
}

/// Run one statement with the schema's CHECK and foreign-key enforcement
/// suspended, so a deliberately corrupt row can be planted.
///
/// The pragmas are connection scoped and the write pool holds exactly one
/// connection, so they are restored before the closure returns.
pub async fn corrupt_write(store: &JobStore, sql: &'static str, binds: Vec<String>) {
    store
        .database()
        .writer()
        .submit(move |mut conn| {
            Box::pin(async move {
                for pragma in [
                    "PRAGMA foreign_keys = OFF",
                    "PRAGMA ignore_check_constraints = ON",
                ] {
                    sqlx::query(pragma)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| JobError::storage("harness pragma", e))?;
                }
                let mut query = sqlx::query(sql);
                for bind in binds {
                    query = query.bind(bind);
                }
                let planted = query
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| JobError::storage("harness corrupt write", e));
                for pragma in [
                    "PRAGMA foreign_keys = ON",
                    "PRAGMA ignore_check_constraints = OFF",
                ] {
                    sqlx::query(pragma)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| JobError::storage("harness pragma", e))?;
                }
                planted?;
                Ok(())
            })
        })
        .await
        .expect("corrupt write");
}

/// An engine over the fixture's database, without an archive root.
pub async fn engine_for(f: &Fixture) -> JobEngine {
    open_engine(f, EngineConfig::new(&f.staging), Vec::new()).await
}

/// An engine whose startup recovery also reconciles the managed archive.
pub async fn engine_with_archive(f: &Fixture) -> JobEngine {
    open_engine(
        f,
        EngineConfig::new(&f.staging).with_archive_root(&f.archive),
        Vec::new(),
    )
    .await
}

/// An engine that fans committed events out to `subscribers`.
pub async fn engine_with_subscribers(
    f: &Fixture,
    subscribers: Vec<Arc<dyn EventSubscriber>>,
) -> JobEngine {
    open_engine(f, EngineConfig::new(&f.staging), subscribers).await
}

/// An engine whose scheduler is configured with `limits`.
pub async fn engine_with_limits(f: &Fixture, limits: SchedulerLimits) -> JobEngine {
    let mut config = EngineConfig::new(&f.staging);
    config.limits = limits;
    open_engine(f, config, Vec::new()).await
}

async fn open_engine(
    f: &Fixture,
    config: EngineConfig,
    subscribers: Vec<Arc<dyn EventSubscriber>>,
) -> JobEngine {
    let db = Database::open(&f.db_path, 4).await.expect("open database");
    JobEngine::open(db, config, subscribers)
        .await
        .expect("open engine")
}
