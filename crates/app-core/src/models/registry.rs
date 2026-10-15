//! Installed-model registry (spec §12, §13.4 — task F2.2).
//!
//! Models live **outside the application bundle**, in
//! `<user_app_data>/models/<engine>/<model-version>/`, and the registry index
//! records what is installed, what it hashes to, and whether the user asked to
//! keep it for offline use. The index is written atomically, so an interrupted
//! write can never leave a half-parsed index or a row pointing at a file that
//! was never completed.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use plaudon_domain::media::Sha256Digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{ModelEntry, ModelId};
use super::ModelError;

/// Name of the index document inside the model root.
pub const INDEX_FILE_NAME: &str = "installed.json";

/// Index schema version.
pub const INDEX_SCHEMA_VERSION: u32 = 1;

/// Directory holding partially-downloaded weights.
pub const STAGING_DIR_NAME: &str = "staging";

/// One installed model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledModel {
    /// Manifest identifier.
    pub id: ModelId,
    /// Engine that loads these weights.
    pub engine: String,
    /// Weight version.
    pub version: String,
    /// Name shown in the manager.
    pub display_name: String,
    /// Path relative to the model root.
    pub relative_path: String,
    /// Digest of the installed bytes.
    pub sha256: Sha256Digest,
    /// Size of the installed bytes.
    pub byte_length: u64,
    /// SPDX identifier of the licence the weights ship under.
    pub license_spdx: String,
    /// Relative path of the licence text, when one was installed.
    pub license_text_path: Option<String>,
    /// "Keep downloaded for offline use" (§12). A model with this flag is
    /// never removed by any automatic cleanup.
    pub keep_offline: bool,
    /// Where the bytes came from, for provenance.
    pub installed_from: InstallOrigin,
}

/// Provenance of an installed model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallOrigin {
    /// Downloaded from the manifest's source URL.
    Download,
    /// Installed from a redistributable offline pack (F2.3).
    OfflinePack,
    /// Imported from a file the user already had, after its checksum matched
    /// the catalogue (issue #303).
    ImportedFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RegistryIndex {
    schema_version: u32,
    #[serde(default)]
    models: BTreeMap<String, InstalledModel>,
}

impl Default for RegistryIndex {
    fn default() -> Self {
        Self {
            schema_version: INDEX_SCHEMA_VERSION,
            models: BTreeMap::new(),
        }
    }
}

/// Result of an integrity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    /// Model that was checked.
    pub id: ModelId,
    /// Whether the bytes on disk still hash to the recorded digest.
    pub intact: bool,
    /// Digest actually observed.
    pub observed_sha256: Option<Sha256Digest>,
    /// Size actually observed.
    pub observed_bytes: Option<u64>,
}

/// What a deletion removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionReport {
    /// Model that was removed.
    pub id: ModelId,
    /// Bytes freed on disk.
    pub freed_bytes: u64,
    /// Whether the weight file existed at all.
    pub file_existed: bool,
}

/// Disk accounting for the model root (§12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskUsage {
    /// Total bytes of installed weights, as recorded.
    pub installed_bytes: u64,
    /// Bytes sitting in the staging directory from interrupted installs.
    pub staging_bytes: u64,
    /// Number of installed models.
    pub model_count: usize,
}

/// The on-disk model store.
#[derive(Debug)]
pub struct ModelRegistry {
    root: PathBuf,
    index: RegistryIndex,
}

impl ModelRegistry {
    /// Open, creating the root and an empty index if needed.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ModelError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| ModelError::io(&root, &source))?;
        let index_path = root.join(INDEX_FILE_NAME);
        let index = if index_path.exists() {
            let bytes = std::fs::read(&index_path)
                .map_err(|source| ModelError::io(&index_path, &source))?;
            let index: RegistryIndex = serde_json::from_slice(&bytes)
                .map_err(|error| ModelError::CorruptIndex(error.to_string()))?;
            if index.schema_version != INDEX_SCHEMA_VERSION {
                return Err(ModelError::CorruptIndex(format!(
                    "index schema version {} is not supported",
                    index.schema_version
                )));
            }
            index
        } else {
            RegistryIndex::default()
        };
        Ok(Self { root, index })
    }

    /// The model root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory used for partially-transferred weights.
    #[must_use]
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(STAGING_DIR_NAME)
    }

    /// Every installed model, ordered by identifier.
    #[must_use]
    pub fn installed(&self) -> Vec<&InstalledModel> {
        self.index.models.values().collect()
    }

    /// Look up an installed model.
    #[must_use]
    pub fn find(&self, id: &ModelId) -> Option<&InstalledModel> {
        self.index.models.get(id.as_str())
    }

    /// Absolute path of an installed model's weights.
    #[must_use]
    pub fn path_of(&self, model: &InstalledModel) -> PathBuf {
        self.root.join(&model.relative_path)
    }

    /// Resolve an installed model's weights by identifier.
    pub fn resolve(&self, id: &ModelId) -> Result<PathBuf, ModelError> {
        let model = self
            .find(id)
            .ok_or_else(|| ModelError::NotInstalled { id: id.to_string() })?;
        let path = self.path_of(model);
        if !path.is_file() {
            return Err(ModelError::NotInstalled { id: id.to_string() });
        }
        Ok(path)
    }

    /// Record a freshly installed model and persist the index.
    pub(crate) fn record(&mut self, model: InstalledModel) -> Result<(), ModelError> {
        self.index.models.insert(model.id.to_string(), model);
        self.persist()
    }

    /// Toggle "keep downloaded for offline use".
    pub fn set_keep_offline(&mut self, id: &ModelId, keep: bool) -> Result<(), ModelError> {
        let model = self
            .index
            .models
            .get_mut(id.as_str())
            .ok_or_else(|| ModelError::NotInstalled { id: id.to_string() })?;
        model.keep_offline = keep;
        self.persist()
    }

    /// Re-hash a model's bytes and compare against the recorded digest.
    pub fn verify(&self, id: &ModelId) -> Result<IntegrityReport, ModelError> {
        let model = self
            .find(id)
            .ok_or_else(|| ModelError::NotInstalled { id: id.to_string() })?;
        let path = self.path_of(model);
        let Ok(mut file) = std::fs::File::open(&path) else {
            return Ok(IntegrityReport {
                id: id.clone(),
                intact: false,
                observed_sha256: None,
                observed_bytes: None,
            });
        };
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut observed_bytes = 0u64;
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|source| ModelError::io(&path, &source))?;
            if read == 0 {
                break;
            }
            observed_bytes += read as u64;
            hasher.update(&buffer[..read]);
        }
        let digest: [u8; 32] = hasher.finalize().into();
        let observed = Sha256Digest::from_bytes(digest);
        Ok(IntegrityReport {
            id: id.clone(),
            intact: observed == model.sha256 && observed_bytes == model.byte_length,
            observed_sha256: Some(observed),
            observed_bytes: Some(observed_bytes),
        })
    }

    /// Remove a model's weights and its index row.
    ///
    /// Leaves no dangling row even when the file has already vanished.
    pub fn delete(&mut self, id: &ModelId) -> Result<DeletionReport, ModelError> {
        let model = self
            .index
            .models
            .remove(id.as_str())
            .ok_or_else(|| ModelError::NotInstalled { id: id.to_string() })?;
        let path = self.root.join(&model.relative_path);
        let file_existed = path.is_file();
        let freed_bytes = if file_existed {
            let size = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
            std::fs::remove_file(&path).map_err(|source| ModelError::io(&path, &source))?;
            size
        } else {
            0
        };
        if let Some(license_path) = &model.license_text_path {
            let _ = std::fs::remove_file(self.root.join(license_path));
        }
        // Prune now-empty version and engine directories so a delete leaves no
        // trace, but never walk outside the root.
        let mut dir = path.parent().map(Path::to_path_buf);
        while let Some(candidate) = dir {
            if candidate == self.root || !candidate.starts_with(&self.root) {
                break;
            }
            if std::fs::remove_dir(&candidate).is_err() {
                break;
            }
            dir = candidate.parent().map(Path::to_path_buf);
        }
        self.persist()?;
        Ok(DeletionReport {
            id: id.clone(),
            freed_bytes,
            file_existed,
        })
    }

    /// Disk accounting, including bytes stranded in staging.
    #[must_use]
    pub fn disk_usage(&self) -> DiskUsage {
        let staging_bytes = std::fs::read_dir(self.staging_dir()).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.metadata().ok())
                .filter(std::fs::Metadata::is_file)
                .map(|metadata| metadata.len())
                .sum()
        });
        DiskUsage {
            installed_bytes: self
                .index
                .models
                .values()
                .map(|model| model.byte_length)
                .sum(),
            staging_bytes,
            model_count: self.index.models.len(),
        }
    }

    /// Remove staging files that no install is resuming.
    ///
    /// A `.part` file is never treated as installed, so deleting one can only
    /// cost a restart of a download, never data.
    pub fn purge_staging(&self) -> Result<u64, ModelError> {
        let staging = self.staging_dir();
        let Ok(entries) = std::fs::read_dir(&staging) else {
            return Ok(0);
        };
        let mut freed = 0u64;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_file() {
                freed += entry.metadata().map_or(0, |metadata| metadata.len());
                std::fs::remove_file(&path).map_err(|source| ModelError::io(&path, &source))?;
            }
        }
        Ok(freed)
    }

    /// Install directory for a manifest entry, created on demand.
    pub(crate) fn prepare_install_dir(&self, entry: &ModelEntry) -> Result<PathBuf, ModelError> {
        let dir = self
            .root
            .join(entry.engine.as_str())
            .join(entry.version.as_str());
        // The manifest validator already rejected separators and traversal in
        // `engine`/`version`; this is the belt to that braces.
        if !dir.starts_with(&self.root) {
            return Err(ModelError::PathEscape {
                path: dir.display().to_string(),
            });
        }
        std::fs::create_dir_all(&dir).map_err(|source| ModelError::io(&dir, &source))?;
        Ok(dir)
    }

    fn persist(&self) -> Result<(), ModelError> {
        let target = self.root.join(INDEX_FILE_NAME);
        let temporary = self.root.join(format!("{INDEX_FILE_NAME}.tmp"));
        let bytes = serde_json::to_vec_pretty(&self.index)
            .map_err(|error| ModelError::CorruptIndex(error.to_string()))?;
        {
            let mut file = std::fs::File::create(&temporary)
                .map_err(|source| ModelError::io(&temporary, &source))?;
            file.write_all(&bytes)
                .map_err(|source| ModelError::io(&temporary, &source))?;
            file.sync_all()
                .map_err(|source| ModelError::io(&temporary, &source))?;
        }
        std::fs::rename(&temporary, &target).map_err(|source| ModelError::io(&target, &source))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{HardwareRecommendation, ModelLicense, ModelSource};
    use super::*;

    fn entry() -> ModelEntry {
        ModelEntry {
            id: ModelId::parse("tiny-en").expect("id"),
            engine: "whisper.cpp".into(),
            version: "v1".into(),
            display_name: "Tiny".into(),
            file_name: "weights.bin".into(),
            size_bytes: 7,
            sha256: Sha256Digest::of(b"weights"),
            languages: Vec::new(),
            license: ModelLicense {
                spdx_id: "MIT".into(),
                name: "MIT".into(),
                redistributable: true,
                text_file: "MIT.txt".into(),
            },
            source: ModelSource::Pack {
                entry_path: "models/weights.bin".into(),
            },
            hardware: HardwareRecommendation {
                min_ram_bytes: 1,
                recommended_ram_bytes: 2,
                notes: String::new(),
            },
        }
    }

    fn install(registry: &mut ModelRegistry, bytes: &[u8]) -> InstalledModel {
        let entry = entry();
        let dir = registry.prepare_install_dir(&entry).expect("dir");
        std::fs::write(dir.join(&entry.file_name), bytes).expect("write");
        let model = InstalledModel {
            id: entry.id.clone(),
            engine: entry.engine.clone(),
            version: entry.version.clone(),
            display_name: entry.display_name.clone(),
            relative_path: entry.relative_path(),
            sha256: Sha256Digest::of(bytes),
            byte_length: bytes.len() as u64,
            license_spdx: entry.license.spdx_id.clone(),
            license_text_path: None,
            keep_offline: false,
            installed_from: InstallOrigin::OfflinePack,
        };
        registry.record(model.clone()).expect("record");
        model
    }

    #[test]
    fn a_fresh_registry_has_nothing_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ModelRegistry::open(dir.path()).expect("open");
        assert_eq!(registry.installed().len(), 0);
        assert_eq!(registry.disk_usage(), DiskUsage::default());
    }

    #[test]
    fn install_paths_follow_the_versioned_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let model = install(&mut registry, b"weights");
        assert_eq!(model.relative_path, "whisper.cpp/v1/weights.bin");
        assert!(dir.path().join("whisper.cpp/v1/weights.bin").is_file());
    }

    #[test]
    fn the_index_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = {
            let mut registry = ModelRegistry::open(dir.path()).expect("open");
            install(&mut registry, b"weights").id
        };
        let reopened = ModelRegistry::open(dir.path()).expect("reopen");
        assert_eq!(reopened.installed().len(), 1);
        assert!(reopened.find(&id).is_some());
        assert!(reopened.resolve(&id).is_ok());
    }

    #[test]
    fn verification_detects_tampering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let model = install(&mut registry, b"weights");
        assert!(registry.verify(&model.id).expect("verify").intact);

        std::fs::write(registry.path_of(&model), b"tampered").expect("tamper");
        let report = registry.verify(&model.id).expect("verify");
        assert!(!report.intact);
        assert_eq!(report.observed_bytes, Some(8));
    }

    #[test]
    fn verification_reports_a_vanished_file_rather_than_failing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let model = install(&mut registry, b"weights");
        std::fs::remove_file(registry.path_of(&model)).expect("remove");
        let report = registry.verify(&model.id).expect("verify");
        assert!(!report.intact);
        assert_eq!(report.observed_sha256, None);
    }

    #[test]
    fn delete_frees_disk_and_leaves_no_dangling_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let model = install(&mut registry, b"weights");
        assert_eq!(registry.disk_usage().installed_bytes, 7);

        let report = registry.delete(&model.id).expect("delete");
        assert_eq!(report.freed_bytes, 7);
        assert!(report.file_existed);
        assert_eq!(registry.installed().len(), 0);
        assert_eq!(registry.disk_usage().installed_bytes, 0);
        assert!(!dir.path().join("whisper.cpp/v1/weights.bin").exists());
        // Empty directories are pruned, but the root survives.
        assert!(!dir.path().join("whisper.cpp").exists());
        assert!(dir.path().exists());

        assert!(matches!(
            registry.delete(&model.id),
            Err(ModelError::NotInstalled { .. })
        ));
    }

    #[test]
    fn delete_still_removes_the_row_when_the_file_is_already_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let model = install(&mut registry, b"weights");
        std::fs::remove_file(registry.path_of(&model)).expect("remove");
        let report = registry.delete(&model.id).expect("delete");
        assert!(!report.file_existed);
        assert_eq!(registry.installed().len(), 0);
    }

    #[test]
    fn keep_offline_is_persisted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = {
            let mut registry = ModelRegistry::open(dir.path()).expect("open");
            let model = install(&mut registry, b"weights");
            registry.set_keep_offline(&model.id, true).expect("flag");
            model.id
        };
        let reopened = ModelRegistry::open(dir.path()).expect("reopen");
        assert!(reopened.find(&id).expect("row").keep_offline);
    }

    #[test]
    fn resolving_a_missing_model_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ModelRegistry::open(dir.path()).expect("open");
        let id = ModelId::parse("absent").expect("id");
        assert!(matches!(
            registry.resolve(&id),
            Err(ModelError::NotInstalled { .. })
        ));
    }

    #[test]
    fn a_corrupt_index_is_reported_not_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(INDEX_FILE_NAME), b"{ not json").expect("write");
        assert!(matches!(
            ModelRegistry::open(dir.path()),
            Err(ModelError::CorruptIndex(_))
        ));
    }

    #[test]
    fn staging_bytes_are_accounted_and_purgeable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ModelRegistry::open(dir.path()).expect("open");
        std::fs::create_dir_all(registry.staging_dir()).expect("mkdir");
        std::fs::write(registry.staging_dir().join("tiny-en.part"), vec![0u8; 100]).expect("write");
        assert_eq!(registry.disk_usage().staging_bytes, 100);
        assert_eq!(registry.purge_staging().expect("purge"), 100);
        assert_eq!(registry.disk_usage().staging_bytes, 0);
    }
}
