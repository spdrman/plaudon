//! `plaudon-tauri-audit` — the CI gate for Tauri least privilege (§2.4, §6.3A,
//! §18.5) and the producer of the §24 release-evidence artifact.
//!
//! ```text
//! plaudon-tauri-audit [--root <dir>] [--format text|json] [--output <file>]
//!                     [--allow-missing-app]
//! ```
//!
//! * `--root` — repository root. Defaults to the workspace root discovered by
//!   walking up from the current directory for a `Cargo.toml` containing
//!   `[workspace]`.
//! * `--format` — `text` (default) for humans, `json` for the evidence bundle.
//! * `--output` — write the report to a file instead of stdout. Parent
//!   directories are created.
//! * `--allow-missing-app` — do not fail merely because
//!   `apps/desktop/src-tauri` has not landed yet; the manifest is still
//!   audited. Without it, a missing application is exit code 3, never a pass.
//!
//! Exit codes:
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | audit passed |
//! | 1 | one or more findings |
//! | 2 | command-line usage error |
//! | 3 | `apps/desktop/src-tauri` is absent and `--allow-missing-app` was not given — least privilege is **not verifiable**, not "fine" |
//! | 4 | the embedded allowlist manifest is invalid, or an input could not be read |

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use plaudon_security::tauri_guard::allowlist::CommandAllowlist;
use plaudon_security::tauri_guard::audit::{self, AuditError, AuditOptions, AuditReport};

const EXIT_OK: u8 = 0;
const EXIT_FINDINGS: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_APP_MISSING: u8 = 3;
const EXIT_INTERNAL: u8 = 4;

const USAGE: &str = "usage: plaudon-tauri-audit [--root <dir>] [--format text|json] \
[--output <file>] [--allow-missing-app]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

#[derive(Debug)]
struct Args {
    root: Option<PathBuf>,
    format: Format,
    output: Option<PathBuf>,
    allow_missing_app: bool,
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let allowlist = match CommandAllowlist::load_default() {
        Ok(allowlist) => allowlist,
        Err(error) => {
            eprintln!("embedded command allowlist is invalid: {error}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };

    let root = match args.root.clone() {
        Some(root) => root,
        None => match AuditOptions::discover_root(&PathBuf::from(".")) {
            Ok(root) => root,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(EXIT_USAGE);
            }
        },
    };

    let options = AuditOptions::new(root).allow_missing_app(args.allow_missing_app);
    let report = match audit::run(&options, allowlist) {
        Ok(report) => report,
        Err(error @ AuditError::AppMissing { .. }) => {
            eprintln!("{error}");
            return ExitCode::from(EXIT_APP_MISSING);
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };

    let rendered = match args.format {
        Format::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => format!("{json}\n"),
            Err(error) => {
                eprintln!("could not serialize the audit report: {error}");
                return ExitCode::from(EXIT_INTERNAL);
            }
        },
        Format::Text => render_text(&report),
    };

    if let Some(path) = &args.output {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(error) = fs::create_dir_all(parent) {
                    eprintln!("cannot create `{}`: {error}", parent.display());
                    return ExitCode::from(EXIT_INTERNAL);
                }
            }
        }
        if let Err(error) = fs::write(path, rendered.as_bytes()) {
            eprintln!("cannot write `{}`: {error}", path.display());
            return ExitCode::from(EXIT_INTERNAL);
        }
    } else {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        if let Err(error) = lock.write_all(rendered.as_bytes()) {
            eprintln!("cannot write the report: {error}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    }

    if report.passed() {
        if !report.app_present {
            eprintln!(
                "NOT VERIFIABLE: {} has no tauri.conf.json yet; only the command allowlist manifest was audited",
                audit::APP_SRC_TAURI
            );
        }
        ExitCode::from(EXIT_OK)
    } else {
        // The text report already lists the findings on stdout; repeat them on
        // stderr only when stdout carried the machine-readable artifact.
        if args.output.is_some() || args.format == Format::Json {
            for finding in &report.findings {
                eprintln!("{finding}");
            }
        }
        eprintln!(
            "tauri least-privilege audit FAILED with {} finding(s)",
            report.findings.len()
        );
        ExitCode::from(EXIT_FINDINGS)
    }
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args {
        root: None,
        format: Format::Text,
        output: None,
        allow_missing_app: false,
    };
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--root" => {
                let value = raw
                    .next()
                    .ok_or_else(|| "--root requires a path".to_owned())?;
                args.root = Some(PathBuf::from(value));
            }
            "--format" => {
                let value = raw
                    .next()
                    .ok_or_else(|| "--format requires a value".to_owned())?;
                args.format = match value.as_str() {
                    "json" => Format::Json,
                    "text" => Format::Text,
                    other => return Err(format!("unknown --format `{other}`")),
                };
            }
            "--output" => {
                let value = raw
                    .next()
                    .ok_or_else(|| "--output requires a path".to_owned())?;
                args.output = Some(PathBuf::from(value));
            }
            "--allow-missing-app" => args.allow_missing_app = true,
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(args)
}

fn render_text(report: &AuditReport) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    // Writing into a `String` is infallible; the `Result` is discarded.
    let _ = write!(
        out,
        "plaudon tauri least-privilege audit (§2.4, §6.3A, §15.1)\n\
         root:                 {root}\n\
         application present:  {present}\n\
         manifest version:     {version}\n\
         allowlisted commands: {allowlisted}\n\
         registered commands:  {registered}\n\
         granted permissions:  {permissions}\n\
         csp directives:       {csp}\n\
         release flags:        devtools={devtools} with_global_tauri={global} \
         remote_window_urls={remote} dangerous_keys={dangerous}\n\
         not covered by this audit:\n",
        root = report.root,
        present = report.app_present,
        version = report.manifest_version,
        allowlisted = report.manifest_commands.len(),
        registered = report.registered_commands.len(),
        permissions = report.granted_permissions.len(),
        csp = report.csp.len(),
        devtools = report.release_flags.devtools,
        global = report.release_flags.with_global_tauri,
        remote = report.release_flags.remote_window_urls,
        dangerous = report.release_flags.dangerous_keys,
    );
    for item in report.not_covered {
        let _ = writeln!(out, "  - {item}");
    }
    if report.findings.is_empty() {
        out.push_str("findings:             none\n");
    } else {
        let _ = writeln!(out, "findings:             {}", report.findings.len());
        for finding in &report.findings {
            let _ = writeln!(out, "  - {finding}");
        }
    }
    out
}
