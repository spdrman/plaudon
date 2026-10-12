//! Hermetic self-test of the opt-in gate: the default run is a no-op skip and
//! says so (issue L2.4, spec §18.3A).

use plaudon_hardware_tests::catalogue::{self, CATALOGUE};
use plaudon_hardware_tests::gate::{
    self, GateDecision, GateError, MapEnv, SkipReason, EVIDENCE_OUT_ENV, GATE_ENV,
    OBSERVATIONS_ENV, PROFILE_ENV,
};
use plaudon_hardware_tests::suite::{self, Execution, SuiteError};
use plaudon_source_plaud_device::evidence::EvidenceRecord;

#[test]
fn an_empty_environment_skips_every_check_instead_of_passing() {
    let env = MapEnv::new([]);
    for spec in CATALOGUE {
        let execution = suite::execute(spec.id, &env).expect("the gate is off, so no error");
        assert_eq!(
            execution,
            Execution::Skipped(SkipReason::GateOff),
            "{} must skip, not pass",
            spec.id
        );
    }
}

#[test]
fn only_the_exact_value_one_opts_in() {
    for value in ["", "0", "true", "yes", "1 ", "on"] {
        let env = MapEnv::new([
            (GATE_ENV, value),
            (PROFILE_ENV, "x:y:z"),
            (OBSERVATIONS_ENV, "/o"),
        ]);
        assert_eq!(
            gate::evaluate(&env),
            Ok(GateDecision::Skip(SkipReason::GateOff)),
            "value {value:?} must not opt in"
        );
    }
}

#[test]
fn opting_in_without_a_named_profile_is_an_error_not_a_run() {
    let env = MapEnv::new([(GATE_ENV, "1"), (OBSERVATIONS_ENV, "/o.json")]);
    assert_eq!(gate::evaluate(&env), Err(GateError::MissingProfile));
    let blank = MapEnv::new([
        (GATE_ENV, "1"),
        (PROFILE_ENV, "  "),
        (OBSERVATIONS_ENV, "/o.json"),
    ]);
    assert_eq!(gate::evaluate(&blank), Err(GateError::MissingProfile));
}

#[test]
fn opting_in_without_observations_is_an_error_not_a_run() {
    let env = MapEnv::new([
        (GATE_ENV, "1"),
        (PROFILE_ENV, "plaud_device_ble:note:legacy_static_token"),
    ]);
    assert_eq!(gate::evaluate(&env), Err(GateError::MissingObservations));
}

#[test]
fn a_fully_configured_opt_in_runs_and_carries_the_runner_paths() {
    let env = MapEnv::new([
        (GATE_ENV, "1"),
        (PROFILE_ENV, "plaud_device_ble:note:legacy_static_token"),
        (OBSERVATIONS_ENV, "/tmp/obs.json"),
        (EVIDENCE_OUT_ENV, "/tmp/evidence.json"),
    ]);
    let decision = gate::evaluate(&env).expect("configured");
    assert!(decision.is_run());
    let GateDecision::Run(config) = decision else {
        unreachable!()
    };
    assert_eq!(config.profile, "plaud_device_ble:note:legacy_static_token");
    assert_eq!(
        config.evidence_out.as_deref(),
        Some(std::path::Path::new("/tmp/evidence.json"))
    );
}

#[test]
fn the_gate_off_emits_no_evidence_record() {
    assert!(suite::emit(&MapEnv::new([])).expect("skip").is_none());
}

#[test]
fn an_unknown_check_is_an_error_even_with_the_gate_off() {
    assert!(matches!(
        suite::execute("not_a_check", &MapEnv::new([])),
        Err(SuiteError::UnknownCheck(_))
    ));
}

#[test]
fn the_real_process_environment_is_off_by_default_in_ci() {
    // CI never exports the gate; if a developer did, this test is meaningless
    // rather than wrong, so it only asserts the unset case.
    if std::env::var_os(GATE_ENV).is_none() {
        assert_eq!(
            gate::evaluate(&gate::ProcessEnv),
            Ok(GateDecision::Skip(SkipReason::GateOff))
        );
    }
}

#[test]
fn every_hardware_test_is_ignored_and_consults_the_gate() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/hardware.rs"),
    )
    .expect("read the hardware suite source");
    let mut tests = 0;
    for chunk in source.split("#[test]").skip(1) {
        tests += 1;
        assert!(
            chunk.trim_start().starts_with("#[ignore"),
            "every #[test] in the hardware suite must be followed by #[ignore]"
        );
    }
    assert!(tests >= 2, "the hardware suite declares its tests");
    assert!(
        source.contains("suite::execute"),
        "the macro must consult the gate"
    );
}

#[test]
fn the_hardware_suite_declares_a_test_for_every_catalogued_check() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/hardware.rs"),
    )
    .expect("read the hardware suite source");
    let compact: String = source.split_whitespace().collect();
    for id in catalogue::ids() {
        assert!(
            compact.contains(&format!("hardware_check!({id},")),
            "no hardware test for {id}"
        );
    }
}

#[test]
fn the_catalogue_covers_every_required_check_and_has_unique_ids() {
    let ids = catalogue::ids();
    for required in EvidenceRecord::REQUIRED_CHECKS {
        assert!(
            ids.contains(required),
            "{required} missing from the catalogue"
        );
    }
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "check ids must be unique");
}

#[test]
fn the_catalogue_covers_every_appendix_b9_item() {
    let ids = catalogue::ids();
    for needed in [
        "discover_only",
        "authorization_success",
        "authorization_failure",
        "device_name_collision",
        "missing_secret",
        "encrypted_handshake_detected",
        "listing_delta_before_after_phone_sync",
        "transfer_pull_15s",
        "transfer_pull_5min",
        "transfer_pull_30min",
        "transfer_pull_multi_hour",
        "opus_decode_measurement",
        "timestamp_verified_against_trusted_source",
        "disconnect_at_connecting",
        "disconnect_at_authenticating",
        "disconnect_at_listing",
        "disconnect_at_requesting",
        "disconnect_at_streaming",
        "disconnect_at_finalizing",
        "sleep_wake",
        "resume_or_restart",
        "low_battery",
        "full_disk",
        "imported_bytes_verified",
        "no_device_write_opcodes",
        "two_devices_in_range",
        "usb_mount_and_eject",
        "network_denied",
        "recovery_after_process_kill",
        "bluetooth_permission",
        "host_ble_exclusivity_with_mobile_app",
    ] {
        assert!(ids.contains(&needed), "{needed} missing");
    }
}
