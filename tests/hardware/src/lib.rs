//! Opt-in hardware suites (spec 18.3A).
//!
//! Every test in this package needs a real machine in a particular state and
//! is therefore gated behind an environment variable. The package exists as a
//! library only so the gate itself is one implementation shared by every
//! suite; the tests live under `tests/`.
//!
//! The gate is deliberately loud rather than silent: a skipped hardware test
//! prints the variable that would have run it, so a CI log shows that the
//! suite was present and declined, not that it was forgotten.

/// Whether an opt-in suite may run, printing the reason when it may not.
///
/// Returns `false` when `variable` is unset or empty.
#[must_use]
pub fn opted_in(variable: &str, what: &str) -> bool {
    match std::env::var(variable) {
        Ok(value) if !value.trim().is_empty() => true,
        _ => {
            println!(
                "skipping {what}: set {variable}=1 to run it. \
                 Opt-in hardware suites never run by default (spec 18.3A)."
            );
            false
        }
    }
}

/// Path to an audio fixture an opt-in suite should transcribe, if provided.
#[must_use]
pub fn audio_fixture(variable: &str) -> Option<std::path::PathBuf> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file())
}

/// Directory the suite writes evidence records into.
///
/// Defaults to `target/hardware-evidence`, which the release bundle collects.
#[must_use]
pub fn evidence_dir() -> std::path::PathBuf {
    std::env::var("PLAUDON_HARDWARE_EVIDENCE_DIR").map_or_else(
        |_| std::path::PathBuf::from("target/hardware-evidence"),
        std::path::PathBuf::from,
    )
}

/// Write one evidence record where the release bundle will find it.
///
/// # Errors
///
/// Returns the I/O error when the directory cannot be created or the file
/// cannot be written.
pub fn write_evidence(
    name: &str,
    record: &serde_json::Value,
) -> std::io::Result<std::path::PathBuf> {
    let dir = evidence_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(record)?)?;
    Ok(path)
}
