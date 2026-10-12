//! The deterministic `FakeDevice` simulator (spec §18.3A, §18.6A).
//!
//! # What it is
//!
//! A [`GattTransport`] that replays a **captured-shape wire script**: a
//! versioned fixture naming its protocol profile, its device model and its
//! firmware, and listing the exact notification frames the device sends in
//! response to each allowlisted request. There is no randomness and no clock
//! dependency, so a scenario produces the same bytes on macOS, Windows and
//! Linux, in CI and locally.
//!
//! # What it is not
//!
//! Evidence. Spec §18.6A: "a synthetic Bluetooth fixture alone cannot produce
//! a `Supported` device compatibility cell". [`DeviceScenario::is_synthetic`]
//! is therefore always true for a fixture in this repository, and the
//! evidence record a simulator run produces is
//! `EvidenceSource::Simulator`, which cannot promote a cell.
//!
//! # Fixture hygiene
//!
//! Spec §18.2A prohibits shipping raw token or audio captures in test assets.
//! Every fixture in `tests/fixtures/device/` is synthesized from the protocol
//! description in spec App. B.3; [`DeviceScenario::validate`] rejects a
//! fixture that does not declare itself synthetic.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use plaudon_plaud_protocol::bulk::BulkFrame;
use plaudon_plaud_protocol::frame::ControlFrame;
use plaudon_plaud_protocol::payload::{AuthResponse, AuthStatus, DataRequest, DeltaListing};
use plaudon_plaud_protocol::profile::{profile_by_name, ProtocolProfile};
use plaudon_plaud_protocol::ReadOnlyOpcode;
use plaudon_recording_sources::device::DeviceError;
use serde::{Deserialize, Serialize};

use crate::transport::{Characteristics, GattTransport, ProbeResult};

/// Fixture schema version this build reads and writes.
pub const FIXTURE_VERSION: u32 = 1;

/// The request an exchange expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum ExpectedRequest {
    /// The authenticate opcode.
    Authenticate,
    /// The delta-list opcode.
    ListDelta,
    /// The data-request opcode, optionally for a named recording.
    RequestData {
        /// Recording the request must name, when the fixture pins one.
        #[serde(default)]
        id: Option<String>,
    },
}

impl ExpectedRequest {
    /// The opcode this request uses.
    #[must_use]
    pub const fn opcode(&self) -> ReadOnlyOpcode {
        match self {
            Self::Authenticate => ReadOnlyOpcode::Authenticate,
            Self::ListDelta => ReadOnlyOpcode::ListDelta,
            Self::RequestData { .. } => ReadOnlyOpcode::RequestData,
        }
    }
}

/// What the device does after delivering an exchange's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterExchange {
    /// Wait for the next request.
    #[default]
    Continue,
    /// Drop the link.
    Disconnect,
    /// Report that another host owns the device.
    Busy,
    /// Report that the battery is too low to continue.
    PowerLow,
    /// Stop answering, so the caller's notification timeout fires.
    Silence,
}

/// One request and the frames it produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    /// Request this exchange answers.
    pub expect: ExpectedRequest,
    /// Notification frames, hex encoded, in delivery order.
    #[serde(default)]
    pub reply_frames: Vec<String>,
    /// What happens once the frames are delivered.
    #[serde(default)]
    pub then: AfterExchange,
}

/// A replayable device script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceScenario {
    /// Fixture schema version.
    pub fixture_version: u32,
    /// Stable scenario name.
    pub name: String,
    /// Protocol profile this script belongs to.
    pub profile: String,
    /// Model the simulated device reports.
    #[serde(default)]
    pub device_model: Option<String>,
    /// Firmware the simulated device reports.
    #[serde(default)]
    pub firmware: Option<String>,
    /// Always true for a fixture in this repository.
    pub synthetic: bool,
    /// Where the script came from and what it exercises.
    pub provenance: String,
    /// Battery percentage the simulated device reports.
    #[serde(default)]
    pub battery_percent: Option<u8>,
    /// Control characteristic the simulated device exposes, when the script
    /// deliberately diverges from the profile.
    #[serde(default)]
    pub control_characteristic: Option<u16>,
    /// Whether the host denies radio access for this scenario.
    #[serde(default)]
    pub radio_permission_denied: bool,
    /// The script.
    pub exchanges: Vec<Exchange>,
}

/// Why a fixture was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScenarioError {
    /// The fixture declares a schema version this build does not read.
    #[error("fixture schema version {found} is not {FIXTURE_VERSION}")]
    Version {
        /// Version found.
        found: u32,
    },
    /// The fixture names a protocol profile this build does not implement.
    #[error("fixture names the unknown protocol profile {profile}")]
    UnknownProfile {
        /// Profile named.
        profile: String,
    },
    /// The fixture does not declare itself synthetic.
    #[error(
        "a fixture must declare itself synthetic; captured credentials and audio are not shipped"
    )]
    NotSynthetic,
    /// A frame could not be decoded from hex.
    #[error("a reply frame is not valid hex")]
    BadHex,
    /// The fixture declares no provenance.
    #[error("a fixture must record where its bytes came from")]
    NoProvenance,
}

impl DeviceScenario {
    /// Parse a fixture document.
    pub fn from_json(document: &str) -> Result<Self, ScenarioError> {
        let scenario: Self = serde_json::from_str(document).map_err(|_| ScenarioError::BadHex)?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Validate the fixture's own hygiene rules.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        if self.fixture_version != FIXTURE_VERSION {
            return Err(ScenarioError::Version {
                found: self.fixture_version,
            });
        }
        if !self.synthetic {
            return Err(ScenarioError::NotSynthetic);
        }
        if self.provenance.trim().is_empty() {
            return Err(ScenarioError::NoProvenance);
        }
        if self.protocol().is_none() {
            return Err(ScenarioError::UnknownProfile {
                profile: self.profile.clone(),
            });
        }
        for exchange in &self.exchanges {
            for frame in &exchange.reply_frames {
                decode_hex(frame).ok_or(ScenarioError::BadHex)?;
            }
        }
        Ok(())
    }

    /// Whether this script is synthetic. Always true for shipped fixtures.
    #[must_use]
    pub const fn is_synthetic(&self) -> bool {
        self.synthetic
    }

    /// The protocol profile this script belongs to.
    #[must_use]
    pub fn protocol(&self) -> Option<&'static ProtocolProfile> {
        profile_by_name(&self.profile)
    }
}

/// Decode a whitespace-tolerant hex string.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !compact.len().is_multiple_of(2) {
        return None;
    }
    let bytes = compact.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push(u8::try_from(high << 4 | low).ok()?);
    }
    Some(out)
}

/// Encode bytes as lowercase hex, for writing fixtures.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// What the simulator observed the host doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostTrace {
    /// Opcodes the host wrote, in order.
    pub emitted_opcodes: Vec<ReadOnlyOpcode>,
    /// Characteristics the host wrote to, in order.
    pub written_characteristics: Vec<u16>,
    /// Requests that did not match the script.
    pub unscripted_requests: Vec<ReadOnlyOpcode>,
    /// Whether the host ever presented a credential.
    pub authenticated: bool,
}

impl HostTrace {
    /// Whether every write stayed inside the read-only allowlist.
    ///
    /// Structurally guaranteed by [`ReadOnlyOpcode`], and asserted here so a
    /// scenario run produces positive evidence rather than an absence
    /// (spec §18.7 scenario 42).
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.emitted_opcodes
            .iter()
            .all(|opcode| !opcode.mutates_device())
    }

    /// The opcode names, for an evidence record.
    #[must_use]
    pub fn opcode_names(&self) -> Vec<String> {
        self.emitted_opcodes
            .iter()
            .map(|opcode| {
                match opcode {
                    ReadOnlyOpcode::Authenticate => "authenticate",
                    ReadOnlyOpcode::ListDelta => "listDelta",
                    ReadOnlyOpcode::RequestData => "requestData",
                }
                .to_owned()
            })
            .collect()
    }
}

/// A deterministic device that replays a scenario.
#[derive(Debug)]
pub struct FakeDevice {
    scenario: DeviceScenario,
    profile: &'static ProtocolProfile,
    next_exchange: usize,
    pending: VecDeque<Vec<u8>>,
    after: AfterExchange,
    trace: HostTrace,
    disconnected: bool,
}

impl FakeDevice {
    /// Build a simulator from a validated scenario.
    pub fn new(scenario: DeviceScenario) -> Result<Self, ScenarioError> {
        scenario.validate()?;
        let profile = scenario
            .protocol()
            .ok_or_else(|| ScenarioError::UnknownProfile {
                profile: scenario.profile.clone(),
            })?;
        Ok(Self {
            scenario,
            profile,
            next_exchange: 0,
            pending: VecDeque::new(),
            after: AfterExchange::Continue,
            trace: HostTrace::default(),
            disconnected: false,
        })
    }

    /// Load a scenario fixture from disk.
    pub fn from_fixture(path: &std::path::Path) -> Result<Self, ScenarioError> {
        let document = std::fs::read_to_string(path).map_err(|_| ScenarioError::BadHex)?;
        Self::new(DeviceScenario::from_json(&document)?)
    }

    /// What the host did.
    #[must_use]
    pub const fn trace(&self) -> &HostTrace {
        &self.trace
    }

    /// The scenario being replayed.
    #[must_use]
    pub const fn scenario(&self) -> &DeviceScenario {
        &self.scenario
    }

    fn arm(&mut self, frames: Vec<Vec<u8>>, after: AfterExchange) {
        self.pending = frames.into();
        self.after = after;
    }
}

#[async_trait]
impl GattTransport for FakeDevice {
    async fn discover(&mut self) -> Result<ProbeResult, DeviceError> {
        if self.scenario.radio_permission_denied {
            return Err(DeviceError::RadioPermissionDenied);
        }
        if self.disconnected {
            return Err(DeviceError::DeviceDisconnected);
        }
        Ok(ProbeResult {
            device_model: self.scenario.device_model.clone(),
            firmware: self.scenario.firmware.clone(),
            characteristics: Characteristics {
                control: self
                    .scenario
                    .control_characteristic
                    .unwrap_or(self.profile.characteristic_control),
                notify: self.profile.characteristic_notify,
                battery: Some(self.profile.characteristic_battery),
            },
            battery_percent: self.scenario.battery_percent,
        })
    }

    async fn write_control(&mut self, frame: &ControlFrame) -> Result<(), DeviceError> {
        if self.disconnected {
            return Err(DeviceError::DeviceDisconnected);
        }
        self.trace.emitted_opcodes.push(frame.opcode);
        self.trace
            .written_characteristics
            .push(self.profile.characteristic_control);
        if frame.opcode == ReadOnlyOpcode::Authenticate {
            self.trace.authenticated = true;
        }

        let Some(exchange) = self.scenario.exchanges.get(self.next_exchange).cloned() else {
            self.trace.unscripted_requests.push(frame.opcode);
            return Err(DeviceError::NotPresentOnDevice);
        };
        if exchange.expect.opcode() != frame.opcode {
            self.trace.unscripted_requests.push(frame.opcode);
            return Err(DeviceError::NotPresentOnDevice);
        }
        if let ExpectedRequest::RequestData { id: Some(expected) } = &exchange.expect {
            let request =
                DataRequest::decode(&frame.payload).map_err(|_| DeviceError::NotPresentOnDevice)?;
            if &request.id != expected {
                self.trace.unscripted_requests.push(frame.opcode);
                return Err(DeviceError::NotPresentOnDevice);
            }
        }

        self.next_exchange += 1;
        let frames: Vec<Vec<u8>> = exchange
            .reply_frames
            .iter()
            .filter_map(|hex| decode_hex(hex))
            .collect();
        self.arm(frames, exchange.then);
        Ok(())
    }

    async fn next_notification(&mut self, _timeout: Duration) -> Result<Vec<u8>, DeviceError> {
        if self.disconnected {
            return Err(DeviceError::DeviceDisconnected);
        }
        if let Some(frame) = self.pending.pop_front() {
            return Ok(frame);
        }
        match self.after {
            AfterExchange::Disconnect => {
                self.disconnected = true;
                Err(DeviceError::DeviceDisconnected)
            }
            AfterExchange::Busy => Err(DeviceError::DeviceBusy),
            AfterExchange::PowerLow => Err(DeviceError::DevicePowerLow),
            // A script that simply runs out of frames is a device that stopped
            // answering: the caller's timeout is what must notice.
            AfterExchange::Continue | AfterExchange::Silence => {
                Err(DeviceError::TransferIncomplete)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        self.disconnected = true;
        self.pending.clear();
        Ok(())
    }
}

/// Builds wire scripts for parameterized hostile cases.
///
/// The declarative fixtures cover the canonical scripts; this builder covers
/// the families that need one scenario per chunk boundary, where writing
/// hundreds of fixture files would obscure rather than document.
#[derive(Debug, Clone)]
pub struct ScenarioBuilder {
    profile: &'static ProtocolProfile,
    name: String,
    device_model: Option<String>,
    firmware: Option<String>,
    battery_percent: Option<u8>,
    exchanges: Vec<Exchange>,
}

impl ScenarioBuilder {
    /// Start a script for a protocol profile.
    #[must_use]
    pub fn new(profile: &'static ProtocolProfile, name: impl Into<String>) -> Self {
        Self {
            profile,
            name: name.into(),
            device_model: Some(profile.device_model.to_owned()),
            firmware: profile
                .firmware_fingerprints
                .first()
                .map(|f| (*f).to_owned()),
            battery_percent: Some(80),
            exchanges: Vec::new(),
        }
    }

    /// Report a different model, for the wrong-device cases.
    #[must_use]
    pub fn reporting_model(mut self, model: Option<&str>) -> Self {
        self.device_model = model.map(str::to_owned);
        self
    }

    /// Report a different firmware, for the unknown-firmware cases.
    #[must_use]
    pub fn reporting_firmware(mut self, firmware: Option<&str>) -> Self {
        self.firmware = firmware.map(str::to_owned);
        self
    }

    /// Report a battery level.
    #[must_use]
    pub const fn with_battery(mut self, percent: Option<u8>) -> Self {
        self.battery_percent = percent;
        self
    }

    /// Answer the authenticate request with `status`.
    #[must_use]
    pub fn authenticating(mut self, status: AuthStatus) -> Self {
        let payload = AuthResponse { status, detail: 0 }.encode();
        let frame = ControlFrame::new(ReadOnlyOpcode::Authenticate, payload)
            .expect("an auth response fits a frame");
        self.exchanges.push(Exchange {
            expect: ExpectedRequest::Authenticate,
            reply_frames: vec![encode_hex(&frame.encode())],
            then: AfterExchange::Continue,
        });
        self
    }

    /// Answer the authenticate request by dropping the link.
    #[must_use]
    pub fn authenticating_then(mut self, status: AuthStatus, then: AfterExchange) -> Self {
        let payload = AuthResponse { status, detail: 0 }.encode();
        let frame = ControlFrame::new(ReadOnlyOpcode::Authenticate, payload)
            .expect("an auth response fits a frame");
        self.exchanges.push(Exchange {
            expect: ExpectedRequest::Authenticate,
            reply_frames: vec![encode_hex(&frame.encode())],
            then,
        });
        self
    }

    /// Answer the list request with `listing`.
    #[must_use]
    pub fn listing(mut self, listing: &DeltaListing) -> Self {
        let payload = listing
            .encode(self.profile)
            .expect("a listing inside the profile bounds encodes");
        let frames = bulk_script(
            &payload,
            usize::from(self.profile.chunk_payload_bytes),
            true,
        );
        self.exchanges.push(Exchange {
            expect: ExpectedRequest::ListDelta,
            reply_frames: frames,
            then: AfterExchange::Continue,
        });
        self
    }

    /// Answer a data request with `bytes`, chunked at the profile size.
    #[must_use]
    pub fn transferring(self, id: &str, bytes: &[u8]) -> Self {
        self.transferring_with(id, bytes, TransferFault::None)
    }

    /// Answer a data request with a deliberate fault.
    #[must_use]
    pub fn transferring_with(mut self, id: &str, bytes: &[u8], fault: TransferFault) -> Self {
        let chunk = usize::from(self.profile.chunk_payload_bytes);
        let mut frames = bulk_script(
            bytes,
            chunk,
            !matches!(fault, TransferFault::DropFinalFrame),
        );
        let mut then = AfterExchange::Continue;
        match fault {
            TransferFault::None | TransferFault::DropFinalFrame => {}
            TransferFault::DuplicateChunk { index } => {
                if let Some(frame) = frames.get(index).cloned() {
                    frames.insert(index + 1, frame);
                }
            }
            TransferFault::ConflictingChunk { index } => {
                if let Some(original) = frames.get(index).cloned() {
                    if let Some(mut raw) = decode_hex(&original) {
                        // Same offset, different payload.
                        if let Some(last) = raw.last_mut() {
                            *last = last.wrapping_add(1);
                        }
                        frames.insert(index + 1, encode_hex(&raw));
                    }
                }
            }
            TransferFault::OutOfOrder { index } => {
                if index + 1 < frames.len() {
                    frames.swap(index, index + 1);
                }
            }
            TransferFault::DisconnectAfter { frames: delivered } => {
                frames.truncate(delivered);
                then = AfterExchange::Disconnect;
            }
            TransferFault::ForgedLength => {
                if let Some(first) = frames.first_mut() {
                    if let Some(mut raw) = decode_hex(first) {
                        // Claim a payload far longer than the frame carries.
                        raw[5] = 0x0F;
                        raw[6] = 0xFF;
                        *first = encode_hex(&raw);
                    }
                }
            }
            TransferFault::ZeroProgressStall { repeats } => {
                let empty = BulkFrame::Chunk {
                    offset: 0,
                    payload: Vec::new(),
                };
                let hex = encode_hex(&empty.encode());
                frames = std::iter::repeat_n(hex, repeats).collect();
                then = AfterExchange::Silence;
            }
            TransferFault::PowerLow => {
                frames.clear();
                then = AfterExchange::PowerLow;
            }
        }
        self.exchanges.push(Exchange {
            expect: ExpectedRequest::RequestData {
                id: Some(id.to_owned()),
            },
            reply_frames: frames,
            then,
        });
        self
    }

    /// Finish the script.
    #[must_use]
    pub fn build(self) -> DeviceScenario {
        DeviceScenario {
            fixture_version: FIXTURE_VERSION,
            name: self.name,
            profile: self.profile.name.to_owned(),
            device_model: self.device_model,
            firmware: self.firmware,
            synthetic: true,
            provenance: "Synthesized by ScenarioBuilder from the protocol description in \
                         spec App. B.3. Contains no captured credential and no captured audio."
                .to_owned(),
            battery_percent: self.battery_percent,
            control_characteristic: None,
            radio_permission_denied: false,
            exchanges: self.exchanges,
        }
    }

    /// Finish and wrap in a simulator.
    ///
    /// # Panics
    ///
    /// Panics if the built scenario is invalid, which is a test-authoring bug.
    #[must_use]
    pub fn device(self) -> FakeDevice {
        FakeDevice::new(self.build()).expect("a built scenario is valid")
    }
}

/// A deliberate fault injected into a transfer script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferFault {
    /// A clean transfer.
    None,
    /// The terminal marker is never sent.
    DropFinalFrame,
    /// A chunk is repeated verbatim.
    DuplicateChunk {
        /// Index of the chunk to repeat.
        index: usize,
    },
    /// The same offset arrives twice with different bytes.
    ConflictingChunk {
        /// Index of the chunk to corrupt.
        index: usize,
    },
    /// Two neighbouring chunks are swapped.
    OutOfOrder {
        /// Index of the first of the pair.
        index: usize,
    },
    /// The link drops after `frames` frames.
    DisconnectAfter {
        /// Frames delivered before the drop.
        frames: usize,
    },
    /// The first frame declares a payload longer than it carries.
    ForgedLength,
    /// The device repeats an empty chunk forever.
    ZeroProgressStall {
        /// How many empty frames to send.
        repeats: usize,
    },
    /// The device reports a low battery instead of sending data.
    PowerLow,
}

/// Encode `bytes` as a bulk frame script.
fn bulk_script(bytes: &[u8], chunk: usize, terminate: bool) -> Vec<String> {
    let chunk = chunk.max(1);
    let mut frames: Vec<String> = bytes
        .chunks(chunk)
        .enumerate()
        .map(|(index, slice)| {
            let frame = BulkFrame::Chunk {
                offset: u32::try_from(index * chunk).unwrap_or(u32::MAX),
                payload: slice.to_vec(),
            };
            encode_hex(&frame.encode())
        })
        .collect();
    if terminate {
        frames.push(encode_hex(&BulkFrame::EndOfTransfer.encode()));
    }
    frames
}

#[cfg(test)]
mod tests {
    use plaudon_plaud_protocol::payload::{DeviceCodec, ListedRecording};
    use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;

    use super::*;

    fn listing() -> DeltaListing {
        DeltaListing {
            entries: vec![ListedRecording {
                id: "REC0001".into(),
                display_name: "REC0001.ASR".into(),
                declared_byte_length: 240,
                captured_at_unix: 1_760_000_000,
                codec: DeviceCodec::AsrOpus,
            }],
            truncated: false,
        }
    }

    #[tokio::test]
    async fn a_script_replays_the_same_bytes_every_run() {
        let payload = vec![0xAB; 200];
        let build = || {
            ScenarioBuilder::new(&LEGACY_NOTE_V0095, "happy")
                .authenticating(AuthStatus::Accepted)
                .listing(&listing())
                .transferring("REC0001", &payload)
                .build()
        };
        assert_eq!(build(), build(), "the script is deterministic");
    }

    #[tokio::test]
    async fn the_simulator_refuses_an_unscripted_request() {
        let mut device = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "auth-only")
            .authenticating(AuthStatus::Accepted)
            .device();

        let list = ControlFrame::new(ReadOnlyOpcode::ListDelta, Vec::new()).unwrap();
        assert_eq!(
            device.write_control(&list).await,
            Err(DeviceError::NotPresentOnDevice),
            "the first scripted exchange is authenticate"
        );
        assert_eq!(
            device.trace().unscripted_requests,
            vec![ReadOnlyOpcode::ListDelta]
        );
    }

    #[tokio::test]
    async fn every_write_the_simulator_sees_is_read_only() {
        let mut device = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "happy")
            .authenticating(AuthStatus::Accepted)
            .listing(&listing())
            .device();

        let auth = ControlFrame::new(ReadOnlyOpcode::Authenticate, vec![0u8; 16]).unwrap();
        device.write_control(&auth).await.unwrap();
        let list = ControlFrame::new(ReadOnlyOpcode::ListDelta, Vec::new()).unwrap();
        device.write_control(&list).await.unwrap();

        assert!(device.trace().is_read_only());
        assert_eq!(
            device.trace().opcode_names(),
            vec!["authenticate", "listDelta"]
        );
        assert_eq!(
            device.trace().written_characteristics,
            vec![LEGACY_NOTE_V0095.characteristic_control; 2],
            "the simulator only ever sees writes to the control characteristic"
        );
    }

    #[tokio::test]
    async fn a_disconnect_fault_stops_delivering_frames() {
        let payload = vec![0x11; 400];
        let mut device = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "disconnect")
            .transferring_with(
                "REC0001",
                &payload,
                TransferFault::DisconnectAfter { frames: 2 },
            )
            .device();

        let request = DataRequest {
            id: "REC0001".into(),
            offset: 0,
            length: 0,
        };
        let frame =
            ControlFrame::new(ReadOnlyOpcode::RequestData, request.encode().unwrap()).unwrap();
        device.write_control(&frame).await.unwrap();

        for _ in 0..2 {
            device
                .next_notification(Duration::from_secs(1))
                .await
                .unwrap();
        }
        assert_eq!(
            device.next_notification(Duration::from_secs(1)).await,
            Err(DeviceError::DeviceDisconnected)
        );
    }

    #[test]
    fn a_fixture_must_declare_itself_synthetic_with_provenance() {
        let mut scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "x").build();
        scenario.synthetic = false;
        assert_eq!(scenario.validate(), Err(ScenarioError::NotSynthetic));

        let mut scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "x").build();
        scenario.provenance = "  ".into();
        assert_eq!(scenario.validate(), Err(ScenarioError::NoProvenance));

        let mut scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "x").build();
        scenario.fixture_version = 99;
        assert_eq!(
            scenario.validate(),
            Err(ScenarioError::Version { found: 99 })
        );

        let mut scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "x").build();
        scenario.profile = "plaud-note-future".into();
        assert!(matches!(
            scenario.validate(),
            Err(ScenarioError::UnknownProfile { .. })
        ));
    }

    #[test]
    fn a_scenario_round_trips_through_json() {
        let scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "happy")
            .authenticating(AuthStatus::Accepted)
            .listing(&listing())
            .transferring("REC0001", &[0x22; 160])
            .build();
        let json = serde_json::to_string_pretty(&scenario).unwrap();
        assert_eq!(DeviceScenario::from_json(&json).unwrap(), scenario);
    }

    #[test]
    fn hex_decoding_is_whitespace_tolerant_and_total() {
        assert_eq!(decode_hex("01 02  03"), Some(vec![1, 2, 3]));
        assert_eq!(decode_hex("0102\n03"), Some(vec![1, 2, 3]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("zz"), None);
        assert_eq!(encode_hex(&[0x0a, 0xff]), "0aff");
    }

    #[tokio::test]
    async fn a_radio_denied_scenario_never_reaches_discovery() {
        let mut scenario = ScenarioBuilder::new(&LEGACY_NOTE_V0095, "denied").build();
        scenario.radio_permission_denied = true;
        let mut device = FakeDevice::new(scenario).unwrap();
        assert_eq!(
            device.discover().await,
            Err(DeviceError::RadioPermissionDenied)
        );
    }
}
