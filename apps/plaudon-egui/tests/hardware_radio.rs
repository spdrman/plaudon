//! What `--features hardware-scan` means in this binary (matrix row D12).
//!
//! The feature used to be a dependency edge and nothing else: it forwarded to
//! `plaudon-app/hardware-scan`, which compiled the `btleplug` stack into the
//! binary, and then no code ever built a radio from it. A `--features
//! hardware-scan` build therefore reported `RadioState::UnavailableInBuild`
//! exactly like a default one — the feature was unobservable.
//!
//! These tests pin both halves of the fix, and they do it without a radio,
//! because neither CI nor a developer machine is guaranteed to have one:
//!
//! * the default build must contain no radio and must *say* it contains no
//!   radio, which is the honest state and the one CI runs in;
//! * a `hardware-scan` build must hand the engine the host adapter, asserted
//!   from the configuration the binary builds rather than by scanning, so the
//!   feature-on leg still touches no hardware.
//!
//! What no test here can establish is that a transfer over a real PLAUD
//! device works; that needs the device. Row D12 stays short of `done` for
//! that reason and no other.

mod support;

#[test]
fn the_published_build_flag_matches_the_feature_it_reports() {
    assert_eq!(
        plaudon_egui::HARDWARE_SCAN,
        cfg!(feature = "hardware-scan"),
        "HARDWARE_SCAN is what the rest of the product reads; it must not drift \
         from the feature that decides the wiring"
    );
}

/// A default build contains no Bluetooth stack, so the honest answer to "scan
/// for devices" is that this build cannot — not an empty list, which a user
/// would read as "no device is near".
#[cfg(not(feature = "hardware-scan"))]
#[test]
fn a_default_build_reports_that_it_cannot_scan() {
    use plaudon_app::command::{AppCommand, AppResponse};
    use plaudon_app::dto::{ListNearbyDevicesRequest, RadioState};
    use plaudon_app::runtime::AppEngine;

    let dir = tempfile::tempdir().expect("temporary library");
    let engine = AppEngine::start(plaudon_egui::with_build_hardware(support::config(
        dir.path(),
    )))
    .expect("the engine starts over a fresh library");

    let response = engine
        .dispatcher()
        .call_blocking(AppCommand::ListNearbyDevices(ListNearbyDevicesRequest {
            timeout_ms: 5_000,
        }))
        .expect("a scan request is valid even without a radio");
    let AppResponse::ListNearbyDevices(scan) = response else {
        panic!("list_nearby_devices answered with the wrong response type");
    };
    assert_eq!(
        scan.radio.state,
        RadioState::UnavailableInBuild,
        "a build without the feature must not claim a radio"
    );
    assert!(
        scan.scanned_at.is_none(),
        "nothing was scanned, so no scan has an instant"
    );
    engine.shutdown(std::time::Duration::from_secs(5));
}

/// A `hardware-scan` build asks the engine for the host's own adapter.
///
/// Asserted from the configuration, not from a scan: constructing the radio
/// is what the wiring does, and touching it is what the hardware suite does.
/// The engine is never started here, so this leg opens no adapter even on a
/// machine that has one.
#[cfg(feature = "hardware-scan")]
#[test]
fn a_hardware_build_asks_the_engine_for_the_host_adapter() {
    let dir = tempfile::tempdir().expect("temporary library");
    let described = format!(
        "{:?}",
        plaudon_egui::with_build_hardware(support::config(dir.path()))
    );
    assert!(
        described.contains("HostBluetooth"),
        "the hardware build must choose the host adapter: {described}"
    );
}

/// The mirror of the test above: without the feature the configuration the
/// binary builds carries the supplied (absent) radio, so there is no path
/// from a default build to a Bluetooth adapter at all.
#[cfg(not(feature = "hardware-scan"))]
#[test]
fn a_default_build_never_asks_for_an_adapter() {
    let dir = tempfile::tempdir().expect("temporary library");
    let described = format!(
        "{:?}",
        plaudon_egui::with_build_hardware(support::config(dir.path()))
    );
    assert!(
        described.contains("Supplied"),
        "the default build must keep the radio it was given: {described}"
    );
    assert!(
        !described.contains("HostBluetooth"),
        "a default build must not reach for an adapter: {described}"
    );
}
