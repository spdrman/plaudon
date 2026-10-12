//! Native file dialogs behind a port the view layer can fake.
//!
//! The view never calls `rfd` directly: it calls [`FileDialogs`], and the
//! binary decides whether that is [`NativeFileDialogs`] or, in a test, a
//! [`FakeFileDialogs`] that returns fixture paths without opening a window.
//!
//! # Threading
//!
//! [`NativeFileDialogs`] uses `rfd`'s **blocking** API: every method returns
//! only after the user has answered. That is the right shape for the
//! application layer — a picked path is a value, not a callback — but it has
//! two consequences the caller must respect:
//!
//! * **Never call these from the render/update callback.** The frame cannot be
//!   finished while a modal dialog is open, so the window stops painting for
//!   as long as the user is browsing. The shell runs a dialog from the event
//!   loop's main thread *between* frames and feeds the result back in as a
//!   command.
//! * **On macOS, AppKit panels must be shown from the main thread.** That is
//!   the same thread as the one above, so the rule costs nothing in practice,
//!   but a worker thread must not call these methods.
//!
//! Whatever a dialog returns is still untrusted: a path chosen in a picker is
//! validated against the approved roots exactly like a dropped path
//! ([`crate::dragdrop`], spec §7.4).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::associations::ARCHIVE_EXTENSION;
use crate::lock;

/// One named group of extensions offered by a dialog's type filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    /// Group name shown by the OS dialog, e.g. `Audio`.
    pub name: String,
    /// Lowercase extensions without a leading dot.
    pub extensions: Vec<String>,
}

impl FileFilter {
    /// Build a filter from a name and a list of extensions.
    #[must_use]
    pub fn new(name: impl Into<String>, extensions: &[&str]) -> Self {
        Self {
            name: name.into(),
            extensions: extensions
                .iter()
                .map(|extension| extension.to_ascii_lowercase())
                .collect(),
        }
    }
}

/// Filter for the media containers the importer accepts.
///
/// Taken from [`plaudon_app::validate::ALLOWED_MEDIA_EXTENSIONS`] so the dialog
/// cannot drift from what the import command will actually accept.
#[must_use]
pub fn audio_filter() -> FileFilter {
    FileFilter::new("Audio", plaudon_app::validate::ALLOWED_MEDIA_EXTENSIONS)
}

/// Filter for the `.plaudon` portable archive.
#[must_use]
pub fn archive_filter() -> FileFilter {
    FileFilter::new("Plaudon archive", &[ARCHIVE_EXTENSION])
}

/// The OS file dialogs the shell needs.
///
/// Every method is blocking and cancellable: a cancelled dialog is an empty
/// `Vec` or `None`, never an error, because the user cancelling is not a
/// failure.
pub trait FileDialogs: Send + Sync + fmt::Debug {
    /// Pick one or more audio files to import.
    fn pick_audio_files(&self) -> Vec<PathBuf>;

    /// Pick a single existing folder (watched folder, export destination).
    fn pick_folder(&self, title: &str) -> Option<PathBuf>;

    /// Pick a single existing file matching `filters`.
    fn pick_file(&self, title: &str, filters: &[FileFilter]) -> Option<PathBuf>;

    /// Choose a destination to write to, pre-filled with `suggested_name`.
    fn save_file(
        &self,
        title: &str,
        suggested_name: &str,
        filters: &[FileFilter],
    ) -> Option<PathBuf>;
}

/// The real dialogs, drawn by the operating system through `rfd`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeFileDialogs;

impl NativeFileDialogs {
    /// Build the native dialog adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Apply every filter to a dialog builder.
fn with_filters(mut dialog: rfd::FileDialog, filters: &[FileFilter]) -> rfd::FileDialog {
    for filter in filters {
        dialog = dialog.add_filter(&filter.name, &filter.extensions);
    }
    dialog
}

impl FileDialogs for NativeFileDialogs {
    fn pick_audio_files(&self) -> Vec<PathBuf> {
        with_filters(
            rfd::FileDialog::new().set_title("Import audio"),
            &[audio_filter()],
        )
        .pick_files()
        .unwrap_or_default()
    }

    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }

    fn pick_file(&self, title: &str, filters: &[FileFilter]) -> Option<PathBuf> {
        with_filters(rfd::FileDialog::new().set_title(title), filters).pick_file()
    }

    fn save_file(
        &self,
        title: &str,
        suggested_name: &str,
        filters: &[FileFilter],
    ) -> Option<PathBuf> {
        with_filters(
            rfd::FileDialog::new()
                .set_title(title)
                .set_file_name(suggested_name),
            filters,
        )
        .save_file()
    }
}

/// One dialog invocation, as recorded by [`FakeFileDialogs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogCall {
    /// [`FileDialogs::pick_audio_files`] was called.
    PickAudioFiles,
    /// [`FileDialogs::pick_folder`] was called.
    PickFolder {
        /// Title the caller asked for.
        title: String,
    },
    /// [`FileDialogs::pick_file`] was called.
    PickFile {
        /// Title the caller asked for.
        title: String,
        /// Filters the caller offered.
        filters: Vec<FileFilter>,
    },
    /// [`FileDialogs::save_file`] was called.
    SaveFile {
        /// Title the caller asked for.
        title: String,
        /// Pre-filled file name.
        suggested_name: String,
        /// Filters the caller offered.
        filters: Vec<FileFilter>,
    },
}

/// Scripted dialogs for tests: records every call, answers from a script.
///
/// Default behaviour is "the user cancelled everything", which is the case
/// most easily forgotten in a UI test.
#[derive(Debug, Default)]
pub struct FakeFileDialogs {
    calls: Mutex<Vec<DialogCall>>,
    audio_files: Vec<PathBuf>,
    folder: Option<PathBuf>,
    file: Option<PathBuf>,
    save_target: Option<PathBuf>,
}

impl FakeFileDialogs {
    /// A fake where every dialog is cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the answer to [`FileDialogs::pick_audio_files`].
    #[must_use]
    pub fn with_audio_files(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.audio_files = paths.into_iter().collect();
        self
    }

    /// Script the answer to [`FileDialogs::pick_folder`].
    #[must_use]
    pub fn with_folder(mut self, folder: impl AsRef<Path>) -> Self {
        self.folder = Some(folder.as_ref().to_path_buf());
        self
    }

    /// Script the answer to [`FileDialogs::pick_file`].
    #[must_use]
    pub fn with_file(mut self, file: impl AsRef<Path>) -> Self {
        self.file = Some(file.as_ref().to_path_buf());
        self
    }

    /// Script the answer to [`FileDialogs::save_file`].
    #[must_use]
    pub fn with_save_target(mut self, target: impl AsRef<Path>) -> Self {
        self.save_target = Some(target.as_ref().to_path_buf());
        self
    }

    /// Every call made so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<DialogCall> {
        lock(&self.calls).clone()
    }

    /// How many dialogs have been opened.
    #[must_use]
    pub fn call_count(&self) -> usize {
        lock(&self.calls).len()
    }

    fn record(&self, call: DialogCall) {
        lock(&self.calls).push(call);
    }
}

impl FileDialogs for FakeFileDialogs {
    fn pick_audio_files(&self) -> Vec<PathBuf> {
        self.record(DialogCall::PickAudioFiles);
        self.audio_files.clone()
    }

    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        self.record(DialogCall::PickFolder {
            title: title.to_owned(),
        });
        self.folder.clone()
    }

    fn pick_file(&self, title: &str, filters: &[FileFilter]) -> Option<PathBuf> {
        self.record(DialogCall::PickFile {
            title: title.to_owned(),
            filters: filters.to_vec(),
        });
        self.file.clone()
    }

    fn save_file(
        &self,
        title: &str,
        suggested_name: &str,
        filters: &[FileFilter],
    ) -> Option<PathBuf> {
        self.record(DialogCall::SaveFile {
            title: title.to_owned(),
            suggested_name: suggested_name.to_owned(),
            filters: filters.to_vec(),
        });
        self.save_target.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audio_filter_matches_the_import_allowlist() {
        let filter = audio_filter();
        let expected: Vec<String> = plaudon_app::validate::ALLOWED_MEDIA_EXTENSIONS
            .iter()
            .map(|extension| (*extension).to_owned())
            .collect();
        assert_eq!(filter.extensions, expected);
        assert!(filter.extensions.iter().all(|e| !e.starts_with('.')));
    }

    #[test]
    fn the_archive_filter_offers_the_portable_archive_extension() {
        assert_eq!(archive_filter().extensions, vec!["plaudon".to_owned()]);
    }

    #[test]
    fn a_filter_lowercases_its_extensions() {
        let filter = FileFilter::new("Audio", &["WAV", "Mp3"]);
        assert_eq!(filter.extensions, vec!["wav".to_owned(), "mp3".to_owned()]);
    }

    #[test]
    fn the_fake_records_every_call_in_order() {
        let dialogs = FakeFileDialogs::new()
            .with_audio_files([PathBuf::from("/tmp/a.wav")])
            .with_folder("/tmp/watched")
            .with_file("/tmp/backup.plaudon")
            .with_save_target("/tmp/export.md");

        assert_eq!(
            dialogs.pick_audio_files(),
            vec![PathBuf::from("/tmp/a.wav")]
        );
        assert_eq!(
            dialogs.pick_folder("Watch a folder"),
            Some(PathBuf::from("/tmp/watched"))
        );
        assert_eq!(
            dialogs.pick_file("Restore", &[archive_filter()]),
            Some(PathBuf::from("/tmp/backup.plaudon"))
        );
        assert_eq!(
            dialogs.save_file("Export", "export.md", &[]),
            Some(PathBuf::from("/tmp/export.md"))
        );

        assert_eq!(dialogs.call_count(), 4);
        assert_eq!(
            dialogs.calls(),
            vec![
                DialogCall::PickAudioFiles,
                DialogCall::PickFolder {
                    title: "Watch a folder".to_owned()
                },
                DialogCall::PickFile {
                    title: "Restore".to_owned(),
                    filters: vec![archive_filter()],
                },
                DialogCall::SaveFile {
                    title: "Export".to_owned(),
                    suggested_name: "export.md".to_owned(),
                    filters: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn an_unscripted_fake_reports_a_cancelled_dialog() {
        let dialogs = FakeFileDialogs::new();
        assert!(
            dialogs.pick_audio_files().is_empty(),
            "a cancelled dialog yields no paths"
        );
        assert!(dialogs.pick_folder("Watch a folder").is_none());
        assert!(dialogs.pick_file("Restore", &[]).is_none());
        assert!(dialogs.save_file("Export", "export.md", &[]).is_none());
        assert_eq!(dialogs.call_count(), 4);
    }

    #[test]
    fn the_port_is_usable_as_a_trait_object() {
        let dialogs: std::sync::Arc<dyn FileDialogs> =
            std::sync::Arc::new(FakeFileDialogs::new().with_folder("/tmp"));
        assert_eq!(dialogs.pick_folder("pick"), Some(PathBuf::from("/tmp")));
        let _: &dyn FileDialogs = &NativeFileDialogs::new();
    }
}
