//! The engine: one owned Tokio runtime, one set of ports, one event channel.
//!
//! A user interface constructs an [`AppEngine`] once, keeps it for the life of
//! the process, and then only ever
//!
//! * sends [`AppCommand`](crate::command::AppCommand)s through
//!   [`AppEngine::dispatcher`], and
//! * drains [`AppEvent`](crate::event::AppEvent)s from [`AppEngine::events`].
//!
//! The engine owns the runtime so that nothing else has to: there is exactly
//! one multi-threaded Tokio runtime in the process, built here, shut down
//! here. The render thread never enters it.
//!
//! **Data directory (migration risk R7).** The Tauri shell used
//! `app_data_dir()`, which is the platform data directory joined with the
//! bundle identifier `app.plaudon.desktop`. [`default_data_dir`] resolves the
//! same path through `dirs` so an existing library is still found after the
//! shell is replaced:
//!
//! | Platform | Path |
//! | --- | --- |
//! | macOS | `~/Library/Application Support/app.plaudon.desktop` |
//! | Windows | `%APPDATA%\app.plaudon.desktop` (Roaming) |
//! | Linux | `$XDG_DATA_HOME/app.plaudon.desktop`, default `~/.local/share/...` |

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_notifications::{Dispatcher as NotificationDispatcher, NotificationSettings};

use crate::backend::devices::{DeviceRadio, NoRadio};
use crate::command::Dispatcher;
use crate::event::{self, AppEvent, ChannelEventSink, EventReceiver, NoopWaker, Waker};
use crate::events::{EventPublisher, EventSink};
use crate::notify;
use crate::secrets::{KeyringSecretStore, SecretStore};
use crate::updater::UpdateService;
use crate::{default_approved_roots, AppPorts};

/// The bundle identifier the Tauri shell shipped with, and therefore the
/// directory name an existing installation already uses.
pub const APPLICATION_IDENTIFIER: &str = "app.plaudon.desktop";

/// Where the library lives, matching Tauri's `app_data_dir()`.
///
/// `None` when the platform has no data directory, which is a fatal
/// configuration problem rather than something to paper over with a guess.
#[must_use]
pub fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|base| base.join(APPLICATION_IDENTIFIER))
}

/// How the engine should be built.
pub struct EngineConfig {
    data_dir: PathBuf,
    approved_roots: Vec<PathBuf>,
    event_capacity: usize,
    waker: Arc<dyn Waker>,
    secrets: Arc<dyn SecretStore>,
    radio: Arc<dyn DeviceRadio>,
    cloud_transport: Option<Arc<dyn plaudon_ai_openrouter::transport::AsyncHttpTransport>>,
    notifier: Option<Arc<dyn plaudon_notifications::Notifier>>,
    engines: crate::EngineOverrides,
    worker_threads: Option<usize>,
}

impl std::fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("data_dir", &self.data_dir)
            .field("approved_roots", &self.approved_roots.len())
            .field("event_capacity", &self.event_capacity)
            .field("worker_threads", &self.worker_threads)
            .finish_non_exhaustive()
    }
}

impl EngineConfig {
    /// Configuration for a library in `data_dir`, with production defaults:
    /// the OS keychain, no Bluetooth radio and the platform notifier.
    #[must_use]
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            approved_roots: default_approved_roots(),
            event_capacity: event::DEFAULT_CAPACITY,
            waker: Arc::new(NoopWaker),
            secrets: Arc::new(KeyringSecretStore::new()),
            radio: Arc::new(NoRadio),
            cloud_transport: None,
            notifier: None,
            engines: crate::EngineOverrides::default(),
            worker_threads: None,
        }
    }

    /// Configuration for the platform's standard library location.
    ///
    /// # Errors
    ///
    /// Fails when the platform exposes no data directory.
    pub fn for_this_platform() -> Result<Self, AppError> {
        let data_dir = default_data_dir().ok_or_else(|| {
            AppError::new(
                AppErrorCode::InternalError,
                "this platform has no application data directory",
            )
        })?;
        Ok(Self::new(data_dir))
    }

    /// Roots a user-selected file may resolve into (spec §7.4).
    #[must_use]
    pub fn with_approved_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.approved_roots = roots;
        self
    }

    /// Backlog of distinct pending invalidations before the queue coalesces
    /// into a resynchronisation.
    #[must_use]
    pub fn with_event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity;
        self
    }

    /// The waker the engine calls when an event arrives.
    #[must_use]
    pub fn with_waker(mut self, waker: Arc<dyn Waker>) -> Self {
        self.waker = waker;
        self
    }

    /// Replace the keychain store (tests use an in-memory one).
    #[must_use]
    pub fn with_secrets(mut self, secrets: Arc<dyn SecretStore>) -> Self {
        self.secrets = secrets;
        self
    }

    /// Supply a Bluetooth radio actor; the default reports
    /// `RadioState::UnavailableInBuild`.
    #[must_use]
    pub fn with_radio(mut self, radio: Arc<dyn DeviceRadio>) -> Self {
        self.radio = radio;
        self
    }

    /// Replace the speech engines the transcribe executor probes for.
    ///
    /// Tests use this to run the whole import → transcribe → summarize path
    /// against scripted engines, with no model file and no network.
    #[must_use]
    pub fn with_stt_engines(
        mut self,
        engines: Vec<Arc<dyn plaudon_ai_contracts::stt::SpeechToText>>,
    ) -> Self {
        self.engines.stt = engines;
        self
    }

    /// Replace the text model every summarization and tool run uses.
    #[must_use]
    pub fn with_text_model(
        mut self,
        model: Arc<dyn plaudon_ai_contracts::text::TextModel>,
    ) -> Self {
        self.engines.text = Some(model);
        self
    }

    /// Replace the cloud HTTPS transport.
    ///
    /// The one reason to do this is a test that must prove nothing left the
    /// process: a transport that records and refuses is the only way to tell
    /// "no request was made" apart from "a request was made and failed".
    #[must_use]
    pub fn with_cloud_transport(
        mut self,
        transport: Arc<dyn plaudon_ai_openrouter::transport::AsyncHttpTransport>,
    ) -> Self {
        self.cloud_transport = Some(transport);
        self
    }

    /// Replace the OS notifier; tests use
    /// [`crate::notify::UnsupportedNotifier`].
    #[must_use]
    pub fn with_notifier(mut self, notifier: Arc<dyn plaudon_notifications::Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Where this configuration's library lives.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Size the runtime's worker pool. Default is Tokio's (one per core).
    #[must_use]
    pub fn with_worker_threads(mut self, threads: usize) -> Self {
        self.worker_threads = Some(threads);
        self
    }
}

/// The running application: runtime, ports, dispatcher and event channel.
#[derive(Debug)]
pub struct AppEngine {
    runtime: Option<tokio::runtime::Runtime>,
    dispatcher: Dispatcher,
    events: EventReceiver,
    ports: AppPorts,
    data_dir: PathBuf,
}

impl AppEngine {
    /// Build the runtime, open the library and recover interrupted work.
    ///
    /// Blocks until the library is open: there is nothing a user interface can
    /// usefully paint before that.
    ///
    /// # Errors
    ///
    /// Fails when the runtime cannot be built, when the library cannot be
    /// opened or migrated, or when the device backend cannot be constructed.
    pub fn start(config: EngineConfig) -> Result<Self, AppError> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all().thread_name("plaudon-app");
        if let Some(threads) = config.worker_threads {
            builder.worker_threads(threads);
        }
        let runtime = builder.build().map_err(|err| {
            AppError::new(
                AppErrorCode::InternalError,
                format!("the async runtime could not be built: {err}"),
            )
        })?;

        let (sender, receiver) = event::channel(config.event_capacity, config.waker);
        let sink: Arc<dyn EventSink> = Arc::new(ChannelEventSink::new(sender.clone()));

        // A notification click has nowhere to route until the ports exist, so
        // the publisher is shared by handle and filled in below. Activation
        // then travels the ordinary event stream instead of focusing a window
        // the application layer knows nothing about.
        let publisher: Arc<Mutex<Option<EventPublisher>>> = Arc::new(Mutex::new(None));
        let notifier = match config.notifier {
            Some(notifier) => notifier,
            None => {
                let activation_publisher = Arc::clone(&publisher);
                let handle = runtime.handle().clone();
                let on_activated: notify::ActivationHandler = Arc::new(move |target| {
                    let Some(target) = target else {
                        return;
                    };
                    let events = activation_publisher.lock().clone();
                    let Some(events) = events else {
                        tracing::debug!("notification activated before the library was open");
                        return;
                    };
                    handle.spawn(async move {
                        notify::publish_activation(&events, target).await;
                    });
                });
                notify::platform_notifier(on_activated)
            }
        };
        // Preferences are loaded by the backend; the dispatcher starts
        // disabled so nothing can be delivered before they are applied.
        let notifications = Arc::new(NotificationDispatcher::new(
            notifier,
            NotificationSettings::uniform(false),
        ));

        let ports = runtime.block_on(crate::build_ports_with(
            &config.data_dir,
            sink,
            notifications,
            config.secrets,
            config.approved_roots,
            config.radio,
            config.cloud_transport,
            config.engines,
        ))?;
        *publisher.lock() = Some(ports.events.clone());

        // Spec 16.5 and 22(8): opening the update root runs crash recovery,
        // discarding any uncommitted attempt so the launched version is always
        // a complete, previously verified one.
        recover_update_state(&config.data_dir);

        let dispatcher = Dispatcher::new(ports.clone(), runtime.handle().clone(), sender);
        Ok(Self {
            runtime: Some(runtime),
            dispatcher,
            events: receiver,
            ports,
            data_dir: config.data_dir,
        })
    }

    /// The one path from the interface into the application.
    #[must_use]
    pub fn dispatcher(&self) -> &Dispatcher {
        &self.dispatcher
    }

    /// The event channel the interface drains each frame.
    #[must_use]
    pub fn events(&self) -> &EventReceiver {
        &self.events
    }

    /// Install the interface's waker once its context exists.
    pub fn set_waker(&self, waker: Arc<dyn Waker>) {
        self.events.set_waker(waker);
    }

    /// The ports, for media resolution and for tests.
    #[must_use]
    pub fn ports(&self) -> &AppPorts {
        &self.ports
    }

    /// Where this engine's library lives.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// A handle to the owned runtime, for background work that is not a
    /// command (peak generation, for instance).
    ///
    /// # Panics
    ///
    /// Panics after [`AppEngine::shutdown`] has taken the runtime.
    #[must_use]
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime
            .as_ref()
            .expect("the runtime is owned for the engine's lifetime")
            .handle()
            .clone()
    }

    /// Queue an event as if the application had produced it.
    ///
    /// The one legitimate use is a host hook — a file dialog or a window
    /// callback — that needs to reach the same drain loop as everything else.
    pub fn post(&self, event: AppEvent) {
        self.events.sender().send(event);
    }

    /// Stop the job workers, stop accepting events and wind the runtime down.
    ///
    /// Workers are given `grace` to finish the job they hold; anything still
    /// running is abandoned, which is safe because every durable step commits
    /// before it reports and the next start reconciles the row (§14.7).
    pub fn shutdown(mut self, grace: std::time::Duration) {
        self.events.close();
        if let Some(runtime) = self.runtime.take() {
            let processing = self.ports.processing.clone();
            runtime.block_on(async move { processing.stop(grace).await });
            runtime.shutdown_timeout(grace);
        }
    }
}

/// Open the update root so a half-applied update is rolled back at startup.
///
/// A root that does not exist yet is not an error: this build was installed by
/// the packaged installer, not by the updater.
pub fn recover_update_state(data_dir: &Path) {
    match UpdateService::open(data_dir, true) {
        Ok(service) => match service.installed_version() {
            Ok(version) => tracing::info!(%version, "update state recovered"),
            Err(error) => tracing::warn!(%error, "update state is unreadable"),
        },
        Err(error) => {
            tracing::debug!(%error, "no update root yet; nothing to recover");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{AppCommand, AppResponse};
    use crate::dto::{
        ListRecordingsRequest, RecordingFilter, RecordingSort, RecordingSortField, SortDirection,
    };
    use crate::event::CountingWaker;
    use crate::notify::UnsupportedNotifier;
    use crate::secrets::InMemorySecretStore;

    fn engine(dir: &Path, waker: Arc<dyn Waker>) -> AppEngine {
        AppEngine::start(
            EngineConfig::new(dir)
                .with_secrets(Arc::new(InMemorySecretStore::new()))
                .with_notifier(Arc::new(UnsupportedNotifier))
                .with_waker(waker)
                .with_worker_threads(2)
                .with_approved_roots(vec![dir.to_path_buf()]),
        )
        .expect("the engine starts over a fresh library")
    }

    fn list_recordings() -> AppCommand {
        AppCommand::ListRecordings(ListRecordingsRequest {
            cursor: None,
            limit: 25,
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
                field: RecordingSortField::RecordedAt,
                direction: SortDirection::Desc,
            },
        })
    }

    #[test]
    fn the_data_directory_matches_the_tauri_bundle_identifier() {
        let resolved = default_data_dir().expect("a data directory on a supported platform");
        assert!(
            resolved.ends_with(APPLICATION_IDENTIFIER),
            "an existing library must still be found: {}",
            resolved.display()
        );
        assert_eq!(APPLICATION_IDENTIFIER, "app.plaudon.desktop");
    }

    #[test]
    fn a_command_sent_from_a_non_async_caller_answers_on_the_event_channel() {
        let dir = tempfile::tempdir().unwrap();
        let waker = Arc::new(CountingWaker::new());
        let engine = engine(dir.path(), waker.clone());
        let _ = engine.events().drain();

        let id = engine.dispatcher().send(list_recordings());
        let event = engine
            .events()
            .next_timeout(std::time::Duration::from_secs(10))
            .expect("the outcome arrives");
        match event {
            AppEvent::Response {
                id: answered,
                command,
                result,
            } => {
                assert_eq!(answered, id);
                assert_eq!(command, "list_recordings");
                match (*result).expect("an empty library lists successfully") {
                    AppResponse::ListRecordings(page) => assert!(page.items.is_empty()),
                    other => panic!("unexpected {other:?}"),
                }
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(waker.count() >= 1, "the interface was woken");
        engine.shutdown(std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_blocking_call_returns_the_same_typed_response() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path(), Arc::new(NoopWaker));
        let response = engine
            .dispatcher()
            .call_blocking(AppCommand::GetCapabilities)
            .expect("capabilities are readable");
        assert_eq!(response.command(), "get_capabilities");
        engine.shutdown(std::time::Duration::from_secs(5));
    }

    #[test]
    fn an_idle_engine_produces_no_events() {
        let dir = tempfile::tempdir().unwrap();
        let waker = Arc::new(CountingWaker::new());
        let engine = engine(dir.path(), waker.clone());
        let _ = engine.events().drain();
        let before = waker.count();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(engine.events().drain().is_empty(), "idle means silent");
        assert_eq!(waker.count(), before, "nothing asked for a repaint");
        engine.shutdown(std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_rejected_command_answers_with_an_envelope_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path(), Arc::new(NoopWaker));
        let envelope = engine
            .dispatcher()
            .call_blocking(AppCommand::GetRecording(crate::dto::GetRecordingRequest {
                recording_id: "not-a-uuid".to_owned(),
            }))
            .expect_err("an invalid id is refused");
        assert_eq!(envelope.code, AppErrorCode::InvalidArgument);
        engine.shutdown(std::time::Duration::from_secs(5));
    }
}
