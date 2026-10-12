//! The menu bar (parity row S8).
//!
//! **One description, two menus.** [`menu_model`] is the only statement of
//! what the menu contains: the titles, the order of the rows, where the
//! separators fall, and which command identifier each row runs. Two renderers
//! consume it and neither owns it — [`menu_bar`] draws it in-window with
//! egui, and `plaudon_egui::app_menu` turns the same description into a real
//! `NSMenu` on macOS through `muda`. A row therefore cannot exist in one menu
//! and be missing from the other, and the drift tests at the bottom of this
//! file hold for both.
//!
//! **Why there is a native menu now.** An earlier revision of this file
//! refused one, on two grounds: that `muda` would be a new external
//! dependency needing its own licence-register row and `cargo deny` review,
//! and that a platform menu renders nothing a kittest harness can see and
//! carries no AccessKit tree, so the keyboard-reachability and screen-reader
//! evidence ADR-0023 requires would stop at the menu. The first ground is
//! stale: `muda` 0.20 is already in the dependency graph and already carries
//! a register row, because the status item (`tray-icon` 0.25) links it on
//! macOS and Windows. The second is answered by construction rather than by
//! doing without — the description below, not the native menu, is the source
//! of truth; every row it names is a command in the registry; and the
//! registry stays reachable from the palette, the keyboard map and the
//! in-window bar the harness drives. The ADR-0023 note under `docs/adr/`
//! records the reversal and the evidence it rests on.
//!
//! **The in-window bar on macOS.** It is drawn unless the host reports that
//! it installed a native menu ([`Shell::set_in_window_menu`]). Only the macOS
//! binary reports that, and only after `muda` accepted every row and
//! `init_for_nsapp` ran; no `cfg` decides it and no target is special-cased
//! here. A test build consequently always has the in-window bar — kittest
//! installs no native menu — which is what keeps the ADR-0023 evidence (every
//! command reachable through the AccessKit tree) true on every platform,
//! macOS included. It also means a macOS build whose native menu failed to
//! install keeps a working menu instead of none.
//!
//! Entries are labelled with the platform's own modifier (Cmd on macOS, Ctrl
//! elsewhere) through [`super::shortcuts::display`]; the native menu parses
//! its accelerators out of that same string, so a rebinding moves both.
//!
//! [`Shell::set_in_window_menu`]: super::Shell::set_in_window_menu

use std::sync::LazyLock;

use egui::containers::menu::{MenuBar, MenuButton};

use super::commands::CommandRegistry;
use super::route::Destination;
use super::shortcuts::{display, KeyMap};

/// One row of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A row that runs the command with this identifier.
    Command(&'static str),
    /// A rule between two groups of rows.
    Separator,
    /// A nested menu.
    Submenu(MenuSection),
}

/// A menu: a title and the rows under it, in the order they are shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSection {
    /// What the menu is called.
    pub title: &'static str,
    /// Its rows, in order.
    pub entries: Vec<MenuEntry>,
}

impl MenuSection {
    /// A menu with these rows.
    #[must_use]
    pub const fn new(title: &'static str, entries: Vec<MenuEntry>) -> Self {
        Self { title, entries }
    }
}

/// The menu, as a description rather than as drawing code.
///
/// Built once: both renderers walk it, the in-window one on every frame.
///
/// Commands a screen registered that no menu claims stay reachable through
/// the palette and their own chords. The menu does not grow a catch-all,
/// which would list them in whatever order they happened to register.
#[must_use]
pub fn menu_model() -> &'static [MenuSection] {
    static MODEL: LazyLock<Vec<MenuSection>> = LazyLock::new(build_model);
    &MODEL
}

fn build_model() -> Vec<MenuSection> {
    let mut go: Vec<MenuEntry> = Destination::ALL
        .into_iter()
        .map(|destination| MenuEntry::Command(super::commands::destination_command_id(destination)))
        .collect();
    go.extend([
        MenuEntry::Separator,
        MenuEntry::Command("search_library"),
        MenuEntry::Command("focus_main_region"),
    ]);

    vec![
        MenuSection::new(
            "File",
            vec![
                MenuEntry::Command("import_audio"),
                MenuEntry::Separator,
                MenuEntry::Command("refresh"),
            ],
        ),
        MenuSection::new(
            "View",
            vec![
                MenuEntry::Command("toggle_sidebar"),
                MenuEntry::Command("toggle_aside"),
                MenuEntry::Separator,
                MenuEntry::Submenu(MenuSection::new(
                    "Appearance",
                    vec![
                        MenuEntry::Command("appearance_system"),
                        MenuEntry::Command("appearance_light"),
                        MenuEntry::Command("appearance_dark"),
                    ],
                )),
                MenuEntry::Submenu(MenuSection::new(
                    "Contrast",
                    vec![
                        MenuEntry::Command("contrast_standard"),
                        MenuEntry::Command("contrast_high"),
                    ],
                )),
            ],
        ),
        MenuSection::new("Go", go),
        MenuSection::new(
            "Help",
            vec![
                MenuEntry::Command("command_palette"),
                MenuEntry::Command("keyboard_shortcuts"),
            ],
        ),
    ]
}

/// Draw the menu bar and report the command the user chose.
pub fn menu_bar(ui: &mut egui::Ui, registry: &CommandRegistry, keymap: &KeyMap) -> Option<String> {
    let mut invoked: Option<String> = None;
    MenuBar::new().ui(ui, |ui| {
        for section in menu_model() {
            MenuButton::new(section.title).ui(ui, |ui| {
                section_ui(ui, registry, keymap, section, &mut invoked);
            });
        }
    });
    invoked
}

fn section_ui(
    ui: &mut egui::Ui,
    registry: &CommandRegistry,
    keymap: &KeyMap,
    section: &MenuSection,
    invoked: &mut Option<String>,
) {
    for item in &section.entries {
        match item {
            MenuEntry::Command(id) => entry(ui, registry, keymap, id, invoked),
            MenuEntry::Separator => {
                ui.separator();
            }
            MenuEntry::Submenu(nested) => {
                MenuButton::new(nested.title).ui(ui, |ui| {
                    section_ui(ui, registry, keymap, nested, invoked);
                });
            }
        }
    }
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

/// The commands the menu exposes, in menu order, submenus included.
///
/// Read off [`menu_model`] rather than listed again, so the test that asserts
/// the menu and the registry cannot drift apart is asserting about the
/// description both menus are built from.
#[must_use]
pub fn menu_command_ids() -> Vec<&'static str> {
    let mut ids = Vec::new();
    for section in menu_model() {
        collect_ids(section, &mut ids);
    }
    ids
}

fn collect_ids(section: &'static MenuSection, ids: &mut Vec<&'static str>) {
    for item in &section.entries {
        match item {
            MenuEntry::Command(id) => ids.push(id),
            MenuEntry::Separator => {}
            MenuEntry::Submenu(nested) => collect_ids(nested, ids),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::commands::ShellAction;
    use std::collections::BTreeSet;

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

    #[test]
    fn no_command_appears_twice_in_the_menu() {
        let ids = menu_command_ids();
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "a command listed twice would run twice from two routes"
        );
    }

    #[test]
    fn the_description_is_the_menu_the_shell_has_always_shown() {
        let titles: Vec<&str> = menu_model().iter().map(|section| section.title).collect();
        assert_eq!(titles, ["File", "View", "Go", "Help"]);

        let file = &menu_model()[0];
        assert_eq!(
            file.entries,
            vec![
                MenuEntry::Command("import_audio"),
                MenuEntry::Separator,
                MenuEntry::Command("refresh"),
            ]
        );

        let view = &menu_model()[1];
        let submenus: Vec<&str> = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                MenuEntry::Submenu(nested) => Some(nested.title),
                _ => None,
            })
            .collect();
        assert_eq!(submenus, ["Appearance", "Contrast"]);

        let go = &menu_model()[2];
        let first_destination = super::super::commands::destination_command_id(Destination::ALL[0]);
        assert_eq!(go.entries[0], MenuEntry::Command(first_destination));
        assert!(go.entries.contains(&MenuEntry::Command("search_library")));
    }

    #[test]
    fn no_menu_is_empty_and_no_menu_opens_or_closes_on_a_rule() {
        for section in menu_model() {
            check_section(section);
        }
    }

    fn check_section(section: &MenuSection) {
        assert!(
            !section.entries.is_empty(),
            "{} would open an empty menu",
            section.title
        );
        assert_ne!(
            section.entries.first(),
            Some(&MenuEntry::Separator),
            "{} starts with a rule",
            section.title
        );
        assert_ne!(
            section.entries.last(),
            Some(&MenuEntry::Separator),
            "{} ends with a rule",
            section.title
        );
        for entry in &section.entries {
            if let MenuEntry::Submenu(nested) = entry {
                check_section(nested);
            }
        }
    }

    #[test]
    fn the_model_is_built_once_and_shared() {
        assert!(std::ptr::eq(menu_model(), menu_model()));
    }
}
