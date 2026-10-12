//! Native desktop integration (matrix rows C10, C11, J15).
//!
//! `plaudon-platform` owns the adapters — dialogs, clipboard, opener, OS
//! theme, notifications, tray model, drag-drop classification and
//! deep-link parsing — and is testable without a window. This module is the
//! part that only the binary can own: it holds one set of adapters, drains
//! what the OS delivered since the last frame, and turns it into
//! [`DesktopEvent`]s the shell acts on.
//!
//! Three rules are enforced here rather than left to the caller:
//!
//! * **Nothing polls the OS per frame.** The tray channel is drained because
//!   it is a non-blocking queue the platform already filled; the OS theme is
//!   re-read only when [`Desktop::refresh_os_theme`] is called, which the
//!   shell does on focus, not on every repaint.
//! * **A dropped path is validated before it is an import.** The window hands
//!   over whatever the user dragged; [`Desktop::classify_drop`] resolves it
//!   against the approved roots, so a symlink that escapes to a system
//!   location is refused and the other nine files still import.
//! * **A foreign process cannot navigate the application.** A deep link or a
//!   notification token that this build did not write is dropped, and the
//!   `tracing` line carries a reason code and no user data.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_app::dto::EventKind;
use plaudon_platform::activation::{self, Activation};
use plaudon_platform::clipboard::{host_clipboard, Clipboard, ClipboardError};
use plaudon_platform::dragdrop::{self, DropClassification};
use plaudon_platform::opener::{NativeOpener, OpenError, Opener};
use plaudon_platform::theme::{NativeOsTheme, OsTheme, OsThemeSource};
use plaudon_platform::tray::{Tray, TrayEvent, TrayItemId, TrayModel, TraySupport};

/// Something the desktop asked the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Show and focus the window: a tray click, a notification click or a
    /// second launch with a deep link.
    FocusWindow,
    /// Navigate somewhere, because a notification or a deep link said so.
    Activate(Activation),
    /// A tray menu row was chosen.
    TrayCommand(TrayItemId),
    /// The user asked to quit from the tray.
    Quit,
    /// The operating system's light/dark preference changed.
    OsThemeChanged(OsTheme),
}

/// The tray row that quits.
pub const TRAY_QUIT: &str = "quit";
/// The tray row that shows the window.
pub const TRAY_SHOW: &str = "show";

/// One set of platform adapters, owned by the binary.
pub struct Desktop {
    clipboard: Box<dyn Clipboard>,
    opener: Box<dyn Opener>,
    theme: Box<dyn OsThemeSource>,
    tray: Box<dyn Tray>,
    tray_support: TraySupport,
    approved_roots: Vec<PathBuf>,
    last_os_theme: Option<OsTheme>,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("tray_support", &self.tray_support)
            .field("approved_roots", &self.approved_roots.len())
            .field("last_os_theme", &self.last_os_theme)
            .finish_non_exhaustive()
    }
}

impl Desktop {
    /// The real adapters for this host.
    ///
    /// On macOS the tray must be created on the main thread after the event
    /// loop has started, which is where eframe runs the app constructor, so
    /// this is called from there and nowhere else.
    #[must_use]
    pub fn native(approved_roots: Vec<PathBuf>) -> Self {
        Self::with_parts(
            host_clipboard(),
            Box::new(NativeOpener::new()),
            Box::new(NativeOsTheme::new()),
            crate::tray::host_tray(),
            crate::tray::host_support(),
            approved_roots,
        )
    }

    /// Adapters a test supplies.
    #[must_use]
    pub fn with_parts(
        clipboard: Box<dyn Clipboard>,
        opener: Box<dyn Opener>,
        theme: Box<dyn OsThemeSource>,
        tray: Box<dyn Tray>,
        tray_support: TraySupport,
        approved_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            clipboard,
            opener,
            theme,
            tray,
            tray_support,
            approved_roots,
            last_os_theme: None,
        }
    }

    /// What this build can offer as a tray, in words the Settings screen can
    /// show.
    #[must_use]
    pub const fn tray_support(&self) -> &TraySupport {
        &self.tray_support
    }

    /// Install or replace the tray menu.
    ///
    /// A platform with no backend returns the reason rather than quietly
    /// doing nothing; the caller logs it once and carries on, because a
    /// missing tray is not a reason to refuse to start.
    pub fn apply_tray(&self, model: &TrayModel) -> Result<(), plaudon_platform::tray::TrayError> {
        self.tray.apply(model)
    }

    /// Everything the desktop delivered since the last call.
    ///
    /// Non-blocking: it drains queues the platform already filled, so calling
    /// it once per frame costs a `try_recv` on an empty channel.
    pub fn drain(&mut self) -> Vec<DesktopEvent> {
        let mut events = Vec::new();
        for event in self.tray.poll_events() {
            match event {
                TrayEvent::IconActivated => events.push(DesktopEvent::FocusWindow),
                TrayEvent::MenuItemSelected(id) if id.as_str() == TRAY_QUIT => {
                    events.push(DesktopEvent::Quit);
                }
                TrayEvent::MenuItemSelected(id) if id.as_str() == TRAY_SHOW => {
                    events.push(DesktopEvent::FocusWindow);
                }
                TrayEvent::MenuItemSelected(id) => events.push(DesktopEvent::TrayCommand(id)),
            }
        }
        events
    }

    /// Re-read the operating system's light/dark preference.
    ///
    /// Spawns a short-lived query, so the shell calls it when the window gains
    /// focus or the appearance section opens — never per frame. Returns an
    /// event only when the answer changed.
    pub fn refresh_os_theme(&mut self) -> Option<DesktopEvent> {
        let observed = self.theme.refresh();
        if observed.is_some() && observed != self.last_os_theme {
            self.last_os_theme = observed;
            return observed.map(DesktopEvent::OsThemeChanged);
        }
        self.last_os_theme = observed;
        None
    }

    /// The last observed operating-system theme, without querying again.
    #[must_use]
    pub fn os_theme(&self) -> Option<OsTheme> {
        self.theme.os_theme()
    }

    /// Classify what the user dropped on the window.
    #[must_use]
    pub fn classify_drop(&self, paths: &[PathBuf]) -> DropClassification {
        dragdrop::classify(paths, &self.approved_roots)
    }

    /// Route a deep link into an activation, or refuse it.
    #[must_use]
    pub fn activation_from_url(url: &str) -> Option<DesktopEvent> {
        activation::from_url(url).map(DesktopEvent::Activate)
    }

    /// Route the activation token an OS notification handed back.
    ///
    /// This is the second half of matrix row J15: the application layer
    /// publishes `ui.activation_requested`, and the window focuses itself.
    #[must_use]
    pub fn activation_from_notification(token: &str) -> Option<DesktopEvent> {
        activation::from_notification_token(token).map(DesktopEvent::Activate)
    }

    /// Route a deep link handed over on the command line at launch.
    #[must_use]
    pub fn activation_from_launch<I, S>(arguments: I) -> Option<DesktopEvent>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        activation::from_launch_arguments(arguments).map(DesktopEvent::Activate)
    }

    /// Put non-secret text on the clipboard.
    pub fn copy_text(&self, text: &str) -> Result<(), ClipboardError> {
        self.clipboard.set_text(text)
    }

    /// Show a path selected in the file manager.
    pub fn reveal_in_folder(&self, path: &Path) -> Result<(), OpenError> {
        self.opener.reveal_in_folder(path)
    }

    /// Open a folder in the file manager.
    pub fn open_folder(&self, directory: &Path) -> Result<(), OpenError> {
        self.opener.open_folder(directory)
    }

    /// Open an `http`, `https` or `mailto` URL in the OS handler.
    pub fn open_url(&self, url: &str) -> Result<(), OpenError> {
        self.opener.open_url(url)
    }
}

/// The viewport commands that answer an activation (matrix row J15).
///
/// The React shell un-minimised and focused the window; the same two commands
/// do it here, and they are returned rather than sent so the caller decides
/// when the window is allowed to come forward.
#[must_use]
pub fn focus_commands() -> [egui::ViewportCommand; 2] {
    [
        egui::ViewportCommand::Minimized(false),
        egui::ViewportCommand::Focus,
    ]
}

/// Whether a `§15.3` event is one the desktop layer reacts to.
///
/// Only the activation event: everything else is the shell's business, and a
/// desktop layer that reacted to job updates would be focusing the window
/// behind the user's back.
#[must_use]
pub const fn is_activation_event(kind: EventKind) -> bool {
    matches!(kind, EventKind::ActivationRequested)
}

/// Apply the first tray model and report the reason when there is no tray.
///
/// Called once at startup. A missing tray is logged at `debug`, not `warn`:
/// on Linux it is the expected state, and a warning every launch would train
/// the reader to ignore warnings.
pub fn install_tray(desktop: &Desktop, local_only: bool) {
    let model = crate::tray::default_model(local_only);
    match desktop.apply_tray(&model) {
        Ok(()) => tracing::debug!("the system tray is installed"),
        Err(error) => tracing::debug!(%error, "no system tray in this build"),
    }
}

/// The approved roots a drop is validated against.
///
/// The same set `plaudon-app` validates a picked file against, so dragging a
/// file in and choosing it in the dialog cannot disagree.
#[must_use]
pub fn approved_roots() -> Vec<PathBuf> {
    plaudon_app::default_approved_roots()
}

/// An `Arc` of the real clipboard, for a caller that needs to share it.
#[must_use]
pub fn shared_clipboard() -> Arc<dyn Clipboard> {
    Arc::from(host_clipboard())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_platform::clipboard::FakeClipboard;
    use plaudon_platform::opener::FakeOpener;
    use plaudon_platform::theme::FixedOsTheme;
    use plaudon_platform::tray::{FakeTray, TrayItem};

    fn desktop(tray: FakeTray, theme: FixedOsTheme) -> (Desktop, std::sync::Arc<FakeTray>) {
        let tray = std::sync::Arc::new(tray);
        let desktop = Desktop::with_parts(
            Box::new(FakeClipboard::new()),
            Box::new(FakeOpener::new()),
            Box::new(theme),
            Box::new(std::sync::Arc::clone(&tray)),
            TraySupport::Supported { backend: "fake" },
            vec![],
        );
        (desktop, tray)
    }

    #[test]
    fn a_tray_click_asks_for_the_window() {
        let tray = FakeTray::new();
        tray.push_event(TrayEvent::IconActivated);
        let (mut desktop, _tray) = desktop(tray, FixedOsTheme::silent());
        assert_eq!(desktop.drain(), vec![DesktopEvent::FocusWindow]);
        assert!(desktop.drain().is_empty(), "events are drained once");
    }

    #[test]
    fn the_quit_and_show_rows_are_not_ordinary_commands() {
        let tray = FakeTray::new();
        tray.push_event(TrayEvent::MenuItemSelected(TrayItemId::new(TRAY_QUIT)));
        tray.push_event(TrayEvent::MenuItemSelected(TrayItemId::new(TRAY_SHOW)));
        tray.push_event(TrayEvent::MenuItemSelected(TrayItemId::new("open_jobs")));
        let (mut desktop, _tray) = desktop(tray, FixedOsTheme::silent());
        assert_eq!(
            desktop.drain(),
            vec![
                DesktopEvent::Quit,
                DesktopEvent::FocusWindow,
                DesktopEvent::TrayCommand(TrayItemId::new("open_jobs")),
            ]
        );
    }

    #[test]
    fn the_tray_model_is_handed_to_the_backend_verbatim() {
        let tray = FakeTray::new();
        let (desktop, handle) = desktop(tray, FixedOsTheme::silent());
        let model = TrayModel::new("Plaudon", vec![TrayItem::action("show", "Show Plaudon")]);
        desktop.apply_tray(&model).expect("the fake tray accepts");
        assert_eq!(handle.current().as_ref(), Some(&model));
    }

    #[test]
    fn the_theme_is_reported_only_when_it_changes() {
        let (mut desktop, _tray) = desktop(FakeTray::new(), FixedOsTheme::new(Some(OsTheme::Dark)));
        assert_eq!(
            desktop.refresh_os_theme(),
            Some(DesktopEvent::OsThemeChanged(OsTheme::Dark))
        );
        assert_eq!(desktop.refresh_os_theme(), None, "no change, no event");
    }

    #[test]
    fn a_host_that_does_not_say_produces_no_theme_event() {
        let (mut desktop, _tray) = desktop(FakeTray::new(), FixedOsTheme::silent());
        assert_eq!(desktop.refresh_os_theme(), None);
        assert_eq!(desktop.os_theme(), None);
    }

    #[test]
    fn a_plaudon_deep_link_routes_and_anything_else_does_not() {
        let recording = "plaudon://recording/0199ab5e-7f8c-7c2a-9f4e-2b6d1c3a5e70";
        assert!(matches!(
            Desktop::activation_from_url(recording),
            Some(DesktopEvent::Activate(Activation::Recording(_)))
        ));
        assert_eq!(
            Desktop::activation_from_url("plaudon://settings"),
            Some(DesktopEvent::Activate(Activation::Settings))
        );
        for hostile in [
            "https://example.invalid/recording/1",
            "plaudon://recording/not-a-uuid",
            "plaudon://moon",
            "",
        ] {
            assert_eq!(
                Desktop::activation_from_url(hostile),
                None,
                "{hostile} must not navigate the application"
            );
        }
    }

    #[test]
    fn a_launch_argument_can_carry_a_deep_link_and_argv_zero_cannot() {
        assert_eq!(
            Desktop::activation_from_launch(["plaudon-egui", "plaudon://settings"]),
            Some(DesktopEvent::Activate(Activation::Settings))
        );
        assert_eq!(
            Desktop::activation_from_launch(["plaudon://settings"]),
            None,
            "argv[0] is the program, not a link"
        );
    }

    #[test]
    fn only_the_activation_event_concerns_the_desktop_layer() {
        assert!(is_activation_event(EventKind::ActivationRequested));
        for kind in [
            EventKind::JobUpdated,
            EventKind::LibraryChanged,
            EventKind::ActivityAdded,
        ] {
            assert!(!is_activation_event(kind), "{kind:?}");
        }
    }

    #[test]
    fn focusing_un_minimises_before_it_focuses() {
        let commands = focus_commands();
        assert!(matches!(
            commands[0],
            egui::ViewportCommand::Minimized(false)
        ));
        assert!(matches!(commands[1], egui::ViewportCommand::Focus));
    }

    #[test]
    fn a_drop_outside_every_approved_root_is_refused_without_refusing_the_rest() {
        let root = tempfile::tempdir().expect("tempdir");
        let inside = root.path().join("meeting.wav");
        std::fs::write(&inside, b"RIFF").expect("write");
        let outside = tempfile::tempdir().expect("tempdir");
        let escaped = outside.path().join("elsewhere.wav");
        std::fs::write(&escaped, b"RIFF").expect("write");

        let desktop = Desktop::with_parts(
            Box::new(FakeClipboard::new()),
            Box::new(FakeOpener::new()),
            Box::new(FixedOsTheme::silent()),
            Box::new(FakeTray::new()),
            TraySupport::Supported { backend: "fake" },
            vec![root.path().to_path_buf()],
        );
        let classified = desktop.classify_drop(&[inside, escaped]);
        assert_eq!(classified.audio.len(), 1);
        assert_eq!(classified.rejected.len(), 1);
    }

    #[test]
    fn the_approved_roots_a_drop_uses_are_the_ones_the_application_validates_against() {
        assert_eq!(approved_roots(), plaudon_app::default_approved_roots());
    }

    #[test]
    fn copying_and_opening_go_through_the_adapters() {
        let (desktop, _tray) = desktop(FakeTray::new(), FixedOsTheme::silent());
        desktop.copy_text("sha256:abc").expect("the fake accepts");
        assert!(
            desktop.open_url("javascript:alert(1)").is_err(),
            "only http, https and mailto may be opened"
        );
        desktop
            .open_url("https://plaudon.app")
            .expect("an https url is allowed");
    }
}
