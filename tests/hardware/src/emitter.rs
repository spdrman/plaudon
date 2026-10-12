//! Writing and reading the evidence record (§24 "Release evidence bundle").

use std::path::{Path, PathBuf};

use plaudon_source_plaud_device::evidence::{EvidenceRecord, EvidenceRecordError};

/// Path of the JSON Schema the emitted document satisfies.
#[must_use]
pub fn schema_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/device-evidence.v1.schema.json")
}

/// Why a record could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    /// The record is internally inconsistent.
    #[error(transparent)]
    Invalid(#[from] EvidenceRecordError),
    /// A check detail would leak a credential or recording content.
    #[error("the detail of check {id} is refused: {reason}")]
    Detail {
        /// Check id.
        id: String,
        /// Reason.
        reason: crate::guard::DetailRejection,
    },
    /// Serialisation failed.
    #[error("cannot serialise the record: {0}")]
    Json(#[from] serde_json::Error),
    /// I/O failed.
    #[error("cannot access {path}: {source}")]
    Io {
        /// Path.
        path: String,
        /// Cause.
        source: std::io::Error,
    },
}

/// Serialise `record` as pretty JSON after validating it and re-checking
/// every detail against the guard.
pub fn to_json(record: &EvidenceRecord) -> Result<String, EmitError> {
    record.validate()?;
    for check in &record.checks {
        crate::guard::check_detail(&check.detail).map_err(|reason| EmitError::Detail {
            id: check.id.clone(),
            reason,
        })?;
    }
    let mut text = serde_json::to_string_pretty(record)?;
    text.push('\n');
    Ok(text)
}

/// Write `record` to `path`, creating parent directories.
pub fn write_record(path: &Path, record: &EvidenceRecord) -> Result<(), EmitError> {
    let text = to_json(record)?;
    let io = |source| EmitError::Io {
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(path, text).map_err(io)
}

/// Read a record back.
pub fn read_record(path: &Path) -> Result<EvidenceRecord, EmitError> {
    let text = std::fs::read_to_string(path).map_err(|source| EmitError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(serde_json::from_str(&text)?)
}
