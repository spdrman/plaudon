//! Activation and document opening, at the shell's own entry points.
//!
//! Parity rows C10 (file association and protocol activation) and J15
//! (notification activation) of `docs/EGUI_MIGRATION_PLAN.md`. The host — a
//! real window, a tray, an Apple event — is not here: everything the desktop
//! can ask for is a value, and this file hands those values to the shell and
//! asserts where the user ended up.

use std::sync::Arc;

use plaudon_app::dto::{EventKind, UiEvent};
use plaudon_app::event::AppEvent;
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app::shell_state::ShellState;
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_ui::settings::shell_adapter::SettingsShellScreen;
use plaudon_ui::shell::{ActivationTarget, HostHooks, OpenRequest, Route, Shell, ShellHost};

const ARCHIVE: &str = "/Users/someone/Backups/plaudon-2026-10-12.plaudon";

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

/// A shell with the Settings destination registered, over a real engine.
///
/// The engine is only here because a `ShellHost` needs a dispatcher; nothing
/// in this file waits for a command to come back.
struct Fixture {
    _dir: tempfile::TempDir,
    engine: AppEngine,
    hooks: HostHooks,
    shell: Shell,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temporary library");
        let mut shell = Shell::new(ShellState::default());
        shell.register(Box::new(SettingsShellScreen::new()));
        Self {
            engine: engine(dir.path()),
            _dir: dir,
            hooks: HostHooks::none(),
            shell,
        }
    }

    fn with<R>(&mut self, visit: impl FnOnce(&mut Shell, &ShellHost<'_>) -> R) -> R {
        let host = ShellHost {
            dispatcher: self.engine.dispatcher(),
            hooks: &self.hooks,
        };
        visit(&mut self.shell, &host)
    }
}

fn activation_event(target: &ActivationTarget) -> UiEvent {
    let (recording_id, job_id, kind) = match target {
        ActivationTarget::Recording(id) => (Some(*id), None, "recording"),
        ActivationTarget::Job(id) => (None, Some(*id), "job"),
        ActivationTarget::Settings => (None, None, "settings"),
    };
    UiEvent {
        seq: 1,
        kind: EventKind::ActivationRequested,
        occurred_at: "2026-10-12T00:00:00Z".to_owned(),
        job_id,
        recording_id,
        // The hint `plaudon_app::notify::publish_activation` writes.
        payload: [(
            "target".to_owned(),
            serde_json::json!({ "kind": kind, "id": null }),
        )]
        .into_iter()
        .collect(),
    }
}

fn targets() -> Vec<(ActivationTarget, Route)> {
    let recording_id = RecordingId::new();
    let job_id = JobId::new();
    vec![
        (
            ActivationTarget::Recording(recording_id),
            Route::LibraryRecording {
                recording_id: recording_id.to_string(),
            },
        ),
        (
            ActivationTarget::Job(job_id),
            Route::JobsJob {
                job_id: job_id.to_string(),
            },
        ),
        (ActivationTarget::Settings, Route::Settings),
    ]
}

#[test]
fn every_activation_navigates_and_asks_for_the_window() {
    for (target, expected) in targets() {
        let mut shell = Shell::new(ShellState::default());
        shell.activate_target(target);
        assert_eq!(shell.route(), &expected, "{target:?} went somewhere else");
        assert!(
            shell.take_activation(),
            "{target:?} did not raise the window"
        );
        assert!(
            !shell.take_activation(),
            "the raise is consumed once, or the window would fight the user"
        );
    }
}

#[test]
fn a_tray_click_raises_the_window_without_moving_the_user() {
    let mut shell = Shell::new(ShellState::default());
    shell.activate_target(ActivationTarget::Settings);
    assert!(shell.take_activation());

    shell.activate_window();
    assert!(shell.take_activation());
    assert_eq!(
        shell.route(),
        &Route::Settings,
        "raising the window is not a navigation"
    );
}

#[test]
fn the_event_path_reaches_the_same_place_as_the_direct_one() {
    let mut fixture = Fixture::new();
    for (target, expected) in targets() {
        let event = AppEvent::Ui(activation_event(&target));
        fixture.with(|shell, host| shell.handle_event(event, host));
        assert_eq!(
            fixture.shell.route(),
            &expected,
            "a notification activation for {target:?} went somewhere else"
        );
        assert!(fixture.shell.take_activation());
    }
}

#[test]
fn an_activation_that_names_nothing_leaves_the_user_where_they_were() {
    let mut fixture = Fixture::new();
    fixture.with(|shell, _host| shell.navigate(Route::Jobs));
    let mut event = activation_event(&ActivationTarget::Settings);
    event.payload.clear();
    fixture.with(|shell, host| shell.handle_event(AppEvent::Ui(event), host));
    assert_eq!(fixture.shell.route(), &Route::Jobs);
    assert!(
        fixture.shell.take_activation(),
        "the window still comes forward"
    );
}

#[test]
fn an_opened_archive_lands_in_the_restore_flow() {
    let mut fixture = Fixture::new();
    fixture.with(|shell, host| {
        shell.open_files(
            OpenRequest {
                archives: vec![ARCHIVE.to_owned()],
                ..OpenRequest::default()
            },
            host,
        );
    });
    assert_eq!(fixture.shell.route(), &Route::Settings);
    assert_eq!(fixture.shell.status(), "Opened an archive to restore from");
    assert!(
        fixture.shell.toasts().is_empty(),
        "an archive that was taken raises nothing"
    );
}

#[test]
fn an_archive_with_no_screen_to_take_it_says_so() {
    let mut shell = Shell::new(ShellState::default());
    let dir = tempfile::tempdir().expect("temporary library");
    let engine = engine(dir.path());
    let hooks = HostHooks::none();
    let host = ShellHost {
        dispatcher: engine.dispatcher(),
        hooks: &hooks,
    };
    shell.open_files(
        OpenRequest {
            archives: vec![ARCHIVE.to_owned()],
            ..OpenRequest::default()
        },
        &host,
    );
    let titles: Vec<&str> = shell
        .toasts()
        .items()
        .iter()
        .map(|toast| toast.title.as_str())
        .collect();
    assert_eq!(titles, ["This archive could not be opened"]);
}

#[test]
fn audio_handed_over_by_the_desktop_is_imported_into_the_library() {
    let mut fixture = Fixture::new();
    fixture.with(|shell, host| {
        shell.open_files(
            OpenRequest {
                audio: vec!["/tmp/meeting.wav".to_owned()],
                ..OpenRequest::default()
            },
            host,
        );
    });
    assert_eq!(fixture.shell.route(), &Route::Library);
    assert_eq!(fixture.shell.status(), "Importing 1 file(s)…");
}

#[test]
fn a_refused_path_is_reported_by_its_reason_and_never_by_its_path() {
    let mut fixture = Fixture::new();
    fixture.with(|shell, host| {
        shell.open_files(
            OpenRequest {
                refused: vec!["outside_approved_roots".to_owned()],
                ..OpenRequest::default()
            },
            host,
        );
    });
    let toasts = fixture.shell.toasts().items();
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].title, "1 file(s) could not be opened");
    assert_eq!(
        toasts[0].body.as_deref(),
        Some("Reason(s): outside_approved_roots.")
    );
    assert_eq!(
        fixture.shell.route(),
        &Route::Library,
        "a refusal does not navigate"
    );
}
