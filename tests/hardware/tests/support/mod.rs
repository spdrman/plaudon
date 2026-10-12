//! Shared hermetic fixtures: a complete, passing set of observations.

#![allow(dead_code)]

use plaudon_hardware_tests::catalogue::CATALOGUE;
use plaudon_hardware_tests::host::HostInfo;
use plaudon_hardware_tests::observations::{Observations, ObservedCheck};
use plaudon_hardware_tests::profile::{self, ResolvedProfile};
use plaudon_source_plaud_device::evidence::{CheckOutcome, TransferEvidence};
use plaudon_source_plaud_device::register::CompatibilityRegister;

pub fn register() -> CompatibilityRegister {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compatibility.yaml");
    CompatibilityRegister::from_yaml(&std::fs::read_to_string(path).expect("read register"))
        .expect("committed register validates")
}

pub fn mac_ble_profile() -> ResolvedProfile {
    profile::resolve(
        "plaud_device_ble:note:legacy_static_token",
        "macos",
        &register(),
    )
    .expect("the macOS legacy BLE cell exists")
}

pub fn mac_host() -> HostInfo {
    HostInfo {
        os: "macos".into(),
        build: "27.0 (27A123)".into(),
    }
}

pub fn complete_observations() -> Observations {
    let mut observations = Observations {
        observed_firmware: "V0095".into(),
        emitted_opcodes: vec![
            "authenticate".into(),
            "listDelta".into(),
            "requestData".into(),
        ],
        non_destructive_scan_observed: true,
        network_denied_run: true,
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
        ..Observations::default()
    };
    for spec in CATALOGUE {
        observations.checks.insert(
            spec.id.to_owned(),
            ObservedCheck {
                outcome: CheckOutcome::Passed,
                detail: "observed on the device".into(),
            },
        );
    }
    observations
}
