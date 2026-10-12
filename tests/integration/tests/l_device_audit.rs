//! L1.4/L2.5 — the non-destructive audit and the credential boundary
//! (spec §7.9, §16.2, §16.4, §17.3A, §22(14), §22(16), App. B.6, App. B.7).
//!
//! The audit here is driven by the same deterministic simulator the scenario
//! suite uses, so "reading from this device changes its own file list" is an
//! observed behaviour of a script rather than an assumption.

use std::time::Duration;

use plaudon_integration_tests::fakes::ble::{listed, listing, sidecar};
use plaudon_plaud_ble::actor::{DeviceActor, DeviceHandle};
use plaudon_plaud_ble::fake::{DeviceScenario, FakeDevice, ScenarioBuilder};
use plaudon_plaud_protocol::payload::{AuthStatus, DeltaListing};
use plaudon_plaud_protocol::profile::{ProfileAuthMode, ProtocolProfile, LEGACY_NOTE_V0095};
use plaudon_security::secrets::InMemorySecretStore;
use plaudon_source_plaud_device::audit::{AuditObservation, AuditOutcome, NonDestructiveAudit};
use plaudon_source_plaud_device::credentials::{
    CredentialError, DeviceBinding, DeviceCredentials, IdentityAssurance, ProvisioningRoute,
};
use plaudon_source_plaud_device::evidence::{
    CheckOutcome, CheckResult, EvidenceRecord, EvidenceSource, TransferEvidence,
};
use plaudon_source_plaud_device::{
    CompatibilityRegister, GatePolicy, ProbedProfile, SupportStatus, SyncEligibility,
};
use tokio::sync::mpsc;

/// Synthetic 16-byte credential, shaped like the legacy profile's token.
const TOKEN_HEX: &str = "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";

fn spawn(scenario: DeviceScenario) -> DeviceHandle {
    let device = FakeDevice::new(scenario).expect("a built scenario is valid");
    let (events, _received) = mpsc::unbounded_channel();
    DeviceActor::new(device, &LEGACY_NOTE_V0095, events)
        .with_notification_timeout(Duration::from_millis(50))
        .spawn()
}

/// Build a script whose two listings are `first` then `second`.
fn auditable(first: &DeltaListing, second: &DeltaListing, media: &[u8]) -> DeviceScenario {
    ScenarioBuilder::new(&LEGACY_NOTE_V0095, "audit")
        .authenticating(AuthStatus::Accepted)
        .listing(first)
        .transferring("REC0001", media)
        .listing(second)
        .build()
}

async fn run_audit(scenario: DeviceScenario) -> AuditOutcome {
    let handle = spawn(scenario);
    handle.probe().await.expect("probing");
    handle
        .authenticate(hex_bytes(TOKEN_HEX))
        .await
        .expect("authenticating");

    let mut audit = NonDestructiveAudit::new();
    let before = handle.list().await.expect("the first listing");
    audit.observe(AuditObservation::from_listing(
        &before.entries,
        before.truncated,
        None,
    ));

    // A read is the operation under suspicion: does looking at the device
    // change what it reports?
    let recording = before.entries[0].clone();
    let acquisition = plaudon_source_plaud_device::StagingAcquisition::begin(
        &LEGACY_NOTE_V0095,
        &recording,
        Vec::new(),
    )
    .expect("staging");
    let shared = std::sync::Arc::new(parking_lot::Mutex::new(Some(acquisition)));
    let sink = Sink {
        inner: std::sync::Arc::clone(&shared),
    };
    handle
        .transfer(
            recording,
            Box::new(sink),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("the audit script transfers cleanly");

    let after = handle.list().await.expect("the second listing");
    audit
        .observe(AuditObservation::from_listing(
            &after.entries,
            after.truncated,
            None,
        ))
        .clone()
}

struct Sink {
    inner: std::sync::Arc<
        parking_lot::Mutex<Option<plaudon_source_plaud_device::StagingAcquisition<Vec<u8>>>>,
    >,
}

impl plaudon_plaud_ble::FrameSink for Sink {
    fn accept(
        &mut self,
        frame: &plaudon_plaud_protocol::BulkFrame,
    ) -> Result<(), plaudon_recording_sources::device::DeviceError> {
        let mut guard = self.inner.lock();
        let acquisition = guard
            .as_mut()
            .ok_or(plaudon_recording_sources::device::DeviceError::IntegrityFailed)?;
        plaudon_plaud_ble::FrameSink::accept(acquisition, frame)
    }

    fn written(&self) -> u64 {
        self.inner
            .lock()
            .as_ref()
            .map_or(0, plaudon_source_plaud_device::StagingAcquisition::written)
    }
}

fn hex_bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("ascii hex"), 16)
                .expect("valid hex")
        })
        .collect()
}

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

fn committed_policy() -> GatePolicy {
    let document = std::fs::read_to_string(
        plaudon_integration_tests::fakes::ble::repo_root().join("compatibility.yaml"),
    )
    .expect("the register is committed");
    GatePolicy::new(CompatibilityRegister::from_yaml(&document).expect("a valid register"))
}

// ------------------------------------------------------------- the audit

#[tokio::test]
async fn a_device_whose_listing_is_unchanged_by_a_read_passes_the_audit() {
    let media = sidecar(10);
    let declared = media.len() as u64;
    let stable = listing(
        vec![listed("REC0001", declared), listed("REC0002", declared)],
        false,
    );
    let outcome = run_audit(auditable(&stable, &stable, &media)).await;
    assert_eq!(outcome, AuditOutcome::Unchanged { rounds: 2 });
    assert!(outcome.is_non_destructive());
}

#[tokio::test]
async fn a_device_that_drops_a_read_recording_from_its_listing_fails_the_audit() {
    let media = sidecar(10);
    let declared = media.len() as u64;
    let before = listing(
        vec![listed("REC0001", declared), listed("REC0002", declared)],
        false,
    );
    // The device treats our read as a synchronization and removes the file:
    // exactly the behaviour App. B.6 says we must detect before enabling any
    // automatic sync.
    let after = listing(vec![listed("REC0002", declared)], false);

    let outcome = run_audit(auditable(&before, &after, &media)).await;
    assert_eq!(
        outcome,
        AuditOutcome::ListingChanged {
            disappeared: vec!["REC0001".into()],
            appeared: vec![],
        }
    );
    assert!(!outcome.is_non_destructive());

    let eligibility = SyncEligibility::from_audit(LEGACY_NOTE_V0095.name, outcome, true);
    assert!(
        !eligibility.may_offer_automatic_sync(),
        "a mutating profile must never be offered nearby sync"
    );
}

#[tokio::test]
async fn automatic_sync_stays_disabled_without_both_a_passing_audit_and_a_validated_cell() {
    let policy = committed_policy();
    // The committed register has no supported device cell today, so even a
    // passing audit cannot open the gate.
    assert!(!policy.may_enable_automatic_sync(&probed(), true));
    assert!(!policy.may_enable_automatic_sync(&probed(), false));

    let eligibility = SyncEligibility::from_audit(
        LEGACY_NOTE_V0095.name,
        AuditOutcome::Unchanged { rounds: 3 },
        policy.evaluate(&probed()).permits_transfer(),
    );
    assert!(!eligibility.may_offer_automatic_sync());
    assert!(!eligibility.automatic_sync_default_on());
}

// ------------------------------------------------------- opcode discipline

#[tokio::test]
async fn no_flow_emits_a_mutating_opcode() {
    use plaudon_plaud_ble::GattTransport as _;
    use plaudon_plaud_protocol::{ControlFrame, ReadOnlyOpcode};

    let media = sidecar(10);
    let declared = media.len() as u64;
    let one = listing(vec![listed("REC0001", declared)], false);
    let mut device = FakeDevice::new(auditable(&one, &one, &media)).unwrap();

    device.discover().await.unwrap();
    for frame in [
        ControlFrame::new(ReadOnlyOpcode::Authenticate, hex_bytes(TOKEN_HEX)).unwrap(),
        ControlFrame::new(ReadOnlyOpcode::ListDelta, Vec::new()).unwrap(),
    ] {
        device.write_control(&frame).await.unwrap();
        while device
            .next_notification(Duration::from_millis(1))
            .await
            .is_ok()
        {}
    }

    let trace = device.trace();
    assert!(trace.is_read_only());
    for name in trace.opcode_names() {
        assert!(
            EvidenceRecord::FORBIDDEN_OPCODES
                .iter()
                .all(|forbidden| !forbidden.eq_ignore_ascii_case(&name)),
            "{name} is a forbidden opcode"
        );
    }
    assert!(
        trace.unscripted_requests.is_empty(),
        "the host asked for nothing the script did not expect"
    );
}

// ------------------------------------------------------------ credentials

#[test]
fn a_colliding_device_cannot_reuse_another_devices_credential() {
    let store = InMemorySecretStore::new();
    let credentials = DeviceCredentials::new(&store);

    let genuine = DeviceBinding::new(
        "peripheral-genuine",
        "Note",
        "V0095",
        ProvisioningRoute::ImportedExistingToken,
        IdentityAssurance::Unverified,
    );
    credentials
        .authorize(&genuine, &LEGACY_NOTE_V0095, TOKEN_HEX)
        .expect("authorizing the user's own device");

    // A second device advertising the same name and reporting the same model
    // and firmware, but a different host peripheral identity.
    let clone = DeviceBinding::new(
        "peripheral-clone",
        "Note",
        "V0095",
        ProvisioningRoute::ImportedExistingToken,
        IdentityAssurance::Unverified,
    );
    assert!(!credentials.status(&clone).expect("status").present);
    assert!(matches!(
        credentials.load(&clone, "peripheral-clone", "Note", "V0095"),
        Err(CredentialError::Store(_))
    ));

    // And the genuine binding refuses to answer for the clone's observation.
    assert!(matches!(
        credentials.load(&genuine, "peripheral-clone", "Note", "V0095"),
        Err(CredentialError::IdentityMismatch)
    ));
}

#[test]
fn an_encrypted_mode_profile_is_never_authorized_or_downgraded() {
    let store = InMemorySecretStore::new();
    let credentials = DeviceCredentials::new(&store);
    let encrypted = ProtocolProfile {
        auth_mode: ProfileAuthMode::EncryptedHandshake,
        ..LEGACY_NOTE_V0095
    };
    let binding = DeviceBinding::new(
        "peripheral-encrypted",
        "Note",
        "V0200",
        ProvisioningRoute::ImportedExistingToken,
        IdentityAssurance::Unverified,
    );
    assert_eq!(
        credentials.authorize(&binding, &encrypted, TOKEN_HEX),
        Err(CredentialError::AuthModeUnsupported)
    );
    assert!(!credentials.status(&binding).expect("status").present);
    assert!(
        !encrypted.auth_mode_is_implemented(),
        "the legacy handshake is never offered as a fallback"
    );
}

#[test]
fn revocation_returns_a_device_to_needing_authorization() {
    let store = InMemorySecretStore::new();
    let credentials = DeviceCredentials::new(&store);
    let binding = DeviceBinding::new(
        "peripheral-a",
        "Note",
        "V0095",
        ProvisioningRoute::ImportedExistingToken,
        IdentityAssurance::Unverified,
    );
    credentials
        .authorize(&binding, &LEGACY_NOTE_V0095, TOKEN_HEX)
        .expect("authorizing");
    credentials.revoke(&binding).expect("revoking");
    assert!(!credentials.status(&binding).expect("status").present);
}

#[test]
fn nothing_persistable_carries_a_device_credential() {
    // What a backup, the database and an evidence record may contain is a
    // binding, a vault reference and a digest. None of them may contain the
    // credential (spec §22(9), App. B.7).
    let store = InMemorySecretStore::new();
    let credentials = DeviceCredentials::new(&store);
    let binding = DeviceBinding::new(
        "peripheral-a",
        "Note",
        "V0095",
        ProvisioningRoute::ImportedExistingToken,
        IdentityAssurance::ProfileAuthenticated,
    );
    let reference = credentials
        .authorize(&binding, &LEGACY_NOTE_V0095, TOKEN_HEX)
        .expect("authorizing");

    let record = EvidenceRecord {
        schema_version: EvidenceRecord::SCHEMA_VERSION,
        test_run_id: "run-simulated-001".into(),
        recorded_at: "2026-10-11".into(),
        source: EvidenceSource::Simulator,
        source_type: "plaud_device_ble".into(),
        vendor: "PLAUD".into(),
        device_model: "Note".into(),
        observed_firmware: "V0095".into(),
        host_os: "macos".into(),
        host_os_build: "27.0".into(),
        transport: "ble".into(),
        auth_mode: "legacy_static_token".into(),
        provisioning_route: "imported_existing_token".into(),
        transfer: Some(TransferEvidence {
            device_recording_id: "REC0001".into(),
            acquired_mime: "audio/opus".into(),
            quality_class: "compressed_device_transfer".into(),
            received_bytes: 1_025,
            device_declared_byte_length: Some(1_025),
            terminal_marker_observed: true,
            sha256_hex: "b".repeat(64),
            measured_duration_ms: Some(500),
        }),
        non_destructive_scan_observed: true,
        emitted_opcodes: vec!["authenticate".into(), "listDelta".into()],
        network_denied_run: true,
        checks: vec![CheckResult::new(
            "discover_only",
            CheckOutcome::Passed,
            "one candidate",
        )],
    };

    let persisted = [
        serde_json::to_string(&binding).expect("a binding serializes"),
        serde_json::to_string(&reference).expect("a reference serializes"),
        serde_json::to_string(&record).expect("a record serializes"),
    ]
    .join("\n");
    assert!(
        !persisted.contains(TOKEN_HEX),
        "a persisted document must never contain the credential"
    );
    for fragment in [&TOKEN_HEX[..16], &TOKEN_HEX[8..24]] {
        assert!(!persisted.contains(fragment));
    }

    // And the simulator's own evidence can never promote a cell.
    let register = committed_policy();
    let cell = register
        .register()
        .cells
        .iter()
        .find(|cell| cell.source_type == "plaud_device_ble" && cell.host_os == "macos")
        .expect("the register describes the legacy BLE cell");
    assert_eq!(cell.status, SupportStatus::NotEvaluated);
    assert!(record.promotes(cell).is_err());
}

#[test]
fn no_committed_device_fixture_contains_a_credential_shaped_value() {
    // Spec §18.2A: raw token and audio captures are prohibited in test assets.
    let directory = plaudon_integration_tests::fakes::ble::fixture_dir();
    for entry in std::fs::read_dir(&directory)
        .expect("the fixture directory exists")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let document = std::fs::read_to_string(&path).expect("a readable fixture");
        assert!(
            !document.contains(TOKEN_HEX),
            "{} contains a credential-shaped value",
            path.display()
        );
        let scenario = DeviceScenario::from_json(&document).expect("a valid fixture");
        // Only device replies are scripted; a fixture never carries a request
        // payload, so it cannot carry a credential the host would send.
        for exchange in &scenario.exchanges {
            for frame in &exchange.reply_frames {
                assert!(
                    !frame.contains(TOKEN_HEX),
                    "{} replays a credential",
                    path.display()
                );
            }
        }
    }
}
