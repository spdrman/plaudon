//! Opt-in local model management (spec §12, §14.4, §1.2(2) — tasks F2.2, F2.3).
//!
//! > We will never silently download multi-gigabyte weights.
//!
//! Every entry point here needs an explicit user action, and the only path
//! that is guaranteed to work on an air-gapped machine — installing from a
//! redistributable [`pack`] — cannot reach a network transport at all,
//! because it never touches [`ModelByteSource`].

pub mod catalogue;
pub mod import_file;
pub mod install;
pub mod manifest;
pub mod pack;
pub mod registry;

pub use import_file::import_model_file;
pub use install::{
    ExplicitApprovalFetchPolicy, FetchRefused, HostResources, InstallObserver, InstallOutcome,
    InstallProgress, InstallRequest, LocalOnlyFetchPolicy, ModelByteSource, ModelFetchPolicy,
    ModelInstaller, NullInstallObserver, SystemResources, TransferError,
};
pub use manifest::{
    parse_manifest, validate_manifest, HardwareRecommendation, ManifestError, ModelEntry, ModelId,
    ModelLicense, ModelManifest, ModelSource, MANIFEST_SCHEMA_VERSION, MAX_DECLARED_MODEL_BYTES,
};
pub use pack::{ModelPack, PackError, PackFile, PackManifest, PACK_SCHEMA_VERSION};
pub use registry::{
    DeletionReport, DiskUsage, InstallOrigin, InstalledModel, IntegrityReport, ModelRegistry,
};

use std::path::Path;

use plaudon_domain::error::{AppError, AppErrorCode};

/// Everything that can go wrong managing local models.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The manifest itself is invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    /// A pack is invalid.
    #[error(transparent)]
    Pack(#[from] PackError),

    /// The egress policy refused the transfer.
    #[error(transparent)]
    FetchRefused(#[from] FetchRefused),

    /// The transport failed.
    #[error(transparent)]
    Transfer(#[from] TransferError),

    /// The requested model is not in the manifest or pack.
    #[error("model `{id}` is not offered by this manifest")]
    NotInManifest {
        /// Identifier that was requested.
        id: String,
    },

    /// The requested model is not installed.
    #[error("model `{id}` is not installed")]
    NotInstalled {
        /// Identifier that was requested.
        id: String,
    },

    /// Bytes on disk do not match the declared digest.
    #[error("checksum mismatch: expected {expected}, observed {observed}")]
    ChecksumMismatch {
        /// Digest the manifest declared, hex.
        expected: String,
        /// Digest actually observed, hex.
        observed: String,
    },

    /// Bytes on disk do not match the declared size.
    #[error("size mismatch: expected {expected} bytes, observed {observed}")]
    SizeMismatch {
        /// Size the manifest declared.
        expected: u64,
        /// Size actually observed.
        observed: u64,
    },

    /// Preflight refused the install (§14.4).
    #[error("insufficient disk space: {required_bytes} bytes needed, {available_bytes} available")]
    InsufficientDisk {
        /// Bytes the install needs, including headroom.
        required_bytes: u64,
        /// Bytes currently available.
        available_bytes: u64,
    },

    /// The machine has less RAM than the model requires (§14.4).
    #[error("insufficient memory: model needs {required_bytes} bytes, this machine has {available_bytes}")]
    InsufficientMemory {
        /// Bytes the model declares it needs.
        required_bytes: u64,
        /// Bytes the machine reports.
        available_bytes: u64,
    },

    /// The resource probe itself failed.
    #[error("could not measure host resources: {0}")]
    ResourceProbeFailed(String),

    /// The registry index is unreadable.
    #[error("installed-model index is unusable: {0}")]
    CorruptIndex(String),

    /// A computed path escaped the model root.
    #[error("refusing to write outside the model root: {path}")]
    PathEscape {
        /// The offending path.
        path: String,
    },

    /// The user cancelled the install. Nothing was installed and the partial
    /// file was removed.
    #[error("the install was cancelled; nothing was installed")]
    Cancelled,

    /// An imported file matches no catalogue entry, by size or by checksum.
    #[error("the file ({observed_bytes} bytes) does not match any model in the catalogue")]
    NoCatalogueMatch {
        /// Size of the file that was offered.
        observed_bytes: u64,
    },

    /// Filesystem failure.
    #[error("io error at {path}: {detail}")]
    Io {
        /// Path involved.
        path: String,
        /// Non-secret description.
        detail: String,
    },
}

impl ModelError {
    pub(crate) fn io(path: impl AsRef<Path>, source: &std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            detail: source.to_string(),
        }
    }

    /// Map onto the closed application error code set (§15.4).
    #[must_use]
    pub const fn app_error_code(&self) -> AppErrorCode {
        match self {
            ModelError::Manifest(_) | ModelError::Pack(_) => AppErrorCode::MalformedOutput,
            ModelError::FetchRefused(FetchRefused::LocalOnlyMode) => AppErrorCode::LocalOnlyMode,
            ModelError::FetchRefused(_) => AppErrorCode::ConsentRequired,
            ModelError::Transfer(_) => AppErrorCode::DownloadInterrupted,
            ModelError::NotInManifest { .. } | ModelError::NotInstalled { .. } => {
                AppErrorCode::ModelUnavailable
            }
            ModelError::ChecksumMismatch { .. }
            | ModelError::SizeMismatch { .. }
            | ModelError::NoCatalogueMatch { .. } => AppErrorCode::IntegrityFailed,
            ModelError::Cancelled => AppErrorCode::Canceled,
            ModelError::InsufficientDisk { .. } => AppErrorCode::StorageFull,
            ModelError::InsufficientMemory { .. } | ModelError::ResourceProbeFailed(_) => {
                AppErrorCode::ModelUnavailable
            }
            ModelError::CorruptIndex(_) => AppErrorCode::IntegrityFailed,
            ModelError::PathEscape { .. } => AppErrorCode::ArchivePathRejected,
            ModelError::Io { .. } => AppErrorCode::StorageUnavailable,
        }
    }
}

impl PartialEq for ModelError {
    fn eq(&self, other: &Self) -> bool {
        // Comparing the rendered form keeps assertions readable without
        // requiring `PartialEq` on `std::io::Error`.
        self.to_string() == other.to_string()
            && std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl From<ModelError> for AppError {
    fn from(error: ModelError) -> Self {
        AppError::new(error.app_error_code(), error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_refusal_maps_to_the_local_only_code() {
        let error = ModelError::FetchRefused(FetchRefused::LocalOnlyMode);
        assert_eq!(error.app_error_code(), AppErrorCode::LocalOnlyMode);
        let app: AppError = error.into();
        assert_eq!(app.code(), AppErrorCode::LocalOnlyMode);
    }

    #[test]
    fn integrity_failures_map_to_the_integrity_code() {
        assert_eq!(
            ModelError::ChecksumMismatch {
                expected: "a".into(),
                observed: "b".into()
            }
            .app_error_code(),
            AppErrorCode::IntegrityFailed
        );
        assert_eq!(
            ModelError::SizeMismatch {
                expected: 1,
                observed: 2
            }
            .app_error_code(),
            AppErrorCode::IntegrityFailed
        );
    }

    #[test]
    fn a_full_disk_maps_to_a_code_with_a_free_space_affordance() {
        let code = ModelError::InsufficientDisk {
            required_bytes: 10,
            available_bytes: 1,
        }
        .app_error_code();
        assert_eq!(code, AppErrorCode::StorageFull);
        assert_eq!(
            code.action(),
            Some(plaudon_domain::error::ErrorAction::FreeDiskSpace)
        );
    }

    #[test]
    fn a_missing_model_maps_to_model_unavailable_not_a_crash() {
        assert_eq!(
            ModelError::NotInstalled { id: "tiny".into() }.app_error_code(),
            AppErrorCode::ModelUnavailable
        );
    }
}
