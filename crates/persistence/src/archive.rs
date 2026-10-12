//! The content-addressed immutable media archive (spec 13.4, 6.3B).
//!
//! Every stored path is derived from content identity alone. An original
//! filename is metadata; it never reaches the filesystem. Reads verify the
//! digest and byte length before returning bytes, so a silently corrupted or
//! swapped blob fails with [`PersistenceError::IntegrityFailed`] instead of
//! being served.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use plaudon_domain::ids::ImportIntentId;
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use sha2::{Digest, Sha256};

use crate::error::{PersistenceError, PersistenceResult};

/// Extensions the archive is willing to write.
///
/// Anything else is stored as `.bin`: the extension is a convenience for the
/// user's file manager, never a trust signal.
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "mp4", "aac", "ogg", "opus", "flac", "caf", "asr", "bin",
];

/// Extension used when the sniffed container has no allowlisted extension.
pub const FALLBACK_EXTENSION: &str = "bin";

/// Reduce a caller-supplied extension to a safe, allowlisted value.
///
/// The input is the *sniffed* container's conventional extension or the
/// original filename's extension; either way, only an exact allowlist match
/// survives, lowercased.
#[must_use]
pub fn sanitize_extension(candidate: &str) -> &'static str {
    let trimmed = candidate
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    ALLOWED_EXTENSIONS
        .iter()
        .copied()
        .find(|allowed| *allowed == trimmed)
        .unwrap_or(FALLBACK_EXTENSION)
}

/// The canonical archive-relative path for a blob.
///
/// `media/sha256/<first-two-hex>/<hash>.<ext>` (spec 13.4). The result is
/// always a two-segment relative path under `media/sha256`, whatever the
/// caller passes as `extension`.
#[must_use]
pub fn relative_path(identity: &MediaIdentity, extension: &str) -> String {
    let hex = identity.sha256.to_hex();
    format!(
        "media/sha256/{}/{}.{}",
        &hex[..2],
        hex,
        sanitize_extension(extension)
    )
}

/// A managed media archive rooted at the user's application data directory.
#[derive(Debug, Clone)]
pub struct MediaArchive {
    root: PathBuf,
}

/// Outcome of relocating an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocationReport {
    /// Number of blobs copied and verified.
    pub blobs_moved: u64,
    /// Total bytes copied.
    pub bytes_moved: u64,
    /// The new archive root.
    pub new_root: PathBuf,
}

impl MediaArchive {
    /// Maximum bytes read into memory when hashing or verifying in one chunk.
    const CHUNK: usize = 64 * 1024;

    /// Open or create an archive under `root`.
    pub fn open(root: impl AsRef<Path>) -> PersistenceResult<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(root.join("media/sha256"))?;
        std::fs::create_dir_all(root.join("temp/imports"))?;
        Ok(Self { root })
    }

    /// The archive root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding in-flight staging files.
    #[must_use]
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("temp/imports")
    }

    /// Allocate a staging path for an import intent.
    ///
    /// The name is derived from the intent identity, never from a source
    /// filename, and always lands inside the archive's own staging directory
    /// so staging and archive share a volume.
    #[must_use]
    pub fn staging_path(&self, intent: ImportIntentId) -> PathBuf {
        self.staging_dir().join(format!("{intent}.part"))
    }

    /// Resolve an archive-relative path to an absolute path inside the root.
    ///
    /// Rejects absolute paths, parent traversal, drive-relative prefixes and
    /// any component that is not a plain name.
    pub fn absolute(&self, relative: &str) -> PersistenceResult<PathBuf> {
        if relative.is_empty() {
            return Err(PersistenceError::ArchivePathRejected("empty path".into()));
        }
        if relative.contains('\0') {
            return Err(PersistenceError::ArchivePathRejected(
                "path contains NUL".into(),
            ));
        }
        let candidate = Path::new(relative);
        for component in candidate.components() {
            match component {
                Component::Normal(_) => {}
                Component::CurDir => {
                    return Err(PersistenceError::ArchivePathRejected(
                        "path contains a current-directory component".into(),
                    ))
                }
                Component::ParentDir => {
                    return Err(PersistenceError::ArchivePathRejected(
                        "path escapes the archive root".into(),
                    ))
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PersistenceError::ArchivePathRejected(
                        "path is not archive-relative".into(),
                    ))
                }
            }
        }
        Ok(self.root.join(candidate))
    }

    /// Whether a blob exists at `relative`.
    pub fn exists(&self, relative: &str) -> PersistenceResult<bool> {
        Ok(self.absolute(relative)?.is_file())
    }

    /// Stream `source` into a staging file, returning its identity.
    ///
    /// The hash is computed while writing, with a bounded buffer, so a forged
    /// declared length cannot drive allocation. The file and its directory are
    /// flushed before returning (spec 13.3A steps 1 and 2).
    pub fn stage_from_reader(
        &self,
        staging_path: &Path,
        source: &mut dyn Read,
        observed_codec: &str,
    ) -> PersistenceResult<MediaIdentity> {
        self.assert_inside_staging(staging_path)?;
        let mut file = std::fs::File::create(staging_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; Self::CHUNK];
        let mut total: u64 = 0;
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            std::io::Write::write_all(&mut file, &buffer[..read])?;
            total += read as u64;
        }
        file.sync_all()?;
        drop(file);
        sync_directory(self.staging_dir().as_path())?;

        Ok(MediaIdentity::new(
            Sha256Digest::from_bytes(hasher.finalize().into()),
            total,
            observed_codec,
        ))
    }

    /// Hash an existing file without loading it into memory.
    pub fn digest_file(path: &Path) -> PersistenceResult<(Sha256Digest, u64)> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; Self::CHUNK];
        let mut total: u64 = 0;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            total += read as u64;
        }
        Ok((Sha256Digest::from_bytes(hasher.finalize().into()), total))
    }

    /// Atomically move a staged file into the archive.
    ///
    /// Fails with [`PersistenceError::CrossVolumeCommit`] rather than falling
    /// back to a copy that only looks atomic.
    pub fn commit_staged(&self, staging_path: &Path, relative: &str) -> PersistenceResult<PathBuf> {
        self.assert_inside_staging(staging_path)?;
        let target = self.absolute(relative)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(error) = std::fs::rename(staging_path, &target) {
            // `CrossesDevices` is the only rename failure we can meaningfully
            // reclassify; everything else is a real I/O failure.
            if error.kind() == std::io::ErrorKind::CrossesDevices {
                return Err(PersistenceError::CrossVolumeCommit);
            }
            return Err(PersistenceError::from(error));
        }
        if let Some(parent) = target.parent() {
            sync_directory(parent)?;
        }
        Ok(target)
    }

    /// Read a blob, verifying it against the identity it is recorded under.
    pub fn read_verified(
        &self,
        relative: &str,
        expected: &MediaIdentity,
    ) -> PersistenceResult<Vec<u8>> {
        let path = self.resolve_existing(relative)?;
        let bytes = std::fs::read(&path)?;
        if bytes.len() as u64 != expected.byte_length || Sha256Digest::of(&bytes) != expected.sha256
        {
            return Err(PersistenceError::IntegrityFailed);
        }
        Ok(bytes)
    }

    /// Verify a blob in place without returning its bytes.
    pub fn verify(&self, relative: &str, expected: &MediaIdentity) -> PersistenceResult<()> {
        let path = self.resolve_existing(relative)?;
        let (digest, length) = Self::digest_file(&path)?;
        if digest != expected.sha256 || length != expected.byte_length {
            return Err(PersistenceError::IntegrityFailed);
        }
        Ok(())
    }

    /// Staging files that no longer belong to a live intent.
    ///
    /// Only files directly inside our own staging directory are considered,
    /// and only `*.part` files we could have written ourselves.
    pub fn orphan_staging_files(
        &self,
        live_intents: &[ImportIntentId],
    ) -> PersistenceResult<Vec<PathBuf>> {
        let mut orphans = Vec::new();
        let dir = self.staging_dir();
        if !dir.is_dir() {
            return Ok(orphans);
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("part") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(intent) = stem.parse::<ImportIntentId>() else {
                // Not a name this application produces: leave it alone.
                continue;
            };
            if !live_intents.contains(&intent) {
                orphans.push(path);
            }
        }
        orphans.sort();
        Ok(orphans)
    }

    /// Relocate the archive to `new_root`, verifying every blob as it is copied.
    ///
    /// The old archive is left intact until every blob has been copied and
    /// verified, so an interruption leaves one of the two archives fully
    /// valid.
    pub fn relocate_to(&self, new_root: impl AsRef<Path>) -> PersistenceResult<RelocationReport> {
        let new_root = new_root.as_ref().to_path_buf();
        let blobs = self.list_blobs()?;
        let required: u64 = blobs.iter().map(|(_, length)| *length).sum();
        let available = available_space(&new_root)?;
        if available < required {
            return Err(PersistenceError::StorageFull);
        }

        let destination = Self::open(&new_root)?;
        let mut moved = 0u64;
        for (relative, _) in &blobs {
            let source = self.absolute(relative)?;
            let (digest, length) = Self::digest_file(&source)?;
            let target = destination.absolute(relative)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source, &target)?;
            let (copied_digest, copied_length) = Self::digest_file(&target)?;
            if copied_digest != digest || copied_length != length {
                std::fs::remove_file(&target)?;
                return Err(PersistenceError::IntegrityFailed);
            }
            moved += 1;
        }
        Ok(RelocationReport {
            blobs_moved: moved,
            bytes_moved: required,
            new_root,
        })
    }

    /// Every blob in the archive as `(relative path, byte length)`.
    pub fn list_blobs(&self) -> PersistenceResult<Vec<(String, u64)>> {
        let mut out = Vec::new();
        let media = self.root.join("media/sha256");
        if !media.is_dir() {
            return Ok(out);
        }
        for prefix in std::fs::read_dir(&media)? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(prefix.path())? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(&self.root)
                    .map_err(|_| PersistenceError::ArchivePathRejected("blob outside root".into()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((relative, entry.metadata()?.len()));
            }
        }
        out.sort();
        Ok(out)
    }

    fn resolve_existing(&self, relative: &str) -> PersistenceResult<PathBuf> {
        let path = self.absolute(relative)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(PersistenceError::ArchivePathRejected(
                "archive entries must be regular files, not symlinks".into(),
            ));
        }
        if !metadata.is_file() {
            return Err(PersistenceError::ArchivePathRejected(
                "archive entry is not a regular file".into(),
            ));
        }
        // Defence in depth: a symlinked ancestor would let a path inside the
        // root resolve outside it.
        let canonical_root = std::fs::canonicalize(&self.root)?;
        let canonical = std::fs::canonicalize(&path)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(PersistenceError::ArchivePathRejected(
                "resolved path escapes the archive root".into(),
            ));
        }
        Ok(path)
    }

    fn assert_inside_staging(&self, path: &Path) -> PersistenceResult<()> {
        let staging = self.staging_dir();
        if path.parent() != Some(staging.as_path()) {
            return Err(PersistenceError::ArchivePathRejected(
                "staging files must live in the archive staging directory".into(),
            ));
        }
        Ok(())
    }
}

/// Flush a directory entry so a rename or create survives a power loss.
///
/// Windows has no directory fsync; the rename is already ordered there.
fn sync_directory(path: &Path) -> PersistenceResult<()> {
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(path)?;
        dir.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Free space available at `path`, creating it if needed.
fn available_space(path: &Path) -> PersistenceResult<u64> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| PersistenceError::ArchivePathRejected("path contains NUL".into()))?;
        // SAFETY: `c_path` is a valid NUL-terminated path, and `stat` is a
        // correctly sized out-parameter that `statvfs` only writes to. The
        // zeroed value is a valid `statvfs` because every field is an
        // integer.
        #[allow(unsafe_code)]
        let (rc, stat) = unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            (libc::statvfs(c_path.as_ptr(), &raw mut stat), stat)
        };
        if rc != 0 {
            // We cannot establish free space; claiming "full" would block a
            // legitimate relocation, and the copy itself still fails loudly.
            return Ok(u64::MAX);
        }
        // `statvfs::f_bavail` is `fsblkcnt_t`, which is 32-bit on macOS and
        // 64-bit on Linux, so the widening cannot be written once: `u64::from`
        // is required on one and a no-op clippy rejects on the other. Both
        // arms produce the same `u64`, and `f_frsize` is `c_ulong` (64-bit on
        // every target we build) on both.
        #[cfg(target_os = "macos")]
        let available = u64::from(stat.f_bavail);
        #[cfg(not(target_os = "macos"))]
        let available = stat.f_bavail;
        Ok(available.saturating_mul(stat.f_frsize))
    }
    #[cfg(not(unix))]
    {
        // Without a platform call we must not claim to know the free space;
        // the copy itself still fails loudly if the volume runs out.
        Ok(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive() -> (tempfile::TempDir, MediaArchive) {
        let dir = tempfile::tempdir().unwrap();
        let archive = MediaArchive::open(dir.path()).unwrap();
        (dir, archive)
    }

    fn identity(bytes: &[u8]) -> MediaIdentity {
        MediaIdentity::of_bytes(bytes, "pcm_s16le")
    }

    #[test]
    fn a_digest_always_maps_to_one_canonical_path() {
        let id = identity(b"audio");
        let hex = id.sha256.to_hex();
        assert_eq!(
            relative_path(&id, "wav"),
            format!("media/sha256/{}/{hex}.wav", &hex[..2])
        );
        assert_eq!(relative_path(&id, "wav"), relative_path(&id, ".WAV"));
    }

    #[test]
    fn hostile_original_names_cannot_influence_the_stored_path() {
        let id = identity(b"audio");
        let long_name = "really-long-".repeat(40);
        let hostile = [
            "../../../../etc/passwd",
            "/etc/passwd",
            "C:\\Windows\\System32\\config",
            "CON",
            "NUL",
            "evil.wav.exe",
            "..",
            "",
            "wav\0",
            long_name.as_str(),
        ];
        let expected_prefix = format!("media/sha256/{}/", &id.sha256.to_hex()[..2]);
        for name in hostile {
            let path = relative_path(&id, name);
            assert!(
                path.starts_with(&expected_prefix),
                "{name:?} produced {path}"
            );
            assert_eq!(path.rsplit_once('.').map(|(_, ext)| ext), Some("bin"));
            assert_eq!(path.matches('/').count(), 3);
        }
    }

    #[test]
    fn only_allowlisted_extensions_survive() {
        for allowed in ALLOWED_EXTENSIONS {
            assert_eq!(sanitize_extension(allowed), *allowed);
        }
        for rejected in ["exe", "sh", "wav.exe", "", "..", "WAVE", "🎵"] {
            assert_eq!(
                sanitize_extension(rejected),
                FALLBACK_EXTENSION,
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused() {
        let (_dir, archive) = archive();
        for hostile in [
            "../outside.wav",
            "media/../../outside.wav",
            "/absolute.wav",
            "./media/x.wav",
            "",
        ] {
            assert!(
                matches!(
                    archive.absolute(hostile),
                    Err(PersistenceError::ArchivePathRejected(_))
                ),
                "{hostile:?} was not rejected"
            );
        }
        assert!(archive.absolute("media/sha256/ab/file.wav").is_ok());
    }

    #[test]
    fn staging_outside_the_staging_directory_is_refused() {
        let (dir, archive) = archive();
        let outside = dir.path().join("elsewhere.part");
        let mut source = std::io::Cursor::new(b"audio".to_vec());
        assert!(matches!(
            archive.stage_from_reader(&outside, &mut source, "wav"),
            Err(PersistenceError::ArchivePathRejected(_))
        ));
    }

    #[test]
    fn staging_then_committing_produces_a_verifiable_blob() {
        let (_dir, archive) = archive();
        let intent = ImportIntentId::new();
        let staging = archive.staging_path(intent);
        let mut source = std::io::Cursor::new(b"audio bytes".to_vec());
        let id = archive
            .stage_from_reader(&staging, &mut source, "wav")
            .unwrap();

        assert_eq!(id, identity(b"audio bytes"));
        let relative = relative_path(&id, "wav");
        archive.commit_staged(&staging, &relative).unwrap();

        assert!(
            !staging.exists(),
            "the staging file is consumed by the rename"
        );
        assert!(archive.exists(&relative).unwrap());
        assert_eq!(
            archive.read_verified(&relative, &id).unwrap(),
            b"audio bytes"
        );
        archive.verify(&relative, &id).unwrap();
    }

    #[test]
    fn a_tampered_blob_fails_with_integrity_failed_instead_of_returning_data() {
        let (_dir, archive) = archive();
        let intent = ImportIntentId::new();
        let staging = archive.staging_path(intent);
        let mut source = std::io::Cursor::new(b"original".to_vec());
        let id = archive
            .stage_from_reader(&staging, &mut source, "wav")
            .unwrap();
        let relative = relative_path(&id, "wav");
        archive.commit_staged(&staging, &relative).unwrap();

        std::fs::write(archive.absolute(&relative).unwrap(), b"tampered").unwrap();
        assert!(matches!(
            archive.read_verified(&relative, &id),
            Err(PersistenceError::IntegrityFailed)
        ));
        assert!(matches!(
            archive.verify(&relative, &id),
            Err(PersistenceError::IntegrityFailed)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_archive_entry_is_refused() {
        let (dir, archive) = archive();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"not audio").unwrap();

        let id = identity(b"not audio");
        let relative = relative_path(&id, "wav");
        let target = archive.absolute(&relative).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&secret, &target).unwrap();

        assert!(matches!(
            archive.read_verified(&relative, &id),
            Err(PersistenceError::ArchivePathRejected(_))
        ));
    }

    #[test]
    fn orphan_detection_only_claims_our_own_staging_files() {
        let (_dir, archive) = archive();
        let live = ImportIntentId::new();
        let dead = ImportIntentId::new();
        std::fs::write(archive.staging_path(live), b"live").unwrap();
        std::fs::write(archive.staging_path(dead), b"dead").unwrap();
        std::fs::write(archive.staging_dir().join("user-notes.txt"), b"not ours").unwrap();
        std::fs::write(
            archive.staging_dir().join("something-else.part"),
            b"not ours",
        )
        .unwrap();

        let orphans = archive.orphan_staging_files(&[live]).unwrap();
        assert_eq!(orphans, vec![archive.staging_path(dead)]);
    }

    #[test]
    fn relocation_verifies_every_blob_and_leaves_the_source_valid() {
        let (_dir, archive) = archive();
        let mut stored = Vec::new();
        for content in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            let intent = ImportIntentId::new();
            let staging = archive.staging_path(intent);
            let mut source = std::io::Cursor::new(content.to_vec());
            let id = archive
                .stage_from_reader(&staging, &mut source, "wav")
                .unwrap();
            let relative = relative_path(&id, "wav");
            archive.commit_staged(&staging, &relative).unwrap();
            stored.push((relative, id));
        }

        let destination = tempfile::tempdir().unwrap();
        let report = archive.relocate_to(destination.path()).unwrap();
        assert_eq!(report.blobs_moved, 3);

        let moved = MediaArchive::open(destination.path()).unwrap();
        for (relative, id) in &stored {
            moved.verify(relative, id).unwrap();
            archive.verify(relative, id).unwrap();
        }
    }

    #[test]
    fn relocation_refuses_when_the_destination_cannot_hold_the_archive() {
        let (_dir, archive) = archive();
        let intent = ImportIntentId::new();
        let staging = archive.staging_path(intent);
        let mut source = std::io::Cursor::new(vec![0u8; 1024]);
        let id = archive
            .stage_from_reader(&staging, &mut source, "wav")
            .unwrap();
        archive
            .commit_staged(&staging, &relative_path(&id, "wav"))
            .unwrap();

        // A destination that cannot be created is reported as an I/O failure,
        // never as a silently successful relocation. The unwritable path is a
        // child of a regular file, which is refused with `ENOTDIR` whoever
        // asks: an absolute path outside the test's own directories would only
        // be unwritable for a non-root user, and the containerised runner is
        // root.
        let elsewhere = tempfile::tempdir().unwrap();
        let blocker = elsewhere.path().join("a-file-not-a-directory");
        std::fs::write(&blocker, b"not a parent").unwrap();
        let impossible = blocker.join("destination");
        assert!(matches!(
            archive.relocate_to(&impossible),
            Err(PersistenceError::Io(_))
        ));
        assert!(
            !impossible.exists(),
            "nothing was created at {impossible:?}"
        );
    }

    #[test]
    fn listing_blobs_round_trips_relative_paths() {
        let (_dir, archive) = archive();
        let intent = ImportIntentId::new();
        let staging = archive.staging_path(intent);
        let mut source = std::io::Cursor::new(b"content".to_vec());
        let id = archive
            .stage_from_reader(&staging, &mut source, "mp3")
            .unwrap();
        let relative = relative_path(&id, "mp3");
        archive.commit_staged(&staging, &relative).unwrap();

        assert_eq!(archive.list_blobs().unwrap(), vec![(relative, 7u64)]);
    }
}
