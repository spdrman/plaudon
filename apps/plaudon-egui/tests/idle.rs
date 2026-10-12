//! What the window does when nobody is doing anything: nothing.
//!
//! The Tauri shell inherited a webview that repaints on its own schedule;
//! an egui window repaints when it is told to, and the whole architecture of
//! this build — a waker on the event channel instead of a poll loop — exists
//! so that "told to" means "something actually happened".
//!
//! This is the regression test for that. An unconditional
//! `request_repaint()` anywhere in the shell, a screen that re-asks for a
//! frame every frame, or a timer in a panel would all make the loop run at
//! the display's refresh rate forever, and every one of them is caught here
//! by the same assertion: once the application has settled, the frame egui
//! asks for next is **never**.
//!
//! The matching figure for the shipped binary — real CPU over thirty idle
//! seconds — is recorded in `docs/EGUI_MIGRATION_PLAN.md` §4.1 row S16; a
//! unit test cannot measure a process it is not.

mod support;

use std::time::Duration;

use plaudon_app::shell_state::ShellStateStore;
use plaudon_ui::shell::Route;

/// The repaint the application asked for after the frame that just ran.
fn next_repaint(
    harness: &plaudon_ui_testkit::Harness<'_, plaudon_egui::shell_app::PlaudonShell>,
) -> Duration {
    harness
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .expect("the root viewport reports its repaint schedule")
        .repaint_delay
}

#[test]
fn a_settled_window_asks_for_no_further_frame() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let mut harness = support::harness(support::app(dir.path(), support::hooks(store), audio));

    // `run` returns only when the application stops asking for an immediate
    // frame; exceeding the step budget is a panic, so reaching this line is
    // already half the assertion.
    let steps = harness.run();
    assert!(
        steps < 64,
        "the library settled in {steps} frames, which is a repaint loop, not a settle"
    );
    assert_eq!(
        next_repaint(&harness),
        Duration::MAX,
        "an idle window must schedule no frame at all"
    );
}

#[test]
fn every_destination_settles_to_no_frame() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let mut harness = support::harness(support::app(dir.path(), support::hooks(store), audio));

    for route in [
        Route::Library,
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
        assert_eq!(
            next_repaint(&harness),
            Duration::MAX,
            "{} keeps the window awake",
            route.path()
        );
    }
}

#[test]
fn a_pending_state_write_schedules_one_frame_and_then_stops() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let mut harness = support::harness(support::app(
        dir.path(),
        support::hooks(store.clone()),
        audio,
    ));
    harness.run();

    harness
        .state_mut()
        .shell_mut()
        .set_appearance(plaudon_app::dto::Appearance::Dark);
    // Settle the theme change itself: swapping the palette animates, and an
    // animation legitimately asks for the next frame immediately.
    harness.run();

    // What is left once it has settled is the debounce, the one timer the
    // shell owns: a single scheduled frame, not a running loop.
    let scheduled = next_repaint(&harness);
    assert!(
        scheduled > Duration::ZERO && scheduled <= plaudon_egui::shell_app::SAVE_DEBOUNCE,
        "the debounced write scheduled {scheduled:?}"
    );
}
