//! The hermetic application every integration test in this crate drives.
//!
//! One engine, one set of host hooks, one registration call — the same
//! [`plaudon_egui::screens::register_all`] the binary uses. The only things
//! swapped are the edges the test must own:
//!
//! * the secret store is in memory, so no test touches the OS keychain;
//! * the notifier is the unsupported one, so no test raises a notification;
//! * the radio is whatever the test supplies (the default has none);
//! * the audio device is [`plaudon_audio_playback::FakeAudioOut`], which
//!   renders on demand instead of in real time, so playback assertions are
//!   deterministic;
//! * the file dialogs return what the test told them to, so no test opens a
//!   window.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app::shell_state::{ShellState, ShellStateStore};
use plaudon_audio_playback::engine::AudioOut;
use plaudon_audio_playback::{FakeAudioOut, FakeDevice};
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_ui::screens::recording::AudioOutFactory;
use plaudon_ui::shell::{HostHooks, Startup};
use plaudon_ui_testkit::Harness;

/// How long a test waits for the engine to answer before giving up.
pub const DEADLINE: Duration = Duration::from_secs(60);

/// Sample rate of the fake device. 16 kHz mono is what the decoding path
/// uses, so a fixture written for one works for the other.
pub const FAKE_RATE: u32 = 16_000;

/// An engine over a fresh library in `data_dir`.
pub fn engine(data_dir: &Path) -> AppEngine {
    AppEngine::start(config(data_dir)).expect("the engine starts over the library")
}

/// The hermetic engine configuration, for a test that needs to add to it.
pub fn config(data_dir: &Path) -> EngineConfig {
    EngineConfig::new(data_dir)
        .with_secrets(Arc::new(InMemorySecretStore::new()))
        .with_notifier(Arc::new(UnsupportedNotifier))
        .with_worker_threads(2)
        .with_approved_roots(vec![data_dir.to_path_buf()])
}

/// Host hooks that write shell state synchronously and pick nothing.
pub fn hooks(store: ShellStateStore) -> HostHooks {
    HostHooks {
        save_shell_state: Arc::new(move |state: ShellState| {
            store.save(&state).expect("the shell state is written");
        }),
        ..HostHooks::default()
    }
}

/// Host hooks whose audio-file picker returns `paths`.
pub fn hooks_picking(store: ShellStateStore, paths: Vec<PathBuf>) -> HostHooks {
    let chosen = Arc::new(Mutex::new(paths));
    HostHooks {
        pick_audio_files: Arc::new(move || chosen.lock().expect("the picker list").clone()),
        ..hooks(store)
    }
}

/// A fake output device, and the handle a test renders it through.
///
/// The device is taken **before** the factory boxes it, because that is the
/// only way to hold a handle on a device the player owns.
pub fn fake_audio_out() -> (AudioOutFactory, FakeDevice) {
    let out = FakeAudioOut::new(FAKE_RATE, 1);
    let device = out.device();
    let slot = Mutex::new(Some(out));
    let factory: AudioOutFactory = Arc::new(move || {
        slot.lock().expect("the device slot").take().map_or_else(
            || {
                Err(plaudon_audio_playback::engine::PlaybackError::Device(
                    "the fake device has already been opened once".to_owned(),
                ))
            },
            |out| Ok(Box::new(out) as Box<dyn AudioOut>),
        )
    });
    (factory, device)
}

/// The whole application over `data_dir`, with every screen registered.
pub fn app(data_dir: &Path, hooks: HostHooks, audio: AudioOutFactory) -> PlaudonShell {
    let mut app = PlaudonShell::new(engine(data_dir), hooks, ShellState::default());
    plaudon_egui::screens::register_all(&mut app, audio, None);
    app
}

/// A harness over the whole application, run until the shell is ready.
pub fn harness(app: PlaudonShell) -> Harness<'static, PlaudonShell> {
    let mut harness = plaudon_ui_testkit::builder()
        .build_ui_state(|ui, app: &mut PlaudonShell| app.view(ui), app);
    settle(&mut harness);
    harness
}

/// Run frames until the shell reports it is ready.
pub fn settle(harness: &mut Harness<'_, PlaudonShell>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run_steps(1);
        if matches!(harness.state().shell().startup(), Startup::Ready) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never became ready: {:?}",
            harness.state().shell().startup()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Run frames until `ready` answers true, or fail with `what`.
pub fn run_until(
    harness: &mut Harness<'_, PlaudonShell>,
    what: &str,
    mut ready: impl FnMut(&PlaudonShell) -> bool,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run_steps(1);
        if ready(harness.state()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Everything the current frame announces, in tree order.
pub fn announced(harness: &Harness<'_, PlaudonShell>) -> Vec<String> {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect()
}

/// Whether the current frame announces `label` anywhere.
pub fn announces(harness: &Harness<'_, PlaudonShell>, label: &str) -> bool {
    announced(harness).iter().any(|text| text.contains(label))
}

/// The whole application over an engine the test configured itself.
///
/// [`app`] builds the engine from [`config`]; a test that has to add a radio,
/// a transport or a preference to the engine builds its own and hands it
/// here, so the registration is still the binary's.
pub fn app_over(engine: AppEngine, hooks: HostHooks, audio: AudioOutFactory) -> PlaudonShell {
    let mut app = PlaudonShell::new(engine, hooks, ShellState::default());
    plaudon_egui::screens::register_all(&mut app, audio, None);
    app
}

/// Record the first-run choices, so the shell is not gated by onboarding.
///
/// A fresh library has `onboarding_completed: false`, which is the honest
/// default and which the shell honours by showing the first-run flow instead
/// of the destinations. A test that drives a destination has to get past it,
/// and the only way that is not a lie is the command the flow itself sends.
///
/// Every notification category is left off, so nothing can raise one.
pub fn complete_onboarding(engine: &AppEngine, local_only_mode: bool) {
    let mut categories = std::collections::BTreeMap::new();
    for category in plaudon_app::dto::NotificationCategory::ALL {
        categories.insert(*category, false);
    }
    let answer =
        engine
            .dispatcher()
            .call_blocking(plaudon_app::command::AppCommand::CompleteOnboarding(
                plaudon_app::dto::CompleteOnboardingRequest {
                    default_stt_engine_id: None,
                    default_text_engine_id: None,
                    local_only_mode,
                    notifications: plaudon_app::dto::NotificationSettings {
                        enabled: false,
                        categories,
                    },
                },
            ));
    assert!(answer.is_ok(), "onboarding was not recorded: {answer:?}");
}

/// Run frames until `ready` answers true of the frame, or fail with `what`.
///
/// [`run_until`] asks about the application; this asks about what is on
/// screen, which is what an assertion over the accessibility tree needs.
///
/// One frame at a time, like [`run_until`] and unlike `Harness::run`: a
/// screen that is legitimately animating — the player while sound is coming
/// out — asks for the next frame immediately, and `run` treats that as a
/// runaway loop after four steps.
pub fn run_until_frame(
    harness: &mut Harness<'_, PlaudonShell>,
    what: &str,
    mut ready: impl FnMut(&Harness<'_, PlaudonShell>) -> bool,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run_steps(1);
        if ready(harness) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Run frames until the frame announces `label` anywhere.
pub fn run_until_announced(harness: &mut Harness<'_, PlaudonShell>, label: &str) {
    run_until_frame(harness, &format!("{label:?} to be announced"), |harness| {
        announces(harness, label)
    });
}
