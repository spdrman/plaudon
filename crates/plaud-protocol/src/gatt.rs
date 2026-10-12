//! Vendor GATT identifiers observed for the legacy profile (spec App. B.3).
//!
//! Characteristics are always located by UUID. A fixed ATT handle is never
//! used: handles are assigned per connection and differ between hosts, so
//! caching one is both wrong and a way to write to the wrong characteristic.

/// Vendor service.
pub const SERVICE_VENDOR: u16 = 0x1910;

/// Characteristic the host writes control frames to.
pub const CHARACTERISTIC_CONTROL: u16 = 0x2BB1;

/// Characteristic the device notifies on.
pub const CHARACTERISTIC_NOTIFY: u16 = 0x2BB0;

/// Standard battery level characteristic.
pub const CHARACTERISTIC_BATTERY: u16 = 0x2A19;

/// Whether the host is permitted to write to a characteristic.
///
/// Exactly one characteristic is writable, and only with an allowlisted
/// opcode.
#[must_use]
pub const fn is_writable(characteristic: u16) -> bool {
    characteristic == CHARACTERISTIC_CONTROL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_control_characteristic_is_writable() {
        assert!(is_writable(CHARACTERISTIC_CONTROL));
        for read_only in [
            CHARACTERISTIC_NOTIFY,
            CHARACTERISTIC_BATTERY,
            SERVICE_VENDOR,
            0x2A00,
        ] {
            assert!(
                !is_writable(read_only),
                "{read_only:#06x} must not be writable"
            );
        }
    }

    #[test]
    fn the_observed_identifiers_match_the_recorded_profile() {
        assert_eq!(SERVICE_VENDOR, 0x1910);
        assert_eq!(CHARACTERISTIC_CONTROL, 0x2BB1);
        assert_eq!(CHARACTERISTIC_NOTIFY, 0x2BB0);
        assert_eq!(CHARACTERISTIC_BATTERY, 0x2A19);
    }
}
