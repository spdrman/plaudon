//! What the desktop asks the running binary to do (parity row C10).
//!
//! Every event here is synthetic: no tray is installed, no Apple event is
//! sent and no window is opened. The assertions are about the one thing the
//! binary owns — that an event the desktop delivered reaches the shell, and
//! reaches it exactly once.

mod support;

use std::path::PathBuf;

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_egui::desktop::{Desktop, DesktopEvent};
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_platform::activation::Activation;
use plaudon_platform::dragdrop::{DropClassification, DropRejection, RejectedDrop};
use plaudon_platform::tray::TrayItemId;
use plaudon_ui::shell::Route;

/// A context to pump against. Nothing is painted: `pump_desktop` reads the
/// viewport's focus and sends viewport commands, neither of which needs a
/// frame.
fn context() -> egui::Context {
    egui::Context::default()
}

/// The application over a fresh library, with every screen registered.
fn app(dir: &std::path::Path) -> PlaudonShell {
    let store = plaudon_app::shell_state::ShellStateStore::new(dir);
    let (audio, _device) = support::fake_audio_out();
    support::app(dir, support::hooks(store), audio)
}

/// The approved roots the binary would classify a launch argument against.
fn roots(dir: &std::path::Path) -> Vec<PathBuf> {
    vec![std::fs::canonicalize(dir).expect("canonical root")]
}

#[test]
fn every_desktop_activation_navigates_and_asks_for_the_window() {
    let dir = tempfile::tempdir().expect("temporary library");
    let mut app = app(dir.path());
    let ctx = context();

    let recording_id = RecordingId::new();
    let job_id = JobId::new();
    let cases = [
        (
            Activation::Recording(recording_id),
            Route::LibraryRecording {
                recording_id: recording_id.to_string(),
            },
        ),
        (
            Activation::Job(job_id),
            Route::JobsJob {
                job_id: job_id.to_string(),
            },
        ),
        (Activation::Settings, Route::Settings),
    ];

    for (activation, expected) in cases {
        app.queue_desktop_event(DesktopEvent::Activate(activation));
        app.pump_desktop(&ctx);
        assert_eq!(app.shell().route(), &expected, "{activation:?}");
        assert!(
            app.shell_mut().take_activation(),
            "{activation:?} did not ask for the window"
        );
    }
}

#[test]
fn several_events_in_one_frame_are_all_applied() {
    let dir = tempfile::tempdir().expect("temporary library");
    let mut app = app(dir.path());
    let ctx = context();
    let job_id = JobId::new();

    app.queue_desktop_event(DesktopEvent::FocusWindow);
    app.queue_desktop_event(DesktopEvent::TrayCommand(TrayItemId::new("toggle_sidebar")));
    app.queue_desktop_event(DesktopEvent::Activate(Activation::Job(job_id)));
    app.queue_desktop_event(DesktopEvent::Open(DropClassification {
        rejected: vec![RejectedDrop {
            path: PathBuf::from("/etc/passwd"),
            reason: DropRejection::OutsideApprovedRoots,
        }],
        ..DropClassification::default()
    }));
    let collapsed_before = app.shell().panels().sidebar_collapsed;

    app.pump_desktop(&ctx);

    assert_eq!(
        app.shell().route(),
        &Route::JobsJob {
            job_id: job_id.to_string()
        },
        "the activation in the batch was dropped"
    );
    assert_ne!(
        app.shell().panels().sidebar_collapsed,
        collapsed_before,
        "the tray command in the batch was dropped"
    );
    assert_eq!(
        app.shell()
            .toasts()
            .items()
            .iter()
            .map(|toast| toast.title.as_str())
            .collect::<Vec<_>>(),
        ["1 file(s) could not be opened"],
        "the refusal in the batch was dropped"
    );
    assert!(app.shell_mut().take_activation());
}

#[test]
fn a_launched_archive_opens_the_restore_flow() {
    let dir = tempfile::tempdir().expect("temporary library");
    let archive = dir.path().join("library.plaudon");
    std::fs::write(&archive, b"PK").expect("write the archive");
    let mut app = app(dir.path());
    let ctx = context();

    let event = Desktop::launch_event(
        ["plaudon-egui".to_owned(), archive.display().to_string()],
        &roots(dir.path()),
    )
    .expect("a double-clicked archive is a request");
    app.queue_desktop_event(event);
    app.pump_desktop(&ctx);

    assert_eq!(app.shell().route(), &Route::Settings);
    assert_eq!(app.shell().status(), "Opened an archive to restore from");
}

#[test]
fn a_launched_audio_file_is_imported() {
    let dir = tempfile::tempdir().expect("temporary library");
    let audio = dir.path().join("meeting.wav");
    std::fs::write(&audio, b"RIFF0000WAVE").expect("write the audio");
    let mut app = app(dir.path());
    let ctx = context();

    let event = Desktop::launch_event(
        ["plaudon-egui".to_owned(), audio.display().to_string()],
        &roots(dir.path()),
    )
    .expect("a dropped-in audio path is a request");
    app.queue_desktop_event(event);
    app.pump_desktop(&ctx);

    assert_eq!(app.shell().route(), &Route::Library);
    assert_eq!(app.shell().status(), "Importing 1 file(s)…");
}

#[test]
fn a_path_outside_the_approved_roots_is_refused_and_never_opened() {
    let dir = tempfile::tempdir().expect("temporary library");
    let elsewhere = tempfile::tempdir().expect("somewhere else");
    let escaped = elsewhere.path().join("library.plaudon");
    std::fs::write(&escaped, b"PK").expect("write the archive");
    let mut app = app(dir.path());
    let ctx = context();

    let event = Desktop::launch_event(
        ["plaudon-egui".to_owned(), escaped.display().to_string()],
        &roots(dir.path()),
    )
    .expect("the refusal is still reported");
    app.queue_desktop_event(event);
    app.pump_desktop(&ctx);

    assert_eq!(
        app.shell().route(),
        &Route::Library,
        "a refused path must not navigate anywhere"
    );
    let toasts = app.shell().toasts().items();
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].title, "1 file(s) could not be opened");
    assert_eq!(
        toasts[0].body.as_deref(),
        Some("Reason(s): outside_approved_roots.")
    );
    assert!(
        !toasts[0].body.as_deref().unwrap_or_default().contains(
            escaped
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .expect("a file name")
        ),
        "a refusal reports a reason code, never the path"
    );
}

#[test]
fn a_deep_link_in_the_argument_vector_reaches_the_shell() {
    let dir = tempfile::tempdir().expect("temporary library");
    let mut app = app(dir.path());
    let ctx = context();
    let recording_id = RecordingId::new();

    let event = Desktop::launch_event(
        [
            "plaudon-egui".to_owned(),
            format!("plaudon://recording/{recording_id}"),
        ],
        &roots(dir.path()),
    )
    .expect("a deep link is a request");
    app.queue_desktop_event(event);
    app.pump_desktop(&ctx);

    assert_eq!(
        app.shell().route(),
        &Route::LibraryRecording {
            recording_id: recording_id.to_string()
        }
    );
    assert!(app.shell_mut().take_activation());
}

#[test]
fn junk_in_the_argument_vector_asks_for_nothing() {
    let dir = tempfile::tempdir().expect("temporary library");
    let roots = roots(dir.path());
    for junk in [
        "-psn_0_1234567",
        "--flag",
        "",
        "https://example.invalid/recording/1",
        "plaudon://recording/not-a-uuid",
    ] {
        assert_eq!(
            Desktop::launch_event(["plaudon-egui", junk], &roots),
            None,
            "{junk} must not ask the application for anything"
        );
    }
}

#[test]
fn quitting_from_the_tray_is_orderly_and_the_os_theme_reaches_the_shell() {
    let dir = tempfile::tempdir().expect("temporary library");
    let mut app = app(dir.path());
    let ctx = context();

    app.queue_desktop_event(DesktopEvent::OsThemeChanged(
        plaudon_platform::theme::OsTheme::Dark,
    ));
    app.queue_desktop_event(DesktopEvent::Quit);
    app.pump_desktop(&ctx);

    assert!(
        app.shell().theme().system_dark,
        "the operating system's appearance was dropped"
    );
    // Quitting asks the viewport to close; the engine is wound down in
    // `on_exit`, after every screen has had its last turn, so a screen's
    // pending autosave is still dispatchable at this point.
    let _ = app.engine();
}
