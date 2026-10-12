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
//! Scope, stated honestly: the pump syncs the folders it is given. This build
//! publishes no command that enumerates the watched folders stored on disk, so
//! a host registers the folders it knows — the ones `source_add_folder` and
//! `source_sync` answered for in this session. Adding the enumeration would
//! widen the published command surface, which this task does not do.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::runtime::Handle;

use crate::command::{AppCommand, Dispatcher};
use crate::dto::{SourceSyncRequest, SyncTrigger};

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

#[derive(Debug, Default)]
struct Registry {
    folders: BTreeSet<String>,
    stopped: bool,
}

/// A running watched-folder timer.
///
/// Dropping the pump stops it: the task observes the flag on its next tick and
/// returns, so no sweep outlives the application.
#[derive(Clone)]
pub struct WatchedFolderPump {
    registry: Arc<Mutex<Registry>>,
}

impl std::fmt::Debug for WatchedFolderPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let registry = self.registry.lock();
        f.debug_struct("WatchedFolderPump")
            .field("folders", &registry.folders.len())
            .field("stopped", &registry.stopped)
            .finish()
    }
}

impl WatchedFolderPump {
    /// Start the timer on `runtime`.
    ///
    /// Returns immediately; nothing is swept until a folder is registered with
    /// [`WatchedFolderPump::watch`].
    #[must_use]
    pub fn start<R: SyncRequester>(
        requester: Arc<R>,
        runtime: &Handle,
        interval: Duration,
    ) -> Self {
        let pump = Self {
            registry: Arc::new(Mutex::new(Registry::default())),
        };
        let registry = Arc::clone(&pump.registry);
        runtime.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first tick of a Tokio interval completes immediately; a
            // folder that was just registered has nothing new to sweep, so the
            // pump waits one period before its first pass.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let folders = {
                    let registry = registry.lock();
                    if registry.stopped {
                        return;
                    }
                    registry.folders.iter().cloned().collect::<Vec<_>>()
                };
                for source_id in folders {
                    requester.request_background_sync(&source_id);
                }
            }
        });
        pump
    }

    /// Sweep `source_id` from now on. Registering it twice changes nothing.
    pub fn watch(&self, source_id: impl Into<String>) {
        self.registry.lock().folders.insert(source_id.into());
    }

    /// Stop sweeping `source_id`.
    pub fn forget(&self, source_id: &str) {
        self.registry.lock().folders.remove(source_id);
    }

    /// The folders the timer sweeps.
    #[must_use]
    pub fn watched(&self) -> Vec<String> {
        self.registry.lock().folders.iter().cloned().collect()
    }

    /// Stop the timer. Idempotent.
    pub fn stop(&self) {
        self.registry.lock().stopped = true;
    }
}

impl Drop for WatchedFolderPump {
    fn drop(&mut self) {
        // The last handle going away stops the task; an earlier clone going
        // away must not.
        if Arc::strong_count(&self.registry) == 1 {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .expect("a runtime")
    }

    #[test]
    fn the_timer_sweeps_every_registered_folder_and_nothing_else() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let pump = WatchedFolderPump::start(
            Arc::clone(&recorder),
            runtime.handle(),
            Duration::from_millis(10),
        );
        pump.watch("folder-a");
        pump.watch("folder-a");
        pump.watch("folder-b");
        assert_eq!(pump.watched(), vec!["folder-a", "folder-b"]);

        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(60)).await });
        let calls = recorder.calls.lock().clone();
        assert!(
            calls.iter().any(|id| id == "folder-a") && calls.iter().any(|id| id == "folder-b"),
            "both folders are swept: {calls:?}"
        );

        pump.forget("folder-b");
        recorder.calls.lock().clear();
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(40)).await });
        let calls = recorder.calls.lock().clone();
        assert!(
            calls.iter().all(|id| id == "folder-a"),
            "a forgotten folder is not swept again: {calls:?}"
        );
    }

    #[test]
    fn an_empty_registry_asks_for_nothing() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let _pump = WatchedFolderPump::start(
            Arc::clone(&recorder),
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
    fn stopping_the_pump_ends_the_task() {
        let runtime = runtime();
        let recorder = Arc::new(Recorder::default());
        let pump = WatchedFolderPump::start(
            Arc::clone(&recorder),
            runtime.handle(),
            Duration::from_millis(5),
        );
        pump.watch("folder-a");
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
