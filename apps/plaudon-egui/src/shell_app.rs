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
use plaudon_ui::shell::{HostHooks, Shell, ShellHost};

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
    waker_installed: bool,
    pending_save: Option<(ShellState, Instant)>,
}

impl PlaudonShell {
    /// Build the application over a running engine and the restored state.
    #[must_use]
    pub fn new(engine: AppEngine, hooks: HostHooks, state: ShellState) -> Self {
        Self {
            engine: Some(engine),
            hooks,
            shell: Shell::new(state),
            waker_installed: false,
            pending_save: None,
        }
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

    /// Paint one frame.
    pub fn view(&mut self, ui: &mut egui::Ui) {
        self.pump_events();
        let Some(engine) = self.engine.as_ref() else {
            return;
        };
        let host = ShellHost {
            dispatcher: engine.dispatcher(),
            hooks: &self.hooks,
        };
        self.shell.ui(ui, &host);
        // Parity row J15: a notification click routes the shell and raises
        // the window. The shell decided where to go; only the host can
        // un-minimise and focus a viewport.
        if self.shell.take_activation() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
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
