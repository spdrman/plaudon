//! What the user can do about a radio that refused a scan (#301).
//!
//! One table, shared by every screen that shows a radio state: which
//! [`RadioState`] a radio error code stands for, whether a system settings
//! pane can help, and the fixed deep link that opens it. The links are
//! compiled-in constants and the only input is which pane, so nothing a user
//! or a device sends can reach the opener through here.

use plaudon_domain::error::AppErrorCode;

use crate::dto::RadioState;

/// macOS: System Settings → Privacy & Security → Bluetooth.
pub const MACOS_BLUETOOTH_PRIVACY_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth";

/// Windows: Settings → Bluetooth & devices.
pub const WINDOWS_BLUETOOTH_SETTINGS_URL: &str = "ms-settings:bluetooth";

/// A system settings pane the application may ask the host to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemSettingsPane {
    /// Where Bluetooth access and the adapter's power switch live: the
    /// Bluetooth row under Privacy & Security on macOS, the Bluetooth page on
    /// Windows.
    BluetoothPrivacy,
}

/// The settings application a pane is opened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHost {
    /// macOS System Settings.
    MacOs,
    /// Windows Settings.
    Windows,
    /// Anything else. A Linux desktop has no settings link every environment
    /// honours, so it gets no control rather than one that does nothing.
    Other,
}

impl SettingsHost {
    /// The host this binary was built for.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

impl SystemSettingsPane {
    /// The deep link that opens this pane on `host`, if it has one.
    #[must_use]
    pub const fn deep_link(self, host: SettingsHost) -> Option<&'static str> {
        match (self, host) {
            (Self::BluetoothPrivacy, SettingsHost::MacOs) => Some(MACOS_BLUETOOTH_PRIVACY_URL),
            (Self::BluetoothPrivacy, SettingsHost::Windows) => Some(WINDOWS_BLUETOOTH_SETTINGS_URL),
            (Self::BluetoothPrivacy, SettingsHost::Other) => None,
        }
    }

    /// The label of the control that opens this pane on `host`. It names the
    /// path through the settings application, so a user whose click does not
    /// land can still find the pane by hand. The separator is a plain `>`:
    /// the fonts the app bundles have no `→`, which rendered as empty boxes.
    #[must_use]
    pub const fn control_label(self, host: SettingsHost) -> Option<&'static str> {
        match (self, host) {
            (Self::BluetoothPrivacy, SettingsHost::MacOs) => {
                Some("Open System Settings > Privacy & Security > Bluetooth")
            }
            (Self::BluetoothPrivacy, SettingsHost::Windows) => {
                Some("Open Settings → Bluetooth & devices")
            }
            (Self::BluetoothPrivacy, SettingsHost::Other) => None,
        }
    }
}

/// The radio state a scan error code stands for, if it is a radio refusal.
#[must_use]
pub const fn radio_state_for(code: AppErrorCode) -> Option<RadioState> {
    match code {
        AppErrorCode::RadioPermissionDenied => Some(RadioState::PermissionDenied),
        AppErrorCode::RadioPermissionNotDetermined => Some(RadioState::PermissionNotDetermined),
        AppErrorCode::RadioPoweredOff => Some(RadioState::PoweredOff),
        AppErrorCode::RadioNoAdapter => Some(RadioState::NoAdapter),
        _ => None,
    }
}

/// The settings pane that can fix `state`, if any.
///
/// A build without a radio gets none: no setting gives a binary a Bluetooth
/// stack it was compiled without.
#[must_use]
pub const fn settings_pane_for(state: RadioState) -> Option<SystemSettingsPane> {
    match state {
        RadioState::PermissionDenied
        | RadioState::PermissionNotDetermined
        | RadioState::PoweredOff
        | RadioState::NoAdapter => Some(SystemSettingsPane::BluetoothPrivacy),
        RadioState::Scanned | RadioState::UnavailableInBuild => None,
    }
}

/// What the operating system says about this application's Bluetooth access
/// right now, or `None` when this build carries no Bluetooth stack and so
/// cannot ask.
#[cfg(feature = "hardware-scan")]
#[must_use]
pub fn bluetooth_authorization() -> Option<plaudon_plaud_ble::permission::BluetoothAuthorization> {
    Some(plaudon_plaud_ble::permission::host_authorization())
}

/// What the operating system says about this application's Bluetooth access
/// right now, or `None` when this build carries no Bluetooth stack and so
/// cannot ask.
#[cfg(not(feature = "hardware-scan"))]
#[must_use]
pub fn bluetooth_authorization() -> Option<plaudon_plaud_ble::permission::BluetoothAuthorization> {
    None
}
