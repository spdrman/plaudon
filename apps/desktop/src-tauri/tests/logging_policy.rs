//! The desktop shell must never install a plain `tracing` subscriber.
//!
//! `AppError::detail` carries internal diagnostic text (paths, provider
//! responses, vendor messages). It is kept out of the §15.4 envelope, but it is
//! logged. With no subscriber installed those records are dropped; the hazard
//! is the day someone reaches for `tracing_subscriber::fmt::init()` while
//! debugging and every detail string starts reaching stdout unredacted.
//!
//! This test asserts the source property rather than the runtime one: the
//! global dispatcher is process-wide and set-once, so a runtime assertion would
//! pass or fail depending on test order. Only `plaudon_diagnostics::init_logging`
//! (EPIC D's redacting sink) may ever install a subscriber here.

use std::fs;
use std::path::{Path, PathBuf};

/// Ways of installing a global `tracing` subscriber that are not the redacting sink.
const FORBIDDEN: &[&str] = &[
    "subscriber::set_global_default",
    "set_global_default(",
    "fmt::init(",
    "fmt().init(",
    "fmt::try_init(",
    "tracing_subscriber::registry()",
    "tracing_log::LogTracer",
];

/// The single approved call, allowed anywhere in this crate.
const APPROVED: &str = "plaudon_diagnostics::init_logging";

#[test]
fn no_source_file_installs_a_non_redacting_tracing_subscriber() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    for file in rust_sources(&root.join("src")) {
        let text = fs::read_to_string(&file).expect("source file is readable");
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);
            if code.contains(APPROVED) {
                continue;
            }
            for needle in FORBIDDEN {
                if code.contains(needle) {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "only plaudon_diagnostics::init_logging may install a tracing subscriber; found:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_crate_declares_no_tracing_subscriber_dependency() {
    let manifest = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("the crate manifest is readable");
    for forbidden in ["tracing-subscriber", "tracing-log", "env_logger", "simple_logger"] {
        assert!(
            !manifest.contains(forbidden),
            "{forbidden} must not be a dependency of the desktop shell: logging goes through \
             plaudon-diagnostics so records are redacted before they reach a writer"
        );
    }
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).expect("source directory exists");
    for entry in entries {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            files.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files
}
