//! "Import a model file I already have" (issue #303).
//!
//! The air-gapped path: the user hands over a weights file they obtained some
//! other way, and it is installed only when its size and SHA-256 are exactly
//! those of a catalogue entry. No byte source is involved, so there is no
//! egress decision to make and it works the same with Local-only Mode on.
//!
//! The user's file is read, never moved or altered: the bytes are copied into
//! the staging directory while they are hashed, and only a copy whose digest
//! matches is renamed into the model root. Any other outcome removes the
//! copy, so a refused import leaves nothing behind.

use std::io::{Read, Write};
use std::path::Path;

use plaudon_domain::media::Sha256Digest;
use sha2::{Digest, Sha256};

use super::manifest::{ModelEntry, ModelId, ModelManifest};
use super::registry::{InstallOrigin, InstalledModel, ModelRegistry};
use super::ModelError;

/// How much of the file is read at a time.
const CHUNK_BYTES: usize = 1024 * 1024;

/// Install `file` as the catalogue entry whose checksum it matches.
///
/// With `expected` the file must be that entry; without it the file is
/// matched against every entry of the same size.
pub fn import_model_file(
    registry: &mut ModelRegistry,
    catalogue: &ModelManifest,
    file: &Path,
    expected: Option<&ModelId>,
    keep_offline: bool,
) -> Result<InstalledModel, ModelError> {
    let metadata = std::fs::metadata(file).map_err(|source| ModelError::io(file, &source))?;
    if !metadata.is_file() {
        let source = std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file");
        return Err(ModelError::io(file, &source));
    }
    let size = metadata.len();

    // Size first: it is free, and it narrows the candidates before a byte is
    // read. A named model whose size is wrong is refused without hashing.
    let candidates: Vec<&ModelEntry> = match expected {
        Some(id) => {
            let entry = catalogue
                .find(id)
                .ok_or_else(|| ModelError::NotInManifest { id: id.to_string() })?;
            if entry.size_bytes != size {
                return Err(ModelError::SizeMismatch {
                    expected: entry.size_bytes,
                    observed: size,
                });
            }
            vec![entry]
        }
        None => catalogue
            .models
            .iter()
            .filter(|entry| entry.size_bytes == size)
            .collect(),
    };
    if candidates.is_empty() {
        return Err(ModelError::NoCatalogueMatch {
            observed_bytes: size,
        });
    }

    let staging_dir = registry.staging_dir();
    std::fs::create_dir_all(&staging_dir)
        .map_err(|source| ModelError::io(&staging_dir, &source))?;
    let staging_path = staging_dir.join("import.part");
    let digest = match copy_hashing(file, &staging_path, size) {
        Ok(digest) => digest,
        Err(error) => {
            let _ = std::fs::remove_file(&staging_path);
            return Err(error);
        }
    };

    let Some(entry) = candidates.iter().find(|entry| entry.sha256 == digest) else {
        let _ = std::fs::remove_file(&staging_path);
        return Err(match expected {
            Some(_) => ModelError::ChecksumMismatch {
                expected: candidates[0].sha256.to_hex(),
                observed: digest.to_hex(),
            },
            None => ModelError::NoCatalogueMatch {
                observed_bytes: size,
            },
        });
    };

    let install_dir = match registry.prepare_install_dir(entry) {
        Ok(dir) => dir,
        Err(error) => {
            let _ = std::fs::remove_file(&staging_path);
            return Err(error);
        }
    };
    let final_path = install_dir.join(&entry.file_name);
    std::fs::rename(&staging_path, &final_path).map_err(|source| {
        let _ = std::fs::remove_file(&staging_path);
        ModelError::io(&final_path, &source)
    })?;

    let model = InstalledModel {
        id: entry.id.clone(),
        engine: entry.engine.clone(),
        version: entry.version.clone(),
        display_name: entry.display_name.clone(),
        relative_path: entry.relative_path(),
        sha256: entry.sha256,
        byte_length: entry.size_bytes,
        license_spdx: entry.license.spdx_id.clone(),
        license_text_path: None,
        keep_offline,
        installed_from: InstallOrigin::ImportedFile,
    };
    registry.record(model.clone())?;
    Ok(model)
}

/// Copy `from` to `to`, returning the SHA-256 of what was copied.
///
/// Stops at `expected_bytes`: a file that grows while it is read must not be
/// able to fill the disk.
fn copy_hashing(from: &Path, to: &Path, expected_bytes: u64) -> Result<Sha256Digest, ModelError> {
    let mut reader = std::fs::File::open(from).map_err(|source| ModelError::io(from, &source))?;
    let mut writer = std::fs::File::create(to).map_err(|source| ModelError::io(to, &source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let mut copied = 0u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| ModelError::io(from, &source))?;
        if read == 0 {
            break;
        }
        copied += read as u64;
        if copied > expected_bytes {
            return Err(ModelError::SizeMismatch {
                expected: expected_bytes,
                observed: copied,
            });
        }
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|source| ModelError::io(to, &source))?;
    }
    writer
        .sync_all()
        .map_err(|source| ModelError::io(to, &source))?;
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(Sha256Digest::from_bytes(digest))
}
