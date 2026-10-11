//! The content-addressed media archive (spec §13.4, §13.3A durable import order).
//!
//! Layout under the library root:
//!
//! ```text
//! media/<sha[0..2]>/<sha[2..4]>/<sha>.<ext>
//! staging/<uuid>.part
//! ```
//!
//! Import order is: write a private staging file while hashing with a bounded
//! buffer, fsync the file and its directory, rename inside the same filesystem,
//! and only then commit the database row. The source file is never modified,
//! moved or deleted.

use std::path::{Path, PathBuf};

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::media::Sha256Digest;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt as _;

/// Size of the streaming buffer. The file is never loaded into memory.
pub const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// A staged file: fully written and fsynced, not yet part of the archive.
#[derive(Debug)]
pub struct StagedFile {
    /// Absolute path of the `.part` file.
    pub path: PathBuf,
    /// Digest of the bytes that were written.
    pub digest: Sha256Digest,
    /// Number of bytes written.
    pub byte_length: u64,
}

/// The archive directories.
#[derive(Debug, Clone)]
pub struct Archive {
    root: PathBuf,
}

impl Archive {
    /// Archive rooted at the library directory.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Archive-relative path of a blob (spec §13.4).
    #[must_use]
    pub fn relative_path(digest: Sha256Digest, extension: &str) -> String {
        let hex = digest.to_hex();
        format!("media/{}/{}/{hex}.{extension}", &hex[0..2], &hex[2..4])
    }

    /// Absolute path of an archive-relative path.
    #[must_use]
    pub fn absolute(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Whether a blob is already on disk.
    #[must_use]
    pub fn contains(&self, relative: &str) -> bool {
        self.absolute(relative).is_file()
    }

    /// Copy `source` into staging, hashing while streaming.
    pub async fn stage(&self, source: &Path) -> Result<StagedFile, AppError> {
        let staging_dir = self.root.join("staging");
        tokio::fs::create_dir_all(&staging_dir).await.map_err(|err| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot create the staging directory: {err}"),
            )
        })?;
        let path = staging_dir.join(format!("{}.part", uuid::Uuid::new_v4()));

        let mut input = tokio::fs::File::open(source).await.map_err(|err| {
            AppError::new(
                AppErrorCode::ArchivePathRejected,
                format!("cannot open the source file ({}): {err}", source.display()),
            )
        })?;
        let mut output = tokio::fs::File::create(&path).await.map_err(|err| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot create the staging file: {err}"),
            )
        })?;

        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
        let mut byte_length: u64 = 0;
        loop {
            let read = match input.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => read,
                Err(err) => {
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(AppError::new(
                        AppErrorCode::DownloadInterrupted,
                        format!("reading the source file failed: {err}"),
                    ));
                }
            };
            hasher.update(&buffer[..read]);
            byte_length += read as u64;
            if let Err(err) = tokio::io::AsyncWriteExt::write_all(&mut output, &buffer[..read]).await
            {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(write_error(&err));
            }
        }

        if let Err(err) = tokio::io::AsyncWriteExt::flush(&mut output).await {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(write_error(&err));
        }
        if let Err(err) = output.sync_all().await {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(write_error(&err));
        }
        drop(output);
        sync_dir(&staging_dir).await;

        let digest = Sha256Digest::from_bytes(hasher.finalize().into());
        Ok(StagedFile { path, digest, byte_length })
    }

    /// Move a staged file into the archive and return its relative path.
    ///
    /// Idempotent: when the blob is already present the staged copy is dropped
    /// and the existing blob is kept, so identical bytes never produce a second
    /// file on disk.
    pub async fn commit(
        &self,
        staged: &StagedFile,
        extension: &str,
    ) -> Result<String, AppError> {
        let relative = Self::relative_path(staged.digest, extension);
        let destination = self.absolute(&relative);
        if destination.is_file() {
            let _ = tokio::fs::remove_file(&staged.path).await;
            return Ok(relative);
        }
        let parent = destination.parent().ok_or_else(|| {
            AppError::new(AppErrorCode::StorageUnavailable, "archive path has no parent")
        })?;
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot create the archive directory: {err}"),
            )
        })?;
        tokio::fs::rename(&staged.path, &destination).await.map_err(|err| {
            if is_cross_device(&err) {
                AppError::new(
                    AppErrorCode::CrossVolumeCommit,
                    "staging and the archive are on different filesystems",
                )
            } else {
                AppError::new(
                    AppErrorCode::StorageUnavailable,
                    format!("cannot commit the staged file: {err}"),
                )
            }
        })?;
        sync_dir(parent).await;
        Ok(relative)
    }

    /// Remove a staged file that will not be committed.
    pub async fn discard(staged: StagedFile) {
        if let Err(err) = tokio::fs::remove_file(&staged.path).await {
            tracing::warn!(error = %err, "could not remove a staging file");
        }
    }
}

fn write_error(err: &std::io::Error) -> AppError {
    let code = if err.kind() == std::io::ErrorKind::StorageFull {
        AppErrorCode::StorageFull
    } else {
        AppErrorCode::StorageUnavailable
    };
    AppError::new(code, format!("writing the staging file failed: {err}"))
}

fn is_cross_device(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    const CROSS_DEVICE: i32 = 18;
    #[cfg(windows)]
    const CROSS_DEVICE: i32 = 17;
    #[cfg(not(any(unix, windows)))]
    const CROSS_DEVICE: i32 = i32::MIN;
    err.raw_os_error() == Some(CROSS_DEVICE)
}

async fn sync_dir(dir: &Path) {
    match tokio::fs::File::open(dir).await {
        Ok(handle) => {
            if let Err(err) = handle.sync_all().await {
                tracing::debug!(error = %err, "directory fsync is not supported here");
            }
        }
        Err(err) => tracing::debug!(error = %err, "could not open the directory for fsync"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relative_path_fans_out_on_the_first_four_hex_characters() {
        let digest = Sha256Digest::of(b"hello");
        let hex = digest.to_hex();
        let relative = Archive::relative_path(digest, "wav");
        assert_eq!(relative, format!("media/{}/{}/{hex}.wav", &hex[0..2], &hex[2..4]));
        assert!(!relative.contains(".."));
    }

    #[tokio::test]
    async fn staging_hashes_while_streaming_and_leaves_the_source_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.wav");
        let payload: Vec<u8> = (0..(COPY_BUFFER_BYTES * 3 + 17)).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&source, &payload).await.unwrap();

        let archive = Archive::new(dir.path().to_path_buf());
        let staged = archive.stage(&source).await.unwrap();

        assert_eq!(staged.byte_length, payload.len() as u64);
        assert_eq!(staged.digest, Sha256Digest::of(&payload));
        assert_eq!(tokio::fs::read(&source).await.unwrap(), payload, "source is untouched");
        assert!(staged.path.is_file());
    }

    #[tokio::test]
    async fn committing_identical_bytes_twice_keeps_one_blob() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.wav");
        tokio::fs::write(&source, b"identical bytes").await.unwrap();
        let archive = Archive::new(dir.path().to_path_buf());

        let first = archive.stage(&source).await.unwrap();
        let first_path = archive.commit(&first, "wav").await.unwrap();
        let second = archive.stage(&source).await.unwrap();
        let second_staging = second.path.clone();
        let second_path = archive.commit(&second, "wav").await.unwrap();

        assert_eq!(first_path, second_path);
        assert!(!second_staging.exists(), "the redundant staged copy is removed");
        let staged_files: Vec<_> =
            std::fs::read_dir(dir.path().join("staging")).unwrap().flatten().collect();
        assert!(staged_files.is_empty(), "staging is left clean");
        assert!(archive.contains(&first_path));
    }

    #[tokio::test]
    async fn discarding_removes_the_staged_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.ogg");
        tokio::fs::write(&source, b"bytes").await.unwrap();
        let archive = Archive::new(dir.path().to_path_buf());
        let staged = archive.stage(&source).await.unwrap();
        let path = staged.path.clone();
        Archive::discard(staged).await;
        assert!(!path.exists());
    }
}
