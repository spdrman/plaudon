//! The window: the O1.3 shell over the O1.2 engine.
//!
//! This is the glue and nothing else. It owns the [`AppEngine`], drains its
//! events into [`Shell`], supplies the host hooks the view layer is not
//! allowed to reach for itself, and persists the shell's presentation state
//! off the render thread.
//!
//! Screens are registered here, one line each:
//!
//! ```ignore
//! let mut app = PlaudonShell::new(engine, hooks, state);
//! app.shell_mut().register(Box::new(LibraryScreen::new()));
//! ```
//!
//! Until a screen is registered its destination renders an honest "not
//! available yet" state naming the task that owns it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_app::event::Waker;
use plaudon_app::runtime::AppEngine;
use plaudon_app::shell_state::ShellState;
use plaudon_platform::activation::Activation;
use plaudon_platform::dragdrop::DropClassification;
use plaudon_platform::theme::OsTheme;
use plaudon_ui::shell::{ActivationTarget, HostHooks, OpenRequest, Shell, ShellHost};

use crate::desktop::{Desktop, DesktopEvent};

/// How long the shell waits after the last change before writing its state.
///
/// A window drag produces a geometry change every frame; writing each one
/// would hit the disk sixty times a second for no benefit.
pub const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);

/// How long the engine is given to finish what the closing window started.
///
/// The last frame can dispatch a `save_transcript_revision`; the process
/// must not exit before it has committed. Everything durable commits before
/// it reports, so anything still running after this is safe to abandon.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The waker the engine calls: one repaint per batch of events, no polling.
#[derive(Debug, Clone)]
struct RepaintWaker {
    ctx: egui::Context,
}

impl Waker for RepaintWaker {
    fn wake(&self) {
        self.ctx.request_repaint();
    }
}

/// The application the binary runs.
#[derive(Debug)]
pub struct PlaudonShell {
    /// `None` only after [`PlaudonShell::on_exit`] has wound the runtime
    /// down, which is the last thing that happens to this value.
    engine: Option<AppEngine>,
    hooks: HostHooks,
    shell: Shell,
    /// The platform adapters, when the binary supplied them. A test drives
    /// the shell without any, and queues [`DesktopEvent`]s directly.
    desktop: Option<Desktop>,
    /// What the desktop asked for before the first frame could ask for it:
    /// the launch argument vector, parsed before the window existed.
    queued_desktop: Vec<DesktopEvent>,
    /// Set when the desktop asked to quit, so the window closes once.
    closing: bool,
    waker_installed: bool,
    pending_save: Option<(ShellState, Instant)>,
    /// Whether the window had focus on the previous frame, so the operating
    /// system's appearance is re-read when it comes back and never per
    /// frame.
    was_focused: bool,
}

impl PlaudonShell {
    /// Build the application over a running engine and the restored state.
    #[must_use]
    pub fn new(engine: AppEngine, hooks: HostHooks, state: ShellState) -> Self {
        Self {
            engine: Some(engine),
            hooks,
            shell: Shell::new(state),
            desktop: None,
            queued_desktop: Vec::new(),
            closing: false,
            waker_installed: false,
            pending_save: None,
            was_focused: true,
        }
    }

    /// Hand over the platform adapters (matrix rows C10 and C11).
    ///
    /// The binary builds them inside eframe's creation closure, because on
    /// macOS the status item has to be created on the main thread after the
    /// event loop started; owning them here is what makes their events
    /// arrive, since nothing else is called once per frame.
    pub fn set_desktop(&mut self, desktop: Desktop) {
        self.desktop = Some(desktop);
    }

    /// Queue something the desktop asked for before the window existed.
    ///
    /// The launch argument vector is parsed before `eframe::run_native`, so
    /// its request waits here until there is a frame to answer it in.
    pub fn queue_desktop_event(&mut self, event: DesktopEvent) {
        self.queued_desktop.push(event);
    }

    /// The shell, for registering screens and for tests.
    pub fn shell_mut(&mut self) -> &mut Shell {
        &mut self.shell
    }

    /// The shell.
    #[must_use]
    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// The engine, for tests that assert on the application layer.
    ///
    /// # Panics
    ///
    /// Panics after the window has closed and the runtime has been wound
    /// down, which is the only point at which there is no engine.
    #[must_use]
    pub fn engine(&self) -> &AppEngine {
        self.engine
            .as_ref()
            .expect("the engine is owned until the window closes")
    }

    /// Install the repaint waker once the interface's context exists.
    pub fn install_waker(&mut self, ctx: &egui::Context) {
        if !self.waker_installed {
            if let Some(engine) = self.engine.as_ref() {
                engine.set_waker(Arc::new(RepaintWaker { ctx: ctx.clone() }));
            }
            self.waker_installed = true;
        }
    }

    /// Hand everything the engine produced to the shell.
    pub fn pump_events(&mut self) {
        let Some(engine) = self.engine.as_ref() else {
            return;
        };
        let host = ShellHost {
            dispatcher: engine.dispatcher(),
            hooks: &self.hooks,
        };
        for event in engine.events().drain() {
            self.shell.handle_event(event, &host);
        }
    }

    /// Hand everything the desktop delivered to the shell (matrix rows C10
    /// and C11).
    ///
    /// Called once per frame before the shell paints, so an activation is
    /// answered by the frame it arrived in. Every event has exactly one
    /// effect and none is dropped.
    pub fn pump_desktop(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        {
            // An Apple event can land while the window is idle, and this
            // application paints nothing when nothing happened.
            crate::mac_open::set_repaint_target(ctx);
            // The application menu posts to the same process-global `muda`
            // channel the tray uses, and it exists even when the tray
            // backend could not be built (parity row S8).
            for id in crate::app_menu::drain() {
                self.with_host(|shell, host| shell.run_command(&id, host));
            }
        }

        // The operating system's appearance is re-read when the window comes
        // back, never per frame: it is a process spawn on macOS.
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        let regained = focused && !self.was_focused;
        self.was_focused = focused;

        let mut events = std::mem::take(&mut self.queued_desktop);
        if let Some(desktop) = self.desktop.as_mut() {
            events.extend(desktop.drain());
            if regained {
                events.extend(desktop.refresh_os_theme());
            }
        }
        for event in events {
            self.apply_desktop_event(event, ctx);
        }
    }

    /// One desktop event, as the one thing it means.
    fn apply_desktop_event(&mut self, event: DesktopEvent, ctx: &egui::Context) {
        match event {
            DesktopEvent::FocusWindow => self.shell.activate_window(),
            DesktopEvent::Activate(activation) => {
                self.shell.activate_target(activation_target(activation));
            }
            DesktopEvent::Open(classified) => {
                let request = open_request(classified);
                self.with_host(|shell, host| shell.open_files(request, host));
            }
            DesktopEvent::TrayCommand(id) => {
                self.with_host(|shell, host| shell.run_command(id.as_str(), host));
            }
            DesktopEvent::Quit => self.request_close(ctx),
            DesktopEvent::OsThemeChanged(theme) => {
                self.shell.set_system_dark(theme == OsTheme::Dark);
            }
        }
    }

    /// Run `visit` with the shell and a host over the live engine.
    ///
    /// Does nothing once the engine has been wound down: there is no
    /// dispatcher to send to, and a command raised then could not commit.
    fn with_host<R>(&mut self, visit: impl FnOnce(&mut Shell, &ShellHost<'_>) -> R) -> Option<R> {
        let engine = self.engine.as_ref()?;
        let host = ShellHost {
            dispatcher: engine.dispatcher(),
            hooks: &self.hooks,
        };
        Some(visit(&mut self.shell, &host))
    }

    /// Close the window the orderly way, once.
    ///
    /// `ViewportCommand::Close` and not an immediate shutdown: eframe then
    /// calls [`PlaudonShell::on_exit`], which is where every screen gets its
    /// last turn and the shell state is flushed.
    fn request_close(&mut self, ctx: &egui::Context) {
        if self.closing {
            return;
        }
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Paint one frame.
    pub fn view(&mut self, ui: &mut egui::Ui) {
        self.pump_desktop(ui.ctx());
        self.pump_events();
        let Some(engine) = self.engine.as_ref() else {
            return;
        };
        let host = ShellHost {
            dispatcher: engine.dispatcher(),
            hooks: &self.hooks,
        };
        self.shell.ui(ui, &host);
        // Parity rows C10 and J15: a notification click, a deep link or a
        // tray click routes the shell and raises the window. The shell
        // decided where to go; only the host can un-minimise and focus a
        // viewport.
        if self.shell.take_activation() {
            for command in crate::desktop::focus_commands() {
                ui.ctx().send_viewport_cmd(command);
            }
        }
        self.persist(ui.ctx());
    }

    /// Write the presentation state once it has stopped changing.
    fn persist(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if let Some(state) = self.shell.take_dirty_state() {
            self.pending_save = Some((state, now));
        }
        let Some((state, changed_at)) = &self.pending_save else {
            return;
        };
        if now.duration_since(*changed_at) < SAVE_DEBOUNCE {
            // Come back when the debounce expires, and not before: this is
            // the only timer the shell owns.
            ctx.request_repaint_after(SAVE_DEBOUNCE);
            return;
        }
        (self.hooks.save_shell_state)(state.clone());
        self.pending_save = None;
    }

    /// Write the state now, whatever the debounce says. Called when the
    /// window is closing.
    pub fn flush_state(&mut self) {
        let state = self
            .pending_save
            .take()
            .map(|(state, _)| state)
            .or_else(|| self.shell.take_dirty_state());
        if let Some(state) = state {
            (self.hooks.save_shell_state)(state);
        }
    }

    /// Give every screen its last turn, then write the shell state.
    ///
    /// The order matters: a screen's `on_shutdown` can dispatch a command —
    /// the transcript editor's pending autosave is exactly that — so the
    /// commands go out first and the runtime is wound down afterwards, with
    /// [`SHUTDOWN_GRACE`] to finish them.
    pub fn shutdown(&mut self) {
        if let Some(engine) = self.engine.as_ref() {
            let host = ShellHost {
                dispatcher: engine.dispatcher(),
                hooks: &self.hooks,
            };
            self.shell.shutdown(&host);
        }
        self.flush_state();
        if let Some(engine) = self.engine.take() {
            engine.shutdown(SHUTDOWN_GRACE);
        }
    }
}

/// The platform's activation, as the shell's own vocabulary.
///
/// Total in both directions and deliberately dull: the view layer does not
/// depend on `plaudon-platform`, so this is the one place the two
/// three-case enums meet.
const fn activation_target(activation: Activation) -> ActivationTarget {
    match activation {
        Activation::Recording(recording_id) => ActivationTarget::Recording(recording_id),
        Activation::Job(job_id) => ActivationTarget::Job(job_id),
        Activation::Settings => ActivationTarget::Settings,
    }
}

/// A validated drop or launch, as the shell's open request.
///
/// Only reason codes cross: a path the application refused is user data,
/// and the toast that reports the refusal has no business echoing it.
fn open_request(classified: DropClassification) -> OpenRequest {
    let mut refused: Vec<String> = classified
        .rejected
        .iter()
        .map(|rejected| rejected.reason.reason_code().to_owned())
        .collect();
    // A folder is not a document: the installer claims no folder type, and
    // watched folders are configured in Settings, not opened.
    refused.extend(
        classified
            .directories
            .iter()
            .map(|_| "folder_is_not_a_document".to_owned()),
    );
    OpenRequest {
        audio: classified
            .audio
            .into_iter()
            .map(|validated| validated.requested)
            .collect(),
        archives: classified
            .archives
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        refused,
    }
}

impl eframe::App for PlaudonShell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.install_waker(ui.ctx());
        self.view(ui);
        // No unconditional `request_repaint`: at idle this window paints
        // nothing.
    }

    fn on_exit(&mut self) {
        self.shutdown();
    }
}
