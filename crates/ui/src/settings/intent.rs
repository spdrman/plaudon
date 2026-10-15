//! What a settings section asks the host to do.
//!
//! A section never dispatches a command, never opens a dialog and never
//! touches the clipboard itself: it appends an intent, the host drains them
//! after the frame. That is what keeps the kittest suite hermetic — a test
//! renders a section, reads the intents it produced and asserts on them,
//! without a Tokio runtime, a database or a file picker.

use plaudon_app::command::AppCommand;
use plaudon_app::os_permissions::OsPermissionKind;

/// What a folder the user is about to pick is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderPurpose {
    /// Destination of a portable backup.
    BackupDestination,
    /// Default destination for transcript exports.
    ExportDestination,
    /// Root of an offline model pack.
    ModelPack,
    /// A folder to watch for new recordings.
    WatchedFolder,
}

impl FolderPurpose {
    /// Title of the folder dialog.
    #[must_use]
    pub const fn dialog_title(self) -> &'static str {
        match self {
            Self::BackupDestination => "Choose backup destination",
            Self::ExportDestination => "Choose the default export folder",
            Self::ModelPack => "Choose a model pack folder",
            Self::WatchedFolder => "Choose a folder to watch",
        }
    }
}

/// What a file the user is about to pick is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePurpose {
    /// A model weights file to import (issue #303).
    ModelFile,
    /// A portable archive to inspect and restore.
    RestoreArchive,
}

impl FilePurpose {
    /// Title of the open dialog.
    #[must_use]
    pub const fn dialog_title(self) -> &'static str {
        match self {
            Self::ModelFile => "Choose a model file to import",
            Self::RestoreArchive => "Choose archive to restore",
        }
    }

    /// Extensions the dialog filters on.
    #[must_use]
    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::ModelFile => &["bin"],
            Self::RestoreArchive => &["plaudon"],
        }
    }
}

/// A destination the settings screen can send the user to.
///
/// The shell owns routing; the settings screen only names where it wants to
/// go, so the two can be developed and tested apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsDestination {
    /// The PLAUD connector and watched-folder panel.
    Sources,
    /// The activity history.
    Activity,
    /// The jobs list.
    Jobs,
    /// The people roster (VP-090, VP-091). Reachable whether or not voice
    /// matching is on: the list is names, not voiceprints.
    People,
}

/// One thing a section wants the host to do.
#[derive(Debug)]
pub enum SettingsIntent {
    /// Hand a validated command to the dispatcher.
    Command(AppCommand),
    /// Open the native folder picker; the chosen path comes back through
    /// [`crate::settings::SettingsScreen::folder_picked`].
    PickFolder(FolderPurpose),
    /// Open the native open dialog.
    PickFile(FilePurpose),
    /// Navigate elsewhere in the shell.
    Navigate(SettingsDestination),
    /// Show `path` in the OS file manager.
    RevealInFolder(String),
    /// Put `text` on the clipboard. Only ever non-secret facts: a digest, a
    /// path the user can already see, a version string.
    CopyText(String),
    /// Re-read `get_capabilities`, because something changed that it reports.
    RefreshCapabilities,
    /// Read the live status of every operating-system privacy permission,
    /// without prompting (#307).
    CheckOsPermissions,
    /// Ask the operating system to show its prompt for one permission. Only
    /// ever raised by a click on "Ask macOS".
    AskOsPermission(OsPermissionKind),
    /// Open the System Settings pane where one permission is granted.
    OpenOsPermissionSettings(OsPermissionKind),
}

impl SettingsIntent {
    /// The command name, when the intent is a command. For assertions.
    #[must_use]
    pub const fn command_name(&self) -> Option<&'static str> {
        match self {
            Self::Command(command) => Some(command.name()),
            _ => None,
        }
    }
}

/// Everything a frame of the settings screen asked for.
#[derive(Debug, Default)]
pub struct SettingsOutput {
    /// Intents in the order they were raised.
    pub intents: Vec<SettingsIntent>,
}

impl SettingsOutput {
    /// An empty output.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise one intent.
    pub fn push(&mut self, intent: SettingsIntent) {
        self.intents.push(intent);
    }

    /// Raise a command.
    pub fn command(&mut self, command: AppCommand) {
        self.push(SettingsIntent::Command(command));
    }

    /// Whether anything was raised.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }

    /// Every command name raised this frame, for assertions.
    #[must_use]
    pub fn command_names(&self) -> Vec<&'static str> {
        self.intents
            .iter()
            .filter_map(SettingsIntent::command_name)
            .collect()
    }

    /// Take everything raised, leaving the output empty.
    #[must_use]
    pub fn drain(&mut self) -> Vec<SettingsIntent> {
        std::mem::take(&mut self.intents)
    }
}
