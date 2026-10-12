//! D2.2 — path canonicalisation, symlink/junction handling, private temp
//! directories and archive-entry validation.
//!
//! Spec: §16.5 (canonicalise at trust boundaries, private temp directories,
//! reject archive traversal, restrictive permissions), §7.4 (import from
//! arbitrary user-selected paths into a managed archive; never trust a symlink
//! or junction target), §13.4 (managed archive layout, sanitised extensions,
//! original names as metadata only, symlink races), §13.5 (restore inspects
//! before writing), §6.3B (staged file, checksum, fsync, same-volume rename),
//! §22.11 (imported source files remain untouched).
//!
//! # The three things this module provides
//!
//! 1. **[`canonicalize_and_validate`]** — the single entry point for every
//!    externally supplied path. It applies the naming rules of *both* platforms
//!    before touching the disk, resolves symlinks and junctions, and then
//!    requires the *resolved* path to be inside a permitted root. A link whose
//!    target leaves the root is rejected, never followed. The result is a
//!    [`ValidatedPath`], whose inner `PathBuf` is private — an unvalidated path
//!    cannot be substituted for one.
//!
//! 2. **[`PrivateTempDir`] / [`StagedFile`]** — an owner-only staging directory
//!    and the §6.3B commit: `fsync` the bytes, prove the target is on the same
//!    volume, `rename`, `fsync` the directory. A staging directory left behind
//!    by a crashed run is reclaimed at startup by
//!    [`PrivateTempDir::reconcile_abandoned`], which only ever removes
//!    directories matching our own name shape and older than a grace period.
//!
//! 3. **[`archive`]** — pure, filesystem-free validation of one portable-backup
//!    entry, so restore can reject a hostile archive *before* mutating user
//!    data (§13.5, §18.6A).
//!
//! [`sanitize_filename`] sits alongside them: it turns an untrusted name into a
//! pair of strings, one safe for the filesystem and one safe for the UI.
//!
//! # What this module does not promise
//!
//! * It cannot stop another process from replacing a directory between
//!   [`canonicalize_and_validate`] and the subsequent open; that race is
//!   narrowed by validating immediately before the operation, by staging into a
//!   `0o700` directory, and by opening staged files with `O_CREAT|O_EXCL`. It
//!   is not eliminated, because portable Rust has no `openat2`/`RESOLVE_BENEATH`.
//! * On Windows it does not read or set ACLs; see
//!   [`set_restrictive_permissions`] for exactly what is and is not established
//!   there.
//! * Deleting a validated path says nothing about user backups, synced folders
//!   or OS snapshots (§13.5).

pub mod archive;
mod canonical;
mod filename;
mod nfc;
mod nfc_tables;
mod perms;
mod tempdir;

#[cfg(test)]
mod tests;

pub use archive::{
    validate_archive_entry, ArchiveEntryError, ArchiveEntryKind, ArchiveEntryMeta, ArchivePolicy,
    ArchiveRejection, ValidatedArchiveEntry,
};
pub use canonical::{
    canonicalize_and_validate, check_component, escape_for_message, is_invisible_or_bidi,
    is_windows_device_name, validate_relative, PathError, PathPolicy, PathRejection, ValidatedPath,
    DEFAULT_MAX_COMPONENT_LEN, DEFAULT_MAX_PATH_LEN, WINDOWS_DEVICE_NAMES, WINDOWS_RESERVED_CHARS,
    WINDOWS_SHORT_PATH_LIMIT,
};
pub use filename::{
    sanitize_filename, SanitizedName, MAX_DISPLAY_CHARS, MAX_EXTENSION_BYTES, MAX_ON_DISK_BYTES,
};
pub use nfc::{combining_class, is_nfc, to_nfc};
pub use perms::{is_restrictive, same_volume, set_restrictive_permissions, volume_id, VolumeId};
pub use tempdir::{PrivateTempDir, ReconcileReport, StagedFile, STAGED_SUFFIX};

#[cfg(unix)]
pub use perms::{DIRECTORY_MODE, FILE_MODE};
