//! The cpal output device.
//!
//! Everything above this module is device-independent and tested against
//! [`crate::FakeAudioOut`]; this is the only file that talks to real
//! hardware, and it is deliberately thin: negotiate a config, convert the
//! mixer's `f32` into whatever the device wants, count played frames, and
//! record errors instead of panicking inside a callback.
//!
//! Structure follows filmcraft's `CpalOut` (`apps/filmcraft/src/audio.rs`,
//! MIT OR Apache-2.0 — see `NOTICE`): a played-frame counter as the master
//! clock, `catch_unwind` around the mixer, sample-format conversion with
//! clamping, and a health record fed by cpal's error callback.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::engine::{AudioOut, PlaybackError};
use crate::ring::FillCallback;

/// What cpal has reported about the current stream.
#[derive(Debug, Default)]
struct Health {
    errors: AtomicU64,
    fatal: AtomicBool,
    last: Mutex<Option<String>>,
}

impl Health {
    fn record(&self, error: &cpal::Error) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if matches!(
            error.kind(),
            cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable
        ) {
            self.fatal.store(true, Ordering::Release);
        }
        if let Ok(mut slot) = self.last.lock() {
            *slot = Some(error.to_string());
        }
    }
}

/// A cpal output stream.
pub struct CpalOut {
    device: cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    stream: Option<cpal::Stream>,
    played: Arc<AtomicU64>,
    health: Arc<Health>,
}

impl std::fmt::Debug for CpalOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpalOut")
            .field("sample_rate", &self.config.sample_rate)
            .field("channels", &self.config.channels)
            .field("sample_format", &self.sample_format)
            .field("running", &self.stream.is_some())
            .finish_non_exhaustive()
    }
}

impl CpalOut {
    /// Open the system's default output device.
    pub fn default_output() -> Result<Self, PlaybackError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlaybackError::Device("no default output device".to_owned()))?;
        Self::for_device(device)
    }

    /// Open a specific device.
    pub fn for_device(device: cpal::Device) -> Result<Self, PlaybackError> {
        let supported = device
            .default_output_config()
            .map_err(|error| PlaybackError::Device(error.to_string()))?;
        let sample_format = supported.sample_format();
        let config = supported.config();
        Ok(Self {
            device,
            config,
            sample_format,
            stream: None,
            played: Arc::new(AtomicU64::new(0)),
            health: Arc::new(Health::default()),
        })
    }

    /// Human-readable device name, when the host provides one.
    #[must_use]
    pub fn device_name(&self) -> Option<String> {
        self.device
            .description()
            .ok()
            .map(|description| description.name().to_owned())
    }

    /// The last error cpal reported, if any. A fatal one means the stream
    /// will not recover without being rebuilt.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.health.last.lock().ok().and_then(|slot| slot.clone())
    }

    /// Whether the device has gone away.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        self.health.fatal.load(Ordering::Acquire)
    }

    fn build<T>(&self, fill: FillCallback) -> Result<cpal::Stream, cpal::Error>
    where
        T: SizedSample + FromSample<f32>,
    {
        let channels = usize::from(self.config.channels).max(1);
        let played = Arc::clone(&self.played);
        let health = Arc::clone(&self.health);
        let error_health = Arc::clone(&self.health);
        let mut fill = fill;
        let mut scratch: Vec<f32> = Vec::new();
        self.device.build_output_stream::<T, _, _>(
            self.config,
            move |data: &mut [T], _info| {
                scratch.clear();
                scratch.resize(data.len(), 0.0);
                // A mixer panic becomes silence, never a torn-down process.
                let mixed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    fill(&mut scratch, channels);
                }))
                .is_ok();
                if !mixed {
                    scratch.fill(0.0);
                    health.errors.fetch_add(1, Ordering::Relaxed);
                }
                for (slot, sample) in data.iter_mut().zip(scratch.iter()) {
                    let value = if sample.is_finite() {
                        sample.clamp(-1.0, 1.0 - f32::EPSILON)
                    } else {
                        0.0
                    };
                    *slot = T::from_sample(value);
                }
                played.fetch_add((data.len() / channels) as u64, Ordering::Relaxed);
            },
            move |error| {
                error_health.record(&error);
                tracing::warn!(%error, "audio output stream error");
            },
            None,
        )
    }
}

impl AudioOut for CpalOut {
    fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    fn channels(&self) -> usize {
        usize::from(self.config.channels).max(1)
    }

    fn start(&mut self, fill: FillCallback) -> Result<(), PlaybackError> {
        self.stop();
        self.health.fatal.store(false, Ordering::Release);
        let stream = match self.sample_format {
            SampleFormat::F32 => self.build::<f32>(fill),
            SampleFormat::F64 => self.build::<f64>(fill),
            SampleFormat::I8 => self.build::<i8>(fill),
            SampleFormat::I16 => self.build::<i16>(fill),
            SampleFormat::I32 => self.build::<i32>(fill),
            SampleFormat::I64 => self.build::<i64>(fill),
            SampleFormat::U8 => self.build::<u8>(fill),
            SampleFormat::U16 => self.build::<u16>(fill),
            SampleFormat::U32 => self.build::<u32>(fill),
            SampleFormat::U64 => self.build::<u64>(fill),
            other => {
                return Err(PlaybackError::Device(format!(
                    "device wants an unsupported sample format: {other:?}"
                )))
            }
        }
        .map_err(|error| PlaybackError::Device(error.to_string()))?;
        stream
            .play()
            .map_err(|error| PlaybackError::Device(error.to_string()))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
            drop(stream);
        }
    }

    fn played_frames(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    fn is_running(&self) -> bool {
        self.stream.is_some() && !self.is_fatal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI containers have no sound card, so the only portable assertion is
    /// that opening reports a device error rather than panicking or hanging.
    /// Real playback is verified by hand and on MARS.
    #[test]
    fn opening_the_default_device_either_works_or_reports_why() {
        match CpalOut::default_output() {
            Ok(out) => {
                assert!(out.sample_rate() > 0);
                assert!(out.channels() >= 1);
                assert!(!out.is_running());
            }
            Err(PlaybackError::Device(message)) => assert_ne!(message, ""),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
