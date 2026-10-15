//! Subprocess chokepoint for network-capable child processes (D1.1, §16.2A, §7.1).
//!
//! The official PLAUD Cloud CLI is a separate executable that may issue network
//! requests outside our HTTP client, so §16.2A requires that we **never invoke
//! it in Local-only Mode**. This module is the only place that may spawn it.
//!
//! Hardening per §7.1:
//! * hardcoded program leaf name selected during discovery, never an arbitrary
//!   user-supplied executable path;
//! * typed, strictly validated arguments — no shell, no `cmd.exe`, no
//!   `PowerShell`, no string concatenation;
//! * a scrubbed environment, so no secret ever reaches a subprocess env var
//!   (§16.4);
//! * bounded stdout/stderr capture and a wall-clock timeout.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{
    Destination, EgressDenied, EgressPurpose, EgressRequest, EgressTicket, OutboundPolicy,
    PayloadClass, Subsystem,
};

/// Arguments we are willing to pass to the cloud CLI. Modelling them as an enum
/// rather than strings makes flag injection unrepresentable: a hostile title or
/// search term can only ever land in a value position, and values are validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliArg {
    /// A fixed subcommand or flag from our own vocabulary.
    Literal(&'static str),
    /// An opaque identifier produced by the CLI itself (IDs, cursors).
    Identifier(String),
    /// A filesystem path we control (already canonicalized by `fsguard`).
    Path(PathBuf),
    /// A bounded integer.
    Number(i64),
}

/// Why an argument was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgError {
    #[error("argument contains a control character or NUL")]
    ControlCharacter,
    #[error("argument looks like a flag and may not come from untrusted input")]
    FlagInjection,
    #[error("argument exceeds the maximum length")]
    TooLong,
    #[error("identifier contains characters outside [A-Za-z0-9._:-]")]
    IllegalIdentifier,
    #[error("path is not absolute")]
    RelativePath,
    #[error("path contains a non-UTF-8 component")]
    NonUtf8Path,
}

/// Maximum length of a single argument. The CLI never legitimately needs more.
pub const MAX_ARG_LEN: usize = 512;

impl CliArg {
    /// Render the argument, validating it. Untrusted text can only enter via
    /// [`CliArg::Identifier`], which is restricted to an identifier charset, so
    /// shell metacharacters, newlines, NUL bytes and leading `-` are rejected
    /// rather than escaped.
    pub fn render(&self) -> Result<OsString, ArgError> {
        match self {
            CliArg::Literal(s) => Ok(OsString::from(*s)),
            CliArg::Identifier(s) => {
                if s.len() > MAX_ARG_LEN {
                    return Err(ArgError::TooLong);
                }
                if s.chars().any(char::is_control) {
                    return Err(ArgError::ControlCharacter);
                }
                if s.starts_with('-') {
                    return Err(ArgError::FlagInjection);
                }
                if !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
                {
                    return Err(ArgError::IllegalIdentifier);
                }
                Ok(OsString::from(s))
            }
            CliArg::Path(p) => {
                if !p.is_absolute() {
                    return Err(ArgError::RelativePath);
                }
                let s = p.to_str().ok_or(ArgError::NonUtf8Path)?;
                if s.len() > MAX_ARG_LEN * 4 {
                    return Err(ArgError::TooLong);
                }
                if s.chars().any(char::is_control) {
                    return Err(ArgError::ControlCharacter);
                }
                Ok(OsString::from(s))
            }
            CliArg::Number(n) => Ok(OsString::from(n.to_string())),
        }
    }
}

/// Environment variables that must never reach a child process, because they
/// change how it resolves code or where it sends data (§7.1).
pub const STRIPPED_ENV_PREFIXES: [&str; 4] = ["LD_", "DYLD_", "PLAUDON_SECRET", "NODE_OPTIONS"];
/// Exact environment variable names that must never reach a child process.
pub const STRIPPED_ENV_NAMES: [&str; 10] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "OPENROUTER_API_KEY",
    "PLAUD_TOKEN",
    "PLAUD_API_KEY",
    "AWS_SECRET_ACCESS_KEY",
];

/// Failure launching or running a gated subprocess.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("egress policy denied the subprocess: {0}")]
    Denied(#[from] EgressDenied),
    #[error("invalid argument: {0}")]
    Arg(#[from] ArgError),
    #[error("program not found at the discovered path")]
    ProgramNotFound,
    #[error("subprocess failed to start: {0}")]
    Start(String),
    #[error("subprocess exceeded its time budget")]
    Timeout,
}

/// A validated, policy-approved subprocess invocation.
///
/// Holding one is proof the policy allowed the launch; `into_command` consumes
/// the ticket.
#[derive(Debug)]
pub struct GatedCommand {
    ticket: EgressTicket,
    program: PathBuf,
    args: Vec<OsString>,
    env: BTreeMap<String, String>,
    timeout: Duration,
    max_output_bytes: usize,
}

impl GatedCommand {
    #[must_use]
    pub fn ticket(&self) -> &EgressTicket {
        &self.ticket
    }
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }
    #[must_use]
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    #[must_use]
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    /// Build the `std::process::Command`. No shell is involved at any point.
    #[must_use]
    pub fn into_command(self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.args);
        cmd.env_clear();
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd
    }
}

/// Directories where the runtime the cloud CLI needs is commonly installed.
///
/// `@plaud-ai/cli` is a Node.js script (`#!/usr/bin/env node`). An app
/// launched from Finder or the Dock inherits launchd's minimal `PATH`
/// (`/usr/bin:/bin:/usr/sbin:/sbin`), where `env` cannot find `node` even
/// though Homebrew or the official installer put it here (#304). This is a
/// fixed allowlist of package-manager prefixes, never the output of a login
/// shell: running the user's shell profile to learn a `PATH` would execute
/// arbitrary user code before every launch.
#[cfg(not(windows))]
pub const CLOUD_CLI_RUNTIME_DIRS: &[&str] =
    &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"];
/// Directories where the runtime the cloud CLI needs is commonly installed.
///
/// Empty on Windows: the tool there is a native `plaud.exe` and nothing is
/// resolved through `PATH` on its behalf.
#[cfg(windows)]
pub const CLOUD_CLI_RUNTIME_DIRS: &[&str] = &[];

/// The environment a cloud-CLI child is started with.
///
/// Production uses [`CliChildEnvironment::ambient`]: the scrubbed parent
/// environment and [`CLOUD_CLI_RUNTIME_DIRS`]. Tests inject a minimal one to
/// reproduce a Finder launch without touching the process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliChildEnvironment {
    base: BTreeMap<String, String>,
    runtime_dirs: Vec<PathBuf>,
}

impl CliChildEnvironment {
    /// The scrubbed environment of this process and the fixed runtime dirs.
    #[must_use]
    pub fn ambient() -> Self {
        Self::new(
            scrubbed_env(),
            CLOUD_CLI_RUNTIME_DIRS.iter().map(PathBuf::from).collect(),
        )
    }

    /// An explicit base environment and runtime directory list. The base is
    /// scrubbed exactly like the ambient one, so a test cannot smuggle a
    /// secret into a child either.
    #[must_use]
    pub fn new(base: BTreeMap<String, String>, runtime_dirs: Vec<PathBuf>) -> Self {
        Self {
            base: base
                .into_iter()
                .filter(|(name, _)| !is_stripped_env(name))
                .collect(),
            runtime_dirs,
        }
    }

    /// The environment for one launch of `program`.
    #[must_use]
    pub fn env_for(&self, program: &Path) -> BTreeMap<String, String> {
        let mut env = self.base.clone();
        let path = cloud_cli_search_path(
            program,
            self.base.get("PATH").map(String::as_str),
            &self.runtime_dirs,
        );
        if !path.is_empty() {
            env.insert("PATH".to_owned(), path);
        }
        env
    }
}

/// The `PATH` a cloud-CLI child gets.
///
/// In order, without duplicates: the directory the tool itself was found in
/// (npm puts `plaud` next to the `node` that installed it), the absolute
/// entries of the ambient `PATH`, then [`CLOUD_CLI_RUNTIME_DIRS`]. Relative
/// entries are dropped, because they resolve against a working directory we do
/// not control.
#[must_use]
pub fn cloud_cli_search_path(
    program: &Path,
    ambient_path: Option<&str>,
    runtime_dirs: &[PathBuf],
) -> String {
    let mut entries: Vec<PathBuf> = Vec::new();
    let mut push = |entry: PathBuf| {
        if entry.is_absolute() && !entries.contains(&entry) {
            entries.push(entry);
        }
    };
    if let Some(directory) = program.parent() {
        push(directory.to_path_buf());
    }
    if let Some(ambient) = ambient_path {
        std::env::split_paths(ambient).for_each(&mut push);
    }
    runtime_dirs.iter().cloned().for_each(&mut push);
    std::env::join_paths(&entries).map_or_else(
        // An entry holding the separator itself cannot be joined; such an
        // entry is unusable as a search directory anyway, so drop it.
        |_| {
            let usable: Vec<&PathBuf> = entries
                .iter()
                .filter(|entry| std::env::join_paths([entry]).is_ok())
                .collect();
            std::env::join_paths(usable)
                .map(|joined| joined.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        |joined| joined.to_string_lossy().into_owned(),
    )
}

/// The only launcher for the network-capable cloud CLI.
#[derive(Debug, Clone)]
pub struct GatedProcessLauncher {
    policy: Arc<OutboundPolicy>,
    child_environment: Option<Arc<CliChildEnvironment>>,
}

impl GatedProcessLauncher {
    #[must_use]
    pub fn new(policy: Arc<OutboundPolicy>) -> Self {
        Self {
            policy,
            child_environment: None,
        }
    }

    /// Start children with this environment instead of the ambient one.
    #[must_use]
    pub fn with_child_environment(mut self, environment: CliChildEnvironment) -> Self {
        self.child_environment = Some(Arc::new(environment));
        self
    }

    /// Prepare a launch of the discovered cloud CLI.
    ///
    /// Returns `Err(SpawnError::Denied)` with [`super::DenyReason::LocalOnlyMode`]
    /// whenever Local-only Mode is on, which is how §22(15) ("never launches the
    /// PLAUD Cloud CLI") is enforced rather than merely documented.
    pub fn prepare_cloud_cli(
        &self,
        discovered_program: &Path,
        args: &[CliArg],
        purpose: EgressPurpose,
    ) -> Result<GatedCommand, SpawnError> {
        let program_label = discovered_program
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<non-utf8>")
            .to_owned();

        let request = EgressRequest::new(
            Subsystem::PlaudCloudCli,
            Destination::subprocess(program_label),
            purpose,
            PayloadClass::Metadata,
        );
        let ticket = self.policy.authorize(&request)?;

        let mut rendered = Vec::with_capacity(args.len());
        for arg in args {
            rendered.push(arg.render()?);
        }

        Ok(GatedCommand {
            ticket,
            program: discovered_program.to_path_buf(),
            args: rendered,
            env: self.child_environment.as_ref().map_or_else(
                || CliChildEnvironment::ambient().env_for(discovered_program),
                |environment| environment.env_for(discovered_program),
            ),
            timeout: Duration::from_secs(120),
            max_output_bytes: 8 * 1024 * 1024,
        })
    }
}

/// Build the child environment: start from the parent, remove everything that
/// could redirect code loading or carry a secret.
#[must_use]
pub fn scrubbed_env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, _)| !is_stripped_env(name))
        .collect()
}

#[must_use]
pub fn is_stripped_env(name: &str) -> bool {
    STRIPPED_ENV_NAMES.contains(&name)
        || STRIPPED_ENV_PREFIXES.iter().any(|p| name.starts_with(p))
        || name.to_ascii_uppercase().contains("TOKEN")
        || name.to_ascii_uppercase().contains("SECRET")
        || name.to_ascii_uppercase().contains("API_KEY")
        || name.to_ascii_uppercase().contains("PASSWORD")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbound::{ApprovalSource, DenyReason, NetworkGrant};

    fn launcher(policy: OutboundPolicy) -> GatedProcessLauncher {
        GatedProcessLauncher::new(Arc::new(policy))
    }

    #[test]
    fn local_only_never_launches_the_cloud_cli() {
        let l = launcher(OutboundPolicy::local_only());
        let err = l
            .prepare_cloud_cli(
                Path::new("/usr/local/bin/plaud"),
                &[CliArg::Literal("list")],
                EgressPurpose::AccountSync,
            )
            .unwrap_err();
        match err {
            SpawnError::Denied(d) => assert_eq!(d.reason, DenyReason::LocalOnlyMode),
            other => panic!("expected denial, got {other:?}"),
        }
    }

    #[test]
    fn permitted_mode_prepares_the_pinned_program() {
        let l = launcher(OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            0,
            "privacy-v1",
        )));
        let cmd = l
            .prepare_cloud_cli(
                Path::new("/usr/local/bin/plaud"),
                &[CliArg::Literal("list"), CliArg::Number(50)],
                EgressPurpose::AccountSync,
            )
            .unwrap();
        assert_eq!(cmd.args(), [OsString::from("list"), OsString::from("50")]);
    }

    #[test]
    fn a_non_pinned_program_is_refused_even_when_network_is_permitted() {
        let l = launcher(OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            0,
            "privacy-v1",
        )));
        let err = l
            .prepare_cloud_cli(
                Path::new("/tmp/evil"),
                &[CliArg::Literal("list")],
                EgressPurpose::AccountSync,
            )
            .unwrap_err();
        match err {
            SpawnError::Denied(d) => assert_eq!(d.reason, DenyReason::DestinationNotAllowlisted),
            other => panic!("expected denial, got {other:?}"),
        }
    }

    #[test]
    fn hostile_identifiers_cannot_inject_flags_or_shell_syntax() {
        let hostile = [
            "--delete-all",
            "-rf",
            "a;rm -rf /",
            "a`id`",
            "a$(id)",
            "a|b",
            "a&b",
            "a>b",
            "a\nb",
            "a\0b",
            "a'b",
            "a\"b",
            "a b",
            "a\\b",
            "%SYSTEMROOT%",
            "$HOME",
        ];
        for h in hostile {
            let err = CliArg::Identifier(h.to_owned()).render().unwrap_err();
            assert!(
                matches!(
                    err,
                    ArgError::FlagInjection
                        | ArgError::IllegalIdentifier
                        | ArgError::ControlCharacter
                ),
                "{h:?} produced {err:?}"
            );
        }
    }

    #[test]
    fn legitimate_identifiers_pass() {
        for ok in ["rec_01HF8", "abc-123", "a.b:c", "ID_0"] {
            CliArg::Identifier(ok.to_owned()).render().unwrap();
        }
    }

    #[test]
    fn overlong_identifier_is_refused() {
        let long = "a".repeat(MAX_ARG_LEN + 1);
        assert_eq!(
            CliArg::Identifier(long).render().unwrap_err(),
            ArgError::TooLong
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn issue_304_child_path_adds_the_tool_dir_and_the_runtime_allowlist() {
        // A Finder launch: launchd's PATH, which holds no node.
        let path = cloud_cli_search_path(
            Path::new("/opt/homebrew/bin/plaud"),
            Some("/usr/bin:/bin:/usr/sbin:/sbin"),
            &[
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ],
        );
        assert_eq!(
            path, "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin",
            "the tool's own directory first, the ambient PATH, then the allowlist, deduplicated"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn issue_304_child_path_drops_relative_entries_and_survives_no_path() {
        let path = cloud_cli_search_path(
            Path::new("/Users/u/.npm-global/bin/plaud"),
            Some(".:bin::/usr/bin"),
            &[PathBuf::from("/usr/local/bin")],
        );
        assert_eq!(path, "/Users/u/.npm-global/bin:/usr/bin:/usr/local/bin");
        let path = cloud_cli_search_path(Path::new("/usr/local/bin/plaud"), None, &[]);
        assert_eq!(path, "/usr/local/bin");
    }

    #[cfg(not(windows))]
    #[test]
    fn issue_304_the_prepared_command_carries_the_widened_path_and_no_secret() {
        let l = launcher(OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            0,
            "privacy-v1",
        )))
        .with_child_environment(CliChildEnvironment::new(
            BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("PLAUD_TOKEN".to_owned(), "nope".to_owned()),
                ("HOME".to_owned(), "/Users/u".to_owned()),
            ]),
            vec![PathBuf::from("/opt/homebrew/bin")],
        ));
        let cmd = l
            .prepare_cloud_cli(
                Path::new("/usr/local/bin/plaud"),
                &[CliArg::Literal("version")],
                EgressPurpose::Diagnostic,
            )
            .unwrap();
        assert_eq!(
            cmd.env().get("PATH").map(String::as_str),
            Some("/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin")
        );
        assert!(!cmd.env().contains_key("PLAUD_TOKEN"));
        assert_eq!(cmd.env().get("HOME").map(String::as_str), Some("/Users/u"));
    }

    #[test]
    fn env_scrubbing_removes_secret_and_loader_variables() {
        for name in [
            "OPENROUTER_API_KEY",
            "PLAUD_TOKEN",
            "DYLD_INSERT_LIBRARIES",
            "LD_PRELOAD",
            "HTTPS_PROXY",
            "MY_SECRET_THING",
            "SOME_PASSWORD",
            "X_API_KEY",
        ] {
            assert!(is_stripped_env(name), "{name} should be stripped");
        }
        assert!(!is_stripped_env("PATH"));
        assert!(!is_stripped_env("HOME"));
    }
}
