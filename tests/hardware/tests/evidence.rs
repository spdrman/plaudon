//! Hermetic tests of the evidence emitter, its JSON Schema and the promotion
//! rules (spec §7.9, §18.6A, §24, App. B.9).

mod support;

use plaudon_hardware_tests::emitter;
use plaudon_hardware_tests::guard;
use plaudon_hardware_tests::observations::ObservedCheck;
use plaudon_hardware_tests::suite::{self, Assembly};
use plaudon_source_plaud_device::evidence::{
    CheckOutcome, EvidenceRecord, EvidenceRecordError, EvidenceSource,
};
use plaudon_source_plaud_device::register::SupportStatus;
use support::{complete_observations, mac_ble_profile, mac_host, register};

fn record_from(
    source: EvidenceSource,
    observations: &plaudon_hardware_tests::observations::Observations,
) -> EvidenceRecord {
    suite::assemble(&Assembly {
        test_run_id: "hw-macos-test-0001".into(),
        recorded_at: "2026-10-11".into(),
        source,
        profile: &mac_ble_profile(),
        host: &mac_host(),
        observations,
    })
}

fn complete(source: EvidenceSource) -> EvidenceRecord {
    record_from(source, &complete_observations())
}

fn validator() -> jsonschema::Validator {
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(emitter::schema_path()).expect("schema"))
            .expect("schema parses");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("schema compiles")
}

#[test]
fn the_assembled_record_carries_every_field_the_shipping_gate_names() {
    let record = complete(EvidenceSource::Hardware);
    assert_eq!(record.test_run_id, "hw-macos-test-0001");
    assert_eq!(record.recorded_at, "2026-10-11");
    assert_eq!(record.host_os, "macos");
    assert_eq!(record.host_os_build, "27.0 (27A123)");
    assert_eq!(record.device_model, "Note");
    assert_eq!(record.observed_firmware, "V0095");
    assert_eq!(record.transport, "ble");
    assert_eq!(record.auth_mode, "legacy_static_token");
    assert_eq!(record.provisioning_route, "imported_existing_token");
    assert_eq!(
        record.emitted_opcodes,
        ["authenticate", "listDelta", "requestData"]
    );
    assert!(record.non_destructive_scan_observed);
    assert!(record
        .transfer
        .as_ref()
        .is_some_and(plaudon_source_plaud_device::TransferEvidence::is_verified));
    assert_eq!(
        record.checks.len(),
        plaudon_hardware_tests::catalogue::CATALOGUE.len()
    );
}

#[test]
fn the_emitted_json_round_trips_through_a_file_the_runner_chooses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/run/evidence.json");
    let record = complete(EvidenceSource::Hardware);
    emitter::write_record(&path, &record).expect("write");
    assert_eq!(emitter::read_record(&path).expect("read"), record);
}

#[test]
fn the_emitted_document_satisfies_the_published_schema() {
    let value = serde_json::from_str::<serde_json::Value>(
        &emitter::to_json(&complete(EvidenceSource::Hardware)).unwrap(),
    )
    .unwrap();
    let errors: Vec<String> = validator()
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "schema errors: {errors:?}");
}

#[test]
fn a_record_without_a_transfer_and_a_simulator_record_also_satisfy_the_schema() {
    let mut observations = complete_observations();
    observations.transfer = None;
    let value =
        serde_json::to_value(record_from(EvidenceSource::Simulator, &observations)).unwrap();
    assert!(validator().is_valid(&value));
}

#[test]
fn the_schema_rejects_malformed_documents() {
    let good = serde_json::to_value(complete(EvidenceSource::Hardware)).unwrap();
    let validator = validator();
    for (name, mutate) in [
        (
            "missing test_run_id",
            Box::new(|v: &mut serde_json::Value| {
                v.as_object_mut().unwrap().remove("test_run_id");
            }) as Box<dyn Fn(&mut serde_json::Value)>,
        ),
        (
            "unknown source",
            Box::new(|v| v["source"] = "laboratory".into()),
        ),
        (
            "bad date",
            Box::new(|v| v["recorded_at"] = "11/10/2026".into()),
        ),
        (
            "bad outcome",
            Box::new(|v| v["checks"][0]["outcome"] = "maybe".into()),
        ),
        (
            "short digest",
            Box::new(|v| v["transfer"]["sha256_hex"] = "abc".into()),
        ),
        ("extra field", Box::new(|v| v["device_token"] = "x".into())),
        (
            "wrong version",
            Box::new(|v| v["schema_version"] = 2.into()),
        ),
    ] {
        let mut broken = good.clone();
        mutate(&mut broken);
        assert!(!validator.is_valid(&broken), "{name} must be rejected");
    }
}

#[test]
fn every_catalogued_check_is_recorded_with_a_known_outcome() {
    let record = complete(EvidenceSource::Hardware);
    for spec in plaudon_hardware_tests::catalogue::CATALOGUE {
        assert_eq!(
            record.check(spec.id),
            Some(CheckOutcome::Passed),
            "{} should be recorded",
            spec.id
        );
    }
}

#[test]
fn an_unobserved_check_is_recorded_skipped_not_passed() {
    let mut observations = complete_observations();
    observations.checks.remove("sleep_wake");
    let record = record_from(EvidenceSource::Hardware, &observations);
    assert_eq!(record.check("sleep_wake"), Some(CheckOutcome::Skipped));
}

#[test]
fn a_check_detail_with_a_credential_is_replaced_and_fails_the_check() {
    let mut observations = complete_observations();
    observations.checks.insert(
        "authorization_success".into(),
        ObservedCheck {
            outcome: CheckOutcome::Passed,
            detail: "authenticated with token=abcdef".into(),
        },
    );
    let record = record_from(EvidenceSource::Hardware, &observations);
    let result = record
        .checks
        .iter()
        .find(|c| c.id == "authorization_success")
        .unwrap();
    assert_eq!(result.outcome, CheckOutcome::Failed);
    assert_eq!(result.detail, guard::REFUSED_DETAIL);
    assert!(!emitter::to_json(&record).unwrap().contains("abcdef"));
}

#[test]
fn the_detail_guard_refuses_credentials_and_recording_content() {
    for refused in [
        "password: hunter2",
        "Authorization: Bearer abc",
        "api_key = sk-123",
        &"a".repeat(33),
        &"transcript words ".repeat(20),
    ] {
        assert!(guard::check_detail(refused).is_err(), "{refused:?}");
    }
    for accepted in [
        "device refused the wrong authorization",
        "3 opcodes, all read-only",
    ] {
        assert!(guard::check_detail(accepted).is_ok(), "{accepted:?}");
    }
}

#[test]
fn the_emitter_refuses_a_record_whose_detail_leaks() {
    let mut record = complete(EvidenceSource::Hardware);
    record.checks[0].detail = "secret=hunter2".into();
    assert!(matches!(
        emitter::to_json(&record),
        Err(emitter::EmitError::Detail { .. })
    ));
}

#[test]
fn the_opcode_check_is_derived_from_the_trace_and_cannot_be_overridden() {
    let mut observations = complete_observations();
    observations.emitted_opcodes.push("deleteFile".into());
    let record = record_from(EvidenceSource::Hardware, &observations);
    assert_eq!(
        record.check("no_device_write_opcodes"),
        Some(CheckOutcome::Failed)
    );

    let mut unknown = complete_observations();
    unknown.emitted_opcodes.push("mysteryOpcode".into());
    assert_eq!(
        record_from(EvidenceSource::Hardware, &unknown).check("no_device_write_opcodes"),
        Some(CheckOutcome::Failed),
        "an opcode outside the allowlist fails closed"
    );

    let mut empty = complete_observations();
    empty.emitted_opcodes.clear();
    assert_eq!(
        record_from(EvidenceSource::Hardware, &empty).check("no_device_write_opcodes"),
        Some(CheckOutcome::Skipped)
    );
}

#[test]
fn the_disconnect_aggregate_requires_every_stage() {
    let mut observations = complete_observations();
    observations.checks.remove("disconnect_at_streaming");
    assert_eq!(
        record_from(EvidenceSource::Hardware, &observations).check("disconnect_during_transfer"),
        Some(CheckOutcome::Skipped)
    );
    observations.checks.insert(
        "disconnect_at_streaming".into(),
        ObservedCheck {
            outcome: CheckOutcome::Failed,
            detail: "left a finished file".into(),
        },
    );
    assert_eq!(
        record_from(EvidenceSource::Hardware, &observations).check("disconnect_during_transfer"),
        Some(CheckOutcome::Failed)
    );
}

#[test]
fn claims_that_need_a_verified_transfer_or_a_denied_network_are_downgraded_without_one() {
    let mut no_transfer = complete_observations();
    no_transfer.transfer = None;
    let record = record_from(EvidenceSource::Hardware, &no_transfer);
    assert_eq!(
        record.check("imported_bytes_verified"),
        Some(CheckOutcome::Failed)
    );
    assert_eq!(
        record.check("transfer_verified"),
        Some(CheckOutcome::Skipped)
    );

    let mut unverified = complete_observations();
    unverified
        .transfer
        .as_mut()
        .unwrap()
        .terminal_marker_observed = false;
    assert_eq!(
        record_from(EvidenceSource::Hardware, &unverified).check("transfer_verified"),
        Some(CheckOutcome::Failed)
    );

    let mut online = complete_observations();
    online.network_denied_run = false;
    assert_eq!(
        record_from(EvidenceSource::Hardware, &online).check("network_denied"),
        Some(CheckOutcome::Failed)
    );
}

#[test]
fn a_simulator_record_never_promotes_a_cell() {
    let cell = mac_ble_profile().cell;
    let record = complete(EvidenceSource::Simulator);
    assert_eq!(
        record.promotes(&cell),
        Err(EvidenceRecordError::NotHardware)
    );
    assert!(record.promoted(&cell).is_err());
}

#[test]
fn a_record_missing_or_failing_any_required_check_never_promotes() {
    let cell = mac_ble_profile().cell;
    for required in EvidenceRecord::REQUIRED_CHECKS {
        let mut missing = complete(EvidenceSource::Hardware);
        missing.checks.retain(|c| c.id != *required);
        assert!(
            missing.promotes(&cell).is_err(),
            "missing {required} promoted"
        );

        for outcome in [CheckOutcome::Failed, CheckOutcome::Skipped] {
            let mut weakened = complete(EvidenceSource::Hardware);
            weakened
                .checks
                .iter_mut()
                .find(|c| c.id == *required)
                .unwrap()
                .outcome = outcome;
            assert_eq!(
                weakened.promotes(&cell),
                Err(EvidenceRecordError::CheckNotPassed {
                    id: (*required).to_owned()
                }),
                "{required} as {outcome:?} promoted"
            );
        }
    }
}

#[test]
fn a_record_that_a_run_could_not_fully_observe_never_promotes() {
    let cell = mac_ble_profile().cell;
    let mut observations = complete_observations();
    observations.checks.remove("disconnect_at_streaming");
    assert!(record_from(EvidenceSource::Hardware, &observations)
        .promotes(&cell)
        .is_err());

    let mut forbidden = complete_observations();
    forbidden.emitted_opcodes.push("otaBegin".into());
    assert!(record_from(EvidenceSource::Hardware, &forbidden)
        .promotes(&cell)
        .is_err());

    let mut destructive_scan = complete_observations();
    destructive_scan.non_destructive_scan_observed = false;
    assert!(record_from(EvidenceSource::Hardware, &destructive_scan)
        .promotes(&cell)
        .is_err());
}

#[test]
fn a_complete_hardware_record_promotes_exactly_the_matching_committed_cell() {
    let register = register();
    let record = complete(EvidenceSource::Hardware);
    let promoting: Vec<_> = register
        .cells
        .iter()
        .enumerate()
        .filter(|(_, cell)| record.promotes(cell).is_ok())
        .collect();
    assert_eq!(promoting.len(), 1, "exactly one committed cell must match");
    let (_, original) = promoting[0];
    assert_eq!(original.host_os, "macos");
    assert_eq!(original.auth_mode, "legacy_static_token");

    let promoted = record.promoted(original).expect("promotes");
    assert_eq!(promoted.status, SupportStatus::Supported);
    assert_eq!(promoted.test_run_id.as_deref(), Some("hw-macos-test-0001"));
    assert_eq!(promoted.last_verified_at.as_deref(), Some("2026-10-11"));
    assert!(promoted.verified_import_recorded);
    promoted
        .validate()
        .expect("a promoted cell satisfies the register rules");
}

#[test]
fn a_record_for_another_firmware_or_host_promotes_nothing() {
    let mut other_firmware = complete_observations();
    other_firmware.observed_firmware = "V9999".into();
    let register = register();
    let record = record_from(EvidenceSource::Hardware, &other_firmware);
    assert!(register
        .cells
        .iter()
        .all(|cell| record.promotes(cell).is_err()));
}
