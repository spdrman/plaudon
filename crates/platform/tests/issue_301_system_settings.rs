//! The System Settings opener behind the Bluetooth control (#301).
//!
//! The settings deep links are not web links, so they do not go through
//! `open_url` and its http/https/mailto allowlist: each pane is a fixed,
//! compiled-in URI, and the only input is which pane. Asserted for every
//! platform on every host, like the rest of the opener.

use plaudon_app::permissions::{
    SystemSettingsPane, MACOS_BLUETOOTH_PRIVACY_URL, WINDOWS_BLUETOOTH_SETTINGS_URL,
};
use plaudon_platform::opener::{system_settings_command, OpenError};
use plaudon_platform::Platform;

fn args(platform: Platform) -> (String, Vec<String>) {
    let spec = system_settings_command(platform, SystemSettingsPane::BluetoothPrivacy)
        .expect("a settings command for this platform");
    (
        spec.program.to_owned(),
        spec.args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
    )
}

#[test]
fn issue_301_macos_opens_privacy_and_security_bluetooth() {
    assert_eq!(
        args(Platform::MacOs),
        (
            "open".to_owned(),
            vec![MACOS_BLUETOOTH_PRIVACY_URL.to_owned()]
        )
    );
}

#[test]
fn issue_301_windows_opens_the_bluetooth_settings_page_without_a_shell() {
    assert_eq!(
        args(Platform::Windows),
        (
            "rundll32".to_owned(),
            vec![
                "url.dll,FileProtocolHandler".to_owned(),
                WINDOWS_BLUETOOTH_SETTINGS_URL.to_owned()
            ]
        )
    );
}

#[test]
fn issue_301_linux_says_it_has_no_settings_link() {
    assert!(matches!(
        system_settings_command(Platform::Linux, SystemSettingsPane::BluetoothPrivacy),
        Err(OpenError::UnsupportedPlatform { .. })
    ));
}
