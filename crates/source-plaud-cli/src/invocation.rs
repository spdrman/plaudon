//! Safe invocation of the vendor CLI (spec 7.1, App. A.4).
//!
//! The CLI is spawned with an argument array, never a command string. On
//! Windows that matters doubly: a command string would be re-parsed by
//! `cmd.exe`, so a recording title containing `&` or `|` would become a
//! second command. This module builds the argument vector and validates the
//! executable path; it deliberately does not spawn anything, so the policy is
//! unit-testable with no process in the loop.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The only subcommands this adapter will ever invoke.
///
/// Read-only by construction: there is no delete, no settings write and no
/// firmware command in the list, and a subcommand outside it is
/// unrepresentable because [`CliSubcommand`] is a closed enum.
pub const PLAUD_SUBCOMMANDS: &[&str] = &["--version", "login", "files", "audio"];

/// A subcommand we are willing to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliSubcommand {
    /// Report the CLI version.
    Version,
    /// Start the vendor's browser login.
    Login,
    /// Enumerate recordings.
    Files,
    /// Request a temporary download URL for one recording.
    Audio {
        /// Vendor recording identifier.
        external_id: String,
    },
}

impl CliSubcommand {
    /// The argument vector for this subcommand.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        match self {
            Self::Version => vec!["--version".to_owned()],
            Self::Login => vec!["login".to_owned()],
            Self::Files => vec!["files".to_owned()],
            Self::Audio { external_id } => vec!["audio".to_owned(), external_id.clone()],
        }
    }
}

/// Why an invocation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvocationError {
    /// The executable path is not absolute.
    #[error("the command line tool path must be absolute")]
    PathNotAbsolute,
    /// The executable path contains a traversal or non-plain component.
    #[error("the command line tool path contains an unsafe component")]
    PathNotCanonical,
    /// The executable path does not exist or is not a file.
    #[error("the command line tool was not found at the configured path")]
    ExecutableMissing,
    /// An argument contains a character we refuse to pass through.
    #[error("an argument contains a character that cannot be passed safely")]
    UnsafeArgument {
        /// The offending argument.
        argument: String,
    },
    /// An environment override was requested that we do not allow.
    #[error("environment overrides are not permitted for the vendor tool")]
    EnvironmentOverrideRejected {
        /// The variable that was requested.
        variable: String,
    },
}

/// A fully validated invocation, ready to be handed to a process spawner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInvocation {
    /// Absolute path of the executable, as chosen by the user.
    pub program: PathBuf,
    /// Argument vector. Never joined into a string.
    pub args: Vec<String>,
    /// The complete environment the child will see.
    pub env: BTreeMap<String, String>,
    /// Wall-clock budget for the child.
    pub timeout_secs: u64,
    /// Maximum bytes captured from stdout before the child is stopped.
    pub max_stdout_bytes: u64,
    /// Maximum bytes captured from stderr before the child is stopped.
    pub max_stderr_bytes: u64,
}

impl CliInvocation {
    /// Default wall-clock budget.
    pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
    /// Default stdout capture limit.
    pub const DEFAULT_MAX_STDOUT_BYTES: u64 = 8 * 1024 * 1024;
    /// Default stderr capture limit.
    pub const DEFAULT_MAX_STDERR_BYTES: u64 = 256 * 1024;

    /// Environment variables the child is allowed to inherit.
    ///
    /// Everything else is dropped, so a poisoned `PATH`, `LD_PRELOAD`,
    /// `DYLD_INSERT_LIBRARIES` or proxy variable in our own environment
    /// cannot change what the vendor tool does.
    pub const ALLOWED_ENV: &'static [&'static str] =
        &["HOME", "USERPROFILE", "LANG", "TMPDIR", "TEMP"];

    /// Build an invocation, validating the executable and every argument.
    pub fn build(
        program: &Path,
        subcommand: &CliSubcommand,
        inherited_env: &BTreeMap<String, String>,
    ) -> Result<Self, InvocationError> {
        if !program.is_absolute() {
            return Err(InvocationError::PathNotAbsolute);
        }
        for component in program.components() {
            match component {
                Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {}
                Component::CurDir | Component::ParentDir => {
                    return Err(InvocationError::PathNotCanonical)
                }
            }
        }
        if !program.is_file() {
            return Err(InvocationError::ExecutableMissing);
        }

        let args = subcommand.argv();
        for argument in &args {
            validate_argument(argument)?;
        }

        let env = inherited_env
            .iter()
            .filter(|(key, _)| Self::ALLOWED_ENV.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        Ok(Self {
            program: program.to_path_buf(),
            args,
            env,
            timeout_secs: Self::DEFAULT_TIMEOUT_SECS,
            max_stdout_bytes: Self::DEFAULT_MAX_STDOUT_BYTES,
            max_stderr_bytes: Self::DEFAULT_MAX_STDERR_BYTES,
        })
    }

    /// Reject an explicit environment override.
    ///
    /// There is no supported reason to set a variable for the vendor tool,
    /// and several unsupported ones.
    pub fn with_env_override(self, variable: &str, _value: &str) -> Result<Self, InvocationError> {
        Err(InvocationError::EnvironmentOverrideRejected {
            variable: variable.to_owned(),
        })
    }
}

/// Characters we refuse to pass to a child process at all.
///
/// None of them can appear in a legitimate vendor recording identifier, and
/// all of them are meaningful to at least one shell or to `cmd.exe`.
fn validate_argument(argument: &str) -> Result<(), InvocationError> {
    const FORBIDDEN: &[char] = &[
        '\0', '\n', '\r', '&', '|', ';', '`', '$', '<', '>', '^', '"', '\'', '%', '*', '?',
    ];
    if argument.is_empty() {
        return Err(InvocationError::UnsafeArgument {
            argument: argument.to_owned(),
        });
    }
    if argument
        .chars()
        .any(|c| FORBIDDEN.contains(&c) || c.is_control())
    {
        return Err(InvocationError::UnsafeArgument {
            argument: argument.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_executable() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plaud");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write");
        (dir, path)
    }

    fn env() -> BTreeMap<String, String> {
        [
            ("HOME".to_owned(), "/Users/example".to_owned()),
            ("PATH".to_owned(), "/attacker/bin".to_owned()),
            (
                "DYLD_INSERT_LIBRARIES".to_owned(),
                "/attacker/evil.dylib".to_owned(),
            ),
            ("HTTPS_PROXY".to_owned(), "http://attacker".to_owned()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn a_valid_invocation_uses_an_argument_vector_not_a_command_string() {
        let (_dir, program) = fake_executable();
        let invocation =
            CliInvocation::build(&program, &CliSubcommand::Files, &env()).expect("builds");
        assert_eq!(invocation.args, vec!["files".to_owned()]);
        assert_eq!(invocation.program, program);
        assert!(invocation.timeout_secs > 0);
        assert!(invocation.max_stdout_bytes > 0);
    }

    #[test]
    fn the_child_environment_is_an_allowlist() {
        let (_dir, program) = fake_executable();
        let invocation =
            CliInvocation::build(&program, &CliSubcommand::Files, &env()).expect("builds");
        assert_eq!(
            invocation.env.get("HOME").map(String::as_str),
            Some("/Users/example")
        );
        for dropped in ["PATH", "DYLD_INSERT_LIBRARIES", "HTTPS_PROXY"] {
            assert!(
                !invocation.env.contains_key(dropped),
                "{dropped} must not be inherited"
            );
        }
    }

    #[test]
    fn shell_metacharacters_in_a_recording_id_are_refused() {
        let (_dir, program) = fake_executable();
        for hostile in [
            "rec-1 && rm -rf /",
            "rec-1; shutdown",
            "rec-1 | tee /tmp/x",
            "rec`whoami`",
            "rec$(id)",
            "rec\"quote",
            "rec'quote",
            "rec%PATH%",
            "rec\nnewline",
            "rec\0nul",
            "",
        ] {
            let subcommand = CliSubcommand::Audio {
                external_id: hostile.to_owned(),
            };
            let result = CliInvocation::build(&program, &subcommand, &env());
            assert!(
                matches!(result, Err(InvocationError::UnsafeArgument { .. })),
                "{hostile:?}"
            );
        }
    }

    #[test]
    fn a_benign_recording_id_passes_through_verbatim() {
        let (_dir, program) = fake_executable();
        let subcommand = CliSubcommand::Audio {
            external_id: "rec_2026-10-11.abc123".to_owned(),
        };
        let invocation = CliInvocation::build(&program, &subcommand, &env()).expect("builds");
        assert_eq!(
            invocation.args,
            vec!["audio".to_owned(), "rec_2026-10-11.abc123".to_owned()]
        );
    }

    #[test]
    fn an_arbitrary_or_relative_executable_path_is_refused() {
        let (_dir, program) = fake_executable();
        assert_eq!(
            CliInvocation::build(Path::new("plaud"), &CliSubcommand::Files, &env()),
            Err(InvocationError::PathNotAbsolute)
        );
        assert_eq!(
            CliInvocation::build(
                &program.join("..").join("plaud"),
                &CliSubcommand::Files,
                &env()
            ),
            Err(InvocationError::PathNotCanonical)
        );
        assert_eq!(
            CliInvocation::build(
                Path::new("/definitely/not/here/plaud"),
                &CliSubcommand::Files,
                &env()
            ),
            Err(InvocationError::ExecutableMissing)
        );
    }

    #[test]
    fn environment_overrides_are_always_rejected() {
        let (_dir, program) = fake_executable();
        let invocation =
            CliInvocation::build(&program, &CliSubcommand::Files, &env()).expect("builds");
        assert_eq!(
            invocation.with_env_override("PLAUD_TOKEN", "x"),
            Err(InvocationError::EnvironmentOverrideRejected {
                variable: "PLAUD_TOKEN".to_owned()
            })
        );
    }

    #[test]
    fn the_subcommand_set_is_read_only() {
        let all = [
            CliSubcommand::Version,
            CliSubcommand::Login,
            CliSubcommand::Files,
            CliSubcommand::Audio {
                external_id: "x".into(),
            },
        ];
        for subcommand in &all {
            let first = subcommand
                .argv()
                .first()
                .cloned()
                .expect("a leading argument");
            assert!(
                PLAUD_SUBCOMMANDS.contains(&first.as_str()),
                "{first} is not allowlisted"
            );
            for destructive in [
                "delete", "rm", "clear", "reset", "firmware", "config", "set",
            ] {
                assert_ne!(first, destructive);
            }
        }
    }
}
