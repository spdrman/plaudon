//! The live driver: everything a run can determine by itself, determined by
//! running the real actor over a transport (spec §18.3A, App. B.9).
//!
//! The driver is generic over a [`DeviceBackend`], which supplies a scan and a
//! connected [`GattTransport`]. On a real machine the backend is the
//! `radio` feature's `RadioBackend` (btleplug over CoreBluetooth or WinRT); in
//! hermetic tests it is a scripted `FakeDevice`. The checks are identical, but
//! a fake-backed run is a **simulator** run: nothing here upgrades it to
//! hardware evidence, and `EvidenceSource::Simulator` is what the hermetic
//! tests record.
//!
//! What is derived here, and how:
//!
//! | check | derivation |
//! | --- | --- |
//! | `bluetooth_permission` | `failed` only when the OS denies the scan; a pass needs the packaged-app attestation from the operator |
//! | `discover_only` | a scan found candidates while no frame had been written |
//! | `device_name_collision`, `two_devices_in_range` | from the candidates the scan saw; selection is by identifier, never by name |
//! | `authorization_failure` | a random, well-formed credential is authenticated and must be refused |
//! | `encrypted_handshake_detected` | authentication after a good probe answered "encrypted mode required" |
//! | `missing_secret` | a vault lookup for an unbound identity fails closed and writes no frame |
//! | `authorization_success` | the vault credential for this device authenticates |
//! | `listing_delta_before_after_phone_sync`, audit | `NonDestructiveAudit` over repeated read-only listings; the phone sync itself is signalled by the operator |
//! | `transfer_pull_*`, `imported_bytes_verified`, `opus_decode_measurement` | real transfers through `StagingAcquisition`, re-hashed from staging |
//! | opcode trace | every frame the actor wrote, captured by a recording transport |
//!
//! Everything a program cannot know (sleep/wake performed, battery state,
//! USB, process kill, firewall) stays in the operator's observations.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use plaudon_plaud_ble::{DeviceActor, DeviceHandle, FrameSink, GattTransport, ProbeResult};
use plaudon_plaud_protocol::bulk::BulkFrame;
use plaudon_plaud_protocol::frame::ControlFrame;
use plaudon_plaud_protocol::payload::ListedRecording;
use plaudon_plaud_protocol::profile::ProtocolProfile;
use plaudon_plaud_protocol::ReadOnlyOpcode;
use plaudon_recording_sources::device::DeviceError;
use plaudon_security::secrets::{SecretStore, SecretStoreError, SecretString};
use plaudon_source_plaud_device::audit::{AuditObservation, NonDestructiveAudit};
use plaudon_source_plaud_device::credentials::{
    CredentialError, DeviceBinding, DeviceCredentials, IdentityAssurance, ProvisioningRoute,
};
use plaudon_source_plaud_device::evidence::{CheckOutcome, TransferEvidence};
use plaudon_source_plaud_device::transfer::{
    derive_playable, AcquisitionReport, DerivativeOutcome, StagingAcquisition,
};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::observations::{Observations, ObservedCheck};

/// One advertisement that passed the discovery policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCandidate {
    /// Host-assigned stable peripheral identifier.
    pub id: String,
    /// Advertised name. Advisory only; never used to select.
    pub name: Option<String>,
}

/// What a scan saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    /// Host adapter, as the platform names it.
    pub adapter: String,
    /// Advertisements observed before policy.
    pub advertisements: usize,
    /// Advertisements that passed the discovery policy.
    pub candidates: Vec<ScanCandidate>,
}

/// A source of scans and connected transports.
#[async_trait]
pub trait DeviceBackend: Send {
    /// Perform one read-only scan.
    async fn scan(&mut self) -> Result<ScanSummary, DeviceError>;

    /// Open a transport to the peripheral the user selected. Never called
    /// before the driver has selected one by identifier.
    async fn connect(&mut self, peripheral_id: &str)
        -> Result<Box<dyn GattTransport>, DeviceError>;
}

/// Wait for the operator to signal that a phone synchronization happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneSyncWait {
    /// File the operator creates once the phone has synchronized.
    pub flag: PathBuf,
    /// How long to wait for it.
    pub timeout: Duration,
}

/// Parameters of one live run.
#[derive(Debug)]
pub struct LiveConfig<'a> {
    /// Protocol profile selected by the compatibility register.
    pub profile: &'static ProtocolProfile,
    /// Provisioning route the cell under test uses.
    pub route: ProvisioningRoute,
    /// The credential vault. The suite only ever reads from it.
    pub store: &'a dyn SecretStore,
    /// Peripheral identifier chosen by the operator; required when more than
    /// one candidate is in range.
    pub selection: Option<String>,
    /// Most recordings to pull.
    pub max_pulls: usize,
    /// Largest declared length to pull, in bytes.
    pub max_pull_bytes: u64,
    /// Phone synchronization hand-off, when the operator will perform one.
    pub phone_sync: Option<PhoneSyncWait>,
}

/// `(check id, lower bound ms, upper bound ms)` of the pull-length buckets.
pub const PULL_BUCKETS: &[(&str, u64, u64)] = &[
    ("transfer_pull_15s", 5_000, 60_000),
    ("transfer_pull_5min", 180_000, 600_000),
    ("transfer_pull_30min", 1_200_000, 3_600_000),
    ("transfer_pull_multi_hour", 7_200_000, u64::MAX),
];

type Trace = Arc<Mutex<Vec<ReadOnlyOpcode>>>;

/// Records every frame written, then forwards it.
struct RecordingTransport {
    inner: Box<dyn GattTransport>,
    trace: Trace,
}

impl std::fmt::Debug for RecordingTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecordingTransport")
    }
}

#[async_trait]
impl GattTransport for RecordingTransport {
    async fn discover(&mut self) -> Result<ProbeResult, DeviceError> {
        self.inner.discover().await
    }

    async fn write_control(&mut self, frame: &ControlFrame) -> Result<(), DeviceError> {
        self.trace.lock().expect("trace lock").push(frame.opcode);
        self.inner.write_control(frame).await
    }

    async fn next_notification(&mut self, timeout: Duration) -> Result<Vec<u8>, DeviceError> {
        self.inner.next_notification(timeout).await
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        self.inner.disconnect().await
    }
}

/// A staging acquisition the driver can read back after the actor owns it.
#[derive(Clone)]
struct SharedSink(Arc<Mutex<Option<StagingAcquisition<File>>>>);

impl FrameSink for SharedSink {
    fn accept(&mut self, frame: &BulkFrame) -> Result<(), DeviceError> {
        match self.0.lock().expect("sink lock").as_mut() {
            Some(acquisition) => acquisition
                .accept(frame)
                .map(|_| ())
                .map_err(DeviceError::from),
            None => Err(DeviceError::IntegrityFailed),
        }
    }

    fn written(&self) -> u64 {
        self.0
            .lock()
            .expect("sink lock")
            .as_ref()
            .map_or(0, StagingAcquisition::written)
    }
}

fn opcode_name(opcode: ReadOnlyOpcode) -> &'static str {
    match opcode {
        ReadOnlyOpcode::Authenticate => "authenticate",
        ReadOnlyOpcode::ListDelta => "listDelta",
        ReadOnlyOpcode::RequestData => "requestData",
    }
}

/// Keep an adapter or error string short and free of opaque runs.
fn tidy(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ',' | '(' | ')'))
        .take(40)
        .collect()
}

fn hex_to_bytes(secret: &SecretString) -> Option<Vec<u8>> {
    let text = secret.expose();
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()
        })
        .collect()
}

struct PullResult {
    report: AcquisitionReport,
    evidence: TransferEvidence,
    restaged_matches: bool,
    playable: DerivativeOutcome,
}

struct Driver<'a, B: DeviceBackend> {
    backend: &'a mut B,
    config: &'a LiveConfig<'a>,
    trace: Trace,
    out: Observations,
}

impl<B: DeviceBackend> Driver<'_, B> {
    fn record(&mut self, id: &str, outcome: CheckOutcome, detail: impl Into<String>) {
        self.out.checks.insert(
            id.to_owned(),
            ObservedCheck {
                outcome,
                detail: detail.into(),
            },
        );
    }

    fn written_frames(&self) -> usize {
        self.trace.lock().expect("trace lock").len()
    }

    fn authenticate_frames(&self) -> usize {
        self.trace
            .lock()
            .expect("trace lock")
            .iter()
            .filter(|opcode| **opcode == ReadOnlyOpcode::Authenticate)
            .count()
    }

    async fn open(&mut self, id: &str) -> Result<(DeviceHandle, ProbeResult), DeviceError> {
        let inner = self.backend.connect(id).await?;
        let transport = RecordingTransport {
            inner,
            trace: Arc::clone(&self.trace),
        };
        let (events, _unused) = mpsc::unbounded_channel();
        let handle = DeviceActor::new(transport, self.config.profile, events).spawn();
        let probe = handle.probe().await?;
        Ok((handle, probe))
    }

    /// Run every derivable check.
    async fn run(&mut self) {
        let Some(peripheral) = self.discover().await else {
            return;
        };

        // ---- Session A: a wrong credential, before any real one is used. ----
        let (handle, probe) = match self.open(&peripheral).await {
            Ok(opened) => opened,
            Err(error) => {
                let detail = format!("the probe failed: {error}");
                self.record(
                    "authorization_failure",
                    CheckOutcome::Skipped,
                    detail.clone(),
                );
                self.record("authorization_success", CheckOutcome::Skipped, detail);
                return;
            }
        };
        let model = probe.device_model.clone().unwrap_or_default();
        let firmware = probe.firmware.clone().unwrap_or_default();
        self.out.observed_firmware = if firmware.is_empty() {
            "unknown".to_owned()
        } else {
            firmware.clone()
        };

        let proceed = self.wrong_credential(&handle).await;
        let _ = handle.disconnect().await;
        if !proceed {
            return;
        }

        self.missing_secret(&peripheral, &model, &firmware);

        // ---- Session B: the real credential. ----
        let binding = DeviceBinding::new(
            peripheral.clone(),
            model.clone(),
            firmware.clone(),
            self.config.route,
            IdentityAssurance::Unverified,
        );
        let credentials = DeviceCredentials::new(self.config.store);
        let secret = match credentials.load(&binding, &peripheral, &model, &firmware) {
            Ok(secret) => secret,
            Err(CredentialError::Store(SecretStoreError::NotFound)) => {
                self.record(
                    "authorization_success",
                    CheckOutcome::Skipped,
                    "no credential is provisioned in the vault for this device",
                );
                return;
            }
            Err(error) => {
                self.record(
                    "authorization_success",
                    CheckOutcome::Failed,
                    format!("the vault lookup failed: {}", tidy(&error.to_string())),
                );
                return;
            }
        };
        let Some(token) = hex_to_bytes(&secret) else {
            self.record(
                "authorization_success",
                CheckOutcome::Failed,
                "the stored credential is not in the expected form",
            );
            return;
        };
        drop(secret);

        let (handle, _probe) = match self.open(&peripheral).await {
            Ok(opened) => opened,
            Err(error) => {
                self.record(
                    "authorization_success",
                    CheckOutcome::Failed,
                    format!("reconnecting failed: {error}"),
                );
                return;
            }
        };
        match handle.authenticate(token).await {
            Ok(()) => self.record(
                "authorization_success",
                CheckOutcome::Passed,
                "the vault credential authenticated",
            ),
            Err(error) => {
                self.record(
                    "authorization_success",
                    CheckOutcome::Failed,
                    format!("authentication failed: {error}"),
                );
                let _ = handle.disconnect().await;
                return;
            }
        }

        self.listings_and_pulls(&handle).await;
        let _ = handle.disconnect().await;
    }

    /// Scan, record the discovery checks and select a peripheral by id.
    async fn discover(&mut self) -> Option<String> {
        let scan = match self.backend.scan().await {
            Ok(scan) => scan,
            Err(DeviceError::RadioPermissionDenied) => {
                self.record(
                    "bluetooth_permission",
                    CheckOutcome::Failed,
                    "the operating system denied the Bluetooth scan",
                );
                self.record(
                    "discover_only",
                    CheckOutcome::Failed,
                    "the scan was not permitted",
                );
                return None;
            }
            Err(error) => {
                self.record(
                    "discover_only",
                    CheckOutcome::Failed,
                    format!("the scan failed: {error}"),
                );
                return None;
            }
        };

        let frames_during_scan = self.written_frames();
        if scan.candidates.is_empty() {
            self.record(
                "discover_only",
                CheckOutcome::Failed,
                format!(
                    "no candidate among {} advertisements on adapter {}",
                    scan.advertisements,
                    tidy(&scan.adapter)
                ),
            );
            return None;
        }
        self.record(
            "discover_only",
            if frames_during_scan == 0 {
                CheckOutcome::Passed
            } else {
                CheckOutcome::Failed
            },
            format!(
                "{} candidates among {} advertisements on adapter {}; nothing connected or written",
                scan.candidates.len(),
                scan.advertisements,
                tidy(&scan.adapter)
            ),
        );

        let mut shared_name = false;
        for (index, candidate) in scan.candidates.iter().enumerate() {
            if candidate.name.is_some()
                && scan.candidates[index + 1..]
                    .iter()
                    .any(|other| other.name == candidate.name && other.id != candidate.id)
            {
                shared_name = true;
            }
        }
        if shared_name {
            self.record(
                "device_name_collision",
                CheckOutcome::Passed,
                "two candidates share an advertised name and are told apart by identifier",
            );
        } else {
            self.record(
                "device_name_collision",
                CheckOutcome::Skipped,
                "no two candidates in range share an advertised name",
            );
        }

        let chosen = match (&self.config.selection, scan.candidates.as_slice()) {
            (Some(wanted), candidates) => {
                let found = candidates.iter().any(|candidate| &candidate.id == wanted);
                if candidates.len() > 1 {
                    self.record(
                        "two_devices_in_range",
                        if found {
                            CheckOutcome::Passed
                        } else {
                            CheckOutcome::Failed
                        },
                        format!(
                            "{} candidates; the device was selected by identifier",
                            candidates.len()
                        ),
                    );
                }
                found.then(|| wanted.clone())
            }
            (None, [only]) => Some(only.id.clone()),
            (None, many) => {
                self.record(
                    "two_devices_in_range",
                    CheckOutcome::Passed,
                    format!(
                        "{} candidates; refused to auto-select, an explicit identifier is required",
                        many.len()
                    ),
                );
                None
            }
        };
        if chosen.is_none() && self.config.selection.is_some() {
            self.record(
                "discover_only",
                CheckOutcome::Failed,
                "the selected identifier is not among the candidates",
            );
        }
        if scan.candidates.len() < 2 {
            self.record(
                "two_devices_in_range",
                CheckOutcome::Skipped,
                "only one candidate was in range",
            );
        }
        chosen
    }

    /// Authenticate with a random credential. Returns whether the run may
    /// continue to the real credential.
    async fn wrong_credential(&mut self, handle: &DeviceHandle) -> bool {
        let length = self
            .config
            .profile
            .auth_token_byte_lengths
            .first()
            .copied()
            .unwrap_or(16);
        let mut wrong = Vec::with_capacity(length);
        while wrong.len() < length {
            wrong.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        }
        wrong.truncate(length);

        let before = self.authenticate_frames();
        let result = handle.authenticate(wrong).await;
        let attempts = self.authenticate_frames() - before;
        match result {
            Err(DeviceError::AuthRejected) if attempts == 1 => {
                self.record(
                    "authorization_failure",
                    CheckOutcome::Passed,
                    "a random credential was refused after one attempt",
                );
                self.record(
                    "encrypted_handshake_detected",
                    CheckOutcome::Skipped,
                    "the device answered on the legacy path; no encrypted mode was offered",
                );
                true
            }
            Err(DeviceError::AuthRejected) => {
                self.record(
                    "authorization_failure",
                    CheckOutcome::Failed,
                    format!("the credential was sent {attempts} times; it must be sent once"),
                );
                false
            }
            Err(DeviceError::UnsupportedFirmware) if attempts <= 1 => {
                self.record(
                    "encrypted_handshake_detected",
                    CheckOutcome::Passed,
                    "after a good probe the device required the encrypted handshake; \
                     the legacy path was not retried",
                );
                let skipped =
                    "the device requires the encrypted handshake, which is not implemented";
                self.record("authorization_failure", CheckOutcome::Skipped, skipped);
                self.record("authorization_success", CheckOutcome::Skipped, skipped);
                false
            }
            Ok(()) => {
                self.record(
                    "authorization_failure",
                    CheckOutcome::Failed,
                    "the device accepted a random credential",
                );
                false
            }
            Err(error) => {
                self.record(
                    "authorization_failure",
                    CheckOutcome::Failed,
                    format!("unexpected result: {error}"),
                );
                false
            }
        }
    }

    /// An identity with no vault entry must fail closed and send nothing.
    fn missing_secret(&mut self, peripheral: &str, model: &str, firmware: &str) {
        let unbound = format!("{peripheral}-no-secret-{}", uuid::Uuid::new_v4().simple());
        let binding = DeviceBinding::new(
            unbound.clone(),
            model,
            firmware,
            self.config.route,
            IdentityAssurance::Unverified,
        );
        let frames = self.written_frames();
        let outcome =
            DeviceCredentials::new(self.config.store).load(&binding, &unbound, model, firmware);
        match outcome {
            Err(CredentialError::Store(SecretStoreError::NotFound))
                if self.written_frames() == frames =>
            {
                self.record(
                    "missing_secret",
                    CheckOutcome::Passed,
                    "an identity with no stored secret failed closed and no frame was written",
                );
            }
            Err(error) => self.record(
                "missing_secret",
                CheckOutcome::Failed,
                format!("unexpected result: {}", tidy(&error.to_string())),
            ),
            Ok(_) => self.record(
                "missing_secret",
                CheckOutcome::Failed,
                "a secret was found for an identity that was never provisioned",
            ),
        }
    }

    async fn listings_and_pulls(&mut self, handle: &DeviceHandle) {
        // Back-to-back read-only listings: the non-destructive audit.
        let mut audit = NonDestructiveAudit::new();
        let mut first = None;
        for _ in 0..2 {
            match handle.list().await {
                Ok(listing) => {
                    audit.observe(AuditObservation::from_listing(
                        &listing.entries,
                        listing.truncated,
                        None,
                    ));
                    first.get_or_insert(listing);
                }
                Err(error) => {
                    audit.abandon(format!("listing failed: {error}"));
                    break;
                }
            }
        }
        self.out.non_destructive_scan_observed = audit.outcome().is_non_destructive();
        let Some(before) = first else {
            self.record(
                "listing_delta_before_after_phone_sync",
                CheckOutcome::Failed,
                "the device could not be listed",
            );
            return;
        };

        self.phone_sync(handle, &before).await;
        self.pulls(handle, &before).await;
    }

    async fn phone_sync(
        &mut self,
        handle: &DeviceHandle,
        before: &plaudon_plaud_protocol::payload::DeltaListing,
    ) {
        let id = "listing_delta_before_after_phone_sync";
        let Some(wait) = self.config.phone_sync.clone() else {
            self.record(
                id,
                CheckOutcome::Skipped,
                "no phone synchronization was performed in this run",
            );
            return;
        };
        eprintln!(
            "ACTION: synchronize the PLAUD phone app now, then create the file {}",
            wait.flag.display()
        );
        let deadline = tokio::time::Instant::now() + wait.timeout;
        while !wait.flag.exists() {
            if tokio::time::Instant::now() >= deadline {
                self.record(
                    id,
                    CheckOutcome::Skipped,
                    "the operator did not signal a phone synchronization in time",
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        match handle.list().await {
            Ok(after) => {
                let was: Vec<&str> = before.entries.iter().map(|e| e.id.as_str()).collect();
                let now: Vec<&str> = after.entries.iter().map(|e| e.id.as_str()).collect();
                let gone = was.iter().filter(|id| !now.contains(id)).count();
                let new = now.iter().filter(|id| !was.contains(id)).count();
                self.record(
                    id,
                    CheckOutcome::Passed,
                    format!(
                        "listed {} before and {} after the phone synchronization; \
                         {gone} no longer listed, {new} newly listed",
                        was.len(),
                        now.len()
                    ),
                );
            }
            Err(error) => self.record(
                id,
                CheckOutcome::Failed,
                format!("listing after the phone synchronization failed: {error}"),
            ),
        }
    }

    async fn pulls(
        &mut self,
        handle: &DeviceHandle,
        listing: &plaudon_plaud_protocol::payload::DeltaListing,
    ) {
        let staging = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => {
                self.record(
                    "imported_bytes_verified",
                    CheckOutcome::Failed,
                    format!("no staging directory: {}", tidy(&error.to_string())),
                );
                return;
            }
        };

        let mut results: Vec<PullResult> = Vec::new();
        let mut failures = 0usize;
        let mut first_failed_summary = None;
        for (index, recording) in listing
            .entries
            .iter()
            .filter(|entry| entry.declared_byte_length <= self.config.max_pull_bytes)
            .take(self.config.max_pulls)
            .enumerate()
        {
            match self
                .pull(
                    handle,
                    recording,
                    &staging.path().join(format!("pull-{index}.asr")),
                )
                .await
            {
                Ok(result) if result.report.may_commit() && result.restaged_matches => {
                    results.push(result);
                }
                Ok(result) => {
                    failures += 1;
                    first_failed_summary.get_or_insert(result.evidence.clone());
                    results.push(result);
                }
                Err(detail) => {
                    failures += 1;
                    eprintln!("pull of one recording failed: {detail}");
                }
            }
        }

        let verified: Vec<&PullResult> = results
            .iter()
            .filter(|r| r.report.may_commit() && r.restaged_matches)
            .collect();

        for (id, low, high) in PULL_BUCKETS {
            let landed = verified.iter().any(|pull| {
                pull.evidence
                    .measured_duration_ms
                    .is_some_and(|ms| (*low..=*high).contains(&ms))
            });
            if landed {
                self.record(id, CheckOutcome::Passed, "a verified pull of this length");
            } else if failures > 0 {
                self.record(
                    id,
                    CheckOutcome::Failed,
                    "no verified pull of this length, and at least one pull failed",
                );
            } else {
                self.record(
                    id,
                    CheckOutcome::Skipped,
                    "no listed recording of this length within the pull limits",
                );
            }
        }

        // The transfer evidence: the first verified pull, else the first attempt.
        self.out.transfer = verified
            .first()
            .map(|pull| pull.evidence.clone())
            .or_else(|| results.first().map(|pull| pull.evidence.clone()));

        if results.is_empty() {
            self.record(
                "imported_bytes_verified",
                CheckOutcome::Skipped,
                "nothing was pulled in this run",
            );
            self.record(
                "opus_decode_measurement",
                CheckOutcome::Skipped,
                "nothing was pulled in this run",
            );
            return;
        }

        if results.iter().all(|pull| pull.restaged_matches) && failures == 0 {
            self.record(
                "imported_bytes_verified",
                CheckOutcome::Passed,
                format!(
                    "{} staged files re-hashed to the acquired digest and length",
                    results.len()
                ),
            );
        } else {
            self.record(
                "imported_bytes_verified",
                CheckOutcome::Failed,
                "a staged file did not match the acquired digest and length, or a pull failed",
            );
        }

        match results.iter().find(|pull| pull.report.framing.is_some()) {
            Some(pull) => {
                let summary = pull.report.framing.expect("filtered on Some");
                let remux = match &pull.playable {
                    DerivativeOutcome::Produced { .. } => "Ogg Opus remux produced",
                    DerivativeOutcome::Unavailable { .. } => "Ogg Opus remux unavailable",
                };
                self.record(
                    "opus_decode_measurement",
                    if matches!(pull.playable, DerivativeOutcome::Produced { .. }) {
                        CheckOutcome::Passed
                    } else {
                        CheckOutcome::Failed
                    },
                    format!(
                        "{} packets, {} ms measured from the bitstream (no PCM decoder in this \
                         build); {remux}",
                        summary.packet_count, summary.duration_ms
                    ),
                );
            }
            None => self.record(
                "opus_decode_measurement",
                CheckOutcome::Failed,
                "the acquired bytes did not parse as an Opus sidecar",
            ),
        }
    }

    async fn pull(
        &mut self,
        handle: &DeviceHandle,
        recording: &ListedRecording,
        path: &Path,
    ) -> Result<PullResult, String> {
        let file = File::create(path).map_err(|error| tidy(&error.to_string()))?;
        let acquisition = StagingAcquisition::begin(self.config.profile, recording, file)
            .map_err(|error| tidy(&error.to_string()))?;
        let sink = SharedSink(Arc::new(Mutex::new(Some(acquisition))));
        let outcome = handle
            .transfer(
                recording.clone(),
                Box::new(sink.clone()),
                CancellationToken::new(),
            )
            .await;
        let acquisition = sink
            .0
            .lock()
            .expect("sink lock")
            .take()
            .ok_or_else(|| "the acquisition was lost".to_owned())?;
        let report = acquisition
            .finish()
            .map_err(|error| tidy(&error.to_string()))?;
        let summary = outcome.map_err(|error| error.to_string())?;

        let mut digest = Sha256::new();
        let mut restaged = File::open(path).map_err(|error| tidy(&error.to_string()))?;
        let mut length = 0u64;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = restaged
                .read(&mut buffer)
                .map_err(|error| tidy(&error.to_string()))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            length += read as u64;
        }
        let digest_hex =
            digest
                .finalize()
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    use std::fmt::Write as _;
                    let _ = write!(hex, "{byte:02x}");
                    hex
                });
        let restaged_matches = digest_hex == report.identity.sha256.to_hex()
            && length == report.identity.byte_length
            && length == summary.received_bytes;

        let mut source = File::open(path).map_err(|error| tidy(&error.to_string()))?;
        let playable = derive_playable(&report, &mut source, &mut std::io::sink());

        let evidence = TransferEvidence {
            device_recording_id: report.device_recording_id.clone(),
            acquired_mime: report.identity.observed_codec.clone(),
            quality_class: serde_json::to_value(report.quality_class)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned()),
            received_bytes: report.verification.received_bytes,
            device_declared_byte_length: report.verification.device_declared_byte_length,
            terminal_marker_observed: summary.terminal_marker_observed
                && report.verification.terminal_marker_observed,
            sha256_hex: report.identity.sha256.to_hex(),
            measured_duration_ms: report.verification.measured_duration_ms,
        };
        Ok(PullResult {
            report,
            evidence,
            restaged_matches,
            playable,
        })
    }
}

/// Run every derivable check against `backend` and return what was observed.
///
/// The result carries `checks`, `emitted_opcodes`, `observed_firmware`,
/// `non_destructive_scan_observed` and `transfer`. It does not set
/// `network_denied_run`: only the operator can attest to a firewall.
pub async fn drive<B: DeviceBackend>(backend: &mut B, config: &LiveConfig<'_>) -> Observations {
    let trace: Trace = Arc::new(Mutex::new(Vec::new()));
    let mut driver = Driver {
        backend,
        config,
        trace: Arc::clone(&trace),
        out: Observations::default(),
    };
    driver.run().await;
    let mut out = driver.out;
    out.emitted_opcodes = trace
        .lock()
        .expect("trace lock")
        .iter()
        .map(|opcode| opcode_name(*opcode).to_owned())
        .collect();
    out
}

/// Checks the live driver owns. The operator cannot supply or override them;
/// one the run never reached is recorded `skipped`.
pub const LIVE_OWNED: &[&str] = &[
    "discover_only",
    "device_name_collision",
    "two_devices_in_range",
    "authorization_failure",
    "authorization_success",
    "missing_secret",
    "encrypted_handshake_detected",
    "listing_delta_before_after_phone_sync",
    "transfer_pull_15s",
    "transfer_pull_5min",
    "transfer_pull_30min",
    "transfer_pull_multi_hour",
    "imported_bytes_verified",
    "opus_decode_measurement",
];

/// Combine live results with the operator's observations.
///
/// A live result always wins, and the operator cannot speak for a check the
/// run owns. The operator contributes only what software cannot know: whether
/// the packaged app was granted Bluetooth, sleep/wake, disconnects, battery,
/// disk, USB, process kill, timestamps against a trusted source and the
/// firewall.
#[must_use]
pub fn merge_operator(mut live: Observations, operator: &Observations) -> Observations {
    for (id, check) in &operator.checks {
        if !LIVE_OWNED.contains(&id.as_str()) {
            live.checks
                .entry(id.clone())
                .or_insert_with(|| check.clone());
        }
    }
    for id in LIVE_OWNED {
        live.checks
            .entry((*id).to_owned())
            .or_insert_with(|| ObservedCheck {
                outcome: CheckOutcome::Skipped,
                detail: "not reached in this run".to_owned(),
            });
    }
    live.network_denied_run = operator.network_denied_run;
    live
}

/// Selects one peripheral by its host identifier when several are in range.
pub const PERIPHERAL_ENV: &str = "PLAUDON_HARDWARE_PERIPHERAL";
/// Most recordings a run pulls. Default 4.
pub const MAX_PULLS_ENV: &str = "PLAUDON_HARDWARE_MAX_PULLS";
/// Largest declared recording length a run pulls, in bytes. Default 512 MiB.
pub const MAX_PULL_BYTES_ENV: &str = "PLAUDON_HARDWARE_MAX_PULL_BYTES";
/// File the operator creates after a phone synchronization.
pub const PHONE_SYNC_FLAG_ENV: &str = "PLAUDON_HARDWARE_PHONE_SYNC_FLAG";
/// Seconds to wait for the phone-synchronization flag. Default 600.
pub const PHONE_SYNC_WAIT_ENV: &str = "PLAUDON_HARDWARE_PHONE_SYNC_WAIT_SECS";
/// Seconds to scan. Default 8.
pub const SCAN_SECS_ENV: &str = "PLAUDON_HARDWARE_SCAN_SECS";

/// Live-run settings that come from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSettings {
    /// Operator-chosen peripheral identifier.
    pub selection: Option<String>,
    /// Most recordings to pull.
    pub max_pulls: usize,
    /// Largest declared length to pull.
    pub max_pull_bytes: u64,
    /// Phone synchronization hand-off.
    pub phone_sync: Option<PhoneSyncWait>,
    /// How long to scan.
    pub scan: Duration,
}

impl LiveSettings {
    /// Read the settings. An unparseable number falls back to its default, so
    /// a typo can only make a run smaller or slower, never wider.
    #[must_use]
    pub fn from_env(env: &dyn crate::gate::Env) -> Self {
        let number = |key: &str, default: u64| {
            env.get(key)
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(default)
        };
        Self {
            selection: env
                .get(PERIPHERAL_ENV)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            max_pulls: usize::try_from(number(MAX_PULLS_ENV, 4)).unwrap_or(4),
            max_pull_bytes: number(MAX_PULL_BYTES_ENV, 512 * 1024 * 1024),
            phone_sync: env
                .get(PHONE_SYNC_FLAG_ENV)
                .filter(|value| !value.trim().is_empty())
                .map(|flag| PhoneSyncWait {
                    flag: PathBuf::from(flag),
                    timeout: Duration::from_secs(number(PHONE_SYNC_WAIT_ENV, 600)),
                }),
            scan: Duration::from_secs(number(SCAN_SECS_ENV, 8)),
        }
    }
}

/// Why a register cell cannot be driven live.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LiveError {
    /// No protocol profile is named for the cell's model and firmware range.
    #[error("no protocol profile covers the device model and firmware range of this cell")]
    NoProfile,
    /// The cell's provisioning method is not a route this suite understands.
    #[error("the provisioning method {0:?} is not a known route")]
    UnknownRoute(String),
}

/// The protocol profile that covers `cell`.
pub fn protocol_profile_for(
    cell: &plaudon_source_plaud_device::register::CompatibilityCell,
) -> Result<&'static ProtocolProfile, LiveError> {
    cell.firmware_range
        .iter()
        .find_map(|firmware| {
            plaudon_plaud_protocol::profile::profile_for(&cell.device_model, firmware)
        })
        .ok_or(LiveError::NoProfile)
}

/// The provisioning route a cell names.
pub fn route_for(
    cell: &plaudon_source_plaud_device::register::CompatibilityCell,
) -> Result<ProvisioningRoute, LiveError> {
    serde_json::from_value(serde_json::Value::String(cell.provisioning_method.clone()))
        .ok()
        .or_else(|| {
            (cell.auth_mode == "encrypted_handshake")
                .then_some(ProvisioningRoute::EncryptedModeProvisioning)
        })
        .ok_or_else(|| LiveError::UnknownRoute(cell.provisioning_method.clone()))
}
