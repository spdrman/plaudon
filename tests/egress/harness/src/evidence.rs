//! The §24 release-evidence artifact: `target/security-evidence/egress-trace.json`.
//!
//! The bundle records what was *observed*, including the statements this
//! observation cannot support (§16.2A, §16.6). It is written on every run of
//! the harness, pass or fail, so a failing gate still leaves evidence behind.

use std::fs;
use std::io;
use std::path::Path;

use plaudon_egress_scenario::{ExplainedDenial, LocalArtifact, ScenarioSummary};
use plaudon_security::outbound::{DisabledCapability, EgressRecord, NOT_GUARANTEED};
use serde::{Deserialize, Serialize};

use crate::fixture::ScenarioRun;
use crate::trace::TraceLine;

/// Schema version of the artifact, so the release tooling can detect drift.
pub const SCHEMA_VERSION: &str = "plaudon.egress-trace.v1";

/// Per-scenario evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioEvidence {
    pub scenario: String,
    /// `dyld-interpose` | `ld-preload` | `unsupported`.
    pub monitor: String,
    pub platform: String,
    pub timestamp: i64,
    pub monitor_injected: bool,
    pub exit_code: Option<i32>,
    pub outcome: String,
    /// Every connect/resolve the monitor saw, denied or permitted.
    pub attempts: Vec<TraceLine>,
    /// How many of `attempts` were aimed outside loopback (must be 0 outside
    /// the negative control).
    pub non_loopback_attempt_count: usize,
    /// Sockets the process created. Creating one transmits nothing, but a
    /// created `AF_INET` socket with no matching connect is itself evidence.
    pub socket_records: Vec<TraceLine>,
    /// Every process execution the monitor saw.
    pub exec_records: Vec<TraceLine>,
    /// Every decision the `plaudon_security::outbound` gate made.
    pub decisions: Vec<EgressRecord>,
    pub disabled_capabilities: Vec<DisabledCapability>,
    pub explained_denials: Vec<ExplainedDenial>,
    pub local_artifact: Option<LocalArtifact>,
    pub notes: Vec<String>,
    /// `null` when the scenario passed, otherwise the acceptance rule it broke.
    pub failure: Option<String>,
}

impl ScenarioEvidence {
    /// Build the evidence for one completed run.
    #[must_use]
    pub fn from_run(
        run: &ScenarioRun,
        monitor: &str,
        timestamp: i64,
        failure: Option<String>,
    ) -> ScenarioEvidence {
        let attempts: Vec<TraceLine> = run
            .trace
            .lines
            .iter()
            .filter(|l| l.is_network_attempt())
            .cloned()
            .collect();
        let non_loopback_attempt_count = attempts.iter().filter(|l| l.denied).count();
        let socket_records: Vec<TraceLine> = run
            .trace
            .lines
            .iter()
            .filter(|l| l.call == "socket")
            .cloned()
            .collect();
        let exec_records: Vec<TraceLine> = run
            .trace
            .lines
            .iter()
            .filter(|l| l.is_exec())
            .cloned()
            .collect();
        let summary: Option<&ScenarioSummary> = run.summary.as_ref();

        ScenarioEvidence {
            scenario: run.scenario.name().to_owned(),
            monitor: monitor.to_owned(),
            platform: platform_label(),
            timestamp,
            monitor_injected: run.trace.monitor_injected(),
            exit_code: run.exit_code,
            outcome: summary.map_or_else(
                || "no decision summary was written".to_owned(),
                |s| s.outcome.clone(),
            ),
            attempts,
            non_loopback_attempt_count,
            socket_records,
            exec_records,
            decisions: summary.map(|s| s.decisions.clone()).unwrap_or_default(),
            disabled_capabilities: summary
                .map(|s| s.disabled_capabilities.clone())
                .unwrap_or_default(),
            explained_denials: summary
                .map(|s| s.explained_denials.clone())
                .unwrap_or_default(),
            local_artifact: summary.and_then(|s| s.local_artifact.clone()),
            notes: summary.map(|s| s.notes.clone()).unwrap_or_default(),
            failure,
        }
    }
}

/// The whole artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBundle {
    pub schema_version: String,
    pub generated_at_unix: i64,
    pub platform: String,
    pub monitor: String,
    pub scenarios: Vec<ScenarioEvidence>,
    /// What the observation covers.
    pub observation_scope: Vec<String>,
    /// What it explicitly does not cover (`outbound::NOT_GUARANTEED` plus the
    /// limits of the injection mechanism itself).
    pub not_guaranteed: Vec<String>,
}

impl EvidenceBundle {
    #[must_use]
    pub fn new(generated_at_unix: i64, monitor: &str, scenarios: Vec<ScenarioEvidence>) -> Self {
        let mut not_guaranteed: Vec<String> =
            NOT_GUARANTEED.iter().map(|s| (*s).to_owned()).collect();
        not_guaranteed.push(
            "The monitor observes the scenario process only. Injection is stripped by the OS for \
             hardened-runtime, setuid and platform binaries, so system daemons and platform \
             services are not observed here."
                .to_owned(),
        );
        not_guaranteed.push(
            "Sends on an already-connected socket are not individually recorded; the connect that \
             must precede them is."
                .to_owned(),
        );
        not_guaranteed.push(
            "This run proves nothing about builds other than the one whose artifacts are named in \
             this bundle."
                .to_owned(),
        );

        EvidenceBundle {
            schema_version: SCHEMA_VERSION.to_owned(),
            generated_at_unix,
            platform: platform_label(),
            monitor: monitor.to_owned(),
            scenarios,
            observation_scope: vec![
                "Every connect/connectx/sendto and every getaddrinfo/gethostbyname/res_query \
                 issued by the scenario process was intercepted at the libc boundary."
                    .to_owned(),
                "Every execve/execvp/posix_spawn/posix_spawnp issued by the scenario process was \
                 recorded, which is how 'the PLAUD Cloud CLI never runs in Local-only Mode' is \
                 checked by observation rather than by trusting the launcher."
                    .to_owned(),
                "Non-loopback internet destinations were refused with EPERM/EAI_FAIL, so a \
                 regression cannot complete a connection even on a networked CI machine."
                    .to_owned(),
            ],
            not_guaranteed,
        }
    }

    /// Write the artifact, creating `target/security-evidence/` if needed.
    pub fn write_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(path, json)
    }
}

/// `os/arch`, e.g. `macos/aarch64`.
#[must_use]
pub fn platform_label() -> String {
    format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn bundle_round_trips_and_states_its_limits() {
        let bundle = EvidenceBundle::new(1_760_000_000, "dyld-interpose", Vec::new());
        let json = serde_json::to_string(&bundle).unwrap();
        let back: EvidenceBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, bundle);
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert!(back.not_guaranteed.len() >= NOT_GUARANTEED.len() + 3);
        assert!(back
            .not_guaranteed
            .iter()
            .any(|s| s.contains("observes the scenario process only")));
        assert_ne!(back.observation_scope.len(), 0);
    }

    #[test]
    fn platform_label_is_os_slash_arch() {
        let label = platform_label();
        assert!(label.contains('/'));
        assert!(label.starts_with(std::env::consts::OS));
    }

    #[test]
    fn writes_the_artifact_to_disk() {
        let dir = std::env::temp_dir().join(format!("plaudon-evidence-{}", std::process::id()));
        let path = dir.join("security-evidence").join("egress-trace.json");
        let bundle = EvidenceBundle::new(42, "dyld-interpose", Vec::new());
        bundle.write_to(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let back: EvidenceBundle = serde_json::from_str(&text).unwrap();
        assert_eq!(back.generated_at_unix, 42);
        fs::remove_dir_all(&dir).unwrap();
    }
}
