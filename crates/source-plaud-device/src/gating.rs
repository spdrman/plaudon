//! Compatibility gating and the one-shot experimental probe (spec §2.5B,
//! §7.0, §7.9, App. B.2, App. B.10).
//!
//! Everything a device is allowed to do passes through [`GatePolicy::evaluate`].
//! The rules it implements are the specification's, stated once:
//!
//! * §2.5B — "We will fail closed if a firmware or OS combination is unknown."
//!   An unmatched tuple yields [`GateRefusal::UnknownProfile`] and grants
//!   nothing: no listing, no transfer, no label beyond `Not evaluated`.
//! * §7.0 — an unknown combination "may run a narrowly scoped user-approved
//!   **experimental read-only probe**, but not automatic recurring sync".
//!   [`ExperimentalProbeGrant`] is single-use and single-session.
//! * §7.9 — a legacy profile is never applied to a newly advertised model.
//!   A permitted decision requires a *named* [`ProtocolProfile`] whose model
//!   and firmware both match what was probed.
//! * §7.9, App. B.6 — automatic nearby sync stays disabled until a
//!   non-destructive audit passes for the profile.

use plaudon_plaud_protocol::profile::{profile_for, ProtocolProfile};
use serde::{Deserialize, Serialize};

use crate::register::{CompatibilityCell, CompatibilityRegister, ProfileKey, Resolution, SupportStatus};

/// What a probe established about a device and this host.
///
/// Every field is an observation. Nothing here is taken from an
/// advertisement's local name: §7.7 refuses to treat a name as identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbedProfile {
    /// Source kind, for example `plaud_device_ble` (spec §7.0).
    pub source_type: String,
    /// Device model as established by the probe.
    pub device_model: String,
    /// Firmware fingerprint as established by the probe.
    pub firmware: String,
    /// Host operating system family.
    pub host_os: String,
    /// Host operating system version.
    pub os_version: String,
    /// Transport in use.
    pub transport: String,
    /// Authentication mode the device offered.
    pub auth_mode: String,
}

impl ProbedProfile {
    /// The register lookup key for this observation.
    #[must_use]
    pub fn key(&self) -> ProfileKey {
        ProfileKey {
            source_type: self.source_type.clone(),
            device_model: self.device_model.clone(),
            firmware: self.firmware.clone(),
            host_os: self.host_os.clone(),
            os_version: self.os_version.clone(),
            transport: self.transport.clone(),
            auth_mode: self.auth_mode.clone(),
        }
    }

    /// Whether any component is missing, which is itself a reason to refuse.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        ![
            &self.source_type,
            &self.device_model,
            &self.firmware,
            &self.host_os,
            &self.os_version,
            &self.transport,
            &self.auth_mode,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    }
}

/// Why the gate refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum GateRefusal {
    /// The probe did not establish every component.
    IncompleteProbe,
    /// No register cell covers this tuple.
    UnknownProfile,
    /// The register records this cell as known not to work.
    KnownUnsupported {
        /// Limitations recorded for the cell, for display.
        known_limitations: Vec<String>,
    },
    /// The cell exists but has not been validated.
    NotEvaluated,
    /// The register says `Supported` but no named wire profile implements this
    /// model and firmware, so the codec constants would have to be guessed.
    NoNamedProtocolProfile,
    /// The device offered an authentication mode this build does not implement.
    ///
    /// Spec §7.9 and App. B.5: we fail closed; we never downgrade.
    AuthModeUnimplemented {
        /// Mode the device offered.
        offered: String,
    },
}

impl GateRefusal {
    /// A stable machine-readable reason code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::IncompleteProbe => "INCOMPLETE_PROBE",
            Self::UnknownProfile => "UNSUPPORTED_PROFILE",
            Self::KnownUnsupported { .. } => "KNOWN_UNSUPPORTED",
            Self::NotEvaluated => "NOT_EVALUATED",
            Self::NoNamedProtocolProfile => "UNSUPPORTED_PROFILE",
            Self::AuthModeUnimplemented { .. } => "AUTH_MODE_UNSUPPORTED",
        }
    }
}

/// What the gate permits for one probed device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Listing and transfer are permitted for this validated cell.
    Permitted {
        /// The register cell that authorized it.
        cell: Box<CompatibilityCell>,
        /// The named wire profile to speak.
        protocol: &'static ProtocolProfile,
    },
    /// Nothing is permitted until the user approves a single read-only probe.
    ///
    /// The probe is a diagnostic: it may connect, read capabilities and list,
    /// and it may not transfer, write or schedule anything.
    ExperimentalProbeAvailable {
        /// The register cell, when one matched.
        cell: Option<Box<CompatibilityCell>>,
        /// Why a full session was not granted.
        refusal: GateRefusal,
    },
    /// Nothing is permitted at all.
    Refused {
        /// Why.
        refusal: GateRefusal,
    },
}

impl GateDecision {
    /// Whether a listing or a transfer may run right now.
    #[must_use]
    pub const fn permits_transfer(&self) -> bool {
        matches!(self, Self::Permitted { .. })
    }

    /// Whether the user may be offered a one-shot experimental probe.
    #[must_use]
    pub const fn offers_experimental_probe(&self) -> bool {
        matches!(self, Self::ExperimentalProbeAvailable { .. })
    }

    /// The status label the interface may show, taken from the register.
    #[must_use]
    pub fn ui_label(&self) -> &'static str {
        match self {
            Self::Permitted { cell, .. } => cell.status.ui_label(),
            Self::ExperimentalProbeAvailable { cell, .. } => cell
                .as_ref()
                .map_or(SupportStatus::NotEvaluated.ui_label(), |cell| {
                    cell.status.ui_label()
                }),
            Self::Refused { refusal } => match refusal {
                GateRefusal::KnownUnsupported { .. } => SupportStatus::KnownUnsupported.ui_label(),
                _ => SupportStatus::NotEvaluated.ui_label(),
            },
        }
    }

    /// Limitations the interface must show alongside the label.
    #[must_use]
    pub fn known_limitations(&self) -> Vec<String> {
        match self {
            Self::Permitted { cell, .. } => cell.known_limitations.clone(),
            Self::ExperimentalProbeAvailable { cell, .. } => cell
                .as_ref()
                .map(|cell| cell.known_limitations.clone())
                .unwrap_or_default(),
            Self::Refused { refusal } => match refusal {
                GateRefusal::KnownUnsupported { known_limitations } => known_limitations.clone(),
                _ => Vec::new(),
            },
        }
    }
}

/// A user's explicit approval of one experimental read-only probe.
///
/// Single-use by construction: [`Self::consume`] takes `self`, so the same
/// grant cannot authorize a second session, and nothing here can schedule a
/// recurring one (spec §7.0).
#[derive(Debug, PartialEq, Eq)]
pub struct ExperimentalProbeGrant {
    peripheral_id: String,
    approved_at_unix: i64,
}

/// A probe session authorized by exactly one grant.
#[derive(Debug, PartialEq, Eq)]
pub struct ExperimentalProbeSession {
    /// Device the grant named.
    pub peripheral_id: String,
    /// When the user approved it.
    pub approved_at_unix: i64,
}

impl ExperimentalProbeSession {
    /// Whether this session may transfer bytes. It may not.
    #[must_use]
    pub const fn permits_transfer(&self) -> bool {
        false
    }

    /// Whether this session may enable background or recurring sync. It may not.
    #[must_use]
    pub const fn permits_recurring_sync(&self) -> bool {
        false
    }

    /// Whether this session may read capabilities and list recordings.
    #[must_use]
    pub const fn permits_read_only_inspection(&self) -> bool {
        true
    }
}

/// Why a grant could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeGrantError {
    /// The grant names a different device.
    #[error("this approval was given for a different device")]
    DeviceMismatch,
    /// The gate did not offer an experimental probe for this device.
    #[error("an experimental probe is not available for this device")]
    NotOffered,
}

impl ExperimentalProbeGrant {
    /// Record a user's approval for one device.
    #[must_use]
    pub fn new(peripheral_id: impl Into<String>, approved_at_unix: i64) -> Self {
        Self {
            peripheral_id: peripheral_id.into(),
            approved_at_unix,
        }
    }

    /// Device this grant names.
    #[must_use]
    pub fn peripheral_id(&self) -> &str {
        &self.peripheral_id
    }

    /// Spend the grant on one probe session.
    pub fn consume(
        self,
        peripheral_id: &str,
        decision: &GateDecision,
    ) -> Result<ExperimentalProbeSession, ProbeGrantError> {
        if self.peripheral_id != peripheral_id {
            return Err(ProbeGrantError::DeviceMismatch);
        }
        if !decision.offers_experimental_probe() {
            return Err(ProbeGrantError::NotOffered);
        }
        Ok(ExperimentalProbeSession {
            peripheral_id: self.peripheral_id,
            approved_at_unix: self.approved_at_unix,
        })
    }
}

/// Evaluates probed profiles against the committed register.
#[derive(Debug, Clone)]
pub struct GatePolicy {
    register: CompatibilityRegister,
}

impl GatePolicy {
    /// Build a policy over a parsed register.
    #[must_use]
    pub const fn new(register: CompatibilityRegister) -> Self {
        Self { register }
    }

    /// The register this policy enforces.
    #[must_use]
    pub const fn register(&self) -> &CompatibilityRegister {
        &self.register
    }

    /// Decide what a probed device may do.
    pub fn evaluate(&self, probed: &ProbedProfile) -> GateDecision {
        if !probed.is_complete() {
            return GateDecision::Refused {
                refusal: GateRefusal::IncompleteProbe,
            };
        }

        let resolution = self.register.resolve(&probed.key());
        let cell = match resolution {
            Resolution::Matched(cell) => cell,
            Resolution::Unmatched => {
                // Unknown tuples get no session, but a user may still approve a
                // one-shot read-only diagnostic (§7.0).
                return GateDecision::ExperimentalProbeAvailable {
                    cell: None,
                    refusal: GateRefusal::UnknownProfile,
                };
            }
        };

        match cell.status {
            SupportStatus::KnownUnsupported => GateDecision::Refused {
                refusal: GateRefusal::KnownUnsupported {
                    known_limitations: cell.known_limitations.clone(),
                },
            },
            SupportStatus::NotEvaluated => GateDecision::ExperimentalProbeAvailable {
                cell: Some(cell),
                refusal: GateRefusal::NotEvaluated,
            },
            SupportStatus::Experimental => GateDecision::ExperimentalProbeAvailable {
                cell: Some(cell),
                refusal: GateRefusal::NotEvaluated,
            },
            SupportStatus::Supported => {
                // A validated cell still cannot run without a named wire
                // profile: the constants would otherwise be applied to a model
                // they were never observed on (§7.7).
                let Some(protocol) = profile_for(&probed.device_model, &probed.firmware) else {
                    return GateDecision::Refused {
                        refusal: GateRefusal::NoNamedProtocolProfile,
                    };
                };
                if !protocol.auth_mode_is_implemented() {
                    return GateDecision::Refused {
                        refusal: GateRefusal::AuthModeUnimplemented {
                            offered: probed.auth_mode.clone(),
                        },
                    };
                }
                GateDecision::Permitted { cell, protocol }
            }
        }
    }

    /// Whether automatic "sync when nearby" may be offered for a profile.
    ///
    /// Spec §7.9: "We will **disable automatic nearby sync** unless repeated
    /// on-device audits prove file lists and cursors are unchanged." Both a
    /// validated cell and a passing audit are required; either alone is not
    /// enough.
    #[must_use]
    pub fn may_enable_automatic_sync(
        &self,
        probed: &ProbedProfile,
        audit_passed_for_profile: bool,
    ) -> bool {
        audit_passed_for_profile && self.evaluate(probed).permits_transfer()
    }
}

#[cfg(test)]
mod tests {
    use crate::register::EvidenceLevel;

    use super::*;

    fn probed() -> ProbedProfile {
        ProbedProfile {
            source_type: "plaud_device_ble".into(),
            device_model: "Note".into(),
            firmware: "V0095".into(),
            host_os: "macos".into(),
            os_version: "26.0".into(),
            transport: "ble".into(),
            auth_mode: "legacy_static_token".into(),
        }
    }

    fn cell(status: SupportStatus) -> CompatibilityCell {
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
            evidence_level: match status {
                SupportStatus::Supported => EvidenceLevel::ObservedOnIdentifiedHardware,
                SupportStatus::KnownUnsupported => EvidenceLevel::Unsupported,
                _ => EvidenceLevel::ImplementedNotVerified,
            },
            status,
            test_run_id: matches!(status, SupportStatus::Supported).then(|| "run-ble-001".into()),
            last_verified_at: matches!(status, SupportStatus::Supported)
                .then(|| "2026-10-11".into()),
            verified_import_recorded: matches!(status, SupportStatus::Supported),
            known_limitations: vec!["the listing is a delta of unsynchronized files".into()],
        }
    }

    fn policy(cells: Vec<CompatibilityCell>) -> GatePolicy {
        GatePolicy::new(CompatibilityRegister {
            schema_version: 1,
            updated_at: "2026-10-11".into(),
            cells,
        })
    }

    #[test]
    fn a_validated_cell_with_a_named_profile_is_permitted() {
        let decision = policy(vec![cell(SupportStatus::Supported)]).evaluate(&probed());
        let GateDecision::Permitted { protocol, cell } = &decision else {
            panic!("expected a permitted decision, got {decision:?}");
        };
        assert_eq!(protocol.name, "plaud-note-legacy-v0095");
        assert_eq!(cell.status, SupportStatus::Supported);
        assert_eq!(decision.ui_label(), "Supported");
        assert!(decision.permits_transfer());
    }

    #[test]
    fn an_unknown_tuple_grants_nothing_but_a_user_approved_probe() {
        let policy = policy(vec![cell(SupportStatus::Supported)]);
        for mutate in [
            |p: &mut ProbedProfile| p.firmware = "V0096".into(),
            |p: &mut ProbedProfile| p.os_version = "27.0".into(),
            |p: &mut ProbedProfile| p.device_model = "Note Pro".into(),
            |p: &mut ProbedProfile| p.host_os = "windows".into(),
            |p: &mut ProbedProfile| p.transport = "usb_mass_storage".into(),
            |p: &mut ProbedProfile| p.auth_mode = "encrypted_handshake".into(),
        ] {
            let mut near_miss = probed();
            mutate(&mut near_miss);
            let decision = policy.evaluate(&near_miss);
            assert!(!decision.permits_transfer(), "{near_miss:?} must fail closed");
            assert!(decision.offers_experimental_probe());
            assert_eq!(decision.ui_label(), "Not evaluated");
        }
    }

    #[test]
    fn an_incomplete_probe_is_refused_outright() {
        let policy = policy(vec![cell(SupportStatus::Supported)]);
        let mut incomplete = probed();
        incomplete.firmware = "   ".into();
        let decision = policy.evaluate(&incomplete);
        assert_eq!(
            decision,
            GateDecision::Refused {
                refusal: GateRefusal::IncompleteProbe
            }
        );
        assert!(!decision.offers_experimental_probe());
    }

    #[test]
    fn a_known_unsupported_cell_is_refused_with_its_recorded_reasons() {
        let decision = policy(vec![cell(SupportStatus::KnownUnsupported)]).evaluate(&probed());
        assert!(!decision.permits_transfer());
        assert!(!decision.offers_experimental_probe());
        assert_eq!(decision.ui_label(), "Known unsupported");
        assert!(!decision.known_limitations().is_empty());
    }

    #[test]
    fn a_supported_cell_for_a_model_with_no_named_profile_is_still_refused() {
        // The register may be edited; the wire profile table may not. A cell
        // claiming support for an unimplemented model must not cause the
        // legacy constants to be applied to it.
        let mut unnamed = cell(SupportStatus::Supported);
        unnamed.device_model = "Note Pro".into();
        let mut probe = probed();
        probe.device_model = "Note Pro".into();

        assert_eq!(
            policy(vec![unnamed]).evaluate(&probe),
            GateDecision::Refused {
                refusal: GateRefusal::NoNamedProtocolProfile
            }
        );
    }

    #[test]
    fn a_supported_cell_with_an_unknown_firmware_is_refused() {
        let mut future = cell(SupportStatus::Supported);
        future.firmware_range = vec!["V0200".into()];
        let mut probe = probed();
        probe.firmware = "V0200".into();
        assert_eq!(
            policy(vec![future]).evaluate(&probe),
            GateDecision::Refused {
                refusal: GateRefusal::NoNamedProtocolProfile
            }
        );
    }

    #[test]
    fn a_probe_grant_is_single_use_and_device_bound() {
        let policy = policy(vec![cell(SupportStatus::NotEvaluated)]);
        let decision = policy.evaluate(&probed());
        assert!(decision.offers_experimental_probe());

        let grant = ExperimentalProbeGrant::new("peripheral-1", 1_760_000_000);
        assert_eq!(
            ExperimentalProbeGrant::new("peripheral-1", 1).consume("peripheral-2", &decision),
            Err(ProbeGrantError::DeviceMismatch)
        );

        let session = grant.consume("peripheral-1", &decision).unwrap();
        assert!(session.permits_read_only_inspection());
        assert!(!session.permits_transfer());
        assert!(!session.permits_recurring_sync());
        // `grant` was moved into `consume`; a second probe needs a new approval.
    }

    #[test]
    fn a_grant_cannot_be_spent_where_no_probe_was_offered() {
        let decision = policy(vec![cell(SupportStatus::KnownUnsupported)]).evaluate(&probed());
        assert_eq!(
            ExperimentalProbeGrant::new("peripheral-1", 1).consume("peripheral-1", &decision),
            Err(ProbeGrantError::NotOffered)
        );
    }

    #[test]
    fn automatic_sync_requires_both_a_validated_cell_and_a_passing_audit() {
        let supported = policy(vec![cell(SupportStatus::Supported)]);
        assert!(supported.may_enable_automatic_sync(&probed(), true));
        assert!(
            !supported.may_enable_automatic_sync(&probed(), false),
            "a validated cell without an audit must not enable nearby sync"
        );

        let unevaluated = policy(vec![cell(SupportStatus::NotEvaluated)]);
        assert!(
            !unevaluated.may_enable_automatic_sync(&probed(), true),
            "an audit cannot promote an unevaluated cell"
        );
    }

    #[test]
    fn a_firmware_change_demotes_a_cell_and_closes_the_gate() {
        let supported = cell(SupportStatus::Supported);
        let demoted = supported.demoted("firmware changed from V0095 to V0096");
        let decision = policy(vec![demoted]).evaluate(&probed());
        assert!(!decision.permits_transfer());
        assert!(decision.offers_experimental_probe());
    }

    #[test]
    fn the_committed_register_grants_no_device_transfer_today() {
        // The shipped register has no `supported` device cell, so the gate is
        // closed for every device profile in this build.
        let register =
            CompatibilityRegister::from_yaml(include_str!("../../../compatibility.yaml")).unwrap();
        let policy = GatePolicy::new(register);
        assert!(!policy.evaluate(&probed()).permits_transfer());

        let mut usb = probed();
        usb.source_type = "plaud_device_usb".into();
        usb.transport = "usb_mass_storage".into();
        usb.auth_mode = "none".into();
        assert!(!policy.evaluate(&usb).permits_transfer());
    }
}
