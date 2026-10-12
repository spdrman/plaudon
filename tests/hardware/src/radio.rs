//! The real radio backend (feature `radio`; spec §18.3A, App. B.4).
//!
//! Wraps `plaudon_plaud_ble::scanner::scan_once` for discovery and
//! `plaudon_plaud_ble::hardware::RadioTransport` for the link. Compiled only
//! with `--features radio`; the default build has no Bluetooth stack and the
//! gate keeps the radio untouched even when it is compiled in.

use std::time::Duration;

use async_trait::async_trait;
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use plaudon_plaud_ble::discovery::DiscoveryPolicy;
use plaudon_plaud_ble::hardware::RadioTransport;
use plaudon_plaud_ble::scanner::scan_once;
use plaudon_plaud_ble::GattTransport;
use plaudon_plaud_protocol::profile::ProtocolProfile;
use plaudon_recording_sources::device::DeviceError;

use crate::live::{DeviceBackend, ScanCandidate, ScanSummary};

/// How long to look for a selected peripheral before giving up.
const FIND_TIMEOUT: Duration = Duration::from_secs(15);

/// btleplug over CoreBluetooth (macOS) or WinRT (Windows).
#[derive(Debug)]
pub struct RadioBackend {
    policy: DiscoveryPolicy,
    profile: &'static ProtocolProfile,
    scan: Duration,
}

impl RadioBackend {
    /// Build a backend for `profile`.
    #[must_use]
    pub fn new(profile: &'static ProtocolProfile, scan: Duration) -> Self {
        Self {
            policy: DiscoveryPolicy {
                required_service_id: profile.service_vendor,
                min_rssi: None,
            },
            profile,
            scan,
        }
    }
}

#[async_trait]
impl DeviceBackend for RadioBackend {
    async fn scan(&mut self) -> Result<ScanSummary, DeviceError> {
        let report = scan_once(&self.policy, self.scan).await?;
        Ok(ScanSummary {
            adapter: report.adapter,
            advertisements: report.observed.len(),
            candidates: report
                .candidates
                .into_iter()
                .map(|candidate| ScanCandidate {
                    id: candidate.peripheral_id,
                    name: candidate.advertised_name,
                })
                .collect(),
        })
    }

    async fn connect(
        &mut self,
        peripheral_id: &str,
    ) -> Result<Box<dyn GattTransport>, DeviceError> {
        let manager = Manager::new()
            .await
            .map_err(|_| DeviceError::RadioPermissionDenied)?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|_| DeviceError::RadioPermissionDenied)?
            .into_iter()
            .next()
            .ok_or(DeviceError::RadioPermissionDenied)?;
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|_| DeviceError::RadioPermissionDenied)?;
        let deadline = tokio::time::Instant::now() + FIND_TIMEOUT;
        let found = loop {
            let peripherals = adapter
                .peripherals()
                .await
                .map_err(|_| DeviceError::DeviceDisconnected)?;
            if let Some(found) = peripherals
                .into_iter()
                .find(|peripheral| peripheral.id().to_string() == peripheral_id)
            {
                break Some(found);
            }
            if tokio::time::Instant::now() >= deadline {
                break None;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let _ = adapter.stop_scan().await;
        let peripheral = found.ok_or(DeviceError::DeviceDisconnected)?;
        Ok(Box::new(RadioTransport::new(peripheral, self.profile)))
    }
}
