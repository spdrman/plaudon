//! Restrictive permissions on managed data, and the volume identity used to
//! prove a rename cannot cross a filesystem boundary (spec §16.5, §6.3B).

use std::fs;
use std::path::Path;

use super::canonical::{PathError, PathRejection, ValidatedPath};

/// Directory mode for managed data: owner-only read/write/traverse.
#[cfg(unix)]
pub const DIRECTORY_MODE: u32 = 0o700;
/// File mode for managed data: owner-only read/write.
#[cfg(unix)]
pub const FILE_MODE: u32 = 0o600;

/// Apply the most restrictive permissions this platform lets us set.
///
/// * **unix** — `0o700` for a directory, `0o600` for a file, applied with
///   `chmod`. This is enforcement: after the call the mode bits are checked by
///   [`is_restrictive`].
/// * **Windows** — std exposes only the read-only flag, which is not an access
///   control. The mechanism there is **inheritance**: the managed data lives
///   under the user profile (`%LOCALAPPDATA%`), whose ACL grants the owning
///   user and administrators only, and a newly created child inherits it. We
///   do not claim to have verified that ACL — reading it needs
///   `GetNamedSecurityInfo`, which this crate does not link — so
///   [`is_restrictive`] returns `None` on Windows rather than a reassuring
///   `true`.
pub fn set_restrictive_permissions(path: &ValidatedPath) -> Result<(), PathError> {
    let metadata = fs::symlink_metadata(path.as_path()).map_err(|err| PathError::from_io(&err))?;
    if metadata.file_type().is_symlink() {
        // Changing the mode of a symlink changes the mode of its target.
        return Err(PathError::new(PathRejection::SymlinkEscapesRoot));
    }
    apply_restrictive(path.as_path(), metadata.is_dir())
}

#[cfg(unix)]
fn apply_restrictive(path: &Path, is_dir: bool) -> Result<(), PathError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if is_dir { DIRECTORY_MODE } else { FILE_MODE };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|err| PathError::from_io(&err))?;
    match is_restrictive(path) {
        Some(true) | None => Ok(()),
        Some(false) => Err(PathError::new(PathRejection::PermissionsNotApplied)),
    }
}

#[cfg(not(unix))]
fn apply_restrictive(path: &Path, _is_dir: bool) -> Result<(), PathError> {
    // Confirm the entry is still reachable; the ACL itself is inherited from
    // the user profile and is not something std can set or read.
    fs::symlink_metadata(path)
        .map_err(|err| PathError::from_io(&err))
        .map(|_| ())
}

/// Whether `path` is readable only by its owner.
///
/// `None` means "this platform's API does not let us answer" — on Windows,
/// where std cannot read an ACL. Callers must treat `None` as unknown, never as
/// a guarantee.
#[cfg(unix)]
pub fn is_restrictive(path: &Path) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path).ok()?;
    let mode = metadata.permissions().mode() & 0o777;
    // The low six bits are the group and other permissions; all clear means
    // owner-only.
    Some(mode.trailing_zeros() >= 6)
}

#[cfg(not(unix))]
pub fn is_restrictive(path: &Path) -> Option<bool> {
    let _ = fs::symlink_metadata(path).ok()?;
    None
}

/// Identity of the filesystem a path lives on.
///
/// Two paths with the same `VolumeId` are on the same mounted filesystem, so a
/// rename between them is an atomic directory-entry swap. Different ids mean a
/// rename would fail with `EXDEV` (or silently become copy+delete in a helper
/// library), which §6.3B forbids for an import commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VolumeId(u64);

impl VolumeId {
    /// Construct from a raw platform id. Exposed so the comparison logic can be
    /// exercised deterministically on a host that has only one volume.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Read the volume identity of an existing path.
///
/// * **unix** — `st_dev` from `lstat`, the kernel's own filesystem id.
/// * **Windows** — the canonical path prefix (drive letter or UNC share),
///   lowercased and hashed. std cannot read a volume serial number on stable
///   Rust (`MetadataExt::volume_serial_number` is unstable), and the prefix of
///   a canonicalised path is the mount point, so two paths with the same prefix
///   are on the same volume. A directory mounted *into* a folder on another
///   volume would defeat the prefix comparison; the rename itself still fails
///   closed with an `io` error in that case rather than silently copying.
pub fn volume_id(path: &Path) -> Result<VolumeId, PathError> {
    volume_id_impl(path)
}

#[cfg(unix)]
fn volume_id_impl(path: &Path) -> Result<VolumeId, PathError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|err| PathError::from_io(&err))?;
    Ok(VolumeId(metadata.dev()))
}

#[cfg(not(unix))]
fn volume_id_impl(path: &Path) -> Result<VolumeId, PathError> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::path::Component;

    let canonical = fs::canonicalize(path).map_err(|err| PathError::from_io(&err))?;
    let prefix = canonical
        .components()
        .find_map(|component| match component {
            Component::Prefix(prefix) => prefix.as_os_str().to_str().map(str::to_ascii_lowercase),
            _ => None,
        })
        .ok_or_else(|| PathError::new(PathRejection::AmbiguousRootRelativePath))?;
    let mut hasher = DefaultHasher::new();
    prefix.hash(&mut hasher);
    Ok(VolumeId(hasher.finish()))
}

/// Whether a rename between two volumes may be attempted.
pub const fn same_volume(a: VolumeId, b: VolumeId) -> bool {
    a.0 == b.0
}

/// Refuse a rename that would cross a filesystem boundary.
///
/// Separated from [`volume_id`] so the decision can be exercised with injected
/// ids on a host that has only one writable volume.
pub(crate) fn require_same_volume(staging: VolumeId, target: VolumeId) -> Result<(), PathError> {
    if same_volume(staging, target) {
        return Ok(());
    }
    Err(PathError::with_detail(
        PathRejection::CrossVolumeRename,
        format!("{:x} != {:x}", staging.as_u64(), target.as_u64()),
    ))
}
