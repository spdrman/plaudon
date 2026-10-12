//! `plaudon-egui`: the window.
//!
//! Four things happen here and nowhere else: logging is initialised (the
//! Tauri shell never did this — `init_logging` had no caller), the shell's
//! presentation state is read from disk, the engine is started over the
//! platform data directory, and eframe is handed the shell with its host
//! hooks.

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

    let engine = match AppEngine::start(config) {
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

    let options = eframe::NativeOptions {
        viewport: window_state::viewport(state.window),
        renderer: eframe::Renderer::Wgpu,
        ..eframe::NativeOptions::default()
    };
    let hooks = native_hooks(store, engine.handle());

    let result = eframe::run_native(
        "Plaudon",
        options,
        Box::new(move |_cc| {
            let mut app = PlaudonShell::new(engine, hooks, state);
            // One line per screen (O1.3's registry contract).
            app.shell_mut()
                .register(Box::new(plaudon_ui::screens::library::LibraryScreen::new()));
            app.shell_mut()
                .register(Box::new(plaudon_ui::screens::inbox::InboxScreen::new()));
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

/// The real dialogs, and a state writer that never touches the render
/// thread.
fn native_hooks(store: ShellStateStore, runtime: tokio::runtime::Handle) -> HostHooks {
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
    }
}

/// Logs go next to the library, under `logs/`.
fn data_dir_logs(config: &EngineConfig) -> PathBuf {
    config.data_dir().join("logs")
}
