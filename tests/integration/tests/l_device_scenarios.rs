//! L1.2/L1.3 — the deterministic device scenario suite (spec §18.3A, §18.6A).
//!
//! Every hostile case in the §18.6A "BLE authentication" and "BLE transfer"
//! clusters is a scenario here, with its required observable outcome asserted.
//! The scripts are versioned wire fixtures under
//! `tests/fixtures/device/<profile>/`, so they are keyed by protocol profile
//! and therefore by device model and firmware.
//!
//! No test in this file touches a radio, a network or a clock, so the suite is
//! identical on macOS, Windows and Linux. None of it is compatibility
//! evidence: spec §18.6A is explicit that a synthetic fixture cannot produce a
//! `Supported` cell, and `l_evidence_rules` asserts that too.

use std::time::Duration;

use plaudon_integration_tests::fakes::ble::{fixture, fixture_dir, listed, listing, sidecar};
use plaudon_plaud_ble::actor::{DeviceActor, DeviceHandle};
use plaudon_plaud_ble::fake::{
    AfterExchange, DeviceScenario, FakeDevice, ScenarioBuilder, TransferFault,
};
use plaudon_plaud_protocol::payload::AuthStatus;
use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;
use plaudon_plaud_protocol::ReadOnlyOpcode;
use plaudon_recording_sources::device::{DeviceError, DeviceSessionState};
use plaudon_source_plaud_device::transfer::StagingAcquisition;
use plaudon_source_plaud_device::TransferVerdict;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Token shaped like the legacy profile's 16-byte credential. Synthetic.
const TOKEN: [u8; 16] = [0xA5; 16];

/// Bytes every transfer scenario carries: a 25-packet synthetic sidecar.
fn media() -> Vec<u8> {
    sidecar(25)
}

/// Spawn an actor over a scenario script.
fn spawn(
    scenario: DeviceScenario,
) -> (
    DeviceHandle,
    mpsc::UnboundedReceiver<plaudon_plaud_ble::DeviceEvent>,
) {
    let device = FakeDevice::new(scenario).expect("a committed scenario is valid");
    let (events, received) = mpsc::unbounded_channel();
    let handle = DeviceActor::new(device, &LEGACY_NOTE_V0095, events)
        .with_notification_timeout(Duration::from_millis(50))
        .spawn();
    (handle, received)
}

/// Spawn an actor whose notification timeout tolerates a deliberately slow
/// script.
fn spawn_slow(
    scenario: DeviceScenario,
) -> (
    DeviceHandle,
    mpsc::UnboundedReceiver<plaudon_plaud_ble::DeviceEvent>,
) {
    let device = FakeDevice::new(scenario).expect("a built scenario is valid");
    let (events, received) = mpsc::unbounded_channel();
    let handle = DeviceActor::new(device, &LEGACY_NOTE_V0095, events)
        .with_notification_timeout(Duration::from_secs(5))
        .spawn();
    (handle, received)
}

/// Spawn an actor over a committed fixture.
fn spawn_fixture(
    name: &str,
) -> (
    DeviceHandle,
    mpsc::UnboundedReceiver<plaudon_plaud_ble::DeviceEvent>,
) {
    let path = fixture(name);
    let document = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("fixture {} is committed", path.display()));
    spawn(DeviceScenario::from_json(&document).expect("a committed fixture parses"))
}

/// Drive probe + authenticate + list, returning the listing.
async fn reach_listing(
    handle: &DeviceHandle,
) -> Result<plaudon_plaud_protocol::payload::DeltaListing, DeviceError> {
    handle.probe().await?;
    handle.authenticate(TOKEN.to_vec()).await?;
    handle.list().await
}

/// Transfer one recording into an in-memory staging buffer.
async fn transfer(
    handle: &DeviceHandle,
    recording: &plaudon_plaud_protocol::payload::ListedRecording,
) -> (
    Result<plaudon_plaud_ble::TransferSummary, DeviceError>,
    Vec<u8>,
) {
    let staging: Vec<u8> = Vec::new();
    let acquisition = StagingAcquisition::begin(&LEGACY_NOTE_V0095, recording, staging)
        .expect("a declared length inside the profile ceiling");
    let shared = std::sync::Arc::new(parking_lot::Mutex::new(Some(acquisition)));
    let sink = SharedSink {
        inner: std::sync::Arc::clone(&shared),
    };
    let result = handle
        .transfer(recording.clone(), Box::new(sink), CancellationToken::new())
        .await;
    let report = shared
        .lock()
        .take()
        .expect("the sink is returned once")
        .finish()
        .expect("finishing a staged acquisition");
    let bytes = report.verification.received_bytes;
    assert_eq!(bytes, report.identity.byte_length);
    (result, Vec::new())
}

/// A `FrameSink` that keeps the acquisition reachable after the transfer.
struct SharedSink {
    inner: std::sync::Arc<parking_lot::Mutex<Option<StagingAcquisition<Vec<u8>>>>>,
}

impl plaudon_plaud_ble::FrameSink for SharedSink {
    fn accept(&mut self, frame: &plaudon_plaud_protocol::BulkFrame) -> Result<(), DeviceError> {
        let mut guard = self.inner.lock();
        let acquisition = guard.as_mut().ok_or(DeviceError::IntegrityFailed)?;
        plaudon_plaud_ble::FrameSink::accept(acquisition, frame)
    }

    fn written(&self) -> u64 {
        self.inner
            .lock()
            .as_ref()
            .map_or(0, plaudon_source_plaud_device::StagingAcquisition::written)
    }
}

// ---------------------------------------------------------------- fixtures

/// Every committed fixture, rebuilt from the builder so the files stay honest.
fn committed_scenarios() -> Vec<(&'static str, DeviceScenario)> {
    let media = media();
    let declared = media.len() as u64;
    let one = listing(vec![listed("REC0001", declared)], false);
    let two = listing(
        vec![listed("REC0001", declared), listed("REC0002", declared)],
        false,
    );

    vec![
        (
            "happy-path",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "happy-path")
                .authenticating(AuthStatus::Accepted)
                .listing(&two)
                .transferring("REC0001", &media)
                .build(),
        ),
        (
            "auth-rejected",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "auth-rejected")
                .authenticating(AuthStatus::Rejected)
                .build(),
        ),
        (
            "encrypted-mode-offer",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "encrypted-mode-offer")
                .authenticating(AuthStatus::EncryptedModeRequired)
                .build(),
        ),
        (
            "empty-delta-listing",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "empty-delta-listing")
                .authenticating(AuthStatus::Accepted)
                .listing(&listing(vec![], false))
                .build(),
        ),
        (
            "missing-final-frame",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "missing-final-frame")
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with("REC0001", &media, TransferFault::DropFinalFrame)
                .build(),
        ),
        (
            "forged-length",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "forged-length")
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with("REC0001", &media, TransferFault::ForgedLength)
                .build(),
        ),
        (
            "duplicate-chunk",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "duplicate-chunk")
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with(
                    "REC0001",
                    &media,
                    TransferFault::DuplicateChunk { index: 2 },
                )
                .build(),
        ),
        (
            "out-of-order-chunks",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "out-of-order-chunks")
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with("REC0001", &media, TransferFault::OutOfOrder { index: 1 })
                .build(),
        ),
        (
            "conflicting-chunk",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "conflicting-chunk")
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with(
                    "REC0001",
                    &media,
                    TransferFault::ConflictingChunk { index: 1 },
                )
                .build(),
        ),
        (
            "disconnect-mid-transfer",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "disconnect-mid-transfer")
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with(
                    "REC0001",
                    &media,
                    TransferFault::DisconnectAfter { frames: 3 },
                )
                .build(),
        ),
        (
            "zero-progress-stall",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "zero-progress-stall")
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with(
                    "REC0001",
                    &media,
                    TransferFault::ZeroProgressStall { repeats: 80 },
                )
                .build(),
        ),
        (
            "low-battery",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "low-battery")
                .with_battery(Some(3))
                .authenticating(AuthStatus::Accepted)
                .listing(&one)
                .transferring_with("REC0001", &media, TransferFault::PowerLow)
                .build(),
        ),
        (
            "disconnect-during-authentication",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "disconnect-during-authentication")
                .authenticating_then(AuthStatus::Accepted, AfterExchange::Disconnect)
                .build(),
        ),
        (
            "unknown-firmware",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "unknown-firmware")
                .reporting_firmware(Some("V0200"))
                .authenticating(AuthStatus::Accepted)
                .build(),
        ),
        (
            "wrong-model-same-name",
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "wrong-model-same-name")
                .reporting_model(Some("Note Pro"))
                .authenticating(AuthStatus::Accepted)
                .build(),
        ),
    ]
}

#[test]
fn the_committed_fixtures_match_the_scripts_they_document() {
    // `UPDATE_DEVICE_FIXTURES=1 cargo test -p plaudon-integration-tests` rewrites
    // them; CI runs without the variable, so drift fails the build.
    let update = std::env::var("UPDATE_DEVICE_FIXTURES").is_ok();
    if update {
        std::fs::create_dir_all(fixture_dir()).expect("fixture directory");
    }

    for (name, scenario) in committed_scenarios() {
        let path = fixture(name);
        let rendered = format!(
            "{}\n",
            serde_json::to_string_pretty(&scenario).expect("a scenario serializes")
        );
        if update {
            std::fs::write(&path, &rendered).expect("writing a fixture");
            continue;
        }
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("fixture {} is committed", path.display()));
        assert_eq!(
            committed, rendered,
            "fixture {name} drifted from the script it documents"
        );
    }
}

#[test]
fn every_fixture_declares_its_profile_firmware_and_synthetic_provenance() {
    let mut seen = 0usize;
    for entry in std::fs::read_dir(fixture_dir())
        .expect("fixture directory")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let document = std::fs::read_to_string(&path).expect("a readable fixture");
        let scenario = DeviceScenario::from_json(&document)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert!(scenario.is_synthetic(), "{}", path.display());
        assert_eq!(scenario.profile, LEGACY_NOTE_V0095.name);
        assert!(
            scenario.firmware.is_some(),
            "{} must name the firmware it documents",
            path.display()
        );
        assert!(
            scenario
                .provenance
                .to_lowercase()
                .contains("no captured credential"),
            "{} must disclaim captured credentials",
            path.display()
        );
        seen += 1;
    }
    assert_eq!(
        seen,
        committed_scenarios().len(),
        "every committed fixture is accounted for"
    );
}

// -------------------------------------------- 18.6A: BLE authentication

#[tokio::test]
async fn the_happy_path_authenticates_lists_and_transfers() {
    let (handle, mut events) = spawn_fixture("happy-path");
    let listing = reach_listing(&handle).await.expect("the happy path lists");
    assert_eq!(listing.entries.len(), 2);

    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    let summary = result.expect("the happy path transfers");
    assert!(summary.terminal_marker_observed);
    assert_eq!(summary.received_bytes, media().len() as u64);
    assert_eq!(handle.state().await.unwrap(), DeviceSessionState::Verifying);

    let mut kinds = Vec::new();
    while let Ok(event) = events.try_recv() {
        kinds.push(event);
    }
    assert!(kinds
        .iter()
        .any(|event| matches!(event, plaudon_plaud_ble::DeviceEvent::Authenticated)));
    assert!(kinds
        .iter()
        .any(|event| matches!(event, plaudon_plaud_ble::DeviceEvent::TransferEnded { .. })));
}

#[tokio::test]
async fn a_rejected_credential_leaves_the_session_closed() {
    let (handle, _events) = spawn_fixture("auth-rejected");
    handle.probe().await.expect("probing is allowed");
    assert_eq!(
        handle.authenticate(TOKEN.to_vec()).await,
        Err(DeviceError::AuthRejected)
    );
    assert_eq!(
        handle.state().await.unwrap(),
        DeviceSessionState::UnsupportedAuth
    );
    assert_eq!(
        handle.list().await,
        Err(DeviceError::AuthBootstrapRequired),
        "a rejected device must not be listable"
    );
}

#[tokio::test]
async fn an_encrypted_mode_offer_is_refused_and_never_downgraded() {
    let (handle, _events) = spawn_fixture("encrypted-mode-offer");
    handle.probe().await.expect("probing is allowed");
    assert_eq!(
        handle.authenticate(TOKEN.to_vec()).await,
        Err(DeviceError::UnsupportedFirmware),
        "an encrypted-mode device is unsupported, not a bad password"
    );
    assert_eq!(
        handle.state().await.unwrap(),
        DeviceSessionState::UnsupportedAuth
    );
    // A second attempt must not retry the legacy handshake.
    assert!(handle.authenticate(TOKEN.to_vec()).await.is_err());
    assert_eq!(
        handle.state().await.unwrap(),
        DeviceSessionState::UnsupportedAuth
    );
}

#[tokio::test]
async fn an_unknown_firmware_fails_closed_before_authentication() {
    let (handle, _events) = spawn_fixture("unknown-firmware");
    assert_eq!(handle.probe().await, Err(DeviceError::UnsupportedFirmware));
    assert_eq!(
        handle.authenticate(TOKEN.to_vec()).await,
        Err(DeviceError::AuthBootstrapRequired),
        "nothing may be presented to a device whose profile did not validate"
    );
}

#[tokio::test]
async fn a_device_reporting_another_model_is_refused_even_under_the_same_name() {
    let (handle, _events) = spawn_fixture("wrong-model-same-name");
    assert_eq!(handle.probe().await, Err(DeviceError::UnsupportedFirmware));
    assert_eq!(
        handle.state().await.unwrap(),
        DeviceSessionState::UnsupportedProfile
    );
}

#[tokio::test]
async fn a_malformed_handshake_is_refused_rather_than_interpreted() {
    // A reply that is not a decodable control frame at all.
    let mut scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "malformed-handshake")
        .authenticating(AuthStatus::Accepted)
        .build();
    scenario.exchanges[0].reply_frames = vec!["01ffff0000".to_owned()];
    let (handle, _events) = spawn(scenario);
    handle.probe().await.expect("probing is allowed");
    assert_eq!(
        handle.authenticate(TOKEN.to_vec()).await,
        Err(DeviceError::AuthRejected)
    );
}

#[tokio::test]
async fn a_disconnect_during_authentication_is_reported_as_a_disconnect() {
    let (handle, _events) = spawn_fixture("disconnect-during-authentication");
    handle.probe().await.expect("probing is allowed");
    handle
        .authenticate(TOKEN.to_vec())
        .await
        .expect("the reply arrives before the drop");
    assert_eq!(handle.list().await, Err(DeviceError::DeviceDisconnected));
}

#[tokio::test]
async fn a_credential_of_the_wrong_shape_never_reaches_the_wire() {
    let (handle, _events) = spawn_fixture("happy-path");
    handle.probe().await.expect("probing is allowed");
    assert_eq!(
        handle.authenticate(vec![0xAB; 8]).await,
        Err(DeviceError::AuthRejected),
        "a token outside the profile's length set is refused locally"
    );
}

// ------------------------------------------------- 18.6A: BLE transfer

#[tokio::test]
async fn a_missing_final_frame_is_never_verified() {
    let (handle, _events) = spawn_fixture("missing-final-frame");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    assert_eq!(
        result,
        Err(DeviceError::TransferIncomplete),
        "a stream that stops is incomplete, not complete"
    );
}

#[tokio::test]
async fn a_forged_frame_length_aborts_the_transfer() {
    let (handle, _events) = spawn_fixture("forged-length");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    assert_eq!(result, Err(DeviceError::TransferIncomplete));
}

#[tokio::test]
async fn a_duplicate_chunk_is_tolerated_and_counted_once() {
    let (handle, _events) = spawn_fixture("duplicate-chunk");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    let summary = result.expect("a retransmission is normal on a radio link");
    assert_eq!(summary.received_bytes, media().len() as u64);
}

#[tokio::test]
async fn out_of_order_chunks_reassemble_in_order() {
    let (handle, _events) = spawn_fixture("out-of-order-chunks");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    let summary = result.expect("reordering is normal on a radio link");
    assert_eq!(summary.received_bytes, media().len() as u64);
}

#[tokio::test]
async fn a_conflicting_duplicate_aborts_the_transfer() {
    let (handle, _events) = spawn_fixture("conflicting-chunk");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    assert_eq!(
        result,
        Err(DeviceError::TransferIncomplete),
        "two different payloads for one offset is corruption, not retransmission"
    );
}

#[tokio::test]
async fn a_disconnect_mid_transfer_leaves_nothing_committed() {
    let (handle, _events) = spawn_fixture("disconnect-mid-transfer");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    assert_eq!(result, Err(DeviceError::DeviceDisconnected));
}

#[tokio::test]
async fn a_zero_progress_stall_terminates() {
    let (handle, _events) = spawn_fixture("zero-progress-stall");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    assert!(
        matches!(
            result,
            Err(DeviceError::TransferIncomplete | DeviceError::IntegrityFailed)
        ),
        "a device that repeats an empty chunk must not loop forever, got {result:?}"
    );
}

#[tokio::test]
async fn a_low_battery_device_reports_power_rather_than_silence() {
    let (handle, _events) = spawn_fixture("low-battery");
    let listing = reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listing.entries[0]).await;
    assert_eq!(result, Err(DeviceError::DevicePowerLow));
}

#[tokio::test]
async fn an_empty_delta_listing_is_not_a_claim_that_the_device_is_empty() {
    let (handle, _events) = spawn_fixture("empty-delta-listing");
    let listing = reach_listing(&handle)
        .await
        .expect("an empty listing is valid");
    assert!(listing.entries.is_empty());
    assert!(
        LEGACY_NOTE_V0095.listing_is_delta_only,
        "the profile records that this listing is a delta of unsynchronized files"
    );
}

#[tokio::test]
async fn a_transfer_of_an_unlisted_recording_is_refused() {
    let (handle, _events) = spawn_fixture("happy-path");
    reach_listing(&handle).await.expect("listing");
    let (result, _) = transfer(&handle, &listed("REC9999", 100)).await;
    assert_eq!(result, Err(DeviceError::NotPresentOnDevice));
}

#[tokio::test]
async fn a_cancelled_transfer_returns_cancelled_and_commits_nothing() {
    let media = media();
    let declared = media.len() as u64;
    let scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "cancelled")
        .authenticating(AuthStatus::Accepted)
        .listing(&listing(vec![listed("REC0001", declared)], false))
        .transferring("REC0001", &media)
        .build();
    let (handle, _events) = spawn(scenario);
    let listing = reach_listing(&handle).await.expect("listing");

    let cancel = CancellationToken::new();
    cancel.cancel();
    let acquisition =
        StagingAcquisition::begin(&LEGACY_NOTE_V0095, &listing.entries[0], Vec::new()).unwrap();
    let shared = std::sync::Arc::new(parking_lot::Mutex::new(Some(acquisition)));
    let sink = SharedSink {
        inner: std::sync::Arc::clone(&shared),
    };
    assert_eq!(
        handle
            .transfer(listing.entries[0].clone(), Box::new(sink), cancel)
            .await,
        Err(DeviceError::Cancelled)
    );
    let report = shared
        .lock()
        .take()
        .unwrap()
        .finish()
        .expect("a cancelled acquisition still reports");
    assert!(
        !report.may_commit(),
        "a cancelled transfer must never be committable"
    );
}

// ------------------------------------------------------- actor ownership

#[tokio::test]
async fn only_one_command_stream_owns_a_device() {
    let media = media();
    let declared = media.len() as u64;
    let scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "busy")
        .with_frame_delay_ms(40)
        .authenticating(AuthStatus::Accepted)
        .listing(&listing(vec![listed("REC0001", declared)], false))
        .transferring("REC0001", &media)
        .build();
    let (handle, _events) = spawn_slow(scenario);
    reach_listing(&handle).await.expect("listing");

    let transfer_handle = handle.clone();
    let recording = listed("REC0001", declared);
    let running = tokio::spawn(async move {
        let acquisition =
            StagingAcquisition::begin(&LEGACY_NOTE_V0095, &recording, Vec::new()).unwrap();
        let shared = std::sync::Arc::new(parking_lot::Mutex::new(Some(acquisition)));
        transfer_handle
            .transfer(
                recording.clone(),
                Box::new(SharedSink { inner: shared }),
                CancellationToken::new(),
            )
            .await
    });

    // Give the actor a moment to pick the transfer up, then try to interleave.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        handle.is_busy(),
        "the actor owns the device during a transfer"
    );
    let concurrent = handle.list().await;
    assert_eq!(
        concurrent,
        Err(DeviceError::DeviceBusy),
        "a parallel GATT command must be refused, never interleaved"
    );
    let _ = running.await;
}

#[tokio::test]
async fn a_disconnect_command_stops_the_actor() {
    let (handle, _events) = spawn_fixture("happy-path");
    handle.probe().await.expect("probing");
    handle.disconnect().await.expect("disconnecting");
    assert_eq!(
        handle.probe().await,
        Err(DeviceError::DeviceDisconnected),
        "the actor is gone once the link is dropped"
    );
}

// ------------------------------------------------- read-only opcode trace

#[tokio::test]
async fn a_full_session_emits_only_allowlisted_opcodes() {
    let media = media();
    let declared = media.len() as u64;
    let scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "trace")
        .authenticating(AuthStatus::Accepted)
        .listing(&listing(vec![listed("REC0001", declared)], false))
        .transferring("REC0001", &media)
        .build();

    // Drive the transport directly so the trace survives the actor.
    let mut device = FakeDevice::new(scenario).unwrap();
    {
        use plaudon_plaud_ble::GattTransport as _;
        use plaudon_plaud_protocol::ControlFrame;

        device.discover().await.unwrap();
        device
            .write_control(
                &ControlFrame::new(ReadOnlyOpcode::Authenticate, TOKEN.to_vec()).unwrap(),
            )
            .await
            .unwrap();
        while device
            .next_notification(Duration::from_millis(1))
            .await
            .is_ok()
        {}
        device
            .write_control(&ControlFrame::new(ReadOnlyOpcode::ListDelta, Vec::new()).unwrap())
            .await
            .unwrap();
        while device
            .next_notification(Duration::from_millis(1))
            .await
            .is_ok()
        {}
    }

    let trace = device.trace();
    assert!(trace.is_read_only());
    assert_eq!(trace.opcode_names(), vec!["authenticate", "listDelta"]);
    for name in trace.opcode_names() {
        for forbidden in [
            "delete", "clear", "record", "setting", "ota", "sync", "reset", "write",
        ] {
            assert!(
                !name.to_lowercase().contains(forbidden),
                "{name} looks like a mutating command"
            );
        }
    }
}

#[tokio::test]
async fn the_transferred_bytes_verify_and_label_themselves_honestly() {
    let (handle, _events) = spawn_fixture("happy-path");
    let listing = reach_listing(&handle).await.expect("listing");
    let recording = listing.entries[0].clone();

    let acquisition =
        StagingAcquisition::begin(&LEGACY_NOTE_V0095, &recording, Vec::new()).unwrap();
    let shared = std::sync::Arc::new(parking_lot::Mutex::new(Some(acquisition)));
    handle
        .transfer(
            recording,
            Box::new(SharedSink {
                inner: std::sync::Arc::clone(&shared),
            }),
            CancellationToken::new(),
        )
        .await
        .expect("the happy path transfers");

    let report = shared.lock().take().unwrap().finish().unwrap();
    assert_eq!(report.verification.verdict, TransferVerdict::Verified);
    assert!(report.may_commit());
    assert_eq!(
        report.quality_class,
        plaudon_domain::media::QualityClass::CompressedDeviceTransfer
    );
    assert!(!report.quality_class.is_original_fidelity());
    assert_eq!(report.source_label(), "Device BLE · compressed ASR");
    assert_eq!(report.verification.measured_duration_ms, Some(500));
}
