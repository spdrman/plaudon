//! The window's own contract: the shell is hosted, and what the user changed
//! reaches the disk.
//!
//! The shell's behaviour is asserted in `plaudon-ui`'s `shell_kittest`; what
//! is only true here is the glue — the host hooks, the debounce and the
//! round trip through [`plaudon_app::shell_state::ShellStateStore`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app::shell_state::{ShellState, ShellStateStore};
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_ui::shell::{HostHooks, Startup};
use plaudon_ui_testkit::Harness;

const DEADLINE: Duration = Duration::from_secs(60);

fn engine(data_dir: &std::path::Path) -> AppEngine {
    AppEngine::start(
        EngineConfig::new(data_dir)
            .with_secrets(Arc::new(InMemorySecretStore::new()))
            .with_notifier(Arc::new(UnsupportedNotifier))
            .with_worker_threads(2)
            .with_approved_roots(vec![data_dir.to_path_buf()]),
    )
    .expect("the engine starts over a fresh library")
}

/// Hooks that write through a real store, as the binary's do, but
/// synchronously so the test does not have to wait on a runtime.
fn hooks(store: ShellStateStore) -> HostHooks {
    HostHooks {
        save_shell_state: Arc::new(move |state: ShellState| {
            store.save(&state).expect("the state is written");
        }),
        ..HostHooks::none()
    }
}

#[test]
fn what_the_user_changed_reaches_the_disk_and_comes_back() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let app = PlaudonShell::new(
        engine(dir.path()),
        hooks(store.clone()),
        ShellState::default(),
    );

    let mut harness = plaudon_ui_testkit::builder()
        .build_ui_state(|ui, app: &mut PlaudonShell| app.view(ui), app);
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run_steps(1);
        if *harness.state().shell().startup() == Startup::Ready {
            break;
        }
        assert!(Instant::now() < deadline, "the shell never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }

    harness
        .state_mut()
        .shell_mut()
        .set_appearance(plaudon_app::dto::Appearance::Dark);
    harness.run_steps(1);

    // Nothing is written yet: the debounce is there so dragging a window
    // does not write sixty files a second.
    assert_eq!(
        store.load().appearance,
        plaudon_app::dto::Appearance::System
    );

    // Closing the window flushes whatever is pending.
    harness.state_mut().flush_state();
    assert_eq!(store.load().appearance, plaudon_app::dto::Appearance::Dark);
}

#[test]
fn a_shell_restored_from_disk_opens_with_what_was_saved() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let mut saved = ShellState::default();
    saved.window.width = 1400.0;
    saved.window.height = 900.0;
    saved.panels.sidebar_collapsed = true;
    store.save(&saved).expect("the state is written");

    let restored = store.load();
    let viewport = plaudon_ui::shell::window_state::viewport(restored.window);
    assert_eq!(viewport.inner_size, Some(egui::vec2(1400.0, 900.0)));

    let app = PlaudonShell::new(engine(dir.path()), hooks(store), restored);
    let harness: Harness<'_, PlaudonShell> = plaudon_ui_testkit::builder()
        .build_ui_state(|ui, app: &mut PlaudonShell| app.view(ui), app);
    assert!(harness.state().shell().panels().sidebar_collapsed);
}
