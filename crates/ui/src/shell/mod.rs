//! The application shell (EPIC O task O1.3, #201).
//!
//! Everything around a screen: the navigation rail, the menu bar, the
//! toolbar, the resizable panes, the status bar, the toasts, the keyboard
//! map, the command palette, the themes and the state that survives a
//! restart. Parity rows S1–S14, S16 and S17 of
//! `docs/EGUI_MIGRATION_PLAN.md` §4.1.
//!
//! Three rules hold throughout:
//!
//! * **One command map.** The menu, the toolbar, the palette, the keyboard
//!   and a screen's own entries all run [`commands::CommandSpec`]s from one
//!   [`commands::CommandRegistry`], so the keyboard reference cannot drift
//!   from what the keyboard does.
//! * **Nothing reaches the application except through the dispatcher.** A
//!   shell action that touches the library sends an
//!   [`AppCommand`](plaudon_app::command::AppCommand); the shell never calls
//!   a backend, never blocks and never awaits.
//! * **No polling.** The shell repaints when there is an event, an input or
//!   a running animation. It asks for a frame only when it has a reason.

pub mod activation;
pub mod bell;
pub mod commands;
pub mod host;
pub mod intents;
pub mod layout;
pub mod menu;
pub mod nav;
pub mod palette;
pub mod route;
pub mod screen;
pub mod shortcuts;
pub mod statusbar;
pub mod toolbar;
pub mod window_state;

pub use activation::{ActivationTarget, OpenRequest};
pub use commands::{CommandGroup, CommandRegistry, CommandSpec, RegisterError, ShellAction};
pub use host::HostHooks;
pub use palette::Palette;
pub use route::{Destination, Route};
pub use screen::{Screen, ScreenCtx, ScreenRegistry};
pub use shortcuts::{BindingError, KeyMap};
pub use window_state::WindowTracker;

use std::collections::BTreeMap;

use plaudon_app::command::{AppCommand, AppResponse, CommandId, Dispatcher};
use plaudon_app::dto::{
    Capabilities, DuplicatePolicy, EventKind, ImportFilesRequest, ImportOutcome,
    ListActivityRequest, UiEvent,
};
use plaudon_app::event::{AppEvent, ResyncReason};
use plaudon_app::shell_state::{PanelLayout, ShellState, SIDEBAR_WIDTH_RANGE};
use plaudon_craft_ui::layout::SplitSize;
use plaudon_domain::error::ErrorEnvelope;

use crate::activity::{unread, PAGE_SIZE as ACTIVITY_PAGE_SIZE};
use crate::theme::{ThemeState, Tokens, COMPACT_LAYOUT_WIDTH};
use crate::widgets::toast::{ToastTone, Toasts};

use self::screen::ShellBus;

/// What the shell needs from its host for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ShellHost<'a> {
    /// The one path into the application.
    pub dispatcher: &'a Dispatcher,
    /// Platform capabilities the view may not reach for itself.
    pub hooks: &'a HostHooks,
}

/// Where the shell is in its startup sequence (parity row S14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Startup {
    /// `get_capabilities` is in flight.
    Loading,
    /// It failed; the user is offered the same retry the React shell gave.
    Failed(Box<ErrorEnvelope>),
    /// The shell is usable.
    Ready,
}

/// The shell.
///
/// The several `bool`s are genuinely independent pieces of window state —
/// an overlay being open says nothing about whether the sidebar is collapsed
/// — so `clippy::struct_excessive_bools` is allowed rather than inventing a
/// flags type that would have to be destructured at every use.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct Shell {
    screens: ScreenRegistry,
    commands: CommandRegistry,
    keymap: KeyMap,
    palette: Palette,
    bus: ShellBus,
    theme: ThemeState,
    tokens: Tokens,
    panels: PanelLayout,
    window: WindowTracker,
    route: Route,
    startup: Startup,
    capabilities: Option<Box<Capabilities>>,
    capabilities_request: Option<CommandId>,
    status: String,
    in_flight: usize,
    shortcuts_open: bool,
    aside_visible: bool,
    focus_main: bool,
    dirty: bool,
    pending_import: Option<CommandId>,
    /// Unread activity entries, as the last `list_activity` answer counted
    /// them. The header bell is the only reader (parity row J12).
    activity_unread: usize,
    activation_requested: bool,
    in_window_menu: bool,
}

impl Shell {
    /// Build a shell over the state restored from disk.
    #[must_use]
    pub fn new(state: ShellState) -> Self {
        let state = state.sanitized(None);
        let theme = ThemeState {
            appearance: state.appearance,
            contrast: state.contrast,
            system_dark: false,
        };
        Self {
            screens: ScreenRegistry::new(),
            commands: CommandRegistry::with_builtins(),
            keymap: KeyMap::from_overrides(&state.bindings),
            palette: Palette::new(),
            bus: ShellBus::default(),
            theme,
            tokens: Tokens::for_theme(theme.effective()),
            panels: state.panels,
            window: WindowTracker::new(state.window),
            route: Route::default(),
            startup: Startup::Loading,
            capabilities: None,
            capabilities_request: None,
            status: "Starting".to_owned(),
            in_flight: 0,
            shortcuts_open: false,
            aside_visible: true,
            focus_main: false,
            dirty: false,
            pending_import: None,
            activity_unread: 0,
            activation_requested: false,
            in_window_menu: true,
        }
    }

    /// Register a screen. One line per screen, which is the whole contract
    /// between the shell and a feature team.
    pub fn register(&mut self, screen: Box<dyn Screen>) {
        for spec in screen.commands() {
            if let Err(error) = self.commands.register(spec.clone()) {
                tracing::warn!(command = spec.id, %error, "a screen command was refused");
            }
        }
        self.screens.register(screen);
    }

    /// Register the onboarding screen shown until onboarding is complete.
    pub fn register_onboarding(&mut self, screen: Box<dyn Screen>) {
        self.screens.register_onboarding(screen);
    }

    /// Add a command that belongs to no screen.
    ///
    /// # Errors
    ///
    /// Fails when the identifier is empty or already registered.
    pub fn register_command(&mut self, spec: CommandSpec) -> Result<(), RegisterError> {
        self.commands.register(spec)
    }

    /// Every command the shell knows, for a settings editor or a test.
    #[must_use]
    pub fn command_specs(&self) -> &[CommandSpec] {
        self.commands.all()
    }

    /// The live command registry, for a host that has to mirror the menu
    /// somewhere this crate cannot reach — the macOS application menu is the
    /// only such place.
    #[must_use]
    pub const fn commands(&self) -> &CommandRegistry {
        &self.commands
    }

    /// The live key map, so a mirrored menu shows the chords the shell will
    /// actually honour rather than the defaults.
    #[must_use]
    pub const fn keymap(&self) -> &KeyMap {
        &self.keymap
    }

    /// Tell the shell whether to draw its own menu bar.
    ///
    /// The host turns this off only when it has installed a *working* native
    /// menu over the same description ([`menu::menu_model`]); nothing in this
    /// crate decides it by target, so a test build — which installs no native
    /// menu — always keeps the in-window bar and with it the AccessKit
    /// evidence ADR-0023 asks for.
    pub const fn set_in_window_menu(&mut self, visible: bool) {
        self.in_window_menu = visible;
    }

    /// The keyboard overrides the user has made.
    #[must_use]
    pub fn bindings(&self) -> BTreeMap<String, Vec<String>> {
        self.keymap.overrides().clone()
    }

    /// Replace the keyboard overrides, reporting the ones that were refused.
    pub fn apply_bindings(
        &mut self,
        overrides: &BTreeMap<String, Vec<String>>,
    ) -> Vec<BindingError> {
        let refused = self.keymap.apply(&self.commands, overrides);
        self.dirty = true;
        refused
    }

    /// Where the shell is.
    #[must_use]
    pub fn route(&self) -> &Route {
        &self.route
    }

    /// Go somewhere. Used by the shell itself, by screens and by a deep link
    /// from a notification activation.
    pub fn navigate(&mut self, route: Route) {
        if self.route != route {
            self.route = route;
        }
    }

    /// The startup state, for the host and for tests.
    #[must_use]
    pub fn startup(&self) -> &Startup {
        &self.startup
    }

    /// The theme being painted.
    #[must_use]
    pub fn theme(&self) -> ThemeState {
        self.theme
    }

    /// Switch the appearance preference.
    pub fn set_appearance(&mut self, appearance: plaudon_app::dto::Appearance) {
        if self.theme.appearance != appearance {
            self.theme.appearance = appearance;
            self.dirty = true;
        }
    }

    /// Switch the contrast preference.
    pub fn set_contrast(&mut self, contrast: plaudon_app::dto::Contrast) {
        if self.theme.contrast != contrast {
            self.theme.contrast = contrast;
            self.dirty = true;
        }
    }

    /// The status line.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// The toasts currently showing.
    #[must_use]
    pub fn toasts(&self) -> &Toasts {
        &self.bus.toasts
    }

    /// The palette, for tests.
    #[must_use]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Unread activity entries, as the header bell shows them (row J12).
    ///
    /// The count the last `list_activity` answer carried, counted with
    /// [`crate::activity::unread`]; a host that mirrors the bell — a dock
    /// badge, say — reads it here rather than counting a second time.
    #[must_use]
    pub const fn activity_unread(&self) -> usize {
        self.activity_unread
    }

    /// Whether the keyboard reference is open.
    #[must_use]
    pub const fn shortcuts_open(&self) -> bool {
        self.shortcuts_open
    }

    /// The panel sizes.
    #[must_use]
    pub const fn panels(&self) -> PanelLayout {
        self.panels
    }

    /// Everything that should survive a restart.
    #[must_use]
    pub fn shell_state(&self) -> ShellState {
        ShellState {
            window: self.window.geometry(),
            panels: self.panels,
            appearance: self.theme.appearance,
            contrast: self.theme.contrast,
            bindings: self.keymap.overrides().clone(),
        }
    }

    /// The state to persist, if anything changed since the last call.
    pub fn take_dirty_state(&mut self) -> Option<ShellState> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        Some(self.shell_state())
    }

    /// Whether a destination has a screen, rather than the registry's
    /// "not built yet" state.
    #[must_use]
    pub fn has_screen(&self, destination: Destination) -> bool {
        self.screens.has(destination)
    }

    /// Whether the first-run flow is registered (parity row S15).
    #[must_use]
    pub fn has_onboarding(&self) -> bool {
        self.screens.has_onboarding()
    }

    /// The window is closing: give every screen its last, frame-less turn.
    ///
    /// Called by the host from `eframe::App::on_exit`, before the engine is
    /// wound down, so a command a screen sends here still runs.
    pub fn shutdown(&mut self, host: &ShellHost<'_>) {
        self.for_each_screen(host, |screen, cx| screen.on_shutdown(cx));
    }

    /// Apply one event from the application.
    ///
    /// The host drains the channel and hands every event here; the shell
    /// keeps what concerns it and fans the rest out to the screens.
    pub fn handle_event(&mut self, event: AppEvent, host: &ShellHost<'_>) {
        match event {
            AppEvent::Response {
                id,
                command,
                result,
            } => self.handle_response(id, command, &result, host),
            AppEvent::Ui(event) => self.handle_ui_event(&event, host),
            AppEvent::Resync { reason, dropped } => self.handle_resync(reason, dropped, host),
        }
    }

    fn handle_response(
        &mut self,
        id: CommandId,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        host: &ShellHost<'_>,
    ) {
        self.in_flight = self.in_flight.saturating_sub(1);
        match result {
            Ok(AppResponse::GetCapabilities(capabilities)) => {
                self.capabilities = Some(Box::new(capabilities.clone()));
                self.capabilities_request = None;
                self.startup = Startup::Ready;
                "Ready".clone_into(&mut self.status);
            }
            Ok(AppResponse::ListActivity(page)) => {
                // One count, from the function the history itself counts
                // with: the bell cannot disagree with the page (row J12).
                self.activity_unread = unread(&page.items);
            }
            Ok(AppResponse::CompleteOnboarding(preferences)) => {
                // The gate is the shell's, so the shell is what lifts it:
                // the stored preferences have changed, and the capabilities
                // it holds would otherwise still say onboarding is pending.
                if let Some(capabilities) = self.capabilities.as_mut() {
                    capabilities.preferences = preferences.clone();
                }
                self.navigate(Route::Library);
            }
            Ok(AppResponse::ImportFiles(response)) => {
                if self.pending_import == Some(id) {
                    self.pending_import = None;
                }
                self.report_import(&response.results);
            }
            Err(envelope) => {
                if self.capabilities_request == Some(id) {
                    self.capabilities_request = None;
                    self.startup = Startup::Failed(Box::new(envelope.clone()));
                }
                if self.pending_import == Some(id) {
                    self.pending_import = None;
                }
                // The envelope is all the user sees: a canonical message and
                // a diagnostic id, never a path, a key or transcript text.
                self.status.clone_from(&envelope.message);
                self.bus.toasts.push(
                    ToastTone::Danger,
                    envelope.message.clone(),
                    Some(format!("Diagnostic id {}", envelope.diagnostic_id)),
                );
            }
            Ok(_) => {}
        }
        self.for_each_screen(host, |screen, cx| {
            screen.on_response(id, command, result, cx);
        });
    }

    fn report_import(&mut self, outcomes: &[ImportOutcome]) {
        let accepted = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ImportOutcome::Accepted { .. }))
            .count();
        let rejected = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ImportOutcome::Rejected { .. }))
            .count();
        let duplicates = outcomes.len() - accepted - rejected;
        self.status = format!("Imported {accepted} of {} file(s)", outcomes.len());
        let tone = if rejected > 0 {
            ToastTone::Warning
        } else {
            ToastTone::Success
        };
        let detail = (duplicates > 0 || rejected > 0)
            .then(|| format!("{duplicates} already in the library, {rejected} rejected"));
        self.bus
            .toasts
            .push(tone, format!("Imported {accepted} recording(s)"), detail);
    }

    fn handle_ui_event(&mut self, event: &UiEvent, host: &ShellHost<'_>) {
        if event.kind == EventKind::ActivationRequested {
            self.activate(event);
        }
        // §15.3: the event says an entry exists, never what it is. The count
        // is read back from the library rather than incremented locally,
        // which is also what keeps it right when an entry is read elsewhere.
        if event.kind == EventKind::ActivityAdded {
            self.request_activity(host);
        }
        self.for_each_screen(host, |screen, cx| screen.on_ui_event(event, cx));
    }

    /// Read the first page of the activity history, for the header bell.
    ///
    /// The answer lands in [`Shell::handle_response`] like any other. The
    /// page the bell counts is the same first page the history shows, which
    /// is the point: two reads of one thing, never two definitions of it.
    fn request_activity(&mut self, host: &ShellHost<'_>) {
        self.send(
            host,
            AppCommand::ListActivity(ListActivityRequest {
                cursor: None,
                limit: ACTIVITY_PAGE_SIZE,
            }),
        );
    }

    /// A notification was clicked (parity row J15).
    ///
    /// The event names what the user clicked, so the shell goes there; the
    /// window itself is the host's to raise, which it does by taking
    /// [`Shell::take_activation`]. The shell never guesses a destination
    /// from an event that names nothing — it raises the window and leaves
    /// the user where they were.
    fn activate(&mut self, event: &UiEvent) {
        match ActivationTarget::from_event(event) {
            Some(target) => self.activate_target(target),
            None => self.activate_window(),
        }
    }

    /// Show what a deep link, a notification or a tray click named (parity
    /// rows C10 and J15).
    ///
    /// The host calls this directly: a `plaudon://` URL is already a target,
    /// and making the host fabricate a `§15.3` event to deliver one would
    /// put a second, fake event source into a stream whose whole value is
    /// that everything in it was committed by the application layer.
    pub fn activate_target(&mut self, target: ActivationTarget) {
        self.activate_window();
        self.navigate(target.route());
    }

    /// Raise the window without going anywhere.
    ///
    /// A tray click and an activation that names nothing both mean "come
    /// forward", and neither is a reason to move the user.
    pub fn activate_window(&mut self) {
        self.activation_requested = true;
    }

    /// Open documents the desktop handed over (parity row C10).
    ///
    /// The installer claims the `.plaudon` extension and the audio types the
    /// importer accepts, so this is where that claim is honoured: audio is
    /// imported exactly as the Import command imports it, and an archive
    /// opens the restore flow with that archive already chosen — the user
    /// still inspects and confirms, because nothing may restore a library
    /// because a file was double-clicked.
    ///
    /// Paths the host refused are reported rather than dropped, by their
    /// stable reason code: the path itself is user data the window has no
    /// reason to echo.
    pub fn open_files(&mut self, request: OpenRequest, host: &ShellHost<'_>) {
        if !request.audio.is_empty() {
            let count = request.audio.len();
            self.navigate(Route::Library);
            self.pending_import = Some(self.send(
                host,
                AppCommand::ImportFiles(ImportFilesRequest {
                    paths: request.audio,
                    duplicate_policy: DuplicatePolicy::OpenExisting,
                }),
            ));
            self.status = format!("Importing {count} file(s)…");
        }
        if let Some(archive) = request.archives.first() {
            self.open_archive(archive, host);
            if request.archives.len() > 1 {
                self.bus.toasts.push(
                    ToastTone::Warning,
                    "Only the first archive was opened",
                    Some("A library is restored from one archive at a time.".to_owned()),
                );
            }
        }
        if !request.refused.is_empty() {
            let count = request.refused.len();
            self.bus.toasts.push(
                ToastTone::Warning,
                format!("{count} file(s) could not be opened"),
                Some(format!("Reason(s): {}.", request.refused.join(", "))),
            );
        }
    }

    /// Hand a portable archive to the screen that owns restoring.
    fn open_archive(&mut self, path: &str, host: &ShellHost<'_>) {
        self.navigate(Route::Settings);
        let taken = self
            .with_active_screen(host, |screen, cx| screen.on_open_archive(path, cx))
            .unwrap_or(false);
        if taken {
            "Opened an archive to restore from".clone_into(&mut self.status);
        } else {
            self.bus.toasts.push(
                ToastTone::Warning,
                "This archive could not be opened",
                Some("Backup and restore is not available in this build.".to_owned()),
            );
        }
    }

    /// Record the light/dark preference the operating system reports.
    ///
    /// egui reads the system appearance itself where the windowing backend
    /// exposes it ([`crate::theme::ThemeState::observe_system`]); this is the
    /// host's second source, for the platforms where it does not, and it is
    /// pushed rather than polled.
    pub fn set_system_dark(&mut self, dark: bool) {
        self.theme.system_dark = dark;
    }

    /// Whether the window should be raised and focused, consumed once.
    pub fn take_activation(&mut self) -> bool {
        std::mem::take(&mut self.activation_requested)
    }

    fn handle_resync(&mut self, reason: ResyncReason, dropped: u64, host: &ShellHost<'_>) {
        if reason == ResyncReason::Overflow {
            tracing::debug!(dropped, "event backlog overflowed; screens refetch");
        }
        self.for_each_screen(host, |screen, cx| screen.on_resync(reason, cx));
    }

    /// Run a command by identifier, from wherever it was invoked.
    pub fn run_command(&mut self, id: &str, host: &ShellHost<'_>) {
        let Some(action) = self.commands.get(id).map(|spec| spec.action.clone()) else {
            return;
        };
        match action {
            ShellAction::Navigate(route) => self.navigate(route),
            ShellAction::OpenPalette => self.palette.open(),
            ShellAction::ShowShortcuts => self.shortcuts_open = true,
            ShellAction::CloseOverlay => {
                self.palette.close();
                self.shortcuts_open = false;
            }
            ShellAction::ToggleSidebar => {
                self.panels.sidebar_collapsed = !self.panels.sidebar_collapsed;
                self.dirty = true;
            }
            ShellAction::ToggleAside => self.aside_visible = !self.aside_visible,
            ShellAction::FocusMain => self.focus_main = true,
            ShellAction::SetAppearance(appearance) => self.set_appearance(appearance),
            ShellAction::SetContrast(contrast) => self.set_contrast(contrast),
            ShellAction::ImportAudio => self.import_audio(host),
            ShellAction::Refresh => {
                self.request_capabilities(host);
                self.for_each_screen(host, |screen, cx| {
                    screen.on_resync(ResyncReason::Subscribed, cx);
                });
            }
            ShellAction::Screen => {
                let owned = id.to_owned();
                self.with_active_screen(host, |screen, cx| {
                    screen.handle_command(&owned, cx);
                });
            }
        }
    }

    fn import_audio(&mut self, host: &ShellHost<'_>) {
        let paths = (host.hooks.pick_audio_files)();
        if paths.is_empty() {
            "Import cancelled".clone_into(&mut self.status);
            return;
        }
        let count = paths.len();
        let request = ImportFilesRequest {
            paths: paths
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            duplicate_policy: DuplicatePolicy::OpenExisting,
        };
        self.pending_import = Some(self.send(host, AppCommand::ImportFiles(request)));
        self.status = format!("Importing {count} file(s)…");
    }

    fn request_capabilities(&mut self, host: &ShellHost<'_>) {
        if self.capabilities_request.is_some() {
            return;
        }
        self.capabilities_request = Some(self.send(host, AppCommand::GetCapabilities));
    }

    fn send(&mut self, host: &ShellHost<'_>, command: AppCommand) -> CommandId {
        self.in_flight += 1;
        host.dispatcher.send(command)
    }

    /// Run `visit` against every registered screen.
    fn for_each_screen(
        &mut self,
        host: &ShellHost<'_>,
        mut visit: impl FnMut(&mut dyn Screen, &mut ScreenCtx<'_>),
    ) {
        let mut screens = std::mem::take(&mut self.screens);
        for screen in screens.iter_mut() {
            let mut cx = ScreenCtx::new(
                host.dispatcher,
                host.hooks,
                self.tokens,
                &self.route,
                &mut self.bus,
            );
            visit(screen.as_mut(), &mut cx);
        }
        self.screens = screens;
        self.drain_bus();
    }

    /// Run `visit` against the screen the current route belongs to, if one is
    /// registered.
    fn with_active_screen<R>(
        &mut self,
        host: &ShellHost<'_>,
        visit: impl FnOnce(&mut dyn Screen, &mut ScreenCtx<'_>) -> R,
    ) -> Option<R> {
        let destination = self.route.destination();
        let mut screen = self.screens.take(destination)?;
        let outcome = {
            let mut cx = ScreenCtx::new(
                host.dispatcher,
                host.hooks,
                self.tokens,
                &self.route,
                &mut self.bus,
            );
            visit(screen.as_mut(), &mut cx)
        };
        self.screens.restore(destination, screen);
        self.drain_bus();
        Some(outcome)
    }

    /// Apply what the screens asked for during the call that just finished.
    fn drain_bus(&mut self) {
        if let Some(route) = self.bus.navigation.take() {
            self.navigate(route);
        }
        if let Some(status) = self.bus.status.take() {
            self.status = status;
        }
        self.in_flight += std::mem::take(&mut self.bus.sent);
    }

    /// Paint one frame.
    ///
    /// Takes a `Ui` rather than a `Context` so the same code paints inside
    /// `eframe` and inside a kittest harness; the shell's own panels are
    /// nested with `show_inside`.
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &ShellHost<'_>) {
        let ctx = ui.ctx().clone();
        self.theme.observe_system(&ctx);
        self.tokens = self.theme.apply(&ctx);
        if self.window.track(&ctx) {
            self.dirty = true;
        }
        if self.capabilities.is_none() && self.capabilities_request.is_none() {
            self.request_capabilities(host);
            // The header bell has nothing to show until the history has been
            // read once, and this is that read. It is sent beside the
            // capabilities rather than behind them so start-up stays one
            // round trip; everything after it is driven by `activity.added`,
            // so the shell still never polls.
            self.request_activity(host);
        }

        for id in shortcuts::pressed_commands(&ctx, &self.commands, &self.keymap) {
            self.run_command(&id, host);
        }

        match self.startup.clone() {
            Startup::Loading => self.loading_ui(ui),
            Startup::Failed(envelope) => self.failed_ui(ui, &envelope, host),
            Startup::Ready => {
                if self.onboarding_required() {
                    self.onboarding_ui(ui, host);
                } else {
                    self.shell_ui(ui, host);
                }
            }
        }

        if let Some(id) = self.palette.ui(&ctx, &self.commands, &self.keymap) {
            self.run_command(&id, host);
        }
        shortcuts::shortcuts_dialog(&ctx, &self.commands, &self.keymap, &mut self.shortcuts_open);
        self.bus.toasts.ui(&ctx);
        if std::mem::take(&mut self.bus.repaint) {
            ctx.request_repaint();
        }
    }

    /// Whether onboarding still gates the shell (parity row S15).
    #[must_use]
    pub fn onboarding_required(&self) -> bool {
        self.capabilities
            .as_ref()
            .is_some_and(|capabilities| !capabilities.preferences.onboarding_completed)
    }

    fn loading_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(self.tokens.space(8));
                ui.add(egui::Spinner::new());
                ui.label("Loading your library…");
            });
        });
    }

    fn failed_ui(&mut self, ui: &mut egui::Ui, envelope: &ErrorEnvelope, host: &ShellHost<'_>) {
        let mut retry = false;
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(self.tokens.space(8));
                ui.heading("Plaudon could not start");
                ui.label(&envelope.message);
                ui.label(
                    egui::RichText::new(format!("Diagnostic id {}", envelope.diagnostic_id))
                        .color(self.tokens.color.text_secondary),
                );
                ui.add_space(self.tokens.space(4));
                retry = ui.button("Try again").clicked();
            });
        });
        if retry {
            self.startup = Startup::Loading;
            self.request_capabilities(host);
        }
    }

    fn onboarding_ui(&mut self, ui: &mut egui::Ui, host: &ShellHost<'_>) {
        let Some(mut screen) = self.screens.take_onboarding() else {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(self.tokens.space(8));
                    ui.heading("Welcome to Plaudon");
                    ui.label("First-run setup is not available in this build yet.");
                    ui.label(
                        egui::RichText::new(
                            "It arrives with task O2.4 (#208) of the desktop rebuild.",
                        )
                        .color(self.tokens.color.text_secondary),
                    );
                });
            });
            return;
        };
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let mut cx = ScreenCtx::new(
                host.dispatcher,
                host.hooks,
                self.tokens,
                &self.route,
                &mut self.bus,
            );
            screen.ui(ui, &mut cx);
        });
        self.screens.restore_onboarding(screen);
        self.drain_bus();
    }

    fn shell_ui(&mut self, ui: &mut egui::Ui, host: &ShellHost<'_>) {
        let mut invoked: Vec<String> = Vec::new();

        if self.in_window_menu {
            egui::Panel::top("plaudon-menu-bar").show(ui, |ui| {
                if let Some(id) = menu::menu_bar(ui, &self.commands, &self.keymap) {
                    invoked.push(id);
                }
            });
        }
        let mut bell_rung = false;
        egui::Panel::top("plaudon-toolbar").show(ui, |ui| {
            let items = toolbar_items();
            let title = self.route.title();
            let unread = self.activity_unread;
            if let Some(id) = toolbar::toolbar(ui, &self.commands, &self.keymap, &items, |ui| {
                // Right-to-left: the bell sits at the far right of the
                // header, the route title to its left (parity row J12).
                bell_rung = bell::activity_bell(ui, unread);
                ui.label(
                    egui::RichText::new(title)
                        .size(self.tokens.text.lg)
                        .strong(),
                );
            }) {
                invoked.push(id);
            }
        });
        if bell_rung {
            self.navigate(Route::SettingsActivity);
        }
        let notice = statusbar::narrow_notice(ui.ctx().content_rect().width());
        egui::Panel::bottom("plaudon-status-bar").show(ui, |ui| {
            statusbar::status_bar(ui, &self.status, self.in_flight, notice.as_deref());
        });
        egui::CentralPanel::default_margins().show(ui, |ui| self.body(ui, host));

        for id in invoked {
            self.run_command(&id, host);
        }
    }

    fn body(&mut self, ui: &mut egui::Ui, host: &ShellHost<'_>) {
        if Self::skip_link(ui) {
            self.focus_main = true;
        }

        let compact = ui.ctx().content_rect().width() < COMPACT_LAYOUT_WIDTH;
        let collapsed = compact || self.panels.sidebar_collapsed;
        let rect = ui.available_rect_before_wrap();
        let available: [bool; Destination::ALL.len()] =
            Destination::ALL.map(|d| self.screens.has(d));

        let (nav_rect, content_rect) = if collapsed {
            let split = rect.width() > nav::COLLAPSED_WIDTH * 2.0;
            if split {
                let mut nav = rect;
                nav.max.x = rect.min.x + nav::COLLAPSED_WIDTH;
                let mut content = rect;
                content.min.x = nav.max.x + layout::DIVIDER_WIDTH;
                (nav, content)
            } else {
                (egui::Rect::NOTHING, rect)
            }
        } else {
            match layout::vertical_divider(
                ui,
                egui::Id::new("plaudon-sidebar-divider"),
                "Resize the sidebar",
                SplitSize::FixedFirst(self.panels.sidebar_width),
                SIDEBAR_WIDTH_RANGE.0,
                rect,
            ) {
                Some(split) => {
                    if split.changed {
                        self.panels.sidebar_width = split.size;
                        self.panels = self.panels.sanitized();
                        self.dirty = true;
                    }
                    (split.first, split.second)
                }
                // Too narrow for two panes: the screen keeps the window and
                // the rail is reachable through the menu and `Mod+1`…`Mod+6`.
                None => (egui::Rect::NOTHING, rect),
            }
        };

        if nav_rect.is_positive() {
            let mut nav_ui = layout::pane(ui, nav_rect);
            nav::navigation_landmark(&mut nav_ui);
            let chosen = nav::nav_rail(
                &mut nav_ui,
                self.route.destination(),
                collapsed,
                &|destination| {
                    Destination::ALL
                        .iter()
                        .position(|candidate| *candidate == destination)
                        .is_some_and(|index| available[index])
                },
            );
            if let Some(destination) = chosen {
                self.navigate(destination.route());
            }
        }

        let wants_aside = self.aside_visible
            && self
                .screens
                .get(self.route.destination())
                .is_some_and(Screen::has_aside);
        let (main_rect, aside_rect) = if wants_aside {
            match layout::vertical_divider(
                ui,
                egui::Id::new("plaudon-aside-divider"),
                "Resize the side pane",
                SplitSize::FixedSecond(self.panels.aside_width),
                SIDEBAR_WIDTH_RANGE.0,
                content_rect,
            ) {
                Some(split) => {
                    if split.changed {
                        self.panels.aside_width = split.size;
                        self.panels = self.panels.sanitized();
                        self.dirty = true;
                    }
                    (split.first, Some(split.second))
                }
                None => (content_rect, None),
            }
        } else {
            (content_rect, None)
        };

        let mut main_ui = layout::pane(ui, main_rect);
        let main_id = main_landmark(&mut main_ui);
        if std::mem::take(&mut self.focus_main) {
            ui.ctx().memory_mut(|memory| memory.request_focus(main_id));
        }
        let destination = self.route.destination();
        if self
            .with_active_screen(host, |screen, cx| screen.ui(&mut main_ui, cx))
            .is_none()
        {
            screen::not_available(&mut main_ui, destination);
        }

        if let Some(rect) = aside_rect {
            let mut aside_ui = layout::pane(ui, rect);
            self.with_active_screen(host, |screen, cx| screen.aside(&mut aside_ui, cx));
        }
    }

    /// The first focusable control in the window, as `App.tsx`'s skip link
    /// was (parity row S3).
    fn skip_link(ui: &mut egui::Ui) -> bool {
        let response = ui.add(
            egui::Button::new("Skip to main content")
                .small()
                .frame(ui.memory(|memory| memory.focused().is_some())),
        );
        response.clicked()
    }
}

/// The actions the toolbar offers, in order. Everything else is reachable
/// from the menu, the palette and the keyboard.
fn toolbar_items() -> Vec<toolbar::ToolbarItem> {
    vec![
        toolbar::ToolbarItem::new("import_audio"),
        toolbar::ToolbarItem::new("search_library"),
        toolbar::ToolbarItem::new("command_palette"),
        toolbar::ToolbarItem::new("refresh"),
    ]
}

/// The main landmark: `Role::Main`, focusable, so the skip link has
/// somewhere to send focus.
fn main_landmark(ui: &mut egui::Ui) -> egui::Id {
    let id = egui::Id::new("plaudon-main-region");
    let rect = egui::Rect::from_min_size(ui.max_rect().min, egui::Vec2::splat(1.0));
    let response = ui.interact(rect, id, egui::Sense::click());
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Main);
        node.set_label("Main content");
    });
    id
}
