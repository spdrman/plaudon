//! A stand-in for the official PLAUD Cloud CLI (spec 18.3).
//!
//! The suite must be hermetic and must run identically on macOS and Windows,
//! so the mock is a real executable rather than a shell script. It is copied
//! into a temporary directory under the pinned leaf name `plaud` (`plaud.exe`
//! on Windows) — which is what the egress allowlist and `CliDiscovery` both
//! require — and reads its script from `plaud.mock.json` sitting next to the
//! executable.
//!
//! Configuration travels through a file rather than the environment on
//! purpose: the runner scrubs the child environment, and two tests running in
//! parallel must not be able to see each other's setup.

use std::io::Write;
use std::path::PathBuf;

/// One scripted subcommand response.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct Response {
    /// Bytes written to stdout, verbatim.
    stdout: String,
    /// Bytes written to stderr, verbatim.
    stderr: String,
    /// Process exit code.
    exit_code: i32,
    /// Milliseconds to sleep before writing anything.
    delay_ms: u64,
    /// When set, write this many kibibytes of filler to stdout instead of
    /// `stdout`, to exercise the runner's output cap.
    stdout_filler_kib: u64,
    /// When set, write raw non-UTF-8 bytes to stdout.
    stdout_invalid_utf8: bool,
}

/// The whole script.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct Script {
    version: Response,
    login: Response,
    logout: Response,
    files: Response,
    /// Response for `files` when a cursor argument was supplied.
    files_page_2: Response,
    audio: Response,
    /// `{id}` in this template is replaced with the requested recording id.
    audio_url_template: Option<String>,
    /// Subcommand the mock should reject as unknown.
    unknown_exit_code: i32,
    /// Every argument vector the mock has been invoked with, appended as JSON
    /// lines to `plaud.invocations.jsonl` next to the executable.
    record_invocations: bool,
}

fn script_path() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.with_file_name("plaud.mock.json")
}

fn emit(response: &Response) -> i32 {
    if response.delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(response.delay_ms));
    }
    let mut stdout = std::io::stdout().lock();
    if response.stdout_invalid_utf8 {
        stdout.write_all(&[0xff, 0xfe, b'b', b'a', b'd']).ok();
    } else if response.stdout_filler_kib > 0 {
        let chunk = vec![b'x'; 1024];
        for _ in 0..response.stdout_filler_kib {
            if stdout.write_all(&chunk).is_err() {
                break;
            }
        }
    } else {
        stdout.write_all(response.stdout.as_bytes()).ok();
    }
    stdout.flush().ok();
    if !response.stderr.is_empty() {
        let mut stderr = std::io::stderr().lock();
        stderr.write_all(response.stderr.as_bytes()).ok();
        stderr.flush().ok();
    }
    response.exit_code
}

/// The page number a `files` invocation asked for, when it used numbered
/// paging.
fn page_argument(args: &[String]) -> Option<u32> {
    args.windows(2)
        .find(|pair| pair[0] == "-p" || pair[0] == "--page")
        .and_then(|pair| pair[1].parse().ok())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let script: Script = std::fs::read_to_string(script_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();

    if script.record_invocations {
        let path = script_path().with_file_name("plaud.invocations.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let line = serde_json::to_string(&args).unwrap_or_default();
            writeln!(file, "{line}").ok();
        }
    }

    let code = match args.first().map(String::as_str) {
        Some("version") => emit(&script.version),
        Some("login") => emit(&script.login),
        Some("logout") => emit(&script.logout),
        Some("files") => {
            // Page two is selected either by a cursor argument or by the
            // numbered paging the real 0.3.x releases use (`-p N -s N`).
            if page_argument(&args).map_or(args.len() > 1, |page| page >= 2) {
                emit(&script.files_page_2)
            } else {
                emit(&script.files)
            }
        }
        Some("audio") => {
            let id = args.get(1).cloned().unwrap_or_default();
            if let Some(template) = &script.audio_url_template {
                let mut response = Response {
                    stdout: format!("{}\n", template.replace("{id}", &id)),
                    ..Response::default()
                };
                response.exit_code = script.audio.exit_code;
                response.stderr.clone_from(&script.audio.stderr);
                response.delay_ms = script.audio.delay_ms;
                emit(&response)
            } else {
                emit(&script.audio)
            }
        }
        _ => script.unknown_exit_code,
    };
    std::process::exit(code);
}
