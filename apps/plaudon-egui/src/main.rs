//! `plaudon-egui`: the window.
//!
//! Three things happen here and nowhere else: logging is initialised (the
//! Tauri shell never did this — `init_logging` had no caller), the engine is
//! started over the platform data directory, and eframe is handed the view
//! with its host hooks.

use std::process::ExitCode;

use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_diagnostics::crash::install_crash_hook;
use plaudon_diagnostics::logging::{init_logging, LoggingConfig};
use plaudon_egui::{viewport, HostHooks, PlaudonEgui};

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
        viewport: viewport(),
        renderer: eframe::Renderer::Wgpu,
        ..eframe::NativeOptions::default()
    };

    let result = eframe::run_native(
        "Plaudon",
        options,
        Box::new(move |_cc| Ok(Box::new(PlaudonEgui::new(engine, HostHooks::native())))),
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

/// Logs go next to the library, under `logs/`.
fn data_dir_logs(config: &EngineConfig) -> std::path::PathBuf {
    config.data_dir().join("logs")
}
