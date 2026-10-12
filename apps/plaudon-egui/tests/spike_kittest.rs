//! The O1.2 interaction test: one real library, one real import, one window.
//!
//! Everything the spike claims is asserted here through `egui_kittest`, over
//! the real [`plaudon_app`] engine on a temporary library — no mock backend, no
//! network, no window, and no `rfd` dialog (the file picker is a host hook, and
//! the test supplies one that returns a fixture path).
//!
//! `scripts/e2e-remote.sh cargo test -p plaudon-egui --test spike_kittest` runs
//! the same file on MARS, where `PLAUDON_VISUAL=1` additionally compares the
//! committed baseline under `tests/visual/`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_egui::{HostHooks, PlaudonEgui};
use plaudon_ui_testkit::kittest::Queryable as _;
use plaudon_ui_testkit::{a11y, visual, Harness};

/// Longest a command may take before the test calls it a failure.
const DEADLINE: Duration = Duration::from_secs(60);

fn repository_root() -> PathBuf {
    // apps/plaudon-egui -> apps -> repository root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn fixture() -> PathBuf {
    let path = repository_root().join("tests/fixtures/audio/valid/tone-mono-16k.wav");
    assert!(path.is_file(), "missing fixture {}", path.display());
    path
}

fn engine(data_dir: &Path) -> AppEngine {
    AppEngine::start(
        EngineConfig::new(data_dir)
            .with_secrets(Arc::new(InMemorySecretStore::new()))
            .with_notifier(Arc::new(UnsupportedNotifier))
            .with_worker_threads(2)
            .with_approved_roots(vec![
                data_dir.to_path_buf(),
                repository_root().join("tests/fixtures"),
            ]),
    )
    .expect("the engine starts over a fresh library")
}

/// A file picker that answers with a fixture and records that it was asked.
fn picker(paths: Vec<PathBuf>) -> (HostHooks, Arc<Mutex<u32>>) {
    let calls = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&calls);
    (
        HostHooks {
            pick_audio_files: Arc::new(move || {
                *counter.lock().expect("picker counter") += 1;
                paths.clone()
            }),
        },
        calls,
    )
}

/// Run frames until `done`, or fail with what the interface was showing.
fn pump_until(
    harness: &mut Harness<'_, PlaudonEgui>,
    what: &str,
    done: impl Fn(&PlaudonEgui) -> bool,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run();
        if done(harness.state()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; status={:?} activity={:?}",
            harness.state().state().status,
            harness.state().state().activity
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn the_spike_lists_an_empty_library_imports_a_file_and_shows_the_result() {
    let data_dir = tempfile::tempdir().expect("temporary library");
    let (hooks, picker_calls) = picker(vec![fixture()]);
    let app = PlaudonEgui::new(engine(data_dir.path()), hooks);
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut PlaudonEgui| app.view(ui), app);

    // 1. A fresh subscriber is told to fetch everything, and the first answer
    //    is an empty library — stated as an empty state, not as a zero row.
    pump_until(&mut harness, "the first library answer", |app| {
        app.state().library_fetched
    });
    assert!(harness.state().state().recordings.is_empty());
    assert_eq!(harness.state().state().total, 0);
    assert!(a11y::has_label(
        &harness,
        "No recordings yet. Import audio to get started."
    ));
    assert_eq!(harness.state().state().status, "0 recording(s)");

    // 2. The vendored divider is a real splitter to a screen reader.
    let splitter = a11y::nodes(&harness)
        .into_iter()
        .find(|node| node.label.as_deref() == Some("Resize the library pane"))
        .expect("the divider is in the accessibility tree");
    assert_eq!(splitter.role, "Splitter");

    // 3. Both toolbar actions are reachable with the keyboard alone.
    let order = a11y::focus_order(&mut harness, 8);
    for label in [
        "Import audio…",
        "Refresh library",
        "Resize the library pane",
    ] {
        assert!(
            order.iter().any(|stop| stop == label),
            "{label} is unreachable by keyboard; order was {order:?}"
        );
    }

    // 4. Importing runs on the Tokio runtime through the dispatcher: the click
    //    returns immediately, the typed outcome and the §15.3 events arrive
    //    later and the list refetches itself.
    harness.get_by_label("Import audio…").click();
    pump_until(&mut harness, "the imported recording", |app| {
        !app.state().recordings.is_empty()
    });
    assert_eq!(*picker_calls.lock().expect("picker counter"), 1);
    assert_eq!(harness.state().state().recordings.len(), 1);
    assert_eq!(harness.state().state().total, 1);
    let activity = harness.state().state().activity.clone();
    assert!(
        activity.iter().any(|line| line.contains("import accepted")),
        "the import outcome was not reported: {activity:?}"
    );
    assert!(
        activity
            .iter()
            .any(|line| line.contains("recording.imported")),
        "no §15.3 event reached the interface: {activity:?}"
    );
    assert!(
        !a11y::has_label(&harness, "No recordings yet. Import audio to get started."),
        "the empty state is still showing after an import"
    );

    // 5. The committed baseline, compared only where the renderer is fixed.
    //    See docs/EGUI_TESTING.md §4.
    visual::assert_snapshot(&mut harness, "spike-library-after-import");
}

#[test]
fn a_cancelled_dialog_imports_nothing_and_says_so() {
    let data_dir = tempfile::tempdir().expect("temporary library");
    let (hooks, picker_calls) = picker(Vec::new());
    let app = PlaudonEgui::new(engine(data_dir.path()), hooks);
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut PlaudonEgui| app.view(ui), app);
    pump_until(&mut harness, "the first library answer", |app| {
        app.state().library_fetched
    });

    harness.get_by_label("Import audio…").click();
    harness.run();

    assert_eq!(*picker_calls.lock().expect("picker counter"), 1);
    assert_eq!(harness.state().state().status, "Import cancelled");
    assert!(harness.state().state().recordings.is_empty());
}

#[test]
fn an_idle_window_asks_for_no_further_repaint() {
    let data_dir = tempfile::tempdir().expect("temporary library");
    let (hooks, _) = picker(Vec::new());
    let app = PlaudonEgui::new(engine(data_dir.path()), hooks);
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut PlaudonEgui| app.view(ui), app);
    pump_until(&mut harness, "the first library answer", |app| {
        app.state().library_fetched
    });

    // Let anything in flight finish, then settle the interface.
    std::thread::sleep(Duration::from_millis(250));
    harness.run();
    harness.run();

    assert!(
        !harness.ctx.has_requested_repaint(),
        "an idle Plaudon window must not repaint continuously"
    );
}
