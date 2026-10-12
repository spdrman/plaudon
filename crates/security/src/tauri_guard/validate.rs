//! Rust-side argument validation for Tauri commands (§15.1, §6.3A).
//!
//! §15.1: *"We will validate all input arguments inside Rust, never accept raw
//! file paths for arbitrary filesystem operations except those received via
//! approved file-selection mechanisms"*. These helpers are the shared
//! vocabulary for that; a command handler validates every argument through them
//! before the value reaches domain code.
//!
//! Validation is deliberately **allowlist-shaped**: each helper states the
//! characters and bounds it accepts and rejects everything else, rather than
//! trying to enumerate hostile inputs.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{
    ErrorEnvelopeParts, SecurityError, CHOOSE_DIFFERENT_FILE, COMMAND_ARGUMENT_INVALID,
};

/// Upper bound on an identifier (UUIDs, job IDs, device IDs, model IDs).
pub const MAX_IDENTIFIER_LEN: usize = 128;
/// Upper bound on an opaque pagination cursor.
pub const MAX_CURSOR_LEN: usize = 512;

/// A command argument that failed Rust-side validation.
///
/// `field` is always a `&'static str` from our own vocabulary, so an envelope
/// built from this error can never carry attacker-controlled text (§15.4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgumentError {
    #[error("`{field}` must not be empty")]
    Empty { field: &'static str },
    #[error("`{field}` exceeds the maximum length of {max}")]
    TooLong { field: &'static str, max: usize },
    #[error("`{field}` contains a character that is not allowed")]
    DisallowedCharacter { field: &'static str },
    #[error("`{field}` is not one of the accepted values")]
    NotAccepted { field: &'static str },
    /// §15.1: a raw filesystem path arrived where only an identifier or an
    /// [`ApprovedPathHandle`] is acceptable.
    #[error("`{field}` looks like a filesystem path; only approved file selections are accepted")]
    RawPath { field: &'static str },
    /// A selection could not be resolved to a real, canonical location.
    #[error("`{field}` could not be resolved to an accessible location")]
    UnresolvableSelection { field: &'static str },
}

impl ArgumentError {
    #[must_use]
    pub fn field(&self) -> &'static str {
        match self {
            ArgumentError::Empty { field }
            | ArgumentError::TooLong { field, .. }
            | ArgumentError::DisallowedCharacter { field }
            | ArgumentError::NotAccepted { field }
            | ArgumentError::RawPath { field }
            | ArgumentError::UnresolvableSelection { field } => field,
        }
    }
}

impl SecurityError for ArgumentError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        let parts = ErrorEnvelopeParts::new(
            COMMAND_ARGUMENT_INVALID,
            format!("The value supplied for “{}” is not valid.", self.field()),
        )
        .retryable(false);
        match self {
            ArgumentError::RawPath { .. } | ArgumentError::UnresolvableSelection { .. } => {
                parts.action(CHOOSE_DIFFERENT_FILE)
            }
            _ => parts,
        }
    }
}

/// Accept an opaque identifier: non-empty, bounded, `[A-Za-z0-9_-]` only.
///
/// Covers UUIDs, job IDs, revision IDs, device IDs and source IDs. Rejecting
/// `/`, `\`, `.`, `:` and whitespace means an identifier can never be reused as
/// a path fragment, a CLI flag or a shell token.
pub fn validate_identifier<'a>(
    field: &'static str,
    value: &'a str,
) -> Result<&'a str, ArgumentError> {
    if value.is_empty() {
        return Err(ArgumentError::Empty { field });
    }
    if value.len() > MAX_IDENTIFIER_LEN {
        return Err(ArgumentError::TooLong {
            field,
            max: MAX_IDENTIFIER_LEN,
        });
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ArgumentError::DisallowedCharacter { field });
    }
    Ok(value)
}

/// Accept free text (titles, tags, search terms) up to `max` **characters**.
///
/// Control characters other than `\n` and `\t` are rejected: they are never
/// meaningful in user text and are the usual vehicle for terminal-escape and
/// log-injection tricks. Surrogate/NUL handling is covered by `&str` itself.
pub fn validate_bounded_string<'a>(
    field: &'static str,
    value: &'a str,
    max: usize,
) -> Result<&'a str, ArgumentError> {
    if value.chars().count() > max {
        return Err(ArgumentError::TooLong { field, max });
    }
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ArgumentError::DisallowedCharacter { field });
    }
    Ok(value)
}

/// Accept an opaque pagination cursor previously issued by Rust.
///
/// Cursors are base64url-shaped (`[A-Za-z0-9_-=.]`) and bounded. The frontend
/// must treat them as opaque; this check only establishes that nothing else can
/// be smuggled through the field.
pub fn validate_cursor<'a>(field: &'static str, value: &'a str) -> Result<&'a str, ArgumentError> {
    if value.is_empty() {
        return Err(ArgumentError::Empty { field });
    }
    if value.len() > MAX_CURSOR_LEN {
        return Err(ArgumentError::TooLong {
            field,
            max: MAX_CURSOR_LEN,
        });
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '.'))
    {
        return Err(ArgumentError::DisallowedCharacter { field });
    }
    Ok(value)
}

/// Accept one of a fixed set of values, returning the matched `&'static str` so
/// downstream code works with our own vocabulary rather than the caller's
/// buffer.
pub fn validate_enum(
    field: &'static str,
    value: &str,
    accepted: &[&'static str],
) -> Result<&'static str, ArgumentError> {
    accepted
        .iter()
        .copied()
        .find(|candidate| *candidate == value)
        .ok_or(ArgumentError::NotAccepted { field })
}

/// Reject a value that looks like a filesystem path (§15.1).
///
/// Used on string arguments that must never be paths — identifiers, titles,
/// export format names. The only way a path reaches a command is as an
/// [`ApprovedPathHandle`], which this crate alone can mint.
///
/// Rejected shapes: absolute POSIX paths, `~` expansion, Windows drive letters,
/// UNC prefixes, any `/` or `\` separator, `..` traversal, and URL schemes that
/// resolve to local files (`file:`, `asset:`).
pub fn reject_raw_path(field: &'static str, value: &str) -> Result<(), ArgumentError> {
    let looks_like_path = value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with("\\\\")
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || has_windows_drive_prefix(value)
        || starts_with_ascii_case_insensitive(value, "file:")
        || starts_with_ascii_case_insensitive(value, "asset:");
    if looks_like_path {
        return Err(ArgumentError::RawPath { field });
    }
    Ok(())
}

fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Compare an ASCII prefix **by bytes**.
///
/// `value` is untrusted command input and may contain multi-byte characters,
/// so slicing it by byte index (`value[..prefix.len()]`) would panic whenever
/// the cut lands inside a character. Byte comparison has no such edge.
fn starts_with_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    let value = value.as_bytes();
    let prefix = prefix.as_bytes();
    value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Where an approved path came from. Commands may require a specific origin —
/// an import must not accept a save-dialog destination, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionOrigin {
    /// The user picked existing file(s) in the OS open dialog.
    OpenFileDialog,
    /// The user picked a folder in the OS folder dialog (watched folders).
    OpenFolderDialog,
    /// The user chose a destination in the OS save dialog (exports, backups).
    SaveFileDialog,
    /// The user dropped file(s) onto the window. The OS grants access the same
    /// way it does for an open dialog.
    DragAndDrop,
}

impl SelectionOrigin {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SelectionOrigin::OpenFileDialog => "open_file_dialog",
            SelectionOrigin::OpenFolderDialog => "open_folder_dialog",
            SelectionOrigin::SaveFileDialog => "save_file_dialog",
            SelectionOrigin::DragAndDrop => "drag_and_drop",
        }
    }

    /// A save destination may not exist yet, so only its parent is resolved.
    const fn target_may_not_exist(self) -> bool {
        matches!(self, SelectionOrigin::SaveFileDialog)
    }
}

impl fmt::Display for SelectionOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A filesystem location the **backend** approved, carrying its provenance.
///
/// # What this type proves, and what it does not
///
/// It proves that backend code called [`ApprovedPathHandle::from_dialog_selection`]
/// with a stated origin at a stated time, and that the location resolved to a
/// real canonical path. There is deliberately no `From<String>`, no
/// [`std::str::FromStr`], no `Deserialize` and no public field, so a string that
/// arrives over the IPC boundary cannot become a handle — the frontend has no
/// route into this type at all.
///
/// It does **not** cryptographically prove that an operating-system dialog
/// returned the path; that would require an OS primitive that does not exist on
/// the target platforms. The assurance is structural, and intentionally stated
/// as such (§16.6).
///
/// A plain path string cannot be converted:
///
/// ```compile_fail
/// use plaudon_security::tauri_guard::ApprovedPathHandle;
/// let handle: ApprovedPathHandle = "/etc/passwd".to_owned().into();
/// ```
///
/// …nor deserialized from an IPC payload:
///
/// ```compile_fail
/// use plaudon_security::tauri_guard::ApprovedPathHandle;
/// let handle: ApprovedPathHandle = serde_json::from_str("\"/etc/passwd\"").unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedPathHandle {
    canonical: PathBuf,
    origin: SelectionOrigin,
    approved_at_unix: i64,
}

impl ApprovedPathHandle {
    /// Mint a handle from a path the OS file-selection UI returned.
    ///
    /// This is the **only** constructor. Call it in the backend code that
    /// receives the dialog result; never from a command handler using a
    /// frontend-supplied string.
    ///
    /// `approved_at_unix` is injected rather than read from the clock so the
    /// decision is deterministic and testable.
    ///
    /// The path is canonicalized here (symlinks resolved, `..` removed) so
    /// every downstream check works on a stable location. For
    /// [`SelectionOrigin::SaveFileDialog`] the parent directory is resolved and
    /// the chosen file name appended, because the target does not exist yet.
    pub fn from_dialog_selection(
        origin: SelectionOrigin,
        selected: &Path,
        approved_at_unix: i64,
    ) -> Result<Self, ArgumentError> {
        const FIELD: &str = "selection";
        let canonical = if origin.target_may_not_exist() {
            if let Ok(existing) = selected.canonicalize() {
                existing
            } else {
                let parent = selected
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .ok_or(ArgumentError::UnresolvableSelection { field: FIELD })?;
                let name = selected
                    .file_name()
                    .ok_or(ArgumentError::UnresolvableSelection { field: FIELD })?;
                parent
                    .canonicalize()
                    .map_err(|_| ArgumentError::UnresolvableSelection { field: FIELD })?
                    .join(name)
            }
        } else {
            selected
                .canonicalize()
                .map_err(|_| ArgumentError::UnresolvableSelection { field: FIELD })?
        };
        Ok(Self {
            canonical,
            origin,
            approved_at_unix,
        })
    }

    /// The canonical location this handle authorizes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.canonical
    }

    #[must_use]
    pub fn origin(&self) -> SelectionOrigin {
        self.origin
    }

    #[must_use]
    pub fn approved_at_unix(&self) -> i64 {
        self.approved_at_unix
    }

    /// Require a particular provenance, e.g. an export destination must come
    /// from the save dialog and not from a dropped file.
    pub fn require_origin(&self, expected: SelectionOrigin) -> Result<&Self, ArgumentError> {
        if self.origin == expected {
            Ok(self)
        } else {
            Err(ArgumentError::NotAccepted {
                field: "selection_origin",
            })
        }
    }
}
