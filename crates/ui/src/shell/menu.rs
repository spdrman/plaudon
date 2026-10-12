//! The menu bar (parity row S8).
//!
//! **Why egui menus and not a native menu bar.** The plan asks for a
//! "platform menu where practical". A native bar means `muda`, which is a new
//! external dependency outside the vendored set, needs its own licence-register
//! row and `cargo deny` review, renders nothing a kittest harness can see, and
//! carries no AccessKit tree of its own — so the keyboard-reachability and
//! screen-reader evidence ADR-0023 requires would stop at the menu. egui's own
//! menu bar is in the AccessKit tree, is driven by the same command registry as
//! the palette and the keyboard map, and is tested by the same harness.
//! Entries are labelled with the platform's own modifier (Cmd on macOS, Ctrl
//! elsewhere) through [`super::shortcuts::display`].

use egui::containers::menu::{MenuBar, MenuButton};

use super::commands::CommandRegistry;
use super::route::Destination;
use super::shortcuts::{display, KeyMap};

/// Draw the menu bar and report the command the user chose.
pub fn menu_bar(ui: &mut egui::Ui, registry: &CommandRegistry, keymap: &KeyMap) -> Option<String> {
    let mut invoked: Option<String> = None;
    MenuBar::new().ui(ui, |ui| {
        MenuButton::new("File").ui(ui, |ui| {
            entry(ui, registry, keymap, "import_audio", &mut invoked);
            ui.separator();
            entry(ui, registry, keymap, "refresh", &mut invoked);
        });
        MenuButton::new("View").ui(ui, |ui| {
            entry(ui, registry, keymap, "toggle_sidebar", &mut invoked);
            entry(ui, registry, keymap, "toggle_aside", &mut invoked);
            ui.separator();
            MenuButton::new("Appearance").ui(ui, |ui| {
                entry(ui, registry, keymap, "appearance_system", &mut invoked);
                entry(ui, registry, keymap, "appearance_light", &mut invoked);
                entry(ui, registry, keymap, "appearance_dark", &mut invoked);
            });
            MenuButton::new("Contrast").ui(ui, |ui| {
                entry(ui, registry, keymap, "contrast_standard", &mut invoked);
                entry(ui, registry, keymap, "contrast_high", &mut invoked);
            });
        });
        MenuButton::new("Go").ui(ui, |ui| {
            for destination in Destination::ALL {
                entry(
                    ui,
                    registry,
                    keymap,
                    super::commands::destination_command_id(destination),
                    &mut invoked,
                );
            }
            ui.separator();
            entry(ui, registry, keymap, "search_library", &mut invoked);
            entry(ui, registry, keymap, "focus_main_region", &mut invoked);
        });
        MenuButton::new("Help").ui(ui, |ui| {
            entry(ui, registry, keymap, "command_palette", &mut invoked);
            entry(ui, registry, keymap, "keyboard_shortcuts", &mut invoked);
        });
        // Commands a screen registered that no menu claims are reachable
        // through the palette and their own chords; the menu does not grow a
        // catch-all, which would list them without any ordering.
    });
    invoked
}

fn entry(
    ui: &mut egui::Ui,
    registry: &CommandRegistry,
    keymap: &KeyMap,
    id: &str,
    invoked: &mut Option<String>,
) {
    let Some(spec) = registry.get(id) else {
        return;
    };
    let os = ui.ctx().os();
    let label = match keymap.primary_chord(spec) {
        Some(chord) => format!("{}\t{}", spec.label, display(&chord, os)),
        None => spec.label.to_owned(),
    };
    if ui.button(label).clicked() {
        *invoked = Some(spec.id.to_owned());
        ui.close();
    }
}

/// The commands the menu bar exposes, for the test that asserts the menu and
/// the registry cannot drift apart.
#[must_use]
pub fn menu_command_ids() -> Vec<&'static str> {
    let mut ids = vec!["import_audio", "refresh", "toggle_sidebar", "toggle_aside"];
    ids.extend([
        "appearance_system",
        "appearance_light",
        "appearance_dark",
        "contrast_standard",
        "contrast_high",
    ]);
    ids.extend(
        Destination::ALL
            .into_iter()
            .map(super::commands::destination_command_id),
    );
    ids.extend([
        "search_library",
        "focus_main_region",
        "command_palette",
        "keyboard_shortcuts",
    ]);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::commands::ShellAction;

    #[test]
    fn every_menu_entry_names_a_registered_command() {
        let registry = CommandRegistry::with_builtins();
        for id in menu_command_ids() {
            assert!(registry.get(id).is_some(), "{id} is not registered");
        }
    }

    #[test]
    fn every_shell_command_is_reachable_from_the_menu_or_is_a_key_only_action() {
        let registry = CommandRegistry::with_builtins();
        let menu = menu_command_ids();
        for spec in registry.all() {
            let reachable =
                menu.contains(&spec.id) || matches!(spec.action, ShellAction::CloseOverlay);
            assert!(reachable, "{} is not reachable from the menu", spec.id);
        }
    }
}
