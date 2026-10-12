//! Building and running the monitored scenario process.
//!
//! The harness builds the injected `cdylib` and the scenario binary itself, in
//! the *same* target directory as the test that is running, then launches each
//! scenario as a child process with the monitor injected and a fresh trace and
//! decision file.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use plaudon_egress_monitor::{MonitorStatus, STATUS};
use plaudon_egress_scenario::{Scenario, ScenarioSummary, DECISIONS_ENV, WORKDIR_ENV};
use serde::Deserialize;

use crate::trace::Trace;

/// Whether the monitor is injected into the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injection {
    /// Normal operation: `DYLD_INSERT_LIBRARIES` / `LD_PRELOAD` is set.
    Enabled,
    /// Used once, on purpose, to prove the harness notices an un-observed run.
    Disabled,
}

impl Injection {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Injection::Enabled => "monitor injected",
            Injection::Disabled => "monitor deliberately not injected",
        }
    }
}

/// Something went wrong before any acceptance rule could be applied.
#[derive(Debug)]
pub enum FixtureError {
    Io(io::Error),
    /// `cargo build` of the monitor or the scenario failed.
    Build {
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    /// A required artifact was not produced.
    ArtifactMissing {
        what: String,
        hint: String,
    },
    /// The trace file could not be parsed.
    Trace(crate::trace::TraceParseError),
    /// The scenario's decision summary could not be parsed.
    Summary {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FixtureError::Io(err) => write!(f, "io error: {err}"),
            FixtureError::Build {
                command,
                status,
                stderr,
            } => {
                write!(f, "`{command}` failed with {status:?}:\n{stderr}")
            }
            FixtureError::ArtifactMissing { what, hint } => {
                write!(f, "could not locate {what}: {hint}")
            }
            FixtureError::Trace(err) => write!(f, "{err}"),
            FixtureError::Summary { path, message } => {
                write!(
                    f,
                    "could not read the decision summary {}: {message}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for FixtureError {}

impl From<io::Error> for FixtureError {
    fn from(err: io::Error) -> Self {
        FixtureError::Io(err)
    }
}

/// Paths the harness needs at run time.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub workspace_root: PathBuf,
    pub target_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub scenario_bin: PathBuf,
    pub monitor_lib: PathBuf,
}

/// Where the §24 evidence artifact is written.
#[must_use]
pub fn evidence_path(target_dir: &Path) -> PathBuf {
    target_dir
        .join("security-evidence")
        .join("egress-trace.json")
}

#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    target: Option<CargoTarget>,
    #[serde(default)]
    filenames: Option<Vec<PathBuf>>,
    #[serde(default)]
    executable: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    #[serde(default)]
    kind: Vec<String>,
}

impl Fixture {
    /// Build the monitor `cdylib` and the scenario binary into the same target
    /// directory the running test lives in, and return their exact paths as
    /// reported by cargo.
    pub fn build() -> Result<Fixture, FixtureError> {
        let workspace_root = workspace_root();
        let (target_dir, profile_dir) = target_dirs()?;

        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(&cargo);
        command
            .arg("build")
            .arg("--manifest-path")
            .arg(workspace_root.join("Cargo.toml"))
            .arg("-p")
            .arg("plaudon-egress-monitor")
            .arg("-p")
            .arg("plaudon-egress-scenario")
            // The injected artifact must have the hooks compiled in; the copy
            // this harness links does not.
            .arg("--features")
            .arg("plaudon-egress-monitor/interpose")
            .arg("--message-format=json-render-diagnostics")
            .env("CARGO_TARGET_DIR", &target_dir)
            // The parent cargo's jobserver file descriptors are not inherited
            // by this child, so passing its handle along would hang the build.
            .env_remove("CARGO_MAKEFLAGS")
            .env_remove("MAKEFLAGS")
            .env_remove("RUSTC_WORKSPACE_WRAPPER");
        if let Some(flag) = profile_flag(&profile_dir) {
            command.args(flag);
        }

        let rendered = format!(
            "{} build -p plaudon-egress-monitor -p plaudon-egress-scenario",
            Path::new(&cargo).display()
        );
        let output = command.output()?;
        if !output.status.success() {
            return Err(FixtureError::Build {
                command: rendered,
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let mut scenario_bin = None;
        let mut monitor_lib = None;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Ok(message) = serde_json::from_str::<CargoMessage>(line) else {
                continue;
            };
            if message.reason != "compiler-artifact" {
                continue;
            }
            let Some(target) = message.target.as_ref() else {
                continue;
            };
            if target.name == "plaudon-egress-scenario" && target.kind.iter().any(|k| k == "bin") {
                scenario_bin.clone_from(&message.executable);
            }
            if target.kind.iter().any(|k| k == "cdylib") {
                if let Some(files) = message.filenames.as_ref() {
                    monitor_lib = files
                        .iter()
                        .find(|p| {
                            p.extension()
                                .and_then(|e| e.to_str())
                                .is_some_and(|ext| matches!(ext, "dylib" | "so" | "dll"))
                        })
                        .cloned();
                }
            }
        }

        let scenario_bin = scenario_bin.ok_or_else(|| FixtureError::ArtifactMissing {
            what: "the plaudon-egress-scenario binary".to_owned(),
            hint: "cargo reported no `bin` artifact for the package".to_owned(),
        })?;
        let monitor_lib = monitor_lib.ok_or_else(|| FixtureError::ArtifactMissing {
            what: "the plaudon-egress-monitor cdylib".to_owned(),
            hint: "cargo reported no `cdylib` artifact; the monitor cannot be injected".to_owned(),
        })?;

        // Scratch from a previous run of this test binary is removed once, up
        // front; individual runs then get their own directory so tests running
        // in parallel cannot clobber each other.
        let scratch = target_dir.join("egress-fixture");
        if scratch.exists() {
            fs::remove_dir_all(&scratch)?;
        }

        Ok(Fixture {
            workspace_root,
            target_dir,
            profile_dir,
            scenario_bin,
            monitor_lib,
        })
    }

    /// Scratch directory for one run: unique, so concurrent tests that use the
    /// same scenario do not share state.
    fn run_dir(&self, scenario: Scenario, injection: Injection) -> PathBuf {
        static RUN_SEQ: AtomicU64 = AtomicU64::new(0);
        let suffix = match injection {
            Injection::Enabled => "",
            Injection::Disabled => "-uninjected",
        };
        let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
        self.target_dir
            .join("egress-fixture")
            .join(format!("{}{suffix}-{seq}", scenario.name()))
    }

    /// Run one scenario as a child process and collect everything it produced.
    pub fn run(
        &self,
        scenario: Scenario,
        injection: Injection,
    ) -> Result<ScenarioRun, FixtureError> {
        let run_dir = self.run_dir(scenario, injection);
        let workdir = run_dir.join("work");
        fs::create_dir_all(&workdir)?;
        let trace_path = run_dir.join("trace.jsonl");
        let decisions_path = run_dir.join("decisions.json");
        fs::write(&trace_path, b"")?;

        let mut command = Command::new(&self.scenario_bin);
        command
            .arg(scenario.name())
            .current_dir(&run_dir)
            .env(plaudon_egress_monitor::LOG_ENV_NAME, &trace_path)
            .env(DECISIONS_ENV, &decisions_path)
            .env(WORKDIR_ENV, &workdir);
        match (injection, STATUS.injection_env()) {
            (Injection::Enabled, Some(var)) => {
                command.env(var, &self.monitor_lib);
            }
            (Injection::Enabled, None) => {
                return Err(FixtureError::ArtifactMissing {
                    what: "an injection mechanism".to_owned(),
                    hint: format!("{STATUS:?} has none on this platform"),
                })
            }
            (Injection::Disabled, Some(var)) => {
                command.env_remove(var);
            }
            (Injection::Disabled, None) => {}
        }

        let output = command.output()?;
        let trace_text = fs::read_to_string(&trace_path)?;
        let trace = Trace::parse(&trace_text).map_err(FixtureError::Trace)?;
        let summary = match fs::read(&decisions_path) {
            Ok(bytes) => Some(serde_json::from_slice::<ScenarioSummary>(&bytes).map_err(
                |err| FixtureError::Summary {
                    path: decisions_path.clone(),
                    message: err.to_string(),
                },
            )?),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(FixtureError::Io(err)),
        };

        Ok(ScenarioRun {
            scenario,
            injection,
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            trace,
            summary,
            trace_path,
            decisions_path,
        })
    }
}

/// One completed scenario execution.
#[derive(Debug, Clone)]
pub struct ScenarioRun {
    pub scenario: Scenario,
    pub injection: Injection,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub trace: Trace,
    pub summary: Option<ScenarioSummary>,
    pub trace_path: PathBuf,
    pub decisions_path: PathBuf,
}

/// The workspace root, derived from this package's manifest directory.
#[must_use]
pub fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // tests/egress/harness -> tests/egress -> tests -> <root>
    manifest
        .ancestors()
        .nth(3)
        .map_or_else(|| manifest.clone(), Path::to_path_buf)
}

/// `(target dir, profile dir)` derived from the running test executable, so a
/// custom `CARGO_TARGET_DIR` or `--release` run is honoured.
///
/// The path from the target directory down to a test binary is not fixed.
/// Cargo has long used `<target>/<profile>/deps/<test>`, and the
/// build-directory layout the coverage run's toolchain produces uses
/// `<target>/<profile>/build/<package>/<hash>/out/<test>`. Counting
/// components therefore mistakes a hash for the profile name — observed as
/// the child build failing with ``profile `e9765538642806e4` is not
/// defined``. The target directory is instead found by the marker cargo
/// writes into it, and the profile directory is its child on the way down.
pub fn target_dirs() -> Result<(PathBuf, PathBuf), FixtureError> {
    let exe = std::env::current_exe()?;
    let mut profile_dir = None;
    for directory in exe.ancestors().skip(1) {
        if directory.join("CACHEDIR.TAG").is_file() {
            let profile_dir = profile_dir.ok_or_else(|| FixtureError::ArtifactMissing {
                what: "the profile directory below the cargo target directory".to_owned(),
                hint: exe.display().to_string(),
            })?;
            return Ok((directory.to_path_buf(), profile_dir));
        }
        profile_dir = Some(directory.to_path_buf());
    }
    Err(FixtureError::ArtifactMissing {
        what: "the cargo target directory above the test executable".to_owned(),
        hint: exe.display().to_string(),
    })
}

/// Translate a profile *directory* name back into the cargo flag that selects
/// it, so the child build lands next to the test that is running.
#[must_use]
pub fn profile_flag(profile_dir: &Path) -> Option<[String; 2]> {
    let name = profile_dir.file_name()?.to_str()?;
    match name {
        "debug" => None,
        other => Some(["--profile".to_owned(), other.to_owned()]),
    }
}

/// True when this build of the monitor can observe anything at all.
#[must_use]
pub fn monitor_supported() -> bool {
    STATUS.is_supported()
}

/// The label used for the `monitor` field of the evidence artifact.
#[must_use]
pub fn monitor_label() -> &'static str {
    STATUS.label()
}

/// Whether an unsupported platform was explicitly acknowledged.
#[must_use]
pub fn unsupported_acknowledged() -> bool {
    std::env::var(plaudon_egress_monitor::ALLOW_UNSUPPORTED_ENV).is_ok_and(|value| value == "1")
}

/// Guard against a vacuous pass on a platform with no interception mechanism.
pub fn require_supported_monitor() -> Result<(), crate::verify::HarnessFailure> {
    if monitor_supported() || unsupported_acknowledged() {
        return Ok(());
    }
    Err(crate::verify::HarnessFailure::MonitorUnsupported {
        platform: std::env::consts::OS.to_owned(),
    })
}

/// Sanity check used by the tests: the status is one of the two implemented
/// mechanisms on the platforms CI runs.
#[must_use]
pub fn expected_status() -> MonitorStatus {
    STATUS
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn workspace_root_contains_the_workspace_manifest() {
        let root = workspace_root();
        let manifest = root.join("Cargo.toml");
        assert!(manifest.is_file(), "{} is not a file", manifest.display());
        let text = fs::read_to_string(&manifest).unwrap();
        assert!(
            text.contains("[workspace]"),
            "{} is not the workspace root",
            manifest.display()
        );
    }

    #[test]
    fn target_dirs_point_at_a_real_cargo_target_directory() {
        let (target_dir, profile_dir) = target_dirs().unwrap();
        assert!(target_dir.join("CACHEDIR.TAG").is_file());
        assert_eq!(profile_dir.parent(), Some(target_dir.as_path()));
        // The name is handed to the child build as `--profile`, so it has to
        // be the profile cargo built this test with and not an intermediate
        // directory on the way down to the executable.
        let name = profile_dir.file_name().unwrap().to_str().unwrap();
        assert!(
            matches!(name, "debug" | "release") || !name.chars().all(|c| c.is_ascii_hexdigit()),
            "{name} does not look like a cargo profile directory"
        );
    }

    #[test]
    fn profile_flag_only_set_for_non_default_profiles() {
        assert_eq!(profile_flag(Path::new("/t/debug")), None);
        assert_eq!(
            profile_flag(Path::new("/t/release")),
            Some(["--profile".to_owned(), "release".to_owned()])
        );
    }

    #[test]
    fn evidence_path_is_the_section_24_location() {
        assert!(evidence_path(Path::new("/t")).ends_with("security-evidence/egress-trace.json"));
    }
}
