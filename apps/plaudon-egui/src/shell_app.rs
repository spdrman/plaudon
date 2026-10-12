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
    engine: AppEngine,
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
            engine,
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
    #[must_use]
    pub fn engine(&self) -> &AppEngine {
        &self.engine
    }

    /// Install the repaint waker once the interface's context exists.
    pub fn install_waker(&mut self, ctx: &egui::Context) {
        if !self.waker_installed {
            self.engine
                .set_waker(Arc::new(RepaintWaker { ctx: ctx.clone() }));
            self.waker_installed = true;
        }
    }

    /// Hand everything the engine produced to the shell.
    pub fn pump_events(&mut self) {
        let host = ShellHost {
            dispatcher: self.engine.dispatcher(),
            hooks: &self.hooks,
        };
        for event in self.engine.events().drain() {
            self.shell.handle_event(event, &host);
        }
    }

    /// Paint one frame.
    pub fn view(&mut self, ui: &mut egui::Ui) {
        self.pump_events();
        let host = ShellHost {
            dispatcher: self.engine.dispatcher(),
            hooks: &self.hooks,
        };
        self.shell.ui(ui, &host);
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
}

impl eframe::App for PlaudonShell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.install_waker(ui.ctx());
        self.view(ui);
        // No unconditional `request_repaint`: at idle this window paints
        // nothing.
    }

    fn on_exit(&mut self) {
        self.flush_state();
    }
}
