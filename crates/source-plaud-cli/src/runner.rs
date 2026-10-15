//! The hardened process runner (spec 7.1, 18.5, 22(15)).
//!
//! Everything dangerous about launching a third-party executable is handled in
//! one place:
//!
//! | Risk | Control |
//! |---|---|
//! | Shell interpretation, `cmd.exe` re-parsing | argument array only, via [`CliCommand::args`] |
//! | Flag or metacharacter injection | `CliArg` validation; hostile values are refused, not escaped |
//! | Loader and proxy environment overrides | [`plaudon_security::outbound::subprocess::scrubbed_env`] |
//! | Secrets leaking into a child's environment | the same scrub, which drops anything token-shaped |
//! | A hung vendor process | wall-clock budget, then kill, then `SourceUnavailable` |
//! | Unbounded output | per-stream byte caps; exceeding one kills the child and yields `MalformedOutput` |
//! | Launching at all in Local-only Mode | [`plaudon_security::outbound::OutboundPolicy`] denies before a process exists |
//! | Tokens and signed URLs in logs | every emitted line goes through [`plaudon_diagnostics::redact::Redactor`] |
//!
//! The runner never returns vendor stderr to a caller that might show it to a
//! user; it returns a redacted *excerpt* that the diagnostics layer may log,
//! and a classified error (see [`crate::errors`]).

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_diagnostics::redact::Redactor;
use plaudon_recording_sources::source::SourceError;
use plaudon_security::outbound::subprocess::{GatedProcessLauncher, SpawnError};
use plaudon_security::outbound::{DenyReason, EgressPurpose};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::invocation::CliCommand;

/// Proof that a human asked for this invocation.
///
/// Spec 22(15) and 4.2.3 both require the vendor tool to run only after an
/// explicit user action. Requiring this token in [`CliRunner::run`] keeps a
/// background timer from acquiring one by accident: there is no `Default` and
/// no way to build one without naming the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInitiated {
    /// The user-facing action that triggered the run.
    pub action: &'static str,
    /// Milliseconds since the Unix epoch at which the user acted.
    pub at_ms: i64,
}

impl UserInitiated {
    /// Record a user action.
    #[must_use]
    pub const fn new(action: &'static str, at_ms: i64) -> Self {
        Self { action, at_ms }
    }
}

/// Per-call resource budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    /// Wall-clock budget for the child.
    pub timeout: Duration,
    /// Maximum bytes captured from stdout.
    pub max_stdout_bytes: usize,
    /// Maximum bytes captured from stderr.
    pub max_stderr_bytes: usize,
}

impl RunLimits {
    /// Budget for a quick, non-interactive call (`version`, `audio`).
    pub const PROBE: Self = Self {
        timeout: Duration::from_secs(10),
        max_stdout_bytes: 64 * 1024,
        max_stderr_bytes: 64 * 1024,
    };
    /// Budget for enumeration, which may legitimately return a large listing.
    pub const ENUMERATE: Self = Self {
        timeout: Duration::from_secs(90),
        max_stdout_bytes: 8 * 1024 * 1024,
        max_stderr_bytes: 256 * 1024,
    };
    /// Budget for a browser login, which waits on a human.
    pub const INTERACTIVE: Self = Self {
        timeout: Duration::from_secs(300),
        max_stdout_bytes: 256 * 1024,
        max_stderr_bytes: 256 * 1024,
    };
}

impl Default for RunLimits {
    fn default() -> Self {
        Self::ENUMERATE
    }
}

/// What a completed run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    /// Captured stdout, decoded as UTF-8.
    ///
    /// Held in memory only for as long as the parser needs it. It is never
    /// logged: a listing contains recording titles, and `plaud audio` output
    /// contains a signed URL.
    pub stdout: String,
    /// Redacted stderr excerpt, safe to log, never shown to a user.
    pub stderr_excerpt: String,
    /// Process exit code, absent when the process was signalled.
    pub exit_code: Option<i32>,
    /// Wall-clock duration of the run.
    pub elapsed: Duration,
}

impl CliOutput {
    /// Whether the process reported success.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        matches!(self.exit_code, Some(0))
    }
}

/// Why a run failed before it produced classified output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// Local-only Mode, or another egress rule, refused the launch.
    #[error("the egress policy refused to launch the vendor tool")]
    Denied {
        /// Why the policy refused.
        reason: DenyReason,
    },
    /// An argument could not be rendered safely.
    #[error("an argument could not be passed safely to the vendor tool")]
    UnsafeArgument,
    /// The executable could not be started.
    #[error("the vendor tool could not be started")]
    StartFailed,
    /// The executable exists, but the interpreter its `#!` line names could
    /// not be found, so the kernel refused to start it (#304).
    #[error("the vendor tool's interpreter could not be found")]
    InterpreterMissing,
    /// The child exceeded its wall-clock budget and was killed.
    #[error("the vendor tool did not finish within its time budget")]
    Timeout,
    /// A stream exceeded its byte cap; the child was killed.
    #[error("the vendor tool produced more output than we will read")]
    OutputTooLarge {
        /// Which stream overflowed.
        stream: &'static str,
        /// The cap that was exceeded.
        limit_bytes: usize,
    },
    /// stdout was not valid UTF-8.
    #[error("the vendor tool produced output that is not text")]
    NonUtf8Output,
}

impl From<RunError> for SourceError {
    fn from(error: RunError) -> Self {
        match error {
            // Local-only Mode is a user setting, not a vendor failure. It is
            // reported as "the source is unavailable" and the diagnostics layer
            // explains which setting to change.
            RunError::Denied { .. }
            | RunError::StartFailed
            | RunError::InterpreterMissing
            | RunError::Timeout => Self::SourceUnavailable,
            RunError::UnsafeArgument
            | RunError::OutputTooLarge { .. }
            | RunError::NonUtf8Output => Self::MalformedOutput,
        }
    }
}

/// Launches the vendor CLI under policy.
#[derive(Debug, Clone)]
pub struct CliRunner {
    launcher: GatedProcessLauncher,
    redactor: Arc<Redactor>,
    stderr_excerpt_bytes: usize,
}

impl CliRunner {
    /// Longest redacted stderr excerpt we retain for diagnostics.
    pub const DEFAULT_STDERR_EXCERPT_BYTES: usize = 2048;

    /// Build a runner over an egress-gated launcher.
    #[must_use]
    pub fn new(launcher: GatedProcessLauncher, redactor: Arc<Redactor>) -> Self {
        Self {
            launcher,
            redactor,
            stderr_excerpt_bytes: Self::DEFAULT_STDERR_EXCERPT_BYTES,
        }
    }

    /// Run one command.
    ///
    /// `program` must be a path produced by [`crate::discovery`]; the egress
    /// policy independently refuses a program it does not recognize, so a
    /// caller that bypasses discovery still cannot launch an arbitrary binary.
    pub async fn run(
        &self,
        program: &Path,
        command: &CliCommand,
        limits: RunLimits,
        intent: &UserInitiated,
    ) -> Result<CliOutput, RunError> {
        let args = command.args().map_err(|_| RunError::UnsafeArgument)?;
        let gated = self
            .launcher
            .prepare_cloud_cli(program, &args, egress_purpose(command))
            .map_err(|error| match error {
                SpawnError::Denied(denied) => RunError::Denied {
                    reason: denied.reason,
                },
                SpawnError::Arg(_) => RunError::UnsafeArgument,
                SpawnError::ProgramNotFound | SpawnError::Start(_) => RunError::StartFailed,
                SpawnError::Timeout => RunError::Timeout,
            })?;

        // Never exceed the budget the security crate attached to the ticket.
        let timeout = limits.timeout.min(gated.timeout());
        let stdout_cap = limits.max_stdout_bytes.min(gated.max_output_bytes());
        let stderr_cap = limits.max_stderr_bytes.min(gated.max_output_bytes());

        tracing::info!(
            target: "plaudon::plaud_cli",
            command = command.label(),
            action = intent.action,
            timeout_secs = timeout.as_secs(),
            "launching the vendor cloud CLI"
        );

        let mut std_command = gated.into_command();
        std_command.stdin(Stdio::null());
        std_command.stdout(Stdio::piped());
        std_command.stderr(Stdio::piped());
        let mut command_handle = Command::from(std_command);
        command_handle.kill_on_drop(true);

        let started = Instant::now();
        let mut child = command_handle.spawn().map_err(|error| {
            tracing::warn!(
                target: "plaudon::plaud_cli",
                command = command.label(),
                kind = ?error.kind(),
                "the vendor cloud CLI could not be started"
            );
            RunError::StartFailed
        })?;

        let mut stdout_pipe = child.stdout.take().ok_or(RunError::StartFailed)?;
        let mut stderr_pipe = child.stderr.take().ok_or(RunError::StartFailed)?;

        // Both pipes are drained concurrently, and the first overflow aborts
        // the pair: waiting for the other stream would mean waiting on a child
        // that is still writing into a pipe nobody is reading.
        let collect = async {
            let (stdout, stderr) = tokio::try_join!(
                read_capped(&mut stdout_pipe, stdout_cap, "stdout"),
                read_capped(&mut stderr_pipe, stderr_cap, "stderr"),
            )?;
            let status = child.wait().await;
            Ok::<_, RunError>((stdout, stderr, status))
        };

        let settled = tokio::time::timeout(timeout, collect).await;
        let (stdout, stderr, status) = match settled {
            Ok(Ok(parts)) => parts,
            Ok(Err(error)) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                tracing::warn!(
                    target: "plaudon::plaud_cli",
                    command = command.label(),
                    "the vendor cloud CLI exceeded its output cap and was killed"
                );
                return Err(error);
            }
            Err(_elapsed) => {
                // `kill_on_drop` handles the process, but we ask explicitly so
                // the kill is ordered before we report rather than at an
                // arbitrary later drop.
                let _ = child.start_kill();
                let _ = child.wait().await;
                tracing::warn!(
                    target: "plaudon::plaud_cli",
                    command = command.label(),
                    timeout_secs = timeout.as_secs(),
                    "the vendor cloud CLI was killed after exceeding its time budget"
                );
                return Err(RunError::Timeout);
            }
        };

        let stdout = String::from_utf8(stdout).map_err(|_| RunError::NonUtf8Output)?;
        let stderr_excerpt = self.excerpt(&stderr);
        let exit_code = status.ok().and_then(|status| status.code());

        tracing::info!(
            target: "plaudon::plaud_cli",
            command = command.label(),
            exit_code,
            elapsed_ms = started.elapsed().as_millis() as u64,
            stdout_bytes = stdout.len(),
            "the vendor cloud CLI finished"
        );

        Ok(CliOutput {
            stdout,
            stderr_excerpt,
            exit_code,
            elapsed: started.elapsed(),
        })
    }

    /// Redact and truncate a stderr capture so it is safe to log.
    ///
    /// Lossy UTF-8 decoding is deliberate: refusing to redact non-UTF-8 stderr
    /// would mean either dropping the diagnostic or logging raw bytes.
    fn excerpt(&self, stderr: &[u8]) -> String {
        let text = String::from_utf8_lossy(stderr);
        let redacted = self.redactor.redact(&text).into_owned();
        truncate_on_char_boundary(&redacted, self.stderr_excerpt_bytes).to_owned()
    }
}

/// The egress purpose each command serves (spec 16.2A).
const fn egress_purpose(command: &CliCommand) -> EgressPurpose {
    match command {
        CliCommand::Version => EgressPurpose::Diagnostic,
        // Sign-in and sign-out are account operations on the vendor side, and
        // `Subsystem::PlaudCloudCli` is only ever authorized for account sync
        // and diagnostics.
        CliCommand::Login | CliCommand::Logout => EgressPurpose::AccountSync,
        CliCommand::Files { .. } | CliCommand::Audio { .. } => EgressPurpose::AccountSync,
    }
}

/// Read a stream, failing once `limit` bytes have been exceeded.
///
/// Reads one byte past the limit on purpose: a stream that stops exactly at the
/// cap is complete, and one that has more to give is an overflow. The error
/// carries the limit so the caller can name it.
async fn read_capped<R>(
    reader: &mut R,
    limit: usize,
    stream: &'static str,
) -> Result<Vec<u8>, RunError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = Vec::with_capacity(limit.min(64 * 1024));
    // Heap, not stack: a 16 KiB array here lands in every future that awaits
    // this function and makes `CliRunner::run` a 34 KiB future.
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        let Ok(read) = reader.read(&mut chunk).await else {
            // A broken pipe means the child died; whatever we have is what
            // there is, and the exit status explains the rest.
            return Ok(buffer);
        };
        if read == 0 {
            return Ok(buffer);
        }
        if buffer.len() + read > limit {
            return Err(RunError::OutputTooLarge {
                stream,
                limit_bytes: limit,
            });
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Truncate to at most `max` bytes without splitting a character.
fn truncate_on_char_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
    use std::path::PathBuf;

    fn permitted_runner() -> CliRunner {
        let policy = OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            0,
            "privacy-v1",
        ));
        CliRunner::new(
            GatedProcessLauncher::new(Arc::new(policy)),
            Arc::new(Redactor::new()),
        )
    }

    fn local_only_runner() -> CliRunner {
        CliRunner::new(
            GatedProcessLauncher::new(Arc::new(OutboundPolicy::local_only())),
            Arc::new(Redactor::new()),
        )
    }

    /// Write a shell script that behaves like the vendor tool, at the one
    /// program path the egress policy pins.
    fn fake_cli(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("plaud");
        std::fs::write(&path, body).expect("write fake cli");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake cli");
        }
        path
    }

    fn intent() -> UserInitiated {
        UserInitiated::new("connect_plaud_account", 1_700_000_000_000)
    }

    #[tokio::test]
    async fn local_only_mode_never_launches_the_tool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(
            dir.path(),
            "#!/bin/sh\necho 'MUST NOT RUN' > \"$(dirname \"$0\")/ran\"\n",
        );
        let error = local_only_runner()
            .run(&program, &CliCommand::files(), RunLimits::PROBE, &intent())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            RunError::Denied {
                reason: DenyReason::LocalOnlyMode
            }
        );
        assert!(
            !dir.path().join("ran").exists(),
            "the process must never have started"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn arguments_arrive_literally_and_are_never_interpreted() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The script echoes its arguments one per line. If any layer handed
        // the vector to a shell, the metacharacters would not survive as text.
        let program = fake_cli(
            dir.path(),
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\n",
        );
        let output = permitted_runner()
            .run(
                &program,
                &CliCommand::Audio {
                    external_id: "rec_01HF8-ZT.a:b".to_owned(),
                },
                RunLimits::PROBE,
                &intent(),
            )
            .await
            .expect("ran");
        assert_eq!(output.stdout, "audio\nrec_01HF8-ZT.a:b\n");
        assert!(output.succeeded());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_environment_carries_no_secret_or_loader_variable() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Dump the child's whole environment and check every name against the
        // security crate's predicate. Asserting on the *result* rather than on
        // an injected variable keeps the test from mutating process state that
        // other tests in this binary share.
        let program = fake_cli(dir.path(), "#!/bin/sh\nenv\n");
        let output = permitted_runner()
            .run(&program, &CliCommand::Version, RunLimits::PROBE, &intent())
            .await
            .expect("ran");
        let names: Vec<&str> = output
            .stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, _)| name)
            .collect();
        assert!(!names.is_empty(), "the child must inherit something");
        for name in names {
            assert!(
                !plaudon_security::outbound::subprocess::is_stripped_env(name),
                "{name} reached the child"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hung_process_is_killed_at_the_deadline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(dir.path(), "#!/bin/sh\nsleep 30\n");
        let limits = RunLimits {
            timeout: Duration::from_millis(250),
            ..RunLimits::PROBE
        };
        let error = permitted_runner()
            .run(&program, &CliCommand::files(), limits, &intent())
            .await
            .unwrap_err();
        assert_eq!(error, RunError::Timeout);
        assert_eq!(SourceError::from(error), SourceError::SourceUnavailable);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_output_fails_closed_instead_of_filling_memory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(
            dir.path(),
            "#!/bin/sh\nwhile :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done\n",
        );
        let limits = RunLimits {
            timeout: Duration::from_secs(20),
            max_stdout_bytes: 4096,
            max_stderr_bytes: 4096,
        };
        let error = permitted_runner()
            .run(&program, &CliCommand::files(), limits, &intent())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            RunError::OutputTooLarge {
                stream: "stdout",
                limit_bytes: 4096
            }
        );
        assert_eq!(SourceError::from(error), SourceError::MalformedOutput);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_utf8_stdout_is_malformed_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(dir.path(), "#!/bin/sh\nprintf '\\377\\376bad'\n");
        let error = permitted_runner()
            .run(&program, &CliCommand::files(), RunLimits::PROBE, &intent())
            .await
            .unwrap_err();
        assert_eq!(error, RunError::NonUtf8Output);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_nonzero_exit_is_returned_for_classification_not_as_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(dir.path(), "#!/bin/sh\necho 'not logged in' >&2\nexit 7\n");
        let output = permitted_runner()
            .run(&program, &CliCommand::files(), RunLimits::PROBE, &intent())
            .await
            .expect("ran");
        assert_eq!(output.exit_code, Some(7));
        assert!(!output.succeeded());
        assert_eq!(output.stdout, "");
        assert!(output.stderr_excerpt.contains("not logged in"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stderr_excerpts_are_redacted_and_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(
            dir.path(),
            "#!/bin/sh\necho 'failed: https://assets.example.com/a.wav?X-Amz-Signature=deadbeefdeadbeefdeadbeefdeadbeef&X-Amz-Expires=86400' >&2\nexit 1\n",
        );
        let output = permitted_runner()
            .run(&program, &CliCommand::files(), RunLimits::PROBE, &intent())
            .await
            .expect("ran");
        assert!(
            !output.stderr_excerpt.contains("X-Amz-Signature=deadbeef"),
            "signed URL survived redaction: {}",
            output.stderr_excerpt
        );
        assert!(output.stderr_excerpt.len() <= CliRunner::DEFAULT_STDERR_EXCERPT_BYTES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_process_that_crashes_immediately_is_reported_not_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(dir.path(), "#!/bin/sh\nkill -9 $$\n");
        let output = permitted_runner()
            .run(&program, &CliCommand::files(), RunLimits::PROBE, &intent())
            .await
            .expect("ran");
        assert_eq!(output.exit_code, None, "a signalled process has no code");
    }

    #[tokio::test]
    async fn a_missing_executable_fails_to_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = permitted_runner()
            .run(
                &dir.path().join("plaud"),
                &CliCommand::files(),
                RunLimits::PROBE,
                &intent(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, RunError::StartFailed);
    }

    #[tokio::test]
    async fn an_unsafe_argument_never_reaches_the_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(dir.path(), "#!/bin/sh\nexit 0\n");
        let error = permitted_runner()
            .run(
                &program,
                &CliCommand::Audio {
                    external_id: "a; rm -rf /".to_owned(),
                },
                RunLimits::PROBE,
                &intent(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, RunError::UnsafeArgument);
    }

    #[test]
    fn issue_304_a_local_only_denial_is_not_reported_as_an_unreachable_service() {
        assert_eq!(
            SourceError::from(RunError::Denied {
                reason: DenyReason::LocalOnlyMode
            }),
            SourceError::LocalOnlyMode
        );
        assert_ne!(
            SourceError::from(RunError::Denied {
                reason: DenyReason::DestinationNotAllowlisted
            }),
            SourceError::LocalOnlyMode,
            "only Local-only Mode may be reported as Local-only Mode"
        );
        assert_eq!(
            SourceError::from(RunError::InterpreterMissing),
            SourceError::CliRuntimeMissing
        );
    }

    /// A runner whose children see what a Finder-launched app sees: launchd's
    /// minimal `PATH` and nothing else, plus `runtime_dirs` as the allowlist.
    #[cfg(unix)]
    fn finder_runner(runtime_dirs: Vec<PathBuf>) -> CliRunner {
        use plaudon_security::outbound::subprocess::CliChildEnvironment;
        use std::collections::BTreeMap;
        let policy = OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            0,
            "privacy-v1",
        ));
        let environment = CliChildEnvironment::new(
            BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
            runtime_dirs,
        );
        CliRunner::new(
            GatedProcessLauncher::new(Arc::new(policy)).with_child_environment(environment),
            Arc::new(Redactor::new()),
        )
    }

    /// A stand-in `node`: prints what it was asked to run.
    #[cfg(unix)]
    fn fake_node(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).expect("node dir");
        let node = dir.join("node");
        std::fs::write(&node, "#!/bin/sh\necho \"plaud 0.3.15\"\n").expect("write node");
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn issue_304_a_finder_path_still_finds_node_installed_next_to_the_tool() {
        let dir = tempfile::tempdir().expect("tempdir");
        fake_node(dir.path());
        let program = fake_cli(dir.path(), "#!/usr/bin/env node\n");
        let output = finder_runner(Vec::new())
            .run(&program, &CliCommand::Version, RunLimits::PROBE, &intent())
            .await
            .expect("ran");
        assert!(
            output.succeeded(),
            "exit {:?}, stderr {:?}",
            output.exit_code,
            output.stderr_excerpt
        );
        assert_eq!(output.stdout.trim(), "plaud 0.3.15");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn issue_304_a_finder_path_finds_node_in_an_allowlisted_directory() {
        let tool = tempfile::tempdir().expect("tempdir");
        let runtime = tempfile::tempdir().expect("tempdir");
        fake_node(runtime.path());
        let program = fake_cli(tool.path(), "#!/usr/bin/env node\n");
        let output = finder_runner(vec![runtime.path().to_path_buf()])
            .run(&program, &CliCommand::Version, RunLimits::PROBE, &intent())
            .await
            .expect("ran");
        assert!(
            output.succeeded(),
            "exit {:?}, stderr {:?}",
            output.exit_code,
            output.stderr_excerpt
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn issue_304_no_node_anywhere_is_classified_as_a_missing_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(dir.path(), "#!/usr/bin/env node\n");
        let output = finder_runner(Vec::new())
            .run(&program, &CliCommand::Version, RunLimits::PROBE, &intent())
            .await
            .expect("env itself starts; it is node that is missing");
        assert_eq!(output.exit_code, Some(127));
        assert_eq!(
            crate::errors::classify(&output, &CliCommand::Version),
            Some(SourceError::CliRuntimeMissing),
            "stderr {:?}",
            output.stderr_excerpt
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn issue_304_an_absolute_interpreter_that_is_missing_is_a_missing_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = fake_cli(dir.path(), "#!/nonexistent-issue-304/bin/node\n");
        let error = permitted_runner()
            .run(&program, &CliCommand::Version, RunLimits::PROBE, &intent())
            .await
            .unwrap_err();
        assert_eq!(error, RunError::InterpreterMissing);
        assert_eq!(SourceError::from(error), SourceError::CliRuntimeMissing);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let text = "ééééé";
        for limit in 0..=text.len() {
            let cut = truncate_on_char_boundary(text, limit);
            assert!(cut.len() <= limit);
            assert!(text.starts_with(cut));
        }
    }

    #[tokio::test]
    async fn read_capped_accepts_exactly_the_limit() {
        let mut data = &b"0123456789"[..];
        assert_eq!(
            read_capped(&mut data, 10, "stdout").await.unwrap().len(),
            10
        );
        let mut data = &b"0123456789"[..];
        assert_eq!(
            read_capped(&mut data, 9, "stdout").await.unwrap_err(),
            RunError::OutputTooLarge {
                stream: "stdout",
                limit_bytes: 9
            }
        );
    }
}
