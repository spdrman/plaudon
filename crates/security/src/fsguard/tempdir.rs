//! Private staging directories and the staged-file commit (spec §16.5 "use
//! private temp directories", §6.3B "staged file, checksum, fsync, verified
//! same-volume rename/commit", §13.4 `temp/imports/<job-uuid>.part`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::canonical::{
    canonicalize_and_validate, check_component, PathError, PathPolicy, PathRejection,
    ValidatedPath, DEFAULT_MAX_COMPONENT_LEN,
};
use super::perms::{is_restrictive, require_same_volume, set_restrictive_permissions, volume_id};

/// Length of the random part of a staging directory name, in hex characters.
const TOKEN_HEX_LEN: usize = 32;
/// Suffix of a file that has been staged but not committed.
pub const STAGED_SUFFIX: &str = ".part";
/// How many times we retry a name collision before giving up.
const CREATE_ATTEMPTS: usize = 8;

/// A per-process staging directory that only its owner can enter.
///
/// Created as `<base>/<prefix>-<pid>-<32 hex>` with mode `0o700` on unix
/// (atomically, via `DirBuilder::mode`, so there is no window in which the
/// umask leaves it group- or world-readable). On Windows the directory inherits
/// the ACL of the user profile; see [`super::set_restrictive_permissions`] for
/// what that does and does not establish.
///
/// Removed on drop. A process that dies without unwinding leaves the directory
/// behind, which is what [`PrivateTempDir::reconcile_abandoned`] exists for.
#[derive(Debug)]
pub struct PrivateTempDir {
    path: PathBuf,
    prefix: String,
}

impl PrivateTempDir {
    /// Create a staging directory under `base` (or the OS temp directory).
    ///
    /// `process_prefix` identifies the owning app/subsystem and must be ASCII
    /// alphanumeric, `-` or `_`; it is the only thing
    /// [`PrivateTempDir::reconcile_abandoned`] will act on later.
    pub fn create(base: Option<&Path>, process_prefix: &str) -> Result<Self, PathError> {
        check_prefix(process_prefix)?;
        let base = match base {
            Some(base) => base.to_path_buf(),
            None => std::env::temp_dir(),
        };
        fs::create_dir_all(&base).map_err(|err| PathError::from_io(&err))?;
        let base = fs::canonicalize(&base).map_err(|err| PathError::from_io(&err))?;

        let pid = std::process::id();
        let mut last_err = None;
        for _ in 0..CREATE_ATTEMPTS {
            let candidate = base.join(format!("{process_prefix}-{pid}-{}", random_token()));
            match create_private_dir(&candidate) {
                Ok(()) => {
                    // Canonicalise: on macOS the OS temp directory is reached
                    // through /var -> /private/var, and every later containment
                    // check compares against this path.
                    let path =
                        fs::canonicalize(&candidate).map_err(|err| PathError::from_io(&err))?;
                    if is_restrictive(&path) == Some(false) {
                        let _ = fs::remove_dir_all(&path);
                        return Err(PathError::new(PathRejection::PermissionsNotApplied));
                    }
                    return Ok(Self {
                        path,
                        prefix: process_prefix.to_owned(),
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    last_err = Some(err);
                }
                Err(err) => return Err(PathError::from_io(&err)),
            }
        }
        Err(last_err
            .as_ref()
            .map_or_else(|| PathError::new(PathRejection::Io), PathError::from_io))
    }

    /// The canonical path of the staging directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The process prefix this directory was created with.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// A [`PathPolicy`] whose only permitted root is this directory.
    pub fn policy(&self) -> PathPolicy {
        PathPolicy::for_roots([self.path.clone()])
    }

    /// Create a new `<name>.part` file inside the staging directory.
    ///
    /// Opened with `create_new`, i.e. `O_CREAT|O_EXCL`, which refuses to follow
    /// a pre-existing symlink at that name — the staging-directory half of the
    /// symlink-race defence in §13.4.
    pub fn stage_file(&self, name: &str) -> Result<StagedFile, PathError> {
        check_component(name, DEFAULT_MAX_COMPONENT_LEN)?;
        let file_name = format!("{name}{STAGED_SUFFIX}");
        if file_name.len() > DEFAULT_MAX_COMPONENT_LEN {
            return Err(PathError::with_detail(
                PathRejection::ComponentTooLong,
                format!("{} bytes", file_name.len()),
            ));
        }
        let policy = self.policy();
        let validated = canonicalize_and_validate(&policy, &self.path.join(&file_name))?;

        let file = open_private_new(validated.as_path()).map_err(|err| PathError::from_io(&err))?;
        Ok(StagedFile {
            file: Some(file),
            path: validated,
            staging_dir: self.path.clone(),
            committed: false,
        })
    }

    /// Remove staging directories left behind by earlier runs.
    ///
    /// Only directories whose name matches `<process_prefix>-<pid>-<32 hex>`
    /// exactly, that are real directories (not symlinks), that belong to
    /// another process, and whose mtime is at least `older_than_secs` before
    /// `now_unix` are removed. Everything else — a user's own folder, a fresh
    /// directory from a concurrently starting instance, a symlink someone
    /// planted — is counted and left alone.
    ///
    /// `now_unix` is passed in rather than read from the clock so the grace
    /// period is testable.
    pub fn reconcile_abandoned(
        base: &Path,
        process_prefix: &str,
        older_than_secs: u64,
        now_unix: i64,
    ) -> Result<ReconcileReport, PathError> {
        check_prefix(process_prefix)?;
        let mut report = ReconcileReport::default();
        let entries = match fs::read_dir(base) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(report),
            Err(err) => return Err(PathError::from_io(&err)),
        };
        let cutoff = now_unix.saturating_sub(i64::try_from(older_than_secs).unwrap_or(i64::MAX));
        let self_pid = std::process::id();

        for entry in entries {
            let Ok(entry) = entry else {
                report.failed += 1;
                continue;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                report.skipped_foreign += 1;
                continue;
            };
            let Some(pid) = parse_staging_name(&name, process_prefix) else {
                report.skipped_foreign += 1;
                continue;
            };
            // symlink_metadata, never metadata: a symlink named like one of our
            // staging directories must not be followed, let alone deleted
            // recursively.
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                report.failed += 1;
                continue;
            };
            if !metadata.file_type().is_dir() {
                report.skipped_foreign += 1;
                continue;
            }
            if pid == self_pid {
                report.skipped_own += 1;
                continue;
            }
            let modified = metadata.modified().ok().map_or(i64::MAX, unix_seconds);
            if modified > cutoff {
                report.skipped_recent += 1;
                continue;
            }
            // std's remove_dir_all walks with openat/unlinkat on unix and
            // refuses to descend through a symlink, so a link planted inside an
            // abandoned directory cannot redirect the deletion.
            if fs::remove_dir_all(entry.path()).is_ok() {
                report.removed += 1;
            } else {
                report.failed += 1;
            }
        }
        Ok(report)
    }
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        // Best effort: a failure here is reconciled at the next startup.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// What [`PrivateTempDir::reconcile_abandoned`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Abandoned staging directories removed.
    pub removed: usize,
    /// Entries that are not ours: wrong name shape, or not a directory.
    pub skipped_foreign: usize,
    /// Ours, but inside the grace period.
    pub skipped_recent: usize,
    /// Ours, and owned by this process.
    pub skipped_own: usize,
    /// Entries we could not inspect or remove.
    pub failed: usize,
}

/// A `*.part` file inside a [`PrivateTempDir`], waiting to be committed.
///
/// Dropping it without committing removes the partial file.
#[derive(Debug)]
pub struct StagedFile {
    file: Option<File>,
    path: ValidatedPath,
    staging_dir: PathBuf,
    committed: bool,
}

impl StagedFile {
    /// The staging path. Validated, inside the private directory.
    pub fn path(&self) -> &ValidatedPath {
        &self.path
    }

    /// The open file handle, for callers that need `Seek` or `set_len`.
    pub fn file_mut(&mut self) -> Option<&mut File> {
        self.file.as_mut()
    }

    /// Flush and `fsync` the staged bytes without committing. Useful between
    /// checkpoints of a long download (§14.3).
    pub fn sync(&mut self) -> Result<(), PathError> {
        match &mut self.file {
            Some(file) => {
                file.flush().map_err(|err| PathError::from_io(&err))?;
                file.sync_all().map_err(|err| PathError::from_io(&err))
            }
            None => Err(PathError::new(PathRejection::Io)),
        }
    }

    /// Durably move the staged bytes to `target`.
    ///
    /// 1. `fsync` the file, so the bytes are on the medium before any directory
    ///    entry points at them;
    /// 2. compare the volume of the staging directory and of the target's
    ///    parent — a cross-volume move is **not** a rename and cannot be
    ///    atomic, so it is refused with [`PathRejection::CrossVolumeRename`]
    ///    instead of being silently turned into a copy;
    /// 3. refuse a target that already exists as a symlink or directory;
    /// 4. `rename`;
    /// 5. `fsync` the target's **directory**, so the new entry survives a power
    ///    loss (unix only — see [`fsync_dir`]);
    /// 6. apply restrictive permissions to the committed file.
    pub fn commit_by_rename(mut self, target: &ValidatedPath) -> Result<ValidatedPath, PathError> {
        let Some(mut file) = self.file.take() else {
            return Err(PathError::new(PathRejection::Io));
        };
        file.flush().map_err(|err| PathError::from_io(&err))?;
        file.sync_all().map_err(|err| PathError::from_io(&err))?;
        drop(file);

        let parent = target
            .as_path()
            .parent()
            .ok_or_else(|| PathError::new(PathRejection::NoExistingAncestor))?;
        let staging_volume = volume_id(&self.staging_dir)?;
        let target_volume = volume_id(parent)?;
        require_same_volume(staging_volume, target_volume)?;

        match fs::symlink_metadata(target.as_path()) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(PathError::new(PathRejection::SymlinkEscapesRoot))
            }
            Ok(metadata) if metadata.is_dir() => {
                return Err(PathError::with_detail(PathRejection::Io, "IsADirectory"))
            }
            // An existing regular file is the content-addressed same-bytes case
            // and the crash-recovery retry case; replacing it is the point of a
            // rename commit.
            Ok(_) | Err(_) => {}
        }

        fs::rename(self.path.as_path(), target.as_path())
            .map_err(|err| PathError::from_io(&err))?;
        self.committed = true;
        fsync_dir(parent)?;
        set_restrictive_permissions(target)?;
        Ok(target.clone())
    }

    /// Remove the partial file now instead of at drop.
    pub fn abandon(mut self) -> Result<(), PathError> {
        self.file = None;
        let result = fs::remove_file(self.path.as_path()).map_err(|err| PathError::from_io(&err));
        self.committed = true;
        result
    }
}

impl Write for StagedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.file {
            Some(file) => file.write(buf),
            None => Err(io::Error::other("staged file already committed")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.committed {
            self.file = None;
            let _ = fs::remove_file(self.path.as_path());
        }
    }
}

/// `fsync` a directory so a rename into it is durable.
///
/// On unix this opens the directory and calls `fsync`, which is the documented
/// way to make a directory entry survive a crash. On Windows std cannot open a
/// directory handle (`FILE_FLAG_BACKUP_SEMANTICS` is not exposed), so this is a
/// no-op there: the durability we rely on is NTFS metadata journalling, and we
/// do not claim the stronger unix guarantee on that platform.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<(), PathError> {
    let handle = File::open(dir).map_err(|err| PathError::from_io(&err))?;
    handle.sync_all().map_err(|err| PathError::from_io(&err))
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> Result<(), PathError> {
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new()
        .mode(super::perms::DIRECTORY_MODE)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().create(path)
}

#[cfg(unix)]
fn open_private_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(super::perms::FILE_MODE)
        .open(path)
}

#[cfg(not(unix))]
fn open_private_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn check_prefix(prefix: &str) -> Result<(), PathError> {
    if prefix.is_empty() || prefix.len() > 32 {
        return Err(PathError::with_detail(
            PathRejection::ComponentTooLong,
            format!("{} bytes", prefix.len()),
        ));
    }
    if !prefix
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(PathError::with_component(
            PathRejection::ReservedCharacter,
            prefix,
        ));
    }
    Ok(())
}

/// `<prefix>-<pid>-<32 lowercase hex>` -> pid.
fn parse_staging_name(name: &str, prefix: &str) -> Option<u32> {
    let rest = name.strip_prefix(prefix)?.strip_prefix('-')?;
    let (pid, token) = rest.split_once('-')?;
    if token.len() != TOKEN_HEX_LEN
        || !token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse::<u32>().ok()
}

fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => i64::try_from(delta.as_secs()).unwrap_or(i64::MAX),
        Err(err) => i64::try_from(err.duration().as_secs()).map_or(i64::MIN, i64::wrapping_neg),
    }
}

/// 16 bytes of unpredictability rendered as hex.
///
/// On unix this is the kernel CSPRNG via `/dev/urandom`. If that cannot be
/// opened — a sandbox without `/dev`, or Windows — we fall back to a SHA-256 of
/// the process id, the current time at nanosecond resolution, the per-process
/// `RandomState` seed (itself OS-seeded) and a monotonically increasing
/// counter. The fallback is unique but it is *not* a CSPRNG, which is why the
/// security property of a staging directory is its `0o700` mode, not the
/// secrecy of its name.
fn random_token() -> String {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut urandom) = File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if urandom.read_exact(&mut buf).is_ok() {
                return hex::encode(buf);
            }
        }
    }
    hex::encode(fallback_entropy())
}

fn fallback_entropy() -> [u8; 16] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut hasher = RandomState::new().build_hasher();
    std::process::id().hash(&mut hasher);
    COUNTER.fetch_add(1, Ordering::Relaxed).hash(&mut hasher);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |delta| delta.as_nanos());
    nanos.hash(&mut hasher);
    std::ptr::from_ref(&COUNTER).addr().hash(&mut hasher);

    let digest = {
        use sha2::Digest;
        let mut sha = sha2::Sha256::new();
        sha.update(hasher.finish().to_le_bytes());
        sha.update(nanos.to_le_bytes());
        sha.finalize()
    };
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}
