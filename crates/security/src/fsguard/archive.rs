//! Portable-backup entry validation (spec §13.5, §13.6, §18.6A "Portable
//! backup | Traversal, symlink, duplicate entry, … | Reject archive **before
//! user data mutation**").
//!
//! This module is storage-format agnostic on purpose: it takes an
//! [`ArchiveEntryMeta`] describing one entry, never a zip or tar reader, so
//! `crates/export` can drive it without `crates/security` depending on an
//! archive library — and so the same rules apply to any container we add later.
//!
//! [`validate_archive_entry`] performs **no filesystem access at all**. That is
//! what lets a restore validate an entire manifest first and only then start
//! writing; a test can assert the destination is still empty after a hostile
//! manifest has been rejected. Resolving an accepted entry to a real path is a
//! separate, explicit step ([`ValidatedArchiveEntry::resolve_target`]) that
//! runs immediately before the write, so a symlink planted between inspection
//! and extraction is still caught.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{
    ErrorEnvelopeParts, SecurityError, ARCHIVE_ENTRY_REJECTED, CHOOSE_DIFFERENT_FILE,
};

use super::canonical::{
    canonicalize_and_validate, escape_for_message, validate_relative, PathError, PathPolicy,
    PathRejection, ValidatedPath,
};
use super::nfc::to_nfc;

/// Default per-entry uncompressed cap. Larger than any single recording we
/// expect, small enough that one declared entry cannot ask for a terabyte.
pub const DEFAULT_MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Default cumulative uncompressed cap for one archive.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
/// Default entry-count cap.
pub const DEFAULT_MAX_ENTRIES: usize = 1_000_000;
/// Default uncompressed:compressed ratio cap. Audio does not compress; JSON
/// manages roughly 10:1; a decompression bomb is three orders of magnitude up.
pub const DEFAULT_MAX_COMPRESSION_RATIO: u64 = 100;
/// Entries below this size are not ratio-checked: a 20-byte file in a 4-byte
/// deflate block is a 5:1 "ratio" that means nothing.
pub const DEFAULT_RATIO_CHECK_MIN_BYTES: u64 = 1024 * 1024;

/// What one archive entry claims to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArchiveEntryKind {
    File,
    Directory,
    /// Symbolic link. Never extracted: its target is attacker-chosen (§13.5).
    Symlink,
    /// Hard link. Never extracted: it aliases bytes we did not verify.
    HardLink,
    CharacterDevice,
    BlockDevice,
    Fifo,
    Socket,
    /// Anything the reader could not classify.
    Unknown,
}

impl ArchiveEntryKind {
    pub const fn label(self) -> &'static str {
        match self {
            ArchiveEntryKind::File => "file",
            ArchiveEntryKind::Directory => "directory",
            ArchiveEntryKind::Symlink => "symlink",
            ArchiveEntryKind::HardLink => "hard_link",
            ArchiveEntryKind::CharacterDevice => "character_device",
            ArchiveEntryKind::BlockDevice => "block_device",
            ArchiveEntryKind::Fifo => "fifo",
            ArchiveEntryKind::Socket => "socket",
            ArchiveEntryKind::Unknown => "unknown",
        }
    }

    /// Only regular files and directories are ever materialised.
    pub const fn is_extractable(self) -> bool {
        matches!(self, ArchiveEntryKind::File | ArchiveEntryKind::Directory)
    }

    const fn is_link(self) -> bool {
        matches!(self, ArchiveEntryKind::Symlink | ArchiveEntryKind::HardLink)
    }
}

impl fmt::Display for ArchiveEntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One entry's declared metadata, as read from the container's directory.
///
/// Everything here is attacker-controlled. `declared_uncompressed_size` in
/// particular is a claim: the extractor must still enforce the limit while
/// streaming, because a container can lie about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntryMeta {
    /// Path as declared in the container, `/`-separated by convention.
    pub path: String,
    pub kind: ArchiveEntryKind,
    pub declared_uncompressed_size: u64,
    pub compressed_size: u64,
}

impl ArchiveEntryMeta {
    /// A regular file entry.
    pub fn file(path: impl Into<String>, uncompressed: u64, compressed: u64) -> Self {
        Self {
            path: path.into(),
            kind: ArchiveEntryKind::File,
            declared_uncompressed_size: uncompressed,
            compressed_size: compressed,
        }
    }

    /// A directory entry.
    pub fn directory(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: ArchiveEntryKind::Directory,
            declared_uncompressed_size: 0,
            compressed_size: 0,
        }
    }

    /// An entry of some other type, which validation always rejects.
    pub fn of_kind(path: impl Into<String>, kind: ArchiveEntryKind) -> Self {
        Self {
            path: path.into(),
            kind,
            declared_uncompressed_size: 0,
            compressed_size: 0,
        }
    }
}

/// Caps and accumulated state for validating one archive.
///
/// The instance is **per archive**: it carries the set of normalised paths seen
/// so far and the running uncompressed total, which is how duplicate entries
/// and cumulative decompression bombs are caught.
#[derive(Debug, Clone)]
pub struct ArchivePolicy {
    destination_root: PathBuf,
    path_policy: PathPolicy,
    max_entry_bytes: u64,
    max_total_bytes: u64,
    max_entries: usize,
    max_compression_ratio: u64,
    ratio_check_min_bytes: u64,
    seen: BTreeSet<String>,
    total_bytes: u64,
}

impl ArchivePolicy {
    /// Validate entries for extraction into `destination_root`.
    pub fn new(destination_root: impl Into<PathBuf>) -> Self {
        let destination_root = destination_root.into();
        Self {
            path_policy: PathPolicy::for_roots([destination_root.clone()]),
            destination_root,
            max_entry_bytes: DEFAULT_MAX_ENTRY_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_compression_ratio: DEFAULT_MAX_COMPRESSION_RATIO,
            ratio_check_min_bytes: DEFAULT_RATIO_CHECK_MIN_BYTES,
            seen: BTreeSet::new(),
            total_bytes: 0,
        }
    }

    #[must_use]
    pub fn with_max_entry_bytes(mut self, bytes: u64) -> Self {
        self.max_entry_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_max_total_bytes(mut self, bytes: u64) -> Self {
        self.max_total_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_max_entries(mut self, entries: usize) -> Self {
        self.max_entries = entries;
        self
    }

    #[must_use]
    pub fn with_max_compression_ratio(mut self, ratio: u64) -> Self {
        self.max_compression_ratio = ratio;
        self
    }

    #[must_use]
    pub fn with_ratio_check_min_bytes(mut self, bytes: u64) -> Self {
        self.ratio_check_min_bytes = bytes;
        self
    }

    /// Require every entry path to stay inside the legacy Windows `MAX_PATH`
    /// budget, for an archive that must restore on any Windows host (§13.6).
    #[must_use]
    pub fn with_windows_short_path_compatible(mut self, yes: bool) -> Self {
        self.path_policy = self.path_policy.with_windows_short_path_compatible(yes);
        self
    }

    /// Cap the length of the destination path an entry expands to.
    #[must_use]
    pub fn with_max_path_len(mut self, len: usize) -> Self {
        self.path_policy = self.path_policy.with_max_path_len(len);
        self
    }

    pub fn destination_root(&self) -> &Path {
        &self.destination_root
    }

    /// Number of entries accepted so far.
    pub fn accepted_entries(&self) -> usize {
        self.seen.len()
    }

    /// Declared uncompressed bytes accepted so far.
    pub fn accepted_bytes(&self) -> u64 {
        self.total_bytes
    }
}

/// An entry that passed every pre-flight rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedArchiveEntry {
    relative: PathBuf,
    key: String,
    kind: ArchiveEntryKind,
    declared_uncompressed_size: u64,
}

impl ValidatedArchiveEntry {
    /// Normalised relative path: `/`-separated input, empty and `.` segments
    /// removed, no `..`, never absolute.
    pub fn relative_path(&self) -> &Path {
        &self.relative
    }

    /// The case- and NFC-folded key used for duplicate detection. Two entries
    /// with the same key would collide on a case-insensitive filesystem.
    pub fn duplicate_key(&self) -> &str {
        &self.key
    }

    pub const fn kind(&self) -> ArchiveEntryKind {
        self.kind
    }

    pub const fn declared_uncompressed_size(&self) -> u64 {
        self.declared_uncompressed_size
    }

    /// Resolve this entry against the destination root, on the filesystem.
    ///
    /// Call this immediately before creating the entry, not during the
    /// pre-flight pass: it is the check that catches a symlink planted into the
    /// destination tree *after* the manifest was inspected (§13.4 "account for
    /// symlink races"). The parent directories must already exist.
    pub fn resolve_target(&self, policy: &ArchivePolicy) -> Result<ValidatedPath, PathError> {
        canonicalize_and_validate(
            &policy.path_policy,
            &policy.destination_root.join(&self.relative),
        )
    }
}

/// Why an archive entry was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArchiveRejection {
    /// Symlink or hard-link entry.
    LinkEntry,
    /// Device, fifo, socket or unclassifiable entry.
    SpecialEntry,
    /// Absolute path, or one qualified with a drive or UNC prefix.
    AbsolutePath,
    /// `..` traversal, before or after normalisation.
    Traversal,
    /// The normalised path escapes the destination root.
    EscapesDestination,
    /// Another entry in this archive normalises to the same path, allowing for
    /// case-insensitive filesystems.
    DuplicatePath,
    /// The entry name breaks a filesystem naming rule (see [`PathRejection`]).
    UnsupportedName,
    /// Declared uncompressed size exceeds the per-entry cap.
    EntryTooLarge,
    /// Cumulative declared uncompressed size exceeds the archive cap.
    ArchiveTooLarge,
    /// More entries than the archive cap allows.
    TooManyEntries,
    /// Uncompressed:compressed ratio exceeds the cap — a decompression bomb.
    CompressionRatioExceeded,
}

impl ArchiveRejection {
    pub const fn rule(self) -> &'static str {
        match self {
            ArchiveRejection::LinkEntry => "link_entry",
            ArchiveRejection::SpecialEntry => "special_entry",
            ArchiveRejection::AbsolutePath => "absolute_path",
            ArchiveRejection::Traversal => "traversal",
            ArchiveRejection::EscapesDestination => "escapes_destination",
            ArchiveRejection::DuplicatePath => "duplicate_path",
            ArchiveRejection::UnsupportedName => "unsupported_name",
            ArchiveRejection::EntryTooLarge => "entry_too_large",
            ArchiveRejection::ArchiveTooLarge => "archive_too_large",
            ArchiveRejection::TooManyEntries => "too_many_entries",
            ArchiveRejection::CompressionRatioExceeded => "compression_ratio_exceeded",
        }
    }
}

impl fmt::Display for ArchiveRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.rule())
    }
}

/// A refused archive entry.
///
/// Carries the rule, the entry's declared kind, and at most a truncated,
/// escaped fragment of the entry name — never the whole path, which is
/// attacker-controlled text (§15.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntryError {
    reason: ArchiveRejection,
    kind: ArchiveEntryKind,
    label: String,
    name_rule: Option<PathRejection>,
}

impl ArchiveEntryError {
    fn new(reason: ArchiveRejection, entry: &ArchiveEntryMeta) -> Self {
        Self {
            reason,
            kind: entry.kind,
            label: escape_for_message(&entry.path, LABEL_CHARS),
            name_rule: None,
        }
    }

    fn from_name(entry: &ArchiveEntryMeta, error: &PathError) -> Self {
        let reason = match error.reason() {
            PathRejection::ParentTraversal => ArchiveRejection::Traversal,
            PathRejection::AbsolutePath
            | PathRejection::ForeignAbsolutePath
            | PathRejection::AmbiguousRootRelativePath
            | PathRejection::UncPath
            | PathRejection::DeviceNamespace => ArchiveRejection::AbsolutePath,
            _ => ArchiveRejection::UnsupportedName,
        };
        Self {
            reason,
            kind: entry.kind,
            label: escape_for_message(&entry.path, LABEL_CHARS),
            name_rule: Some(error.reason()),
        }
    }

    pub const fn reason(&self) -> ArchiveRejection {
        self.reason
    }

    pub const fn kind(&self) -> ArchiveEntryKind {
        self.kind
    }

    /// The specific naming rule, when the entry was refused for its name.
    pub const fn name_rule(&self) -> Option<PathRejection> {
        self.name_rule
    }

    /// Escaped, truncated entry label. Safe to log.
    pub fn label(&self) -> &str {
        &self.label
    }
}

const LABEL_CHARS: usize = 48;

impl fmt::Display for ArchiveEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "archive entry rejected: {} ({} entry \"{}\"",
            self.reason, self.kind, self.label
        )?;
        if let Some(rule) = self.name_rule {
            write!(f, ", rule {rule}")?;
        }
        f.write_str(")")
    }
}

impl std::error::Error for ArchiveEntryError {}

impl SecurityError for ArchiveEntryError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        ErrorEnvelopeParts::new(ARCHIVE_ENTRY_REJECTED, self.to_string())
            .retryable(false)
            .action(CHOOSE_DIFFERENT_FILE)
    }
}

/// Validate one archive entry against `policy`, mutating the policy's seen-set
/// and running totals when the entry is accepted.
///
/// Touches no filesystem. Run it over every entry of the container before
/// extracting any of them (§13.5: "reject archive before user data mutation").
pub fn validate_archive_entry(
    entry: &ArchiveEntryMeta,
    policy: &mut ArchivePolicy,
) -> Result<ValidatedArchiveEntry, ArchiveEntryError> {
    if entry.kind.is_link() {
        return Err(ArchiveEntryError::new(ArchiveRejection::LinkEntry, entry));
    }
    if !entry.kind.is_extractable() {
        return Err(ArchiveEntryError::new(
            ArchiveRejection::SpecialEntry,
            entry,
        ));
    }
    if policy.seen.len() >= policy.max_entries {
        return Err(ArchiveEntryError::new(
            ArchiveRejection::TooManyEntries,
            entry,
        ));
    }

    let relative = validate_relative(&policy.path_policy, &entry.path)
        .map_err(|error| ArchiveEntryError::from_name(entry, &error))?;

    // Defence in depth: `validate_relative` has already refused `..` and
    // absolute forms, so the join cannot escape — assert it anyway, because
    // this is the invariant the whole module exists to provide.
    let target = policy.destination_root.join(&relative);
    if !target.starts_with(&policy.destination_root) {
        return Err(ArchiveEntryError::new(
            ArchiveRejection::EscapesDestination,
            entry,
        ));
    }
    if let Some(target) = target.to_str() {
        if target.len() > policy.path_policy.max_path_len
            || (policy.path_policy.windows_short_path_compatible
                && target.len() > super::canonical::WINDOWS_SHORT_PATH_LIMIT)
        {
            return Err(ArchiveEntryError {
                reason: ArchiveRejection::UnsupportedName,
                kind: entry.kind,
                label: escape_for_message(&entry.path, LABEL_CHARS),
                name_rule: Some(PathRejection::PathTooLong),
            });
        }
    }

    let key = duplicate_key(&relative);
    if policy.seen.contains(&key) {
        return Err(ArchiveEntryError::new(
            ArchiveRejection::DuplicatePath,
            entry,
        ));
    }

    if entry.kind == ArchiveEntryKind::File {
        if entry.declared_uncompressed_size > policy.max_entry_bytes {
            return Err(ArchiveEntryError::new(
                ArchiveRejection::EntryTooLarge,
                entry,
            ));
        }
        let total = policy
            .total_bytes
            .saturating_add(entry.declared_uncompressed_size);
        if total > policy.max_total_bytes {
            return Err(ArchiveEntryError::new(
                ArchiveRejection::ArchiveTooLarge,
                entry,
            ));
        }
        if entry.declared_uncompressed_size >= policy.ratio_check_min_bytes {
            let ratio = entry.declared_uncompressed_size / entry.compressed_size.max(1);
            if ratio > policy.max_compression_ratio {
                return Err(ArchiveEntryError::new(
                    ArchiveRejection::CompressionRatioExceeded,
                    entry,
                ));
            }
        }
        policy.total_bytes = total;
    }

    policy.seen.insert(key.clone());
    Ok(ValidatedArchiveEntry {
        relative,
        key,
        kind: entry.kind,
        declared_uncompressed_size: entry.declared_uncompressed_size,
    })
}

/// Fold a normalised relative path to the key two entries would collide on.
///
/// NFC first (so a macOS-decomposed name and a Windows-composed name are one
/// key) then lowercase (so NTFS and APFS case-insensitivity cannot turn two
/// archive entries into one file with a surprise winner).
fn duplicate_key(relative: &Path) -> String {
    let joined: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    to_nfc(&joined.join("/")).to_lowercase()
}
