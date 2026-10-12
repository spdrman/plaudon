//! End-to-end proof of the direct-device byte path (spec §4.2A, §7.9, §13.2,
//! App. B.4): scan → probe → authorize → list → import, over a scripted
//! `FakeDevice`, a real SQLite ledger, a real media archive and the real
//! `DeviceActor`. No radio, no network, no OS vault.
//!
//! Every assertion here would fail if the byte path were absent: the archive
//! must hold the exact transferred bytes and a separate derivative, the ledger
//! row must say `complete`/`verified`, a transfer that lost its terminal frame
//! must commit nothing, and the host must have emitted read-only opcodes only.
//!
//! It lives in `plaudon-app` because that is where the devices backend, the
//! radio actor and the `DevicesPort` live. It was written in the Tauri shell
//! crate and moved here unchanged (bar the crate name) with EPIC O task O2.5;
//! the device path never had a Tauri dependency.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use plaudon_app::backend::device_radio::{ActorRadio, TransportFactory};
use plaudon_app::backend::devices::{
    DeviceRadio, DevicesBackend, DevicesDeps, HostProfile, RadioAvailability,
};
use plaudon_app::dto::{DeviceSection, EventKind, RadioState};
use plaudon_app::events::{EventPublisher, EventSink, RecordingEventSink};
use plaudon_app::ports::DevicesPort;
use plaudon_app::secrets::SecretValue;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::device::DeviceRepository;
use plaudon_persistence::journal::ImportJournal;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_plaud_ble::discovery::ObservedAdvertisement;
use plaudon_plaud_ble::{FakeDevice, GattTransport, ScenarioBuilder, TransferFault};
use plaudon_plaud_protocol::gatt::SERVICE_VENDOR;
use plaudon_plaud_protocol::payload::{AuthStatus, DeltaListing, DeviceCodec, ListedRecording};
use plaudon_plaud_protocol::profile::{ProtocolProfile, LEGACY_NOTE_V0095};
use plaudon_security::secrets::InMemorySecretStore;
use plaudon_source_plaud_device::{CompatibilityRegister, DeviceImportCommitter};
use tokio::sync::Mutex as AsyncMutex;

const DEVICE: &str = "AABBCCDD-0011-2233-4455-667788990011";
const TOKEN: &str = "00112233445566778899aabbccddeeff";

const SUPPORTED: &str = r#"
schema_version: 1
updated_at: "2026-10-11"
cells:
  - source_type: plaud_device_ble
    vendor: PLAUD
    device_model: Note
    firmware_range: ["V0095"]
    host_os: macos
    os_version_range: ["27.0"]
    auth_mode: legacy_static_token
    provisioning_method: imported_existing_token
    transport: ble
    codec: opus
    quality_class: compressed_device_transfer
    evidence_level: observed_on_identified_hardware
    status: supported
    test_run_id: "run-fixture"
    last_verified_at: "2026-10-11"
    verified_import_recorded: true
    known_limitations: ["fixture register used by a test; no hardware was involved"]
"#;

const NOT_EVALUATED: &str = r#"
schema_version: 1
updated_at: "2026-10-11"
cells:
  - source_type: plaud_device_ble
    vendor: PLAUD
    device_model: Note
    firmware_range: ["V0095"]
    host_os: macos
    os_version_range: ["27.0"]
    auth_mode: legacy_static_token
    provisioning_method: imported_existing_token
    transport: ble
    codec: opus
    quality_class: compressed_device_transfer
    evidence_level: implemented_not_verified
    status: not_evaluated
    known_limitations: ["never exercised against real hardware"]
"#;

/// One 20 ms mono SILK-wideband Opus packet; synthetic payload, real framing.
fn opus_packet(payload_bytes: usize) -> Vec<u8> {
    let mut packet = vec![9 << 3];
    packet.extend(std::iter::repeat_n(0x55u8, payload_bytes));
    packet
}

/// A `.ASR` sidecar of length-prefixed Opus packets.
fn sidecar(packets: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..packets {
        let packet = opus_packet(40);
        out.extend_from_slice(&u16::try_from(packet.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&packet);
    }
    out
}

fn listing_of(id: &str, bytes: &[u8]) -> DeltaListing {
    DeltaListing {
        entries: vec![ListedRecording {
            id: id.to_owned(),
            display_name: format!("{id}.ASR"),
            declared_byte_length: bytes.len() as u64,
            captured_at_unix: 1_790_000_000,
            codec: DeviceCodec::AsrOpus,
        }],
        truncated: false,
    }
}

fn device_for(id: &str, bytes: &[u8], fault: TransferFault) -> FakeDevice {
    ScenarioBuilder::new(&LEGACY_NOTE_V0095, "e2e")
        .reporting_model(Some("Note"))
        .reporting_firmware(Some("V0095"))
        .with_battery(Some(80))
        .authenticating(AuthStatus::Accepted)
        .listing(&listing_of(id, bytes))
        .transferring_with(id, bytes, fault)
        .device()
}

/// A scripted device the test can still inspect after the actor owns it.
#[derive(Clone)]
struct SharedFake(Arc<AsyncMutex<FakeDevice>>);

#[async_trait]
impl GattTransport for SharedFake {
    async fn discover(
        &mut self,
    ) -> Result<plaudon_plaud_ble::ProbeResult, plaudon_recording_sources::device::DeviceError>
    {
        self.0.lock().await.discover().await
    }

    async fn write_control(
        &mut self,
        frame: &plaudon_plaud_protocol::frame::ControlFrame,
    ) -> Result<(), plaudon_recording_sources::device::DeviceError> {
        self.0.lock().await.write_control(frame).await
    }

    async fn next_notification(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<u8>, plaudon_recording_sources::device::DeviceError> {
        self.0.lock().await.next_notification(timeout).await
    }

    async fn disconnect(&mut self) -> Result<(), plaudon_recording_sources::device::DeviceError> {
        self.0.lock().await.disconnect().await
    }
}

/// Hands out the queued scripted devices, one per `connect`.
#[derive(Debug, Default)]
struct ScriptedFactory {
    queue: Mutex<VecDeque<SharedFake>>,
    connects: Mutex<u32>,
}

/// The factory as the radio owns it; the test keeps the shared queue.
#[derive(Debug, Clone)]
struct FactoryHandle(Arc<ScriptedFactory>);

impl std::ops::Deref for FactoryHandle {
    type Target = ScriptedFactory;
    fn deref(&self) -> &ScriptedFactory {
        &self.0
    }
}

impl std::fmt::Debug for SharedFake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedFake")
    }
}

#[async_trait]
impl TransportFactory for FactoryHandle {
    fn availability(&self) -> RadioAvailability {
        RadioAvailability::Present
    }

    async fn scan(&self, _timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError> {
        Ok(vec![ObservedAdvertisement {
            peripheral_id: DEVICE.to_owned(),
            local_name: Some("PLAUD_NOTE".to_owned()),
            service_ids: vec![SERVICE_VENDOR],
            manufacturer_ids: vec![0x0499],
            rssi: Some(-51),
        }])
    }

    async fn connect(
        &self,
        _peripheral_id: &str,
        _profile: &'static ProtocolProfile,
    ) -> Result<Box<dyn GattTransport>, AppError> {
        *self.connects.lock().unwrap() += 1;
        let next =
            self.queue.lock().unwrap().pop_front().ok_or_else(|| {
                AppError::new(AppErrorCode::DeviceDisconnected, "no scripted device")
            })?;
        Ok(Box::new(next))
    }
}

struct Rig {
    backend: DevicesBackend,
    factory: Arc<ScriptedFactory>,
    sink: Arc<RecordingEventSink>,
    db: Database,
    archive: MediaArchive,
    vault: Arc<InMemorySecretStore>,
    dir: tempfile::TempDir,
}

async fn open_db(dir: &std::path::Path) -> (Database, MediaArchive) {
    let db = Database::open(dir.join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    (db, MediaArchive::open(dir).unwrap())
}

fn backend_over(
    register: &str,
    db: &Database,
    archive: &MediaArchive,
    vault: Arc<InMemorySecretStore>,
    factory: &Arc<ScriptedFactory>,
    sink: &Arc<RecordingEventSink>,
) -> DevicesBackend {
    let events = EventPublisher::new(db.pool().clone(), Arc::clone(sink) as Arc<dyn EventSink>);
    let committer = DeviceImportCommitter::new(
        ImportJournal::new(db.clone(), archive.clone()),
        DeviceRepository::new(db.clone()),
    );
    let radio: Arc<dyn DeviceRadio> = Arc::new(ActorRadio::new(
        FactoryHandle(Arc::clone(factory)),
        committer,
    ));
    DevicesBackend::new(DevicesDeps {
        register: CompatibilityRegister::from_yaml(register).unwrap(),
        host: HostProfile {
            host_os: "macos".into(),
            os_version: "27.0".into(),
        },
        radio,
        vault,
        db: db.clone(),
        archive: archive.clone(),
        events,
    })
}

async fn rig(register: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let (db, archive) = open_db(dir.path()).await;
    let vault = Arc::new(InMemorySecretStore::new());
    let factory = Arc::new(ScriptedFactory::default());
    let sink = Arc::new(RecordingEventSink::new());
    let backend = backend_over(register, &db, &archive, Arc::clone(&vault), &factory, &sink);
    Rig {
        backend,
        factory,
        sink,
        db,
        archive,
        vault,
        dir,
    }
}

fn script(rig: &Rig, id: &str, bytes: &[u8], fault: TransferFault) -> SharedFake {
    let fake = SharedFake(Arc::new(AsyncMutex::new(device_for(id, bytes, fault))));
    rig.factory.queue.lock().unwrap().push_back(fake.clone());
    fake
}

async fn authorize_and_list(rig: &Rig) {
    let scan = rig.backend.list_nearby(5_000).await.unwrap();
    assert_eq!(scan.radio.state, RadioState::Scanned);
    assert_eq!(scan.devices[0].section, DeviceSection::Nearby);
    let probe = rig.backend.probe(DEVICE, false).await.unwrap();
    assert_eq!(probe.model.as_deref(), Some("Note"));
    assert_eq!(probe.firmware.as_deref(), Some("V0095"));
    assert_eq!(probe.battery_percent, Some(80));
    rig.backend
        .authorize(DEVICE, SecretValue::new(TOKEN.to_owned()).unwrap())
        .await
        .unwrap();
    rig.backend.list_recordings(DEVICE).await.unwrap();
}

async fn transfer_status(rig: &Rig) -> (String, String, i64) {
    for _ in 0..400 {
        let row: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT transfer_status, verification_status, received_bytes FROM device_transfers",
        )
        .fetch_optional(rig.db.pool())
        .await
        .unwrap();
        if let Some(row) = row {
            if row.0 != "in_flight" {
                return row;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the transfer never left in_flight");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_supported_transfer_lands_the_exact_bytes_and_a_separate_derivative_in_the_archive() {
    let rig = rig(SUPPORTED).await;
    let bytes = sidecar(60);
    let fake = script(&rig, "rec-1", &bytes, TransferFault::None);
    authorize_and_list(&rig).await;

    let ids = rig
        .backend
        .import(DEVICE, vec!["rec-1".into()], true)
        .await
        .unwrap();
    assert_eq!(ids.len(), 1);
    let (status, verification, received) = transfer_status(&rig).await;
    assert_eq!(
        (status.as_str(), verification.as_str()),
        ("complete", "verified")
    );
    assert_eq!(received, i64::try_from(bytes.len()).unwrap());

    // The archive holds the exact bytes under their SHA-256 …
    let digest = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(&bytes))
    };
    let assets: Vec<(String, i64, String, String, String)> = sqlx::query_as(
        "SELECT sha256, byte_length, relative_path, quality_class, verification_state \
         FROM media_assets ORDER BY created_at, id",
    )
    .fetch_all(rig.db.pool())
    .await
    .unwrap();
    assert_eq!(
        assets.len(),
        2,
        "the acquired bytes and one derivative: {assets:?}"
    );
    let acquired = assets
        .iter()
        .find(|a| a.0 == digest)
        .expect("the exact bytes are archived");
    assert_eq!(acquired.1, i64::try_from(bytes.len()).unwrap());
    assert_eq!(acquired.3, "compressed_device_transfer");
    assert!(
        !acquired.3.contains("original"),
        "an .ASR transfer is never recorded as an original"
    );
    assert_eq!(acquired.4, "verified");
    let on_disk = std::fs::read(rig.archive.absolute(&acquired.2).unwrap()).unwrap();
    assert_eq!(
        on_disk, bytes,
        "the archived blob is byte-identical to what the device sent"
    );

    // … and a separate .opus derivative classed as a local derivative.
    let derivative = assets.iter().find(|a| a.0 != digest).unwrap();
    assert_eq!(derivative.3, "local_derivative");
    assert_eq!(
        std::path::Path::new(&derivative.2)
            .extension()
            .and_then(|e| e.to_str()),
        Some("opus")
    );
    assert_ne!(derivative.2, acquired.2);
    let ogg = std::fs::read(rig.archive.absolute(&derivative.2).unwrap()).unwrap();
    assert_eq!(&ogg[..4], b"OggS");

    // The journal rows are committed.
    let states: Vec<String> = sqlx::query_scalar("SELECT state FROM import_journal")
        .fetch_all(rig.db.pool())
        .await
        .unwrap();
    assert_eq!(states.len(), 2);
    assert!(
        states.iter().all(|state| state == "committed"),
        "{states:?}"
    );

    // The device only ever saw read-only opcodes.
    let trace = fake.0.lock().await.trace().clone();
    assert_eq!(
        trace.opcode_names(),
        vec!["authenticate", "listDelta", "requestData"]
    );
    assert!(trace.is_read_only());
    assert_eq!(trace.unscripted_requests, Vec::new());

    // The UI-facing ledger walked to Archived: the last progress event says so.
    let last_state = rig
        .sink
        .events()
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::DeviceTransferProgress)
        .and_then(|event| {
            event
                .payload
                .get("state")
                .and_then(|v| v.as_str().map(str::to_owned))
        });
    assert_eq!(last_state.as_deref(), Some("imported"));
    let kinds = rig.sink.kinds();
    assert!(kinds.contains(&EventKind::DeviceTransferProgress));
    assert!(kinds.contains(&EventKind::DeviceTransferVerified));
    drop(rig.dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transfer_that_loses_its_final_frame_commits_nothing_and_is_interrupted() {
    let rig = rig(SUPPORTED).await;
    let bytes = sidecar(60);
    let fake = script(&rig, "rec-2", &bytes, TransferFault::DropFinalFrame);
    authorize_and_list(&rig).await;

    rig.backend
        .import(DEVICE, vec!["rec-2".into()], true)
        .await
        .unwrap();
    let (status, verification, _) = transfer_status(&rig).await;
    assert_eq!(
        (status.as_str(), verification.as_str()),
        ("interrupted", "unverified")
    );

    let assets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_assets")
        .fetch_one(rig.db.pool())
        .await
        .unwrap();
    assert_eq!(assets, 0, "nothing reached the archive");
    assert_eq!(
        rig.archive.list_blobs().unwrap(),
        Vec::new(),
        "no blob was written"
    );
    let committed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM import_journal WHERE state = 'committed'")
            .fetch_one(rig.db.pool())
            .await
            .unwrap();
    assert_eq!(committed, 0);

    // The envelope code a caller sees for this failure.
    let state = sqlx::query_scalar::<_, String>("SELECT transfer_status FROM device_transfers")
        .fetch_one(rig.db.pool())
        .await
        .unwrap();
    assert_eq!(state, "interrupted");
    let trace = fake.0.lock().await.trace().clone();
    assert_eq!(
        trace.opcode_names(),
        vec!["authenticate", "listDelta", "requestData"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unverified_transfer_surfaces_as_transfer_incomplete() {
    // The radio is the unit that maps a failed commit into the envelope code.
    let dir = tempfile::tempdir().unwrap();
    let (db, archive) = open_db(dir.path()).await;
    let factory = Arc::new(ScriptedFactory::default());
    let committer = DeviceImportCommitter::new(
        ImportJournal::new(db.clone(), archive.clone()),
        DeviceRepository::new(db.clone()),
    );
    let radio = ActorRadio::new(FactoryHandle(Arc::clone(&factory)), committer);
    let bytes = sidecar(60);
    factory
        .queue
        .lock()
        .unwrap()
        .push_back(SharedFake(Arc::new(AsyncMutex::new(device_for(
            "rec-3",
            &bytes,
            TransferFault::DropFinalFrame,
        )))));

    radio.probe(DEVICE).await.unwrap();
    let token = plaudon_security::secrets::SecretString::new(TOKEN);
    let listing = radio.list(DEVICE, &token).await.unwrap();

    struct Sink;
    #[async_trait]
    impl plaudon_app::backend::devices::TransferReporter for Sink {
        async fn report(&self, _update: plaudon_app::backend::devices::TransferUpdate) {}
    }
    // A ledger row must exist for the committer to update.
    let repo = DeviceRepository::new(db.clone());
    let device_id: plaudon_domain::ids::DeviceId =
        uuid::Uuid::from_u128(7).to_string().parse().unwrap();
    repo.upsert_device(
        &plaudon_persistence::device::DeviceRecord {
            id: device_id,
            vendor: "PLAUD".into(),
            model: "Note".into(),
            serial_fingerprint: Some(DEVICE.into()),
            discovered_name: None,
            observed_firmware: Some("V0095".into()),
            trust_state: plaudon_persistence::device::TrustState::Unverified,
        },
        plaudon_domain::time::Timestamp::now(),
    )
    .await
    .unwrap();
    let transfer_id = plaudon_domain::ids::TransferId::new();
    repo.start_transfer(
        &plaudon_persistence::device::TransferRecord {
            id: transfer_id,
            device_id,
            transport: "ble".into(),
            native_format: None,
            expected_bytes: None,
            received_bytes: 0,
            sha256: None,
            status: plaudon_persistence::device::TransferStatus::InFlight,
            verification: plaudon_persistence::device::TransferVerification::Unverified,
        },
        plaudon_domain::time::Timestamp::now(),
    )
    .await
    .unwrap();

    let error = radio
        .transfer(
            plaudon_app::backend::devices::TransferRequest {
                peripheral_id: DEVICE,
                transfer_id,
                recording: &listing.entries[0],
                token: &token,
                local_only: true,
            },
            &Sink,
        )
        .await
        .expect_err("an unverified transfer is an error");
    assert_eq!(error.code(), AppErrorCode::TransferIncomplete);
    assert_eq!(archive.list_blobs().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_profile_that_is_not_supported_is_refused_before_any_frame_is_written() {
    let rig = rig(NOT_EVALUATED).await;
    let bytes = sidecar(60);
    let fake = script(&rig, "rec-1", &bytes, TransferFault::None);

    rig.backend.list_nearby(5_000).await.unwrap();
    // Listing is allowed only after an explicit read-only probe approval.
    rig.backend.probe(DEVICE, true).await.unwrap();
    rig.backend
        .authorize(DEVICE, SecretValue::new(TOKEN.to_owned()).unwrap())
        .await
        .unwrap();
    rig.backend.list_recordings(DEVICE).await.unwrap();
    let before = fake.0.lock().await.trace().emitted_opcodes.len();

    let err = rig
        .backend
        .import(DEVICE, vec!["rec-1".into()], true)
        .await
        .unwrap_err();
    assert_eq!(err.code(), AppErrorCode::CapabilityUnverified);

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_transfers")
        .fetch_one(rig.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "no ledger row was created");
    let trace = fake.0.lock().await.trace().clone();
    assert_eq!(
        trace.emitted_opcodes.len(),
        before,
        "no request frame was written"
    );
    assert_eq!(trace.opcode_names(), vec!["authenticate", "listDelta"]);
    assert_eq!(rig.archive.list_blobs().unwrap(), Vec::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ledger_survives_a_restart_and_never_reports_in_flight() {
    let rig = rig(SUPPORTED).await;
    let bytes = sidecar(60);
    script(&rig, "rec-1", &bytes, TransferFault::None);
    authorize_and_list(&rig).await;

    // A device row, its credential reference and no token anywhere in SQLite.
    let refs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_credential_refs")
        .fetch_one(rig.db.pool())
        .await
        .unwrap();
    assert_eq!(refs, 1);
    let observations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_firmware_observations")
        .fetch_one(rig.db.pool())
        .await
        .unwrap();
    assert_eq!(observations, 1);

    // Simulate a crash mid-transfer: a row left in_flight by a dead process.
    let device_id: String = sqlx::query_scalar("SELECT id FROM devices")
        .fetch_one(rig.db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO device_transfers (id, device_id, transport, received_bytes, \
         transfer_status, verification_status, started_at) \
         VALUES (?1, ?2, 'ble', 10, 'in_flight', 'unverified', 1)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&device_id)
    .execute(rig.db.pool())
    .await
    .unwrap();

    // A fresh backend over the same database, as after a restart.
    let sink = Arc::new(RecordingEventSink::new());
    let factory = Arc::new(ScriptedFactory::default());
    let restarted = backend_over(
        SUPPORTED,
        &rig.db,
        &rig.archive,
        Arc::clone(&rig.vault),
        &factory,
        &sink,
    );
    restarted.recover_interrupted().await.unwrap();
    let in_flight: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_transfers WHERE transfer_status = 'in_flight'",
    )
    .fetch_one(rig.db.pool())
    .await
    .unwrap();
    assert_eq!(in_flight, 0, "a restart never sees an in-flight transfer");
    let interrupted: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_transfers WHERE transfer_status = 'interrupted'",
    )
    .fetch_one(rig.db.pool())
    .await
    .unwrap();
    assert_eq!(interrupted, 1);

    // The linked device is still known, and still unverified until it authenticates.
    let scan = restarted.list_nearby(5_000).await.unwrap();
    assert_eq!(scan.devices.len(), 1);
    assert!(scan.devices[0].authorized);
    assert_eq!(scan.devices[0].identity_label, "Unverified identity");
    assert_eq!(rig.vault.entry_count(), 1);
}
