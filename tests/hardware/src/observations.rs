//! The observations a hardware run reports to the suite.
//!
//! The suite does not invent results. The run harness (the operator following
//! `docs/hardware/`, with the app under test) writes what it observed; the
//! suite validates it, derives what can be derived (opcode safety, transfer
//! proof, disconnect aggregate), refuses anything that leaks a credential or
//! recording content, and records the outcome. A check with no observation is
//! recorded `skipped`, which blocks promotion.

use std::collections::BTreeMap;
use std::path::Path;

use plaudon_source_plaud_device::evidence::{CheckOutcome, TransferEvidence};
use serde::{Deserialize, Serialize};

/// One observed check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedCheck {
    /// What happened.
    pub outcome: CheckOutcome,
    /// Secret-free, content-free detail.
    pub detail: String,
}

/// The observations file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Observations {
    /// Firmware fingerprint read from the device, not assumed.
    pub observed_firmware: String,
    /// Opcodes the app under test actually emitted, in order.
    #[serde(default)]
    pub emitted_opcodes: Vec<String>,
    /// Whether repeated listings left the device file list and cursor unchanged.
    #[serde(default)]
    pub non_destructive_scan_observed: bool,
    /// Whether the run proved zero application-initiated network requests.
    #[serde(default)]
    pub network_denied_run: bool,
    /// The verified transfer, when one completed.
    #[serde(default)]
    pub transfer: Option<TransferEvidence>,
    /// Observed checks by identifier.
    #[serde(default)]
    pub checks: BTreeMap<String, ObservedCheck>,
}

/// Why observations could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ObservationsError {
    /// The file could not be read.
    #[error("cannot read the observations file {path}: {source}")]
    Read {
        /// Path.
        path: String,
        /// Cause.
        source: std::io::Error,
    },
    /// The file is not valid observations JSON.
    #[error("the observations file is not valid: {0}")]
    Parse(#[from] serde_json::Error),
    /// The file reports a check this suite does not define.
    #[error("the observations file reports an unknown check {0}")]
    UnknownCheck(String),
}

impl Observations {
    /// Load and validate an observations file.
    pub fn load(path: &Path) -> Result<Self, ObservationsError> {
        let text = std::fs::read_to_string(path).map_err(|source| ObservationsError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let observations: Self = serde_json::from_str(&text)?;
        observations.validate()?;
        Ok(observations)
    }

    /// Reject check identifiers outside the catalogue, so a typo cannot
    /// silently leave the intended check `skipped`.
    pub fn validate(&self) -> Result<(), ObservationsError> {
        for id in self.checks.keys() {
            if crate::catalogue::find(id).is_none() {
                return Err(ObservationsError::UnknownCheck(id.clone()));
            }
        }
        Ok(())
    }
}
