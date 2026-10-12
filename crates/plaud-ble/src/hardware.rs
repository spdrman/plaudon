//! The real radio transport (spec §7.7, App. B.4). Opt-in only.
//!
//! Compiled behind the `hardware-scan` feature, excluded from the default
//! build and from CI, so a machine without a Bluetooth stack still builds and
//! tests the whole workspace. It implements [`GattTransport`] over
//! `btleplug`, which supports macOS and Windows centrals — a fact about the
//! crate, not evidence that the PLAUD protocol succeeds there (spec §2.5A).
//!
//! Characteristics are resolved by UUID on every connection. A fixed ATT
//! handle is never cached: handles are per connection and differ between
//! hosts, so reusing one risks writing to the wrong characteristic
//! (spec App. B.3).

use std::time::Duration;

use async_trait::async_trait;
use btleplug::api::{CharPropFlags, Characteristic, Peripheral as _, WriteType};
use btleplug::platform::Peripheral;
use futures::StreamExt;
use plaudon_plaud_protocol::frame::ControlFrame;
use plaudon_plaud_protocol::profile::ProtocolProfile;
use plaudon_recording_sources::device::DeviceError;
use uuid::Uuid;

use crate::transport::{Characteristics, GattTransport, ProbeResult};

/// Build the 128-bit form of a 16-bit Bluetooth identifier.
#[must_use]
pub fn long_uuid(short: u16) -> Uuid {
    const BASE: u128 = 0x0000_0000_0000_1000_8000_0080_5F9B_34FB;
    Uuid::from_u128(BASE | (u128::from(short) << 96))
}

/// A connected peripheral speaking one named profile.
pub struct RadioTransport {
    peripheral: Peripheral,
    profile: &'static ProtocolProfile,
    control: Option<Characteristic>,
    notify: Option<Characteristic>,
    battery: Option<Characteristic>,
    notifications: Option<
        std::pin::Pin<Box<dyn futures::Stream<Item = btleplug::api::ValueNotification> + Send>>,
    >,
}

impl std::fmt::Debug for RadioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RadioTransport")
            .field("profile", &self.profile.name)
            .field("connected", &self.control.is_some())
            .finish_non_exhaustive()
    }
}

impl RadioTransport {
    /// Wrap a peripheral the user selected.
    ///
    /// Connecting happens in [`GattTransport::discover`], never on
    /// construction: spec §7.9 forbids auto-connecting on an advertisement.
    #[must_use]
    pub const fn new(peripheral: Peripheral, profile: &'static ProtocolProfile) -> Self {
        Self {
            peripheral,
            profile,
            control: None,
            notify: None,
            battery: None,
            notifications: None,
        }
    }

    fn find(characteristics: &[Characteristic], short: u16) -> Option<Characteristic> {
        let wanted = long_uuid(short);
        characteristics
            .iter()
            .find(|characteristic| characteristic.uuid == wanted)
            .cloned()
    }
}

#[async_trait]
impl GattTransport for RadioTransport {
    async fn discover(&mut self) -> Result<ProbeResult, DeviceError> {
        self.peripheral
            .connect()
            .await
            .map_err(|_| DeviceError::DeviceBusy)?;
        self.peripheral
            .discover_services()
            .await
            .map_err(|_| DeviceError::DeviceDisconnected)?;

        let characteristics: Vec<Characteristic> =
            self.peripheral.characteristics().into_iter().collect();
        let control = Self::find(&characteristics, self.profile.characteristic_control)
            .ok_or(DeviceError::UnsupportedFirmware)?;
        let notify = Self::find(&characteristics, self.profile.characteristic_notify)
            .ok_or(DeviceError::UnsupportedFirmware)?;
        if !control
            .properties
            .intersects(CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE)
        {
            return Err(DeviceError::UnsupportedFirmware);
        }
        if !notify
            .properties
            .intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE)
        {
            return Err(DeviceError::UnsupportedFirmware);
        }
        let battery = Self::find(&characteristics, self.profile.characteristic_battery);

        self.peripheral
            .subscribe(&notify)
            .await
            .map_err(|_| DeviceError::DeviceDisconnected)?;
        let stream = self
            .peripheral
            .notifications()
            .await
            .map_err(|_| DeviceError::DeviceDisconnected)?;
        self.notifications = Some(Box::pin(stream));

        let battery_percent = match &battery {
            Some(characteristic) => self
                .peripheral
                .read(characteristic)
                .await
                .ok()
                .and_then(|bytes| bytes.first().copied())
                .filter(|percent| *percent <= 100),
            None => None,
        };

        self.control = Some(control);
        self.notify = Some(notify);
        self.battery = battery;

        Ok(ProbeResult {
            // The legacy profile carries no model or firmware report we have
            // validated, so we claim neither rather than guessing from the
            // advertised name (spec §7.7).
            device_model: None,
            firmware: None,
            characteristics: Characteristics {
                control: self.profile.characteristic_control,
                notify: self.profile.characteristic_notify,
                battery: self
                    .battery
                    .as_ref()
                    .map(|_| self.profile.characteristic_battery),
            },
            battery_percent,
        })
    }

    async fn write_control(&mut self, frame: &ControlFrame) -> Result<(), DeviceError> {
        let control = self
            .control
            .as_ref()
            .ok_or(DeviceError::DeviceDisconnected)?;
        self.peripheral
            .write(control, &frame.encode(), WriteType::WithResponse)
            .await
            .map_err(|_| DeviceError::DeviceDisconnected)
    }

    async fn next_notification(&mut self, timeout: Duration) -> Result<Vec<u8>, DeviceError> {
        let stream = self
            .notifications
            .as_mut()
            .ok_or(DeviceError::DeviceDisconnected)?;
        match tokio::time::timeout(timeout, stream.next()).await {
            Ok(Some(notification)) => Ok(notification.value),
            Ok(None) => Err(DeviceError::DeviceDisconnected),
            Err(_) => Err(DeviceError::TransferIncomplete),
        }
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        self.notifications = None;
        self.control = None;
        self.notify = None;
        let _ = self.peripheral.disconnect().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_identifiers_expand_to_the_bluetooth_base_uuid() {
        assert_eq!(
            long_uuid(0x2BB1).to_string(),
            "00002bb1-0000-1000-8000-00805f9b34fb"
        );
        assert_eq!(
            long_uuid(0x2A19).to_string(),
            "00002a19-0000-1000-8000-00805f9b34fb"
        );
    }
}
