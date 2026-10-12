//! Where an export is allowed to land, and how it gets there (spec 16.5).
//!
//! The rules live in `plaudon-security`: [`sanitize_filename`] produces the
//! on-disk name, [`canonicalize_and_validate`] resolves the target and refuses
//! a link that leaves the chosen folder. This module only composes them for
//! the export case and performs the atomic write, so there is exactly one
//! implementation of "is this path safe" in the workspace.
//!
//! Writes are atomic: bytes land in a private temporary file beside the
//! target, are flushed and fsynced, and are then renamed over the target. An
//! interrupted export leaves either the previous file or nothing, never a
//! half-written export.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use plaudon_security::fsguard::{
    canonicalize_and_validate, sanitize_filename, PathPolicy, ValidatedPath,
};
use uuid::Uuid;

use crate::error::{ExportError, ExportResult};

/// Build a file name from an untrusted title and a trusted extension.
///
/// The title contributes a sanitized stem only. Separators, traversal,
/// control and bidi characters, reserved Windows names and trailing dots
/// cannot survive [`sanitize_filename`]; the extension is ours.
#[must_use]
pub fn build_file_name(title: &str, discriminator: &str, extension: &str) -> String {
    let sanitized = sanitize_filename(title);
    let mut stem = sanitized.on_disk().trim().to_owned();
    // Drop any extension the title carried: the real one is appended below, so
    // `notes.exe` cannot become `notes.exe.txt`-looking-like-an-executable.
    if let Some(found) = sanitized.extension() {
        if let Some(trimmed) = stem.strip_suffix(&format!(".{found}")) {
            stem = trimmed.to_owned();
        }
    }
    let stem = stem.trim_end_matches(['.', ' ']).trim().to_owned();
    let stem = if stem.is_empty() {
        "recording".to_owned()
    } else {
        stem
    };
    let short: String = stem.chars().take(80).collect();
    let short = short.trim_end_matches(['.', ' ']).to_owned();
    let short = if short.is_empty() {
        "recording".to_owned()
    } else {
        short
    };
    if discriminator.is_empty() {
        format!("{short}.{extension}")
    } else {
        format!("{short}-{discriminator}.{extension}")
    }
}

/// A validated directory an export may be written into.
#[derive(Debug, Clone)]
pub struct ExportDestination {
    directory: PathBuf,
    policy: PathPolicy,
}

impl ExportDestination {
    /// Validate a user-selected directory.
    ///
    /// The directory must already exist. It is canonicalized once here and
    /// every later write is pinned to the resolved location, so a path swapped
    /// underneath us cannot redirect a write. Exports are user-chosen
    /// locations, so a symlinked *export folder* is honoured
    /// ([`PathPolicy::allowing_symlink_escape`]); a link appearing at the
    /// target **file** name is still refused.
    pub fn new(directory: impl AsRef<Path>) -> ExportResult<Self> {
        let raw = directory.as_ref();
        if raw.as_os_str().is_empty() {
            return Err(ExportError::DestinationRejected(
                "no folder was selected".into(),
            ));
        }
        let canonical = std::fs::canonicalize(raw).map_err(|error| {
            ExportError::DestinationRejected(format!(
                "the folder could not be opened ({})",
                error.kind()
            ))
        })?;
        if !canonical.is_dir() {
            return Err(ExportError::DestinationRejected(
                "the selected path is not a folder".into(),
            ));
        }
        let policy = PathPolicy::for_roots([canonical.clone()]).allowing_symlink_escape();
        Ok(Self {
            directory: canonical,
            policy,
        })
    }

    /// The resolved directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// A short hint suitable for `export_events.destination_hint`.
    ///
    /// Spec 13.2 keeps complete sensitive paths out of the database, so this
    /// is the final directory component only.
    #[must_use]
    pub fn hint(&self) -> String {
        self.directory.file_name().map_or_else(
            || "selected folder".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Resolve a generated file name inside this destination.
    pub fn resolve(&self, file_name: &str) -> ExportResult<ValidatedPath> {
        let safe = sanitize_filename(file_name).on_disk().to_owned();
        if safe.is_empty() {
            return Err(ExportError::DestinationRejected(
                "the generated file name is empty".into(),
            ));
        }
        let validated = canonicalize_and_validate(&self.policy, &self.directory.join(&safe))
            .map_err(|error| {
                ExportError::DestinationRejected(format!("{} ({safe})", error.reason()))
            })?;
        if validated.parent() != Some(self.directory.as_path()) {
            return Err(ExportError::DestinationRejected(
                "the generated file name is not a direct child of the folder".into(),
            ));
        }
        Ok(validated)
    }

    /// Write `bytes` atomically to `file_name` inside this destination.
    pub fn write_atomic(&self, file_name: &str, bytes: &[u8]) -> ExportResult<PathBuf> {
        self.write_atomic_with(file_name, |file| {
            file.write_all(bytes)?;
            Ok(bytes.len() as u64)
        })
        .map(|(path, _)| path)
    }

    /// Stream into `file_name` atomically.
    ///
    /// `fill` receives the temporary file and returns the number of bytes it
    /// wrote. Any error removes the temporary file and leaves the destination
    /// untouched.
    pub fn write_atomic_with<F>(&self, file_name: &str, fill: F) -> ExportResult<(PathBuf, u64)>
    where
        F: FnOnce(&mut File) -> ExportResult<u64>,
    {
        let target = self.resolve(file_name)?;
        Self::reject_unsafe_target(target.as_path())?;
        let temporary = self
            .directory
            .join(format!(".plaudon-export-{}.part", Uuid::now_v7()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let outcome = fill(&mut file).and_then(|written| {
            file.flush()?;
            file.sync_all()?;
            Ok(written)
        });
        drop(file);
        let written = match outcome {
            Ok(written) => written,
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(error);
            }
        };
        if let Err(error) = std::fs::rename(&temporary, target.as_path()) {
            let _ = std::fs::remove_file(&temporary);
            return Err(ExportError::Io(error));
        }
        sync_directory(&self.directory);
        Ok((target.as_path().to_path_buf(), written))
    }

    /// Refuse to overwrite anything that is not a plain file we can replace.
    fn reject_unsafe_target(target: &Path) -> ExportResult<()> {
        match std::fs::symlink_metadata(target) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(
                ExportError::DestinationRejected("a symbolic link already exists at that name".into()),
            ),
            Ok(metadata) if !metadata.is_file() => Err(ExportError::DestinationRejected(
                "a folder or special file already exists at that name".into(),
            )),
            _ => Ok(()),
        }
    }
}

/// Best-effort directory flush so a rename survives a power loss.
///
/// Windows has no directory handle to flush; the rename is already ordered.
pub(crate) fn sync_directory(path: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_and_separators_cannot_survive_a_title() {
        for hostile in [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "/etc/shadow",
            "a/b",
            "a\0b",
            "\u{202e}gnp.exe",
            "CON",
            "   ",
            "...",
        ] {
            let name = build_file_name(hostile, "", "txt");
            assert!(!name.contains('/'), "{hostile} -> {name}");
            assert!(!name.contains('\\'), "{hostile} -> {name}");
            assert!(!name.contains('\0'), "{hostile} -> {name}");
            assert_eq!(Path::new(&name).components().count(), 1, "{hostile}");
            assert!(
                std::path::Path::new(&name).extension().is_some_and(|e| e == "txt"),
                "{hostile} -> {name}"
            );
        }
    }

    #[test]
    fn a_title_extension_cannot_masquerade_as_the_export_format() {
        let name = build_file_name("payroll.exe", "", "json");
        assert_eq!(name, "payroll.json");
    }

    #[test]
    fn the_discriminator_keeps_two_recordings_with_one_title_apart() {
        let a = build_file_name("Weekly sync", "0191aa", "md");
        let b = build_file_name("Weekly sync", "0191bb", "md");
        assert_ne!(a, b);
        assert!(a.starts_with("Weekly sync-"));
    }

    #[test]
    fn a_missing_directory_is_rejected_rather_than_created() {
        let dir = tempfile::tempdir().unwrap();
        let error = ExportDestination::new(dir.path().join("absent")).unwrap_err();
        assert!(matches!(error, ExportError::DestinationRejected(_)));
        assert!(!dir.path().join("absent").exists());
    }

    #[test]
    fn a_file_is_not_a_destination() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(ExportDestination::new(&file).is_err());
    }

    #[test]
    fn an_atomic_write_leaves_no_partial_file_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let destination = ExportDestination::new(dir.path()).unwrap();
        let error = destination
            .write_atomic_with("notes.txt", |file| {
                file.write_all(b"half")?;
                Err(ExportError::IntegrityFailed)
            })
            .unwrap_err();
        assert!(matches!(error, ExportError::IntegrityFailed));
        let remaining: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(remaining.is_empty(), "left behind {remaining:?}");
    }

    #[test]
    fn an_atomic_write_replaces_the_previous_export() {
        let dir = tempfile::tempdir().unwrap();
        let destination = ExportDestination::new(dir.path()).unwrap();
        destination.write_atomic("notes.txt", b"first").unwrap();
        let path = destination.write_atomic("notes.txt", b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "no temporary file may remain");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_target_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("notes.txt")).unwrap();

        let destination = ExportDestination::new(dir.path()).unwrap();
        let error = destination.write_atomic("notes.txt", b"attack").unwrap_err();
        assert!(matches!(error, ExportError::DestinationRejected(_)));
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");
    }
}
