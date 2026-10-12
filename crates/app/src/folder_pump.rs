//! The watched-folder timer (parity matrix row D18).
//!
//! `plaudon_source_folder::WatchedFolderService` is poll-driven: it imports
//! what appeared in a folder when someone calls `sync`, and nothing else calls
//! it. The Tauri shell never did, so a watched folder only ever imported when
//! the user pressed a control.
//!
//! This is that caller, and it is deliberately **not** the render loop: it is a
//! task on the application's own Tokio runtime that dispatches
//! `source_sync { trigger: background }` for the folders it has been told
//! about. A render thread therefore never waits for a folder sweep, and a
//! sweep still goes through the ordinary validated command path, with the
//! ordinary job, events and rule enforcement.
//!
//! Scope, stated honestly: the pump sweeps every watched folder the library
//! stores that the user has not paused, read back through
//! [`crate::ports::SourcesPort::watched_folder_ids`] on every tick. Reading
//! it per tick rather than once at start-up is what makes a folder added in
//! this session start sweeping without a restart, and what makes a folder
//! added in a *previous* session start sweeping without the user opening the
//! sources screen. No command enumerates folders — the published IPC surface
//! is unchanged — because the timer is inside the application, not a caller
//! of it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaudon_domain::error::AppError;
use tokio::runtime::Handle;

use crate::command::{AppCommand, Dispatcher};
use crate::dto::{SourceSyncRequest, SyncTrigger};
use crate::ports::SourcesPort;

/// How often the timer fires, when the caller does not choose.
///
/// Folder changes arrive as filesystem hints the service debounces; a sweep a
/// minute keeps a folder current without turning the disk into a poll loop.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

/// Where the pump sends the sync it wants.
///
/// A trait, so the timer's behaviour is testable without a runtime, a database
/// or a set of ports.
pub trait SyncRequester: Send + Sync + 'static {
    /// Ask for one background sync of `source_id`.
    fn request_background_sync(&self, source_id: &str);
}

impl SyncRequester for Dispatcher {
    fn request_background_sync(&self, source_id: &str) {
        self.send(AppCommand::SourceSync(SourceSyncRequest {
            source_id: source_id.to_owned(),
            trigger: SyncTrigger::Background,
        }));
    }
}

/// Where the pump learns which folders there are to sweep.
///
/// Also a trait, and for the same reason: the timer is then testable against a
/// list a test writes, with no library open.
#[async_trait]
pub trait FolderRoster: Send + Sync + 'static {
    /// The folders this tick should sweep.
    ///
    /// # Errors
    ///
    /// Returns whatever the library said. The pump logs it and tries again on
    /// the next tick rather than ending: a database that is briefly busy must
    /// not stop a folder being watched for the rest of the session.
    async fn folders_to_sweep(&self) -> Result<Vec<String>, AppError>;
}

#[async_trait]
impl FolderRoster for Arc<dyn SourcesPort> {
    async fn folders_to_sweep(&self) -> Result<Vec<String>, AppError> {
        self.watched_folder_ids().await
    }
}

/// A running watched-folder timer.
///
/// Dropping the pump stops it: the task observes the flag on its next tick and
/// returns, so no sweep outlives the application.
#[derive(Clone)]
pub struct WatchedFolderPump {
    stopped: Arc<AtomicBool>,
}

impl std::fmt::Debug for WatchedFolderPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchedFolderPump")
            .field("stopped", &self.stopped.load(Ordering::SeqCst))
            .finish()
    }
}

impl WatchedFolderPump {
    /// Start the timer on `runtime`.
    ///
    /// Returns immediately. The first sweep happens one `interval` later, not
    /// at once: the engine has just reconciled the library at start-up, and a
    /// sweep fired into that has nothing to find.
    #[must_use]
    pub fn start<R: SyncRequester>(
        requester: Arc<R>,
        roster: Arc<dyn FolderRoster>,
        runtime: &Handle,
        interval: Duration,
    ) -> Self {
        let pump = Self {
            stopped: Arc::new(AtomicBool::new(false)),
        };
        let stopped = Arc::clone(&pump.stopped);
        runtime.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first tick of a Tokio interval completes immediately, which
            // is the pass this waits out.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                match roster.folders_to_sweep().await {
                    Ok(folders) => {
                        for source_id in folders {
                            requester.request_background_sync(&source_id);
                        }
                    }
                    Err(error) => tracing::warn!(
                        code = error.code().as_str(),
                        "the watched folders could not be listed; the next tick tries again"
                    ),
                }
            }
        });
        pump
    }

    /// Stop the timer. Idempotent.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

impl Drop for WatchedFolderPump {
    fn drop(&mut self) {
        // The last handle going away stops the task; an earlier clone going
        // away must not.
        if Arc::strong_count(&self.stopped) == 1 {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use parking_lot::Mutex;
    use plaudon_domain::error::AppErrorCode;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        count: AtomicUsize,
    }

    impl SyncRequester for Recorder {
        fn request_background_sync(&self, source_id: &str) {
            self.calls.lock().push(source_id.to_owned());
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A roster a test writes, standing in for the library's folder table.
    #[derive(Default)]
    struct Folders {
        ids: Mutex<Vec<String>>,
        fail: AtomicBool,
    }

    impl Folders {
        fn with(ids: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                ids: Mutex::new(ids.iter().map(|id| (*id).to_owned()).collect()),
                fail: AtomicBool::new(false),
            })
        }

        fn set(&self, ids: &[&str]) {
            *self.ids.lock() = ids.iter().map(|id| (*id).to_owned()).collect();
        }
    }

    #[async_trait]
    impl FolderRoster for Folders {
        async fn folders_to_sweep(&self) -> Result<Vec<String>, AppError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::new(
                    AppErrorCode::InternalError,
                    "the table is busy",
                ));
            }
            Ok(self.ids.lock().clone())
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .expect("a runtime")
    }

    #[test]
    fn the_timer_sweeps_every_folder_the_roster_names_and_nothing_else() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let folders = Folders::with(&["folder-a", "folder-b"]);
        let _pump = WatchedFolderPump::start(
            Arc::clone(&recorder),
            Arc::clone(&folders) as Arc<dyn FolderRoster>,
            runtime.handle(),
            Duration::from_millis(10),
        );

        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(60)).await });
        let calls = recorder.calls.lock().clone();
        assert!(
            calls.iter().any(|id| id == "folder-a") && calls.iter().any(|id| id == "folder-b"),
            "both folders are swept: {calls:?}"
        );

        // The roster is read on every tick, so a folder that stopped being
        // watched stops being swept without the pump being restarted.
        folders.set(&["folder-a"]);
        recorder.calls.lock().clear();
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(40)).await });
        let calls = recorder.calls.lock().clone();
        assert!(
            calls.iter().all(|id| id == "folder-a"),
            "a folder the roster dropped is not swept again: {calls:?}"
        );
    }

    #[test]
    fn an_empty_roster_asks_for_nothing() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let _pump = WatchedFolderPump::start(
            Arc::clone(&recorder),
            Folders::with(&[]) as Arc<dyn FolderRoster>,
            runtime.handle(),
            Duration::from_millis(5),
        );
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(40)).await });
        assert_eq!(
            recorder.count.load(Ordering::SeqCst),
            0,
            "the timer never invents work"
        );
    }

    #[test]
    fn a_roster_that_fails_once_does_not_end_the_timer() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let folders = Folders::with(&["folder-a"]);
        folders.fail.store(true, Ordering::SeqCst);
        let _pump = WatchedFolderPump::start(
            Arc::clone(&recorder),
            Arc::clone(&folders) as Arc<dyn FolderRoster>,
            runtime.handle(),
            Duration::from_millis(5),
        );
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(30)).await });
        assert_eq!(
            recorder.count.load(Ordering::SeqCst),
            0,
            "a failed read sweeps nothing"
        );

        folders.fail.store(false, Ordering::SeqCst);
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(40)).await });
        assert!(
            recorder.count.load(Ordering::SeqCst) > 0,
            "the timer is still running once the library answers again"
        );
    }

    #[test]
    fn stopping_the_pump_ends_the_task() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let pump = WatchedFolderPump::start(
            Arc::clone(&recorder),
            Folders::with(&["folder-a"]) as Arc<dyn FolderRoster>,
            runtime.handle(),
            Duration::from_millis(5),
        );
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(30)).await });
        assert!(recorder.count.load(Ordering::SeqCst) > 0);

        pump.stop();
        let before = recorder.count.load(Ordering::SeqCst);
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(40)).await });
        assert_eq!(
            recorder.count.load(Ordering::SeqCst),
            before,
            "a stopped pump sweeps nothing"
        );
    }
}
