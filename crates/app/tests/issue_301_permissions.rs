//! The one place that turns a radio refusal into "what the user can do about
//! it" (#301): which state a code is, whether a settings pane can help, and
//! the link that opens it. The onboarding screen reuses the same table, so it
//! is asserted here rather than only through the Devices screen.

use plaudon_app::dto::RadioState;
use plaudon_app::permissions::{
    radio_state_for, settings_pane_for, SettingsHost, SystemSettingsPane,
    MACOS_BLUETOOTH_PRIVACY_URL, WINDOWS_BLUETOOTH_SETTINGS_URL,
};
use plaudon_domain::error::{AppErrorCode, ErrorAction};

#[test]
fn issue_301_every_radio_code_names_its_state() {
    assert_eq!(
        radio_state_for(AppErrorCode::RadioPermissionDenied),
        Some(RadioState::PermissionDenied)
    );
    assert_eq!(
        radio_state_for(AppErrorCode::RadioPermissionNotDetermined),
        Some(RadioState::PermissionNotDetermined)
    );
    assert_eq!(
        radio_state_for(AppErrorCode::RadioPoweredOff),
        Some(RadioState::PoweredOff)
    );
    assert_eq!(
        radio_state_for(AppErrorCode::RadioNoAdapter),
        Some(RadioState::NoAdapter)
    );
    assert_eq!(radio_state_for(AppErrorCode::DeviceBusy), None);
}

#[test]
fn issue_301_every_real_radio_refusal_points_at_the_bluetooth_pane() {
    for state in [
        RadioState::PermissionDenied,
        RadioState::PermissionNotDetermined,
        RadioState::PoweredOff,
        RadioState::NoAdapter,
    ] {
        assert_eq!(
            settings_pane_for(state),
            Some(SystemSettingsPane::BluetoothPrivacy),
            "{state:?}"
        );
    }
    assert_eq!(settings_pane_for(RadioState::Scanned), None);
    assert_eq!(
        settings_pane_for(RadioState::UnavailableInBuild),
        None,
        "no setting gives a binary a radio it was built without"
    );
}

#[test]
fn issue_301_the_new_codes_resolve_through_the_permission_settings_action() {
    for code in [
        AppErrorCode::RadioPermissionNotDetermined,
        AppErrorCode::RadioPoweredOff,
        AppErrorCode::RadioNoAdapter,
    ] {
        assert_eq!(code.action(), Some(ErrorAction::OpenPermissionSettings));
        assert_ne!(
            code.canonical_message(),
            AppErrorCode::RadioPermissionDenied.canonical_message()
        );
    }
}

#[test]
fn issue_301_the_deep_links_are_the_documented_panes() {
    assert_eq!(
        MACOS_BLUETOOTH_PRIVACY_URL,
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth"
    );
    assert_eq!(WINDOWS_BLUETOOTH_SETTINGS_URL, "ms-settings:bluetooth");
    let pane = SystemSettingsPane::BluetoothPrivacy;
    assert_eq!(
        pane.deep_link(SettingsHost::MacOs),
        Some(MACOS_BLUETOOTH_PRIVACY_URL)
    );
    assert_eq!(
        pane.deep_link(SettingsHost::Windows),
        Some(WINDOWS_BLUETOOTH_SETTINGS_URL)
    );
    assert_eq!(pane.deep_link(SettingsHost::Other), None);
    assert_eq!(
        pane.control_label(SettingsHost::MacOs),
        Some("Open System Settings → Privacy & Security → Bluetooth")
    );
    assert_eq!(
        pane.control_label(SettingsHost::Windows),
        Some("Open Settings → Bluetooth & devices")
    );
    assert_eq!(pane.control_label(SettingsHost::Other), None);
}

#[test]
fn issue_301_the_host_is_the_one_this_binary_was_built_for() {
    let expected = if cfg!(target_os = "macos") {
        SettingsHost::MacOs
    } else if cfg!(target_os = "windows") {
        SettingsHost::Windows
    } else {
        SettingsHost::Other
    };
    assert_eq!(SettingsHost::current(), expected);
}

#[test]
fn issue_301_a_build_without_a_radio_reports_no_authorization() {
    // The default build has no Bluetooth stack, so it cannot ask the OS; it
    // says so rather than guessing "allowed".
    if !cfg!(feature = "hardware-scan") {
        assert_eq!(plaudon_app::permissions::bluetooth_authorization(), None);
    }
}
