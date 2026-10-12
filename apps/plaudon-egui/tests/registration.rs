//! Every destination has a screen.
//!
//! The registry's "not available in this build yet" state is correct while a
//! task is in flight and wrong once it has landed: a user cannot tell it
//! apart from "built and empty". This asserts it is unreachable.

mod support;

use plaudon_app::shell_state::ShellStateStore;
use plaudon_ui::shell::route::Destination;
use plaudon_ui::shell::Route;

#[test]
fn every_destination_has_a_screen() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let app = support::app(dir.path(), support::hooks(store), audio);

    for destination in Destination::ALL {
        assert!(
            app.shell().has_screen(destination),
            "{} renders the registry's empty state",
            destination.label()
        );
    }
}

#[test]
fn the_first_run_flow_is_registered() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let app = support::app(dir.path(), support::hooks(store), audio);
    assert!(app.shell().has_onboarding());
}

#[test]
fn no_route_renders_the_not_available_state() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let mut harness = support::harness(support::app(dir.path(), support::hooks(store), audio));

    for route in [
        Route::Library,
        Route::LibrarySearch,
        Route::Inbox,
        Route::Jobs,
        Route::Templates,
        Route::Settings,
        Route::SettingsActivity,
        Route::SettingsSources,
        Route::Devices,
    ] {
        harness.state_mut().shell_mut().navigate(route.clone());
        harness.run();
        assert!(
            !support::announces(&harness, "is not available in this build yet"),
            "{} still renders the registry's empty state",
            route.path()
        );
    }
}
