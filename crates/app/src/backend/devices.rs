//! The direct PLAUD device backend behind [`DevicesPort`] (spec §4.2A, §7.9,
//! §15.2 `device_*`, App. B.8).
//!
//! This module is the single place that turns evidence into the words the
//! Devices workspace shows. It owns no radio and no wire protocol: the BLE
//! actor is a [`DeviceRadio`] injected at construction (EPIC L core), and
//! everything decided here is decided from real crate types:
//!
//! * discovery goes through [`DiscoveryPolicy`]: an advertisement is a
//!   *candidate*, never an identity;
//! * every probed `(model, firmware, host, transport, auth)` tuple goes
//!   through [`GatePolicy`] over the committed `compatibility.yaml`; an
//!   unknown tuple grants nothing, and no simulator result can promote a cell;
//! * the device credential lives only in the OS vault through
//!   [`DeviceCredentials`]; what this module persists is a [`DeviceBinding`]
//!   (identifiers, no token bytes);
//! * transfer states come from [`TransferLedger`], and their labels from
//!   [`TransferState::ui_label`];
//! * "may automatic sync be advertised" is [`SyncEligibility`].
//!
//! # Honest unknowns
//!
//! A field the probe could not establish is `None`, never a guess. A radio this
//! build does not have is reported as [`RadioState::UnavailableInBuild`], never
//! as an empty list that would read as "no devices nearby" (spec §5.4).
//!
//! # Read-only
//!
//! [`DeviceRadio`] has no method that deletes, clears, controls recording,
//! writes settings, updates firmware or acknowledges a sync. Such a call is not
//! representable here (spec §7.9).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{DeviceId, TransferId};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::device::{
    DeviceRecord, DeviceRepository, TransferRecord as DeviceTransferRow, TransferStatus,
    TransferVerification as RowVerification, TrustState,
};
use plaudon_plaud_ble::discovery::{AdvertisementVerdict, DiscoveryPolicy, ObservedAdvertisement};
use plaudon_plaud_protocol::payload::{DeltaListing, ListedRecording};
use plaudon_plaud_protocol::profile::{
    profile_for, ProfileAuthMode, ProtocolProfile, LEGACY_NOTE_V0095,
};
use plaudon_security::secrets::{SecretStore as VaultStore, SecretString};
use plaudon_source_plaud_device::audit::AuditOutcome;
use plaudon_source_plaud_device::{
    CompatibilityRegister, CredentialError, DeviceBinding, DeviceCredentials,
    ExperimentalProbeGrant, ExperimentalProbeSession, GateDecision, GatePolicy, IdentityAssurance,
    ProbedProfile, ProvisioningRoute, SupportStatus, SyncEligibility, TransferEvent,
    TransferLedger, TransferState, LEGACY_DISCLOSURES,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

use crate::dto::{
    CloudRoute, DeviceAuthStatusDto, DeviceCandidateDto, DeviceCapability, DeviceCapabilityDto,
    DeviceGateDto, DeviceListingDto, DeviceProbeDto, DeviceRecordingDto, DeviceScanDto,
    DeviceSection, DeviceSyncPolicyDto, DeviceTransferDto, EventKind, ListingCompleteness,
    RadioState, RadioStatusDto,
};
use crate::error::{internal, invalid_argument};
use crate::events::{EventDraft, EventPublisher};
use crate::permissions::radio_state_for;
use crate::ports::DevicesPort;
use crate::secrets::SecretValue;

/// The committed compatibility register: the only source of compatibility
/// strings this module shows (spec §2.5B).
const COMPATIBILITY_YAML: &str = include_str!("../../../../compatibility.yaml");

/// `app_preferences` key holding the peripheral identifiers whose user opted in
/// to read-only sync when nearby. Identifiers only; no credential material.
const SYNC_KEY: &str = "desktop.device_sync";

/// `source_type` of a direct BLE profile in the register.
const SOURCE_TYPE_BLE: &str = "plaud_device_ble";

/// Transport name of a direct BLE profile in the register.
const TRANSPORT_BLE: &str = "ble";

/// Register `quality_class` of bytes that come from the compressed `.ASR`
/// stream rather than from an original recording.
const QUALITY_COMPRESSED: &str = "compressed_device_transfer";

/// The label of BLE media (App. B.8): never "original high-quality audio".
pub const SOURCE_LABEL_BLE_COMPRESSED: &str = "Device BLE · compressed ASR";

/// The label when the quality class of the bytes has not been established.
pub const SOURCE_LABEL_BLE_UNKNOWN: &str = "Device BLE · quality not established";

/// `ListingCompleteness::UnsyncedDelta` explanation (spec §4.2A.5, §4.2A.8).
pub const COMPLETENESS_DELTA: &str = "Unsynced recordings only. Recordings already synchronized \
     to a phone may be missing, and direct transfer cannot retrieve files deleted from the \
     device, so this is not a complete recording history.";

/// `ListingCompleteness::UnsyncedDeltaTruncated` explanation.
pub const COMPLETENESS_DELTA_TRUNCATED: &str = "Unsynced recordings only, and the device returned \
     fewer entries than it holds, so more unsynced recordings may exist. Recordings already \
     synchronized to a phone may be missing; this is not a complete recording history.";

/// `ListingCompleteness::Unknown` explanation.
pub const COMPLETENESS_UNKNOWN: &str = "How complete this listing is has not been established. \
     Do not treat it as a complete recording history.";

/// Radio message when the OS denied access.
pub const RADIO_DENIED_MESSAGE: &str = "Bluetooth access is not permitted for this application, \
     so no scan ran. Allow Bluetooth for Plaudon in your system settings, then scan again.";

/// Radio message when the user has not answered the Bluetooth prompt yet.
pub const RADIO_NOT_DETERMINED_MESSAGE: &str = "Plaudon has not been given Bluetooth access \
     yet, so no scan ran. This is not the same as no devices being nearby. Answer the system's \
     Bluetooth prompt, or allow Plaudon under Bluetooth in your system settings, then scan again.";

/// Radio message when the adapter is switched off.
pub const RADIO_POWERED_OFF_MESSAGE: &str = "Bluetooth is turned off on this computer, so no \
     scan ran. This is not the same as no devices being nearby. Turn Bluetooth on, then scan \
     again.";

/// Radio message when the host reports no usable adapter.
pub const RADIO_NO_ADAPTER_MESSAGE: &str = "This computer reported no usable Bluetooth \
     adapter, so no scan ran. This is not the same as no devices being nearby. Check that the \
     adapter is connected and enabled, then scan again. Import files works without a device.";

/// The plain-language message for a radio that refused a scan.
const fn radio_refusal_message(state: RadioState) -> &'static str {
    match state {
        RadioState::PermissionNotDetermined => RADIO_NOT_DETERMINED_MESSAGE,
        RadioState::PoweredOff => RADIO_POWERED_OFF_MESSAGE,
        RadioState::NoAdapter => RADIO_NO_ADAPTER_MESSAGE,
        RadioState::UnavailableInBuild => RADIO_UNAVAILABLE_MESSAGE,
        RadioState::PermissionDenied | RadioState::Scanned => RADIO_DENIED_MESSAGE,
    }
}

/// Radio message when this build has no radio.
pub const RADIO_UNAVAILABLE_MESSAGE: &str = "This build of Plaudon cannot scan for Bluetooth \
     devices, so no scan ran. This is not the same as no devices being nearby. Import files \
     works without a device.";

/// Radio message after a scan that ran.
pub const RADIO_SCANNED_MESSAGE: &str = "Scan finished. Only devices advertising right now \
     appear as nearby.";

/* ----------------------------------------------------------------- radio */

/// Whether this build can use a BLE radio at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioAvailability {
    /// A radio implementation is present; the OS may still deny it.
    Present,
    /// This build was compiled without a radio (no `hardware-scan`).
    NotInThisBuild,
}

/// What a probe established. A `None` is an honest unknown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RadioProbe {
    /// Model, when the device reported one.
    pub model: Option<String>,
    /// Firmware fingerprint, when it could be read.
    pub firmware: Option<String>,
    /// Authentication mode the device offered (`legacy_static_token`, …).
    pub auth_mode: Option<String>,
    /// Battery level from the standard battery characteristic.
    pub battery_percent: Option<u8>,
    /// Signal strength observed while probing.
    pub rssi_dbm: Option<i16>,
}

/// One step of a transfer, reported by the radio actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferUpdate {
    /// Device-scoped recording identifier.
    pub recording_id: String,
    /// What happened.
    pub event: TransferEvent,
    /// Bytes received so far, when the step carries a count.
    pub bytes_received: Option<u64>,
    /// Milliseconds since the first byte arrived, as the actor measured it.
    pub elapsed_ms: Option<u64>,
    /// Throughput the actor measured; preferred over one derived from
    /// `elapsed_ms`.
    pub bytes_per_second: Option<u64>,
    /// The actor's estimate of the seconds remaining. Always an estimate.
    pub eta_seconds: Option<u64>,
}

impl TransferUpdate {
    /// A step that carries no byte count.
    #[must_use]
    pub fn step(recording_id: impl Into<String>, event: TransferEvent) -> Self {
        Self {
            recording_id: recording_id.into(),
            event,
            bytes_received: None,
            elapsed_ms: None,
            bytes_per_second: None,
            eta_seconds: None,
        }
    }
}

/// Where the radio actor reports transfer steps.
#[async_trait]
pub trait TransferReporter: Send + Sync {
    /// Record one step. Illegal transitions are ignored by the ledger.
    async fn report(&self, update: TransferUpdate);
}

/// A read-only transfer request handed to the radio actor.
#[derive(Debug)]
pub struct TransferRequest<'a> {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: &'a str,
    /// The ledger row of this transfer (already `in_flight`).
    pub transfer_id: TransferId,
    /// The recording as the device's own listing described it; its declared
    /// length is a claim, not a measurement.
    pub recording: &'a ListedRecording,
    /// The device credential, loaded from the vault for this transfer only.
    pub token: &'a SecretString,
    /// Process the result locally only.
    pub local_only: bool,
}

/// The BLE radio actor seam (EPIC L core owns the implementation).
///
/// There is deliberately no method that writes to the device.
#[async_trait]
pub trait DeviceRadio: Send + Sync + fmt::Debug {
    /// Whether a radio exists in this build.
    fn availability(&self) -> RadioAvailability;

    /// One passive scan. A denied radio is
    /// [`AppErrorCode::RadioPermissionDenied`], never an empty list.
    async fn scan(&self, timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError>;

    /// Establish model, firmware and authentication mode of one device.
    async fn probe(&self, peripheral_id: &str) -> Result<RadioProbe, AppError>;

    /// Authenticate with `token` and list the device's recordings.
    async fn list(
        &self,
        peripheral_id: &str,
        token: &SecretString,
    ) -> Result<DeltaListing, AppError>;

    /// Transfer one recording, reporting every step to `reporter`.
    async fn transfer(
        &self,
        request: TransferRequest<'_>,
        reporter: &dyn TransferReporter,
    ) -> Result<(), AppError>;

    /// Drop the link to a device.
    async fn disconnect(&self, peripheral_id: &str);
}

/// The radio of a build without `hardware-scan`: it exists only to say so.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRadio;

#[async_trait]
impl DeviceRadio for NoRadio {
    fn availability(&self) -> RadioAvailability {
        RadioAvailability::NotInThisBuild
    }

    async fn scan(&self, _timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError> {
        Err(AppError::new(
            AppErrorCode::SourceUnavailable,
            "this build has no BLE radio",
        ))
    }

    async fn probe(&self, _peripheral_id: &str) -> Result<RadioProbe, AppError> {
        Err(AppError::new(
            AppErrorCode::SourceUnavailable,
            "this build has no BLE radio",
        ))
    }

    async fn list(
        &self,
        _peripheral_id: &str,
        _token: &SecretString,
    ) -> Result<DeltaListing, AppError> {
        Err(AppError::new(
            AppErrorCode::SourceUnavailable,
            "this build has no BLE radio",
        ))
    }

    async fn transfer(
        &self,
        _request: TransferRequest<'_>,
        _reporter: &dyn TransferReporter,
    ) -> Result<(), AppError> {
        Err(AppError::new(
            AppErrorCode::SourceUnavailable,
            "this build has no BLE radio",
        ))
    }

    async fn disconnect(&self, _peripheral_id: &str) {}
}

/* ------------------------------------------------------------------ host */

/// The host half of a compatibility tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProfile {
    /// `macos`, `windows` or `linux`.
    pub host_os: String,
    /// Version key as the register spells it (`27.0`, `11`); empty if unknown.
    pub os_version: String,
}

impl HostProfile {
    /// The machine this process runs on.
    #[must_use]
    pub fn detect() -> Self {
        let host_os = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        };
        Self {
            host_os: host_os.to_owned(),
            os_version: os_version_key(host_os, sysinfo::System::os_version().as_deref()),
        }
    }
}

/// Reduce an OS-reported version to the key the register uses.
///
/// macOS: `major.minor`. Windows: `11` when the build number proves
/// Windows 11, otherwise the raw string (which then matches no cell). Anything
/// unrecognised is returned trimmed and therefore fails closed.
#[must_use]
pub fn os_version_key(host_os: &str, raw: Option<&str>) -> String {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return String::new();
    };
    match host_os {
        "macos" => {
            let mut parts = raw.split('.');
            let major = parts.next().unwrap_or_default();
            let minor = parts.next().unwrap_or("0");
            if major.chars().all(|c| c.is_ascii_digit()) && !major.is_empty() {
                format!("{major}.{minor}")
            } else {
                raw.to_owned()
            }
        }
        "windows" => {
            let build = raw
                .split(|c: char| !c.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse::<u32>().ok())
                .find(|number| *number >= 10_000);
            if raw.starts_with("11") || build.is_some_and(|build| build >= 22_000) {
                "11".to_owned()
            } else {
                raw.to_owned()
            }
        }
        _ => raw.to_owned(),
    }
}

/* ----------------------------------------------------------- persistence */

/// One `devices` ⨝ `device_credential_refs` row: peripheral identifier, model,
/// advertised name, firmware and last-seen time.
type LinkRow = (String, String, Option<String>, Option<String>, i64);

/// A device the user authorized: durable, secret-free.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkedDevice {
    binding: DeviceBinding,
    advertised_name: Option<String>,
    last_contact_ms: Option<i64>,
    sync_enabled: bool,
}

/* ------------------------------------------------------------ in-memory */

#[derive(Debug)]
struct ProbeRecord {
    probed: ProbedProfile,
    decision: GateDecision,
    protocol: Option<&'static ProtocolProfile>,
    radio: RadioProbe,
}

#[derive(Debug, Clone)]
struct SeenDevice {
    advertised_name: Option<String>,
    rssi_dbm: Option<i16>,
}

#[derive(Debug)]
struct StoredListing {
    entries: Vec<ListedRecording>,
}

#[derive(Debug)]
struct TransferRecord {
    transfer_id: String,
    ledger: TransferLedger,
    display_name: String,
    declared_byte_length: u64,
    captured_at_unix: u32,
    bytes_received: Option<u64>,
    observed_bytes_per_sec: Option<u64>,
    eta_seconds_estimate: Option<u64>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct State {
    nearby: HashMap<String, SeenDevice>,
    probes: HashMap<String, ProbeRecord>,
    experimental: HashMap<String, ExperimentalProbeSession>,
    authenticated: HashSet<String>,
    listings: HashMap<String, StoredListing>,
    transfers: HashMap<(String, String), TransferRecord>,
    audits: HashMap<String, AuditOutcome>,
}

/// Everything [`DevicesBackend`] is built from.
pub struct DevicesDeps {
    /// The compatibility register behind the gate.
    pub register: CompatibilityRegister,
    /// The host half of every compatibility tuple.
    pub host: HostProfile,
    /// The BLE radio actor.
    pub radio: Arc<dyn DeviceRadio>,
    /// The OS credential vault.
    pub vault: Arc<dyn VaultStore>,
    /// The library database: the device ledger lives in its `devices`,
    /// `device_credential_refs`, `device_transfers` and
    /// `device_firmware_observations` tables.
    pub db: Database,
    /// The media archive, for the durable commit of transferred bytes.
    pub archive: MediaArchive,
    /// The sequenced UI event stream.
    pub events: EventPublisher,
}

impl fmt::Debug for DevicesDeps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DevicesDeps")
    }
}

impl DevicesDeps {
    /// Production wiring: the committed register, this host, no radio unless
    /// one is supplied, the OS vault.
    pub fn production(
        db: Database,
        archive: MediaArchive,
        events: EventPublisher,
        radio: Arc<dyn DeviceRadio>,
    ) -> Result<Self, AppError> {
        let register = CompatibilityRegister::from_yaml(COMPATIBILITY_YAML)
            .map_err(|err| internal(format!("compatibility register is invalid: {err}")))?;
        Ok(Self {
            register,
            host: HostProfile::detect(),
            radio,
            vault: Arc::from(plaudon_security::secrets::os_store()),
            db,
            archive,
            events,
        })
    }
}

struct Inner {
    gate: GatePolicy,
    host: HostProfile,
    radio: Arc<dyn DeviceRadio>,
    vault: Arc<dyn VaultStore>,
    db: Database,
    devices: DeviceRepository,
    events: EventPublisher,
    state: Mutex<State>,
}

/// The [`DevicesPort`] implementation.
#[derive(Clone)]
pub struct DevicesBackend {
    inner: Arc<Inner>,
}

impl fmt::Debug for DevicesBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DevicesBackend")
    }
}

/* ------------------------------------------------------- pure derivations */

/// The register status a gate decision carries.
#[must_use]
pub fn decision_status(decision: &GateDecision) -> SupportStatus {
    match decision {
        GateDecision::Permitted { cell, .. } => cell.status,
        GateDecision::ExperimentalProbeAvailable { cell, .. } => cell
            .as_ref()
            .map_or(SupportStatus::NotEvaluated, |cell| cell.status),
        GateDecision::Refused { refusal } => match refusal {
            plaudon_source_plaud_device::GateRefusal::KnownUnsupported { .. } => {
                SupportStatus::KnownUnsupported
            }
            _ => SupportStatus::NotEvaluated,
        },
    }
}

fn refusal_code(decision: &GateDecision) -> Option<String> {
    match decision {
        GateDecision::Permitted { .. } => None,
        GateDecision::ExperimentalProbeAvailable { refusal, .. }
        | GateDecision::Refused { refusal } => Some(refusal.code().to_owned()),
    }
}

/// Which App. B.8 section a probed device belongs in.
///
/// Pure so every branch is testable: a device that the gate refuses, that has
/// no named protocol profile or that needs an authentication mode this build
/// does not implement is `UnsupportedFirmware`; otherwise it needs a
/// credential, is connected only while a session is authenticated, and is
/// otherwise merely nearby.
#[must_use]
pub fn section_for_probed(
    decision: &GateDecision,
    protocol: Option<&ProtocolProfile>,
    has_credential: bool,
    authenticated: bool,
) -> DeviceSection {
    if matches!(decision, GateDecision::Refused { .. }) {
        return DeviceSection::UnsupportedFirmware;
    }
    let Some(protocol) = protocol else {
        return DeviceSection::UnsupportedFirmware;
    };
    if !protocol.auth_mode_is_implemented() {
        return DeviceSection::UnsupportedFirmware;
    }
    if !has_credential {
        return DeviceSection::NeedsAuthorization;
    }
    if authenticated {
        DeviceSection::Connected
    } else {
        DeviceSection::Nearby
    }
}

/// The label of the media a source yields, from the register row when there is
/// one, otherwise from the named protocol profile.
#[must_use]
pub fn source_label(decision: Option<&GateDecision>, protocol: Option<&ProtocolProfile>) -> String {
    let quality = match decision {
        Some(GateDecision::Permitted { cell, .. }) => Some(cell.quality_class.as_str()),
        Some(GateDecision::ExperimentalProbeAvailable {
            cell: Some(cell), ..
        }) => Some(cell.quality_class.as_str()),
        _ => None,
    };
    let compressed = match quality {
        Some(quality) => quality == QUALITY_COMPRESSED,
        None => protocol.is_some_and(|profile| profile.name == LEGACY_NOTE_V0095.name),
    };
    if compressed {
        SOURCE_LABEL_BLE_COMPRESSED.to_owned()
    } else {
        SOURCE_LABEL_BLE_UNKNOWN.to_owned()
    }
}

/// Estimate seconds remaining from an observed rate.
///
/// `None` when no rate has been observed or the declared length (a device
/// claim) has already been reached: an unknown stays unknown.
#[must_use]
pub fn estimate_eta(declared: u64, received: u64, bytes_per_sec: Option<u64>) -> Option<u64> {
    let rate = bytes_per_sec.filter(|rate| *rate > 0)?;
    let remaining = declared.checked_sub(received).filter(|left| *left > 0)?;
    Some(remaining.div_ceil(rate))
}

/// Observed bandwidth in bytes per second.
#[must_use]
pub fn observed_rate(bytes: u64, elapsed_ms: u64) -> Option<u64> {
    if bytes == 0 || elapsed_ms == 0 {
        return None;
    }
    Some(u64::try_from(u128::from(bytes) * 1000 / u128::from(elapsed_ms)).unwrap_or(u64::MAX))
}

fn state_slug(state: TransferState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn credential_error(err: &CredentialError) -> AppError {
    let code = match err {
        CredentialError::RouteUnavailable { .. } => AppErrorCode::AuthBootstrapRequired,
        CredentialError::AuthModeUnsupported => AppErrorCode::UnsupportedFirmware,
        CredentialError::IdentityMismatch => AppErrorCode::AuthRejected,
        CredentialError::CredentialShape { .. } | CredentialError::Scope(_) => {
            AppErrorCode::InvalidArgument
        }
        CredentialError::Store(_) => match err.code() {
            "SECRET_STORE_LOCKED" => AppErrorCode::SecretStoreLocked,
            "AUTH_BOOTSTRAP_REQUIRED" => AppErrorCode::AuthBootstrapRequired,
            _ => AppErrorCode::InternalError,
        },
    };
    // `CredentialError`'s Display never carries token bytes (it reports a
    // length at most), and the envelope shows only the canonical message.
    AppError::new(code, err.to_string())
}

/// The ledger identifier of a peripheral: a stable, salted digest, so the same
/// device is the same row on every run and a renamed device is not a new one.
fn device_row_id(peripheral_id: &str) -> DeviceId {
    let mut hasher = Sha256::new();
    hasher.update(b"plaudon.device-row.v1");
    hasher.update([0x1f]);
    hasher.update(peripheral_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Uuid::from_bytes(bytes)
        .to_string()
        .parse()
        .expect("a formatted UUID is a canonical device identifier")
}

/// The register spelling of the authentication mode a profile implements.
fn auth_mode_name(profile: &ProtocolProfile) -> &'static str {
    match profile.auth_mode {
        ProfileAuthMode::LegacyStaticToken => "legacy_static_token",
        ProfileAuthMode::EncryptedHandshake => "encrypted_handshake",
    }
}

fn timestamp_string(millis: i64) -> Option<String> {
    Timestamp::from_millis(millis).to_rfc3339()
}

fn unix_millis_now() -> i64 {
    Timestamp::now().as_millis()
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn require_probe_gate_allows_inspection(
    record: &ProbeRecord,
    has_experimental_session: bool,
) -> Result<(), AppError> {
    if record.decision.permits_transfer() || has_experimental_session {
        return Ok(());
    }
    match record.decision {
        GateDecision::Refused { .. } => Err(AppError::new(
            AppErrorCode::UnsupportedFirmware,
            "the gate refused this device profile",
        )),
        _ => Err(AppError::new(
            AppErrorCode::CapabilityUnverified,
            "this profile is not Supported and no experimental probe was approved",
        )),
    }
}

/* ---------------------------------------------------------------- backend */

impl DevicesBackend {
    /// Build the backend from its dependencies.
    #[must_use]
    pub fn new(deps: DevicesDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                gate: GatePolicy::new(deps.register),
                host: deps.host,
                radio: deps.radio,
                vault: deps.vault,
                devices: DeviceRepository::new(deps.db.clone()),
                db: deps.db,
                events: deps.events,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Mark every transfer a previous run left `in_flight` as `interrupted`.
    /// Called once at start-up, before the UI can ask for anything.
    pub async fn recover_interrupted(&self) -> Result<u64, AppError> {
        self.inner.recover_interrupted().await
    }

    /// Record the outcome of a non-destructive audit for a protocol profile.
    ///
    /// The hardware suite calls this; until it has, every profile is
    /// `NotRun` and automatic sync is never advertised.
    pub fn record_audit(&self, profile_name: &str, outcome: AuditOutcome) {
        lock(&self.inner.state)
            .audits
            .insert(profile_name.to_owned(), outcome);
    }
}

impl Inner {
    async fn publish(&self, kind: EventKind, hints: &[(&str, Value)]) {
        let mut draft = EventDraft::new(kind, Timestamp::now());
        for (key, value) in hints {
            draft = draft.with_hint(key, value.clone());
        }
        self.events.publish(draft).await;
    }

    /// Every authorized device, from the ledger: a `devices` row joined with
    /// its `device_credential_refs` row. No token bytes are stored there.
    async fn load_links(&self) -> Result<Vec<LinkedDevice>, AppError> {
        let rows: Vec<LinkRow> = sqlx::query_as(
            "SELECT d.serial_fingerprint, d.model, d.discovered_name, d.observed_firmware, \
                        d.last_seen_at \
                 FROM devices d JOIN device_credential_refs c ON c.device_id = d.id \
                 WHERE d.serial_fingerprint IS NOT NULL ORDER BY d.first_seen_at",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(|err| internal(format!("cannot read device links: {err}")))?;
        let synced = self.synced_devices().await?;
        Ok(rows
            .into_iter()
            .map(
                |(peripheral_id, model, name, firmware, last_seen)| LinkedDevice {
                    sync_enabled: synced.contains(&peripheral_id),
                    binding: DeviceBinding::new(
                        peripheral_id,
                        model,
                        firmware.unwrap_or_default(),
                        ProvisioningRoute::ImportedExistingToken,
                        IdentityAssurance::Unverified,
                    ),
                    advertised_name: name,
                    last_contact_ms: Some(last_seen),
                },
            )
            .collect())
    }

    async fn synced_devices(&self) -> Result<Vec<String>, AppError> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT value_json FROM app_preferences WHERE key = ?1")
                .bind(SYNC_KEY)
                .fetch_optional(self.db.pool())
                .await
                .map_err(|err| internal(format!("cannot read device sync flags: {err}")))?;
        raw.map_or_else(
            || Ok(Vec::new()),
            |value| {
                serde_json::from_str(&value)
                    .map_err(|err| internal(format!("stored device sync flags are invalid: {err}")))
            },
        )
    }

    async fn set_synced(&self, peripheral_id: &str, enabled: bool) -> Result<(), AppError> {
        let mut synced = self.synced_devices().await?;
        synced.retain(|id| id != peripheral_id);
        if enabled {
            synced.push(peripheral_id.to_owned());
        }
        let encoded = serde_json::to_string(&synced)
            .map_err(|err| internal(format!("cannot serialize device sync flags: {err}")))?;
        sqlx::query(
            "INSERT INTO app_preferences (key, version, value_json) VALUES (?1, 1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value_json = ?2",
        )
        .bind(SYNC_KEY)
        .bind(encoded)
        .execute(self.db.pool())
        .await
        .map_err(|err| internal(format!("cannot write device sync flags: {err}")))?;
        Ok(())
    }

    /// Upsert the ledger row of a device. The peripheral identifier is kept in
    /// `serial_fingerprint`: it is the host-stable fingerprint of this device.
    async fn upsert_device_row(
        &self,
        peripheral_id: &str,
        model: &str,
        firmware: Option<&str>,
        name: Option<&str>,
        trust: TrustState,
    ) -> Result<DeviceId, AppError> {
        let id = device_row_id(peripheral_id);
        self.devices
            .upsert_device(
                &DeviceRecord {
                    id,
                    vendor: "PLAUD".to_owned(),
                    model: model.to_owned(),
                    serial_fingerprint: Some(peripheral_id.to_owned()),
                    discovered_name: name.map(str::to_owned),
                    observed_firmware: firmware.map(str::to_owned),
                    trust_state: trust,
                },
                Timestamp::now(),
            )
            .await
            .map_err(|err| internal(format!("cannot record the device: {err}")))?;
        Ok(id)
    }

    /// A restart must never see a transfer that claims to be in flight.
    async fn recover_interrupted(&self) -> Result<u64, AppError> {
        let result = sqlx::query(
            "UPDATE device_transfers SET transfer_status = 'interrupted', ended_at = ?1 \
             WHERE transfer_status = 'in_flight'",
        )
        .bind(Timestamp::now().as_millis())
        .execute(self.db.pool())
        .await
        .map_err(|err| internal(format!("cannot recover device transfers: {err}")))?;
        Ok(result.rows_affected())
    }

    fn identity_of(state: &State, peripheral_id: &str) -> IdentityAssurance {
        if state.authenticated.contains(peripheral_id) {
            IdentityAssurance::ProfileAuthenticated
        } else {
            IdentityAssurance::Unverified
        }
    }

    fn sync_dto(
        state: &State,
        peripheral_id: &str,
        link: Option<&LinkedDevice>,
    ) -> DeviceSyncPolicyDto {
        let record = state.probes.get(peripheral_id);
        let permits = record.is_some_and(|record| record.decision.permits_transfer());
        let profile_name = record
            .and_then(|record| record.protocol)
            .map_or("unknown", |profile| profile.name);
        let outcome = state
            .audits
            .get(profile_name)
            .cloned()
            .unwrap_or(AuditOutcome::NotRun);
        let eligibility = SyncEligibility::from_audit(profile_name, outcome, permits);
        let may_advertise = eligibility.may_offer_automatic_sync();
        let explanation = if permits {
            eligibility.outcome().ui_explanation().to_owned()
        } else {
            "Automatic transfer when nearby is unavailable: this device profile is not Supported \
             in the compatibility register."
                .to_owned()
        };
        DeviceSyncPolicyDto {
            peripheral_id: peripheral_id.to_owned(),
            may_advertise,
            enabled: may_advertise && link.is_some_and(|link| link.sync_enabled),
            default_on: eligibility.automatic_sync_default_on(),
            audit_code: eligibility.outcome().code().to_owned(),
            explanation,
        }
    }

    fn candidate_dto(
        state: &State,
        peripheral_id: &str,
        link: Option<&LinkedDevice>,
    ) -> DeviceCandidateDto {
        let seen = state.nearby.get(peripheral_id);
        let record = state.probes.get(peripheral_id);
        let authenticated = state.authenticated.contains(peripheral_id);
        let authorized = link.is_some();
        let section = if seen.is_none() {
            DeviceSection::PreviouslyLinked
        } else {
            record.map_or(DeviceSection::Nearby, |record| {
                section_for_probed(&record.decision, record.protocol, authorized, authenticated)
            })
        };
        let status = record.map_or(SupportStatus::NotEvaluated, |record| {
            decision_status(&record.decision)
        });
        let identity = Self::identity_of(state, peripheral_id);
        DeviceCandidateDto {
            peripheral_id: peripheral_id.to_owned(),
            section,
            advertised_name: seen
                .and_then(|seen| seen.advertised_name.clone())
                .or_else(|| link.and_then(|link| link.advertised_name.clone())),
            rssi_dbm: seen.and_then(|seen| seen.rssi_dbm),
            transport: TRANSPORT_BLE.to_owned(),
            model: record
                .and_then(|record| record.radio.model.clone())
                .or_else(|| link.map(|link| link.binding.device_model.clone())),
            firmware: record
                .and_then(|record| record.radio.firmware.clone())
                .or_else(|| link.map(|link| link.binding.firmware_fingerprint.clone())),
            support_status: status,
            status_label: status.ui_label().to_owned(),
            identity,
            identity_label: identity.ui_label().to_owned(),
            authorized,
            last_contact_at: link
                .and_then(|link| link.last_contact_ms)
                .and_then(timestamp_string),
        }
    }

    fn capabilities(
        record: &ProbeRecord,
        has_experimental: bool,
        sync: &DeviceSyncPolicyDto,
    ) -> Vec<DeviceCapabilityDto> {
        let status = decision_status(&record.decision);
        let not_supported = format!(
            "Compatibility status is {}; only a Supported profile may transfer.",
            status.ui_label()
        );
        let permits = record.decision.permits_transfer();
        let listing_ok = permits || has_experimental;
        vec![
            DeviceCapabilityDto {
                capability: DeviceCapability::ReadOnlyListing,
                available: listing_ok,
                reason: (!listing_ok).then(|| {
                    format!(
                        "Compatibility status is {}; a read-only experimental probe must be \
                         approved first.",
                        status.ui_label()
                    )
                }),
            },
            DeviceCapabilityDto {
                capability: DeviceCapability::ReadOnlyTransfer,
                available: permits,
                reason: (!permits).then_some(not_supported),
            },
            DeviceCapabilityDto {
                capability: DeviceCapability::AutomaticSync,
                available: sync.may_advertise,
                reason: (!sync.may_advertise).then(|| sync.explanation.clone()),
            },
        ]
    }

    fn probe_dto(
        state: &State,
        peripheral_id: &str,
        link: Option<&LinkedDevice>,
    ) -> Result<DeviceProbeDto, AppError> {
        let record = state
            .probes
            .get(peripheral_id)
            .ok_or_else(|| invalid_argument("the device has not been probed"))?;
        let candidate = Self::candidate_dto(state, peripheral_id, link);
        let has_experimental = state.experimental.contains_key(peripheral_id);
        let sync = Self::sync_dto(state, peripheral_id, link);
        let legacy = record
            .protocol
            .is_some_and(|profile| profile.name == LEGACY_NOTE_V0095.name)
            || record
                .radio
                .auth_mode
                .as_deref()
                .is_some_and(|mode| mode.eq_ignore_ascii_case("legacy_static_token"));
        let provisioning_note = record
            .protocol
            .filter(|profile| profile.auth_mode_is_implemented())
            .map(|_| {
                ProvisioningRoute::ImportedExistingToken
                    .explanation()
                    .to_owned()
            });
        Ok(DeviceProbeDto {
            peripheral_id: peripheral_id.to_owned(),
            advertised_name: candidate.advertised_name,
            transport: TRANSPORT_BLE.to_owned(),
            model: record.radio.model.clone(),
            firmware: record.radio.firmware.clone(),
            auth_mode: record.radio.auth_mode.clone(),
            battery_percent: record.radio.battery_percent,
            rssi_dbm: record.radio.rssi_dbm.or(candidate.rssi_dbm),
            section: candidate.section,
            support_status: candidate.support_status,
            status_label: candidate.status_label,
            known_limitations: record.decision.known_limitations(),
            gate: DeviceGateDto {
                permits_transfer: record.decision.permits_transfer(),
                experimental_probe_available: record.decision.offers_experimental_probe(),
                experimental_probe_approved: has_experimental,
                refusal_code: refusal_code(&record.decision),
            },
            identity: candidate.identity,
            identity_label: candidate.identity_label,
            capabilities: Self::capabilities(record, has_experimental, &sync),
            source_label: source_label(Some(&record.decision), record.protocol),
            cloud_route: CloudRoute::NotUsed,
            disclosures: if legacy {
                LEGACY_DISCLOSURES
                    .iter()
                    .map(|text| (*text).to_owned())
                    .collect()
            } else {
                Vec::new()
            },
            provisioning_note,
            authorized: candidate.authorized,
            last_contact_at: candidate.last_contact_at,
            sync,
        })
    }

    fn transfer_dto(record: Option<&TransferRecord>, label: &str) -> DeviceTransferDto {
        let (state, verified) = record.map_or((TransferState::Discovered, false), |record| {
            (record.ledger.state(), record.ledger.verified())
        });
        DeviceTransferDto {
            transfer_id: record.map(|record| record.transfer_id.clone()),
            state,
            state_label: state.ui_label().to_owned(),
            bytes_received: record.and_then(|record| record.bytes_received),
            observed_bytes_per_sec: record.and_then(|record| record.observed_bytes_per_sec),
            eta_seconds_estimate: record.and_then(|record| record.eta_seconds_estimate),
            verified,
            source_label: label.to_owned(),
            cloud_route: CloudRoute::NotUsed,
        }
    }

    fn require_link(links: &[LinkedDevice], peripheral_id: &str) -> Result<LinkedDevice, AppError> {
        links
            .iter()
            .find(|link| link.binding.peripheral_id == peripheral_id)
            .cloned()
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::AuthBootstrapRequired,
                    "no credential is stored for this device",
                )
            })
    }

    /// Apply one ledger event and refresh the derived progress numbers.
    ///
    /// Returns the new state when the event was legal.
    fn apply_transfer_event(
        state: &mut State,
        peripheral_id: &str,
        update: &TransferUpdate,
    ) -> Option<(TransferState, String, bool)> {
        let record = state
            .transfers
            .get_mut(&(peripheral_id.to_owned(), update.recording_id.clone()))?;
        let next = record.ledger.apply(&update.event).ok()?;
        if let Some(bytes) = update.bytes_received {
            record.bytes_received = Some(bytes);
            record.observed_bytes_per_sec = update
                .bytes_per_second
                .or_else(|| {
                    update
                        .elapsed_ms
                        .and_then(|elapsed| observed_rate(bytes, elapsed))
                })
                .or(record.observed_bytes_per_sec);
            record.eta_seconds_estimate = update.eta_seconds.or_else(|| {
                estimate_eta(
                    record.declared_byte_length,
                    bytes,
                    record.observed_bytes_per_sec,
                )
            });
        }
        if next.is_terminal()
            || matches!(next, TransferState::Verifying | TransferState::Committing)
        {
            record.eta_seconds_estimate = None;
        }
        Some((
            next,
            record.transfer_id.clone(),
            matches!(update.event, TransferEvent::VerificationSucceeded),
        ))
    }

    async fn report_transfer(&self, peripheral_id: &str, update: TransferUpdate) {
        let outcome = {
            let mut state = lock(&self.state);
            Self::apply_transfer_event(&mut state, peripheral_id, &update)
        };
        let Some((next, transfer_id, verified_now)) = outcome else {
            tracing::debug!(
                recording = update.recording_id.as_str(),
                "ignored an illegal or unknown transfer step"
            );
            return;
        };
        let hints = [
            ("peripheral_id", Value::from(peripheral_id)),
            ("recording_id", Value::from(update.recording_id.as_str())),
            ("transfer_id", Value::from(transfer_id.as_str())),
            ("state", Value::from(state_slug(next))),
        ];
        self.publish(EventKind::DeviceTransferProgress, &hints)
            .await;
        if verified_now {
            self.publish(EventKind::DeviceTransferVerified, &hints)
                .await;
        }
    }

    async fn run_transfer(
        self: Arc<Self>,
        link: LinkedDevice,
        transfer_id: TransferId,
        recording: ListedRecording,
        local_only: bool,
    ) {
        let peripheral_id = link.binding.peripheral_id.clone();
        let recording_id = recording.id.clone();
        let reporter = LedgerReporter {
            inner: Arc::clone(&self),
            peripheral_id: peripheral_id.clone(),
        };
        let token = DeviceCredentials::new(&*self.vault).load(
            &link.binding,
            &peripheral_id,
            &link.binding.device_model,
            &link.binding.firmware_fingerprint,
        );
        let outcome = match token {
            Ok(token) => {
                self.radio
                    .transfer(
                        TransferRequest {
                            peripheral_id: &peripheral_id,
                            transfer_id,
                            recording: &recording,
                            token: &token,
                            local_only,
                        },
                        &reporter,
                    )
                    .await
            }
            Err(err) => Err(credential_error(&err)),
        };
        if let Err(err) = outcome {
            tracing::warn!(
                code = err.code().as_str(),
                "a device transfer did not finish"
            );
            // Idempotent with the committer's own row update: a transfer that
            // did not commit is `interrupted`, never `in_flight`.
            let _ = sqlx::query(
                "UPDATE device_transfers SET transfer_status = 'interrupted', ended_at = ?2 \
                 WHERE id = ?1 AND transfer_status = 'in_flight'",
            )
            .bind(transfer_id.to_string())
            .bind(Timestamp::now().as_millis())
            .execute(self.db.pool())
            .await;
            reporter
                .report(TransferUpdate::step(
                    recording_id,
                    TransferEvent::Disconnected,
                ))
                .await;
        }
    }
}

struct LedgerReporter {
    inner: Arc<Inner>,
    peripheral_id: String,
}

#[async_trait]
impl TransferReporter for LedgerReporter {
    async fn report(&self, update: TransferUpdate) {
        self.inner
            .report_transfer(&self.peripheral_id, update)
            .await;
    }
}

#[async_trait]
impl DevicesPort for DevicesBackend {
    async fn list_nearby(&self, timeout_ms: u64) -> Result<DeviceScanDto, AppError> {
        let inner = &self.inner;
        let links = inner.load_links().await?;

        let (radio, adverts) = match inner.radio.availability() {
            RadioAvailability::NotInThisBuild => (
                RadioStatusDto {
                    state: RadioState::UnavailableInBuild,
                    code: Some(AppErrorCode::SourceUnavailable.as_str().to_owned()),
                    message: RADIO_UNAVAILABLE_MESSAGE.to_owned(),
                },
                Vec::new(),
            ),
            RadioAvailability::Present => {
                match inner.radio.scan(Duration::from_millis(timeout_ms)).await {
                    Ok(adverts) => (
                        RadioStatusDto {
                            state: RadioState::Scanned,
                            code: None,
                            message: RADIO_SCANNED_MESSAGE.to_owned(),
                        },
                        adverts,
                    ),
                    // Every radio refusal is a state with its own cause, not
                    // an error and never an empty room (#301).
                    Err(err) => match radio_state_for(err.code()) {
                        Some(state) => (
                            RadioStatusDto {
                                state,
                                code: Some(err.code().as_str().to_owned()),
                                message: radio_refusal_message(state).to_owned(),
                            },
                            Vec::new(),
                        ),
                        None => return Err(err),
                    },
                }
            }
        };
        let scanned = radio.state == RadioState::Scanned;

        let policy = DiscoveryPolicy::default();
        let mut nearby: HashMap<String, SeenDevice> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for advertisement in &adverts {
            if let AdvertisementVerdict::Candidate(candidate) = policy.evaluate(advertisement) {
                if !nearby.contains_key(&candidate.peripheral_id) {
                    order.push(candidate.peripheral_id.clone());
                }
                nearby.insert(
                    candidate.peripheral_id,
                    SeenDevice {
                        advertised_name: candidate.advertised_name,
                        rssi_dbm: advertisement.rssi,
                    },
                );
            }
        }

        let (devices, newly_seen, dropped) = {
            let mut state = lock(&inner.state);
            let newly_seen: Vec<String> = order
                .iter()
                .filter(|id| !state.nearby.contains_key(*id))
                .cloned()
                .collect();
            let mut dropped = Vec::new();
            if scanned {
                dropped = state
                    .authenticated
                    .iter()
                    .filter(|id| !nearby.contains_key(*id))
                    .cloned()
                    .collect();
                for id in &dropped {
                    state.authenticated.remove(id);
                }
            }
            // Without a scan nothing is known to be nearby, so nothing is kept
            // from an earlier scan.
            state.nearby = nearby;
            let mut devices: Vec<DeviceCandidateDto> = Vec::new();
            let mut listed: HashSet<&str> = HashSet::new();
            for id in &order {
                let link = links.iter().find(|link| &link.binding.peripheral_id == id);
                devices.push(Inner::candidate_dto(&state, id, link));
                listed.insert(id.as_str());
            }
            for link in &links {
                if !listed.contains(link.binding.peripheral_id.as_str()) {
                    devices.push(Inner::candidate_dto(
                        &state,
                        &link.binding.peripheral_id,
                        Some(link),
                    ));
                }
            }
            (devices, newly_seen, dropped)
        };

        for id in newly_seen {
            inner
                .publish(
                    EventKind::DeviceDiscovered,
                    &[("peripheral_id", Value::from(id))],
                )
                .await;
        }
        for id in dropped {
            inner
                .publish(
                    EventKind::DeviceDisconnected,
                    &[("peripheral_id", Value::from(id))],
                )
                .await;
        }

        Ok(DeviceScanDto {
            radio,
            devices,
            scanned_at: scanned
                .then(|| timestamp_string(unix_millis_now()))
                .flatten(),
        })
    }

    async fn probe(
        &self,
        peripheral_id: &str,
        experimental_probe_approved: bool,
    ) -> Result<DeviceProbeDto, AppError> {
        let inner = &self.inner;
        let links = inner.load_links().await?;
        let known = {
            let state = lock(&inner.state);
            state.nearby.contains_key(peripheral_id)
        };
        if !known {
            return Err(invalid_argument(
                "the device was not seen in the latest scan; scan again first",
            ));
        }

        let observed = inner.radio.probe(peripheral_id).await?;
        let probed = ProbedProfile {
            source_type: SOURCE_TYPE_BLE.to_owned(),
            device_model: observed.model.clone().unwrap_or_default(),
            firmware: observed.firmware.clone().unwrap_or_default(),
            host_os: inner.host.host_os.clone(),
            os_version: inner.host.os_version.clone(),
            transport: TRANSPORT_BLE.to_owned(),
            auth_mode: observed.auth_mode.clone().unwrap_or_default(),
        };
        let decision = inner.gate.evaluate(&probed);
        let protocol = profile_for(&probed.device_model, &probed.firmware);

        let link = links
            .iter()
            .find(|link| link.binding.peripheral_id == peripheral_id)
            .cloned();
        // A link whose vault entry has vanished is not an authorization.
        let link = match link {
            Some(link) => {
                let present = DeviceCredentials::new(&*inner.vault)
                    .status(&link.binding)
                    .map_err(|err| credential_error(&err))?
                    .present;
                present.then_some(link)
            }
            None => None,
        };

        // Ledger: the device row and its firmware history. A changed firmware
        // forces revalidation of the cell (spec §13.3).
        let mut firmware_changed = false;
        if let (Some(model), Some(firmware)) = (&observed.model, &observed.firmware) {
            let name = lock(&inner.state)
                .nearby
                .get(peripheral_id)
                .and_then(|seen| seen.advertised_name.clone());
            let id = inner
                .upsert_device_row(
                    peripheral_id,
                    model,
                    Some(firmware),
                    name.as_deref(),
                    TrustState::Unverified,
                )
                .await?;
            firmware_changed = inner
                .devices
                .record_firmware_observation(
                    id,
                    firmware,
                    "ble_probe",
                    protocol.map_or("none", |profile| profile.name),
                    observed.auth_mode.as_deref().unwrap_or("unknown"),
                    Timestamp::now(),
                )
                .await
                .map_err(|err| internal(format!("cannot record the firmware: {err}")))?;
        }

        let (changed, dto) = {
            let mut state = lock(&inner.state);
            let changed = firmware_changed
                || state.probes.get(peripheral_id).is_none_or(|previous| {
                    previous.decision != decision || previous.radio.firmware != observed.firmware
                });
            state.experimental.remove(peripheral_id);
            if experimental_probe_approved {
                let grant = ExperimentalProbeGrant::new(peripheral_id, unix_millis_now() / 1000);
                if let Ok(session) = grant.consume(peripheral_id, &decision) {
                    state.experimental.insert(peripheral_id.to_owned(), session);
                }
            }
            state.probes.insert(
                peripheral_id.to_owned(),
                ProbeRecord {
                    probed,
                    decision,
                    protocol,
                    radio: observed,
                },
            );
            (
                changed,
                Inner::probe_dto(&state, peripheral_id, link.as_ref())?,
            )
        };

        if changed {
            inner
                .publish(
                    EventKind::DeviceCompatibilityChanged,
                    &[
                        ("peripheral_id", Value::from(peripheral_id)),
                        (
                            "support_status",
                            serde_json::to_value(dto.support_status).unwrap_or(Value::Null),
                        ),
                    ],
                )
                .await;
        }
        Ok(dto)
    }

    async fn authorize(
        &self,
        peripheral_id: &str,
        token_hex: SecretValue,
    ) -> Result<DeviceAuthStatusDto, AppError> {
        let inner = &self.inner;
        let (model, firmware, protocol, advertised_name) = {
            let state = lock(&inner.state);
            let record = state
                .probes
                .get(peripheral_id)
                .ok_or_else(|| invalid_argument("the device has not been probed"))?;
            if matches!(record.decision, GateDecision::Refused { .. }) {
                return Err(AppError::new(
                    AppErrorCode::UnsupportedFirmware,
                    "the gate refused this device profile",
                ));
            }
            let protocol = record.protocol.ok_or_else(|| {
                AppError::new(
                    AppErrorCode::UnsupportedFirmware,
                    "no named protocol profile covers this device",
                )
            })?;
            (
                record.probed.device_model.clone(),
                record.probed.firmware.clone(),
                protocol,
                state
                    .nearby
                    .get(peripheral_id)
                    .and_then(|seen| seen.advertised_name.clone()),
            )
        };

        let binding = DeviceBinding::new(
            peripheral_id,
            model,
            firmware,
            ProvisioningRoute::ImportedExistingToken,
            IdentityAssurance::Unverified,
        );
        let credentials = DeviceCredentials::new(&*inner.vault);
        credentials
            .authorize(&binding, protocol, token_hex.expose())
            .map_err(|err| credential_error(&err))?;

        let links = inner.load_links().await?;
        if let Some(existing) = links
            .iter()
            .find(|link| link.binding.peripheral_id == peripheral_id)
        {
            if existing.binding != binding {
                // A changed firmware or model forces a fresh credential; the
                // stale vault entry must not outlive its binding.
                let _ = credentials.revoke(&existing.binding);
            }
        }
        let scope = binding
            .scope()
            .map_err(|err| credential_error(&CredentialError::Scope(err)))?;
        let id = inner
            .upsert_device_row(
                peripheral_id,
                &binding.device_model,
                Some(&binding.firmware_fingerprint),
                advertised_name.as_deref(),
                TrustState::Unverified,
            )
            .await?;
        // Only the vault coordinates are stored, never the token.
        inner
            .devices
            .record_credential_ref(
                id,
                scope.service(),
                &scope.account(),
                auth_mode_name(protocol),
                Timestamp::now(),
            )
            .await
            .map_err(|err| internal(format!("cannot record the credential reference: {err}")))?;
        lock(&inner.state).authenticated.remove(peripheral_id);

        Ok(DeviceAuthStatusDto {
            peripheral_id: peripheral_id.to_owned(),
            credential_stored: true,
            identity: IdentityAssurance::Unverified,
            identity_label: IdentityAssurance::Unverified.ui_label().to_owned(),
        })
    }

    async fn revoke(&self, peripheral_id: &str) -> Result<DeviceAuthStatusDto, AppError> {
        let inner = &self.inner;
        let links = inner.load_links().await?;
        if let Some(link) = links
            .iter()
            .find(|link| link.binding.peripheral_id == peripheral_id)
        {
            match DeviceCredentials::new(&*inner.vault).revoke(&link.binding) {
                Ok(())
                | Err(CredentialError::Store(
                    plaudon_security::secrets::SecretStoreError::NotFound,
                )) => {}
                Err(err) => return Err(credential_error(&err)),
            }
        }
        inner
            .devices
            .remove_credential_ref(device_row_id(peripheral_id))
            .await
            .map_err(|err| internal(format!("cannot remove the credential reference: {err}")))?;
        inner.set_synced(peripheral_id, false).await?;
        lock(&inner.state).authenticated.remove(peripheral_id);
        Ok(DeviceAuthStatusDto {
            peripheral_id: peripheral_id.to_owned(),
            credential_stored: false,
            identity: IdentityAssurance::Unverified,
            identity_label: IdentityAssurance::Unverified.ui_label().to_owned(),
        })
    }

    async fn list_recordings(&self, peripheral_id: &str) -> Result<DeviceListingDto, AppError> {
        let inner = &self.inner;
        let links = inner.load_links().await?;
        {
            let state = lock(&inner.state);
            let record = state
                .probes
                .get(peripheral_id)
                .ok_or_else(|| invalid_argument("the device has not been probed"))?;
            require_probe_gate_allows_inspection(
                record,
                state.experimental.contains_key(peripheral_id),
            )?;
        }
        let link = Inner::require_link(&links, peripheral_id)?;
        let (model, firmware) = {
            let state = lock(&inner.state);
            let record = state
                .probes
                .get(peripheral_id)
                .ok_or_else(|| invalid_argument("the device has not been probed"))?;
            (
                record.probed.device_model.clone(),
                record.probed.firmware.clone(),
            )
        };
        let token = DeviceCredentials::new(&*inner.vault)
            .load(&link.binding, peripheral_id, &model, &firmware)
            .map_err(|err| credential_error(&err))?;

        let listing = match inner.radio.list(peripheral_id, &token).await {
            Ok(listing) => listing,
            Err(err) => {
                let was_connected = lock(&inner.state).authenticated.remove(peripheral_id);
                if was_connected {
                    inner
                        .publish(
                            EventKind::DeviceDisconnected,
                            &[("peripheral_id", Value::from(peripheral_id))],
                        )
                        .await;
                }
                return Err(err);
            }
        };
        drop(token);

        let newly_connected = lock(&inner.state)
            .authenticated
            .insert(peripheral_id.to_owned());
        inner
            .upsert_device_row(
                peripheral_id,
                &link.binding.device_model,
                Some(&link.binding.firmware_fingerprint),
                link.advertised_name.as_deref(),
                TrustState::ProfileAuthenticated,
            )
            .await?;

        let dto = {
            let mut state = lock(&inner.state);
            let protocol = state
                .probes
                .get(peripheral_id)
                .and_then(|record| record.protocol);
            let label = source_label(
                state
                    .probes
                    .get(peripheral_id)
                    .map(|record| &record.decision),
                protocol,
            );
            let completeness = match (protocol.map(|p| p.listing_is_delta_only), listing.truncated)
            {
                (Some(true), true) => ListingCompleteness::UnsyncedDeltaTruncated,
                (Some(true), false) => ListingCompleteness::UnsyncedDelta,
                _ => ListingCompleteness::Unknown,
            };
            let completeness_label = match completeness {
                ListingCompleteness::UnsyncedDelta => COMPLETENESS_DELTA,
                ListingCompleteness::UnsyncedDeltaTruncated => COMPLETENESS_DELTA_TRUNCATED,
                ListingCompleteness::Unknown => COMPLETENESS_UNKNOWN,
            };
            let file_count = u32::try_from(listing.entries.len()).unwrap_or(u32::MAX);

            let mut items: Vec<DeviceRecordingDto> = Vec::new();
            let mut listed_ids: HashSet<String> = HashSet::new();
            for entry in &listing.entries {
                listed_ids.insert(entry.id.clone());
                let key = (peripheral_id.to_owned(), entry.id.clone());
                items.push(DeviceRecordingDto {
                    recording_id: entry.id.clone(),
                    display_name: entry.display_name.clone(),
                    declared_byte_length: entry.declared_byte_length,
                    captured_at: timestamp_string(i64::from(entry.captured_at_unix) * 1000),
                    transfer: Inner::transfer_dto(state.transfers.get(&key), &label),
                });
            }
            // A recording already moved off the device may no longer be listed;
            // it keeps its Transferred locally / Archived state in this view.
            let mut extra: Vec<(&String, &TransferRecord)> = state
                .transfers
                .iter()
                .filter(|((device, id), _)| device == peripheral_id && !listed_ids.contains(id))
                .map(|((_, id), record)| (id, record))
                .collect();
            extra.sort_by(|a, b| a.1.transfer_id.cmp(&b.1.transfer_id));
            for (recording_id, record) in extra {
                items.push(DeviceRecordingDto {
                    recording_id: recording_id.clone(),
                    display_name: record.display_name.clone(),
                    declared_byte_length: record.declared_byte_length,
                    captured_at: timestamp_string(i64::from(record.captured_at_unix) * 1000),
                    transfer: Inner::transfer_dto(Some(record), &label),
                });
            }

            state.listings.insert(
                peripheral_id.to_owned(),
                StoredListing {
                    entries: listing.entries,
                },
            );
            DeviceListingDto {
                peripheral_id: peripheral_id.to_owned(),
                items,
                file_count,
                completeness,
                completeness_label: completeness_label.to_owned(),
                identity: IdentityAssurance::ProfileAuthenticated,
                identity_label: IdentityAssurance::ProfileAuthenticated
                    .ui_label()
                    .to_owned(),
            }
        };

        if newly_connected {
            inner
                .publish(
                    EventKind::DeviceConnected,
                    &[("peripheral_id", Value::from(peripheral_id))],
                )
                .await;
        }
        Ok(dto)
    }

    async fn import(
        &self,
        peripheral_id: &str,
        recording_ids: Vec<String>,
        local_only: bool,
    ) -> Result<Vec<String>, AppError> {
        let inner = &self.inner;
        let links = inner.load_links().await?;
        let link = Inner::require_link(&links, peripheral_id)?;

        let accepted: Vec<(TransferId, u64, ListedRecording)> = {
            let mut state = lock(&inner.state);
            let record = state
                .probes
                .get(peripheral_id)
                .ok_or_else(|| invalid_argument("the device has not been probed"))?;
            // Fail closed: only a `Supported` register cell transfers. An
            // experimental probe may list, never transfer.
            match &record.decision {
                GateDecision::Permitted { .. } => {}
                GateDecision::Refused { .. } => {
                    return Err(AppError::new(
                        AppErrorCode::UnsupportedFirmware,
                        "the gate refused this device profile",
                    ));
                }
                GateDecision::ExperimentalProbeAvailable { .. } => {
                    return Err(AppError::new(
                        AppErrorCode::CapabilityUnverified,
                        "transfer is not permitted for a profile that is not Supported",
                    ));
                }
            }
            if !state.authenticated.contains(peripheral_id) {
                return Err(invalid_argument(
                    "list the device's recordings before importing",
                ));
            }
            let listing = state
                .listings
                .get(peripheral_id)
                .ok_or_else(|| invalid_argument("list the device's recordings before importing"))?;

            let mut accepted = Vec::with_capacity(recording_ids.len());
            for id in &recording_ids {
                let entry = listing
                    .entries
                    .iter()
                    .find(|entry| &entry.id == id)
                    .cloned()
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCode::NotPresentOnDevice,
                            "the recording is not in the device's current listing",
                        )
                    })?;
                if let Some(existing) = state.transfers.get(&(peripheral_id.to_owned(), id.clone()))
                {
                    let current = existing.ledger.state();
                    if current == TransferState::Imported {
                        return Err(AppError::new(
                            AppErrorCode::Conflict,
                            "the recording is already archived",
                        ));
                    }
                    if !current.is_terminal() && current != TransferState::Discovered {
                        return Err(AppError::new(
                            AppErrorCode::DeviceBusy,
                            "a transfer of this recording is already running",
                        ));
                    }
                }
                accepted.push((TransferId::new(), entry.declared_byte_length, entry));
            }

            for (transfer_id, declared, entry) in &accepted {
                let transfer_id = transfer_id.to_string();
                let mut ledger = TransferLedger::new();
                // `Discovered -> Queued` is always legal from a fresh ledger.
                let _ = ledger.apply(&TransferEvent::Queued);
                state.transfers.insert(
                    (peripheral_id.to_owned(), entry.id.clone()),
                    TransferRecord {
                        transfer_id,
                        ledger,
                        display_name: entry.display_name.clone(),
                        declared_byte_length: *declared,
                        captured_at_unix: entry.captured_at_unix,
                        bytes_received: None,
                        observed_bytes_per_sec: None,
                        eta_seconds_estimate: None,
                        handle: None,
                    },
                );
            }
            accepted
        };

        let device_id = device_row_id(peripheral_id);
        let mut transfer_ids = Vec::with_capacity(accepted.len());
        for (transfer_id, declared, entry) in accepted {
            let transfer_text = transfer_id.to_string();
            inner
                .devices
                .start_transfer(
                    &DeviceTransferRow {
                        id: transfer_id,
                        device_id,
                        transport: TRANSPORT_BLE.to_owned(),
                        native_format: Some("asr_opus".to_owned()),
                        expected_bytes: Some(declared),
                        received_bytes: 0,
                        sha256: None,
                        status: TransferStatus::InFlight,
                        verification: RowVerification::Unverified,
                    },
                    Timestamp::now(),
                )
                .await
                .map_err(|err| internal(format!("cannot start the transfer record: {err}")))?;
            inner
                .publish(
                    EventKind::DeviceTransferProgress,
                    &[
                        ("peripheral_id", Value::from(peripheral_id)),
                        ("recording_id", Value::from(entry.id.as_str())),
                        ("transfer_id", Value::from(transfer_text.as_str())),
                        ("state", Value::from(state_slug(TransferState::Queued))),
                    ],
                )
                .await;
            let key = (peripheral_id.to_owned(), entry.id.clone());
            let handle = tokio::spawn(Arc::clone(inner).run_transfer(
                link.clone(),
                transfer_id,
                entry,
                local_only,
            ));
            if let Some(record) = lock(&inner.state).transfers.get_mut(&key) {
                record.handle = Some(handle);
            }
            transfer_ids.push(transfer_text);
        }
        Ok(transfer_ids)
    }

    async fn disconnect(&self, peripheral_id: &str) -> Result<(), AppError> {
        let inner = &self.inner;
        let cancelled: Vec<(String, String)> = {
            let mut state = lock(&inner.state);
            let mut cancelled = Vec::new();
            for ((device, recording), record) in &mut state.transfers {
                if device != peripheral_id {
                    continue;
                }
                let current = record.ledger.state();
                if current.is_terminal() || current == TransferState::Discovered {
                    continue;
                }
                if let Some(handle) = record.handle.take() {
                    handle.abort();
                }
                if record.ledger.apply(&TransferEvent::Cancelled).is_ok() {
                    record.eta_seconds_estimate = None;
                    cancelled.push((recording.clone(), record.transfer_id.clone()));
                }
            }
            cancelled
        };
        for (recording_id, transfer_id) in cancelled {
            if let Ok(id) = transfer_id.parse::<TransferId>() {
                let _ = inner
                    .devices
                    .finish_transfer(
                        id,
                        0,
                        None,
                        TransferStatus::Cancelled,
                        RowVerification::Unverified,
                        Timestamp::now(),
                    )
                    .await;
            }
            inner
                .publish(
                    EventKind::DeviceTransferProgress,
                    &[
                        ("peripheral_id", Value::from(peripheral_id)),
                        ("recording_id", Value::from(recording_id)),
                        ("transfer_id", Value::from(transfer_id)),
                        ("state", Value::from(state_slug(TransferState::Cancelled))),
                    ],
                )
                .await;
        }
        inner.radio.disconnect(peripheral_id).await;
        let was_connected = lock(&inner.state).authenticated.remove(peripheral_id);
        if was_connected {
            inner
                .publish(
                    EventKind::DeviceDisconnected,
                    &[("peripheral_id", Value::from(peripheral_id))],
                )
                .await;
        }
        Ok(())
    }

    async fn sync_policy(
        &self,
        peripheral_id: &str,
        enable_when_nearby: bool,
    ) -> Result<DeviceSyncPolicyDto, AppError> {
        let inner = &self.inner;
        let links = inner.load_links().await?;
        let link = Inner::require_link(&links, peripheral_id)?;
        let mut dto = {
            let state = lock(&inner.state);
            if !state.probes.contains_key(peripheral_id) {
                return Err(invalid_argument("the device has not been probed"));
            }
            Inner::sync_dto(&state, peripheral_id, Some(&link))
        };
        if enable_when_nearby && !dto.may_advertise {
            return Err(AppError::new(
                AppErrorCode::CapabilityUnverified,
                "automatic sync is not available for this device profile",
            ));
        }
        inner.set_synced(peripheral_id, enable_when_nearby).await?;
        dto.enabled = enable_when_nearby && dto.may_advertise;
        Ok(dto)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use plaudon_plaud_protocol::gatt::SERVICE_VENDOR;
    use plaudon_plaud_protocol::payload::DeviceCodec;
    use plaudon_security::secrets::InMemorySecretStore;
    use plaudon_source_plaud_device::GateRefusal;

    use super::*;
    use crate::backend::store::Store;
    use crate::events::RecordingEventSink;

    const COMMITTED: &str = COMPATIBILITY_YAML;
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
    known_limitations: ["fixture register used by a unit test"]
"#;
    const TOKEN: &str = "00112233445566778899aabbccddeeff";
    const DEVICE: &str = "AABBCCDD-0011-2233-4455-667788990011";

    #[derive(Debug, Default)]
    struct FakeRadio {
        adverts: Mutex<Vec<ObservedAdvertisement>>,
        probe: Mutex<RadioProbe>,
        listing: Mutex<Option<DeltaListing>>,
        scan_error: Mutex<Option<AppErrorCode>>,
        list_error: Mutex<Option<AppErrorCode>>,
        script: Mutex<Vec<TransferUpdate>>,
        hang_after_script: AtomicBool,
        absent: AtomicBool,
        disconnected: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl DeviceRadio for FakeRadio {
        fn availability(&self) -> RadioAvailability {
            if self.absent.load(Ordering::SeqCst) {
                RadioAvailability::NotInThisBuild
            } else {
                RadioAvailability::Present
            }
        }

        async fn scan(&self, _timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError> {
            if let Some(code) = *self.scan_error.lock().unwrap() {
                return Err(AppError::new(code, "scan refused"));
            }
            Ok(self.adverts.lock().unwrap().clone())
        }

        async fn probe(&self, _id: &str) -> Result<RadioProbe, AppError> {
            Ok(self.probe.lock().unwrap().clone())
        }

        async fn list(&self, _id: &str, token: &SecretString) -> Result<DeltaListing, AppError> {
            assert_eq!(token.expose(), TOKEN, "the vault returned the stored token");
            if let Some(code) = *self.list_error.lock().unwrap() {
                return Err(AppError::new(code, "list refused"));
            }
            Ok(self
                .listing
                .lock()
                .unwrap()
                .clone()
                .expect("a listing was configured"))
        }

        async fn transfer(
            &self,
            request: TransferRequest<'_>,
            reporter: &dyn TransferReporter,
        ) -> Result<(), AppError> {
            assert_eq!(request.token.expose(), TOKEN);
            let script: Vec<TransferUpdate> = self
                .script
                .lock()
                .unwrap()
                .iter()
                .filter(|update| update.recording_id == request.recording.id)
                .cloned()
                .collect();
            for update in script {
                reporter.report(update).await;
            }
            if self.hang_after_script.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(())
        }

        async fn disconnect(&self, id: &str) {
            self.disconnected.lock().unwrap().push(id.to_owned());
        }
    }

    struct Harness {
        backend: DevicesBackend,
        radio: Arc<FakeRadio>,
        vault: Arc<InMemorySecretStore>,
        sink: Arc<RecordingEventSink>,
        pool: sqlx::SqlitePool,
        _dir: tempfile::TempDir,
    }

    fn advert(id: &str, services: &[u16]) -> ObservedAdvertisement {
        ObservedAdvertisement {
            peripheral_id: id.to_owned(),
            local_name: Some("PLAUD_NOTE".to_owned()),
            service_ids: services.to_vec(),
            manufacturer_ids: vec![0x0499],
            rssi: Some(-52),
        }
    }

    fn legacy_probe() -> RadioProbe {
        RadioProbe {
            model: Some("Note".into()),
            firmware: Some("V0095".into()),
            auth_mode: Some("legacy_static_token".into()),
            battery_percent: Some(81),
            rssi_dbm: Some(-50),
        }
    }

    fn listing(ids: &[&str], truncated: bool) -> DeltaListing {
        DeltaListing {
            entries: ids
                .iter()
                .map(|id| ListedRecording {
                    id: (*id).to_owned(),
                    display_name: format!("Recording {id}"),
                    declared_byte_length: 1_000_000,
                    captured_at_unix: 1_790_000_000,
                    codec: DeviceCodec::AsrOpus,
                })
                .collect(),
            truncated,
        }
    }

    async fn harness(register: &str) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let sink = Arc::new(RecordingEventSink::new());
        let events = EventPublisher::new(store.pool().clone(), sink.clone());
        let radio = Arc::new(FakeRadio::default());
        radio
            .adverts
            .lock()
            .unwrap()
            .push(advert(DEVICE, &[SERVICE_VENDOR]));
        *radio.probe.lock().unwrap() = legacy_probe();
        *radio.listing.lock().unwrap() = Some(listing(&["rec-1", "rec-2"], false));
        let vault = Arc::new(InMemorySecretStore::new());
        let backend = DevicesBackend::new(DevicesDeps {
            register: CompatibilityRegister::from_yaml(register).unwrap(),
            host: HostProfile {
                host_os: "macos".into(),
                os_version: "27.0".into(),
            },
            radio: radio.clone(),
            vault: vault.clone(),
            db: store.db().clone(),
            archive: store.archive().clone(),
            events,
        });
        Harness {
            backend,
            radio,
            vault,
            sink,
            pool: store.pool().clone(),
            _dir: dir,
        }
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..300 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition was not met in time");
    }

    fn secret(value: &str) -> SecretValue {
        SecretValue::new(value.to_owned()).unwrap()
    }

    async fn scanned_and_probed(h: &Harness, approve: bool) -> DeviceProbeDto {
        h.backend.list_nearby(5_000).await.unwrap();
        h.backend.probe(DEVICE, approve).await.unwrap()
    }

    /* ----------------------------------------------------------- pure */

    fn decision(
        register: &str,
        firmware: &str,
        auth: &str,
    ) -> (GateDecision, Option<&'static ProtocolProfile>) {
        let gate = GatePolicy::new(CompatibilityRegister::from_yaml(register).unwrap());
        let probed = ProbedProfile {
            source_type: SOURCE_TYPE_BLE.into(),
            device_model: "Note".into(),
            firmware: firmware.into(),
            host_os: "macos".into(),
            os_version: "27.0".into(),
            transport: TRANSPORT_BLE.into(),
            auth_mode: auth.into(),
        };
        (gate.evaluate(&probed), profile_for("Note", firmware))
    }

    #[test]
    fn every_section_is_reachable_from_a_probed_tuple() {
        let (supported, protocol) = decision(SUPPORTED, "V0095", "legacy_static_token");
        assert!(supported.permits_transfer());
        assert_eq!(
            section_for_probed(&supported, protocol, false, false),
            DeviceSection::NeedsAuthorization
        );
        assert_eq!(
            section_for_probed(&supported, protocol, true, false),
            DeviceSection::Nearby
        );
        assert_eq!(
            section_for_probed(&supported, protocol, true, true),
            DeviceSection::Connected
        );

        let (unknown_fw, protocol) = decision(COMMITTED, "V0123", "legacy_static_token");
        assert!(protocol.is_none());
        assert_eq!(
            section_for_probed(&unknown_fw, protocol, true, true),
            DeviceSection::UnsupportedFirmware,
            "an unknown firmware never reads as connected"
        );

        let (encrypted, protocol) = decision(COMMITTED, "encrypted-mode", "encrypted_handshake");
        assert!(matches!(
            encrypted,
            GateDecision::Refused {
                refusal: GateRefusal::KnownUnsupported { .. }
            }
        ));
        assert_eq!(
            section_for_probed(&encrypted, protocol, true, true),
            DeviceSection::UnsupportedFirmware
        );
    }

    #[test]
    fn the_committed_register_marks_the_legacy_profile_not_evaluated() {
        let (decision, _) = decision(COMMITTED, "V0095", "legacy_static_token");
        assert_eq!(decision_status(&decision), SupportStatus::NotEvaluated);
        assert!(!decision.permits_transfer());
        assert!(decision.offers_experimental_probe());
        assert_ne!(decision.known_limitations(), Vec::<String>::new());
    }

    #[test]
    fn os_versions_reduce_to_register_keys_and_unknowns_fail_closed() {
        assert_eq!(os_version_key("macos", Some("27.0.1")), "27.0");
        assert_eq!(os_version_key("macos", Some("26")), "26.0");
        assert_eq!(os_version_key("macos", Some("")), "");
        assert_eq!(os_version_key("macos", None), "");
        assert_eq!(os_version_key("windows", Some("11 (22631)")), "11");
        assert_eq!(os_version_key("windows", Some("10.0.22631")), "11");
        assert_eq!(os_version_key("windows", Some("10.0.19045")), "10.0.19045");
    }

    #[test]
    fn eta_and_bandwidth_are_estimates_that_stay_unknown_when_unmeasurable() {
        assert_eq!(observed_rate(0, 1_000), None);
        assert_eq!(observed_rate(500, 0), None);
        assert_eq!(observed_rate(10_000, 2_000), Some(5_000));
        assert_eq!(estimate_eta(100_000, 10_000, Some(5_000)), Some(18));
        assert_eq!(estimate_eta(100_000, 10_000, None), None);
        assert_eq!(estimate_eta(100_000, 10_000, Some(0)), None);
        assert_eq!(estimate_eta(100_000, 100_000, Some(5_000)), None);
        assert_eq!(estimate_eta(100_000, 120_000, Some(5_000)), None);
    }

    #[test]
    fn ble_media_is_never_labelled_original_or_high_quality() {
        let (decision, protocol) = decision(COMMITTED, "V0095", "legacy_static_token");
        let label = source_label(Some(&decision), protocol);
        assert_eq!(label, "Device BLE · compressed ASR");
        assert_eq!(
            source_label(None, None),
            SOURCE_LABEL_BLE_UNKNOWN,
            "no evidence, no quality claim"
        );
        for text in [
            &label,
            SOURCE_LABEL_BLE_UNKNOWN,
            COMPLETENESS_DELTA,
            COMPLETENESS_UNKNOWN,
        ] {
            let lower = text.to_lowercase();
            for banned in [
                "original wav",
                "high-quality",
                "full-resolution",
                "secure connection",
            ] {
                assert!(!lower.contains(banned), "{text:?} contains {banned}");
            }
        }
    }

    /* -------------------------------------------------------- scanning */

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_build_without_a_radio_says_so_instead_of_returning_an_empty_list() {
        let h = harness(COMMITTED).await;
        h.radio.absent.store(true, Ordering::SeqCst);
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(scan.radio.state, RadioState::UnavailableInBuild);
        assert_eq!(scan.radio.code.as_deref(), Some("SOURCE_UNAVAILABLE"));
        assert!(scan.radio.message.contains("not the same as no devices"));
        assert_eq!(scan.devices, Vec::new());
        assert_eq!(scan.scanned_at, None, "nothing was scanned");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_denied_radio_is_reported_with_its_error_code() {
        let h = harness(COMMITTED).await;
        *h.radio.scan_error.lock().unwrap() = Some(AppErrorCode::RadioPermissionDenied);
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(scan.radio.state, RadioState::PermissionDenied);
        assert_eq!(scan.radio.code.as_deref(), Some("RADIO_PERMISSION_DENIED"));
        assert_eq!(scan.devices, Vec::new());
    }

    /// The wire form of a code, parsed rather than named, so these #301 tests
    /// compile against a taxonomy that does not have the code yet and fail on
    /// the missing behaviour instead.
    fn issue_301_code(wire: &str) -> AppErrorCode {
        serde_json::from_value(serde_json::Value::String(wire.to_owned()))
            .unwrap_or_else(|_| panic!("{wire} is not in the error taxonomy"))
    }

    fn issue_301_state_name(state: RadioState) -> String {
        serde_json::to_value(state)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Scan with the radio failing as `wire`, and return the reported status.
    async fn issue_301_scan_failing_with(wire: &str) -> RadioStatusDto {
        let h = harness(COMMITTED).await;
        *h.radio.scan_error.lock().unwrap() = Some(issue_301_code(wire));
        let scan = h
            .backend
            .list_nearby(5_000)
            .await
            .unwrap_or_else(|err| panic!("{wire} became a scan error: {err:?}"));
        assert_eq!(scan.devices, Vec::new(), "{wire}: nothing was heard");
        assert_eq!(scan.scanned_at, None, "{wire}: nothing was scanned");
        scan.radio
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn issue_301_an_unanswered_permission_prompt_has_its_own_state() {
        let radio = issue_301_scan_failing_with("RADIO_PERMISSION_NOT_DETERMINED").await;
        assert_eq!(
            issue_301_state_name(radio.state),
            "permission_not_determined"
        );
        assert_eq!(
            radio.code.as_deref(),
            Some("RADIO_PERMISSION_NOT_DETERMINED")
        );
        assert_ne!(radio.message, RADIO_DENIED_MESSAGE);
        assert!(radio.message.contains("not the same as no devices"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn issue_301_a_powered_off_adapter_has_its_own_state() {
        let radio = issue_301_scan_failing_with("RADIO_POWERED_OFF").await;
        assert_eq!(issue_301_state_name(radio.state), "powered_off");
        assert_eq!(radio.code.as_deref(), Some("RADIO_POWERED_OFF"));
        assert!(radio.message.contains("turned off"), "{}", radio.message);
        assert!(radio.message.contains("not the same as no devices"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn issue_301_a_missing_adapter_has_its_own_state() {
        let radio = issue_301_scan_failing_with("RADIO_NO_ADAPTER").await;
        assert_eq!(issue_301_state_name(radio.state), "no_adapter");
        assert_eq!(radio.code.as_deref(), Some("RADIO_NO_ADAPTER"));
        assert!(radio.message.contains("adapter"), "{}", radio.message);
        assert!(radio.message.contains("not the same as no devices"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn issue_301_every_radio_refusal_reads_differently() {
        let mut seen = vec![
            RADIO_DENIED_MESSAGE.to_owned(),
            RADIO_UNAVAILABLE_MESSAGE.to_owned(),
        ];
        let mut states = vec![
            "permission_denied".to_owned(),
            "unavailable_in_build".to_owned(),
        ];
        for wire in [
            "RADIO_PERMISSION_NOT_DETERMINED",
            "RADIO_POWERED_OFF",
            "RADIO_NO_ADAPTER",
        ] {
            let radio = issue_301_scan_failing_with(wire).await;
            assert!(!seen.contains(&radio.message), "{wire} reuses a message");
            let name = issue_301_state_name(radio.state);
            assert!(!states.contains(&name), "{wire} reuses the state {name}");
            seen.push(radio.message);
            states.push(name);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn any_other_scan_failure_is_an_error_not_an_empty_scan() {
        let h = harness(COMMITTED).await;
        *h.radio.scan_error.lock().unwrap() = Some(AppErrorCode::DeviceBusy);
        let err = h.backend.list_nearby(5_000).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::DeviceBusy);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_vendor_service_advertisers_are_candidates_and_none_is_an_identity() {
        let h = harness(COMMITTED).await;
        h.radio
            .adverts
            .lock()
            .unwrap()
            .push(advert("fake-name-only", &[0x180F]));
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(scan.radio.state, RadioState::Scanned);
        assert!(scan.scanned_at.is_some());
        assert_eq!(scan.devices.len(), 1, "a name alone is not a candidate");
        let device = &scan.devices[0];
        assert_eq!(device.peripheral_id, DEVICE);
        assert_eq!(device.section, DeviceSection::Nearby);
        assert_eq!(device.identity_label, "Unverified identity");
        assert_eq!(device.model, None, "model is unknown before a probe");
        assert_eq!(device.firmware, None, "firmware is unknown before a probe");
        assert_eq!(device.status_label, "Not evaluated");
        assert!(h.sink.kinds().contains(&EventKind::DeviceDiscovered));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probing_a_device_that_was_not_scanned_is_refused() {
        let h = harness(COMMITTED).await;
        let err = h.backend.probe(DEVICE, false).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::InvalidArgument);
    }

    /* --------------------------------------------------------- probing */

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_legacy_probe_reports_register_copy_and_the_radio_disclosures() {
        let h = harness(COMMITTED).await;
        let dto = scanned_and_probed(&h, false).await;
        assert_eq!(dto.section, DeviceSection::NeedsAuthorization);
        assert_eq!(dto.support_status, SupportStatus::NotEvaluated);
        assert_eq!(dto.status_label, SupportStatus::NotEvaluated.ui_label());
        assert!(dto
            .known_limitations
            .iter()
            .any(|text| text.contains("delta of unsynchronized files")));
        assert_eq!(dto.disclosures, LEGACY_DISCLOSURES);
        assert_eq!(dto.model.as_deref(), Some("Note"));
        assert_eq!(dto.firmware.as_deref(), Some("V0095"));
        assert_eq!(dto.battery_percent, Some(81));
        assert_eq!(dto.source_label, "Device BLE · compressed ASR");
        assert_eq!(dto.cloud_route, CloudRoute::NotUsed);
        assert_eq!(dto.identity_label, "Unverified identity");
        assert!(!dto.gate.permits_transfer);
        assert!(dto.gate.experimental_probe_available);
        assert!(!dto.gate.experimental_probe_approved);
        assert!(dto.provisioning_note.is_some());
        assert!(!dto.sync.may_advertise);
        assert!(!dto.sync.default_on);
        assert!(h
            .sink
            .kinds()
            .contains(&EventKind::DeviceCompatibilityChanged));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_firmware_that_could_not_be_read_is_unknown_never_guessed() {
        let h = harness(COMMITTED).await;
        *h.radio.probe.lock().unwrap() = RadioProbe {
            model: Some("Note".into()),
            firmware: None,
            auth_mode: None,
            battery_percent: None,
            rssi_dbm: None,
        };
        let dto = scanned_and_probed(&h, false).await;
        assert_eq!(dto.firmware, None);
        assert_eq!(dto.battery_percent, None);
        assert_eq!(dto.auth_mode, None);
        assert_eq!(dto.section, DeviceSection::UnsupportedFirmware);
        assert_eq!(dto.status_label, "Not evaluated");
        assert_eq!(dto.gate.refusal_code.as_deref(), Some("INCOMPLETE_PROBE"));
        assert!(!dto.capabilities.iter().any(|c| c.available));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_encrypted_handshake_firmware_is_unsupported_not_a_bad_password() {
        let h = harness(COMMITTED).await;
        *h.radio.probe.lock().unwrap() = RadioProbe {
            model: Some("Note".into()),
            firmware: Some("encrypted-mode".into()),
            auth_mode: Some("encrypted_handshake".into()),
            ..RadioProbe::default()
        };
        let dto = scanned_and_probed(&h, false).await;
        assert_eq!(dto.section, DeviceSection::UnsupportedFirmware);
        assert_eq!(dto.support_status, SupportStatus::KnownUnsupported);
        assert_eq!(dto.status_label, "Known unsupported");
        assert!(
            dto.disclosures.is_empty(),
            "the legacy disclosure is for the legacy profile"
        );
        let err = h
            .backend
            .authorize(DEVICE, secret(TOKEN))
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::UnsupportedFirmware);
        assert_eq!(h.vault.entry_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unlisted_firmware_has_no_profile_and_cannot_be_authorized() {
        let h = harness(COMMITTED).await;
        h.radio.probe.lock().unwrap().firmware = Some("V0777".into());
        let dto = scanned_and_probed(&h, false).await;
        assert_eq!(dto.section, DeviceSection::UnsupportedFirmware);
        assert_eq!(dto.firmware.as_deref(), Some("V0777"));
        let err = h
            .backend
            .authorize(DEVICE, secret(TOKEN))
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::UnsupportedFirmware);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unprobed_device_cannot_be_authorized_listed_imported_or_synced() {
        let h = harness(COMMITTED).await;
        h.backend.list_nearby(5_000).await.unwrap();
        for err in [
            h.backend
                .authorize(DEVICE, secret(TOKEN))
                .await
                .unwrap_err(),
            h.backend.list_recordings(DEVICE).await.unwrap_err(),
            h.backend
                .import(DEVICE, vec!["rec-1".into()], true)
                .await
                .unwrap_err(),
            h.backend.sync_policy(DEVICE, true).await.unwrap_err(),
        ] {
            assert!(
                matches!(
                    err.code(),
                    AppErrorCode::InvalidArgument | AppErrorCode::AuthBootstrapRequired
                ),
                "{:?}",
                err.code()
            );
        }
    }

    /* ----------------------------------------------------- credentials */

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authorizing_stores_only_in_the_vault_and_returns_status_without_the_token() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, false).await;
        let status = h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        assert!(status.credential_stored);
        assert_eq!(status.identity_label, "Unverified identity");
        assert_eq!(h.vault.entry_count(), 1);

        assert!(!serde_json::to_string(&status).unwrap().contains(TOKEN));
        // The ledger holds the device and the vault coordinates, nothing else.
        let refs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_credential_refs")
            .fetch_one(&h.pool)
            .await
            .unwrap();
        assert_eq!(refs, 1);
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT vault_service, vault_account_reference, auth_mode FROM device_credential_refs",
        )
        .fetch_all(&h.pool)
        .await
        .unwrap();
        assert!(rows
            .iter()
            .all(|(a, b, c)| ![a, b, c].iter().any(|v| v.contains(TOKEN))));
        let device_rows: Vec<String> = sqlx::query_scalar(
            "SELECT COALESCE(serial_fingerprint,'') || COALESCE(discovered_name,'') || model || \
             COALESCE(observed_firmware,'') FROM devices",
        )
        .fetch_all(&h.pool)
        .await
        .unwrap();
        assert!(device_rows.iter().all(|row| !row.contains(TOKEN)));
        let all: Vec<String> = sqlx::query_scalar("SELECT value_json FROM app_preferences")
            .fetch_all(&h.pool)
            .await
            .unwrap();
        assert!(all.iter().all(|value| !value.contains(TOKEN)));

        let probe = h.backend.probe(DEVICE, false).await.unwrap();
        assert!(probe.authorized);
        assert_eq!(
            probe.identity_label, "Unverified identity",
            "storing a credential does not authenticate the device"
        );
        assert_eq!(probe.section, DeviceSection::Nearby);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_token_of_the_wrong_length_is_refused_before_the_vault_is_touched() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, false).await;
        let err = h
            .backend
            .authorize(DEVICE, secret("00112233"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::InvalidArgument);
        assert_eq!(h.vault.entry_count(), 0);
        assert!(!format!("{err:?}").contains("00112233"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_locked_vault_is_an_actionable_error_never_a_plaintext_fallback() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, false).await;
        h.vault.set_locked(true);
        let err = h
            .backend
            .authorize(DEVICE, secret(TOKEN))
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::SecretStoreLocked);
        let rows: Vec<String> = sqlx::query_scalar("SELECT value_json FROM app_preferences")
            .fetch_all(&h.pool)
            .await
            .unwrap();
        assert!(rows.iter().all(|row| !row.contains(TOKEN)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn revoking_removes_the_vault_entry_and_the_link() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        let status = h.backend.revoke(DEVICE).await.unwrap();
        assert!(!status.credential_stored);
        assert_eq!(h.vault.entry_count(), 0);
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        assert!(!scan.devices[0].authorized);
        assert!(
            !h.backend.revoke(DEVICE).await.unwrap().credential_stored,
            "revoking twice is harmless"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_linked_device_that_is_not_advertising_is_previously_linked() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        h.radio.adverts.lock().unwrap().clear();
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(scan.radio.state, RadioState::Scanned);
        assert_eq!(scan.devices.len(), 1);
        assert_eq!(scan.devices[0].section, DeviceSection::PreviouslyLinked);
        assert_eq!(scan.devices[0].identity_label, "Unverified identity");
        assert!(scan.devices[0].authorized);
        assert_eq!(
            scan.devices[0].rssi_dbm, None,
            "no signal is invented for a device not heard"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn previously_linked_devices_survive_an_unavailable_radio() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        h.radio.absent.store(true, Ordering::SeqCst);
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(scan.radio.state, RadioState::UnavailableInBuild);
        assert_eq!(scan.devices.len(), 1);
        assert_eq!(scan.devices[0].section, DeviceSection::PreviouslyLinked);
    }

    /* --------------------------------------------------------- listing */

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listing_without_an_experimental_approval_is_refused_for_a_not_evaluated_profile() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        let err = h.backend.list_recordings(DEVICE).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::CapabilityUnverified);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_approved_experimental_probe_may_list_but_never_transfer_or_sync() {
        let h = harness(COMMITTED).await;
        let probe = scanned_and_probed(&h, true).await;
        assert!(probe.gate.experimental_probe_approved);
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();

        let listing = h.backend.list_recordings(DEVICE).await.unwrap();
        assert_eq!(listing.file_count, 2);
        assert_eq!(listing.completeness, ListingCompleteness::UnsyncedDelta);
        assert_eq!(listing.completeness_label, COMPLETENESS_DELTA);
        assert_eq!(listing.identity_label, "Profile-authenticated");

        let err = h
            .backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::CapabilityUnverified);
        let err = h.backend.sync_policy(DEVICE, true).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::CapabilityUnverified);
        assert!(!h.sink.kinds().contains(&EventKind::DeviceTransferProgress));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn re_probing_without_approval_withdraws_an_earlier_experimental_approval() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, true).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        h.backend.probe(DEVICE, false).await.unwrap();
        let err = h.backend.list_recordings(DEVICE).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::CapabilityUnverified);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_device_is_unverified_until_a_listing_authenticates_and_then_connected() {
        let h = harness(SUPPORTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        let before = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(before.devices[0].identity_label, "Unverified identity");
        assert_eq!(before.devices[0].section, DeviceSection::Nearby);

        h.backend.list_recordings(DEVICE).await.unwrap();
        assert!(h.sink.kinds().contains(&EventKind::DeviceConnected));
        let after = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(after.devices[0].identity_label, "Profile-authenticated");
        assert_eq!(after.devices[0].section, DeviceSection::Connected);
        assert!(after.devices[0].last_contact_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rejected_credential_keeps_the_device_unverified() {
        let h = harness(SUPPORTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        *h.radio.list_error.lock().unwrap() = Some(AppErrorCode::AuthRejected);
        let err = h.backend.list_recordings(DEVICE).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::AuthRejected);
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(scan.devices[0].identity_label, "Unverified identity");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncated_and_empty_listings_never_claim_a_complete_history() {
        let h = harness(SUPPORTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();

        *h.radio.listing.lock().unwrap() = Some(listing(&["rec-1"], true));
        let truncated = h.backend.list_recordings(DEVICE).await.unwrap();
        assert_eq!(
            truncated.completeness,
            ListingCompleteness::UnsyncedDeltaTruncated
        );
        assert_eq!(truncated.completeness_label, COMPLETENESS_DELTA_TRUNCATED);

        *h.radio.listing.lock().unwrap() = Some(listing(&[], false));
        let empty = h.backend.list_recordings(DEVICE).await.unwrap();
        assert_eq!(empty.file_count, 0);
        assert_eq!(empty.items, Vec::new());
        assert!(empty
            .completeness_label
            .contains("not a complete recording history"));
    }

    /* -------------------------------------------------------- transfer */

    fn update(
        recording: &str,
        event: TransferEvent,
        bytes: Option<u64>,
        elapsed: Option<u64>,
    ) -> TransferUpdate {
        TransferUpdate {
            recording_id: recording.to_owned(),
            event,
            bytes_received: bytes,
            elapsed_ms: elapsed,
            bytes_per_second: None,
            eta_seconds: None,
        }
    }

    async fn supported_session(h: &Harness) {
        scanned_and_probed(h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        h.backend.list_recordings(DEVICE).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_supported_transfer_walks_on_device_to_transferred_locally_to_archived() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        *h.radio.script.lock().unwrap() = vec![
            update("rec-1", TransferEvent::ConnectStarted, None, None),
            update("rec-1", TransferEvent::LinkEstablished, None, None),
            update("rec-1", TransferEvent::Authenticated, None, None),
            update("rec-1", TransferEvent::RequestSent, None, None),
            update(
                "rec-1",
                TransferEvent::BytesArriving,
                Some(250_000),
                Some(5_000),
            ),
            update(
                "rec-1",
                TransferEvent::StreamEnded,
                Some(1_000_000),
                Some(20_000),
            ),
            update("rec-1", TransferEvent::VerificationSucceeded, None, None),
        ];
        let before = h.backend.list_recordings(DEVICE).await.unwrap();
        assert!(before
            .items
            .iter()
            .all(|item| item.transfer.state == TransferState::Discovered
                && item.transfer.state_label == "On device"));

        let ids = h
            .backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);

        wait_until(|| h.sink.kinds().contains(&EventKind::DeviceTransferVerified)).await;
        let mid = h.backend.list_recordings(DEVICE).await.unwrap();
        let item = mid
            .items
            .iter()
            .find(|i| i.recording_id == "rec-1")
            .unwrap();
        assert_eq!(item.transfer.state, TransferState::Committing);
        assert_eq!(item.transfer.state_label, "Transferred locally");
        assert!(item.transfer.verified);
        assert_eq!(item.transfer.observed_bytes_per_sec, Some(50_000));
        assert_eq!(
            item.transfer.eta_seconds_estimate, None,
            "no estimate once verifying"
        );
        assert_eq!(item.transfer.transfer_id.as_deref(), Some(ids[0].as_str()));
        assert_eq!(item.transfer.source_label, "Device BLE · compressed ASR");
        assert_eq!(item.transfer.cloud_route, CloudRoute::NotUsed);
        let other = mid
            .items
            .iter()
            .find(|i| i.recording_id == "rec-2")
            .unwrap();
        assert_eq!(
            other.transfer.state_label, "On device",
            "an untouched recording stays on the device"
        );

        // The pipeline reports the durable commit: now the recording is archived.
        h.backend
            .inner
            .report_transfer(
                DEVICE,
                update("rec-1", TransferEvent::CommitSucceeded, None, None),
            )
            .await;
        let done = h.backend.list_recordings(DEVICE).await.unwrap();
        let item = done
            .items
            .iter()
            .find(|i| i.recording_id == "rec-1")
            .unwrap();
        assert_eq!(item.transfer.state_label, "Archived");

        let progress = h
            .sink
            .events()
            .iter()
            .filter(|event| event.kind == EventKind::DeviceTransferProgress)
            .count();
        assert!(
            progress >= 7,
            "every step publishes progress, got {progress}"
        );
        let seqs = h.sink.sequences();
        assert!(
            seqs.windows(2).all(|pair| pair[0] < pair[1]),
            "events are sequenced"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_eta_is_an_estimate_derived_from_observed_bandwidth_while_receiving() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        *h.radio.script.lock().unwrap() = vec![
            update("rec-1", TransferEvent::ConnectStarted, None, None),
            update("rec-1", TransferEvent::LinkEstablished, None, None),
            update("rec-1", TransferEvent::Authenticated, None, None),
            update("rec-1", TransferEvent::RequestSent, None, None),
            update(
                "rec-1",
                TransferEvent::BytesArriving,
                Some(100_000),
                Some(2_000),
            ),
        ];
        h.radio.hang_after_script.store(true, Ordering::SeqCst);
        h.backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap();
        wait_until(|| {
            h.sink.events().iter().any(|event| {
                event.payload.get("state").and_then(Value::as_str) == Some("receiving")
            })
        })
        .await;
        let listing = h.backend.list_recordings(DEVICE).await.unwrap();
        let t = &listing
            .items
            .iter()
            .find(|i| i.recording_id == "rec-1")
            .unwrap()
            .transfer;
        assert_eq!(t.state_label, "Transferring");
        assert_eq!(t.bytes_received, Some(100_000));
        assert_eq!(t.observed_bytes_per_sec, Some(50_000));
        assert_eq!(
            t.eta_seconds_estimate,
            Some(18),
            "(1,000,000 - 100,000) / 50,000"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disconnecting_cancels_in_flight_transfers_and_keeps_the_archive() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        *h.radio.script.lock().unwrap() = vec![
            update("rec-1", TransferEvent::ConnectStarted, None, None),
            update("rec-1", TransferEvent::LinkEstablished, None, None),
        ];
        h.radio.hang_after_script.store(true, Ordering::SeqCst);
        h.backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap();
        wait_until(|| {
            h.sink.events().iter().any(|event| {
                event.payload.get("state").and_then(Value::as_str) == Some("authenticating")
            })
        })
        .await;

        h.backend.disconnect(DEVICE).await.unwrap();
        let listing = lock(&h.backend.inner.state)
            .transfers
            .get(&(DEVICE.to_owned(), "rec-1".to_owned()))
            .map(|record| record.ledger.state());
        assert_eq!(listing, Some(TransferState::Cancelled));
        assert!(h
            .radio
            .disconnected
            .lock()
            .unwrap()
            .contains(&DEVICE.to_owned()));
        assert!(h.sink.kinds().contains(&EventKind::DeviceDisconnected));
        let after = h.backend.list_nearby(5_000).await.unwrap();
        assert_eq!(after.devices[0].identity_label, "Unverified identity");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_radio_failure_mid_transfer_leaves_it_interrupted_and_nothing_archived() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        // No script and a failing radio: model by revoking the credential
        // under the transfer so the token cannot be loaded.
        h.vault.set_unavailable(true);
        h.backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap();
        wait_until(|| {
            lock(&h.backend.inner.state)
                .transfers
                .get(&(DEVICE.to_owned(), "rec-1".to_owned()))
                .is_some_and(|record| record.ledger.state() == TransferState::Interrupted)
        })
        .await;
        h.vault.set_unavailable(false);
        let listing = h.backend.list_recordings(DEVICE).await.unwrap();
        let item = listing
            .items
            .iter()
            .find(|i| i.recording_id == "rec-1")
            .unwrap();
        assert_eq!(item.transfer.state_label, "Interrupted");
        assert!(!item.transfer.verified);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_recordings_in_the_devices_own_listing_may_be_imported() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        let err = h
            .backend
            .import(DEVICE, vec!["rec-1".into(), "not-on-device".into()], true)
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::NotPresentOnDevice);
        assert!(
            lock(&h.backend.inner.state).transfers.is_empty(),
            "a refused request starts nothing"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_archived_recording_cannot_be_imported_twice_and_a_running_one_is_busy() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        *h.radio.script.lock().unwrap() =
            vec![update("rec-1", TransferEvent::ConnectStarted, None, None)];
        h.radio.hang_after_script.store(true, Ordering::SeqCst);
        h.backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap();
        let err = h
            .backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::DeviceBusy);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn importing_requires_a_listing_that_authenticated_this_session() {
        let h = harness(SUPPORTED).await;
        scanned_and_probed(&h, false).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        let err = h
            .backend
            .import(DEVICE, vec!["rec-1".into()], true)
            .await
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::InvalidArgument);
    }

    /* ----------------------------------------------------- sync policy */

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn automatic_sync_is_never_advertised_without_a_passed_audit_and_a_supported_cell() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        let dto = h.backend.probe(DEVICE, false).await.unwrap();
        assert!(!dto.sync.may_advertise, "supported but never audited");
        assert_eq!(dto.sync.audit_code, "NOT_RUN");
        let err = h.backend.sync_policy(DEVICE, true).await.unwrap_err();
        assert_eq!(err.code(), AppErrorCode::CapabilityUnverified);
        let off = h.backend.sync_policy(DEVICE, false).await.unwrap();
        assert!(!off.enabled);
        assert!(!off.default_on);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_passed_audit_on_a_supported_cell_lets_the_user_opt_in_and_it_persists() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        h.backend.record_audit(
            LEGACY_NOTE_V0095.name,
            AuditOutcome::Unchanged { rounds: 3 },
        );
        let probe = h.backend.probe(DEVICE, false).await.unwrap();
        assert!(probe.sync.may_advertise);
        assert!(!probe.sync.enabled, "never on by default");
        assert!(probe
            .capabilities
            .iter()
            .any(|c| c.capability == DeviceCapability::AutomaticSync && c.available));

        let on = h.backend.sync_policy(DEVICE, true).await.unwrap();
        assert!(on.enabled);
        let again = h.backend.probe(DEVICE, false).await.unwrap();
        assert!(again.sync.enabled);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_passed_audit_does_not_advertise_sync_for_an_unsupported_profile() {
        let h = harness(COMMITTED).await;
        scanned_and_probed(&h, true).await;
        h.backend.authorize(DEVICE, secret(TOKEN)).await.unwrap();
        h.backend.record_audit(
            LEGACY_NOTE_V0095.name,
            AuditOutcome::Unchanged { rounds: 3 },
        );
        let dto = h.backend.probe(DEVICE, true).await.unwrap();
        assert!(!dto.sync.may_advertise);
        assert!(dto.sync.explanation.contains("not Supported"));
    }

    /* ------------------------------------------------- confidentiality */

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nothing_the_backend_emits_contains_the_token() {
        let h = harness(SUPPORTED).await;
        supported_session(&h).await;
        *h.radio.script.lock().unwrap() =
            vec![update("rec-1", TransferEvent::ConnectStarted, None, None)];
        h.backend
            .import(DEVICE, vec!["rec-1".into()], false)
            .await
            .unwrap();
        wait_until(|| h.sink.kinds().contains(&EventKind::DeviceTransferProgress)).await;
        let scan = h.backend.list_nearby(5_000).await.unwrap();
        let probe = h.backend.probe(DEVICE, false).await.unwrap();
        let listing = h.backend.list_recordings(DEVICE).await.unwrap();
        let rendered = format!(
            "{}{}{}{}",
            serde_json::to_string(&scan).unwrap(),
            serde_json::to_string(&probe).unwrap(),
            serde_json::to_string(&listing).unwrap(),
            serde_json::to_string(&h.sink.events()).unwrap(),
        );
        assert!(!rendered.contains(TOKEN));
        assert!(!rendered.contains("00112233445566778899"));
    }
}
