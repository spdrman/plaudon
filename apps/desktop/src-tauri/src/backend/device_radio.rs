//! The production [`DeviceRadio`]: one `DeviceActor` per peripheral over a
//! pluggable link, and the byte path from the wire to the archive (spec §7.9,
//! §13.2, §4.2A.6–7, App. B.4).
//!
//! # The byte path
//!
//! ```text
//! GATT frames ──▶ StagingAcquisition (hash, framing scan, assembler)
//!                    │ writes every byte to a private staging file
//!                    ▼
//!                AcquisitionReport ── not verified ──▶ `TRANSFER_INCOMPLETE`,
//!                    │                                  row `interrupted`,
//!                    │ verified                         nothing archived
//!                    ▼
//!      derive_playable ──▶ second staging file (Ogg/Opus remux)
//!                    ▼
//!      DeviceImportCommitter: journal intent → rename into archive → commit,
//!      then the `device_transfers` row becomes `complete` / `verified`
//! ```
//!
//! The actor never buffers a recording; the committer refuses an unverified
//! report on its own, so the guarantee does not rest on this file. No step
//! here opens a network connection: a transfer is device → this computer.
//!
//! The link is obtained through a [`TransportFactory`]. The default build has
//! none; the `hardware-scan` cargo feature adds the `btleplug` factory, and
//! tests supply a scripted `FakeDevice`.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::time::Timestamp;
use plaudon_plaud_ble::discovery::ObservedAdvertisement;
use plaudon_plaud_ble::{
    DeviceActor, DeviceEvent, DeviceHandle, FrameSink, GattTransport, TransferProgress,
};
use plaudon_plaud_protocol::bulk::BulkFrame;
use plaudon_plaud_protocol::payload::DeltaListing;
use plaudon_plaud_protocol::profile::{profile_for, ProfileAuthMode, ProtocolProfile, PROFILES};
use plaudon_recording_sources::device::DeviceError;
use plaudon_security::secrets::SecretString;
use plaudon_source_plaud_device::{
    derivative_to_commit, derive_playable, CommitError, DeviceCommitRequest, DeviceImportCommitter,
    DeviceStaging, StagingAcquisition, TransferEvent,
};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

use crate::backend::devices::{
    DeviceRadio, RadioAvailability, RadioProbe, TransferReporter, TransferRequest, TransferUpdate,
};

/// How a link to one peripheral is obtained.
#[async_trait]
pub trait TransportFactory: Send + Sync + fmt::Debug {
    /// Whether a radio exists behind this factory.
    fn availability(&self) -> RadioAvailability;

    /// One passive scan. A denied radio is
    /// [`AppErrorCode::RadioPermissionDenied`], never an empty list.
    async fn scan(&self, timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError>;

    /// Connect to a peripheral for `profile`.
    async fn connect(
        &self,
        peripheral_id: &str,
        profile: &'static ProtocolProfile,
    ) -> Result<Box<dyn GattTransport>, AppError>;
}

/// The profile used to open the first link, before a probe has established
/// which profile the device speaks. It only decides which characteristic
/// UUIDs are resolved; whether the device may be used is the gate's decision.
fn discovery_profile() -> &'static ProtocolProfile {
    PROFILES[0]
}

struct Link {
    handle: DeviceHandle,
    events: Arc<AsyncMutex<mpsc::UnboundedReceiver<DeviceEvent>>>,
    profile: &'static ProtocolProfile,
    authenticated: bool,
    cancel: CancellationToken,
}

/// A [`GattTransport`] behind a `Box`, so the actor can own one.
struct BoxedTransport(Box<dyn GattTransport>);

#[async_trait]
impl GattTransport for BoxedTransport {
    async fn discover(&mut self) -> Result<plaudon_plaud_ble::ProbeResult, DeviceError> {
        self.0.discover().await
    }

    async fn write_control(
        &mut self,
        frame: &plaudon_plaud_protocol::frame::ControlFrame,
    ) -> Result<(), DeviceError> {
        self.0.write_control(frame).await
    }

    async fn next_notification(&mut self, timeout: Duration) -> Result<Vec<u8>, DeviceError> {
        self.0.next_notification(timeout).await
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        self.0.disconnect().await
    }
}

/// The acquisition, shared between the actor (which feeds it frames) and
/// [`ActorRadio::transfer`] (which finishes it once the actor is done).
type SharedAcquisition = Arc<Mutex<Option<StagingAcquisition<File>>>>;

struct SharedSink(SharedAcquisition);

impl FrameSink for SharedSink {
    fn accept(&mut self, frame: &BulkFrame) -> Result<(), DeviceError> {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let acquisition = slot.as_mut().ok_or(DeviceError::DeviceDisconnected)?;
        acquisition
            .accept(frame)
            .map(|_| ())
            .map_err(DeviceError::from)
    }

    fn written(&self) -> u64 {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or(0, StagingAcquisition::written)
    }
}

/// A real [`DeviceRadio`] over a [`TransportFactory`].
pub struct ActorRadio<F: TransportFactory> {
    factory: F,
    committer: DeviceImportCommitter,
    links: Mutex<HashMap<String, Link>>,
}

impl<F: TransportFactory> fmt::Debug for ActorRadio<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ActorRadio")
    }
}

impl<F: TransportFactory> ActorRadio<F> {
    /// A radio that commits what it transfers through `committer`.
    #[must_use]
    pub fn new(factory: F, committer: DeviceImportCommitter) -> Self {
        Self {
            factory,
            committer,
            links: Mutex::new(HashMap::new()),
        }
    }

    async fn open_link(
        &self,
        peripheral_id: &str,
        profile: &'static ProtocolProfile,
    ) -> Result<Link, AppError> {
        let transport = self.factory.connect(peripheral_id, profile).await?;
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let handle = DeviceActor::new(BoxedTransport(transport), profile, events_tx).spawn();
        Ok(Link {
            handle,
            events: Arc::new(AsyncMutex::new(events_rx)),
            profile,
            authenticated: false,
            cancel: CancellationToken::new(),
        })
    }

    fn handle_of(&self, peripheral_id: &str) -> Result<DeviceHandle, AppError> {
        self.links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(peripheral_id)
            .map(|link| link.handle.clone())
            .ok_or_else(|| AppError::from(DeviceError::DeviceDisconnected))
    }
}

fn auth_mode_name(profile: &ProtocolProfile) -> &'static str {
    match profile.auth_mode {
        ProfileAuthMode::LegacyStaticToken => "legacy_static_token",
        ProfileAuthMode::EncryptedHandshake => "encrypted_handshake",
    }
}

fn decode_token(token: &SecretString) -> Result<Vec<u8>, AppError> {
    hex::decode(token.expose()).map_err(|_| {
        AppError::new(
            AppErrorCode::AuthRejected,
            "the stored credential is not hex",
        )
    })
}

fn commit_error(error: &CommitError) -> AppError {
    match error {
        CommitError::NotVerified { reason } => AppError::new(
            AppErrorCode::TransferIncomplete,
            format!("the transfer was not verified: {reason}"),
        ),
        CommitError::Staging(_) => AppError::new(
            AppErrorCode::StorageUnavailable,
            "the staging file could not be written",
        ),
        CommitError::Persistence(_) => AppError::new(
            AppErrorCode::StorageUnavailable,
            "the archive refused the commit",
        ),
    }
}

#[async_trait]
impl<F: TransportFactory + 'static> DeviceRadio for ActorRadio<F> {
    fn availability(&self) -> RadioAvailability {
        self.factory.availability()
    }

    async fn scan(&self, timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError> {
        self.factory.scan(timeout).await
    }

    async fn probe(&self, peripheral_id: &str) -> Result<RadioProbe, AppError> {
        // Drop any earlier link: a probe is a fresh look at the device.
        self.disconnect(peripheral_id).await;
        let mut link = self.open_link(peripheral_id, discovery_profile()).await?;
        let probe = link.handle.probe().await.map_err(AppError::from)?;
        let model = probe.device_model.clone();
        let firmware = probe.firmware.clone();

        // Reconnect under the profile the device actually speaks, if it is a
        // different one. An unmatched device keeps the discovery link, which
        // the gate will refuse to use for anything.
        let matched = match (&model, &firmware) {
            (Some(model), Some(firmware)) => profile_for(model, firmware),
            _ => None,
        };
        if let Some(profile) = matched.filter(|profile| profile.name != link.profile.name) {
            let _ = link.handle.disconnect().await;
            link = self.open_link(peripheral_id, profile).await?;
            link.handle.probe().await.map_err(AppError::from)?;
        }

        self.links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(peripheral_id.to_owned(), link);
        Ok(RadioProbe {
            model,
            firmware,
            // The authentication mode is the profile's, never guessed for a
            // device no profile covers.
            auth_mode: matched.map(|profile| auth_mode_name(profile).to_owned()),
            battery_percent: probe.battery_percent,
            rssi_dbm: None,
        })
    }

    async fn list(
        &self,
        peripheral_id: &str,
        token: &SecretString,
    ) -> Result<DeltaListing, AppError> {
        let handle = self.handle_of(peripheral_id)?;
        let needs_auth = self
            .links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(peripheral_id)
            .is_some_and(|link| !link.authenticated);
        if needs_auth {
            let bytes = decode_token(token)?;
            handle.authenticate(bytes).await.map_err(AppError::from)?;
            if let Some(link) = self
                .links
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_mut(peripheral_id)
            {
                link.authenticated = true;
            }
        }
        handle.list().await.map_err(AppError::from)
    }

    async fn transfer(
        &self,
        request: TransferRequest<'_>,
        reporter: &dyn TransferReporter,
    ) -> Result<(), AppError> {
        let (handle, events, profile, cancel) = {
            let links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
            let link = links
                .get(request.peripheral_id)
                .ok_or_else(|| AppError::from(DeviceError::DeviceDisconnected))?;
            if !link.authenticated {
                return Err(AppError::from(DeviceError::AuthBootstrapRequired));
            }
            (
                link.handle.clone(),
                Arc::clone(&link.events),
                link.profile,
                link.cancel.clone(),
            )
        };
        let recording = request.recording;
        let recording_id = recording.id.clone();
        let step = |event| TransferUpdate::step(recording_id.clone(), event);

        let mut staging =
            DeviceStaging::open(self.committer.journal()).map_err(|e| commit_error(&e))?;
        let writer = staging
            .writer()
            .ok_or_else(|| AppError::new(AppErrorCode::InternalError, "staging has no writer"))?;
        let acquisition = StagingAcquisition::begin(profile, recording, writer)
            .map_err(|error| AppError::from(DeviceError::from(error)))?;
        let shared: SharedAcquisition = Arc::new(Mutex::new(Some(acquisition)));

        // The link is up and authenticated (a listing came first); walk the
        // ledger through those steps before the request is sent.
        for event in [
            TransferEvent::ConnectStarted,
            TransferEvent::LinkEstablished,
            TransferEvent::Authenticated,
            TransferEvent::RequestSent,
        ] {
            reporter.report(step(event)).await;
        }

        let started = Instant::now();
        let mut receiver = events.lock().await;
        while receiver.try_recv().is_ok() {}
        let sink: Box<dyn FrameSink> = Box::new(SharedSink(Arc::clone(&shared)));
        let run = handle.transfer(recording.clone(), sink, cancel);
        tokio::pin!(run);
        let outcome = loop {
            tokio::select! {
                result = &mut run => break result,
                Some(event) = receiver.recv() => {
                    if let DeviceEvent::TransferProgress(progress) = event {
                        reporter.report(progress_update(&recording_id, &progress, started)).await;
                    }
                }
            }
        };
        let mut last = None;
        while let Ok(event) = receiver.try_recv() {
            if let DeviceEvent::TransferProgress(progress) = event {
                last = Some(progress);
            }
        }
        drop(receiver);
        if let Some(progress) = last {
            reporter
                .report(progress_update(&recording_id, &progress, started))
                .await;
        }

        let acquisition = shared.lock().unwrap_or_else(PoisonError::into_inner).take();
        let report = acquisition.and_then(|acquisition| acquisition.finish().ok());
        let actor_error = outcome.err();

        let Some(report) = report else {
            // Nothing coherent was staged.
            let _ = staging.discard();
            reporter.report(step(TransferEvent::Disconnected)).await;
            return Err(actor_error.map_or_else(
                || AppError::from(DeviceError::TransferIncomplete),
                AppError::from,
            ));
        };

        // The final byte count always reaches the ledger, so `StreamEnded` is
        // legal even for a transfer shorter than one progress interval.
        let mut final_bytes = step(TransferEvent::BytesArriving);
        final_bytes.bytes_received = Some(report.verification.received_bytes);
        final_bytes.elapsed_ms = Some(elapsed_ms(started));
        reporter.report(final_bytes).await;

        let staged = staging.finish().map_err(|e| commit_error(&e))?;
        let verified = actor_error.is_none() && report.may_commit();
        if verified {
            reporter.report(step(TransferEvent::StreamEnded)).await;
            reporter
                .report(step(TransferEvent::VerificationSucceeded))
                .await;
        } else {
            reporter
                .report(step(TransferEvent::VerificationFailed))
                .await;
        }

        // Derivative: a playable Ogg/Opus remux of the verified sidecar,
        // staged separately. It is never labelled the original.
        let mut derivative_staging = None;
        let mut derivative = None;
        if verified {
            let mut dstage =
                DeviceStaging::open(self.committer.journal()).map_err(|e| commit_error(&e))?;
            if let Some(mut writer) = dstage.writer() {
                let mut source = File::open(&staged.path).map_err(|_| {
                    AppError::new(AppErrorCode::StorageUnavailable, "staging is not readable")
                })?;
                let outcome = derive_playable(&report, &mut source, &mut writer);
                drop(writer);
                derivative = Some(outcome);
            }
            derivative_staging = Some(dstage);
        }
        let derivative_staged = match derivative_staging {
            Some(stage) => Some(stage.finish().map_err(|e| commit_error(&e))?),
            None => None,
        };
        let to_commit = match (&derivative, &derivative_staged) {
            (Some(outcome), Some(staged)) => derivative_to_commit(outcome, staged),
            _ => None,
        };

        // The committer refuses an unverified report by itself and records the
        // row as `interrupted`; it is called either way so the ledger is
        // written in one place.
        let committed = self
            .committer
            .commit(
                DeviceCommitRequest {
                    staged: &staged,
                    report: &report,
                    derivative: to_commit,
                    transfer_id: request.transfer_id,
                    recording_id: None,
                    job_id: None,
                },
                Timestamp::now(),
            )
            .await;
        match committed {
            Ok(_) if actor_error.is_none() => {
                reporter.report(step(TransferEvent::CommitSucceeded)).await;
                Ok(())
            }
            Ok(_) => Err(actor_error.map_or_else(
                || AppError::from(DeviceError::TransferIncomplete),
                AppError::from,
            )),
            Err(error) => {
                let _ = std::fs::remove_file(&staged.path);
                if let Some(staged) = &derivative_staged {
                    let _ = std::fs::remove_file(&staged.path);
                }
                if verified {
                    reporter.report(step(TransferEvent::CommitFailed)).await;
                }
                Err(match (&error, actor_error) {
                    (CommitError::NotVerified { .. }, Some(actor)) => AppError::from(actor),
                    _ => commit_error(&error),
                })
            }
        }
    }

    async fn disconnect(&self, peripheral_id: &str) {
        let link = self
            .links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(peripheral_id);
        if let Some(link) = link {
            link.cancel.cancel();
            let _ = link.handle.disconnect().await;
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn progress_update(
    recording_id: &str,
    progress: &TransferProgress,
    started: Instant,
) -> TransferUpdate {
    TransferUpdate {
        recording_id: recording_id.to_owned(),
        event: TransferEvent::BytesArriving,
        bytes_received: Some(progress.received_bytes),
        elapsed_ms: Some(elapsed_ms(started)),
        bytes_per_second: progress.bytes_per_second,
        eta_seconds: progress.eta_seconds,
    }
}

#[cfg(feature = "hardware-scan")]
pub use hardware::BtleplugFactory;

/// The `btleplug` factory. Compiled only with the `hardware-scan` feature, so
/// the default build and CI never contain or touch a Bluetooth stack.
#[cfg(feature = "hardware-scan")]
mod hardware {
    use btleplug::api::{Central, Manager as _, Peripheral as _};
    use btleplug::platform::{Manager, Peripheral};
    use plaudon_plaud_ble::discovery::DiscoveryPolicy;
    use plaudon_plaud_ble::hardware::RadioTransport;

    use super::{
        async_trait, fmt, AppError, AppErrorCode, Duration, GattTransport, ObservedAdvertisement,
        ProtocolProfile, RadioAvailability, TransportFactory,
    };

    /// Scans and connects through the host's Bluetooth adapter.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct BtleplugFactory;

    impl fmt::Display for BtleplugFactory {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("btleplug")
        }
    }

    async fn find(peripheral_id: &str) -> Result<Peripheral, AppError> {
        let denied = || AppError::new(AppErrorCode::RadioPermissionDenied, "no usable adapter");
        let manager = Manager::new().await.map_err(|_| denied())?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|_| denied())?
            .into_iter()
            .next()
            .ok_or_else(denied)?;
        adapter
            .peripherals()
            .await
            .map_err(|_| denied())?
            .into_iter()
            .find(|peripheral| peripheral.id().to_string() == peripheral_id)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::DeviceDisconnected,
                    "the device is not in range",
                )
            })
    }

    #[async_trait]
    impl TransportFactory for BtleplugFactory {
        fn availability(&self) -> RadioAvailability {
            RadioAvailability::Present
        }

        async fn scan(&self, timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError> {
            let report =
                plaudon_plaud_ble::scanner::scan_once(&DiscoveryPolicy::default(), timeout)
                    .await
                    .map_err(AppError::from)?;
            Ok(report.observed)
        }

        async fn connect(
            &self,
            peripheral_id: &str,
            profile: &'static ProtocolProfile,
        ) -> Result<Box<dyn GattTransport>, AppError> {
            let peripheral = find(peripheral_id).await?;
            peripheral
                .connect()
                .await
                .map_err(|_| AppError::new(AppErrorCode::DeviceBusy, "connect failed"))?;
            Ok(Box::new(RadioTransport::new(peripheral, profile)))
        }
    }
}
