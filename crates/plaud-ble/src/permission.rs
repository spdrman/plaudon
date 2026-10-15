//! Why a radio may not be used, decided from what the host reports (#301).
//!
//! A real radio can refuse a scan for more than one reason: the user has not
//! answered the macOS Bluetooth prompt yet, has denied it, the adapter is
//! switched off, or there is no usable adapter at all. Each of those needs a
//! different sentence and a different fix, so none of them may be folded into
//! "permission denied" and none may look like an empty room.
//!
//! [`radio_gate`] is a pure function so the default build, which carries no
//! Bluetooth stack, can test the mapping. Reading the real authorization lives
//! behind `hardware-scan` in [`host_authorization`].

use plaudon_recording_sources::device::DeviceError;

/// What the operating system says about this application's Bluetooth access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothAuthorization {
    /// The user has not been asked, or has not answered yet.
    NotDetermined,
    /// The user refused access.
    Denied,
    /// A policy (parental controls, MDM) forbids access.
    Restricted,
    /// Access is allowed.
    Allowed,
}

/// The adapter's power state, as the radio stack reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterPower {
    /// Powered on and usable.
    On,
    /// Present but switched off.
    Off,
    /// The stack reported neither (unsupported, resetting, unauthorized).
    Unknown,
}

/// Decide whether a scan may proceed.
///
/// `power` is `None` when the host reported no adapter at all. Authorization
/// is checked first: an adapter's power state is not trustworthy, and often
/// not reported, before access is granted.
///
/// The scanner calls this *after* it has created the radio manager, because
/// on macOS creating the manager is what shows the Bluetooth prompt. Refusing
/// to touch the radio while the answer is "not determined" would mean the
/// prompt never appears, so only an explicit denial is checked up front
/// ([`refused_before_touching_the_radio`]).
pub fn radio_gate(
    authorization: BluetoothAuthorization,
    power: Option<AdapterPower>,
) -> Result<(), DeviceError> {
    match authorization {
        BluetoothAuthorization::NotDetermined => Err(DeviceError::RadioPermissionNotDetermined),
        BluetoothAuthorization::Denied | BluetoothAuthorization::Restricted => {
            Err(DeviceError::RadioPermissionDenied)
        }
        BluetoothAuthorization::Allowed => match power {
            Some(AdapterPower::On) => Ok(()),
            Some(AdapterPower::Off) => Err(DeviceError::RadioPoweredOff),
            Some(AdapterPower::Unknown) | None => Err(DeviceError::RadioNoAdapter),
        },
    }
}

/// An explicit refusal that makes touching the radio pointless.
///
/// Only a denial or a restriction counts: "not determined" must still reach
/// the radio stack, which is what asks the user.
pub fn refused_before_touching_the_radio(
    authorization: BluetoothAuthorization,
) -> Result<(), DeviceError> {
    match authorization {
        BluetoothAuthorization::Denied | BluetoothAuthorization::Restricted => {
            Err(DeviceError::RadioPermissionDenied)
        }
        BluetoothAuthorization::NotDetermined | BluetoothAuthorization::Allowed => Ok(()),
    }
}

/// The authorization the host reports for this process right now.
///
/// On macOS this is CoreBluetooth's class-level authorization, which reads the
/// TCC decision without creating a central manager (so it never prompts).
/// Windows and Linux have no per-application Bluetooth consent for a desktop
/// binary, so they report [`BluetoothAuthorization::Allowed`] and leave any
/// refusal to the radio stack itself.
#[cfg(feature = "hardware-scan")]
pub fn host_authorization() -> BluetoothAuthorization {
    #[cfg(target_os = "macos")]
    {
        use objc2_core_bluetooth::{CBManager, CBManagerAuthorization};
        // SAFETY: `+[CBManager authorization]` is a class property getter with
        // no arguments and no side effects, available since macOS 10.15, which
        // is below this crate's deployment target.
        #[allow(unsafe_code)]
        let raw = unsafe { CBManager::authorization_class() };
        match raw {
            CBManagerAuthorization::AllowedAlways => BluetoothAuthorization::Allowed,
            CBManagerAuthorization::Denied => BluetoothAuthorization::Denied,
            CBManagerAuthorization::Restricted => BluetoothAuthorization::Restricted,
            _ => BluetoothAuthorization::NotDetermined,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        BluetoothAuthorization::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_outranks_whatever_the_adapter_says() {
        for power in [
            None,
            Some(AdapterPower::On),
            Some(AdapterPower::Off),
            Some(AdapterPower::Unknown),
        ] {
            assert_eq!(
                radio_gate(BluetoothAuthorization::NotDetermined, power),
                Err(DeviceError::RadioPermissionNotDetermined)
            );
        }
    }

    #[test]
    fn an_unanswered_prompt_still_reaches_the_radio_so_the_prompt_can_appear() {
        assert_eq!(
            refused_before_touching_the_radio(BluetoothAuthorization::NotDetermined),
            Ok(())
        );
        assert_eq!(
            refused_before_touching_the_radio(BluetoothAuthorization::Allowed),
            Ok(())
        );
        for refused in [
            BluetoothAuthorization::Denied,
            BluetoothAuthorization::Restricted,
        ] {
            assert_eq!(
                refused_before_touching_the_radio(refused),
                Err(DeviceError::RadioPermissionDenied)
            );
        }
    }
}
