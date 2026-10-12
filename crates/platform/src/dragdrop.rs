//! Classifying what the user dropped on the window.
//!
//! A window drop is exactly as untrusted as a path typed into a text field:
//! the compositor hands over whatever was dragged, including symlinks that
//! point somewhere else entirely. [`classify`] therefore resolves every
//! dropped path and keeps only what resolves **inside an approved root**
//! (spec §7.4), which is the same rule
//! [`plaudon_app::validate::validate_import_path`] applies to a path that came
//! from a file picker, and the roots are the ones
//! [`plaudon_app::default_approved_roots`] produces.
//!
//! The outcome is four buckets:
//!
//! * audio the importer will accept, already validated into
//!   [`plaudon_app::validate::ValidatedImportPath`] so the shell can hand it
//!   straight to the import command,
//! * `.plaudon` portable archives (restore flow),
//! * directories (watch-folder flow),
//! * everything else, with a stable reason code.
//!
//! Dropped paths are **data**: a rejection records the path so the UI can show
//! the user which of their files was not accepted, but nothing in this module
//! logs it, and [`DropRejection::reason_code`] is what belongs in a log line.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use plaudon_app::validate::{validate_import_path, ValidatedImportPath, ALLOWED_MEDIA_EXTENSIONS};

use crate::associations::ARCHIVE_EXTENSION;

/// Why a dropped path was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropRejection {
    /// The path could not be resolved: it is gone, unreadable, or a symlink loop.
    Unresolvable,
    /// The path is not valid UTF-8 and cannot reach the command boundary.
    NotUtf8,
    /// It resolved outside every approved root (the classic escaping symlink).
    OutsideApprovedRoots,
    /// It resolved to a socket, FIFO or device node.
    NotARegularFile,
    /// A regular file, but not audio we import and not a `.plaudon` archive.
    UnsupportedType,
    /// The caller configured no approved root, so nothing can be accepted.
    NoApprovedRoots,
}

impl DropRejection {
    /// Stable, user-content-free reason code for logs and UI copy.
    #[must_use]
    pub const fn reason_code(self) -> &'static str {
        match self {
            Self::Unresolvable => "unresolvable",
            Self::NotUtf8 => "not_utf8",
            Self::OutsideApprovedRoots => "outside_approved_roots",
            Self::NotARegularFile => "not_a_regular_file",
            Self::UnsupportedType => "unsupported_type",
            Self::NoApprovedRoots => "no_approved_roots",
        }
    }
}

/// A dropped path that was not accepted, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedDrop {
    /// The path as dropped. User data: show it, never log it.
    pub path: PathBuf,
    /// Why it was rejected.
    pub reason: DropRejection,
}

/// What a drop contained, after validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DropClassification {
    /// Audio files ready to be imported.
    pub audio: Vec<ValidatedImportPath>,
    /// Resolved `.plaudon` archives.
    pub archives: Vec<PathBuf>,
    /// Resolved directories.
    pub directories: Vec<PathBuf>,
    /// Everything that was refused.
    pub rejected: Vec<RejectedDrop>,
}

impl DropClassification {
    /// Whether anything at all was accepted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.audio.is_empty() && self.archives.is_empty() && self.directories.is_empty()
    }

    /// How many paths were accepted.
    #[must_use]
    pub fn accepted_count(&self) -> usize {
        self.audio.len() + self.archives.len() + self.directories.len()
    }
}

/// Classify every dropped path against `approved_roots`.
///
/// Resolution order per path: canonicalize, check containment, check the file
/// type, then the extension. A path that fails any step lands in
/// [`DropClassification::rejected`] with a reason; the others are unaffected,
/// because dropping ten files of which one is a symlink to `/etc` must still
/// import the nine.
#[must_use]
pub fn classify(paths: &[PathBuf], approved_roots: &[PathBuf]) -> DropClassification {
    let mut out = DropClassification::default();
    let roots = canonical_roots(approved_roots);
    for path in paths {
        match classify_one(path, &roots, approved_roots, &mut out) {
            Ok(()) => {}
            Err(reason) => out.rejected.push(RejectedDrop {
                path: path.clone(),
                reason,
            }),
        }
    }
    tracing::debug!(
        accepted = out.accepted_count(),
        rejected = out.rejected.len(),
        "classified a window drop"
    );
    out
}

/// Resolve the approved roots once per drop, the way the application layer
/// does when it validates a picked path.
fn canonical_roots(approved_roots: &[PathBuf]) -> Vec<PathBuf> {
    approved_roots
        .iter()
        .map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
        .collect()
}

fn classify_one(
    path: &Path,
    roots: &[PathBuf],
    raw_roots: &[PathBuf],
    out: &mut DropClassification,
) -> Result<(), DropRejection> {
    if roots.is_empty() {
        return Err(DropRejection::NoApprovedRoots);
    }
    let resolved = std::fs::canonicalize(path).map_err(|_| DropRejection::Unresolvable)?;
    if !roots.iter().any(|root| resolved.starts_with(root)) {
        return Err(DropRejection::OutsideApprovedRoots);
    }

    let metadata = std::fs::metadata(&resolved).map_err(|_| DropRejection::Unresolvable)?;
    if metadata.is_dir() {
        out.directories.push(resolved);
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(DropRejection::NotARegularFile);
    }

    let extension = resolved
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    if extension == ARCHIVE_EXTENSION {
        out.archives.push(resolved);
        return Ok(());
    }
    if !ALLOWED_MEDIA_EXTENSIONS.contains(&extension.as_str()) {
        return Err(DropRejection::UnsupportedType);
    }

    // Hand the path through the application layer's own validator so a drop
    // and a file-picker selection cannot diverge: it re-checks the roots, the
    // file type and the allowlist, and yields the record the import command
    // consumes.
    let requested = path.to_str().ok_or(DropRejection::NotUtf8)?;
    let validated =
        validate_import_path(requested, raw_roots).map_err(|_| DropRejection::Unresolvable)?;
    out.audio.push(validated);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, bytes).expect("write fixture");
    }

    /// `tempfile` hands back `/var/folders/…` on macOS, which is itself a
    /// symlink to `/private/var/…`; an approved root has to be canonical or
    /// every path "escapes" it.
    fn roots(dir: &Path) -> Vec<PathBuf> {
        vec![std::fs::canonicalize(dir).expect("canonical root")]
    }

    #[test]
    fn audio_archives_and_directories_are_separated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let audio = root.join("meeting.WAV");
        let archive = root.join("backup.plaudon");
        let folder = root.join("inbox");
        write(&audio, b"RIFF");
        write(&archive, b"PK");
        std::fs::create_dir_all(&folder).expect("mkdir");

        let out = classify(
            &[audio.clone(), archive.clone(), folder.clone()],
            &roots(root),
        );

        assert_eq!(out.audio.len(), 1);
        assert_eq!(out.audio[0].extension, "wav");
        assert_eq!(out.audio[0].original_file_name, "meeting.WAV");
        assert_eq!(out.archives, vec![std::fs::canonicalize(&archive).unwrap()]);
        assert_eq!(
            out.directories,
            vec![std::fs::canonicalize(&folder).unwrap()]
        );
        assert!(out.rejected.is_empty(), "nothing in this drop is refusable");
        assert_eq!(out.accepted_count(), 3);
        assert!(!out.is_empty(), "expected out to be non-empty");
    }

    #[test]
    fn every_allowed_media_extension_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let paths: Vec<PathBuf> = ALLOWED_MEDIA_EXTENSIONS
            .iter()
            .map(|extension| {
                let path = root.join(format!("clip.{extension}"));
                write(&path, b"x");
                path
            })
            .collect();

        let out = classify(&paths, &roots(root));
        assert_eq!(out.audio.len(), ALLOWED_MEDIA_EXTENSIONS.len());
        assert!(
            out.rejected.is_empty(),
            "every extension on the import allowlist must be accepted"
        );
    }

    #[test]
    fn an_unsupported_file_is_rejected_by_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let path = root.join("notes.txt");
        write(&path, b"text");
        let bare = root.join("noextension");
        write(&bare, b"text");

        let out = classify(&[path.clone(), bare.clone()], &roots(root));
        assert_eq!(out.accepted_count(), 0);
        assert_eq!(
            out.rejected,
            vec![
                RejectedDrop {
                    path,
                    reason: DropRejection::UnsupportedType
                },
                RejectedDrop {
                    path: bare,
                    reason: DropRejection::UnsupportedType
                },
            ]
        );
    }

    #[test]
    fn a_missing_path_is_rejected_as_unresolvable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gone.wav");
        let out = classify(std::slice::from_ref(&missing), &roots(dir.path()));
        assert_eq!(
            out.rejected,
            vec![RejectedDrop {
                path: missing,
                reason: DropRejection::Unresolvable
            }]
        );
    }

    #[test]
    fn a_path_outside_every_approved_root_is_rejected() {
        let approved = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let outside = elsewhere.path().join("other.wav");
        write(&outside, b"RIFF");

        let out = classify(std::slice::from_ref(&outside), &roots(approved.path()));
        assert_eq!(
            out.rejected,
            vec![RejectedDrop {
                path: outside,
                reason: DropRejection::OutsideApprovedRoots
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_escapes_the_approved_roots_is_rejected() {
        let approved = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let real = elsewhere.path().join("escaped.wav");
        write(&real, b"RIFF");

        // The link lives inside the approved root and looks like audio; only
        // its *resolved* target gives it away.
        let link = approved.path().join("innocent.wav");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(
            link.exists(),
            "the link must resolve, or the test is vacuous"
        );

        let out = classify(std::slice::from_ref(&link), &roots(approved.path()));
        assert!(out.audio.is_empty(), "an escaping symlink must not import");
        assert_eq!(
            out.rejected,
            vec![RejectedDrop {
                path: link,
                reason: DropRejection::OutsideApprovedRoots
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_stays_inside_the_approved_roots_is_accepted() {
        let approved = tempfile::tempdir().expect("tempdir");
        let real = approved.path().join("recorded.wav");
        write(&real, b"RIFF");
        let link = approved.path().join("link.wav");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let out = classify(&[link], &roots(approved.path()));
        assert_eq!(out.audio.len(), 1);
        assert_eq!(
            out.audio[0].resolved,
            std::fs::canonicalize(&real).expect("canonical")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_inside_an_approved_root_is_not_a_regular_file() {
        use std::process::Command;

        let dir = tempfile::tempdir().expect("tempdir");
        let fifo = dir.path().join("pipe.wav");
        let made = Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            // No `mkfifo` on this host; the regular-file rule is still covered
            // by the directory and symlink cases above.
            return;
        }

        let out = classify(std::slice::from_ref(&fifo), &roots(dir.path()));
        assert_eq!(
            out.rejected,
            vec![RejectedDrop {
                path: fifo,
                reason: DropRejection::NotARegularFile
            }]
        );
    }

    #[test]
    fn without_an_approved_root_nothing_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.wav");
        write(&path, b"RIFF");
        let out = classify(std::slice::from_ref(&path), &[]);
        assert_eq!(
            out.rejected,
            vec![RejectedDrop {
                path,
                reason: DropRejection::NoApprovedRoots
            }]
        );
    }

    #[test]
    fn one_bad_path_does_not_discard_the_good_ones() {
        let approved = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let good = approved.path().join("good.wav");
        let bad = elsewhere.path().join("bad.wav");
        write(&good, b"RIFF");
        write(&bad, b"RIFF");

        let out = classify(&[bad, good, PathBuf::from("")], &roots(approved.path()));
        assert_eq!(out.audio.len(), 1);
        assert_eq!(out.rejected.len(), 2);
    }

    #[test]
    fn reason_codes_are_stable_and_content_free() {
        for (reason, code) in [
            (DropRejection::Unresolvable, "unresolvable"),
            (DropRejection::NotUtf8, "not_utf8"),
            (
                DropRejection::OutsideApprovedRoots,
                "outside_approved_roots",
            ),
            (DropRejection::NotARegularFile, "not_a_regular_file"),
            (DropRejection::UnsupportedType, "unsupported_type"),
            (DropRejection::NoApprovedRoots, "no_approved_roots"),
        ] {
            assert_eq!(reason.reason_code(), code);
        }
    }

    #[test]
    fn an_empty_drop_classifies_to_nothing() {
        let out = classify(&[], &[PathBuf::from("/")]);
        assert_eq!(out.accepted_count(), 0);
        assert!(out.rejected.is_empty(), "nothing was dropped to refuse");
    }

    #[test]
    fn the_default_approved_roots_of_the_application_layer_are_usable_here() {
        // The shell passes exactly these; an empty list would silently reject
        // every drop, so the contract is worth asserting.
        let roots = plaudon_app::default_approved_roots();
        assert!(
            !roots.is_empty(),
            "the application layer must offer at least the home directory"
        );
        let out = classify(&[PathBuf::from("/definitely/not/here.wav")], &roots);
        assert_eq!(out.rejected[0].reason, DropRejection::Unresolvable);
    }
}
