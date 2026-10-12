//! DTO fixtures for the Devices and Sources screen tests.
//!
//! Every string a screen renders comes from the application, so a fixture is
//! where the test says what the application said. The labels here are exactly
//! the ones `plaudon-app` derives from the compatibility register and from the
//! `*_ui_label()` helpers, so a test that asserts on them is asserting the real
//! contract.

#![allow(dead_code)]

use plaudon_app::dto::{
    CloudRoute, DeviceCandidateDto, DeviceCapability, DeviceCapabilityDto, DeviceGateDto,
    DeviceIdentityAssurance, DeviceListingDto, DeviceProbeDto, DeviceRecordingDto, DeviceScanDto,
    DeviceSection, DeviceSupportStatus, DeviceSyncPolicyDto, DeviceTransferDto,
    DeviceTransferState, ListingCompleteness, PlaudCompatibility, PlaudConnectorSnapshot,
    PlaudConnectorState, PlaudConnectorStatus, PlaudDiagnostics, PlaudDisconnectCopy,
    PlaudDisconnectVariant, PlaudItemSummary, PlaudListResponse, PlaudListingCompleteness,
    PlaudNextAction, RadioState, RadioStatusDto,
};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::DiagnosticId;

pub fn envelope(message: &str, retryable: bool) -> ErrorEnvelope {
    ErrorEnvelope {
        code: AppErrorCode::SourceUnavailable,
        message: message.to_owned(),
        retryable,
        action: None,
        diagnostic_id: DiagnosticId::new(),
    }
}

pub fn candidate(peripheral_id: &str, section: DeviceSection) -> DeviceCandidateDto {
    DeviceCandidateDto {
        peripheral_id: peripheral_id.to_owned(),
        section,
        advertised_name: Some("PLAUD NotePin".to_owned()),
        rssi_dbm: Some(-58),
        transport: "ble".to_owned(),
        model: Some("NotePin".to_owned()),
        firmware: Some("1.4.2".to_owned()),
        support_status: DeviceSupportStatus::Supported,
        status_label: DeviceSupportStatus::Supported.ui_label().to_owned(),
        identity: DeviceIdentityAssurance::Unverified,
        identity_label: DeviceIdentityAssurance::Unverified.ui_label().to_owned(),
        authorized: false,
        last_contact_at: None,
    }
}

pub fn scan(devices: Vec<DeviceCandidateDto>) -> DeviceScanDto {
    DeviceScanDto {
        radio: RadioStatusDto {
            state: RadioState::Scanned,
            code: None,
            message: "The radio was used and the scan finished.".to_owned(),
        },
        devices,
        scanned_at: Some("2026-01-01T10:00:00Z".to_owned()),
    }
}

pub fn radio_refused(state: RadioState, message: &str) -> DeviceScanDto {
    DeviceScanDto {
        radio: RadioStatusDto {
            state,
            code: Some("ble_permission_denied".to_owned()),
            message: message.to_owned(),
        },
        devices: Vec::new(),
        scanned_at: None,
    }
}

pub fn sync_policy(peripheral_id: &str, may_advertise: bool, enabled: bool) -> DeviceSyncPolicyDto {
    DeviceSyncPolicyDto {
        peripheral_id: peripheral_id.to_owned(),
        may_advertise,
        enabled,
        default_on: false,
        audit_code: "sync_audit_passed".to_owned(),
        explanation: if may_advertise {
            "The profile was audited for unattended read-only transfer.".to_owned()
        } else {
            "The profile has not been audited for unattended transfer.".to_owned()
        },
    }
}

pub fn probe(peripheral_id: &str) -> DeviceProbeDto {
    DeviceProbeDto {
        peripheral_id: peripheral_id.to_owned(),
        advertised_name: Some("PLAUD NotePin".to_owned()),
        transport: "ble".to_owned(),
        model: Some("NotePin".to_owned()),
        firmware: Some("1.4.2".to_owned()),
        auth_mode: Some("static-token".to_owned()),
        battery_percent: Some(62),
        rssi_dbm: Some(-58),
        section: DeviceSection::NeedsAuthorization,
        support_status: DeviceSupportStatus::Supported,
        status_label: DeviceSupportStatus::Supported.ui_label().to_owned(),
        known_limitations: vec!["Transfers only the compressed on-device derivative.".to_owned()],
        gate: DeviceGateDto {
            permits_transfer: true,
            experimental_probe_available: false,
            experimental_probe_approved: false,
            refusal_code: None,
        },
        identity: DeviceIdentityAssurance::Unverified,
        identity_label: DeviceIdentityAssurance::Unverified.ui_label().to_owned(),
        capabilities: vec![
            DeviceCapabilityDto {
                capability: DeviceCapability::ReadOnlyListing,
                available: true,
                reason: None,
            },
            DeviceCapabilityDto {
                capability: DeviceCapability::ReadOnlyTransfer,
                available: true,
                reason: None,
            },
            DeviceCapabilityDto {
                capability: DeviceCapability::AutomaticSync,
                available: false,
                reason: Some("The user has not opted in.".to_owned()),
            },
        ],
        source_label: "Device BLE · compressed ASR".to_owned(),
        cloud_route: CloudRoute::NotUsed,
        disclosures: vec![
            "A Bluetooth scan is visible to devices nearby while it runs.".to_owned(),
        ],
        provisioning_note: Some(
            "Import the token you already hold for your own device.".to_owned(),
        ),
        authorized: true,
        last_contact_at: Some("2026-01-01T09:00:00Z".to_owned()),
        sync: sync_policy(peripheral_id, true, false),
    }
}

/// A probe of a profile nothing has been validated for: no transfer, and the
/// user may opt in to a read-only experimental probe.
pub fn experimental_probe(peripheral_id: &str) -> DeviceProbeDto {
    let mut probe = probe(peripheral_id);
    probe.support_status = DeviceSupportStatus::Experimental;
    DeviceSupportStatus::Experimental
        .ui_label()
        .clone_into(&mut probe.status_label);
    probe.gate = DeviceGateDto {
        permits_transfer: false,
        experimental_probe_available: true,
        experimental_probe_approved: false,
        refusal_code: Some("experimental_profile".to_owned()),
    };
    probe.capabilities[1].available = false;
    probe.capabilities[1].reason =
        Some("The register has no validated evidence for this firmware.".to_owned());
    probe.sync = sync_policy(peripheral_id, false, false);
    probe
}

pub fn transfer(state: DeviceTransferState) -> DeviceTransferDto {
    DeviceTransferDto {
        transfer_id: Some("transfer-1".to_owned()),
        state,
        state_label: state.ui_label().to_owned(),
        bytes_received: None,
        observed_bytes_per_sec: None,
        eta_seconds_estimate: None,
        verified: false,
        source_label: "Device BLE · compressed ASR".to_owned(),
        cloud_route: CloudRoute::NotUsed,
    }
}

pub fn recording(id: &str, state: DeviceTransferState) -> DeviceRecordingDto {
    DeviceRecordingDto {
        recording_id: id.to_owned(),
        display_name: format!("Recording {id}"),
        declared_byte_length: 4_194_304,
        captured_at: Some("2026-01-01T08:00:00Z".to_owned()),
        transfer: transfer(state),
    }
}

pub fn listing(peripheral_id: &str, items: Vec<DeviceRecordingDto>) -> DeviceListingDto {
    let file_count = u32::try_from(items.len()).unwrap_or(u32::MAX);
    DeviceListingDto {
        peripheral_id: peripheral_id.to_owned(),
        items,
        file_count,
        completeness: ListingCompleteness::UnsyncedDelta,
        completeness_label:
            "This is the set of files the device has not yet synced, not a complete history."
                .to_owned(),
        identity: DeviceIdentityAssurance::ProfileAuthenticated,
        identity_label: DeviceIdentityAssurance::ProfileAuthenticated
            .ui_label()
            .to_owned(),
    }
}

/* ------------------------------------------------------------------ sources */

pub fn connector(
    state: PlaudConnectorState,
    headline: &str,
    detail: &str,
    next_action: PlaudNextAction,
) -> PlaudConnectorSnapshot {
    PlaudConnectorSnapshot {
        status: PlaudConnectorStatus {
            state,
            detected_version: Some("0.9.1".to_owned()),
            version_validated: false,
            program_origin: Some("/usr/local/bin/plaud".to_owned()),
            last_error: None,
            listing_possibly_incomplete: true,
            scraping_warning: true,
            updated_at_ms: 1_767_225_600_000,
        },
        diagnostics: PlaudDiagnostics {
            state,
            headline: headline.to_owned(),
            detail: detail.to_owned(),
            next_action,
            detected_version: Some("0.9.1".to_owned()),
            compatibility: PlaudCompatibility::Untested,
            program_origin: Some("/usr/local/bin/plaud".to_owned()),
            last_error: None,
            warnings: vec![
                "No version of the official tool has been validated against this build.".to_owned(),
            ],
        },
        disconnect: PlaudDisconnectCopy {
            variant: PlaudDisconnectVariant::AssociationOnly,
            label: PlaudDisconnectVariant::AssociationOnly.label().to_owned(),
            body: PlaudDisconnectVariant::AssociationOnly.body().to_owned(),
        },
    }
}

pub fn ready_connector() -> PlaudConnectorSnapshot {
    let mut snapshot = connector(
        PlaudConnectorState::Ready,
        "The PLAUD tool is signed in.",
        "The official tool answered and a listing is available.",
        PlaudNextAction::None,
    );
    snapshot.status.version_validated = true;
    snapshot.diagnostics.compatibility = PlaudCompatibility::Tested;
    snapshot.diagnostics.warnings.clear();
    snapshot.disconnect = PlaudDisconnectCopy {
        variant: PlaudDisconnectVariant::WithVendorLogout,
        label: PlaudDisconnectVariant::WithVendorLogout.label().to_owned(),
        body: PlaudDisconnectVariant::WithVendorLogout.body().to_owned(),
    };
    snapshot
}

pub fn cloud_item(external_id: &str, imported: bool) -> PlaudItemSummary {
    PlaudItemSummary {
        external_id: external_id.to_owned(),
        display_name: format!("Cloud recording {external_id}"),
        captured_at: Some("2026-01-01T07:00:00Z".to_owned()),
        duration_ms: Some(65_000),
        declared_byte_length: Some(2_097_152),
        imported_recording_id: imported.then(plaudon_domain::ids::RecordingId::new),
    }
}

pub fn cloud_listing(items: Vec<PlaudItemSummary>) -> PlaudListResponse {
    PlaudListResponse {
        source_id: "8f14e45f-ea0a-4f36-9f1a-000000000001".to_owned(),
        items,
        next_cursor: None,
        completeness: PlaudListingCompleteness::PossiblyIncomplete,
        empty_account_advisory: Some(
            "The account listed no recordings. That is not an error.".to_owned(),
        ),
    }
}
