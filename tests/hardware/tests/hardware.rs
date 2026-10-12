//! The real-hardware suite (spec §18.3A, App. B.9).
//!
//! Every test is `#[ignore]` **and** goes through `suite::execute`, which
//! consults `PLAUDON_HARDWARE_TESTS`. Default `cargo test` therefore lists them
//! as ignored; `cargo test -- --ignored` without the gate prints `SKIPPED` for
//! each; only `PLAUDON_HARDWARE_TESTS=1` plus a named profile and the run
//! harness observations file reaches hardware evidence. See `docs/hardware/`.

use plaudon_hardware_tests::gate::ProcessEnv;
use plaudon_hardware_tests::suite::{self, Execution};
use plaudon_source_plaud_device::evidence::CheckOutcome;

/// Execute one named check and fail the test only when the check failed.
///
/// A check with no observation is reported `SKIPPED` and recorded `skipped`;
/// it does not pass, and it blocks promotion.
fn run_check(id: &str) {
    match suite::execute(id, &ProcessEnv).unwrap_or_else(|error| panic!("{id}: {error}")) {
        Execution::Skipped(reason) => eprintln!("SKIPPED {id}: {reason:?}"),
        Execution::Ran(result) => match result.outcome {
            CheckOutcome::Passed => eprintln!("PASSED {id}: {}", result.detail),
            CheckOutcome::Skipped => eprintln!("SKIPPED {id}: {}", result.detail),
            CheckOutcome::Failed => panic!("{id} FAILED: {}", result.detail),
        },
    }
}

macro_rules! hardware_check {
    ($id:ident, $what:literal) => {
        #[doc = $what]
        #[test]
        #[ignore = "requires real PLAUD hardware; set PLAUDON_HARDWARE_TESTS=1"]
        fn $id() {
            run_check(stringify!($id));
        }
    };
}

hardware_check!(
    bluetooth_permission,
    "OS Bluetooth permission incl. macOS usage description."
);
hardware_check!(discover_only, "Scan finds the device, connects to nothing.");
hardware_check!(authorization_success, "Valid authorization authenticates.");
hardware_check!(
    authorization_failure,
    "Bad authorization is refused without leaking a secret."
);
hardware_check!(
    device_name_collision,
    "Same-name peripherals are not conflated."
);
hardware_check!(missing_secret, "A missing vault secret fails closed.");
hardware_check!(
    encrypted_handshake_detected,
    "Encrypted handshake is detected, legacy path refused."
);
hardware_check!(
    host_ble_exclusivity_with_mobile_app,
    "Mobile-app BLE conflict is reported."
);
hardware_check!(
    two_devices_in_range,
    "Two devices in range require explicit selection."
);
hardware_check!(
    listing_delta_before_after_phone_sync,
    "Delta list before and after phone sync."
);
hardware_check!(transfer_pull_15s, "15 s pull verifies.");
hardware_check!(transfer_pull_5min, "5 min pull verifies.");
hardware_check!(transfer_pull_30min, "30 min pull verifies.");
hardware_check!(
    transfer_pull_multi_hour,
    "Multi-hour pull verifies where storage permits."
);
hardware_check!(
    transfer_verified,
    "Terminal marker, length and digest agree."
);
hardware_check!(
    opus_decode_measurement,
    "Opus decodes and duration is measured."
);
hardware_check!(
    timestamp_verified_against_trusted_source,
    "Timestamp agrees with a trusted source."
);
hardware_check!(disconnect_at_connecting, "Disconnect while connecting.");
hardware_check!(
    disconnect_at_authenticating,
    "Disconnect while authenticating."
);
hardware_check!(disconnect_at_listing, "Disconnect while listing.");
hardware_check!(
    disconnect_at_requesting,
    "Disconnect after the data request."
);
hardware_check!(disconnect_at_streaming, "Disconnect mid-stream.");
hardware_check!(
    disconnect_at_finalizing,
    "Disconnect before verification completes."
);
hardware_check!(
    disconnect_during_transfer,
    "Every disconnect stage behaves."
);
hardware_check!(sleep_wake, "Host sleep and wake.");
hardware_check!(
    resume_or_restart,
    "Interrupted transfer resumes or restarts and verifies."
);
hardware_check!(low_battery, "Low battery ends cleanly.");
hardware_check!(full_disk, "Full disk ends cleanly.");
hardware_check!(
    imported_bytes_verified,
    "Imported bytes equal acquired bytes."
);
hardware_check!(
    no_device_write_opcodes,
    "Trace holds only read-only opcodes."
);
hardware_check!(
    usb_mount_and_eject,
    "USB mount, read-only scan, cancel, eject."
);
hardware_check!(
    network_denied,
    "Zero cloud requests with PLAUD/OpenRouter denied."
);
hardware_check!(
    recovery_after_process_kill,
    "Recovery after killing the app mid-transfer."
);

/// Emit the evidence record at the path the runner chose.
#[test]
#[ignore = "requires real PLAUD hardware; set PLAUDON_HARDWARE_TESTS=1"]
fn emit_evidence_record() {
    match suite::emit(&ProcessEnv).unwrap_or_else(|error| panic!("emit: {error}")) {
        None => eprintln!("SKIPPED emit_evidence_record: GateOff"),
        Some(record) => eprintln!(
            "EMITTED evidence record {} ({} checks)",
            record.test_run_id,
            record.checks.len()
        ),
    }
}
