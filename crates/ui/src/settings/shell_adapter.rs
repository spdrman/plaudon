//! How the Settings destination and the first-run flow plug into the shell.
//!
//! The screens themselves know nothing about `plaudon_ui::shell`: they render
//! from [`SettingsData`](super::SettingsData) and raise
//! [`SettingsIntent`]s, which is what lets a kittest test drive a section with
//! no dispatcher, no routing and no host hooks. This module is the one place
//! that knows both halves — it implements [`Screen`] by draining the intents
//! and turning each one into the shell call it means.
//!
//! The mapping is deliberately total: every intent has exactly one shell
//! action, so a new intent is a compile error here rather than a silently
//! ignored button.

use plaudon_app::command::{AppResponse, CommandId};
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::shell::commands::{builtin_commands, CommandSpec};
use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};
use crate::theme::Tokens;

use super::intent::{FilePurpose, SettingsDestination, SettingsIntent};
use super::theme::SettingsTokens;
use super::{PickedPath, SettingsOutput, SettingsScreen};
use crate::onboarding::OnboardingScreen;

/// The shell's palette, seen through the small trait the sections render with.
///
/// The mapping is the one agreed with O1.3: surface, text, muted text, border
/// and the three status roles come straight from the 20-role token set, so a
/// settings section and a library row use the same colour for the same thing.
impl SettingsTokens for Tokens {
    fn surface(&self) -> egui::Color32 {
        self.color.bg_surface
    }
    fn text(&self) -> egui::Color32 {
        self.color.text_primary
    }
    fn muted_text(&self) -> egui::Color32 {
        self.color.text_secondary
    }
    fn border(&self) -> egui::Color32 {
        self.color.border_subtle
    }
    fn accent(&self) -> egui::Color32 {
        self.color.accent
    }
    fn danger(&self) -> egui::Color32 {
        self.color.status_danger
    }
    fn warning(&self) -> egui::Color32 {
        self.color.status_warning
    }
    fn success(&self) -> egui::Color32 {
        self.color.status_success
    }
}

/// Where a settings navigation target lands in the shell's route table.
const fn route_for(destination: SettingsDestination) -> Route {
    match destination {
        SettingsDestination::Sources => Route::SettingsSources,
        SettingsDestination::Activity => Route::SettingsActivity,
        SettingsDestination::Jobs => Route::Jobs,
    }
}

/// Drain one frame's intents into the shell.
///
/// `picked` collects the dialogs that have to be opened *after* the frame:
/// `pick_folder` blocks on the OS, and calling it from inside the render pass
/// would freeze the frame it was raised in.
fn apply(
    out: &mut SettingsOutput,
    cx: &mut ScreenCtx<'_>,
    copied: &mut Vec<String>,
) -> Vec<(PickedPath, String)> {
    let mut picked = Vec::new();
    for intent in out.drain() {
        match intent {
            SettingsIntent::Command(command) => {
                cx.send(command);
            }
            SettingsIntent::RefreshCapabilities => {
                cx.send(plaudon_app::command::AppCommand::GetCapabilities);
            }
            SettingsIntent::Navigate(destination) => cx.navigate(route_for(destination)),
            SettingsIntent::PickFolder(purpose) => {
                if let Some(path) = (cx.hooks().pick_folder)() {
                    picked.push((PickedPath::Folder(purpose), path.display().to_string()));
                }
            }
            SettingsIntent::PickFile(purpose) => {
                // The shell's host hooks offer a folder picker and a save
                // picker; an archive to restore is an existing file, so it is
                // chosen with the save picker's sibling. Until the host grows
                // one, the folder picker's parent is not a substitute, and
                // saying so is better than opening the wrong dialog.
                if let Some(path) = (cx.hooks().pick_save_path)(suggested_name(purpose)) {
                    picked.push((PickedPath::File(purpose), path.display().to_string()));
                }
            }
            SettingsIntent::RevealInFolder(path) => {
                (cx.hooks().reveal_in_folder)(std::path::Path::new(&path));
            }
            SettingsIntent::CopyText(text) => {
                copied.push(text);
            }
        }
    }
    picked
}

/// The file name the restore dialog opens on.
const fn suggested_name(purpose: FilePurpose) -> &'static str {
    match purpose {
        FilePurpose::RestoreArchive => "library.plaudon",
    }
}

/// Convert the shell's command registry into the editor's view model.
///
/// The two types say the same thing; `plaudon_app::shortcuts::ShortcutSpec`
/// owns its strings because it crosses the command boundary, and the shell's
/// `CommandSpec` is `&'static` because the registry is compiled in.
#[must_use]
pub fn shortcut_specs(specs: &[CommandSpec]) -> Vec<plaudon_app::shortcuts::ShortcutSpec> {
    specs
        .iter()
        .map(|spec| plaudon_app::shortcuts::ShortcutSpec {
            id: spec.id.to_owned(),
            label: spec.label.to_owned(),
            group: spec.group.label().to_owned(),
            default_chord: spec.default_chord.map(str::to_owned),
            rebindable: spec.rebindable,
        })
        .collect()
}

/// The Settings destination, as the shell sees it.
#[derive(Debug, Default)]
pub struct SettingsShellScreen {
    screen: SettingsScreen,
    out: SettingsOutput,
    loaded: bool,
}

impl SettingsShellScreen {
    /// A fresh Settings screen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The screen underneath, for a test that wants to seed it.
    pub fn inner_mut(&mut self) -> &mut SettingsScreen {
        &mut self.screen
    }

    /// Publish the live command registry to the shortcut editor.
    ///
    /// The binary calls this with `Shell::command_specs()` so the editor lists
    /// what the shell actually dispatches, screen-contributed entries
    /// included. Without it the editor falls back to the built-in commands and
    /// says nothing it cannot back up.
    pub fn set_command_registry(&mut self, specs: &[CommandSpec]) {
        self.screen.data_mut().shortcut_specs = shortcut_specs(specs);
    }

    fn drain(&mut self, ui: &egui::Ui, cx: &mut ScreenCtx<'_>) {
        let mut copied = Vec::new();
        let picked = apply(&mut self.out, cx, &mut copied);
        for (path, value) in picked {
            self.screen.path_picked(path, &value, &mut self.out);
        }
        if !self.out.is_empty() {
            apply(&mut self.out, cx, &mut copied);
        }
        for text in copied {
            ui.ctx().copy_text(text);
        }
    }

    fn load(&mut self, cx: &mut ScreenCtx<'_>) {
        for command in SettingsScreen::initial_commands() {
            cx.send(command);
        }
        if self.screen.data().shortcut_specs.is_empty() {
            self.screen.data_mut().shortcut_specs = shortcut_specs(&builtin_commands());
        }
        self.loaded = true;
    }
}

impl Screen for SettingsShellScreen {
    fn destination(&self) -> Destination {
        Destination::Settings
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if !self.loaded {
            self.load(cx);
        }
        let tokens = cx.tokens();
        self.screen.ui_with_tokens(ui, &tokens, &mut self.out);
        self.drain(ui, cx);
    }

    fn on_response(
        &mut self,
        _id: CommandId,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        self.screen.on_response(command, result, &mut self.out);
        let mut copied = Vec::new();
        let picked = apply(&mut self.out, cx, &mut copied);
        for (path, value) in picked {
            self.screen.path_picked(path, &value, &mut self.out);
        }
        apply(&mut self.out, cx, &mut copied);
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        for command in SettingsScreen::initial_commands() {
            cx.send(command);
        }
    }

    fn on_open_archive(&mut self, path: &str, _cx: &mut ScreenCtx<'_>) -> bool {
        self.screen.open_archive(path)
    }
}

/// The first-run flow, as the shell sees it (parity rows C8 and S15).
#[derive(Debug, Default)]
pub struct OnboardingShellScreen {
    screen: OnboardingScreen,
    out: SettingsOutput,
    capabilities: Option<plaudon_app::dto::Capabilities>,
    requested: bool,
}

impl OnboardingShellScreen {
    /// A fresh flow.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Screen for OnboardingShellScreen {
    fn destination(&self) -> Destination {
        // Onboarding is registered with `register_onboarding` and is shown
        // instead of the shell, but the trait still wants a destination; the
        // one it hands over to when it finishes is the library's.
        Destination::Library
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if !self.requested && self.capabilities.is_none() {
            cx.send(plaudon_app::command::AppCommand::GetCapabilities);
            self.requested = true;
        }
        let tokens = cx.tokens();
        if let Some(capabilities) = self.capabilities.clone() {
            self.screen
                .ui_with_tokens(ui, &tokens, &capabilities, &mut self.out);
        } else {
            ui.label("Reading what this machine can do…");
        }
        let mut copied = Vec::new();
        apply(&mut self.out, cx, &mut copied);
        for text in copied {
            ui.ctx().copy_text(text);
        }
    }

    fn on_response(
        &mut self,
        _id: CommandId,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        if let Ok(AppResponse::GetCapabilities(capabilities)) = result {
            self.capabilities = Some(capabilities.clone());
        }
        self.screen.on_response(command, result, &mut self.out);
        apply(&mut self.out, cx, &mut Vec::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    #[test]
    fn the_shell_palette_satisfies_the_sections_token_trait() {
        for kind in ThemeKind::ALL {
            let tokens = Tokens::for_theme(kind);
            assert_eq!(SettingsTokens::surface(&tokens), tokens.color.bg_surface);
            assert_eq!(SettingsTokens::text(&tokens), tokens.color.text_primary);
            assert_eq!(SettingsTokens::danger(&tokens), tokens.color.status_danger);
            assert_ne!(
                SettingsTokens::text(&tokens),
                SettingsTokens::muted_text(&tokens),
                "secondary text must be distinguishable from primary"
            );
        }
    }

    #[test]
    fn every_settings_destination_has_a_route() {
        assert_eq!(
            route_for(SettingsDestination::Sources),
            Route::SettingsSources
        );
        assert_eq!(
            route_for(SettingsDestination::Activity),
            Route::SettingsActivity
        );
        assert_eq!(route_for(SettingsDestination::Jobs), Route::Jobs);
    }

    #[test]
    fn the_settings_screen_answers_for_the_settings_destination() {
        assert_eq!(
            SettingsShellScreen::new().destination(),
            Destination::Settings
        );
    }
}
