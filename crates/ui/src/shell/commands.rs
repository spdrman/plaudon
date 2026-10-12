//! The central command map.
//!
//! One table of everything the shell can be asked to do, by any route: a
//! toolbar button, a menu item, a keyboard chord, the command palette or a
//! screen. The React shell had three separate lists — a `switch` in
//! `App.tsx`'s key handler, the `SHORTCUTS` inventory shown by the help
//! dialog, and the buttons themselves — and they could disagree. Here the
//! help dialog, the palette and the key handler all read this registry, so
//! they cannot.
//!
//! A command either changes view state (navigate, open the palette, switch
//! appearance) or reaches the application, and when it reaches the
//! application it does so as an [`AppCommand`](plaudon_app::command::AppCommand)
//! through the dispatcher — never by calling a backend.

use std::collections::BTreeMap;

use plaudon_app::dto::{Appearance, Contrast};

use super::route::{Destination, Route};

/// The groups the shortcut help dialog is organised by, as
/// `app/shortcuts.ts` declared them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandGroup {
    /// Moving around the application.
    Navigation,
    /// The library table and its selection.
    Library,
    /// Playback.
    Playback,
    /// The transcript editor.
    Transcript,
    /// Everything else.
    General,
}

impl CommandGroup {
    /// Every group, in the order the help dialog lists them.
    pub const ALL: [Self; 5] = [
        Self::Navigation,
        Self::Library,
        Self::Playback,
        Self::Transcript,
        Self::General,
    ];

    /// The heading shown for this group.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Library => "Library",
            Self::Playback => "Playback",
            Self::Transcript => "Transcript",
            Self::General => "General",
        }
    }
}

/// What running a command does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellAction {
    /// Go to a route.
    Navigate(Route),
    /// Open the command palette.
    OpenPalette,
    /// Open the keyboard reference.
    ShowShortcuts,
    /// Close whatever overlay is open, or cancel the current edit.
    CloseOverlay,
    /// Expand or collapse the navigation sidebar.
    ToggleSidebar,
    /// Show or hide the right-hand pane, when the active screen has one.
    ToggleAside,
    /// Move keyboard focus to the main region (the skip link's target).
    FocusMain,
    /// Switch the appearance preference.
    SetAppearance(Appearance),
    /// Switch the contrast preference.
    SetContrast(Contrast),
    /// Ask the host for audio files and import them
    /// (`AppCommand::ImportFiles`).
    ImportAudio,
    /// Re-read the machine's capabilities (`AppCommand::GetCapabilities`) and
    /// tell the active screen to refetch.
    Refresh,
    /// Hand the command to the active screen, which decides what it means.
    ///
    /// This is how a screen contributes its own entries: it returns them from
    /// [`Screen::commands`](super::screen::Screen::commands) and handles them
    /// in [`Screen::handle_command`](super::screen::Screen::handle_command).
    Screen,
}

/// One command: what it is called, where it is grouped, what it is bound to
/// by default, and what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Stable identifier; also the persistence key for a changed binding.
    pub id: &'static str,
    /// The label shown in menus, the palette and the help dialog.
    pub label: &'static str,
    /// Which help-dialog group it belongs to.
    pub group: CommandGroup,
    /// The chord it is bound to out of the box, in the portable form
    /// `Mod+Shift+K` (see [`super::shortcuts`]).
    pub default_chord: Option<&'static str>,
    /// Whether the user may rebind it. `Esc` is not rebindable: a dialog that
    /// cannot be dismissed is a trap.
    pub rebindable: bool,
    /// What it does.
    pub action: ShellAction,
}

impl CommandSpec {
    /// A command with no default binding.
    #[must_use]
    pub const fn new(
        id: &'static str,
        label: &'static str,
        group: CommandGroup,
        action: ShellAction,
    ) -> Self {
        Self {
            id,
            label,
            group,
            default_chord: None,
            rebindable: true,
            action,
        }
    }

    /// The same command with a default chord.
    #[must_use]
    pub const fn with_chord(mut self, chord: &'static str) -> Self {
        self.default_chord = Some(chord);
        self
    }

    /// Mark the command as fixed, so no editor may rebind it.
    #[must_use]
    pub const fn fixed(mut self) -> Self {
        self.rebindable = false;
        self
    }
}

/// Why a command could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// Another command already uses this identifier.
    #[error("a command with this id is already registered")]
    DuplicateId,
    /// The identifier is empty.
    #[error("a command id may not be empty")]
    EmptyId,
}

/// Every command the running shell knows about.
#[derive(Debug, Clone, Default)]
pub struct CommandRegistry {
    specs: Vec<CommandSpec>,
    by_id: BTreeMap<&'static str, usize>,
}

impl CommandRegistry {
    /// A registry holding the shell's own commands.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        for spec in builtin_commands() {
            registry
                .register(spec)
                .expect("the built-in command table has unique ids");
        }
        registry
    }

    /// Add a command.
    ///
    /// # Errors
    ///
    /// Fails when the identifier is empty or already taken; a screen that
    /// registers twice is a bug, and silently keeping one of the two would
    /// hide it.
    pub fn register(&mut self, spec: CommandSpec) -> Result<(), RegisterError> {
        if spec.id.is_empty() {
            return Err(RegisterError::EmptyId);
        }
        if self.by_id.contains_key(spec.id) {
            return Err(RegisterError::DuplicateId);
        }
        self.by_id.insert(spec.id, self.specs.len());
        self.specs.push(spec);
        Ok(())
    }

    /// Remove every command registered by a screen that is going away.
    pub fn unregister(&mut self, id: &str) -> bool {
        let Some(index) = self.by_id.remove(id) else {
            return false;
        };
        self.specs.remove(index);
        self.by_id = self
            .specs
            .iter()
            .enumerate()
            .map(|(position, spec)| (spec.id, position))
            .collect();
        true
    }

    /// Every command, in registration order.
    #[must_use]
    pub fn all(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// One command by identifier.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CommandSpec> {
        self.by_id.get(id).and_then(|index| self.specs.get(*index))
    }
}

/// The commands the shell itself provides.
///
/// `Mod+1`…`Mod+6` are the destination shortcuts of `app/shortcuts.ts` and
/// their order is the navigation order; everything else is new in O1.3 and is
/// named in the parity matrix (S6, S9, S10).
#[must_use]
pub fn builtin_commands() -> Vec<CommandSpec> {
    let mut commands: Vec<CommandSpec> = Destination::ALL
        .iter()
        .enumerate()
        .map(|(index, destination)| {
            let chord: &'static str = match index {
                0 => "Mod+1",
                1 => "Mod+2",
                2 => "Mod+3",
                3 => "Mod+4",
                4 => "Mod+5",
                _ => "Mod+6",
            };
            CommandSpec::new(
                destination_command_id(*destination),
                destination.label(),
                CommandGroup::Navigation,
                ShellAction::Navigate(destination.route()),
            )
            .with_chord(chord)
        })
        .collect();

    commands.extend([
        CommandSpec::new(
            "search_library",
            "Search recordings",
            CommandGroup::Navigation,
            ShellAction::Navigate(Route::LibrarySearch),
        )
        .with_chord("Mod+Shift+F"),
        CommandSpec::new(
            "command_palette",
            "Command palette",
            CommandGroup::General,
            ShellAction::OpenPalette,
        )
        .with_chord("Mod+K"),
        CommandSpec::new(
            "keyboard_shortcuts",
            "Keyboard shortcuts",
            CommandGroup::General,
            ShellAction::ShowShortcuts,
        )
        .with_chord("?"),
        CommandSpec::new(
            "close_overlay",
            "Close the dialog or cancel the current edit",
            CommandGroup::General,
            ShellAction::CloseOverlay,
        )
        .with_chord("Escape")
        .fixed(),
        CommandSpec::new(
            "import_audio",
            "Import audio…",
            CommandGroup::Library,
            ShellAction::ImportAudio,
        )
        .with_chord("Mod+O"),
        CommandSpec::new(
            "refresh",
            "Refresh",
            CommandGroup::General,
            ShellAction::Refresh,
        )
        .with_chord("Mod+R"),
        CommandSpec::new(
            "toggle_sidebar",
            "Collapse or expand the sidebar",
            CommandGroup::Navigation,
            ShellAction::ToggleSidebar,
        )
        .with_chord("Mod+B"),
        CommandSpec::new(
            "toggle_aside",
            "Show or hide the side pane",
            CommandGroup::Navigation,
            ShellAction::ToggleAside,
        )
        .with_chord("Mod+Shift+B"),
        CommandSpec::new(
            "focus_main_region",
            "Skip to main content",
            CommandGroup::Navigation,
            ShellAction::FocusMain,
        ),
        CommandSpec::new(
            "appearance_system",
            "Appearance: follow the system",
            CommandGroup::General,
            ShellAction::SetAppearance(Appearance::System),
        ),
        CommandSpec::new(
            "appearance_light",
            "Appearance: light",
            CommandGroup::General,
            ShellAction::SetAppearance(Appearance::Light),
        ),
        CommandSpec::new(
            "appearance_dark",
            "Appearance: dark",
            CommandGroup::General,
            ShellAction::SetAppearance(Appearance::Dark),
        ),
        CommandSpec::new(
            "contrast_standard",
            "Contrast: standard",
            CommandGroup::General,
            ShellAction::SetContrast(Contrast::Standard),
        ),
        CommandSpec::new(
            "contrast_high",
            "Contrast: increased",
            CommandGroup::General,
            ShellAction::SetContrast(Contrast::High),
        ),
    ]);
    commands
}

/// The command identifier that opens a destination.
#[must_use]
pub const fn destination_command_id(destination: Destination) -> &'static str {
    match destination {
        Destination::Library => "nav_library",
        Destination::Inbox => "nav_inbox",
        Destination::Jobs => "nav_jobs",
        Destination::Templates => "nav_templates",
        Destination::Settings => "nav_settings",
        Destination::Devices => "nav_devices",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_destination_shortcuts_are_mod_one_to_mod_six_in_nav_order() {
        let registry = CommandRegistry::with_builtins();
        for (index, destination) in Destination::ALL.iter().enumerate() {
            let spec = registry
                .get(destination_command_id(*destination))
                .expect("every destination has a command");
            assert_eq!(
                spec.default_chord,
                Some(format!("Mod+{}", index + 1)).as_deref()
            );
            assert_eq!(spec.action, ShellAction::Navigate(destination.route()));
        }
    }

    #[test]
    fn command_ids_are_unique_and_a_duplicate_is_refused() {
        let mut registry = CommandRegistry::with_builtins();
        let duplicate = CommandSpec::new(
            "command_palette",
            "Another palette",
            CommandGroup::General,
            ShellAction::OpenPalette,
        );
        assert_eq!(
            registry.register(duplicate),
            Err(RegisterError::DuplicateId)
        );
        assert_eq!(
            registry.register(CommandSpec::new(
                "",
                "Nameless",
                CommandGroup::General,
                ShellAction::Screen
            )),
            Err(RegisterError::EmptyId)
        );
    }

    #[test]
    fn a_screen_can_add_and_remove_its_own_commands() {
        let mut registry = CommandRegistry::with_builtins();
        let before = registry.all().len();
        registry
            .register(
                CommandSpec::new(
                    "library_select_all",
                    "Select all loaded rows",
                    CommandGroup::Library,
                    ShellAction::Screen,
                )
                .with_chord("Mod+A"),
            )
            .expect("registers");
        assert_eq!(registry.all().len(), before + 1);
        assert!(registry.get("library_select_all").is_some());
        assert!(registry.unregister("library_select_all"));
        assert_eq!(registry.all().len(), before);
        assert!(registry.get("library_select_all").is_none());
        // The index is rebuilt, so the remaining commands are still findable.
        assert!(registry.get("command_palette").is_some());
    }

    #[test]
    fn escape_is_fixed_so_no_editor_can_make_a_dialog_inescapable() {
        let registry = CommandRegistry::with_builtins();
        let escape = registry.get("close_overlay").expect("exists");
        assert!(!escape.rebindable);
        assert_eq!(escape.default_chord, Some("Escape"));
    }
}
