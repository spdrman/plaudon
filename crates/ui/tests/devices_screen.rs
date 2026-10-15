//! Interaction tests for the Devices destination, driven through AccessKit by
//! `egui_kittest` (docs/EGUI_TESTING.md).
//!
//! Every assertion is made the way a user or a screen reader would make it:
//! find a control by its accessible label, act on it, and check the intent the
//! screen produced and the words it published. No test reaches into private
//! state to decide what happened.

mod fixtures;

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    DeviceAuthStatusDto, DeviceDisconnectDto, DeviceIdentityAssurance, DeviceImportDto,
    DeviceSection, DeviceTransferState, RadioState,
};
use plaudon_ui::screens::devices::{
    devices_screen, DeviceIntent, DeviceOp, DeviceOutcome, DevicesState, SCAN_TIMEOUT_MS,
};
use plaudon_ui_testkit::a11y;

/// The screen plus the intents it produced, which is everything a host sees.
struct DevicesApp {
    state: DevicesState,
    intents: Vec<DeviceIntent>,
}

impl DevicesApp {
    fn new() -> Self {
        Self {
            state: DevicesState::new(),
            intents: Vec::new(),
        }
    }
}

/// A tall window, because the destination is a long page and a control that is
/// scrolled out of view cannot be clicked by a pointer. The width is the
/// harness default, so the layout under test is the real one.
fn harness(app: DevicesApp) -> Harness<'static, DevicesApp> {
    plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 4000.0))
        .build_ui_state(
            |ui, app: &mut DevicesApp| {
                let intents = devices_screen(ui, &mut app.state);
                app.intents.extend(intents);
            },
            app,
        )
}

fn labels(harness: &Harness<'_, DevicesApp>) -> Vec<String> {
    a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect()
}

fn says(harness: &Harness<'_, DevicesApp>, needle: &str) -> bool {
    labels(harness).iter().any(|label| label.contains(needle))
}

#[test]
fn nothing_is_scanned_until_the_user_asks() {
    let mut harness = harness(DevicesApp::new());
    harness.run();

    assert!(harness.state().intents.is_empty(), "no command on open");
    assert!(says(&harness, "No scan yet"));

    harness.get_by_label("Start the first scan").click();
    harness.run();

    assert_eq!(harness.state().intents, vec![DeviceIntent::Scan]);
    let command = DeviceIntent::Scan.into_command().expect("a command");
    assert!(
        matches!(command, AppCommand::ListNearbyDevices(request) if request.timeout_ms == SCAN_TIMEOUT_MS)
    );
}

#[test]
fn a_denied_radio_is_not_no_devices_nearby() {
    let mut app = DevicesApp::new();
    app.state
        .applied(DeviceOutcome::Scan(fixtures::radio_refused(
            RadioState::PermissionDenied,
            "Bluetooth permission has not been granted to Plaudon.",
        )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Bluetooth access is not permitted"));
    assert!(says(
        &harness,
        "Bluetooth permission has not been granted to Plaudon."
    ));
    assert!(
        says(
            &harness,
            "Not checked: the radio could not be used for this scan."
        ),
        "an unscanned section must not read as empty"
    );
    assert!(
        !says(&harness, "No PLAUD devices were heard in this scan."),
        "nothing may claim the room was heard"
    );
    assert!(
        !says(&harness, "No PLAUD device found"),
        "a refused radio is never 'no device found'"
    );
    harness.get_by_label("Import audio files instead").click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&DeviceIntent::ImportAudioFiles));
}

#[test]
fn a_build_without_a_radio_says_so_in_its_own_words() {
    let mut app = DevicesApp::new();
    app.state
        .applied(DeviceOutcome::Scan(fixtures::radio_refused(
            RadioState::UnavailableInBuild,
            "This build was compiled without a Bluetooth radio.",
        )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(
        &harness,
        "This build cannot scan for Bluetooth devices"
    ));
    assert!(!says(&harness, "Bluetooth access is not permitted"));
}

/* ------------------------------------------------ radio refusals (#301) */

/// A radio state by its wire name, so these tests compile against a DTO that
/// does not have the state yet and fail on the missing behaviour instead.
fn issue_301_state(wire: &str) -> RadioState {
    serde_json::from_value(serde_json::Value::String(wire.to_owned()))
        .unwrap_or_else(|_| panic!("{wire} is not a radio state"))
}

/// The settings control this host offers. macOS has a Bluetooth row under
/// Privacy & Security; Windows has its Bluetooth page; a Linux desktop has no
/// settings link every environment honours, so it gets no control at all
/// rather than one that silently does nothing.
fn issue_301_settings_control() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("Open System Settings → Privacy & Security → Bluetooth")
    } else if cfg!(target_os = "windows") {
        Some("Open Settings → Bluetooth & devices")
    } else {
        None
    }
}

fn issue_301_refused(wire_state: &str, code: &str, message: &str) -> Harness<'static, DevicesApp> {
    let mut app = DevicesApp::new();
    let mut scan = fixtures::radio_refused(issue_301_state(wire_state), message);
    scan.radio.code = Some(code.to_owned());
    app.state.applied(DeviceOutcome::Scan(scan));
    let mut harness = harness(app);
    harness.run();
    harness
}

/// Every refusal names its own cause, shows its reason code, never reads as an
/// empty room, and offers the settings control where the host has one.
fn issue_301_assert_refusal(wire_state: &str, code: &str, heading: &str) {
    let message = format!("Radio message for {wire_state}.");
    let mut harness = issue_301_refused(wire_state, code, &message);

    assert!(says(&harness, heading), "{wire_state}: heading {heading:?} missing");
    assert!(says(&harness, &message), "{wire_state}: the DTO message is shown");
    assert!(
        says(&harness, &format!("Reason code: {code}")),
        "{wire_state}: the reason code is shown"
    );
    assert!(
        !says(&harness, "This build cannot scan for Bluetooth devices"),
        "{wire_state}: a real radio is not an unavailable build"
    );
    assert!(
        !says(&harness, "No PLAUD device found"),
        "{wire_state}: a refused radio is never 'no device found'"
    );
    assert!(says(
        &harness,
        "Not checked: the radio could not be used for this scan."
    ));

    if let Some(control) = issue_301_settings_control() {
        harness.get_by_label(control).click();
        harness.run();
        let intents = format!("{:?}", harness.state().intents);
        assert!(
            intents.contains("OpenSystemSettings(BluetoothPrivacy)"),
            "{wire_state}: the control asks the host to open the Bluetooth privacy pane, \
             got {intents}"
        );
    }
}

#[test]
fn issue_301_an_unanswered_permission_prompt_is_its_own_state() {
    issue_301_assert_refusal(
        "permission_not_determined",
        "RADIO_PERMISSION_NOT_DETERMINED",
        "Bluetooth access has not been granted yet",
    );
}

#[test]
fn issue_301_a_powered_off_adapter_is_its_own_state() {
    issue_301_assert_refusal(
        "powered_off",
        "RADIO_POWERED_OFF",
        "Bluetooth is turned off",
    );
}

#[test]
fn issue_301_a_missing_adapter_is_its_own_state() {
    issue_301_assert_refusal(
        "no_adapter",
        "RADIO_NO_ADAPTER",
        "No Bluetooth adapter is available",
    );
}

#[test]
fn issue_301_a_denied_radio_offers_the_settings_control_too() {
    issue_301_assert_refusal(
        "permission_denied",
        "RADIO_PERMISSION_DENIED",
        "Bluetooth access is not permitted",
    );
}

#[test]
fn issue_301_a_build_without_a_radio_offers_no_settings_control() {
    let harness = issue_301_refused(
        "unavailable_in_build",
        "SOURCE_UNAVAILABLE",
        "This build was compiled without a Bluetooth radio.",
    );
    // No setting can give a binary a radio it was not built with.
    for control in [
        "Open System Settings → Privacy & Security → Bluetooth",
        "Open Settings → Bluetooth & devices",
    ] {
        assert!(!says(&harness, control), "{control} offered for a radio-less build");
    }
}

#[test]
fn a_scan_that_heard_nothing_offers_the_corrective_actions() {
    let mut app = DevicesApp::new();
    app.state.applied(DeviceOutcome::Scan(fixtures::scan(vec![
        fixtures::candidate("old", DeviceSection::PreviouslyLinked),
    ])));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "No PLAUD device found"));
    assert!(says(
        &harness,
        "Check that the device is powered on and charged."
    ));
    assert!(says(&harness, "Close the PLAUD phone app"));
}

#[test]
fn the_five_appendix_sections_are_all_rendered_with_their_counts() {
    let mut app = DevicesApp::new();
    app.state.applied(DeviceOutcome::Scan(fixtures::scan(vec![
        fixtures::candidate("near", DeviceSection::Nearby),
        fixtures::candidate("auth", DeviceSection::NeedsAuthorization),
    ])));
    let mut harness = harness(app);
    harness.run();

    for heading in [
        "Nearby (1)",
        "Connected (0)",
        "Needs authorization (1)",
        "Unsupported firmware (0)",
        "Previously linked (0)",
    ] {
        assert!(says(&harness, heading), "missing section heading {heading}");
    }
    assert!(says(&harness, "No device is connected."));
}

#[test]
fn opening_a_device_probes_it_and_shows_the_registers_own_words() {
    let mut app = DevicesApp::new();
    app.state.applied(DeviceOutcome::Scan(fixtures::scan(vec![
        fixtures::candidate("dev-1", DeviceSection::NeedsAuthorization),
    ])));
    let mut harness = harness(app);
    harness.run();

    harness
        .get_by_label("Open details of PLAUD NotePin")
        .click();
    harness.run();
    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::Probe {
            peripheral_id: "dev-1".to_owned(),
            experimental_probe_approved: false,
        }]
    );

    harness
        .state_mut()
        .state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    harness.run();

    assert!(says(&harness, "Model: NotePin"));
    assert!(says(&harness, "Firmware: 1.4.2"));
    assert!(says(&harness, "Transport: Bluetooth LE (BLE)"));
    assert!(says(&harness, "Battery: 62%"));
    assert!(says(&harness, "Signal strength: -58 dBm"));
    assert!(says(&harness, "Supported"), "the register's own word");
    assert!(says(&harness, "Unverified identity"));
    assert!(says(
        &harness,
        "Transfers only the compressed on-device derivative."
    ));
    assert!(says(
        &harness,
        "A Bluetooth scan is visible to devices nearby while it runs."
    ));
    assert!(says(&harness, "Device BLE · compressed ASR"));
    assert!(says(&harness, "Cloud: not used"));
}

#[test]
fn facts_the_device_did_not_report_stay_unknown() {
    let mut app = DevicesApp::new();
    let mut candidate = fixtures::candidate("dev-1", DeviceSection::Nearby);
    candidate.model = None;
    candidate.firmware = None;
    candidate.rssi_dbm = None;
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    let intent = app.state.open(&candidate);
    assert!(intent.is_some());
    let mut probe = fixtures::probe("dev-1");
    probe.model = None;
    probe.firmware = None;
    probe.battery_percent = None;
    probe.rssi_dbm = None;
    probe.auth_mode = None;
    app.state.applied(DeviceOutcome::Probe(Box::new(probe)));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Model: Unknown"));
    assert!(says(&harness, "Firmware: Unknown"));
    assert!(says(&harness, "Battery: Unknown"));
    assert!(says(&harness, "Signal strength: Unknown"));
    assert!(says(&harness, "Authentication mode: Unknown"));
    assert!(
        !says(&harness, "0 dBm"),
        "an unknown signal is never rendered as a number"
    );
}

#[test]
fn a_previously_linked_device_is_not_probed_and_says_why() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("old", DeviceSection::PreviouslyLinked);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    assert_eq!(app.state.open(&candidate), None, "nothing is probed");
    let mut harness = harness(app);
    harness.run();

    assert!(says(
        &harness,
        "This device is not advertising right now. Bring it within range and scan again to check it."
    ));
}

#[test]
fn the_experimental_probe_is_an_explicit_opt_in() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-x", DeviceSection::Nearby);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state.applied(DeviceOutcome::Probe(Box::new(
        fixtures::experimental_probe("dev-x"),
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Experimental"));
    assert!(says(
        &harness,
        "The register has no validated evidence for this firmware."
    ));
    harness
        .get_by_label(
            "Allow a read-only experimental probe of this device (listing only, no transfer)",
        )
        .click();
    harness.run();

    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::Probe {
            peripheral_id: "dev-x".to_owned(),
            experimental_probe_approved: true,
        }]
    );
}

#[test]
fn an_unaudited_profile_is_never_offered_automatic_sync() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-x", DeviceSection::Nearby);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state.applied(DeviceOutcome::Probe(Box::new(
        fixtures::experimental_probe("dev-x"),
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(
        &harness,
        "Not offered. The profile has not been audited for unattended transfer."
    ));
    assert!(
        harness
            .query_all_by_label(
                "Transfer recordings automatically when this device is nearby (read-only)"
            )
            .next()
            .is_none(),
        "the control may not even be offered"
    );
}

#[test]
fn opting_in_to_automatic_sync_sends_the_policy_command() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Nearby);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    let mut harness = harness(app);
    harness.run();

    harness
        .get_by_label("Transfer recordings automatically when this device is nearby (read-only)")
        .click();
    harness.run();

    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::SetSyncPolicy {
            peripheral_id: "dev-1".to_owned(),
            enable_when_nearby: true,
        }]
    );
    harness
        .state_mut()
        .state
        .applied(DeviceOutcome::SyncPolicy(fixtures::sync_policy(
            "dev-1", true, true,
        )));
    harness.run();
    assert!(harness
        .state()
        .state
        .probe()
        .is_some_and(|probe| probe.sync.enabled));
}

#[test]
fn the_authorization_token_is_handed_over_once_and_never_rendered_again() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::NeedsAuthorization);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    let mut probe = fixtures::probe("dev-1");
    probe.authorized = false;
    app.state.applied(DeviceOutcome::Probe(Box::new(probe)));
    let mut harness = harness(app);
    harness.run();

    assert!(says(
        &harness,
        "Import the token you already hold for your own device."
    ));
    let field = harness.get_by_label("Authorization token (hexadecimal)");
    field.focus();
    field.type_text("a1b2c3d4");
    harness.run();
    harness.get_by_label("Store in system keychain").click();
    harness.run();

    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::Authorize {
            peripheral_id: "dev-1".to_owned(),
            token_hex: "a1b2c3d4".to_owned(),
        }]
    );
    // The token left the screen's state with the command.
    assert!(
        !labels(&harness)
            .iter()
            .any(|label| label.contains("a1b2c3d4")),
        "the token must not be in the accessibility tree after it is handed over"
    );
    // Nor in a log line: `Debug` prints the peripheral and nothing else.
    let printed = format!("{:?}", harness.state().intents[0]);
    assert!(!printed.contains("a1b2c3d4"), "{printed}");

    harness
        .state_mut()
        .state
        .applied(DeviceOutcome::Authorized(DeviceAuthStatusDto {
            peripheral_id: "dev-1".to_owned(),
            credential_stored: true,
            identity: DeviceIdentityAssurance::Unverified,
            identity_label: DeviceIdentityAssurance::Unverified.ui_label().to_owned(),
        }));
    harness.run();
    assert!(
        !labels(&harness)
            .iter()
            .any(|label| label.contains("a1b2c3d4")),
        "and never after the answer either"
    );
}

#[test]
fn a_listing_is_never_presented_as_a_complete_archive() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    let mut harness = harness(app);
    harness.run();

    harness
        .get_by_label("List recordings on the device")
        .click();
    harness.run();
    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::ListRecordings {
            peripheral_id: "dev-1".to_owned()
        }]
    );

    harness
        .state_mut()
        .state
        .applied(DeviceOutcome::Listing(fixtures::listing(
            "dev-1",
            vec![fixtures::recording(
                "rec-1",
                DeviceTransferState::Discovered,
            )],
        )));
    harness.run();

    assert!(says(
        &harness,
        "This is the set of files the device has not yet synced, not a complete history."
    ));
    assert!(says(&harness, "Files on the device: 1"));
    assert!(says(&harness, "On device"), "the ledger's own state label");
    assert!(says(&harness, "Profile-authenticated"));
}

#[test]
fn choosing_recordings_imports_them_for_local_processing_only() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    app.state.applied(DeviceOutcome::Listing(fixtures::listing(
        "dev-1",
        vec![fixtures::recording(
            "rec-1",
            DeviceTransferState::Discovered,
        )],
    )));
    let mut harness = harness(app);
    harness.run();

    harness.get_by_label("Recording rec-1").click();
    harness.run();
    harness
        .get_by_label("Import selected for local processing")
        .click();
    harness.run();

    let intent = harness
        .state()
        .intents
        .iter()
        .find(|intent| matches!(intent, DeviceIntent::Import { .. }))
        .expect("an import intent");
    assert_eq!(
        *intent,
        DeviceIntent::Import {
            peripheral_id: "dev-1".to_owned(),
            recording_ids: vec!["rec-1".to_owned()],
        }
    );
    let command = DeviceIntent::Import {
        peripheral_id: "dev-1".to_owned(),
        recording_ids: vec!["rec-1".to_owned()],
    }
    .into_command()
    .expect("a command");
    assert!(
        matches!(command, AppCommand::DeviceImport(request) if request.local_only),
        "a device transfer is always local-only"
    );

    harness
        .state_mut()
        .state
        .applied(DeviceOutcome::ImportStarted(DeviceImportDto {
            transfer_ids: vec!["transfer-1".to_owned()],
        }));
    harness.run();
}

#[test]
fn a_running_transfer_shows_observed_bandwidth_and_an_estimate_labelled_as_one() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    let mut item = fixtures::recording("rec-1", DeviceTransferState::Receiving);
    item.transfer.bytes_received = Some(1_048_576);
    item.transfer.observed_bytes_per_sec = Some(65_536);
    item.transfer.eta_seconds_estimate = Some(48);
    app.state.applied(DeviceOutcome::Listing(fixtures::listing(
        "dev-1",
        vec![item],
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Transferring"));
    assert!(says(&harness, "Observed bandwidth: 64.0 KiB/s"));
    assert!(says(
        &harness,
        "Estimated time remaining (an estimate, not a promise): about 48 s"
    ));
    assert!(says(&harness, "1.0 MiB of the size the device declared"));
}

#[test]
fn an_estimate_that_cannot_be_made_is_not_invented() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    app.state.applied(DeviceOutcome::Listing(fixtures::listing(
        "dev-1",
        vec![fixtures::recording("rec-1", DeviceTransferState::Receiving)],
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Observed bandwidth: Not measured yet"));
    assert!(says(&harness, "Cannot be estimated yet"));
}

#[test]
fn cancelling_a_transfer_disconnects_and_keeps_the_archive() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    app.state.applied(DeviceOutcome::Listing(fixtures::listing(
        "dev-1",
        vec![fixtures::recording("rec-1", DeviceTransferState::Receiving)],
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(
        &harness,
        "Cancelling ends this session with the device. Recordings already in your library are kept"
    ));
    harness
        .get_by_label("Cancel transfer of Recording rec-1")
        .click();
    harness.run();

    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::Disconnect {
            peripheral_id: "dev-1".to_owned(),
            forget_authorization: false,
        }]
    );

    harness
        .state_mut()
        .state
        .applied(DeviceOutcome::Disconnected(DeviceDisconnectDto {
            peripheral_id: "dev-1".to_owned(),
            local_archive_retained: true,
            authorization_revoked: false,
        }));
    harness.run();
    assert!(says(
        &harness,
        "Recordings already in your library were kept."
    ));
    assert!(harness.state().state.listing().is_none());
}

#[test]
fn an_interrupted_transfer_can_be_recovered_and_says_what_was_kept() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    let mut item = fixtures::recording("rec-1", DeviceTransferState::Interrupted);
    item.transfer.bytes_received = Some(524_288);
    app.state.applied(DeviceOutcome::Listing(fixtures::listing(
        "dev-1",
        vec![item],
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Interrupted"));
    assert!(says(
        &harness,
        "Nothing partial was added to your library; the bytes that did arrive are staged on this computer and are verified before anything is kept."
    ));

    harness.get_by_label("Try Recording rec-1 again").click();
    harness.run();
    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::Import {
            peripheral_id: "dev-1".to_owned(),
            recording_ids: vec!["rec-1".to_owned()],
        }]
    );
}

#[test]
fn an_unsupported_transfer_offers_no_retry() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    app.state.applied(DeviceOutcome::Listing(fixtures::listing(
        "dev-1",
        vec![fixtures::recording(
            "rec-1",
            DeviceTransferState::Unsupported,
        )],
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Unsupported"));
    assert!(says(
        &harness,
        "is not one this build can read, so there is nothing to retry"
    ));
    assert!(harness
        .query_all_by_label("Try Recording rec-1 again")
        .next()
        .is_none());
}

#[test]
fn a_transfer_event_refreshes_the_listing_and_only_after_one_was_asked_for() {
    let mut state = DevicesState::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    state.applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    state.open(&candidate);
    assert_eq!(
        state.device_event("dev-1"),
        None,
        "nothing is listed, so nothing is refreshed"
    );

    state.applied(DeviceOutcome::Listing(fixtures::listing(
        "dev-1",
        vec![fixtures::recording("rec-1", DeviceTransferState::Receiving)],
    )));
    assert_eq!(
        state.device_event("another-device"),
        None,
        "another device's progress is not ours"
    );
    assert_eq!(
        state.device_event("dev-1"),
        Some(DeviceIntent::ListRecordings {
            peripheral_id: "dev-1".to_owned()
        })
    );
    assert!(state.is_pending(DeviceOp::List));
    assert_eq!(
        state.device_event("dev-1"),
        None,
        "one refresh at a time; the screen never polls"
    );
}

#[test]
fn forgetting_an_authorization_is_confirmed_before_it_happens() {
    let mut app = DevicesApp::new();
    let candidate = fixtures::candidate("dev-1", DeviceSection::Connected);
    app.state
        .applied(DeviceOutcome::Scan(fixtures::scan(vec![candidate.clone()])));
    app.state.open(&candidate);
    app.state
        .applied(DeviceOutcome::Probe(Box::new(fixtures::probe("dev-1"))));
    let mut harness = harness(app);
    harness.run();

    harness.get_by_label("Forget authorization…").click();
    harness.run();
    assert!(says(
        &harness,
        "The stored token is removed from the system keychain. Recordings already in your library are kept."
    ));
    assert!(harness.state().intents.is_empty(), "nothing happened yet");

    harness.get_by_label("Forget authorization").click();
    harness.run();
    assert_eq!(
        harness.state().intents,
        vec![DeviceIntent::Disconnect {
            peripheral_id: "dev-1".to_owned(),
            forget_authorization: true,
        }]
    );

    harness
        .state_mut()
        .state
        .applied(DeviceOutcome::Disconnected(DeviceDisconnectDto {
            peripheral_id: "dev-1".to_owned(),
            local_archive_retained: true,
            authorization_revoked: true,
        }));
    harness.run();
    assert!(says(
        &harness,
        "The stored authorization was removed from the system keychain. Recordings already in your library were kept."
    ));
}

#[test]
fn a_failed_scan_renders_the_envelope_and_offers_the_retry_it_allows() {
    let mut app = DevicesApp::new();
    app.state.applied(DeviceOutcome::Failed {
        op: DeviceOp::Scan,
        error: fixtures::envelope("The Bluetooth adapter did not answer.", true),
    });
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "The Bluetooth adapter did not answer."));
    harness.get_by_label("Try the scan again").click();
    harness.run();
    assert_eq!(harness.state().intents, vec![DeviceIntent::Scan]);
}

#[test]
fn the_setup_wizard_offers_exactly_three_routes() {
    let mut harness = harness(DevicesApp::new());
    harness.run();
    harness.get_by_label("Add PLAUD device").click();
    harness.run();

    for option in [
        "Connect directly (offline after setup)",
        "Use PLAUD account",
        "Import files",
    ] {
        assert!(says(&harness, option), "missing wizard option {option}");
    }

    harness
        .get_by_label("Connect directly (offline after setup)")
        .click();
    harness.run();
    assert!(says(
        &harness,
        "A slow Bluetooth transfer is shown as it is. It never switches to the cloud on its own."
    ));

    harness.get_by_label("Use PLAUD account").click();
    harness.run();
    assert!(says(
        &harness,
        "It is PLAUD Cloud, not a direct connection to a device, and it needs an internet connection."
    ));
    harness.get_by_label("Open account settings").click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&DeviceIntent::OpenAccountSettings));

    harness.get_by_label("Import files").click();
    harness.run();
    harness.get_by_label("Choose audio files").click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&DeviceIntent::ImportAudioFiles));
}

#[test]
fn the_usb_route_states_what_this_build_actually_does() {
    let mut app = DevicesApp::new();
    app.state.applied(DeviceOutcome::Scan(fixtures::scan(vec![
        fixtures::candidate("dev-1", DeviceSection::Nearby),
    ])));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "USB (legacy PLAUD Note volume)"));
    assert!(says(
        &harness,
        "This build has no USB device command, so the recordings are brought in with the ordinary file import"
    ));
    harness
        .get_by_label("Choose files from the mounted volume")
        .click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&DeviceIntent::ImportAudioFiles));
}

#[test]
fn every_control_of_the_empty_screen_is_reachable_by_keyboard() {
    let mut harness = harness(DevicesApp::new());
    harness.run();

    let order = a11y::focus_order(&mut harness, 12);
    for control in [
        "Scan for nearby devices",
        "Add PLAUD device",
        "Import audio files instead",
    ] {
        assert!(
            order.iter().any(|label| label == control),
            "{control} is not in the tab order: {order:?}"
        );
    }
}
