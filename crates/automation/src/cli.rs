//! The command surface of the scripting bridge (spec 14.5).
//!
//! Spec 14.5 names the commands: `list`, `export`, `process` and
//! `jobs status`. They are defined here as typed values so that argument
//! validation happens before any of them touches the library, and so
//! `--help` lists exactly what exists.
//!
//! There is deliberately no `serve`, `daemon` or `listen` subcommand. Spec
//! 14.5 prohibits an unauthenticated loopback REST server in V1 and spec 1.3
//! rules out internet-facing webhooks, so the absence is structural rather
//! than a configuration default.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::policy::Destination;

/// User-local scripting bridge for the plaudon library.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    name = "plaudon",
    version,
    about = "User-local scripting bridge for your plaudon library.",
    long_about = "Runs the same local application services the plaudon desktop app uses, for \
                  scripting your own library.\n\nIt opens no network port and no server. It is \
                  disabled until you enable it in Settings, and it runs only for the signed-in \
                  user who enabled it. Cloud processing still requires the consent you granted \
                  in the app, and Local-only Mode still refuses it.",
    disable_help_subcommand = false
)]
pub struct Cli {
    /// Path of the library database. Defaults to the application's library.
    #[arg(long, global = true, value_name = "PATH")]
    pub library: Option<PathBuf>,

    /// Print indented JSON instead of one line.
    #[arg(long, global = true)]
    pub pretty: bool,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The four scripting commands plus the bridge switch.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// List recordings in the library.
    List(ListArgs),
    /// Export a recording's transcript.
    Export(ExportArgs),
    /// Queue processing for a recording.
    Process(ProcessArgs),
    /// Inspect background jobs.
    Jobs {
        /// Jobs subcommand.
        #[command(subcommand)]
        command: JobsCommand,
    },
    /// Show or change whether the scripting bridge is enabled.
    Bridge {
        /// Bridge subcommand.
        #[command(subcommand)]
        command: BridgeCommand,
    },
}

impl Command {
    /// Name reported in the output envelope.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::List(_) => "list",
            Self::Export(_) => "export",
            Self::Process(_) => "process",
            Self::Jobs { .. } => "jobs",
            Self::Bridge { .. } => "bridge",
        }
    }
}

/// `plaudon list`
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ListArgs {
    /// Maximum number of recordings to return.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
    /// Number of recordings to skip.
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
    /// Ordering.
    #[arg(long, value_enum, default_value_t = SortOrder::Newest)]
    pub sort: SortOrder,
}

/// Listing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortOrder {
    /// Most recently recorded first.
    Newest,
    /// Oldest recorded first.
    Oldest,
    /// Alphabetical by title.
    Title,
}

/// `plaudon export`
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ExportArgs {
    /// Recording identifier.
    #[arg(long, value_name = "UUID")]
    pub recording: String,
    /// Output shape.
    #[arg(long, value_enum, default_value_t = ExportFormat::Json)]
    pub format: ExportFormat,
    /// Write to this file instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// What `export` produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    /// The full segment list with times, speakers and provenance.
    Json,
    /// One line per segment, for piping into other tools.
    Text,
}

/// `plaudon process`
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ProcessArgs {
    /// Recording identifier.
    #[arg(long, value_name = "UUID")]
    pub recording: String,
    /// Which stage to queue.
    #[arg(long, value_enum, default_value_t = Stage::Transcribe)]
    pub stage: Stage,
    /// Where the work runs. Cloud work needs consent and is refused in
    /// Local-only Mode.
    #[arg(long, value_enum, default_value_t = Where::Local)]
    pub run: Where,
    /// Model identifier to use, when the stage needs one.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
}

/// Processing stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Stage {
    /// Speech to text.
    Transcribe,
    /// Summarization and extraction.
    Summarize,
    /// Speaker diarization.
    Diarize,
}

impl Stage {
    /// Job type string recorded in the job row.
    #[must_use]
    pub const fn job_type(self) -> &'static str {
        match self {
            Self::Transcribe => "transcribe",
            Self::Summarize => "summarize",
            Self::Diarize => "diarize",
        }
    }
}

/// Where processing runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Where {
    /// On this machine.
    Local,
    /// Through the configured cloud provider.
    Cloud,
}

impl From<Where> for Destination {
    fn from(value: Where) -> Self {
        match value {
            Where::Local => Self::Local,
            Where::Cloud => Self::Cloud,
        }
    }
}

/// `plaudon jobs ...`
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum JobsCommand {
    /// Show job status.
    Status {
        /// Only jobs for this recording.
        #[arg(long, value_name = "UUID")]
        recording: Option<String>,
        /// Maximum number of jobs to return.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
    },
}

/// `plaudon bridge ...`
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BridgeCommand {
    /// Report whether the bridge is enabled and who owns it.
    Status,
    /// Enable the bridge for the signed-in user.
    Enable {
        /// Required acknowledgement, so enabling is never accidental.
        #[arg(long)]
        confirm: bool,
    },
    /// Disable the bridge.
    Disable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_internally_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_lists_every_specified_command() {
        let help = Cli::command().render_long_help().to_string();
        for command in ["list", "export", "process", "jobs"] {
            assert!(help.contains(command), "--help does not mention {command}");
        }
    }

    #[test]
    fn help_states_that_no_server_is_opened() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("no network port"));
        assert!(help.contains("disabled until you enable it"));
    }

    #[test]
    fn there_is_no_server_or_daemon_subcommand() {
        let names: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        for forbidden in ["serve", "server", "daemon", "listen", "webhook"] {
            assert!(!names.iter().any(|name| name == forbidden), "{forbidden}");
        }
    }

    #[test]
    fn jobs_status_parses_as_specified() {
        let cli = Cli::try_parse_from(["plaudon", "jobs", "status", "--limit", "10"]).unwrap();
        assert_eq!(cli.command.name(), "jobs");
        let Command::Jobs {
            command: JobsCommand::Status { limit, recording },
        } = cli.command
        else {
            panic!("expected jobs status");
        };
        assert_eq!(limit, 10);
        assert_eq!(recording, None);
    }

    #[test]
    fn list_defaults_are_bounded() {
        let cli = Cli::try_parse_from(["plaudon", "list"]).unwrap();
        let Command::List(args) = cli.command else {
            panic!("expected list")
        };
        assert_eq!(args.limit, 50);
        assert_eq!(args.sort, SortOrder::Newest);
        assert!(Cli::try_parse_from(["plaudon", "list", "--limit", "0"]).is_err());
        assert!(Cli::try_parse_from(["plaudon", "list", "--limit", "99999"]).is_err());
    }

    #[test]
    fn export_defaults_to_machine_readable_json() {
        let cli = Cli::try_parse_from(["plaudon", "export", "--recording", "abc"]).unwrap();
        let Command::Export(args) = cli.command else {
            panic!("expected export")
        };
        assert_eq!(args.format, ExportFormat::Json);
        assert_eq!(args.out, None);
    }

    #[test]
    fn process_defaults_to_local_execution() {
        let cli = Cli::try_parse_from(["plaudon", "process", "--recording", "abc"]).unwrap();
        let Command::Process(args) = cli.command else {
            panic!("expected process")
        };
        assert_eq!(args.run, Where::Local);
        assert_eq!(Destination::from(args.run), Destination::Local);
        assert_eq!(args.stage.job_type(), "transcribe");
    }

    #[test]
    fn enabling_the_bridge_requires_an_explicit_confirmation_flag() {
        let cli = Cli::try_parse_from(["plaudon", "bridge", "enable"]).unwrap();
        let Command::Bridge {
            command: BridgeCommand::Enable { confirm },
        } = cli.command
        else {
            panic!("expected bridge enable");
        };
        assert!(!confirm);
    }

    #[test]
    fn an_unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["plaudon", "exfiltrate"]).is_err());
    }
}
