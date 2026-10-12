//! `plaud-cli-probe`: the opt-in K1.1 vendor-CLI capture tool.
//!
//! Never built by default and never run in CI (spec 18.3, 18.8). It exists so
//! that promoting a release into `cli-compatibility.yaml` is a transcription
//! of something observed rather than an assumption (spec 2.1, 2.6, ADR-0050).
//!
//! It runs the *real* vendor CLI through the same hardened runner the
//! application uses — same argument array, same scrubbed environment, same
//! timeouts, same output caps, same egress gate — and writes a capture bundle
//! that the golden-snapshot test can consume directly.
//!
//!   scripts/probe-plaud-cli.sh <output-directory>
//!
//! It never signs in for you, never writes to your PLAUD account and never
//! runs anything outside `--version` and `files`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_diagnostics::redact::Redactor;
use plaudon_security::outbound::subprocess::GatedProcessLauncher;
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
use plaudon_source_plaud_cli::compatibility::CliCompatibilityTable;
use plaudon_source_plaud_cli::discovery::CliDiscovery;
use plaudon_source_plaud_cli::invocation::CliCommand;
use plaudon_source_plaud_cli::parser::CliVersion;
use plaudon_source_plaud_cli::runner::{CliRunner, RunLimits, UserInitiated};
use sha2::{Digest, Sha256};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let Some(output_dir) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: plaud-cli-probe <output-directory>");
        return std::process::ExitCode::FAILURE;
    };

    let discovery = CliDiscovery::new();
    let found = match discovery.discover() {
        Ok(found) => found,
        Err(error) => {
            eprintln!("discovery failed: {error}");
            eprintln!(
                "searched: {:?} and the PATH entries",
                discovery.well_known_directories()
            );
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("program: {}", found.program.display());
    println!("origin:  {}", found.origin.label());

    // Running this binary is the operator acting deliberately, which is the
    // same class of approval as confirming the settings toggle; it is recorded
    // as such rather than inferred from a configuration file.
    let policy = OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::UserSettingsConfirmation,
        now_ms(),
        "probe-cli-v1",
    ));
    let runner = CliRunner::new(
        GatedProcessLauncher::new(Arc::new(policy)),
        Arc::new(Redactor::new()),
    );
    let intent = UserInitiated::new("vendor_cli_probe", now_ms());

    let version_output = match runner
        .run(
            &found.program,
            &CliCommand::Version,
            RunLimits::PROBE,
            &intent,
        )
        .await
    {
        Ok(output) => output,
        Err(error) => {
            eprintln!("`plaud --version` failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let Some(version) = CliVersion::parse_version_output(&version_output.stdout) else {
        eprintln!("could not read a version from: {:?}", version_output.stdout);
        eprintln!("record this verbatim in the capture issue; do not guess a version string.");
        return std::process::ExitCode::FAILURE;
    };
    println!("version: {}", version.as_str());
    println!(
        "in the committed table: {}",
        CliCompatibilityTable::committed()
            .verdict(Some(&version))
            .permits_use()
    );

    let files_output = match runner
        .run(
            &found.program,
            &CliCommand::files(),
            RunLimits::ENUMERATE,
            &intent,
        )
        .await
    {
        Ok(output) => output,
        Err(error) => {
            eprintln!("`plaud files` failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if !files_output.succeeded() {
        eprintln!("`plaud files` exited with {:?}", files_output.exit_code);
        eprintln!("redacted stderr: {}", files_output.stderr_excerpt);
        eprintln!("if this says you are not signed in, run `plaud login` yourself first.");
        return std::process::ExitCode::FAILURE;
    }

    if let Err(error) = write_bundle(&output_dir, &version, &files_output.stdout) {
        eprintln!("could not write the capture bundle: {error}");
        return std::process::ExitCode::FAILURE;
    }

    println!();
    println!("capture written to {}", output_dir.display());
    println!("digest: {}", sha256_hex(files_output.stdout.as_bytes()));
    println!();
    println!("Still to record by hand before promoting this release (spec 2.6):");
    for item in [
        "whether `files` output is a documented machine-readable format or a scraped table",
        "whether pagination exists, which flag carries the cursor, and the page size",
        "the exit codes observed for: signed out, unknown id, rate limit, network loss",
        "whether a Google/Apple PLAUD login works with this release",
        "whether vendor transcripts/summaries are exposed through a supported path",
        "the download URL host, scheme, redirect chain, expiry, content type, checksum \
         header and range behaviour (these go into UrlContract and DownloadPolicy)",
    ] {
        println!("  - {item}");
    }
    println!();
    println!("Then follow ADR-0050, \"How to add a tested release\".");
    std::process::ExitCode::SUCCESS
}

/// Write the capture bundle the golden-snapshot test expects.
///
/// `expected.json` is deliberately *not* generated: writing the expectation
/// from the parser's own output would make the snapshot test tautological. It
/// is written by hand from reading the capture.
fn write_bundle(dir: &Path, version: &CliVersion, files_stdout: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("files.stdout"), files_stdout)?;
    let fixture = serde_json::json!({
        "version": version.as_str(),
        "listing_format": "FILL IN: json_lines | json_array | delimited_table",
        "pagination_observed": false,
        "account_scope": "FILL IN: a non-secret account discriminator, e.g. plaud:eu:account-a",
        "captured_on": "FILL IN: YYYY-MM-DD",
        "note": "Captured by plaud-cli-probe. Review for personal data before committing: \
                 recording titles are user content.",
    });
    std::fs::write(
        dir.join("fixture.json"),
        serde_json::to_string_pretty(&fixture)?,
    )?;
    std::fs::write(
        dir.join("DIGEST"),
        format!("{}  files.stdout\n", sha256_hex(files_stdout.as_bytes())),
    )?;
    Ok(())
}
