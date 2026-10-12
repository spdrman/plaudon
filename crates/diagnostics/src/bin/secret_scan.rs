//! `plaudon-secret-scan` — fail CI when a key, token or signed URL reaches a
//! log, snapshot, crash dump, exported JSON file or SQLite database
//! (spec §18.5, §1.5, §16.2 "Signed URL leak").
//!
//! Findings print as `path:line:pattern`; the matched bytes are never printed.

use std::path::PathBuf;
use std::process::ExitCode;

use plaudon_diagnostics::scan::{
    Allowlist, ScanOptions, ScanReport, Scanner, DEFAULT_MAX_FILE_BYTES,
};

const USAGE: &str = "\
plaudon-secret-scan — scan paths for credentials, tokens and signed URLs

USAGE:
    plaudon-secret-scan [OPTIONS] <PATH>...

OPTIONS:
    --format <text|json>     Output format (default: text)
    --allowlist <FILE>       JSON allowlist of documented false positives
    --exclude <GLOB>         Skip paths matching this glob (repeatable)
    --max-file-bytes <N>     Skip files larger than N bytes
    -h, --help               Print this help

EXIT CODES:
    0  clean
    1  findings, or an allowlist entry that matched nothing
    2  usage or I/O error";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

#[derive(Debug)]
struct Args {
    paths: Vec<PathBuf>,
    format: Format,
    allowlist: Option<PathBuf>,
    exclude: Vec<String>,
    max_file_bytes: u64,
}

fn parse_args(raw: Vec<String>) -> Result<Args, String> {
    let mut args = Args {
        paths: Vec::new(),
        format: Format::Text,
        allowlist: None,
        exclude: Vec::new(),
        max_file_bytes: DEFAULT_MAX_FILE_BYTES,
    };
    let mut iter = raw.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.to_owned()),
            "--format" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--format needs a value".to_owned())?;
                args.format = match value.as_str() {
                    "text" => Format::Text,
                    "json" => Format::Json,
                    other => return Err(format!("unknown format `{other}`")),
                };
            }
            "--allowlist" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--allowlist needs a path".to_owned())?;
                args.allowlist = Some(PathBuf::from(value));
            }
            "--exclude" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--exclude needs a glob".to_owned())?;
                args.exclude.push(value);
            }
            "--max-file-bytes" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--max-file-bytes needs a value".to_owned())?;
                args.max_file_bytes = value
                    .parse()
                    .map_err(|_| format!("`{value}` is not a byte count"))?;
            }
            other if other.starts_with("--") => return Err(format!("unknown option `{other}`")),
            other => args.paths.push(PathBuf::from(other)),
        }
    }
    if args.paths.is_empty() {
        return Err(format!("at least one path is required\n\n{USAGE}"));
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let allowlist = match args.allowlist.as_deref().map(Allowlist::load).transpose() {
        Ok(list) => list.unwrap_or_default(),
        Err(err) => {
            eprintln!("plaudon-secret-scan: {err}");
            return ExitCode::from(2);
        }
    };

    let scanner = Scanner::new(ScanOptions {
        allowlist,
        max_file_bytes: args.max_file_bytes,
        exclude: args.exclude,
    });
    let report = match scanner.scan(&args.paths) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("plaudon-secret-scan: {err}");
            return ExitCode::from(2);
        }
    };

    match args.format {
        Format::Text => print_text(&report),
        Format::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(err) => {
                eprintln!("plaudon-secret-scan: could not render JSON: {err}");
                return ExitCode::from(2);
            }
        },
    }

    if report.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn print_text(report: &ScanReport) {
    for finding in &report.findings {
        println!("{finding}");
    }
    for skipped in &report.skipped_oversize {
        eprintln!("skipped (over size cap): {skipped}");
    }
    for entry in &report.unused_allowlist {
        println!(
            "allowlist:{}:{}: entry matched nothing (remove it or fix the glob)",
            entry.path, entry.pattern
        );
    }
    eprintln!(
        "scanned {} file(s): {} finding(s), {} stale allowlist entr(y/ies)",
        report.files_scanned,
        report.findings.len(),
        report.unused_allowlist.len()
    );
}
