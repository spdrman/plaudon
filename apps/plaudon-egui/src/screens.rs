//! Which screen answers for which destination.
//!
//! One function, called by the binary and by every integration test, so the
//! application a test drives is the application that ships. A destination
//! missing here renders the registry's honest "not built yet" state, and
//! `every_destination_has_a_screen` in `tests/registration.rs` fails if one
//! ever is.
//!
//! The two things a screen cannot supply for itself arrive as arguments: the
//! audio output device (a real `cpal` device in the binary, a fake one in a
//! test) and where built waveforms are cached.

use std::path::PathBuf;

use plaudon_ui::screens::recording::{AudioOutFactory, LivePanels};

use crate::shell_app::PlaudonShell;

/// Register every destination's screen and the first-run flow.
pub fn register_all(
    app: &mut PlaudonShell,
    audio_out: AudioOutFactory,
    peak_cache_dir: Option<PathBuf>,
) {
    // Matrix rows C1-C5, C8, C9, A13, J16, S15, J11-J13, D14 and D16: the
    // Settings destination hosts settings, the activity history and the
    // PLAUD/watched-folder sources; the first-run flow gates all of it.
    let mut settings = plaudon_ui::settings::SettingsDestinationScreen::new();
    let specs = app.shell().command_specs().to_vec();
    settings.settings_mut().set_command_registry(&specs);
    app.shell_mut().register(Box::new(settings));
    app.shell_mut()
        .register_onboarding(Box::new(plaudon_ui::settings::OnboardingShellScreen::new()));

    // Matrix rows L1-L19, P1-P16, T1-T14, A1-A10 and A14: Library owns the
    // recording detail, and the detail's three panels are the real ones.
    let panels = LivePanels::new(audio_out, peak_cache_dir);
    app.shell_mut().register(Box::new(
        plaudon_ui::screens::library::LibraryScreen::with_panels(Box::new(panels)),
    ));
    app.shell_mut()
        .register(Box::new(plaudon_ui::screens::inbox::InboxScreen::new()));

    // Matrix rows J1-J10.
    app.shell_mut().register(Box::new(
        plaudon_ui::jobs::shell_adapter::JobsShellScreen::new(),
    ));

    // Matrix rows D1-D12 and D15.
    app.shell_mut().register(Box::new(
        plaudon_ui::screens::devices::shell_adapter::DevicesShellScreen::new(),
    ));

    // Matrix row A12: an honest placeholder — "no template backend exists",
    // not "this screen was not built" — until EPIC O task O2.6 (#224) lands
    // the template commands and its own screen.
    app.shell_mut()
        .register(Box::new(plaudon_ui::screens::templates::TemplatesScreen::new()));
}
