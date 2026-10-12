//! Redistributable offline model pack (spec §1.2(2), §24 — task F2.3).
//!
//! Goal §1.2(2) requires that a fresh, air-gapped installation can transcribe,
//! "by delivering an offline model artifact rather than assuming the user can
//! download model weights at first launch". §24 makes that a V1 gate, with the
//! artifact checksum and licence archived in the release evidence bundle.
//!
//! **Format.** A pack is a *directory tree*, not a compressed archive:
//!
//! ```text
//! <pack>/
//!   pack.json                     # PackManifest: entries, digests, licences
//!   models/<engine>/<version>/<file>
//!   licenses/<SPDX>.txt
//! ```
//!
//! Choosing an uncompressed tree is deliberate. The pack ships next to an
//! installer or on removable media where compression buys little, and it
//! removes the decompression-bomb class of attack entirely rather than
//! mitigating it: there is no expansion ratio to lie about. The remaining
//! archive risks — path traversal, symlinks, duplicate entries, a declared
//! size that does not match reality — are all checked in [`ModelPack::open`]
//! and [`ModelPack::verify`], **before** any byte is copied into the library.
//!
//! **Reproducibility.** [`PackManifest::artifact_digest`] folds the sorted
//! `(path, size, sha256)` triples into one digest, so the same inputs always
//! produce the same artifact checksum regardless of file-system ordering or
//! timestamps. That value is what `scripts/build-model-pack.sh` emits into the
//! evidence bundle.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use plaudon_domain::media::Sha256Digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::install::verify_file;
use super::manifest::{
    validate_relative_path, ManifestError, ModelEntry, ModelId, ModelSource,
    MAX_DECLARED_MODEL_BYTES,
};
use super::registry::{InstallOrigin, InstalledModel, ModelRegistry};
use super::ModelError;

/// Manifest file name inside a pack.
pub const PACK_MANIFEST_FILE: &str = "pack.json";

/// Pack manifest schema version.
pub const PACK_SCHEMA_VERSION: u32 = 1;

/// Ceiling on the total declared size of a pack.
pub const MAX_PACK_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// One file carried by the pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackFile {
    /// Path relative to the pack root. Validated: relative, no traversal, no
    /// separators other than `/`.
    pub path: String,
    /// Exact size in bytes.
    pub size_bytes: u64,
    /// Digest of the file's bytes.
    pub sha256: Sha256Digest,
}

/// The pack manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackManifest {
    /// Schema version of this document.
    pub schema_version: u32,
    /// Human-readable pack version, e.g. `2026.10.1`.
    pub pack_version: String,
    /// Models the pack installs, in full manifest form.
    pub models: Vec<ModelEntry>,
    /// Every file the pack carries, including licence texts.
    pub files: Vec<PackFile>,
}

impl PackManifest {
    /// Reproducible digest of the pack's content.
    ///
    /// Folds the sorted `(path, size, sha256)` triples plus the pack version,
    /// so two builds from identical inputs agree regardless of file system
    /// ordering, inode numbers or timestamps.
    #[must_use]
    pub fn artifact_digest(&self) -> Sha256Digest {
        let mut lines: Vec<String> = self
            .files
            .iter()
            .map(|file| format!("{} {} {}", file.path, file.size_bytes, file.sha256.to_hex()))
            .collect();
        lines.sort();
        let mut hasher = Sha256::new();
        hasher.update(self.pack_version.as_bytes());
        hasher.update([0u8]);
        for line in lines {
            hasher.update(line.as_bytes());
            hasher.update(*b"\n");
        }
        let digest: [u8; 32] = hasher.finalize().into();
        Sha256Digest::from_bytes(digest)
    }

    /// Total declared bytes.
    #[must_use]
    pub fn total_declared_bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0u64, |acc, file| acc.saturating_add(file.size_bytes))
    }

    fn file_for(&self, path: &str) -> Option<&PackFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

/// Why a pack was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    /// The manifest is missing or unreadable.
    #[error("pack manifest is missing or unreadable at {path}: {detail}")]
    ManifestUnreadable {
        /// Path that was tried.
        path: String,
        /// Non-secret description.
        detail: String,
    },
    /// The manifest does not parse.
    #[error("pack manifest could not be parsed: {0}")]
    Malformed(String),
    /// The manifest declares a schema version this build does not implement.
    #[error("pack schema version {found} is not supported (this build understands {supported})")]
    UnsupportedSchemaVersion {
        /// Declared version.
        found: u32,
        /// Supported version.
        supported: u32,
    },
    /// An entry path is unsafe.
    #[error("pack entry `{path}` is unsafe: {reason}")]
    UnsafeEntry {
        /// The offending path.
        path: String,
        /// Why it was refused.
        reason: &'static str,
    },
    /// Two entries declare the same path.
    #[error("pack declares `{path}` more than once")]
    DuplicateEntry {
        /// The repeated path.
        path: String,
    },
    /// A model refers to a file the manifest does not list.
    #[error("model `{id}` refers to `{path}`, which the pack does not declare")]
    MissingEntry {
        /// Model identifier.
        id: String,
        /// Path it referred to.
        path: String,
    },
    /// The pack declares more bytes than the ceiling allows.
    #[error("pack declares {declared} bytes, over the {limit} byte ceiling")]
    TooLarge {
        /// Declared total.
        declared: u64,
        /// The enforced ceiling.
        limit: u64,
    },
    /// A model's licence forbids redistribution, so it may not ship in a pack.
    #[error("model `{id}` ships under `{spdx_id}`, which is not marked redistributable")]
    NotRedistributable {
        /// Model identifier.
        id: String,
        /// Declared licence.
        spdx_id: String,
    },
    /// A declared licence text is absent from the pack.
    #[error("licence text `{path}` for model `{id}` is missing from the pack")]
    MissingLicenseText {
        /// Model identifier.
        id: String,
        /// Expected licence path.
        path: String,
    },
    /// A model entry inside the pack does not point at a pack file.
    #[error("model `{id}` is not sourced from the pack")]
    NotPackSourced {
        /// Model identifier.
        id: String,
    },
    /// A declared file is absent, the wrong size, or hashes differently.
    #[error("pack file `{path}` failed verification: {detail}")]
    FileVerificationFailed {
        /// The offending path.
        path: String,
        /// Non-secret description.
        detail: String,
    },
    /// The manifest inside the pack is itself invalid.
    #[error("pack contains an invalid model manifest: {0}")]
    InvalidModelManifest(String),
}

impl From<ManifestError> for PackError {
    fn from(error: ManifestError) -> Self {
        PackError::InvalidModelManifest(error.to_string())
    }
}

/// An opened, structurally validated pack.
#[derive(Debug, Clone)]
pub struct ModelPack {
    root: PathBuf,
    manifest: PackManifest,
}

impl ModelPack {
    /// Open and structurally validate a pack directory.
    ///
    /// This performs every check that can be made from the manifest plus
    /// file metadata. It does not hash the payload; call
    /// [`ModelPack::verify`] for that.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PackError> {
        let root = root.as_ref();
        let manifest_path = root.join(PACK_MANIFEST_FILE);
        let bytes =
            std::fs::read(&manifest_path).map_err(|source| PackError::ManifestUnreadable {
                path: manifest_path.display().to_string(),
                detail: source.to_string(),
            })?;
        let manifest: PackManifest = serde_json::from_slice(&bytes)
            .map_err(|error| PackError::Malformed(error.to_string()))?;

        if manifest.schema_version != PACK_SCHEMA_VERSION {
            return Err(PackError::UnsupportedSchemaVersion {
                found: manifest.schema_version,
                supported: PACK_SCHEMA_VERSION,
            });
        }

        let declared = manifest.total_declared_bytes();
        if declared > MAX_PACK_BYTES {
            return Err(PackError::TooLarge {
                declared,
                limit: MAX_PACK_BYTES,
            });
        }

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for file in &manifest.files {
            validate_relative_path(&file.path).map_err(|_| PackError::UnsafeEntry {
                path: file.path.clone(),
                reason: "path is absolute, empty or contains a traversal component",
            })?;
            if !seen.insert(file.path.as_str()) {
                return Err(PackError::DuplicateEntry {
                    path: file.path.clone(),
                });
            }
            if file.size_bytes > MAX_DECLARED_MODEL_BYTES {
                return Err(PackError::UnsafeEntry {
                    path: file.path.clone(),
                    reason: "declared size exceeds the per-file ceiling",
                });
            }
            // A symlink anywhere in the entry is refused outright: resolving it
            // could point at any file on the machine, and a pack has no need
            // for one.
            let resolved = root.join(&file.path);
            if let Ok(metadata) = std::fs::symlink_metadata(&resolved) {
                if metadata.file_type().is_symlink() {
                    return Err(PackError::UnsafeEntry {
                        path: file.path.clone(),
                        reason: "entry is a symbolic link",
                    });
                }
            }
            if !resolved.starts_with(root) {
                return Err(PackError::UnsafeEntry {
                    path: file.path.clone(),
                    reason: "entry resolves outside the pack root",
                });
            }
        }

        super::manifest::validate_manifest(&super::manifest::ModelManifest {
            schema_version: super::manifest::MANIFEST_SCHEMA_VERSION,
            models: manifest.models.clone(),
        })?;

        for model in &manifest.models {
            let ModelSource::Pack { entry_path } = &model.source else {
                return Err(PackError::NotPackSourced {
                    id: model.id.to_string(),
                });
            };
            if !model.license.redistributable {
                return Err(PackError::NotRedistributable {
                    id: model.id.to_string(),
                    spdx_id: model.license.spdx_id.clone(),
                });
            }
            if manifest.file_for(entry_path).is_none() {
                return Err(PackError::MissingEntry {
                    id: model.id.to_string(),
                    path: entry_path.clone(),
                });
            }
            let license_path = format!("licenses/{}", model.license.text_file);
            if manifest.file_for(&license_path).is_none() {
                return Err(PackError::MissingLicenseText {
                    id: model.id.to_string(),
                    path: license_path,
                });
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
        })
    }

    /// The pack root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The validated manifest.
    #[must_use]
    pub const fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    /// Models this pack can install.
    #[must_use]
    pub fn models(&self) -> &[ModelEntry] {
        &self.manifest.models
    }

    /// Reproducible artifact checksum for the evidence bundle (§24).
    #[must_use]
    pub fn artifact_digest(&self) -> Sha256Digest {
        self.manifest.artifact_digest()
    }

    /// Hash every declared file and compare against the manifest.
    ///
    /// Run before install: an altered or truncated artifact is refused before
    /// any model becomes active.
    pub fn verify(&self) -> Result<(), PackError> {
        for file in &self.manifest.files {
            let path = self.root.join(&file.path);
            verify_file(&path, file.size_bytes, file.sha256).map_err(|error| {
                PackError::FileVerificationFailed {
                    path: file.path.clone(),
                    detail: error.to_string(),
                }
            })?;
        }
        Ok(())
    }

    /// Licence text shipped for a model.
    pub fn license_text(&self, model: &ModelEntry) -> Result<String, PackError> {
        let relative = format!("licenses/{}", model.license.text_file);
        let path = self.root.join(&relative);
        std::fs::read_to_string(&path).map_err(|source| PackError::FileVerificationFailed {
            path: relative,
            detail: source.to_string(),
        })
    }

    /// Install one model from the pack. Performs zero network operations by
    /// construction: nothing here can reach a transport.
    pub fn install(
        &self,
        registry: &mut ModelRegistry,
        id: &ModelId,
        keep_offline: bool,
    ) -> Result<InstalledModel, ModelError> {
        let entry = self
            .manifest
            .models
            .iter()
            .find(|model| &model.id == id)
            .ok_or_else(|| ModelError::NotInManifest { id: id.to_string() })?;
        let ModelSource::Pack { entry_path } = &entry.source else {
            return Err(ModelError::Pack(PackError::NotPackSourced {
                id: id.to_string(),
            }));
        };

        let source_path = self.root.join(entry_path);
        // Verify the payload before it is copied anywhere.
        verify_file(&source_path, entry.size_bytes, entry.sha256)?;

        let install_dir = registry.prepare_install_dir(entry)?;
        let final_path = install_dir.join(&entry.file_name);
        let staging_dir = registry.staging_dir();
        std::fs::create_dir_all(&staging_dir)
            .map_err(|source| ModelError::io(&staging_dir, &source))?;
        let staging_path = staging_dir.join(format!("{}.part", entry.id));

        std::fs::copy(&source_path, &staging_path)
            .map_err(|source| ModelError::io(&staging_path, &source))?;
        if let Err(error) = verify_file(&staging_path, entry.size_bytes, entry.sha256) {
            let _ = std::fs::remove_file(&staging_path);
            return Err(error);
        }
        std::fs::rename(&staging_path, &final_path).map_err(|source| {
            let _ = std::fs::remove_file(&staging_path);
            ModelError::io(&final_path, &source)
        })?;

        // Licence text travels with the weights so the manager can show it.
        let license_relative = format!("{}/{}", entry.relative_dir(), entry.license.text_file);
        let license_target = registry.root().join(&license_relative);
        let license_source = self
            .root
            .join(format!("licenses/{}", entry.license.text_file));
        let license_text_path = match std::fs::copy(&license_source, &license_target) {
            Ok(_) => Some(license_relative),
            Err(source) => return Err(ModelError::io(&license_target, &source)),
        };

        let model = InstalledModel {
            id: entry.id.clone(),
            engine: entry.engine.clone(),
            version: entry.version.clone(),
            display_name: entry.display_name.clone(),
            relative_path: entry.relative_path(),
            sha256: entry.sha256,
            byte_length: entry.size_bytes,
            license_spdx: entry.license.spdx_id.clone(),
            license_text_path,
            keep_offline,
            installed_from: InstallOrigin::OfflinePack,
        };
        registry.record(model.clone())?;
        Ok(model)
    }
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{HardwareRecommendation, ModelLicense};
    use super::*;

    const WEIGHTS: &[u8] = b"offline whisper weights, redistributable";
    const LICENSE: &str = "MIT License\n\nPermission is hereby granted...\n";

    fn model_entry() -> ModelEntry {
        ModelEntry {
            id: ModelId::parse("tiny-en").expect("id"),
            engine: "whisper.cpp".into(),
            version: "v1".into(),
            display_name: "Whisper Tiny (English)".into(),
            file_name: "ggml-tiny.en.bin".into(),
            size_bytes: WEIGHTS.len() as u64,
            sha256: Sha256Digest::of(WEIGHTS),
            languages: Vec::new(),
            license: ModelLicense {
                spdx_id: "MIT".into(),
                name: "MIT License".into(),
                redistributable: true,
                text_file: "MIT.txt".into(),
            },
            source: ModelSource::Pack {
                entry_path: "models/whisper.cpp/v1/ggml-tiny.en.bin".into(),
            },
            hardware: HardwareRecommendation {
                min_ram_bytes: 1,
                recommended_ram_bytes: 2,
                notes: String::new(),
            },
        }
    }

    fn write_pack(root: &Path, mutate: impl FnOnce(&mut PackManifest)) -> PathBuf {
        let model = model_entry();
        std::fs::create_dir_all(root.join("models/whisper.cpp/v1")).expect("mkdir");
        std::fs::create_dir_all(root.join("licenses")).expect("mkdir");
        std::fs::write(root.join("models/whisper.cpp/v1/ggml-tiny.en.bin"), WEIGHTS)
            .expect("weights");
        std::fs::write(root.join("licenses/MIT.txt"), LICENSE).expect("license");

        let mut manifest = PackManifest {
            schema_version: PACK_SCHEMA_VERSION,
            pack_version: "2026.10.1".into(),
            models: vec![model],
            files: vec![
                PackFile {
                    path: "models/whisper.cpp/v1/ggml-tiny.en.bin".into(),
                    size_bytes: WEIGHTS.len() as u64,
                    sha256: Sha256Digest::of(WEIGHTS),
                },
                PackFile {
                    path: "licenses/MIT.txt".into(),
                    size_bytes: LICENSE.len() as u64,
                    sha256: Sha256Digest::of(LICENSE.as_bytes()),
                },
            ],
        };
        mutate(&mut manifest);
        std::fs::write(
            root.join(PACK_MANIFEST_FILE),
            serde_json::to_vec_pretty(&manifest).expect("serialise"),
        )
        .expect("manifest");
        root.to_path_buf()
    }

    #[test]
    fn a_well_formed_pack_opens_and_verifies() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |_| {});
        let pack = ModelPack::open(dir.path()).expect("open");
        pack.verify().expect("verify");
        assert_eq!(pack.models().len(), 1);
        assert!(pack
            .license_text(&pack.models()[0])
            .expect("licence")
            .contains("MIT License"));
    }

    #[test]
    fn the_artifact_digest_is_reproducible_and_order_independent() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |_| {});
        let first = ModelPack::open(dir.path()).expect("open").artifact_digest();

        let other = tempfile::tempdir().expect("tempdir");
        write_pack(other.path(), |manifest| manifest.files.reverse());
        let second = ModelPack::open(other.path())
            .expect("open")
            .artifact_digest();

        assert_eq!(first, second, "file ordering changed the artifact digest");
    }

    #[test]
    fn a_different_pack_version_changes_the_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |_| {});
        let first = ModelPack::open(dir.path()).expect("open").artifact_digest();

        let other = tempfile::tempdir().expect("tempdir");
        write_pack(other.path(), |manifest| {
            manifest.pack_version = "2026.10.2".into();
        });
        let second = ModelPack::open(other.path())
            .expect("open")
            .artifact_digest();
        assert_ne!(first, second);
    }

    #[test]
    fn an_altered_payload_is_rejected_before_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |_| {});
        let tampered: Vec<u8> = WEIGHTS.iter().map(|byte| byte ^ 0x20).collect();
        std::fs::write(
            dir.path().join("models/whisper.cpp/v1/ggml-tiny.en.bin"),
            &tampered,
        )
        .expect("tamper");

        let pack = ModelPack::open(dir.path()).expect("open");
        assert!(matches!(
            pack.verify(),
            Err(PackError::FileVerificationFailed { .. })
        ));

        let registry_dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(registry_dir.path()).expect("registry");
        let id = ModelId::parse("tiny-en").expect("id");
        assert!(pack.install(&mut registry, &id, false).is_err());
        assert_eq!(
            registry.installed().len(),
            0,
            "a tampered model became active"
        );
    }

    #[test]
    fn a_truncated_payload_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |_| {});
        std::fs::write(
            dir.path().join("models/whisper.cpp/v1/ggml-tiny.en.bin"),
            &WEIGHTS[..5],
        )
        .expect("truncate");
        let pack = ModelPack::open(dir.path()).expect("open");
        assert!(matches!(
            pack.verify(),
            Err(PackError::FileVerificationFailed { .. })
        ));
    }

    #[test]
    fn traversal_entries_are_rejected_before_any_mutation() {
        for hostile in ["../escape.bin", "/etc/passwd", "models/../../escape.bin"] {
            let dir = tempfile::tempdir().expect("tempdir");
            write_pack(dir.path(), |manifest| {
                manifest.files.push(PackFile {
                    path: hostile.into(),
                    size_bytes: 1,
                    sha256: Sha256Digest::of(b"x"),
                });
            });
            assert!(
                matches!(
                    ModelPack::open(dir.path()),
                    Err(PackError::UnsafeEntry { .. })
                ),
                "`{hostile}` should be rejected"
            );
        }
    }

    #[test]
    fn duplicate_entries_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |manifest| {
            let first = manifest.files[0].clone();
            manifest.files.push(first);
        });
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::DuplicateEntry { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_entries_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |manifest| {
            manifest.files.push(PackFile {
                path: "models/whisper.cpp/v1/link.bin".into(),
                size_bytes: WEIGHTS.len() as u64,
                sha256: Sha256Digest::of(WEIGHTS),
            });
        });
        std::os::unix::fs::symlink(
            "/etc/passwd",
            dir.path().join("models/whisper.cpp/v1/link.bin"),
        )
        .expect("symlink");
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::UnsafeEntry {
                reason: "entry is a symbolic link",
                ..
            })
        ));
    }

    #[test]
    fn an_absurd_declared_total_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |manifest| {
            manifest.files.push(PackFile {
                path: "models/whisper.cpp/v1/huge.bin".into(),
                size_bytes: MAX_PACK_BYTES,
                sha256: Sha256Digest::of(b"x"),
            });
        });
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_non_redistributable_model_is_excluded() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |manifest| {
            manifest.models[0].license.redistributable = false;
            manifest.models[0].license.spdx_id = "LicenseRef-Proprietary".into();
        });
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::NotRedistributable { .. })
        ));
    }

    #[test]
    fn a_missing_licence_text_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |manifest| {
            manifest
                .files
                .retain(|file| file.path != "licenses/MIT.txt");
        });
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::MissingLicenseText { .. })
        ));
    }

    #[test]
    fn a_model_pointing_outside_the_declared_files_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |manifest| {
            manifest.models[0].source = ModelSource::Pack {
                entry_path: "models/whisper.cpp/v1/not-declared.bin".into(),
            };
        });
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::MissingEntry { .. })
        ));
    }

    #[test]
    fn an_unknown_pack_schema_version_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_pack(dir.path(), |manifest| {
            manifest.schema_version = PACK_SCHEMA_VERSION + 1;
        });
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::UnsupportedSchemaVersion { .. })
        ));
    }

    #[test]
    fn a_missing_manifest_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            ModelPack::open(dir.path()),
            Err(PackError::ManifestUnreadable { .. })
        ));
    }

    #[test]
    fn installing_from_a_pack_activates_the_model_and_its_licence() {
        let pack_dir = tempfile::tempdir().expect("tempdir");
        write_pack(pack_dir.path(), |_| {});
        let pack = ModelPack::open(pack_dir.path()).expect("open");
        pack.verify().expect("verify");

        let registry_dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(registry_dir.path()).expect("registry");
        let id = ModelId::parse("tiny-en").expect("id");

        let installed = pack.install(&mut registry, &id, true).expect("install");
        assert_eq!(installed.installed_from, InstallOrigin::OfflinePack);
        assert!(installed.keep_offline);
        assert_eq!(installed.license_spdx, "MIT");

        let weights = registry.resolve(&id).expect("resolve");
        assert_eq!(std::fs::read(weights).expect("read"), WEIGHTS);
        assert!(registry.verify(&id).expect("verify").intact);

        let license = registry
            .root()
            .join(installed.license_text_path.expect("licence path"));
        assert!(std::fs::read_to_string(license)
            .expect("read")
            .contains("MIT"));
        assert_eq!(registry.disk_usage().staging_bytes, 0);
    }

    #[test]
    fn installing_an_unknown_model_from_a_pack_is_refused() {
        let pack_dir = tempfile::tempdir().expect("tempdir");
        write_pack(pack_dir.path(), |_| {});
        let pack = ModelPack::open(pack_dir.path()).expect("open");
        let registry_dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(registry_dir.path()).expect("registry");
        let absent = ModelId::parse("absent").expect("id");
        assert!(matches!(
            pack.install(&mut registry, &absent, false),
            Err(ModelError::NotInManifest { .. })
        ));
    }
}
