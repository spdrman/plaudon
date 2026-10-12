//! The screen registry: how a feature team plugs a screen into the shell.
//!
//! A screen is a [`Screen`] implementation registered against one
//! [`Destination`]. Registration is one line, and a destination with no
//! screen is not a crash and not a blank panel: the registry renders an
//! honest empty state naming the task that owns it, so "not built yet" can
//! never be mistaken for "built and empty".
//!
//! A screen never touches the application layer directly. Everything it can
//! do — send a command, navigate, raise a toast, set the status line, read
//! the design tokens, ask the host for a file — arrives through
//! [`ScreenCtx`], which is also what makes a screen testable: the context is
//! constructed from a dispatcher and nothing else.

use std::collections::BTreeMap;

use plaudon_app::command::{AppCommand, AppResponse, CommandId, Dispatcher};
use plaudon_app::dto::UiEvent;
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::theme::Tokens;
use crate::widgets::toast::{ToastTone, Toasts};

use super::commands::CommandSpec;
use super::host::HostHooks;
use super::route::{Destination, Route};

/// What a screen asked the shell for, collected during a frame.
///
/// Grouping these in one place is what lets the shell hand a screen a
/// context while still holding the registry the screen lives in.
#[derive(Debug, Default)]
pub struct ShellBus {
    /// Messages raised by screens and by the shell itself.
    pub toasts: Toasts,
    /// A navigation a screen asked for.
    pub navigation: Option<Route>,
    /// A status line a screen set.
    pub status: Option<String>,
    /// Whether a screen asked for another frame.
    pub repaint: bool,
    /// Commands screens dispatched this frame, for the activity indicator.
    pub sent: usize,
}

/// Everything a screen may do to the world outside itself.
#[derive(Debug)]
pub struct ScreenCtx<'a> {
    dispatcher: &'a Dispatcher,
    hooks: &'a HostHooks,
    tokens: Tokens,
    route: &'a Route,
    bus: &'a mut ShellBus,
}

impl<'a> ScreenCtx<'a> {
    /// Build a context. The shell does this once per frame; a test does it
    /// directly to drive a screen without the shell.
    pub fn new(
        dispatcher: &'a Dispatcher,
        hooks: &'a HostHooks,
        tokens: Tokens,
        route: &'a Route,
        bus: &'a mut ShellBus,
    ) -> Self {
        Self {
            dispatcher,
            hooks,
            tokens,
            route,
            bus,
        }
    }

    /// Send a command. Returns immediately; the outcome arrives as an
    /// [`AppResponse`] in [`Screen::on_response`].
    pub fn send(&mut self, command: AppCommand) -> CommandId {
        self.bus.sent += 1;
        self.dispatcher.send(command)
    }

    /// The dispatcher, for a screen that needs to hold a clone.
    #[must_use]
    pub fn dispatcher(&self) -> &Dispatcher {
        self.dispatcher
    }

    /// The route being shown, including its sub-route parameters.
    #[must_use]
    pub fn route(&self) -> &Route {
        self.route
    }

    /// Ask the shell to go somewhere else.
    pub fn navigate(&mut self, route: Route) {
        self.bus.navigation = Some(route);
    }

    /// The design tokens of the theme being painted.
    #[must_use]
    pub fn tokens(&self) -> Tokens {
        self.tokens
    }

    /// Platform capabilities the view layer is not allowed to reach for
    /// itself: the file picker, and anything else the binary supplies.
    #[must_use]
    pub fn hooks(&self) -> &HostHooks {
        self.hooks
    }

    /// Raise a toast. Toasts are announced politely and dismissed by the
    /// user; they never steal focus and never auto-hide an error.
    pub fn toast(&mut self, tone: ToastTone, title: impl Into<String>, body: Option<String>) {
        self.bus.toasts.push(tone, title, body);
    }

    /// Replace the status-bar text.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.bus.status = Some(status.into());
    }

    /// Ask for another frame: a worker finished, an animation is running.
    pub fn request_repaint(&mut self) {
        self.bus.repaint = true;
    }
}

/// One screen of the application.
///
/// Only [`Screen::destination`] and [`Screen::ui`] are required.
pub trait Screen: std::fmt::Debug {
    /// The destination this screen answers for.
    fn destination(&self) -> Destination;

    /// Paint the screen.
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>);

    /// Whether this screen has side-pane content right now.
    ///
    /// Asked before the layout is computed, so it must be cheap and must not
    /// paint; the shell allocates the resizable right-hand pane only when it
    /// answers `true`.
    fn has_aside(&self) -> bool {
        false
    }

    /// Paint the right-hand pane. Only called when [`Screen::has_aside`]
    /// answered `true` and the user has not hidden the pane.
    fn aside(&mut self, _ui: &mut egui::Ui, _cx: &mut ScreenCtx<'_>) {}

    /// A command this screen sent has an outcome.
    fn on_response(
        &mut self,
        _id: CommandId,
        _command: &'static str,
        _result: &Result<AppResponse, ErrorEnvelope>,
        _cx: &mut ScreenCtx<'_>,
    ) {
    }

    /// Something changed in the application (spec §15.3). Events never carry
    /// state: refetch what this invalidates.
    fn on_ui_event(&mut self, _event: &UiEvent, _cx: &mut ScreenCtx<'_>) {}

    /// The event stream was interrupted; everything this screen holds may be
    /// stale.
    fn on_resync(&mut self, _reason: ResyncReason, _cx: &mut ScreenCtx<'_>) {}

    /// The desktop asked the application to open a portable archive
    /// (parity row C10: the `.plaudon` document type the installer claims).
    ///
    /// The path has already been resolved inside an approved root by the
    /// host. Return `true` when the screen took it; the shell says so
    /// honestly when nothing did, rather than leaving the user in front of a
    /// screen that quietly ignored their file.
    fn on_open_archive(&mut self, _path: &str, _cx: &mut ScreenCtx<'_>) -> bool {
        false
    }

    /// The window is closing.
    ///
    /// The last chance to finish what the user started: dispatch a pending
    /// autosave, stop a device, hand over anything that would otherwise be
    /// lost. Commands sent here are real commands — the host gives the
    /// engine a grace period to finish them before the process exits — but
    /// nothing will be painted again, so a screen must not expect a
    /// response.
    fn on_shutdown(&mut self, _cx: &mut ScreenCtx<'_>) {}

    /// Commands this screen contributes to the central map, and therefore to
    /// the palette and the keyboard reference.
    fn commands(&self) -> &[CommandSpec] {
        &[]
    }

    /// Run one of this screen's own commands. Return `false` for an
    /// identifier the screen does not know.
    fn handle_command(&mut self, _id: &str, _cx: &mut ScreenCtx<'_>) -> bool {
        false
    }
}

/// The screens registered with this shell.
#[derive(Debug, Default)]
pub struct ScreenRegistry {
    screens: BTreeMap<Destination, Box<dyn Screen>>,
    onboarding: Option<Box<dyn Screen>>,
}

impl ScreenRegistry {
    /// An empty registry: every destination renders its empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `screen` for the destination it declares, replacing any
    /// previous one.
    pub fn register(&mut self, screen: Box<dyn Screen>) -> Option<Box<dyn Screen>> {
        self.screens.insert(screen.destination(), screen)
    }

    /// Register the onboarding screen, which is shown instead of the shell
    /// until onboarding is complete (parity row S15, owned by O2.4).
    pub fn register_onboarding(&mut self, screen: Box<dyn Screen>) -> Option<Box<dyn Screen>> {
        self.onboarding.replace(screen)
    }

    /// Whether a destination has a screen.
    #[must_use]
    pub fn has(&self, destination: Destination) -> bool {
        self.screens.contains_key(&destination)
    }

    /// Whether an onboarding screen was registered.
    #[must_use]
    pub fn has_onboarding(&self) -> bool {
        self.onboarding.is_some()
    }

    /// Every registered screen, for the shell's event fan-out.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Screen>> {
        self.screens.values_mut().chain(self.onboarding.iter_mut())
    }

    /// One screen, for a question that does not paint.
    #[must_use]
    pub fn get(&self, destination: Destination) -> Option<&dyn Screen> {
        self.screens.get(&destination).map(AsRef::as_ref)
    }

    /// Take a screen out so it can be painted while the shell is borrowed.
    /// The caller must put it back with [`ScreenRegistry::restore`].
    pub(crate) fn take(&mut self, destination: Destination) -> Option<Box<dyn Screen>> {
        self.screens.remove(&destination)
    }

    pub(crate) fn restore(&mut self, destination: Destination, screen: Box<dyn Screen>) {
        self.screens.insert(destination, screen);
    }

    pub(crate) fn take_onboarding(&mut self) -> Option<Box<dyn Screen>> {
        self.onboarding.take()
    }

    pub(crate) fn restore_onboarding(&mut self, screen: Box<dyn Screen>) {
        self.onboarding = Some(screen);
    }
}

/// The empty state shown where a screen has not landed yet.
///
/// It names the destination and the task that owns it, and it offers no
/// controls, because there is nothing it could honestly do.
pub fn not_available(ui: &mut egui::Ui, destination: Destination) {
    let tokens = Tokens::get(ui.ctx());
    ui.vertical_centered(|ui| {
        ui.add_space(tokens.space(8));
        ui.heading(destination.label());
        ui.add_space(tokens.space(4));
        ui.label(format!(
            "{} is not available in this build yet.",
            destination.label()
        ));
        ui.label(
            egui::RichText::new(format!(
                "It arrives with task {} of the desktop rebuild.",
                destination.owning_task()
            ))
            .color(tokens.color.text_secondary),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Stub(Destination);

    impl Screen for Stub {
        fn destination(&self) -> Destination {
            self.0
        }
        fn ui(&mut self, ui: &mut egui::Ui, _cx: &mut ScreenCtx<'_>) {
            ui.label("stub");
        }
    }

    #[test]
    fn registering_a_screen_claims_exactly_one_destination() {
        let mut registry = ScreenRegistry::new();
        assert!(!registry.has(Destination::Library));
        assert!(registry
            .register(Box::new(Stub(Destination::Library)))
            .is_none());
        assert!(registry.has(Destination::Library));
        assert!(!registry.has(Destination::Jobs));
    }

    #[test]
    fn registering_twice_replaces_rather_than_silently_keeping_both() {
        let mut registry = ScreenRegistry::new();
        registry.register(Box::new(Stub(Destination::Jobs)));
        let previous = registry.register(Box::new(Stub(Destination::Jobs)));
        assert!(previous.is_some());
        assert_eq!(registry.iter_mut().count(), 1);
    }

    #[test]
    fn every_destination_names_the_task_that_owns_its_screen() {
        for destination in Destination::ALL {
            assert!(destination.owning_task().starts_with('O'));
        }
    }
}
