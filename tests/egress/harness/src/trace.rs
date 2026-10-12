//! Parsing and interrogation of a monitor trace file.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One line of `PLAUDON_EGRESS_LOG`, as written by `plaudon-egress-monitor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceLine {
    pub ts_unix: i64,
    pub call: String,
    pub family: String,
    pub host_or_ip: String,
    pub port: u16,
    pub denied: bool,
    /// Present on `exec` lines only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
}

impl TraceLine {
    /// Calls through which internet traffic or a name resolution could leave
    /// this process.
    #[must_use]
    pub fn is_network_attempt(&self) -> bool {
        matches!(
            self.call.as_str(),
            "connect" | "connectx" | "sendto" | "getaddrinfo" | "gethostbyname" | "res_query"
        )
    }

    #[must_use]
    pub fn is_exec(&self) -> bool {
        self.call == "exec"
    }

    /// Leaf file name of an `exec` record's program.
    #[must_use]
    pub fn program_leaf(&self) -> Option<&str> {
        let program = self.program.as_deref()?;
        Some(program.rsplit('/').next().unwrap_or(program))
    }
}

/// A parse failure in the trace file. A malformed trace is a harness failure,
/// never something to skip over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceParseError {
    pub line_number: usize,
    pub line: String,
    pub message: String,
}

impl fmt::Display for TraceParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "egress trace line {} is not valid monitor output ({}): {}",
            self.line_number, self.message, self.line
        )
    }
}

impl std::error::Error for TraceParseError {}

/// All lines a monitored process produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trace {
    pub lines: Vec<TraceLine>,
}

impl Trace {
    /// Parse newline-delimited JSON. Blank lines are ignored; anything else
    /// that does not parse is an error.
    pub fn parse(text: &str) -> Result<Trace, TraceParseError> {
        let mut lines = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            if raw.trim().is_empty() {
                continue;
            }
            let parsed: TraceLine = serde_json::from_str(raw).map_err(|err| TraceParseError {
                line_number: index + 1,
                line: raw.to_owned(),
                message: err.to_string(),
            })?;
            lines.push(parsed);
        }
        Ok(Trace { lines })
    }

    /// The monitor writes this from its library constructor. Its absence means
    /// the library was never injected, which is a failure, never a pass.
    #[must_use]
    pub fn monitor_injected(&self) -> bool {
        self.lines.iter().any(|l| l.call == "monitor_ready")
    }

    /// Every connection or resolution attempt aimed outside loopback. These
    /// are exactly the calls the monitor refused.
    #[must_use]
    pub fn non_loopback_attempts(&self) -> Vec<&TraceLine> {
        self.lines
            .iter()
            .filter(|l| l.is_network_attempt() && l.denied)
            .collect()
    }

    /// Attempts the monitor allowed because they are loopback or a non-internet
    /// address family. Reported in the evidence for completeness.
    #[must_use]
    pub fn loopback_attempts(&self) -> Vec<&TraceLine> {
        self.lines
            .iter()
            .filter(|l| l.is_network_attempt() && !l.denied)
            .collect()
    }

    #[must_use]
    pub fn exec_records(&self) -> Vec<&TraceLine> {
        self.lines.iter().filter(|l| l.is_exec()).collect()
    }

    /// `exec` records whose program leaf is the pinned cloud-CLI name (§7.1).
    /// macOS records `posix_spawnp` and the `posix_spawn` it delegates to, so
    /// one launch can appear more than once; the assertion is on zero, which
    /// duplicates cannot weaken.
    #[must_use]
    pub fn cloud_cli_execs(&self) -> Vec<&TraceLine> {
        let pinned = plaudon_security::outbound::PLAUD_CLI_PROGRAM;
        self.lines
            .iter()
            .filter(|l| l.is_exec())
            .filter(|l| {
                l.program_leaf()
                    .is_some_and(|leaf| leaf == pinned || leaf == format!("{pinned}.exe"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use plaudon_egress_monitor::wire::{format_attempt, Attempt, Call, Family};

    /// The parser and the monitor's formatter are two halves of one wire
    /// contract, so the test uses the real formatter rather than a fixture
    /// string.
    #[test]
    fn parses_lines_produced_by_the_real_monitor_formatter() {
        let mut text = String::new();
        for attempt in [
            Attempt {
                ts_unix: 1_760_000_000,
                call: Call::Connect,
                family: Family::Inet,
                host_or_ip: b"203.0.113.1",
                port: 443,
                denied: true,
                program: &[],
            },
            Attempt {
                ts_unix: 1_760_000_001,
                call: Call::Connect,
                family: Family::Inet,
                host_or_ip: b"127.0.0.1",
                port: 5432,
                denied: false,
                program: &[],
            },
            Attempt {
                ts_unix: 1_760_000_002,
                call: Call::Exec,
                family: Family::None,
                host_or_ip: &[],
                port: 0,
                denied: false,
                program: b"/opt/plaud/bin/plaud",
            },
            Attempt::new(1_760_000_003, Call::MonitorReady),
        ] {
            let line = format_attempt(&attempt);
            text.push_str(std::str::from_utf8(line.as_bytes()).unwrap());
        }

        let trace = Trace::parse(&text).unwrap();
        assert_eq!(trace.lines.len(), 4);
        assert!(trace.monitor_injected());
        assert_eq!(trace.non_loopback_attempts().len(), 1);
        assert_eq!(trace.non_loopback_attempts()[0].host_or_ip, "203.0.113.1");
        assert_eq!(trace.loopback_attempts().len(), 1);
        assert_eq!(trace.exec_records().len(), 1);
        assert_eq!(trace.cloud_cli_execs().len(), 1);
    }

    #[test]
    fn an_empty_trace_means_the_monitor_was_not_injected() {
        let trace = Trace::parse("\n   \n").unwrap();
        assert_eq!(trace.lines.len(), 0);
        assert!(!trace.monitor_injected());
    }

    #[test]
    fn malformed_lines_are_errors_not_skips() {
        let err = Trace::parse("{\"ts_unix\":1}\n").unwrap_err();
        assert_eq!(err.line_number, 1);
        let err = Trace::parse(
            "{\"ts_unix\":1,\"call\":\"connect\",\"family\":\"inet\",\"host_or_ip\":\"a\",\"port\":1,\"denied\":false}\nnot json\n",
        )
        .unwrap_err();
        assert_eq!(err.line_number, 2);
        assert!(err.to_string().contains("not valid monitor output"));
    }

    #[test]
    fn cloud_cli_detection_uses_the_program_leaf() {
        let text = "{\"ts_unix\":1,\"call\":\"exec\",\"family\":\"none\",\"host_or_ip\":\"\",\
                    \"port\":0,\"denied\":false,\"program\":\"/usr/bin/plaudit\"}\n\
                    {\"ts_unix\":2,\"call\":\"exec\",\"family\":\"none\",\"host_or_ip\":\"\",\
                    \"port\":0,\"denied\":false,\"program\":\"/x/plaud.exe\"}\n";
        let trace = Trace::parse(text).unwrap();
        assert_eq!(trace.exec_records().len(), 2);
        let cli = trace.cloud_cli_execs();
        assert_eq!(cli.len(), 1);
        assert_eq!(cli[0].program_leaf(), Some("plaud.exe"));
    }
}
