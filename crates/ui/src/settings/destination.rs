//! The one screen registered for [`Destination::Settings`].
//!
//! Three routes land on this destination — `Route::Settings`,
//! `Route::SettingsActivity` and `Route::SettingsSources` — but the registry
//! holds one screen per destination. This host owns the three children and
//! chooses between them from [`ScreenCtx::route`].
//!
//! Two things are deliberate:
//!
//! * **Answers go to every child, not just the visible one.** A response, a
//!   §15.3 event and a resync are delivered to all three, because the command
//!   that produced a response was sent by whichever child was on screen at the
//!   time, and the user may have moved since. Only the frame-shaped calls —
//!   `ui`, `aside`, `commands`, `handle_command` — follow the route.
//! * **The sub-routes are reachable.** A click on Settings in the navigation
//!   rail opens `Route::Settings` and nothing else, so this screen puts the two
//!   sub-routes at the top of it and a way back on each of them. Every label is
//!   [`Route::title`], so no user-facing words are invented here.

use plaudon_app::command::{AppResponse, CommandId};
use plaudon_app::dto::UiEvent;
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::activity::shell_adapter::ActivityShellScreen;
use crate::screens::sources::shell_adapter::SourcesShellScreen;
use crate::shell::commands::CommandSpec;
use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};

use super::shell_adapter::SettingsShellScreen;

/// Which child a route selects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Child {
    /// The settings sections themselves.
    #[default]
    Settings,
    /// The activity history.
    Activity,
    /// The PLAUD connector and watched folders.
    Sources,
}

impl Child {
    /// The child a route selects. Anything that falls to this destination and
    /// is not one of the two sub-routes is the settings screen itself.
    fn of(route: &Route) -> Self {
        match route {
            Route::SettingsActivity => Self::Activity,
            Route::SettingsSources => Self::Sources,
            _ => Self::Settings,
        }
    }
}

/// The Settings destination: three screens behind one registration.
#[derive(Debug, Default)]
pub struct SettingsDestinationScreen {
    settings: SettingsShellScreen,
    activity: ActivityShellScreen,
    sources: SourcesShellScreen,
    /// The child the last frame painted. [`Screen::commands`] and
    /// [`Screen::has_aside`] are asked without a context, so the route they
    /// answer for is the one most recently painted.
    showing: Child,
}

impl SettingsDestinationScreen {
    /// A fresh Settings destination.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The settings screen itself, so the binary can publish the live command
    /// registry to its shortcut editor.
    pub fn settings_mut(&mut self) -> &mut SettingsShellScreen {
        &mut self.settings
    }

    /// The activity history, for a test that wants to inspect it.
    pub fn activity_mut(&mut self) -> &mut ActivityShellScreen {
        &mut self.activity
    }

    /// The sources screen, for a test that wants to seed it.
    pub fn sources_mut(&mut self) -> &mut SourcesShellScreen {
        &mut self.sources
    }

    /// The links from the settings route to its two sub-routes.
    fn sub_route_links(ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        ui.horizontal_wrapped(|ui| {
            for route in [Route::SettingsActivity, Route::SettingsSources] {
                if ui.button(route.title()).clicked() {
                    cx.navigate(route);
                }
            }
        });
    }

    /// The way back from a sub-route.
    fn back_link(ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if ui.button(Route::Settings.title()).clicked() {
            cx.navigate(Route::Settings);
        }
    }
}

impl Screen for SettingsDestinationScreen {
    fn destination(&self) -> Destination {
        Destination::Settings
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        self.showing = Child::of(cx.route());
        match self.showing {
            Child::Settings => {
                Self::sub_route_links(ui, cx);
                ui.separator();
                self.settings.ui(ui, cx);
            }
            Child::Activity => {
                Self::back_link(ui, cx);
                ui.separator();
                self.activity.ui(ui, cx);
            }
            Child::Sources => {
                Self::back_link(ui, cx);
                ui.separator();
                self.sources.ui(ui, cx);
            }
        }
    }

    fn has_aside(&self) -> bool {
        match self.showing {
            Child::Settings => self.settings.has_aside(),
            Child::Activity => self.activity.has_aside(),
            Child::Sources => self.sources.has_aside(),
        }
    }

    fn aside(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        let child = Child::of(cx.route());
        match child {
            Child::Settings => self.settings.aside(ui, cx),
            Child::Activity => self.activity.aside(ui, cx),
            Child::Sources => self.sources.aside(ui, cx),
        }
    }

    fn on_response(
        &mut self,
        id: CommandId,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        // All three: the child that sent this command may no longer be the
        // child on screen, and an answer dropped because the user navigated is
        // an answer lost.
        self.settings.on_response(id, command, result, cx);
        self.activity.on_response(id, command, result, cx);
        self.sources.on_response(id, command, result, cx);
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        self.settings.on_ui_event(event, cx);
        self.activity.on_ui_event(event, cx);
        self.sources.on_ui_event(event, cx);
    }

    fn on_resync(&mut self, reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        self.settings.on_resync(reason, cx);
        self.activity.on_resync(reason, cx);
        self.sources.on_resync(reason, cx);
    }

    fn commands(&self) -> &[CommandSpec] {
        match self.showing {
            Child::Settings => self.settings.commands(),
            Child::Activity => self.activity.commands(),
            Child::Sources => self.sources.commands(),
        }
    }

    fn handle_command(&mut self, id: &str, cx: &mut ScreenCtx<'_>) -> bool {
        // Taken before the children are touched: `cx.route()` borrows the
        // context, and the child wants it mutably.
        let child = Child::of(cx.route());
        match child {
            Child::Settings => self.settings.handle_command(id, cx),
            Child::Activity => self.activity.handle_command(id, cx),
            Child::Sources => self.sources.handle_command(id, cx),
        }
    }
}
