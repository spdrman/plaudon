//! `plaudon` — the user-local scripting bridge (spec 14.5, App. A.5).
//!
//! The binary is deliberately thin: parse, resolve the library path, run, and
//! print one JSON envelope. Every policy decision lives in the library crate
//! so it is testable without spawning a process.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use plaudon_automation::cli::Cli;
use plaudon_automation::{commands, Envelope, DEFAULT_LIBRARY_FILE};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let Some(library) = cli.library.clone().or_else(default_library_path) else {
        let envelope = Envelope::error(
            cli.command.name(),
            "INVALID_ARGUMENT",
            "the library location could not be determined; pass --library",
            false,
        );
        println!("{}", render(&envelope, cli.pretty));
        return ExitCode::from(2);
    };

    let now_ms = now_ms();
    let envelope = commands::run(&cli.command, &library, now_ms).await;
    println!("{}", render(&envelope, cli.pretty));
    ExitCode::from(u8::try_from(envelope.exit_code()).unwrap_or(2))
}

fn render(envelope: &Envelope, pretty: bool) -> String {
    if pretty {
        envelope.render_pretty()
    } else {
        envelope.render()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The library the desktop application uses, per platform convention.
///
/// `$PLAUDON_LIBRARY` overrides it, which is what the integration tests and a
/// user with a relocated library both need. It is a path, never a credential.
fn default_library_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PLAUDON_LIBRARY") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;

    #[cfg(target_os = "macos")]
    let dir = home.join("Library/Application Support/plaudon");
    #[cfg(target_os = "windows")]
    let dir = std::env::var_os("APPDATA").map_or_else(
        || home.join("AppData/Roaming/plaudon"),
        |appdata| PathBuf::from(appdata).join("plaudon"),
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let dir = home.join(".local/share/plaudon");

    Some(dir.join(DEFAULT_LIBRARY_FILE))
}
