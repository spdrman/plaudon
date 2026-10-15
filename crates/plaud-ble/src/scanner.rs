//! The opt-in hardware scanner (spec App. B.10 Spike D0).
//!
//! Compiled only with the `hardware-scan` feature, never in CI. It performs a
//! passive scan, applies [`crate::discovery::DiscoveryPolicy`], and reads the
//! standard battery characteristic of a user-selected candidate. It writes
//! nothing to the vendor control characteristic, so a scan cannot mutate a
//! device even by accident.

use std::time::Duration;

use btleplug::api::{Central, CentralState, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use plaudon_recording_sources::device::DeviceError;

use crate::discovery::{AdvertisementVerdict, Candidate, DiscoveryPolicy, ObservedAdvertisement};
use crate::permission::{
    host_authorization, radio_gate, refused_before_touching_the_radio, AdapterPower,
};

/// How long to wait for a freshly created manager to learn the adapter's
/// power state. CoreBluetooth reports "unknown" until its first state
/// callback, which normally arrives within a few hundred milliseconds.
const POWER_STATE_SETTLE: Duration = Duration::from_secs(2);

/// The adapter's power state, waiting briefly while the stack still says
/// "unknown".
async fn adapter_power(adapter: &btleplug::platform::Adapter) -> AdapterPower {
    let deadline = tokio::time::Instant::now() + POWER_STATE_SETTLE;
    loop {
        match adapter.adapter_state().await {
            Ok(CentralState::PoweredOn) => return AdapterPower::On,
            Ok(CentralState::PoweredOff) => return AdapterPower::Off,
            Ok(CentralState::Unknown) | Err(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return AdapterPower::Unknown;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The error for a radio that failed partway, decided by the same gate so a
/// late failure still names its real cause rather than "denied".
fn refusal(power: Option<AdapterPower>) -> DeviceError {
    match radio_gate(host_authorization(), power) {
        Err(error) => error,
        // Authorized and powered, yet the stack refused: the adapter is not
        // usable for a scan.
        Ok(()) => DeviceError::RadioNoAdapter,
    }
}

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
/// A radio that cannot be used is reported as an error, never as an empty
/// result, so the UI can tell "no devices nearby" apart from "we could not
/// look". Each refusal keeps its own cause (#301): an explicit denial, an
/// unanswered macOS prompt, a switched-off adapter and a missing adapter are
/// [`crate::permission::radio_gate`]'s four answers.
pub async fn scan_once(
    policy: &DiscoveryPolicy,
    duration: Duration,
) -> Result<ScanReport, DeviceError> {
    // A denial is final; asking the stack again would only fail later.
    refused_before_touching_the_radio(host_authorization())?;

    // Creating the manager is what shows the macOS prompt when it has not
    // been answered yet. A host with no Bluetooth service at all (no BlueZ on
    // Linux) fails here, which is "no usable adapter".
    let manager = Manager::new().await.map_err(|_| refusal(None))?;
    let adapter = match manager.adapters().await {
        Ok(adapters) => adapters.into_iter().next(),
        Err(_) => None,
    };
    let Some(adapter) = adapter else {
        return Err(refusal(None));
    };
    let power = adapter_power(&adapter).await;
    radio_gate(host_authorization(), Some(power))?;

    let adapter_name = adapter
        .adapter_info()
        .await
        .unwrap_or_else(|_| "unknown adapter".to_owned());

    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(|_| refusal(Some(power)))?;
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
