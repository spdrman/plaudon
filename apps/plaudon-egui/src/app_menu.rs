//! The macOS application menu (parity row S8).
//!
//! macOS puts an application's menu in the system menu bar, and an
//! application without one looks broken: no About, no Services, no Hide, and
//! Cmd+Q belonging to nobody. This module builds that menu — but it does not
//! *decide* it. The structure comes from
//! [`plaudon_ui::shell::menu::menu_model`], the same description the
//! in-window menu bar draws, so a row cannot exist in one menu and be missing
//! from the other. Labels and accelerators come from the live
//! [`CommandRegistry`] and [`KeyMap`], so a rebound chord moves here too
//! instead of being retyped.
//!
//! **How a click gets back to the shell.** Every custom row carries the shell
//! command identifier as its `muda` id. `muda` posts the activation to its
//! process-global channel, [`crate::menu_events`] routes it here, and
//! [`drain`] hands the identifiers to the binary, which runs each one through
//! `Shell::run_command`. That is the same function the in-window menu item,
//! the palette and the keyboard chord end in, so the native menu is a second
//! door into one room rather than a second implementation.
//!
//! **What is left to the platform.** About, Services, Hide, Hide Others, Show
//! All and Quit are `muda` predefined items: the platform implements them, so
//! they are not shell commands and do not appear in the registry. Quit is
//! AppKit's `terminate:`, which posts `applicationWillTerminate:`; winit's
//! delegate turns that into its exit handling and eframe calls
//! [`eframe::App::on_exit`], so [`crate::shell_app::PlaudonShell::shutdown`]
//! still runs — screens get their last turn, the shell state is flushed and
//! the engine gets its shutdown grace. There is deliberately **no** Edit
//! menu: its predefined rows send AppKit selectors (`copy:`, `paste:`) that an
//! egui window does not implement, and a menu row that does nothing is worse
//! than no row. Editing chords are egui's own and are listed in the keyboard
//! reference.
//!
//! **Accelerators.** A chord is rendered with
//! [`plaudon_ui::shell::shortcuts::display`] for macOS (`Mod` becomes `Cmd`)
//! and then parsed by `muda`. A chord `muda` cannot express as a physical
//! accelerator — `?`, which is a shifted key whose position depends on the
//! layout — is left off the native row rather than guessed at; the command
//! keeps working and the chord is still shown in the in-window menu, the
//! palette and the keyboard reference.
//!
//! **The in-window menu bar.** [`install`] hides it, through
//! `Shell::set_in_window_menu`, only after every row was accepted and the
//! menu was installed on the application. Nothing is hidden by `cfg`: a test
//! build installs no native menu and therefore keeps the in-window bar, which
//! is what keeps the ADR-0023 evidence — every command reachable through the
//! AccessKit tree a kittest harness drives — true on macOS as well.

use std::cell::RefCell;
use std::str::FromStr;

use muda::accelerator::Accelerator;
use muda::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use plaudon_ui::shell::commands::CommandRegistry;
use plaudon_ui::shell::menu::{menu_model, MenuEntry, MenuSection};
use plaudon_ui::shell::shortcuts::{display, KeyMap};

use crate::shell_app::PlaudonShell;

/// One row of the planned menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedEntry {
    /// A row that runs a shell command.
    Item(PlannedItem),
    /// A rule between two groups of rows.
    Separator,
    /// A nested menu.
    Submenu(PlannedMenu),
}

/// A row of the planned menu, with everything the platform needs to draw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedItem {
    /// The shell command identifier, which is also the `muda` id.
    pub id: String,
    /// The label, from the command registry.
    pub label: String,
    /// The accelerator as `muda` spells it, when the chord can be expressed
    /// as one.
    pub accelerator: Option<String>,
}

/// A planned menu: a title and its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMenu {
    /// What the menu is called.
    pub title: String,
    /// Its rows, in order.
    pub entries: Vec<PlannedEntry>,
}

thread_local! {
    /// The installed menu, kept for the life of the process.
    ///
    /// `muda`'s `Menu` owns the AppKit objects that carry each row's target
    /// and action; dropping it would leave the menu bar holding items that no
    /// longer report their clicks. It is not `Send`, and the application menu
    /// may only be touched from the main thread, so it lives in a
    /// thread-local on exactly the thread that installed it.
    static APP_MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

/// Turn the shared menu description into rows the platform can draw.
///
/// Pure: no AppKit object is created, which is what lets the tests below
/// assert the identifiers and accelerators without a window.
#[must_use]
pub fn plan(registry: &CommandRegistry, keymap: &KeyMap) -> Vec<PlannedMenu> {
    menu_model()
        .iter()
        .map(|section| plan_section(section, registry, keymap))
        .collect()
}

fn plan_section(section: &MenuSection, registry: &CommandRegistry, keymap: &KeyMap) -> PlannedMenu {
    let mut entries = Vec::with_capacity(section.entries.len());
    for entry in &section.entries {
        match entry {
            MenuEntry::Command(id) => {
                // A command the registry does not have is not drawn, exactly
                // as the in-window menu skips it; the drift test in
                // `plaudon_ui::shell::menu` is what keeps that from happening
                // quietly.
                if let Some(spec) = registry.get(id) {
                    entries.push(PlannedEntry::Item(PlannedItem {
                        id: spec.id.to_owned(),
                        label: spec.label.to_owned(),
                        accelerator: keymap
                            .primary_chord(spec)
                            .and_then(|chord| accelerator_text(&chord)),
                    }));
                }
            }
            MenuEntry::Separator => entries.push(PlannedEntry::Separator),
            MenuEntry::Submenu(nested) => {
                entries.push(PlannedEntry::Submenu(plan_section(
                    nested, registry, keymap,
                )));
            }
        }
    }
    PlannedMenu {
        title: section.title.to_owned(),
        entries,
    }
}

/// The macOS spelling of a chord, when `muda` can parse it as an accelerator.
fn accelerator_text(chord: &str) -> Option<String> {
    let text = display(chord, egui::os::OperatingSystem::Mac);
    Accelerator::from_str(&text).ok().map(|_| text)
}

/// Every identifier the planned menu can report, so the event router knows
/// which activations are ours.
fn planned_ids(menus: &[PlannedMenu]) -> Vec<String> {
    let mut ids = Vec::new();
    for menu in menus {
        collect_ids(&menu.entries, &mut ids);
    }
    ids
}

fn collect_ids(entries: &[PlannedEntry], ids: &mut Vec<String>) {
    for entry in entries {
        match entry {
            PlannedEntry::Item(item) => ids.push(item.id.clone()),
            PlannedEntry::Separator => {}
            PlannedEntry::Submenu(nested) => collect_ids(&nested.entries, ids),
        }
    }
}

/// Build and install the application menu, and hide the in-window one.
///
/// Must be called on the main thread, after the event loop has started —
/// the same requirement the status item has, and the reason both are built
/// inside eframe's creation callback.
///
/// A failure is reported and otherwise survivable: the in-window menu bar
/// stays, so the application is never left with no menu at all.
pub fn install(app: &mut PlaudonShell) {
    let plan = plan(app.shell().commands(), app.shell().keymap());
    let menu = match build(&plan) {
        Ok(menu) => menu,
        Err(reason) => {
            tracing::warn!(%reason, "the application menu could not be built");
            return;
        }
    };
    menu.init_for_nsapp();
    crate::menu_events::claim(planned_ids(&plan));
    APP_MENU.with(|cell| cell.replace(Some(menu)));
    app.shell_mut().set_in_window_menu(false);
}

/// The command identifiers the application menu asked for since the last
/// call.
///
/// Safe before [`install`] and on a build where installation failed: it
/// returns nothing, because nothing claimed anything.
#[must_use]
pub fn drain() -> Vec<String> {
    crate::menu_events::drain_app_menu()
}

/// Assemble the `muda` menu. Creates AppKit objects, so it only runs on the
/// main thread of a running application.
fn build(plan: &[PlannedMenu]) -> Result<Menu, String> {
    let menu = Menu::new();
    menu.append(&application_menu()?)
        .map_err(|error| appended(&error))?;
    for planned in plan {
        let submenu = Submenu::new(&planned.title, true);
        fill(&submenu, &planned.entries)?;
        menu.append(&submenu).map_err(|error| appended(&error))?;
    }
    Ok(menu)
}

/// The one way a menu row can fail to be added, in words a log can carry.
fn appended(error: &muda::Error) -> String {
    format!("a menu row could not be appended: {error}")
}

/// The leftmost menu, which macOS titles with the application's own name.
///
/// Every row here is implemented by the platform; none of them reaches the
/// shell.
fn application_menu() -> Result<Submenu, String> {
    let about = PredefinedMenuItem::about(
        Some("About Plaudon"),
        Some(AboutMetadata {
            name: Some("Plaudon".to_owned()),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            ..AboutMetadata::default()
        }),
    );
    let services = PredefinedMenuItem::services(None);
    let hide = PredefinedMenuItem::hide(None);
    let hide_others = PredefinedMenuItem::hide_others(None);
    let show_all = PredefinedMenuItem::show_all(None);
    let quit = PredefinedMenuItem::quit(None);

    Submenu::with_items(
        "Plaudon",
        true,
        &[
            &about,
            &PredefinedMenuItem::separator(),
            &services,
            &PredefinedMenuItem::separator(),
            &hide,
            &hide_others,
            &show_all,
            &PredefinedMenuItem::separator(),
            &quit,
        ],
    )
    .map_err(|error| format!("the application menu could not be assembled: {error}"))
}

/// Add the planned rows to a submenu, nesting as the description nests.
fn fill(submenu: &Submenu, entries: &[PlannedEntry]) -> Result<(), String> {
    for entry in entries {
        match entry {
            PlannedEntry::Item(item) => {
                let accelerator = item
                    .accelerator
                    .as_deref()
                    .and_then(|text| Accelerator::from_str(text).ok());
                let row = MenuItem::with_id(item.id.as_str(), &item.label, true, accelerator);
                submenu.append(&row).map_err(|error| appended(&error))?;
            }
            PlannedEntry::Separator => submenu
                .append(&PredefinedMenuItem::separator())
                .map_err(|error| appended(&error))?,
            PlannedEntry::Submenu(nested) => {
                let child = Submenu::new(&nested.title, true);
                fill(&child, &nested.entries)?;
                submenu.append(&child).map_err(|error| appended(&error))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_ui::shell::menu::menu_command_ids;
    use std::collections::{BTreeMap, BTreeSet};

    // Only the planning half is exercised here. Building the menu creates
    // AppKit objects, which may only be touched on the main thread of a
    // running application; a `cargo test` harness thread has neither, so
    // `build`, `install` and `init_for_nsapp` are covered by running the
    // application, not by a unit test.

    fn planned() -> Vec<PlannedMenu> {
        plan(&CommandRegistry::with_builtins(), &KeyMap::new())
    }

    #[test]
    fn the_native_menu_lists_exactly_the_shared_description() {
        let ids = planned_ids(&planned());
        assert_eq!(ids, menu_command_ids());
    }

    #[test]
    fn no_identifier_appears_twice() {
        let ids = planned_ids(&planned());
        let unique: BTreeSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn the_titles_and_nesting_follow_the_shared_description() {
        let plan = planned();
        let titles: Vec<&str> = plan.iter().map(|menu| menu.title.as_str()).collect();
        assert_eq!(titles, ["File", "View", "Go", "Help"]);

        let nested: Vec<&str> = plan[1]
            .entries
            .iter()
            .filter_map(|entry| match entry {
                PlannedEntry::Submenu(menu) => Some(menu.title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(nested, ["Appearance", "Contrast"]);

        assert!(plan[0]
            .entries
            .iter()
            .any(|entry| matches!(entry, PlannedEntry::Separator)));
    }

    /// The planned row for `id`, wherever it sits, submenus included.
    fn row(plan: &[PlannedMenu], id: &str) -> PlannedItem {
        fn find(entries: &[PlannedEntry], id: &str) -> Option<PlannedItem> {
            for entry in entries {
                match entry {
                    PlannedEntry::Item(item) if item.id == id => return Some(item.clone()),
                    PlannedEntry::Submenu(nested) => {
                        if let Some(found) = find(&nested.entries, id) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        plan.iter()
            .find_map(|menu| find(&menu.entries, id))
            .unwrap_or_else(|| panic!("{id} is not in the planned menu"))
    }

    fn accelerator_of(plan: &[PlannedMenu], id: &str) -> Option<String> {
        row(plan, id).accelerator
    }

    #[test]
    fn accelerators_are_the_key_maps_own_chords_in_the_platforms_spelling() {
        let plan = planned();
        assert_eq!(
            accelerator_of(&plan, "command_palette"),
            Some("Cmd+K".to_owned())
        );
        assert_eq!(
            accelerator_of(&plan, "search_library"),
            Some("Cmd+Shift+F".to_owned())
        );
        assert_eq!(
            accelerator_of(&plan, "nav_library"),
            Some("Cmd+1".to_owned())
        );
        assert_eq!(
            accelerator_of(&plan, "import_audio"),
            Some("Cmd+O".to_owned())
        );
    }

    #[test]
    fn a_command_without_a_chord_carries_no_accelerator() {
        let plan = planned();
        assert_eq!(accelerator_of(&plan, "focus_main_region"), None);
    }

    #[test]
    fn a_chord_the_platform_cannot_express_is_left_off_rather_than_guessed() {
        // `?` is a shifted key whose physical position depends on the layout,
        // and `muda` accelerators are physical codes. The row is still there
        // and still runs the command.
        let plan = planned();
        assert_eq!(accelerator_of(&plan, "keyboard_shortcuts"), None);
        let ids = planned_ids(&plan);
        assert!(ids.iter().any(|id| id == "keyboard_shortcuts"));
    }

    #[test]
    fn every_accelerator_the_plan_names_parses_as_a_muda_accelerator() {
        fn check(entries: &[PlannedEntry]) {
            for entry in entries {
                match entry {
                    PlannedEntry::Item(item) => {
                        if let Some(text) = &item.accelerator {
                            assert!(
                                Accelerator::from_str(text).is_ok(),
                                "{text} would be dropped by the platform"
                            );
                        }
                    }
                    PlannedEntry::Submenu(nested) => check(&nested.entries),
                    PlannedEntry::Separator => {}
                }
            }
        }
        for menu in planned() {
            check(&menu.entries);
        }
    }

    #[test]
    fn a_rebound_command_moves_the_native_accelerator_too() {
        let registry = CommandRegistry::with_builtins();
        let overrides =
            BTreeMap::from([("command_palette".to_owned(), vec!["Mod+Shift+P".to_owned()])]);
        let keymap = KeyMap::from_overrides(&overrides);
        let plan = plan(&registry, &keymap);
        assert_eq!(
            accelerator_of(&plan, "command_palette"),
            Some("Cmd+Shift+P".to_owned())
        );
    }

    #[test]
    fn labels_are_the_registrys_own() {
        let registry = CommandRegistry::with_builtins();
        let plan = plan(&registry, &KeyMap::new());
        for id in ["import_audio", "appearance_dark", "nav_settings"] {
            let spec = registry.get(id).expect("a built-in command");
            assert_eq!(row(&plan, id).label, spec.label);
        }
    }
}
