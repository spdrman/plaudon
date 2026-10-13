//! The typed argument vocabulary for the vendor CLI (spec 7.1, App. A.4).
//!
//! Spec 7.1 requires typed, strictly validated arguments and forbids routing
//! anything through `cmd.exe` or PowerShell. Both properties are structural
//! here rather than checked at the end:
//!
//! * [`CliCommand`] is a closed enum, so there is no subcommand outside the
//!   read-only set. There is no delete, no settings write, no firmware command
//!   and no way to express one.
//! * Every argument is rendered through [`plaudon_security::outbound::subprocess::CliArg`],
//!   which confines untrusted text to `Identifier` — an `[A-Za-z0-9._:-]`
//!   charset that cannot start with `-`. A hostile recording title or search
//!   term is therefore rejected rather than escaped, and a flag cannot be
//!   injected through a value position.
//! * The arguments are handed to the process API as an array. They are never
//!   joined into a string anywhere in this crate.

use plaudon_security::outbound::subprocess::{ArgError, CliArg};
use serde::{Deserialize, Serialize};

/// The only subcommands this adapter will ever invoke.
///
/// Observed on `@plaud-ai/cli` 0.3.15 (macOS arm64, 2026-10-12): `plaud login`
/// authenticates through a browser, `plaud files` enumerates recordings,
/// `plaud audio <id>` prints a temporary download URL and `plaud version`
/// reports the release. That release rejects `--version` as an unknown
/// option, so the version probe is a subcommand like every other call.
pub const PLAUD_SUBCOMMANDS: &[&str] = &["version", "login", "logout", "files", "audio"];

/// Subcommands that mutate anything on the vendor side. Only the session.
pub const SESSION_MUTATING_SUBCOMMANDS: &[&str] = &["login", "logout"];

/// A command we are willing to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum CliCommand {
    /// Report the CLI version. Always safe, runs before anything else.
    Version,
    /// Start the vendor's browser login.
    Login,
    /// End the vendor's session.
    ///
    /// Only constructible when the validated compatibility row records a
    /// supported logout command; spec 4.2.7 forbids claiming we revoked a
    /// session we cannot actually revoke.
    Logout,
    /// Enumerate recordings.
    Files {
        /// How this call asks for a page.
        ///
        /// Only ever anything but [`FilesPaging::Default`] when the validated
        /// compatibility row records observed pagination, because spec 7.2
        /// forbids guessing how a release pages.
        paging: FilesPaging,
    },
    /// Request a temporary download URL for one recording.
    Audio {
        /// Vendor recording identifier, as the CLI itself reported it.
        external_id: String,
    },
}

/// How one `files` call asks for a page.
///
/// Both paging shapes exist because both have been seen: an opaque cursor is
/// what a machine-readable release would carry in its own output, and numbered
/// pages are what `@plaud-ai/cli` 0.3.15 actually implements (`-p`, `-s`).
/// Neither is ever used unless the compatibility row records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "paging")]
pub enum FilesPaging {
    /// No paging arguments: whatever one bare `files` call returns.
    Default,
    /// An opaque cursor from a previous page, behind an observed flag.
    Cursor {
        /// The flag that carries the cursor, taken verbatim from the
        /// compatibility row rather than invented here.
        flag: String,
        /// Opaque cursor from a previous page.
        cursor: String,
    },
    /// A 1-based page number and an explicit page size, behind observed flags.
    Page {
        /// Flag that carries the page number.
        page_flag: String,
        /// 1-based page number.
        page: u32,
        /// Flag that carries the page size.
        page_size_flag: String,
        /// Items requested per page.
        page_size: u32,
    },
}

impl CliCommand {
    /// Enumerate with no paging arguments.
    #[must_use]
    pub const fn files() -> Self {
        Self::Files {
            paging: FilesPaging::Default,
        }
    }

    /// Enumerate a page using an observed cursor flag.
    #[must_use]
    pub fn files_cursor(cursor_flag: impl Into<String>, cursor: impl Into<String>) -> Self {
        Self::Files {
            paging: FilesPaging::Cursor {
                flag: cursor_flag.into(),
                cursor: cursor.into(),
            },
        }
    }

    /// Enumerate a numbered page using observed page and page-size flags.
    #[must_use]
    pub fn files_numbered(
        page_flag: impl Into<String>,
        page: u32,
        page_size_flag: impl Into<String>,
        page_size: u32,
    ) -> Self {
        Self::Files {
            paging: FilesPaging::Page {
                page_flag: page_flag.into(),
                page,
                page_size_flag: page_size_flag.into(),
                page_size,
            },
        }
    }

    /// Stable label for logs and the diagnostics panel.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::Files { .. } => "files",
            Self::Audio { .. } => "audio",
        }
    }

    /// Whether this command needs the user in front of the screen.
    #[must_use]
    pub const fn is_interactive(&self) -> bool {
        matches!(self, Self::Login | Self::Logout)
    }

    /// Build the validated argument vector.
    ///
    /// Fails rather than escaping when an identifier carries anything outside
    /// the permitted charset.
    pub fn args(&self) -> Result<Vec<CliArg>, ArgError> {
        let args = match self {
            Self::Version => vec![CliArg::Literal("version")],
            Self::Login => vec![CliArg::Literal("login")],
            Self::Logout => vec![CliArg::Literal("logout")],
            Self::Files { paging } => {
                let mut args = vec![CliArg::Literal("files")];
                match paging {
                    FilesPaging::Default => {}
                    FilesPaging::Cursor { flag, cursor } => {
                        args.push(flag_arg(flag, SUPPORTED_CURSOR_FLAGS)?);
                        args.push(CliArg::Identifier(reject_empty(cursor)?));
                    }
                    FilesPaging::Page {
                        page_flag,
                        page,
                        page_size_flag,
                        page_size,
                    } => {
                        args.push(flag_arg(page_flag, SUPPORTED_PAGE_FLAGS)?);
                        args.push(CliArg::Identifier(page_number(*page, MAX_PAGE_NUMBER)?));
                        args.push(flag_arg(page_size_flag, SUPPORTED_PAGE_SIZE_FLAGS)?);
                        args.push(CliArg::Identifier(page_number(*page_size, MAX_PAGE_SIZE)?));
                    }
                }
                args
            }
            Self::Audio { external_id } => vec![
                CliArg::Literal("audio"),
                CliArg::Identifier(reject_empty(external_id)?),
            ],
        };
        for arg in &args {
            arg.render()?;
        }
        Ok(args)
    }
}

/// Refuse an empty value rather than letting it occupy an argv slot, where it
/// would turn `audio ""` into a bare `audio` subcommand.
fn reject_empty(value: &str) -> Result<String, ArgError> {
    if value.is_empty() {
        return Err(ArgError::IllegalIdentifier);
    }
    Ok(value.to_owned())
}

/// Map a flag recorded in the compatibility table onto our static vocabulary.
///
/// `CliArg::Identifier` refuses a leading `-`, which is exactly what a flag
/// needs, so a flag may only ever be a `Literal`. Requiring it to match a
/// committed constant means a compatibility row cannot introduce a new flag —
/// adding one is a code change with a test, as spec 7.2 intends.
fn flag_arg(flag: &str, vocabulary: &'static [&'static str]) -> Result<CliArg, ArgError> {
    vocabulary
        .iter()
        .find(|known| **known == flag)
        .map(|known| CliArg::Literal(known))
        .ok_or(ArgError::FlagInjection)
}

/// Render a 1-based counter as a decimal argument, inside a sane bound.
///
/// The bound is ours, not the vendor's: it keeps a corrupt cursor from turning
/// into an unbounded walk through the account.
fn page_number(value: u32, limit: u32) -> Result<String, ArgError> {
    if value == 0 || value > limit {
        return Err(ArgError::IllegalIdentifier);
    }
    Ok(value.to_string())
}

/// Cursor flags this build knows how to pass.
///
/// A compatibility row that records any other flag cannot be used for paging:
/// spec 7.2 says pagination is honoured only where observed, and "observed"
/// here means observed *and* represented in this static vocabulary.
pub const SUPPORTED_CURSOR_FLAGS: &[&str] = &["--cursor", "--page-token", "--after", "--next"];

/// Page-number flags this build knows how to pass.
///
/// `@plaud-ai/cli` 0.3.15 prints `-p, --page <number>` in `plaud files --help`.
pub const SUPPORTED_PAGE_FLAGS: &[&str] = &["-p", "--page"];

/// Page-size flags this build knows how to pass.
///
/// `@plaud-ai/cli` 0.3.15 prints `-s, --page-size <number>` in
/// `plaud files --help` and rejects a size outside 10..=100 with
/// `[INVALID_ARGS]`.
pub const SUPPORTED_PAGE_SIZE_FLAGS: &[&str] = &["-s", "--page-size"];

/// Largest page number this build will ask any release for.
pub const MAX_PAGE_NUMBER: u32 = 10_000;

/// Largest page size this build will ask any release for.
pub const MAX_PAGE_SIZE: u32 = 1_000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn rendered(command: &CliCommand) -> Vec<OsString> {
        command
            .args()
            .expect("args")
            .iter()
            .map(|a| a.render().expect("render"))
            .collect()
    }

    #[test]
    fn version_login_and_files_render_to_fixed_vectors() {
        // 0.3.15 rejects `--version` as an unknown option; the version probe
        // is the `version` subcommand.
        assert_eq!(rendered(&CliCommand::Version), [OsString::from("version")]);
        assert_eq!(rendered(&CliCommand::Login), [OsString::from("login")]);
        assert_eq!(rendered(&CliCommand::Logout), [OsString::from("logout")]);
        assert_eq!(rendered(&CliCommand::files()), [OsString::from("files")]);
    }

    #[test]
    fn an_audio_request_passes_the_identifier_in_a_value_position() {
        assert_eq!(
            rendered(&CliCommand::Audio {
                external_id: "rec_01HF8ZT".to_owned()
            }),
            [OsString::from("audio"), OsString::from("rec_01HF8ZT")]
        );
    }

    #[test]
    fn hostile_identifiers_are_refused_rather_than_escaped() {
        // Spec 18.5: shell metacharacters and hostile Unicode injected into
        // every argument-bearing field. None of these may produce a command.
        let hostile = [
            "rec; rm -rf /",
            "rec && curl http://evil",
            "rec | tee /tmp/x",
            "rec`id`",
            "rec$(id)",
            "rec\nfiles",
            "rec\r\nlogin",
            "rec\0id",
            "--delete-all",
            "-rf",
            "%SYSTEMROOT%",
            "$HOME",
            "rec\"quote",
            "rec'quote",
            "rec>out",
            "rec<in",
            "rec^caret",
            "../../etc/passwd",
            "rec\u{202e}cba",
            "rec\u{feff}",
            "réc_01",
            "rec 01",
        ];
        for value in hostile {
            let command = CliCommand::Audio {
                external_id: value.to_owned(),
            };
            assert!(
                command.args().is_err(),
                "{value:?} must not render into an argument vector"
            );
        }
    }

    #[test]
    fn an_overlong_identifier_is_refused() {
        let command = CliCommand::Audio {
            external_id: "a".repeat(4096),
        };
        assert_eq!(command.args().unwrap_err(), ArgError::TooLong);
    }

    #[test]
    fn an_empty_identifier_is_refused() {
        // An empty value would render to an empty argv slot and turn
        // `audio ""` into a bare `audio` subcommand.
        let command = CliCommand::Audio {
            external_id: String::new(),
        };
        assert_eq!(command.args().unwrap_err(), ArgError::IllegalIdentifier);
    }

    #[test]
    fn a_cursor_is_only_passed_with_its_observed_flag() {
        let paged = CliCommand::files_cursor("--cursor", "abc123");
        assert_eq!(
            rendered(&paged),
            [
                OsString::from("files"),
                OsString::from("--cursor"),
                OsString::from("abc123")
            ]
        );
    }

    #[test]
    fn an_unobserved_cursor_flag_cannot_be_invented() {
        for flag in ["--exec", "-c", "--cursor; rm", "cursor", "--", "--üñî"] {
            let command = CliCommand::files_cursor(flag, "abc123");
            assert!(command.args().is_err(), "{flag:?} must be refused");
        }
        for flag in SUPPORTED_CURSOR_FLAGS {
            assert!(CliCommand::files_cursor(*flag, "abc123").args().is_ok());
        }
    }

    #[test]
    fn a_numbered_page_renders_the_observed_flags_of_0_3_15() {
        assert_eq!(
            rendered(&CliCommand::files_numbered("-p", 2, "-s", 20)),
            [
                OsString::from("files"),
                OsString::from("-p"),
                OsString::from("2"),
                OsString::from("-s"),
                OsString::from("20"),
            ]
        );
    }

    #[test]
    fn an_unobserved_page_flag_cannot_be_invented() {
        for flag in ["--offset", "-P", "--page; rm", "page", "--cursor"] {
            assert!(
                CliCommand::files_numbered(flag, 1, "-s", 20)
                    .args()
                    .is_err(),
                "{flag:?} must be refused as a page flag"
            );
            assert!(
                CliCommand::files_numbered("-p", 1, flag, 20)
                    .args()
                    .is_err(),
                "{flag:?} must be refused as a page-size flag"
            );
        }
        for page_flag in SUPPORTED_PAGE_FLAGS {
            for size_flag in SUPPORTED_PAGE_SIZE_FLAGS {
                assert!(CliCommand::files_numbered(*page_flag, 1, *size_flag, 20)
                    .args()
                    .is_ok());
            }
        }
    }

    #[test]
    fn a_page_counter_outside_its_bound_is_refused() {
        for (page, size) in [
            (0, 20),
            (MAX_PAGE_NUMBER + 1, 20),
            (1, 0),
            (1, MAX_PAGE_SIZE + 1),
        ] {
            assert_eq!(
                CliCommand::files_numbered("-p", page, "-s", size)
                    .args()
                    .unwrap_err(),
                ArgError::IllegalIdentifier,
                "page {page} size {size} must be refused"
            );
        }
    }

    #[test]
    fn a_hostile_cursor_value_is_refused() {
        for value in ["a;b", "--flag", "a b", "a\nb"] {
            assert!(CliCommand::files_cursor("--cursor", value).args().is_err());
        }
    }

    #[test]
    fn the_subcommand_vocabulary_is_read_only() {
        for forbidden in [
            "delete", "rm", "clear", "erase", "sync-ack", "settings", "firmware", "ota", "upload",
        ] {
            assert!(
                !PLAUD_SUBCOMMANDS.contains(&forbidden),
                "{forbidden} must not be in the vocabulary"
            );
        }
        assert_eq!(SESSION_MUTATING_SUBCOMMANDS, ["login", "logout"]);
    }

    #[test]
    fn interactive_commands_are_identified() {
        assert!(CliCommand::Login.is_interactive());
        assert!(CliCommand::Logout.is_interactive());
        assert!(!CliCommand::files().is_interactive());
        assert!(!CliCommand::Version.is_interactive());
    }
}
