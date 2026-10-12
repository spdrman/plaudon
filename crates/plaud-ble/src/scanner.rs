//! The opt-in hardware scanner (spec App. B.10 Spike D0).
//!
//! Compiled only with the `hardware-scan` feature, never in CI. It performs a
//! passive scan, applies [`crate::discovery::DiscoveryPolicy`], and reads the
//! standard battery characteristic of a user-selected candidate. It writes
//! nothing to the vendor control characteristic, so a scan cannot mutate a
//! device even by accident.

use std::time::Duration;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use plaudon_recording_sources::device::DeviceError;

use crate::discovery::{AdvertisementVerdict, Candidate, DiscoveryPolicy, ObservedAdvertisement};

/// Result of one scan pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    /// Host adapter identifier, as the platform reports it.
    pub adapter: String,
    /// Every advertisement seen, before policy.
    pub observed: Vec<ObservedAdvertisement>,
    /// Advertisements that passed policy.
    pub candidates: Vec<Candidate>,
}

/// Perform one read-only scan pass.
///
/// A missing or unauthorized radio is reported as
/// [`DeviceError::RadioPermissionDenied`] rather than as an empty result, so
/// the UI can tell "no devices nearby" apart from "we were not allowed to
/// look".
pub async fn scan_once(
    policy: &DiscoveryPolicy,
    duration: Duration,
) -> Result<ScanReport, DeviceError> {
    let manager = Manager::new()
        .await
        .map_err(|_| DeviceError::RadioPermissionDenied)?;
    let adapters = manager
        .adapters()
        .await
        .map_err(|_| DeviceError::RadioPermissionDenied)?;
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or(DeviceError::RadioPermissionDenied)?;
    let adapter_name = adapter
        .adapter_info()
        .await
        .unwrap_or_else(|_| "unknown adapter".to_owned());

    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(|_| DeviceError::RadioPermissionDenied)?;
    tokio::time::sleep(duration).await;
    let peripherals = adapter
        .peripherals()
        .await
        .map_err(|_| DeviceError::DeviceDisconnected)?;
    let _ = adapter.stop_scan().await;

    let mut observed = Vec::new();
    let mut candidates = Vec::new();
    for peripheral in peripherals {
        let Ok(Some(properties)) = peripheral.properties().await else {
            continue;
        };
        let advertisement = ObservedAdvertisement {
            peripheral_id: peripheral.id().to_string(),
            local_name: properties.local_name.clone(),
            service_ids: properties
                .services
                .iter()
                .filter_map(|uuid| short_uuid(*uuid))
                .collect(),
            manufacturer_ids: properties.manufacturer_data.keys().copied().collect(),
            rssi: properties.rssi,
        };
        if let AdvertisementVerdict::Candidate(candidate) = policy.evaluate(&advertisement) {
            candidates.push(candidate);
        }
        observed.push(advertisement);
    }

    Ok(ScanReport {
        adapter: adapter_name,
        observed,
        candidates,
    })
}

/// Extract the 16-bit form of a Bluetooth base UUID.
fn short_uuid(uuid: uuid::Uuid) -> Option<u16> {
    const BASE_SUFFIX: u128 = 0x0000_0000_0000_1000_8000_0080_5F9B_34FB;
    let value = uuid.as_u128();
    if value & 0x0000_0000_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF != BASE_SUFFIX {
        return None;
    }
    u16::try_from((value >> 96) & 0xFFFF).ok()
}
