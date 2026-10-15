//! `plaudon-egui`: the window.
//!
//! Four things happen here and nowhere else: logging is initialised (the
//! Tauri shell never did this — `init_logging` had no caller), the shell's
//! presentation state is read from disk, the engine is started over the
//! platform data directory, and eframe is handed the shell with its host
//! hooks.

// A release build must not flash a console window on Windows. Debug builds
// keep the console: it is where the `eprintln!` calls below go when the
// library cannot be opened, and a developer needs to see that.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::shell_state::{ShellState, ShellStateStore};
use plaudon_diagnostics::crash::install_crash_hook;
use plaudon_diagnostics::logging::{init_logging, LoggingConfig};
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_ui::shell::{window_state, HostHooks};

fn main() -> ExitCode {
    let config = match EngineConfig::for_this_platform() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("plaudon: {}", error.code().as_str());
            return ExitCode::FAILURE;
        }
    };

    // Logs live beside the library, redacted at the source.
    match init_logging(LoggingConfig::new(data_dir_logs(&config))) {
        Ok(handle) => install_crash_hook(&handle),
        Err(error) => eprintln!("plaudon: logging is unavailable: {error}"),
    }

    // Read before the window opens: the saved size and position are what the
    // viewport is built from, so there is no visible jump.
    let store = ShellStateStore::new(config.data_dir());
    let state = store.load();

    // Matrix row D12: what this build can actually talk to. With
    // `hardware-scan` the engine gets the host's Bluetooth adapter; without
    // it the engine keeps `NoRadio` and the device screen says the build
    // cannot scan, which is true.
    let engine = match AppEngine::start(plaudon_egui::with_build_hardware(config)) {
        Ok(engine) => engine,
        Err(error) => {
            tracing::error!(
                code = error.code().as_str(),
                "the library could not be opened"
            );
            eprintln!("plaudon: the library could not be opened");
            return ExitCode::FAILURE;
        }
    };

    // The window, Dock and task-switcher icon: the brand kit's artwork, the
    // same image the `.icns` and the `.ico` are cut from. A decode failure
    // leaves the platform default rather than refusing to open a window.
    let mut viewport = window_state::viewport(state.window);
    if let Some(icon) = plaudon_egui::icon::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..eframe::NativeOptions::default()
    };
    let hooks = native_hooks(store, engine.handle());

    // Matrix row C10. What the OS asked for, before the window exists.
    //
    // Windows is the whole mechanism here: the installer registers
    // `"%EXEPATH%" "%1"` for both the `.plaudon` extension and the
    // `plaudon://` scheme (`packaging/windows/plaudon-egui.wxs`), so a
    // double-click or a link **starts a process** with the document or the
    // link as `argv[1]`. There is no single-instance handshake in this
    // build, and nothing in the lock file provides one, so a second launch
    // while Plaudon is running opens a second window which answers that
    // argument; the first window is left as it was.
    //
    // macOS behaves differently and is handled in `plaudon_egui::mac_open`:
    // Launch Services sends an Apple event, which never reaches `argv`.
    let launch = plaudon_egui::desktop::Desktop::launch_event(
        std::env::args(),
        &plaudon_egui::desktop::approved_roots(),
    );
    #[cfg(target_os = "macos")]
    plaudon_egui::mac_open::install();

    let result = eframe::run_native(
        "Plaudon",
        options,
        Box::new(move |_cc| {
            let peak_cache = engine.data_dir().join("peak-cache");
            let mut app = PlaudonShell::new(engine, hooks, state);
            // Matrix row C11: the status item, where a backend exists.
            let desktop =
                plaudon_egui::desktop::Desktop::native(plaudon_egui::desktop::approved_roots());
            plaudon_egui::desktop::install_tray(&desktop, false);
            // Every destination, in one place the tests drive as well.
            plaudon_egui::screens::register_all(&mut app, audio_output(), Some(peak_cache));
            // Parity row S8: the menu macOS expects, built from the same
            // command registry the in-window bar draws. Here, because it has
            // to be created on the event loop's thread once the application
            // exists, and after the screens so their commands are registered.
            #[cfg(target_os = "macos")]
            plaudon_egui::app_menu::install(&mut app);
            if let Some(event) = launch {
                app.queue_desktop_event(event);
            }
            // Nothing drains the tray, the menu or an open event unless the
            // application owns the adapters: this is where they get a home.
            app.set_desktop(desktop);
            Ok(Box::new(app))
        }),
    );

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "the window could not be created");
            eprintln!("plaudon: the window could not be created");
            ExitCode::FAILURE
        }
    }
}

/// The real audio output device.
///
/// A factory, not a device: nothing is opened until the user plays
/// something, and a device that was lost (an unplugged interface, a
/// switched default) is reopened the next time they do. A failure is
/// reported in the player panel in the words `cpal` gave, because this
/// process cannot know why the host's audio stack refused.
fn audio_output() -> plaudon_ui::screens::recording::AudioOutFactory {
    Arc::new(|| {
        plaudon_audio_playback::cpal_out::CpalOut::default_output()
            .map(|out| Box::new(out) as Box<dyn plaudon_audio_playback::engine::AudioOut>)
    })
}

/// The real dialogs, and a state writer that never touches the render
/// thread.
fn native_hooks(store: ShellStateStore, runtime: tokio::runtime::Handle) -> HostHooks {
    // Issue #307: the live macOS privacy statuses for first-run setup and
    // Settings. The host reads them on its own thread, never the render
    // thread, because an "Ask macOS" call waits for the user to answer.
    let os_permissions = plaudon_ui::permissions::PermissionsHost::background(
        plaudon_platform::os_permissions::native(&runtime),
        plaudon_app::dto::Platform::host(),
    );
    HostHooks {
        pick_audio_files: Arc::new(|| {
            rfd::FileDialog::new()
                .set_title("Import audio")
                .add_filter(
                    "Audio",
                    &["wav", "mp3", "m4a", "flac", "ogg", "opus", "aac"],
                )
                .pick_files()
                .unwrap_or_default()
        }),
        pick_folder: Arc::new(|| rfd::FileDialog::new().pick_folder()),
        pick_save_path: Arc::new(|suggested| {
            rfd::FileDialog::new().set_file_name(suggested).save_file()
        }),
        // Issue #303/#305: "Import a model file I already have". The file is
        // checked against the catalogue's size and SHA-256 before it is
        // installed, so the filter is a convenience, not a check.
        pick_open_file: Arc::new(|title| {
            rfd::FileDialog::new()
                .set_title(title)
                .add_filter("Model weights", &["bin"])
                .pick_file()
        }),
        // Matrix row C10: the view never spawns a process; the opener does,
        // with an argument vector and never a shell.
        reveal_in_folder: Arc::new(|path| {
            use plaudon_platform::opener::Opener as _;
            if let Err(error) = plaudon_platform::opener::NativeOpener::new().reveal_in_folder(path)
            {
                tracing::debug!(%error, "the file manager could not be opened");
            }
        }),
        save_shell_state: Arc::new(move |state: ShellState| {
            let store = store.clone();
            // Writing a few hundred bytes is still a blocking syscall, and a
            // dropped frame while the user drags the window is exactly what
            // the render thread must not do.
            runtime.spawn_blocking(move || {
                if let Err(error) = store.save(&state) {
                    tracing::warn!(
                        code = error.code().as_str(),
                        "the shell state could not be saved"
                    );
                }
            });
        }),
        os_permissions,
    }
}

/// Logs go next to the library, under `logs/`.
fn data_dir_logs(config: &EngineConfig) -> PathBuf {
    config.data_dir().join("logs")
}
