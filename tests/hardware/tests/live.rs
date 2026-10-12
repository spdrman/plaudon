//! The live driver against the scripted simulator (spec §18.3A, App. B.9).
//!
//! These runs exercise the same driver the `radio` feature points at a real
//! device. They are **simulator** runs: the records they assemble are tagged
//! `simulator` and can never promote a cell.

mod support;

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use plaudon_hardware_tests::gate::MapEnv;
use plaudon_hardware_tests::live::{
    self, DeviceBackend, LiveConfig, LiveSettings, PhoneSyncWait, ScanCandidate, ScanSummary,
};
use plaudon_hardware_tests::observations::Observations;
use plaudon_hardware_tests::suite::{self, Assembly};
use plaudon_plaud_ble::fake::{FakeDevice, ScenarioBuilder, TransferFault};
use plaudon_plaud_ble::GattTransport;
use plaudon_plaud_protocol::payload::{AuthStatus, DeltaListing, DeviceCodec, ListedRecording};
use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;
use plaudon_recording_sources::device::DeviceError;
use plaudon_security::secrets::InMemorySecretStore;
use plaudon_source_plaud_device::credentials::{
    DeviceBinding, DeviceCredentials, IdentityAssurance, ProvisioningRoute,
};
use plaudon_source_plaud_device::evidence::{CheckOutcome, EvidenceSource};
use support::{mac_ble_profile, mac_host};

const PERIPHERAL: &str = "peripheral-1";
const TOKEN_HEX: &str = "00112233445566778899aabbccddeeff";

struct FakeBackend {
    scan: Result<ScanSummary, DeviceError>,
    devices: VecDeque<FakeDevice>,
    connects: usize,
}

#[async_trait]
impl DeviceBackend for FakeBackend {
    async fn scan(&mut self) -> Result<ScanSummary, DeviceError> {
        self.scan.clone()
    }

    async fn connect(&mut self, _id: &str) -> Result<Box<dyn GattTransport>, DeviceError> {
        self.connects += 1;
        self.devices
            .pop_front()
            .map(|device| Box::new(device) as Box<dyn GattTransport>)
            .ok_or(DeviceError::DeviceDisconnected)
    }
}

fn one_candidate() -> ScanSummary {
    ScanSummary {
        adapter: "Simulated adapter".into(),
        advertisements: 5,
        candidates: vec![ScanCandidate {
            id: PERIPHERAL.into(),
            name: Some("PLAUD_NOTE".into()),
        }],
    }
}

/// 300 twenty-millisecond packets: a 6 s sidecar.
fn sidecar() -> Vec<u8> {
    let mut packet = vec![9u8 << 3];
    packet.extend(std::iter::repeat_n(0x55u8, 40));
    let mut out = Vec::new();
    for _ in 0..300 {
        out.extend_from_slice(&u16::try_from(packet.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&packet);
    }
    out
}

fn listing(ids: &[&str], bytes: usize) -> DeltaListing {
    DeltaListing {
        entries: ids
            .iter()
            .map(|id| ListedRecording {
                id: (*id).into(),
                display_name: format!("{id}.ASR"),
                declared_byte_length: bytes as u64,
                captured_at_unix: 1_760_000_000,
                codec: DeviceCodec::AsrOpus,
            })
            .collect(),
        truncated: false,
    }
}

fn builder(name: &str) -> ScenarioBuilder {
    ScenarioBuilder::new(&LEGACY_NOTE_V0095, name)
        .reporting_model(Some("Note"))
        .reporting_firmware(Some("V0095"))
}

fn rejecting() -> FakeDevice {
    builder("wrong-credential")
        .authenticating(AuthStatus::Rejected)
        .device()
}

fn accepting(fault: TransferFault) -> FakeDevice {
    let bytes = sidecar();
    let listed = listing(&["REC0001"], bytes.len());
    builder("happy")
        .authenticating(AuthStatus::Accepted)
        .listing(&listed)
        .listing(&listed)
        .transferring_with("REC0001", &bytes, fault)
        .device()
}

fn store_with_token() -> InMemorySecretStore {
    let store = InMemorySecretStore::new();
    let binding = DeviceBinding::new(
        PERIPHERAL,
        "Note",
        "V0095",
        ProvisioningRoute::ImportedExistingToken,
        IdentityAssurance::Unverified,
    );
    DeviceCredentials::new(&store)
        .authorize(&binding, &LEGACY_NOTE_V0095, TOKEN_HEX)
        .expect("store the test credential");
    store
}

fn config(store: &InMemorySecretStore) -> LiveConfig<'_> {
    LiveConfig {
        profile: &LEGACY_NOTE_V0095,
        route: ProvisioningRoute::ImportedExistingToken,
        store,
        selection: None,
        max_pulls: 4,
        max_pull_bytes: 1024 * 1024,
        phone_sync: None,
    }
}

async fn run(backend: &mut FakeBackend, config: &LiveConfig<'_>) -> Observations {
    live::drive(backend, config).await
}

fn outcome(observations: &Observations, id: &str) -> CheckOutcome {
    observations
        .checks
        .get(id)
        .unwrap_or_else(|| panic!("{id} was not recorded"))
        .outcome
}

#[tokio::test]
async fn a_full_simulated_session_derives_every_live_check() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([rejecting(), accepting(TransferFault::None)]),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;

    for id in [
        "discover_only",
        "authorization_failure",
        "missing_secret",
        "authorization_success",
        "transfer_pull_15s",
        "imported_bytes_verified",
        "opus_decode_measurement",
    ] {
        assert_eq!(outcome(&observations, id), CheckOutcome::Passed, "{id}");
    }
    assert_eq!(
        outcome(&observations, "encrypted_handshake_detected"),
        CheckOutcome::Skipped
    );
    assert_eq!(
        outcome(&observations, "device_name_collision"),
        CheckOutcome::Skipped
    );
    assert_eq!(
        outcome(&observations, "two_devices_in_range"),
        CheckOutcome::Skipped
    );
    assert_eq!(
        outcome(&observations, "listing_delta_before_after_phone_sync"),
        CheckOutcome::Skipped,
        "no phone synchronization was performed"
    );
    assert_eq!(
        outcome(&observations, "transfer_pull_5min"),
        CheckOutcome::Skipped
    );
    assert_eq!(observations.observed_firmware, "V0095");
    assert!(observations.non_destructive_scan_observed);
    assert_eq!(
        observations.emitted_opcodes,
        [
            "authenticate",
            "authenticate",
            "listDelta",
            "listDelta",
            "requestData"
        ]
    );
    let transfer = observations.transfer.as_ref().expect("a transfer");
    assert!(transfer.is_verified());
    assert_eq!(transfer.received_bytes, sidecar().len() as u64);
    assert_eq!(transfer.measured_duration_ms, Some(6_000));
    assert_eq!(transfer.quality_class, "compressed_device_transfer");
}

#[tokio::test]
async fn a_simulated_run_never_promotes_and_leaks_no_credential() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([rejecting(), accepting(TransferFault::None)]),
        connects: 0,
    };
    let live = run(&mut backend, &config(&store)).await;
    let operator = support::complete_observations();
    let merged = live::merge_operator(live, &operator);
    let record = suite::assemble(&Assembly {
        test_run_id: "sim-1".into(),
        recorded_at: "2026-10-11".into(),
        source: EvidenceSource::Simulator,
        profile: &mac_ble_profile(),
        host: &mac_host(),
        observations: &merged,
    });
    assert!(record.promotes(&mac_ble_profile().cell).is_err());
    let json = plaudon_hardware_tests::emitter::to_json(&record).unwrap();
    assert!(
        !json.contains(TOKEN_HEX),
        "the credential must never be recorded"
    );
    assert!(!json.contains(&plaudon_plaud_ble::fake::encode_hex(&sidecar()[..8])));
}

#[tokio::test]
async fn a_device_that_accepts_a_random_credential_fails_the_check() {
    let store = store_with_token();
    let accepts_anything = builder("accepts-anything")
        .authenticating(AuthStatus::Accepted)
        .device();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([accepts_anything]),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert_eq!(
        outcome(&observations, "authorization_failure"),
        CheckOutcome::Failed
    );
    assert_eq!(
        backend.connects, 1,
        "the run stops rather than using a real credential"
    );
}

#[tokio::test]
async fn an_encrypted_mode_device_is_detected_and_never_retried() {
    let store = store_with_token();
    let encrypted = builder("encrypted")
        .authenticating(AuthStatus::EncryptedModeRequired)
        .device();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([encrypted]),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert_eq!(
        outcome(&observations, "encrypted_handshake_detected"),
        CheckOutcome::Passed
    );
    assert_eq!(
        outcome(&observations, "authorization_failure"),
        CheckOutcome::Skipped
    );
    assert_eq!(
        outcome(&observations, "authorization_success"),
        CheckOutcome::Skipped
    );
    assert_eq!(
        observations.emitted_opcodes,
        ["authenticate"],
        "no downgrade retry"
    );
}

#[tokio::test]
async fn an_unprovisioned_device_skips_authorization_success_and_sends_no_real_credential() {
    let store = InMemorySecretStore::new();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([rejecting()]),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert_eq!(
        outcome(&observations, "missing_secret"),
        CheckOutcome::Passed
    );
    assert_eq!(
        outcome(&observations, "authorization_success"),
        CheckOutcome::Skipped
    );
    assert_eq!(observations.emitted_opcodes, ["authenticate"]);
    assert_eq!(backend.connects, 1);
}

#[tokio::test]
async fn a_denied_radio_fails_permission_and_discovery_without_connecting() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Err(DeviceError::RadioPermissionDenied),
        devices: VecDeque::new(),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert_eq!(
        outcome(&observations, "bluetooth_permission"),
        CheckOutcome::Failed
    );
    assert_eq!(
        outcome(&observations, "discover_only"),
        CheckOutcome::Failed
    );
    assert_eq!(backend.connects, 0);
    assert_eq!(observations.emitted_opcodes, Vec::<String>::new());
}

#[tokio::test]
async fn no_candidate_fails_discovery() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Ok(ScanSummary {
            adapter: "a".into(),
            advertisements: 9,
            candidates: vec![],
        }),
        devices: VecDeque::new(),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert_eq!(
        outcome(&observations, "discover_only"),
        CheckOutcome::Failed
    );
    assert_eq!(backend.connects, 0);
}

fn two_devices_same_name() -> ScanSummary {
    ScanSummary {
        adapter: "Simulated adapter".into(),
        advertisements: 7,
        candidates: vec![
            ScanCandidate {
                id: "peripheral-1".into(),
                name: Some("PLAUD_NOTE".into()),
            },
            ScanCandidate {
                id: "peripheral-2".into(),
                name: Some("PLAUD_NOTE".into()),
            },
        ],
    }
}

#[tokio::test]
async fn two_devices_in_range_are_never_auto_selected() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Ok(two_devices_same_name()),
        devices: VecDeque::from([rejecting()]),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert_eq!(
        outcome(&observations, "two_devices_in_range"),
        CheckOutcome::Passed
    );
    assert_eq!(
        outcome(&observations, "device_name_collision"),
        CheckOutcome::Passed
    );
    assert_eq!(backend.connects, 0, "no selection, so nothing is connected");
}

#[tokio::test]
async fn an_explicit_selection_among_two_devices_is_used() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Ok(two_devices_same_name()),
        devices: VecDeque::from([rejecting(), accepting(TransferFault::None)]),
        connects: 0,
    };
    let mut selected = config(&store);
    selected.selection = Some(PERIPHERAL.into());
    let observations = run(&mut backend, &selected).await;
    assert_eq!(
        outcome(&observations, "two_devices_in_range"),
        CheckOutcome::Passed
    );
    assert_eq!(
        outcome(&observations, "authorization_success"),
        CheckOutcome::Passed
    );

    let mut stranger = config(&store);
    stranger.selection = Some("not-in-range".into());
    let mut backend = FakeBackend {
        scan: Ok(two_devices_same_name()),
        devices: VecDeque::new(),
        connects: 0,
    };
    let observations = run(&mut backend, &stranger).await;
    assert_eq!(
        outcome(&observations, "discover_only"),
        CheckOutcome::Failed
    );
    assert_eq!(backend.connects, 0);
}

#[tokio::test]
async fn a_link_dropped_mid_transfer_is_never_verified() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([
            rejecting(),
            accepting(TransferFault::DisconnectAfter { frames: 3 }),
        ]),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert_eq!(
        outcome(&observations, "imported_bytes_verified"),
        CheckOutcome::Skipped
    );
    assert_eq!(
        outcome(&observations, "transfer_pull_15s"),
        CheckOutcome::Failed
    );
    assert!(
        observations.transfer.is_none(),
        "a failed pull yields no evidence to claim"
    );
}

#[tokio::test]
async fn a_missing_terminal_marker_leaves_the_transfer_unverified() {
    let store = store_with_token();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([rejecting(), accepting(TransferFault::DropFinalFrame)]),
        connects: 0,
    };
    let observations = run(&mut backend, &config(&store)).await;
    assert!(observations
        .transfer
        .as_ref()
        .is_none_or(|t| !t.is_verified()));
    assert_ne!(
        outcome(&observations, "transfer_pull_15s"),
        CheckOutcome::Passed
    );
}

#[tokio::test]
async fn the_phone_sync_handoff_compares_the_listing_before_and_after() {
    let store = store_with_token();
    let bytes = sidecar();
    let before = listing(&["REC0001", "REC0002"], bytes.len());
    let after = listing(&["REC0002", "REC0003"], bytes.len());
    let device = builder("phone-sync")
        .authenticating(AuthStatus::Accepted)
        .listing(&before)
        .listing(&before)
        .listing(&after)
        .transferring("REC0002", &bytes)
        .device();
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("synced");
    std::fs::write(&flag, b"").unwrap();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([rejecting(), device]),
        connects: 0,
    };
    let mut synced = config(&store);
    synced.phone_sync = Some(PhoneSyncWait {
        flag,
        timeout: Duration::from_secs(5),
    });
    synced.max_pulls = 1;
    let observations = run(&mut backend, &synced).await;
    let check = &observations.checks["listing_delta_before_after_phone_sync"];
    assert_eq!(check.outcome, CheckOutcome::Passed);
    assert!(
        check.detail.contains("listed 2 before and 2 after"),
        "{}",
        check.detail
    );
    assert!(
        check.detail.contains("1 no longer listed, 1 newly listed"),
        "{}",
        check.detail
    );
}

#[tokio::test]
async fn an_unsignalled_phone_sync_is_skipped_not_passed() {
    let store = store_with_token();
    let bytes = sidecar();
    let listed = listing(&["REC0001"], bytes.len());
    let device = builder("no-sync")
        .authenticating(AuthStatus::Accepted)
        .listing(&listed)
        .listing(&listed)
        .device();
    let dir = tempfile::tempdir().unwrap();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([rejecting(), device]),
        connects: 0,
    };
    let mut waiting = config(&store);
    waiting.phone_sync = Some(PhoneSyncWait {
        flag: dir.path().join("never"),
        timeout: Duration::from_millis(300),
    });
    waiting.max_pulls = 0;
    let observations = run(&mut backend, &waiting).await;
    assert_eq!(
        outcome(&observations, "listing_delta_before_after_phone_sync"),
        CheckOutcome::Skipped
    );
}

#[tokio::test]
async fn a_listing_that_changes_between_read_only_passes_is_not_non_destructive() {
    let store = store_with_token();
    let bytes = sidecar();
    let device = builder("mutating")
        .authenticating(AuthStatus::Accepted)
        .listing(&listing(&["REC0001", "REC0002"], bytes.len()))
        .listing(&listing(&["REC0001"], bytes.len()))
        .device();
    let mut backend = FakeBackend {
        scan: Ok(one_candidate()),
        devices: VecDeque::from([rejecting(), device]),
        connects: 0,
    };
    let mut cfg = config(&store);
    cfg.max_pulls = 0;
    let observations = run(&mut backend, &cfg).await;
    assert!(!observations.non_destructive_scan_observed);
}

#[test]
fn the_operator_cannot_speak_for_a_check_the_run_owns() {
    let operator = support::complete_observations();
    let mut live = Observations::default();
    live.checks.insert(
        "discover_only".into(),
        plaudon_hardware_tests::observations::ObservedCheck {
            outcome: CheckOutcome::Failed,
            detail: "no candidate".into(),
        },
    );
    let merged = live::merge_operator(live, &operator);
    assert_eq!(outcome(&merged, "discover_only"), CheckOutcome::Failed);
    assert_eq!(
        outcome(&merged, "authorization_success"),
        CheckOutcome::Skipped,
        "unreached live checks are skipped even when the operator says passed"
    );
    assert_eq!(
        outcome(&merged, "sleep_wake"),
        CheckOutcome::Passed,
        "operator-owned"
    );
    assert!(merged.network_denied_run);
}

#[test]
fn live_settings_default_conservatively_and_ignore_garbage() {
    let defaults = LiveSettings::from_env(&MapEnv::new([]));
    assert_eq!(defaults.max_pulls, 4);
    assert_eq!(defaults.max_pull_bytes, 512 * 1024 * 1024);
    assert!(defaults.phone_sync.is_none());
    assert!(defaults.selection.is_none());

    let set = LiveSettings::from_env(&MapEnv::new([
        (live::MAX_PULLS_ENV, "2"),
        (live::MAX_PULL_BYTES_ENV, "lots"),
        (live::PHONE_SYNC_FLAG_ENV, "/tmp/synced"),
        (live::PERIPHERAL_ENV, " abc "),
    ]));
    assert_eq!(set.max_pulls, 2);
    assert_eq!(
        set.max_pull_bytes,
        512 * 1024 * 1024,
        "garbage falls back to the default"
    );
    assert_eq!(set.selection.as_deref(), Some("abc"));
    assert_eq!(set.phone_sync.unwrap().timeout, Duration::from_secs(600));
}

#[test]
fn the_committed_ble_cells_resolve_to_a_protocol_profile_and_route() {
    let cell = mac_ble_profile().cell;
    assert_eq!(
        live::protocol_profile_for(&cell).unwrap().name,
        LEGACY_NOTE_V0095.name
    );
    assert_eq!(
        live::route_for(&cell).unwrap(),
        ProvisioningRoute::ImportedExistingToken
    );
}
