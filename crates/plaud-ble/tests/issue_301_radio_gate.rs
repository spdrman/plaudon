//! Why a scan produced nothing (#301), decided before a radio is touched.
//!
//! `scan_once` used to fold every reason a real radio can refuse (an
//! unanswered macOS prompt, a switched-off adapter, no adapter at all) into
//! `RadioPermissionDenied`. These tests pin the mapping from what the host
//! reports to the error the scan returns, without a radio: the mapping is a
//! pure function so the default build, and CI, can check it.

use plaudon_plaud_ble::permission::{radio_gate, AdapterPower, BluetoothAuthorization};
use plaudon_recording_sources::device::DeviceError;

#[test]
fn issue_301_an_unanswered_prompt_is_not_a_denial() {
    assert_eq!(
        radio_gate(BluetoothAuthorization::NotDetermined, None),
        Err(DeviceError::RadioPermissionNotDetermined)
    );
    assert_eq!(
        radio_gate(
            BluetoothAuthorization::NotDetermined,
            Some(AdapterPower::Unknown)
        ),
        Err(DeviceError::RadioPermissionNotDetermined)
    );
}

#[test]
fn issue_301_denied_and_restricted_are_denials() {
    for authorization in [
        BluetoothAuthorization::Denied,
        BluetoothAuthorization::Restricted,
    ] {
        for power in [None, Some(AdapterPower::On), Some(AdapterPower::Off)] {
            assert_eq!(
                radio_gate(authorization, power),
                Err(DeviceError::RadioPermissionDenied),
                "{authorization:?} with {power:?}"
            );
        }
    }
}

#[test]
fn issue_301_a_switched_off_adapter_is_powered_off() {
    assert_eq!(
        radio_gate(BluetoothAuthorization::Allowed, Some(AdapterPower::Off)),
        Err(DeviceError::RadioPoweredOff)
    );
}

#[test]
fn issue_301_no_adapter_or_an_unusable_one_is_no_adapter() {
    assert_eq!(
        radio_gate(BluetoothAuthorization::Allowed, None),
        Err(DeviceError::RadioNoAdapter)
    );
    assert_eq!(
        radio_gate(BluetoothAuthorization::Allowed, Some(AdapterPower::Unknown)),
        Err(DeviceError::RadioNoAdapter)
    );
}

#[test]
fn issue_301_an_allowed_powered_adapter_may_scan() {
    assert_eq!(
        radio_gate(BluetoothAuthorization::Allowed, Some(AdapterPower::On)),
        Ok(())
    );
}

#[test]
fn issue_301_each_refusal_maps_to_its_own_code() {
    use plaudon_domain::error::AppError;
    let codes: Vec<&str> = [
        DeviceError::RadioPermissionDenied,
        DeviceError::RadioPermissionNotDetermined,
        DeviceError::RadioPoweredOff,
        DeviceError::RadioNoAdapter,
    ]
    .into_iter()
    .map(|error| AppError::from(error).code().as_str())
    .collect();
    assert_eq!(
        codes,
        [
            "RADIO_PERMISSION_DENIED",
            "RADIO_PERMISSION_NOT_DETERMINED",
            "RADIO_POWERED_OFF",
            "RADIO_NO_ADAPTER",
        ]
    );
}
