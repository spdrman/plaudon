//! The hardware evidence record a `Supported` cell requires (spec §7.9
//! "Hardware shipping gate", App. B.9, §24 "Release evidence bundle").
//!
//! > "For each Supported compatibility cell, we will archive real `model +
//! > firmware + host OS/build + transport + auth/bootstrap route + audio
//! > format + checksum/terminal proof + non-destructive scan observation`
//! > evidence and repeat clean-machine setup." (§7.9)
//!
//! This module is the machine-readable form of that paragraph. An
//! [`EvidenceRecord`] is produced by the opt-in hardware suite, archived next
//! to the release, and checked by [`EvidenceRecord::promotes`] before a
//! `compatibility.yaml` cell may claim `Supported`.
//!
//! A record produced by the simulator is explicitly not hardware evidence:
//! [`EvidenceSource::Simulator`] can never promote a cell (spec §18.6A, "a
//! synthetic Bluetooth fixture alone cannot produce a `Supported` device
//! compatibility cell").

use serde::{Deserialize, Serialize};

use crate::register::{CompatibilityCell, EvidenceLevel, SupportStatus};

/// Where a record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    /// A real device attached to a real host.
    Hardware,
    /// The deterministic simulator. Useful for regression, never for a claim.
    Simulator,
}

impl EvidenceSource {
    /// Whether a record from this source may promote a compatibility cell.
    #[must_use]
    pub const fn may_promote(self) -> bool {
        matches!(self, Self::Hardware)
    }
}

/// Result of one named check in the suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// The check ran and passed.
    Passed,
    /// The check ran and failed.
    Failed,
    /// The check could not run on this device or host.
    Skipped,
}

/// One check and what it observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    /// Stable identifier of the check, matching the App. B.9 list.
    pub id: String,
    /// What happened.
    pub outcome: CheckOutcome,
    /// Human-readable detail. Never contains credentials or recording content.
    pub detail: String,
}

impl CheckResult {
    /// Build a result.
    #[must_use]
    pub fn new(id: impl Into<String>, outcome: CheckOutcome, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            outcome,
            detail: detail.into(),
        }
    }
}

/// Proof that one real transfer completed and verified (§7.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEvidence {
    /// Device-scoped recording identifier.
    pub device_recording_id: String,
    /// Media type of the acquired bytes, for example `audio/opus`.
    pub acquired_mime: String,
    /// Fidelity class recorded for the acquired bytes.
    pub quality_class: String,
    /// Bytes received.
    pub received_bytes: u64,
    /// Byte length the device declared, when the profile supplies one.
    pub device_declared_byte_length: Option<u64>,
    /// Whether the protocol's terminal marker was observed.
    pub terminal_marker_observed: bool,
    /// Lowercase hex SHA-256 of the acquired bytes.
    pub sha256_hex: String,
    /// Duration measured from the acquired bitstream, when measurable.
    pub measured_duration_ms: Option<u64>,
}

impl TransferEvidence {
    /// Whether this transfer is itself complete proof (spec §7.9: our digest
    /// alone is not enough; the terminal marker and a declared length that
    /// agrees with the byte count are also required).
    #[must_use]
    pub fn is_verified(&self) -> bool {
        self.terminal_marker_observed
            && self.sha256_hex.len() == 64
            && self
                .device_declared_byte_length
                .is_some_and(|declared| declared == self.received_bytes)
    }
}

/// Why a record was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceRecordError {
    /// A required field is missing or empty.
    #[error("the evidence record is missing {field}")]
    Missing {
        /// Field that is absent.
        field: &'static str,
    },
    /// The record came from the simulator.
    #[error("simulator results are not hardware evidence")]
    NotHardware,
    /// A required check failed or did not run.
    #[error("the required check {id} did not pass")]
    CheckNotPassed {
        /// Check identifier.
        id: String,
    },
    /// The record describes a different compatibility cell.
    #[error("the evidence record describes a different compatibility cell")]
    CellMismatch,
}

/// The archived outcome of one hardware run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    /// Schema version of this document.
    pub schema_version: u32,
    /// Identifier of this run, referenced by `compatibility.yaml`.
    pub test_run_id: String,
    /// Date of the run, `YYYY-MM-DD`.
    pub recorded_at: String,
    /// Where the record came from.
    pub source: EvidenceSource,
    /// Source kind, for example `plaud_device_ble`.
    pub source_type: String,
    /// Vendor.
    pub vendor: String,
    /// Device model.
    pub device_model: String,
    /// Firmware fingerprint observed on the device.
    pub observed_firmware: String,
    /// Host operating system family.
    pub host_os: String,
    /// Host operating system build string.
    pub host_os_build: String,
    /// Transport.
    pub transport: String,
    /// Authentication mode used.
    pub auth_mode: String,
    /// Provisioning route used to obtain the credential.
    pub provisioning_route: String,
    /// The verified transfer, when one completed.
    pub transfer: Option<TransferEvidence>,
    /// Whether repeated listings left the device's file list and cursor
    /// unchanged (spec §7.9, App. B.6).
    pub non_destructive_scan_observed: bool,
    /// Opcodes actually emitted during the run, in order. The golden trace of
    /// scenario 42 in §18.7.
    #[serde(default)]
    pub emitted_opcodes: Vec<String>,
    /// Whether the run proved zero application-initiated network requests.
    pub network_denied_run: bool,
    /// Every check and its outcome.
    #[serde(default)]
    pub checks: Vec<CheckResult>,
}

impl EvidenceRecord {
    /// Schema version this build writes and understands.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Checks that must pass before a cell may be promoted (App. B.9).
    pub const REQUIRED_CHECKS: &'static [&'static str] = &[
        "discover_only",
        "authorization_success",
        "authorization_failure",
        "missing_secret",
        "encrypted_handshake_detected",
        "listing_delta_before_after_phone_sync",
        "transfer_verified",
        "disconnect_during_transfer",
        "sleep_wake",
        "imported_bytes_verified",
        "no_device_write_opcodes",
        "network_denied",
    ];

    /// Opcode names that must never appear in an emitted trace (spec §18.7
    /// scenario 42, §16.2 "Device-side destructive sync").
    pub const FORBIDDEN_OPCODES: &'static [&'static str] = &[
        "deleteFile",
        "clearAllFile",
        "recordingControl",
        "settingsWrite",
        "devicePrivacyToggle",
        "otaBegin",
        "otaData",
        "sendSyncFileInd",
        "factoryReset",
    ];

    /// Whether the emitted opcode trace stayed inside the read-only allowlist.
    #[must_use]
    pub fn opcode_trace_is_read_only(&self) -> bool {
        !self.emitted_opcodes.iter().any(|emitted| {
            Self::FORBIDDEN_OPCODES
                .iter()
                .any(|forbidden| emitted.eq_ignore_ascii_case(forbidden))
        })
    }

    /// Outcome of one named check.
    #[must_use]
    pub fn check(&self, id: &str) -> Option<CheckOutcome> {
        self.checks
            .iter()
            .find(|result| result.id == id)
            .map(|result| result.outcome)
    }

    /// Validate the record's internal consistency.
    pub fn validate(&self) -> Result<(), EvidenceRecordError> {
        for (field, value) in [
            ("test_run_id", &self.test_run_id),
            ("recorded_at", &self.recorded_at),
            ("source_type", &self.source_type),
            ("device_model", &self.device_model),
            ("observed_firmware", &self.observed_firmware),
            ("host_os", &self.host_os),
            ("host_os_build", &self.host_os_build),
            ("transport", &self.transport),
            ("auth_mode", &self.auth_mode),
            ("provisioning_route", &self.provisioning_route),
        ] {
            if value.trim().is_empty() {
                return Err(EvidenceRecordError::Missing { field });
            }
        }
        Ok(())
    }

    /// Whether this record promotes `cell` to `Supported`.
    ///
    /// Everything the shipping gate asks for is required: real hardware, a
    /// matching cell, every required check passed, a verified transfer, a
    /// read-only opcode trace and an observed non-destructive scan. A missing
    /// record is not a soft failure — the caller gets an error and the cell
    /// keeps its previous, lower status.
    pub fn promotes(&self, cell: &CompatibilityCell) -> Result<(), EvidenceRecordError> {
        self.validate()?;
        if !self.source.may_promote() {
            return Err(EvidenceRecordError::NotHardware);
        }
        if !cell.source_type.eq_ignore_ascii_case(&self.source_type)
            || !cell.device_model.eq_ignore_ascii_case(&self.device_model)
            || !cell.host_os.eq_ignore_ascii_case(&self.host_os)
            || !cell.transport.eq_ignore_ascii_case(&self.transport)
            || !cell.auth_mode.eq_ignore_ascii_case(&self.auth_mode)
            || !cell
                .firmware_range
                .iter()
                .any(|firmware| firmware.eq_ignore_ascii_case(&self.observed_firmware))
        {
            return Err(EvidenceRecordError::CellMismatch);
        }
        for id in Self::REQUIRED_CHECKS {
            if self.check(id) != Some(CheckOutcome::Passed) {
                return Err(EvidenceRecordError::CheckNotPassed {
                    id: (*id).to_owned(),
                });
            }
        }
        if !self.opcode_trace_is_read_only() {
            return Err(EvidenceRecordError::CheckNotPassed {
                id: "no_device_write_opcodes".to_owned(),
            });
        }
        if !self.non_destructive_scan_observed {
            return Err(EvidenceRecordError::CheckNotPassed {
                id: "non_destructive_scan".to_owned(),
            });
        }
        if !self.network_denied_run {
            return Err(EvidenceRecordError::CheckNotPassed {
                id: "network_denied".to_owned(),
            });
        }
        match &self.transfer {
            Some(transfer) if transfer.is_verified() => Ok(()),
            Some(_) => Err(EvidenceRecordError::CheckNotPassed {
                id: "transfer_verified".to_owned(),
            }),
            None => Err(EvidenceRecordError::Missing { field: "transfer" }),
        }
    }

    /// Return `cell` promoted by this record.
    ///
    /// Promotion is the only way a `Supported` row is produced, so a cell
    /// cannot be hand-edited into support without an archived run.
    pub fn promoted(
        &self,
        cell: &CompatibilityCell,
    ) -> Result<CompatibilityCell, EvidenceRecordError> {
        self.promotes(cell)?;
        let mut promoted = cell.clone();
        promoted.status = SupportStatus::Supported;
        promoted.evidence_level = EvidenceLevel::ObservedOnIdentifiedHardware;
        promoted.test_run_id = Some(self.test_run_id.clone());
        promoted.last_verified_at = Some(self.recorded_at.clone());
        promoted.verified_import_recorded = true;
        Ok(promoted)
    }
}

#[cfg(test)]
mod tests {
    use crate::register::EvidenceLevel;

    use super::*;

    fn cell() -> CompatibilityCell {
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
            evidence_level: EvidenceLevel::ImplementedNotVerified,
            status: SupportStatus::NotEvaluated,
            test_run_id: None,
            last_verified_at: None,
            verified_import_recorded: false,
            known_limitations: vec![],
        }
    }

    fn passing_record() -> EvidenceRecord {
        EvidenceRecord {
            schema_version: EvidenceRecord::SCHEMA_VERSION,
            test_run_id: "run-ble-macos-001".into(),
            recorded_at: "2026-10-11".into(),
            source: EvidenceSource::Hardware,
            source_type: "plaud_device_ble".into(),
            vendor: "PLAUD".into(),
            device_model: "Note".into(),
            observed_firmware: "V0095".into(),
            host_os: "macos".into(),
            host_os_build: "27.0 (27A123)".into(),
            transport: "ble".into(),
            auth_mode: "legacy_static_token".into(),
            provisioning_route: "imported_existing_token".into(),
            transfer: Some(TransferEvidence {
                device_recording_id: "REC0001".into(),
                acquired_mime: "audio/opus".into(),
                quality_class: "compressed_device_transfer".into(),
                received_bytes: 4_096,
                device_declared_byte_length: Some(4_096),
                terminal_marker_observed: true,
                sha256_hex: "a".repeat(64),
                measured_duration_ms: Some(15_000),
            }),
            non_destructive_scan_observed: true,
            emitted_opcodes: vec![
                "authenticate".into(),
                "listDelta".into(),
                "requestData".into(),
            ],
            network_denied_run: true,
            checks: EvidenceRecord::REQUIRED_CHECKS
                .iter()
                .map(|id| CheckResult::new(*id, CheckOutcome::Passed, "observed"))
                .collect(),
        }
    }

    #[test]
    fn a_complete_hardware_record_promotes_a_cell() {
        let record = passing_record();
        let promoted = record.promoted(&cell()).unwrap();
        assert_eq!(promoted.status, SupportStatus::Supported);
        assert_eq!(promoted.test_run_id.as_deref(), Some("run-ble-macos-001"));
        assert!(promoted.verified_import_recorded);
        promoted
            .validate()
            .expect("a promoted cell satisfies the register's own evidence rules");
    }

    #[test]
    fn a_simulator_record_never_promotes_anything() {
        let mut record = passing_record();
        record.source = EvidenceSource::Simulator;
        assert_eq!(
            record.promotes(&cell()),
            Err(EvidenceRecordError::NotHardware)
        );
        assert!(record.promoted(&cell()).is_err());
    }

    #[test]
    fn a_missing_required_check_blocks_promotion() {
        for id in EvidenceRecord::REQUIRED_CHECKS {
            let mut record = passing_record();
            record.checks.retain(|check| check.id != *id);
            assert_eq!(
                record.promotes(&cell()),
                Err(EvidenceRecordError::CheckNotPassed {
                    id: (*id).to_owned()
                })
            );

            let mut failed = passing_record();
            for check in &mut failed.checks {
                if check.id == *id {
                    check.outcome = CheckOutcome::Failed;
                }
            }
            assert!(failed.promotes(&cell()).is_err());
        }
    }

    #[test]
    fn a_skipped_check_is_not_a_pass() {
        let mut record = passing_record();
        record.checks[0].outcome = CheckOutcome::Skipped;
        assert!(record.promotes(&cell()).is_err());
    }

    #[test]
    fn a_write_opcode_in_the_trace_blocks_promotion() {
        for forbidden in EvidenceRecord::FORBIDDEN_OPCODES {
            let mut record = passing_record();
            record.emitted_opcodes.push((*forbidden).to_owned());
            assert!(!record.opcode_trace_is_read_only(), "{forbidden}");
            assert_eq!(
                record.promotes(&cell()),
                Err(EvidenceRecordError::CheckNotPassed {
                    id: "no_device_write_opcodes".to_owned()
                })
            );
        }
    }

    #[test]
    fn an_unverified_transfer_blocks_promotion() {
        let mut record = passing_record();
        record.transfer.as_mut().unwrap().terminal_marker_observed = false;
        assert!(record.promotes(&cell()).is_err());

        let mut short = passing_record();
        short.transfer.as_mut().unwrap().received_bytes = 4_000;
        assert!(short.promotes(&cell()).is_err());

        let mut undeclared = passing_record();
        undeclared
            .transfer
            .as_mut()
            .unwrap()
            .device_declared_byte_length = None;
        assert!(
            undeclared.promotes(&cell()).is_err(),
            "our own digest is not proof of completeness"
        );

        let mut absent = passing_record();
        absent.transfer = None;
        assert_eq!(
            absent.promotes(&cell()),
            Err(EvidenceRecordError::Missing { field: "transfer" })
        );
    }

    #[test]
    fn a_record_for_another_cell_does_not_promote_this_one() {
        let record = passing_record();
        for mutate in [
            |c: &mut CompatibilityCell| c.host_os = "windows".into(),
            |c: &mut CompatibilityCell| c.device_model = "Note Pro".into(),
            |c: &mut CompatibilityCell| c.firmware_range = vec!["V0096".into()],
            |c: &mut CompatibilityCell| c.transport = "usb_mass_storage".into(),
            |c: &mut CompatibilityCell| c.auth_mode = "encrypted_handshake".into(),
        ] {
            let mut other = cell();
            mutate(&mut other);
            assert_eq!(
                record.promotes(&other),
                Err(EvidenceRecordError::CellMismatch)
            );
        }
    }

    #[test]
    fn a_run_that_was_not_network_denied_blocks_promotion() {
        let mut record = passing_record();
        record.network_denied_run = false;
        assert!(record.promotes(&cell()).is_err());

        let mut mutating = passing_record();
        mutating.non_destructive_scan_observed = false;
        assert!(mutating.promotes(&cell()).is_err());
    }

    #[test]
    fn a_record_round_trips_as_json_without_secret_material() {
        let record = passing_record();
        let json = serde_json::to_string_pretty(&record).unwrap();
        // The record names a provisioning route ("imported_existing_token"),
        // which is a route name. What must never appear is credential
        // material: a long hex run that is not the transfer digest.
        let digest = record.transfer.as_ref().unwrap().sha256_hex.clone();
        let suspicious: Vec<&str> = json
            .split(|c: char| !c.is_ascii_hexdigit())
            .filter(|run| run.len() >= 32 && *run != digest)
            .collect();
        assert!(suspicious.is_empty(), "unexpected hex run: {suspicious:?}");
        assert_eq!(
            serde_json::from_str::<EvidenceRecord>(&json).unwrap(),
            record
        );
    }

    #[test]
    fn an_incomplete_record_names_the_missing_field() {
        let mut record = passing_record();
        record.host_os_build = "  ".into();
        assert_eq!(
            record.validate(),
            Err(EvidenceRecordError::Missing {
                field: "host_os_build"
            })
        );
    }
}
