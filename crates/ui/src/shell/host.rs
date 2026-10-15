//! Host hooks: the platform capabilities the view layer may not reach for
//! itself.
//!
//! `plaudon-ui` must not depend on `rfd`, the clipboard or the operating
//! system (`docs/EGUI_MIGRATION_PLAN.md` §5.2) — that is what keeps the
//! kittest harness hermetic and the view testable. Instead the binary
//! supplies closures, which a test replaces with ones that answer fixed
//! values and record that they were called. The pattern is the one all three
//! reference applications use.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_app::shell_state::ShellState;

/// Picks audio files to import; empty when the user cancels.
pub type PickAudioFiles = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>;

/// Picks a directory; `None` when the user cancels.
pub type PickFolder = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

/// Picks a destination path for a suggested file name; `None` when the user
/// cancels.
pub type PickSavePath = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// Picks an existing file to open, given the dialog's title; `None` when the
/// user cancels.
pub type PickOpenFile = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// Persists the shell's presentation state, off the render thread.
pub type SaveShellState = Arc<dyn Fn(ShellState) + Send + Sync>;

/// Shows a path selected in the OS file manager (parity row C10).
pub type RevealInFolder = Arc<dyn Fn(&Path) + Send + Sync>;

/// What the shell may ask the host to do.
#[derive(Clone)]
pub struct HostHooks {
    /// Ask the user for audio files to import. Empty when they cancel.
    pub pick_audio_files: PickAudioFiles,
    /// Ask the user for a directory. `None` when they cancel.
    pub pick_folder: PickFolder,
    /// Ask the user where to write a file, given a suggested name. `None`
    /// when they cancel.
    pub pick_save_path: PickSavePath,
    /// Ask the user for an existing file to open, given the dialog's title.
    /// `None` when they cancel.
    pub pick_open_file: PickOpenFile,
    /// Persist the shell's presentation state. The binary writes it off the
    /// render thread; the view never opens a file.
    pub save_shell_state: SaveShellState,
    /// Show a path selected in the OS file manager. The view never spawns a
    /// process; the binary does it through `plaudon_platform::opener`.
    pub reveal_in_folder: RevealInFolder,
}

impl fmt::Debug for HostHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostHooks")
    }
}

impl Default for HostHooks {
    fn default() -> Self {
        Self::none()
    }
}

impl HostHooks {
    /// Hooks that answer nothing and persist nothing.
    ///
    /// The honest default for a headless run: a cancelled dialog, not a
    /// pretend path.
    #[must_use]
    pub fn none() -> Self {
        Self {
            pick_audio_files: Arc::new(Vec::new),
            pick_folder: Arc::new(|| None),
            pick_save_path: Arc::new(|_| None),
            pick_open_file: Arc::new(|_| None),
            save_shell_state: Arc::new(|_| {}),
            reveal_in_folder: Arc::new(|_| {}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_hooks_cancel_rather_than_invent_a_path() {
        let hooks = HostHooks::none();
        assert_eq!((hooks.pick_audio_files)(), Vec::<PathBuf>::new());
        assert!((hooks.pick_folder)().is_none());
        assert!((hooks.pick_save_path)("notes.md").is_none());
    }
}
