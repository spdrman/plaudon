//! A deterministic in-memory [`DirectDevice`].

use std::sync::Mutex;

use async_trait::async_trait;
use plaudon_domain::ids::DeviceRecordingId;
use plaudon_domain::media::MediaIdentity;
use plaudon_recording_sources::device::{
    DeviceCapabilities, DeviceError, DeviceRecording, DirectDevice, VerifiedTransfer,
};
use plaudon_recording_sources::sink::AsyncWriteSink;
use tokio_util::sync::CancellationToken;

/// A canned on-device recording.
#[derive(Debug, Clone)]
pub struct FakeDeviceRecording {
    /// The listing entry.
    pub listing: DeviceRecording,
    /// Bytes the device streams, split into transfer chunks.
    pub chunks: Vec<Vec<u8>>,
    /// Whether the device emits its terminal marker at the end.
    pub emits_terminal_marker: bool,
}

/// A read-only device that streams canned chunks.
///
/// The fake has no mutating operation at all, which is the point: the
/// acquisition API cannot express a delete, clear, settings-write or OTA
/// command (spec 7.9).
#[derive(Debug)]
pub struct FakeDevice {
    capabilities: Result<DeviceCapabilities, DeviceError>,
    recordings: Vec<FakeDeviceRecording>,
    fail_after_chunk: Mutex<Option<usize>>,
}

impl FakeDevice {
    /// Create a device exposing `recordings`.
    #[must_use]
    pub fn new(
        capabilities: Result<DeviceCapabilities, DeviceError>,
        recordings: Vec<FakeDeviceRecording>,
    ) -> Self {
        Self { capabilities, recordings, fail_after_chunk: Mutex::new(None) }
    }

    /// Simulate a disconnect after `chunk_count` chunks have been written.
    pub fn disconnect_after(&self, chunk_count: usize) {
        *self.fail_after_chunk.lock().expect("fake device lock") = Some(chunk_count);
    }
}

#[async_trait]
impl DirectDevice for FakeDevice {
    async fn capabilities(&self) -> Result<DeviceCapabilities, DeviceError> {
        self.capabilities.clone()
    }

    async fn list_available(&self) -> Result<Vec<DeviceRecording>, DeviceError> {
        let caps = self.capabilities.clone()?;
        if !caps.is_selectable() {
            return Err(DeviceError::UnsupportedFirmware);
        }
        Ok(self.recordings.iter().map(|entry| entry.listing.clone()).collect())
    }

    async fn read_to_staging(
        &self,
        id: &DeviceRecordingId,
        sink: &mut (dyn AsyncWriteSink + Send),
        cancel: &CancellationToken,
    ) -> Result<VerifiedTransfer, DeviceError> {
        let caps = self.capabilities.clone()?;
        if !caps.is_selectable() {
            return Err(DeviceError::UnsupportedFirmware);
        }
        let entry = self
            .recordings
            .iter()
            .find(|candidate| candidate.listing.id == *id)
            .ok_or(DeviceError::NotPresentOnDevice)?;

        let disconnect_after = *self.fail_after_chunk.lock().expect("fake device lock");
        let mut all = Vec::new();
        for (index, chunk) in entry.chunks.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(DeviceError::Cancelled);
            }
            if disconnect_after == Some(index) {
                return Err(DeviceError::DeviceDisconnected);
            }
            sink.write_all(chunk).await.map_err(|_| DeviceError::TransferIncomplete)?;
            all.extend_from_slice(chunk);
        }
        if cancel.is_cancelled() {
            return Err(DeviceError::Cancelled);
        }
        sink.flush().await.map_err(|_| DeviceError::TransferIncomplete)?;

        Ok(VerifiedTransfer {
            device_recording_id: *id,
            identity: MediaIdentity::of_bytes(&all, "opus"),
            quality_class: caps.quality_class,
            duration_ms: None,
            terminal_marker_observed: entry.emits_terminal_marker,
            device_declared_byte_length: entry.listing.declared_byte_length,
        })
    }
}
