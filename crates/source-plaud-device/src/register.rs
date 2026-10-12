//! The `compatibility.yaml` claim register (spec 2.5A, 2.5B, App. B.2).
//!
//! Two rules make the register trustworthy:
//!
//! * **Fail closed.** [`CompatibilityRegister::resolve`] matches an exact
//!   tuple. A near-miss on the device model, a firmware outside the recorded
//!   range or an unseen host OS resolves to [`Resolution::Unmatched`], which
//!   grants nothing.
//! * **Evidence before status.** A cell cannot be [`SupportStatus::Supported`]
//!   without a firmware range, a host OS, a `test_run_id`, a
//!   `last_verified_at` and a recorded verified import. Validation rejects
//!   such a row rather than trusting it.

use std::collections::BTreeSet;

use plaudon_domain::capability::CapabilityStatus;
use serde::{Deserialize, Serialize};

/// Evidence level behind a cell (spec 2.5A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    /// Observed on identified hardware running identified firmware.
    ObservedOnIdentifiedHardware,
    /// Implemented, but not verified on our target platforms.
    ImplementedNotVerified,
    /// Designed or proposed only.
    Proposed,
    /// Probed and failed, or uses an unimplemented mode.
    Unsupported,
}

/// User-facing support status (App. B.2 vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportStatus {
    /// Not evaluated.
    NotEvaluated,
    /// Opt-in diagnostic only.
    Experimental,
    /// Validated with archived evidence.
    Supported,
    /// Probed and known not to work.
    KnownUnsupported,
}

impl SupportStatus {
    /// The exact string the interface may display for this status.
    ///
    /// Spec 2.5B: user-visible compatibility strings are generated from the
    /// register, never written as copy.
    #[must_use]
    pub const fn ui_label(self) -> &'static str {
        match self {
            Self::NotEvaluated => "Not evaluated",
            Self::Experimental => "Experimental",
            Self::Supported => "Supported",
            Self::KnownUnsupported => "Known unsupported",
        }
    }

    /// Mapping onto the shared capability vocabulary.
    #[must_use]
    pub const fn capability_status(self) -> CapabilityStatus {
        match self {
            Self::NotEvaluated => CapabilityStatus::Unknown,
            Self::Experimental => CapabilityStatus::Experimental,
            Self::Supported => CapabilityStatus::Supported,
            Self::KnownUnsupported => CapabilityStatus::Unsupported,
        }
    }

    /// Whether the feature may actually run for this status.
    #[must_use]
    pub const fn permits_use(self) -> bool {
        matches!(self, Self::Supported)
    }

    /// Whether the feature may be offered as an opt-in diagnostic.
    #[must_use]
    pub const fn permits_opt_in_diagnostic(self) -> bool {
        matches!(self, Self::Experimental)
    }
}

/// The tuple a runtime probe produces and the register is asked about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileKey {
    /// Source type, for example `plaud_device_ble`.
    pub source_type: String,
    /// Device model as observed.
    pub device_model: String,
    /// Firmware fingerprint as observed.
    pub firmware: String,
    /// Host operating system family, for example `macos`.
    pub host_os: String,
    /// Host operating system version as observed.
    pub os_version: String,
    /// Transport in use.
    pub transport: String,
    /// Authentication mode offered by the device.
    pub auth_mode: String,
}

/// One row of the register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityCell {
    /// Source type this cell describes.
    pub source_type: String,
    /// Vendor name.
    pub vendor: String,
    /// Device model.
    pub device_model: String,
    /// Hardware revision, when the device reports one.
    #[serde(default)]
    pub hardware_revision: Option<String>,
    /// Firmware fingerprints this cell covers. Exact strings, no wildcards.
    #[serde(default)]
    pub firmware_range: Vec<String>,
    /// Host operating system family.
    pub host_os: String,
    /// Host operating system versions this cell covers. Exact strings.
    #[serde(default)]
    pub os_version_range: Vec<String>,
    /// Authentication mode.
    pub auth_mode: String,
    /// How the device was provisioned for this cell.
    pub provisioning_method: String,
    /// Transport.
    pub transport: String,
    /// Codec the transport yields.
    pub codec: String,
    /// Fidelity class of the acquired bytes.
    pub quality_class: String,
    /// Evidence level behind the claim.
    pub evidence_level: EvidenceLevel,
    /// User-facing status.
    pub status: SupportStatus,
    /// Identifier of the archived validation run.
    #[serde(default)]
    pub test_run_id: Option<String>,
    /// Date of the last validation, `YYYY-MM-DD`.
    #[serde(default)]
    pub last_verified_at: Option<String>,
    /// Whether an import was actually completed and verified for this cell.
    #[serde(default)]
    pub verified_import_recorded: bool,
    /// Limitations a user must be told about.
    #[serde(default)]
    pub known_limitations: Vec<String>,
}

impl CompatibilityCell {
    /// Whether this cell matches a probed profile exactly.
    ///
    /// Matching is exact on every component. There is no closest-match, no
    /// prefix match and no case-insensitive model comparison beyond
    /// normalization, because a resemblance is not evidence.
    #[must_use]
    pub fn matches(&self, key: &ProfileKey) -> bool {
        eq_ci(&self.source_type, &key.source_type)
            && eq_ci(&self.device_model, &key.device_model)
            && eq_ci(&self.host_os, &key.host_os)
            && eq_ci(&self.transport, &key.transport)
            && eq_ci(&self.auth_mode, &key.auth_mode)
            && self.firmware_range.iter().any(|f| eq_ci(f, &key.firmware))
            && self
                .os_version_range
                .iter()
                .any(|v| eq_ci(v, &key.os_version))
    }

    /// Validate the evidence rules for this row.
    pub fn validate(&self) -> Result<(), RegisterError> {
        if self.status == SupportStatus::Supported {
            if self.evidence_level != EvidenceLevel::ObservedOnIdentifiedHardware {
                return Err(RegisterError::UnsupportededEvidence {
                    device_model: self.device_model.clone(),
                });
            }
            for (field, present) in [
                ("firmware_range", !self.firmware_range.is_empty()),
                ("os_version_range", !self.os_version_range.is_empty()),
                ("test_run_id", self.test_run_id.is_some()),
                ("last_verified_at", self.last_verified_at.is_some()),
                ("verified_import_recorded", self.verified_import_recorded),
            ] {
                if !present {
                    return Err(RegisterError::SupportedWithoutEvidence {
                        device_model: self.device_model.clone(),
                        missing: field,
                    });
                }
            }
        }
        if self.status == SupportStatus::KnownUnsupported
            && self.evidence_level == EvidenceLevel::Proposed
        {
            return Err(RegisterError::UnsupportededEvidence {
                device_model: self.device_model.clone(),
            });
        }
        Ok(())
    }

    /// Return this cell demoted because its evidence was invalidated.
    ///
    /// Spec App. B.10: a firmware change invalidates the cached capability,
    /// and demotion must be expressible.
    #[must_use]
    pub fn demoted(&self, reason: &str) -> Self {
        let mut demoted = self.clone();
        demoted.status = SupportStatus::NotEvaluated;
        demoted.evidence_level = EvidenceLevel::ImplementedNotVerified;
        demoted.test_run_id = None;
        demoted.last_verified_at = None;
        demoted.verified_import_recorded = false;
        demoted.known_limitations.push(reason.to_owned());
        demoted
    }
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// What the register says about a probed profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one cell matched.
    Matched(Box<CompatibilityCell>),
    /// No cell matched. Nothing is permitted.
    Unmatched,
}

impl Resolution {
    /// The user-facing status.
    #[must_use]
    pub const fn status(&self) -> SupportStatus {
        match self {
            Self::Matched(cell) => cell.status,
            Self::Unmatched => SupportStatus::NotEvaluated,
        }
    }

    /// The exact label the interface may display.
    #[must_use]
    pub const fn ui_label(&self) -> &'static str {
        self.status().ui_label()
    }

    /// Whether listing or transfer may proceed.
    #[must_use]
    pub const fn permits_use(&self) -> bool {
        self.status().permits_use()
    }
}

/// Why a register was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// The document could not be parsed.
    #[error("the compatibility register could not be parsed")]
    Unparseable {
        /// Parser detail.
        detail: String,
    },
    /// The document declares a schema version this build does not understand.
    #[error("the compatibility register declares schema version {found}, expected {expected}")]
    SchemaVersionMismatch {
        /// Version found.
        found: u32,
        /// Version expected.
        expected: u32,
    },
    /// A cell claims `Supported` without the required evidence.
    #[error("cell for {device_model} claims Supported without {missing}")]
    SupportedWithoutEvidence {
        /// Model the offending cell describes.
        device_model: String,
        /// The missing field.
        missing: &'static str,
    },
    /// A cell's evidence level contradicts its status.
    #[error("cell for {device_model} has an evidence level that contradicts its status")]
    UnsupportededEvidence {
        /// Model the offending cell describes.
        device_model: String,
    },
    /// Two cells cover the same profile.
    #[error("two cells cover the same profile for {device_model}")]
    AmbiguousProfile {
        /// Model the offending cells describe.
        device_model: String,
    },
}

/// The parsed register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityRegister {
    /// Schema version of the document.
    pub schema_version: u32,
    /// When the register was last updated, `YYYY-MM-DD`.
    pub updated_at: String,
    /// Every cell.
    pub cells: Vec<CompatibilityCell>,
}

impl CompatibilityRegister {
    /// Schema version this build understands.
    pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

    /// Parse and validate a register document.
    pub fn from_yaml(document: &str) -> Result<Self, RegisterError> {
        let register: Self =
            serde_norway::from_str(document).map_err(|error| RegisterError::Unparseable {
                detail: error.to_string(),
            })?;
        register.validate()?;
        Ok(register)
    }

    /// Validate the whole document.
    pub fn validate(&self) -> Result<(), RegisterError> {
        if self.schema_version != Self::SUPPORTED_SCHEMA_VERSION {
            return Err(RegisterError::SchemaVersionMismatch {
                found: self.schema_version,
                expected: Self::SUPPORTED_SCHEMA_VERSION,
            });
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for cell in &self.cells {
            cell.validate()?;
            let fingerprint = format!(
                "{}|{}|{}|{}|{}|{:?}|{:?}",
                cell.source_type.to_ascii_lowercase(),
                cell.device_model.to_ascii_lowercase(),
                cell.host_os.to_ascii_lowercase(),
                cell.transport.to_ascii_lowercase(),
                cell.auth_mode.to_ascii_lowercase(),
                cell.firmware_range,
                cell.os_version_range
            );
            if !seen.insert(fingerprint) {
                return Err(RegisterError::AmbiguousProfile {
                    device_model: cell.device_model.clone(),
                });
            }
        }
        Ok(())
    }

    /// Resolve a probed profile, failing closed on anything unmatched.
    #[must_use]
    pub fn resolve(&self, key: &ProfileKey) -> Resolution {
        self.cells
            .iter()
            .find(|cell| cell.matches(key))
            .map_or(Resolution::Unmatched, |cell| {
                Resolution::Matched(Box::new(cell.clone()))
            })
    }

    /// Every distinct user-facing label the register can produce.
    ///
    /// Used by the interface so no compatibility string is ever hardcoded.
    #[must_use]
    pub fn labels(&self) -> Vec<&'static str> {
        let mut labels: Vec<&'static str> = self
            .cells
            .iter()
            .map(|cell| cell.status.ui_label())
            .collect();
        labels.sort_unstable();
        labels.dedup();
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported_cell() -> CompatibilityCell {
        CompatibilityCell {
            source_type: "plaud_device_ble".into(),
            vendor: "PLAUD".into(),
            device_model: "Note".into(),
            hardware_revision: None,
            firmware_range: vec!["V0095".into()],
            host_os: "macos".into(),
            os_version_range: vec!["26.0".into()],
            auth_mode: "legacy_static_token".into(),
            provisioning_method: "imported_existing_token".into(),
            transport: "ble".into(),
            codec: "opus".into(),
            quality_class: "compressed_device_transfer".into(),
            evidence_level: EvidenceLevel::ObservedOnIdentifiedHardware,
            status: SupportStatus::Supported,
            test_run_id: Some("run-ble-001".into()),
            last_verified_at: Some("2026-10-11".into()),
            verified_import_recorded: true,
            known_limitations: vec![],
        }
    }

    fn key() -> ProfileKey {
        ProfileKey {
            source_type: "plaud_device_ble".into(),
            device_model: "Note".into(),
            firmware: "V0095".into(),
            host_os: "macos".into(),
            os_version: "26.0".into(),
            transport: "ble".into(),
            auth_mode: "legacy_static_token".into(),
        }
    }

    fn register(cells: Vec<CompatibilityCell>) -> CompatibilityRegister {
        CompatibilityRegister {
            schema_version: 1,
            updated_at: "2026-10-11".into(),
            cells,
        }
    }

    #[test]
    fn an_exact_tuple_resolves_and_a_near_miss_does_not() {
        let register = register(vec![supported_cell()]);
        assert!(register.resolve(&key()).permits_use());

        for mutate in [
            |k: &mut ProfileKey| k.firmware = "V0096".into(),
            |k: &mut ProfileKey| k.os_version = "26.1".into(),
            |k: &mut ProfileKey| k.device_model = "Note Pro".into(),
            |k: &mut ProfileKey| k.transport = "usb".into(),
            |k: &mut ProfileKey| k.auth_mode = "encrypted_handshake".into(),
            |k: &mut ProfileKey| k.host_os = "windows".into(),
        ] {
            let mut near_miss = key();
            mutate(&mut near_miss);
            let resolution = register.resolve(&near_miss);
            assert_eq!(
                resolution,
                Resolution::Unmatched,
                "{near_miss:?} must fail closed"
            );
            assert!(!resolution.permits_use());
            assert_eq!(resolution.ui_label(), "Not evaluated");
        }
    }

    #[test]
    fn a_supported_cell_without_archived_evidence_is_rejected() {
        for strip in [
            |c: &mut CompatibilityCell| c.test_run_id = None,
            |c: &mut CompatibilityCell| c.last_verified_at = None,
            |c: &mut CompatibilityCell| c.firmware_range.clear(),
            |c: &mut CompatibilityCell| c.os_version_range.clear(),
            |c: &mut CompatibilityCell| c.verified_import_recorded = false,
        ] {
            let mut cell = supported_cell();
            strip(&mut cell);
            assert!(
                matches!(
                    cell.validate(),
                    Err(RegisterError::SupportedWithoutEvidence { .. })
                ),
                "a Supported cell must carry its evidence"
            );
        }
    }

    #[test]
    fn a_supported_cell_must_be_backed_by_hardware_observation() {
        let mut cell = supported_cell();
        cell.evidence_level = EvidenceLevel::ImplementedNotVerified;
        assert!(matches!(
            cell.validate(),
            Err(RegisterError::UnsupportededEvidence { .. })
        ));
    }

    #[test]
    fn demotion_is_expressible_and_removes_the_claim() {
        let demoted = supported_cell().demoted("firmware changed to V0096");
        assert_eq!(demoted.status, SupportStatus::NotEvaluated);
        assert_eq!(demoted.test_run_id, None);
        assert!(!demoted.verified_import_recorded);
        assert!(!demoted.status.permits_use());
        assert!(demoted
            .known_limitations
            .iter()
            .any(|l| l.contains("firmware changed")));
        demoted.validate().expect("a demoted cell is valid");
    }

    #[test]
    fn duplicate_cells_for_one_profile_are_rejected() {
        let error = register(vec![supported_cell(), supported_cell()])
            .validate()
            .unwrap_err();
        assert!(matches!(error, RegisterError::AmbiguousProfile { .. }));
    }

    #[test]
    fn an_unknown_schema_version_is_rejected_rather_than_guessed() {
        let mut register = register(vec![supported_cell()]);
        register.schema_version = 99;
        assert_eq!(
            register.validate(),
            Err(RegisterError::SchemaVersionMismatch {
                found: 99,
                expected: 1
            })
        );
    }

    #[test]
    fn every_label_comes_from_the_status_vocabulary() {
        assert_eq!(SupportStatus::NotEvaluated.ui_label(), "Not evaluated");
        assert_eq!(SupportStatus::Experimental.ui_label(), "Experimental");
        assert_eq!(SupportStatus::Supported.ui_label(), "Supported");
        assert_eq!(
            SupportStatus::KnownUnsupported.ui_label(),
            "Known unsupported"
        );

        assert!(SupportStatus::Supported.permits_use());
        for status in [
            SupportStatus::NotEvaluated,
            SupportStatus::Experimental,
            SupportStatus::KnownUnsupported,
        ] {
            assert!(!status.permits_use(), "{status:?}");
        }
        assert!(SupportStatus::Experimental.permits_opt_in_diagnostic());
    }

    #[test]
    fn an_unparseable_or_unknown_enum_value_is_refused() {
        let error = CompatibilityRegister::from_yaml("schema_version: 1\ncells: [").unwrap_err();
        assert!(matches!(error, RegisterError::Unparseable { .. }));

        let bad_enum = "schema_version: 1\nupdated_at: '2026-10-11'\ncells:\n  - source_type: x\n    vendor: v\n    device_model: m\n    host_os: macos\n    auth_mode: a\n    provisioning_method: p\n    transport: t\n    codec: c\n    quality_class: q\n    evidence_level: telepathy\n    status: supported\n";
        assert!(matches!(
            CompatibilityRegister::from_yaml(bad_enum),
            Err(RegisterError::Unparseable { .. })
        ));
    }
}
