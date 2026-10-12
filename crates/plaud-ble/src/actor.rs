//! The single-owner device actor (spec §6.3A, §7.7, App. B.4, App. B.6).
//!
//! > "We will place BLE scanner/connection ownership in a dedicated Rust
//! > actor. Only one active command stream can own a device at a time, the UI
//! > cannot issue parallel GATT commands." (App. B.4)
//!
//! The actor owns the [`GattTransport`] outright; nothing else can reach it.
//! Callers hold a [`DeviceHandle`], which is `Clone + Send + Sync` and talks to
//! the actor over a depth-one channel: a second command arriving while one is
//! running is refused with [`DeviceError::DeviceBusy`] rather than interleaved.
//!
//! Three safety properties are structural rather than reviewed:
//!
//! * Only [`ControlFrame`] values reach the wire, so no mutating opcode can be
//!   sent (see [`crate::transport`]).
//! * A read is issued only when the [`SessionDriver`] permits it, so an
//!   unsupported profile or a failed authentication cannot be talked into a
//!   transfer.
//! * Transferred bytes are handed to a [`FrameSink`] frame by frame and are
//!   never accumulated by the actor, so memory is bounded by one frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_plaud_protocol::bulk::{BulkFrame, TransferOutcome};
use plaudon_plaud_protocol::frame::ControlFrame;
use plaudon_plaud_protocol::payload::{
    AuthResponse, AuthStatus, DataRequest, DeltaListing, ListedRecording,
};
use plaudon_plaud_protocol::profile::ProtocolProfile;
use plaudon_plaud_protocol::stream::StreamingAssembler;
use plaudon_plaud_protocol::ReadOnlyOpcode;
use plaudon_recording_sources::device::{DeviceError, DeviceSessionState};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::session::{SessionDriver, SessionEvent};
use crate::transport::{DeviceEvent, GattTransport, ProbeResult, TransferProgress};

/// Largest listing payload this build will assemble, in bytes.
pub const MAX_LISTING_BYTES: u64 = 64 * 1024;

/// How long one notification may take before the transfer is abandoned.
pub const DEFAULT_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Smallest interval between progress events.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Receives transferred frames as they arrive.
///
/// The implementation streams them to staging; the actor never buffers a
/// recording (spec App. B.4).
pub trait FrameSink: Send {
    /// Accept one bulk frame.
    fn accept(&mut self, frame: &BulkFrame) -> Result<(), DeviceError>;

    /// Bytes the sink has committed to staging so far.
    fn written(&self) -> u64;
}

/// Outcome of one transfer run by the actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferSummary {
    /// Bytes the sink accepted.
    pub received_bytes: u64,
    /// Whether the protocol's terminal marker arrived.
    pub terminal_marker_observed: bool,
}

/// A command for the device actor.
enum Command {
    Probe(oneshot::Sender<Result<ProbeResult, DeviceError>>),
    Authenticate {
        token: Vec<u8>,
        reply: oneshot::Sender<Result<(), DeviceError>>,
    },
    List(oneshot::Sender<Result<DeltaListing, DeviceError>>),
    Transfer {
        recording: Box<ListedRecording>,
        sink: Box<dyn FrameSink>,
        cancel: CancellationToken,
        reply: oneshot::Sender<Result<TransferSummary, DeviceError>>,
    },
    State(oneshot::Sender<DeviceSessionState>),
    Disconnect(oneshot::Sender<Result<(), DeviceError>>),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately coarse: an `Authenticate` command carries a credential,
        // which must never reach a log or a panic message.
        let name = match self {
            Self::Probe(_) => "Probe",
            Self::Authenticate { .. } => "Authenticate",
            Self::List(_) => "List",
            Self::Transfer { .. } => "Transfer",
            Self::State(_) => "State",
            Self::Disconnect(_) => "Disconnect",
        };
        f.write_str(name)
    }
}

/// A thread-safe handle to one device actor.
#[derive(Debug, Clone)]
pub struct DeviceHandle {
    commands: mpsc::Sender<Command>,
    busy: Arc<AtomicBool>,
}

impl DeviceHandle {
    /// Probe the device: discover characteristics and read what it reports.
    pub async fn probe(&self) -> Result<ProbeResult, DeviceError> {
        self.request(Command::Probe).await?
    }

    /// Authenticate with a device credential.
    ///
    /// The credential is moved in and dropped by the actor as soon as the
    /// frame is written; it is never stored on the handle or the actor.
    pub async fn authenticate(&self, token: Vec<u8>) -> Result<(), DeviceError> {
        self.request(|reply| Command::Authenticate { token, reply })
            .await?
    }

    /// List the recordings the device currently exposes.
    pub async fn list(&self) -> Result<DeltaListing, DeviceError> {
        self.request(Command::List).await?
    }

    /// Transfer one recording into `sink`.
    pub async fn transfer(
        &self,
        recording: ListedRecording,
        sink: Box<dyn FrameSink>,
        cancel: CancellationToken,
    ) -> Result<TransferSummary, DeviceError> {
        self.request(|reply| Command::Transfer {
            recording: Box::new(recording),
            sink,
            cancel,
            reply,
        })
        .await?
    }

    /// Current session state.
    pub async fn state(&self) -> Result<DeviceSessionState, DeviceError> {
        self.request(Command::State).await
    }

    /// Drop the link.
    pub async fn disconnect(&self) -> Result<(), DeviceError> {
        self.request(Command::Disconnect).await?
    }

    /// Whether the actor is currently running a command.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, DeviceError> {
        if self.busy.load(Ordering::SeqCst) {
            return Err(DeviceError::DeviceBusy);
        }
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(build(reply))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DeviceError::DeviceBusy,
                mpsc::error::TrySendError::Closed(_) => DeviceError::DeviceDisconnected,
            })?;
        response.await.map_err(|_| DeviceError::DeviceDisconnected)
    }
}

/// The actor itself.
#[derive(Debug)]
pub struct DeviceActor<T: GattTransport> {
    transport: T,
    profile: &'static ProtocolProfile,
    session: SessionDriver,
    probe: Option<ProbeResult>,
    events: mpsc::UnboundedSender<DeviceEvent>,
    notification_timeout: Duration,
}

impl<T: GattTransport + 'static> DeviceActor<T> {
    /// Create an actor over a connected transport.
    pub fn new(
        transport: T,
        profile: &'static ProtocolProfile,
        events: mpsc::UnboundedSender<DeviceEvent>,
    ) -> Self {
        Self {
            transport,
            profile,
            session: SessionDriver::new(),
            probe: None,
            events,
            notification_timeout: DEFAULT_NOTIFICATION_TIMEOUT,
        }
    }

    /// Override the per-notification timeout.
    #[must_use]
    pub const fn with_notification_timeout(mut self, timeout: Duration) -> Self {
        self.notification_timeout = timeout;
        self
    }

    /// Spawn the actor, returning the only handle to it.
    pub fn spawn(mut self) -> DeviceHandle {
        let (commands, mut inbox) = mpsc::channel::<Command>(1);
        let busy = Arc::new(AtomicBool::new(false));
        let actor_busy = Arc::clone(&busy);

        tokio::spawn(async move {
            while let Some(command) = inbox.recv().await {
                actor_busy.store(true, Ordering::SeqCst);
                let keep_running = self.handle(command).await;
                actor_busy.store(false, Ordering::SeqCst);
                if !keep_running {
                    break;
                }
            }
        });

        DeviceHandle { commands, busy }
    }

    async fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::Probe(reply) => {
                let result = self.run_probe().await;
                let _ = reply.send(result);
                true
            }
            Command::Authenticate { token, reply } => {
                let result = self.run_authenticate(&token).await;
                drop(token);
                let _ = reply.send(result);
                true
            }
            Command::List(reply) => {
                let result = self.run_list().await;
                let _ = reply.send(result);
                true
            }
            Command::Transfer {
                recording,
                mut sink,
                cancel,
                reply,
            } => {
                let result = self.run_transfer(&recording, sink.as_mut(), &cancel).await;
                let _ = reply.send(result);
                true
            }
            Command::State(reply) => {
                let _ = reply.send(self.session.state());
                true
            }
            Command::Disconnect(reply) => {
                let result = self.transport.disconnect().await;
                self.session.apply(&SessionEvent::Disconnected);
                let _ = self.events.send(DeviceEvent::Disconnected);
                let _ = reply.send(result);
                false
            }
        }
    }

    async fn run_probe(&mut self) -> Result<ProbeResult, DeviceError> {
        self.session.apply(&SessionEvent::ProbeRequested);
        let probe = match self.transport.discover().await {
            Ok(probe) => probe,
            Err(error) => {
                self.note(&error);
                return Err(error);
            }
        };
        if let Err(error) = probe.characteristics.validate(self.profile) {
            self.session.apply(&SessionEvent::ProfileIdentified {
                firmware_validated: false,
                auth_scheme: plaudon_recording_sources::device::AuthScheme::Unknown,
            });
            return Err(error);
        }
        // The profile was chosen by the compatibility gate before the actor
        // was built, so a device that now reports a different model or
        // firmware is refused rather than accommodated.
        let model_matches = probe
            .device_model
            .as_deref()
            .is_none_or(|model| self.profile.covers_model(model));
        let firmware_matches = probe
            .firmware
            .as_deref()
            .is_none_or(|firmware| self.profile.covers_firmware(firmware));
        if !model_matches || !firmware_matches {
            self.session.apply(&SessionEvent::ProfileIdentified {
                firmware_validated: false,
                auth_scheme: plaudon_recording_sources::device::AuthScheme::Unknown,
            });
            return Err(DeviceError::UnsupportedFirmware);
        }

        self.session.apply(&SessionEvent::ProfileIdentified {
            firmware_validated: true,
            auth_scheme: plaudon_recording_sources::device::AuthScheme::LegacyStaticToken,
        });
        self.probe = Some(probe.clone());
        let _ = self.events.send(DeviceEvent::Connected {
            probe: Box::new(probe.clone()),
        });
        Ok(probe)
    }

    async fn run_authenticate(&mut self, token: &[u8]) -> Result<(), DeviceError> {
        if self.probe.is_none() {
            return Err(DeviceError::AuthBootstrapRequired);
        }
        if !self.profile.auth_mode_is_implemented() {
            self.session.apply(&SessionEvent::AuthRejected);
            return Err(DeviceError::UnsupportedFirmware);
        }
        let payload = plaudon_plaud_protocol::payload::encode_authenticate(self.profile, token)
            .map_err(|_| DeviceError::AuthRejected)?;
        let frame = ControlFrame::new(ReadOnlyOpcode::Authenticate, payload)
            .map_err(|_| DeviceError::AuthRejected)?;
        self.transport.write_control(&frame).await?;

        let bytes = self
            .transport
            .next_notification(self.notification_timeout)
            .await?;
        let response = ControlFrame::decode(&bytes).map_err(|_| DeviceError::AuthRejected)?;
        if response.opcode != ReadOnlyOpcode::Authenticate {
            return Err(DeviceError::AuthRejected);
        }
        let decoded =
            AuthResponse::decode(&response.payload).map_err(|_| DeviceError::AuthRejected)?;
        match decoded.status {
            AuthStatus::Accepted => {
                self.session.apply(&SessionEvent::Authenticated);
                let _ = self.events.send(DeviceEvent::Authenticated);
                Ok(())
            }
            AuthStatus::Rejected => {
                self.session.apply(&SessionEvent::AuthRejected);
                Err(DeviceError::AuthRejected)
            }
            AuthStatus::EncryptedModeRequired => {
                // Spec §7.9, App. B.5: fail closed, never downgrade, never
                // retry the legacy handshake.
                self.session.apply(&SessionEvent::AuthRejected);
                Err(DeviceError::UnsupportedFirmware)
            }
        }
    }

    async fn run_list(&mut self) -> Result<DeltaListing, DeviceError> {
        if self.session.state() != DeviceSessionState::Authenticated
            && self.session.state() != DeviceSessionState::Available
        {
            return Err(DeviceError::AuthBootstrapRequired);
        }
        self.session.apply(&SessionEvent::EnumerationStarted);

        let frame = ControlFrame::new(ReadOnlyOpcode::ListDelta, Vec::new())
            .map_err(|_| DeviceError::TransferIncomplete)?;
        self.transport.write_control(&frame).await?;

        let mut assembler = StreamingAssembler::with_budget(MAX_LISTING_BYTES);
        let mut payload = Vec::new();
        loop {
            let bytes = match self
                .transport
                .next_notification(self.notification_timeout)
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.note(&error);
                    return Err(error);
                }
            };
            let bulk = BulkFrame::decode(&bytes).map_err(|_| DeviceError::TransferIncomplete)?;
            let step = assembler
                .accept(&bulk)
                .map_err(|_| DeviceError::TransferIncomplete)?;
            payload.extend_from_slice(&step.ready);
            if assembler.ended() {
                if matches!(step.outcome, TransferOutcome::IncompleteAtEnd { .. }) {
                    return Err(DeviceError::TransferIncomplete);
                }
                break;
            }
        }

        let listing = DeltaListing::decode(self.profile, &payload)
            .map_err(|_| DeviceError::TransferIncomplete)?;
        if listing.entries.len() > self.profile.max_listed_recordings {
            return Err(DeviceError::TransferIncomplete);
        }
        self.session.apply(&SessionEvent::EnumerationSucceeded);
        let _ = self.events.send(DeviceEvent::Listed {
            count: listing.entries.len(),
            truncated: listing.truncated,
            delta_only: self.profile.listing_is_delta_only,
        });
        Ok(listing)
    }

    async fn run_transfer(
        &mut self,
        recording: &ListedRecording,
        sink: &mut dyn FrameSink,
        cancel: &CancellationToken,
    ) -> Result<TransferSummary, DeviceError> {
        if !self.session.may_read() {
            return Err(DeviceError::AuthBootstrapRequired);
        }
        if recording.declared_byte_length > self.profile.max_transfer_bytes {
            return Err(DeviceError::TransferIncomplete);
        }
        self.session.apply(&SessionEvent::TransferStarted);

        let request = DataRequest {
            id: recording.id.clone(),
            offset: 0,
            length: 0,
        };
        let payload = request
            .encode()
            .map_err(|_| DeviceError::NotPresentOnDevice)?;
        let frame = ControlFrame::new(ReadOnlyOpcode::RequestData, payload)
            .map_err(|_| DeviceError::TransferIncomplete)?;
        self.transport.write_control(&frame).await?;

        let started = Instant::now();
        let mut last_event = Instant::now();
        let declared =
            (recording.declared_byte_length > 0).then_some(recording.declared_byte_length);
        let terminal;

        loop {
            if cancel.is_cancelled() {
                self.session.apply(&SessionEvent::Disconnected);
                return Err(DeviceError::Cancelled);
            }
            let bytes = match self
                .transport
                .next_notification(self.notification_timeout)
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.note(&error);
                    return Err(error);
                }
            };
            let bulk = BulkFrame::decode(&bytes).map_err(|_| DeviceError::TransferIncomplete)?;
            sink.accept(&bulk)?;

            if matches!(bulk, BulkFrame::EndOfTransfer) {
                terminal = true;
                break;
            }
            if last_event.elapsed() >= PROGRESS_INTERVAL {
                last_event = Instant::now();
                let _ = self
                    .events
                    .send(DeviceEvent::TransferProgress(TransferProgress::new(
                        sink.written(),
                        declared,
                        started.elapsed(),
                    )));
            }
        }

        self.session.apply(&SessionEvent::TransferEnded);
        let received_bytes = sink.written();
        let _ = self
            .events
            .send(DeviceEvent::TransferEnded { received_bytes });
        Ok(TransferSummary {
            received_bytes,
            terminal_marker_observed: terminal,
        })
    }

    fn note(&mut self, error: &DeviceError) {
        let event = match error {
            DeviceError::DeviceDisconnected => SessionEvent::Disconnected,
            DeviceError::DeviceBusy => SessionEvent::Busy,
            DeviceError::RadioPermissionDenied => SessionEvent::PermissionDenied,
            _ => return,
        };
        self.session.apply(&event);
        if matches!(error, DeviceError::DeviceDisconnected) {
            let _ = self.events.send(DeviceEvent::Disconnected);
        }
    }
}
