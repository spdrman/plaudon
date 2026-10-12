//! The Tauri 2 desktop shell.
//!
//! Everything durable, validated or typed lives in `plaudon_app`: the ports,
//! the backends, the DTOs, the validators, the error envelope, the keychain
//! and the §15.3 event stream. This crate is what is left once Tauri is the
//! only concern:
//!
//! * [`commands`] — the `#[tauri::command]` wrappers and the IPC allowlist
//!   (spec §15.2, ADR-0020, ADR-0022) over `plaudon_app::commands`;
//! * [`events`] — [`events::TauriEventSink`], which puts the event stream on
//!   the webview;
//! * [`media`] — the `plaudon-media` scheme, which serves archived audio
//!   read-only with `Range` support and never accepts a path from a URL.
//!
//! The modules below are re-exported so that the shell's own tests, the
//! acceptance suite and `tests/e2e` keep one import path while both frontends
//! are alive (EPIC O requirement 14).

pub mod commands;
pub mod events;
pub mod media;

pub use plaudon_app::{
    backend, build_ports, build_ports_with_radio, default_approved_roots, dto, error, notify,
    ports, secrets, updater, validate, AppPorts,
};

use std::path::PathBuf;
use std::sync::Arc;

use plaudon_app::events::EventSink;
use plaudon_app::secrets::{KeyringSecretStore, SecretStore};

use crate::events::TauriEventSink;

/// Managed Tauri state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The port implementations.
    pub ports: AppPorts,
}

/// Start the desktop application.
///
/// # Panics
///
/// Panics only if the Tauri context cannot be built, which is a build-time
/// configuration error rather than a runtime condition.
pub fn run() {
    tracing_subscriber_init();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(commands::handler());
    let builder = media::register(builder);

    builder
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = tauri::Manager::path(app)
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(handle.clone()));
            let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::new());

            // A notification click routes back through the same handle.
            let activation_handle = handle.clone();
            let on_activated: notify::ActivationHandler = Arc::new(
                move |target: Option<plaudon_notifications::NotificationTarget>| {
                    if let Some(target) = target {
                        activate(&activation_handle, target);
                    }
                },
            );
            // Preferences are loaded by the backend; the dispatcher starts
            // disabled so nothing can be delivered before they are applied.
            let dispatcher = Arc::new(plaudon_notifications::Dispatcher::new(
                notify::platform_notifier(on_activated),
                plaudon_notifications::NotificationSettings::uniform(false),
            ));

            let ports = tauri::async_runtime::block_on(build_ports(
                &data_dir,
                sink,
                dispatcher,
                secrets,
                default_approved_roots(),
            ))?;

            // Spec 16.5 and 22(8): opening the update root runs crash
            // recovery, discarding any uncommitted attempt so the launched
            // version is always a complete, previously verified one.
            plaudon_app::runtime::recover_update_state(&data_dir);

            tauri::Manager::manage(app, AppState { ports });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the desktop application failed to start");
}

/// Route a notification activation: focus the window, then tell the UI where
/// to go.
///
/// Called by the Windows toast handler and by the macOS application delegate
/// with the token from `targetContentIdentifier`. Never force-raises outside
/// this callback (spec §5.5: subject to OS activation rules).
pub fn activate<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: plaudon_notifications::NotificationTarget,
) {
    use tauri::Manager as _;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let Some(state) = app.try_state::<AppState>() else {
        tracing::debug!("notification activated before the shell was ready");
        return;
    };
    let events = state.ports.events.clone();
    tauri::async_runtime::spawn(async move {
        notify::publish_activation(&events, target).await;
    });
}

/// Route a notification activation from a platform token.
///
/// A token this build did not write is ignored.
pub fn activate_token<R: tauri::Runtime>(app: &tauri::AppHandle<R>, token: &str) {
    if let Some(target) = notify::activation_target(token) {
        activate(app, target);
    }
}

fn tracing_subscriber_init() {
    // Logging is redacted at the source: only `error::envelope` writes error
    // detail, and it writes the domain redactor's output.
    tracing::debug!("desktop shell starting");
}
