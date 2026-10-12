//! The acceptance rules of D2.3, as pure functions over a parsed trace and a
//! scenario summary.
//!
//! Keeping them here (rather than inline in `#[test]`s) means they can be
//! unit-tested against synthetic traces, including the traces that *must*
//! fail.

use std::fmt;

use plaudon_egress_scenario::{Scenario, ScenarioSummary};

use crate::trace::{Trace, TraceLine, TraceParseError};

/// Why the fixture refuses to certify a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessFailure {
    /// No `monitor_ready` line: the library was not injected, so **nothing was
    /// observed**. This is a failure, never a skip (spec §16.2A).
    MonitorUnavailable { scenario: String, injection: String },
    /// This platform has no interception mechanism compiled in.
    MonitorUnsupported { platform: String },
    /// The trace file could not be parsed.
    TraceUnreadable(TraceParseError),
    /// A non-loopback connection or resolution attempt escaped the gate.
    UnexpectedEgress {
        scenario: String,
        attempts: Vec<TraceLine>,
    },
    /// The negative control produced no recorded attempt, which means the
    /// fixture cannot detect traffic and every other "pass" is worthless.
    NegativeControlSilent { scenario: String, detail: String },
    /// A process whose leaf name is the pinned cloud CLI was executed.
    CloudCliExecuted { scenario: String, program: String },
    /// A §18.6A hostile case left a capability unavailable without saying why.
    CapabilityNotExplained { scenario: String, subsystem: String },
    /// The egress gate authorized something while in Local-only Mode.
    GateAuthorizedInLocalOnly {
        scenario: String,
        destination_label: String,
    },
    /// The scenario process exited non-zero, or did not write its summary.
    ScenarioFailed {
        scenario: String,
        exit_code: Option<i32>,
        outcome: String,
    },
    /// The scenario's local (non-network) work did not actually happen.
    LocalWorkNotDone { scenario: String, detail: String },
}

impl fmt::Display for HarnessFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HarnessFailure::MonitorUnavailable {
                scenario,
                injection,
            } => write!(
                f,
                "egress monitor was not injected into `{scenario}` ({injection}): the run \
                 observed nothing, so it cannot show zero app-initiated traffic. This is a \
                 harness failure, not a pass."
            ),
            HarnessFailure::MonitorUnsupported { platform } => write!(
                f,
                "no egress interception mechanism is implemented for `{platform}`; set \
                 {}=1 to acknowledge an unverified platform instead of silently passing",
                plaudon_egress_monitor::ALLOW_UNSUPPORTED_ENV
            ),
            HarnessFailure::TraceUnreadable(err) => write!(f, "{err}"),
            HarnessFailure::UnexpectedEgress { scenario, attempts } => {
                write!(
                    f,
                    "`{scenario}` made {} app-initiated non-loopback attempt(s):",
                    attempts.len()
                )?;
                for attempt in attempts {
                    write!(
                        f,
                        "\n  {} -> {}:{}",
                        attempt.call, attempt.host_or_ip, attempt.port
                    )?;
                }
                Ok(())
            }
            HarnessFailure::NegativeControlSilent { scenario, detail } => write!(
                f,
                "`{scenario}` is the fixture's self-test and it recorded nothing ({detail}); \
                 the egress harness is broken and every other result in this run is \
                 unsupported"
            ),
            HarnessFailure::CloudCliExecuted { scenario, program } => write!(
                f,
                "`{scenario}` executed `{program}`: the PLAUD Cloud CLI must never run in \
                 Local-only Mode (§16.2A, §22(15))"
            ),
            HarnessFailure::CapabilityNotExplained {
                scenario,
                subsystem,
            } => write!(
                f,
                "`{scenario}` left `{subsystem}` unavailable without an explanation; §18.6A \
                 requires disabled capabilities to be explained, not silently absent"
            ),
            HarnessFailure::GateAuthorizedInLocalOnly {
                scenario,
                destination_label,
            } => write!(
                f,
                "`{scenario}`: the outbound gate authorized `{destination_label}` while in \
                 Local-only Mode"
            ),
            HarnessFailure::ScenarioFailed {
                scenario,
                exit_code,
                outcome,
            } => write!(f, "`{scenario}` exited with {exit_code:?}: {outcome}"),
            HarnessFailure::LocalWorkNotDone { scenario, detail } => write!(
                f,
                "`{scenario}` produced no egress but also did not do its local work: {detail}"
            ),
        }
    }
}

impl std::error::Error for HarnessFailure {}

/// Apply every D2.3 acceptance rule to one completed run.
///
/// `summary` is `None` when the scenario never wrote its decision file.
pub fn verify_scenario(
    scenario: Scenario,
    trace: &Trace,
    summary: Option<&ScenarioSummary>,
    exit_code: Option<i32>,
    injection_label: &str,
) -> Result<(), HarnessFailure> {
    let name = scenario.name().to_owned();

    // 1. Nothing is provable without the observer.
    if !trace.monitor_injected() {
        return Err(HarnessFailure::MonitorUnavailable {
            scenario: name,
            injection: injection_label.to_owned(),
        });
    }

    // 2. The scenario itself must have reached the outcome the spec requires.
    let Some(summary) = summary else {
        return Err(HarnessFailure::ScenarioFailed {
            scenario: name,
            exit_code,
            outcome: "no decision summary was written".to_owned(),
        });
    };
    if exit_code != Some(0) || !summary.ok {
        return Err(HarnessFailure::ScenarioFailed {
            scenario: name,
            exit_code,
            outcome: summary.outcome.clone(),
        });
    }

    // 3. Observed traffic.
    let attempts = trace.non_loopback_attempts();
    if scenario.is_negative_control() {
        if attempts.is_empty() {
            return Err(HarnessFailure::NegativeControlSilent {
                scenario: name,
                detail: "no non-loopback connect or resolution was recorded".to_owned(),
            });
        }
        if trace.cloud_cli_execs().is_empty() {
            return Err(HarnessFailure::NegativeControlSilent {
                scenario: name,
                detail: "no cloud-CLI exec was recorded, so the exec assertion is vacuous"
                    .to_owned(),
            });
        }
        return Ok(());
    }
    if !attempts.is_empty() {
        return Err(HarnessFailure::UnexpectedEgress {
            scenario: name,
            attempts: attempts.into_iter().cloned().collect(),
        });
    }

    // 4. Zero cloud-CLI process executions (§16.2A, §22(15)).
    if let Some(exec) = trace.cloud_cli_execs().first() {
        return Err(HarnessFailure::CloudCliExecuted {
            scenario: name,
            program: exec.program.clone().unwrap_or_default(),
        });
    }

    // 5. The gate must not have authorized anything in Local-only Mode.
    if let Some(allowed) = summary.decisions.iter().find(|d| d.allowed) {
        return Err(HarnessFailure::GateAuthorizedInLocalOnly {
            scenario: name,
            destination_label: allowed.destination_label.clone(),
        });
    }

    // 6. §18.6A: a disabled capability must be explained.
    if let Some(subsystem) = scenario.hostile_subsystem() {
        if !summary.explains(subsystem) {
            return Err(HarnessFailure::CapabilityNotExplained {
                scenario: name,
                subsystem: subsystem.label().to_owned(),
            });
        }
    }

    // 7. Local-only does not mean "does nothing": the local path must work.
    if scenario == Scenario::LocalImport {
        match summary.local_artifact.as_ref() {
            Some(artifact) if artifact.verified => {}
            Some(artifact) => {
                return Err(HarnessFailure::LocalWorkNotDone {
                    scenario: name,
                    detail: format!("commit of {} was not verified", artifact.committed_path),
                })
            }
            None => {
                return Err(HarnessFailure::LocalWorkNotDone {
                    scenario: name,
                    detail: "no file was staged and committed".to_owned(),
                })
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use plaudon_egress_scenario::{ExplainedDenial, LocalArtifact};
    use plaudon_security::outbound::{
        DenyReason, DisabledCapability, EgressPurpose, EgressRecord, NetworkMode, PayloadClass,
        Subsystem,
    };

    fn ready_trace() -> Trace {
        Trace::parse(
            "{\"ts_unix\":1,\"call\":\"monitor_ready\",\"family\":\"none\",\
             \"host_or_ip\":\"dyld-interpose\",\"port\":0,\"denied\":false}\n",
        )
        .unwrap()
    }

    fn with_lines(extra: &str) -> Trace {
        let mut text = String::from(
            "{\"ts_unix\":1,\"call\":\"monitor_ready\",\"family\":\"none\",\
             \"host_or_ip\":\"dyld-interpose\",\"port\":0,\"denied\":false}\n",
        );
        text.push_str(extra);
        Trace::parse(&text).unwrap()
    }

    fn summary(scenario: Scenario) -> ScenarioSummary {
        let subsystem = scenario
            .hostile_subsystem()
            .unwrap_or(Subsystem::OpenRouter);
        ScenarioSummary {
            scenario: scenario.name().to_owned(),
            ok: true,
            outcome: "denied".to_owned(),
            mode: NetworkMode::LocalOnly,
            notes: Vec::new(),
            decisions: vec![EgressRecord {
                subsystem,
                destination_label: "https://openrouter.ai:443".to_owned(),
                purpose: EgressPurpose::InferenceDispatch,
                payload: PayloadClass::TranscriptText,
                allowed: false,
                deny_reason: Some(DenyReason::LocalOnlyMode),
            }],
            disabled_capabilities: vec![DisabledCapability {
                subsystem,
                purpose: EgressPurpose::InferenceDispatch,
                reason: DenyReason::LocalOnlyMode,
                explanation: DenyReason::LocalOnlyMode.explanation().to_owned(),
            }],
            explained_denials: Vec::new(),
            not_guaranteed: vec!["os updates are outside this boundary".to_owned()],
            local_artifact: None,
            bypass_attempts: Vec::new(),
        }
    }

    #[test]
    fn a_clean_gated_scenario_passes() {
        let scenario = Scenario::OpenRouterDispatch;
        verify_scenario(
            scenario,
            &ready_trace(),
            Some(&summary(scenario)),
            Some(0),
            "enabled",
        )
        .unwrap();
    }

    #[test]
    fn missing_monitor_is_a_failure_not_a_skip() {
        let scenario = Scenario::OpenRouterDispatch;
        let empty = Trace::default();
        let err = verify_scenario(
            scenario,
            &empty,
            Some(&summary(scenario)),
            Some(0),
            "disabled",
        )
        .unwrap_err();
        assert!(matches!(err, HarnessFailure::MonitorUnavailable { .. }));
        assert!(err.to_string().contains("harness failure, not a pass"));
    }

    #[test]
    fn a_denied_attempt_in_a_gated_scenario_fails_the_run() {
        let scenario = Scenario::OpenRouterDispatch;
        let trace = with_lines(
            "{\"ts_unix\":2,\"call\":\"connect\",\"family\":\"inet\",\
             \"host_or_ip\":\"203.0.113.1\",\"port\":443,\"denied\":true}\n",
        );
        let err = verify_scenario(
            scenario,
            &trace,
            Some(&summary(scenario)),
            Some(0),
            "enabled",
        )
        .unwrap_err();
        match err {
            HarnessFailure::UnexpectedEgress { ref attempts, .. } => {
                assert_eq!(attempts.len(), 1);
            }
            other => panic!("expected UnexpectedEgress, got {other:?}"),
        }
    }

    #[test]
    fn loopback_traffic_is_allowed_in_gated_scenarios() {
        let scenario = Scenario::OpenRouterDispatch;
        let trace = with_lines(
            "{\"ts_unix\":2,\"call\":\"connect\",\"family\":\"unix\",\
             \"host_or_ip\":\"/var/run/mDNSResponder\",\"port\":0,\"denied\":false}\n\
             {\"ts_unix\":3,\"call\":\"connect\",\"family\":\"inet\",\
             \"host_or_ip\":\"127.0.0.1\",\"port\":5432,\"denied\":false}\n",
        );
        verify_scenario(
            scenario,
            &trace,
            Some(&summary(scenario)),
            Some(0),
            "enabled",
        )
        .unwrap();
    }

    #[test]
    fn a_cloud_cli_exec_fails_the_run() {
        let scenario = Scenario::CloudCli;
        let trace = with_lines(
            "{\"ts_unix\":2,\"call\":\"exec\",\"family\":\"none\",\"host_or_ip\":\"\",\
             \"port\":0,\"denied\":false,\"program\":\"/usr/local/bin/plaud\"}\n",
        );
        let err = verify_scenario(
            scenario,
            &trace,
            Some(&summary(scenario)),
            Some(0),
            "enabled",
        )
        .unwrap_err();
        assert!(matches!(err, HarnessFailure::CloudCliExecuted { .. }));
    }

    #[test]
    fn an_unrelated_exec_does_not_fail_the_run() {
        let scenario = Scenario::CloudCli;
        let trace = with_lines(
            "{\"ts_unix\":2,\"call\":\"exec\",\"family\":\"none\",\"host_or_ip\":\"\",\
             \"port\":0,\"denied\":false,\"program\":\"/usr/bin/ffprobe\"}\n",
        );
        verify_scenario(
            scenario,
            &trace,
            Some(&summary(scenario)),
            Some(0),
            "enabled",
        )
        .unwrap();
    }

    #[test]
    fn a_silent_negative_control_is_reported_as_a_broken_fixture() {
        let scenario = Scenario::NegativeControl;
        let mut control = summary(scenario);
        control.decisions.clear();
        let err = verify_scenario(scenario, &ready_trace(), Some(&control), Some(0), "enabled")
            .unwrap_err();
        assert!(matches!(err, HarnessFailure::NegativeControlSilent { .. }));
        assert!(err.to_string().contains("egress harness is broken"));
    }

    #[test]
    fn a_negative_control_without_an_exec_record_is_also_broken() {
        let scenario = Scenario::NegativeControl;
        let mut control = summary(scenario);
        control.decisions.clear();
        let trace = with_lines(
            "{\"ts_unix\":2,\"call\":\"connect\",\"family\":\"inet\",\
             \"host_or_ip\":\"203.0.113.1\",\"port\":443,\"denied\":true}\n",
        );
        let err =
            verify_scenario(scenario, &trace, Some(&control), Some(0), "enabled").unwrap_err();
        assert!(err.to_string().contains("vacuous"));
    }

    #[test]
    fn a_complete_negative_control_passes() {
        let scenario = Scenario::NegativeControl;
        let mut control = summary(scenario);
        control.decisions.clear();
        let trace = with_lines(
            "{\"ts_unix\":2,\"call\":\"connect\",\"family\":\"inet\",\
             \"host_or_ip\":\"203.0.113.1\",\"port\":443,\"denied\":true}\n\
             {\"ts_unix\":3,\"call\":\"exec\",\"family\":\"none\",\"host_or_ip\":\"\",\
             \"port\":0,\"denied\":false,\"program\":\"/tmp/bin/plaud\"}\n",
        );
        verify_scenario(scenario, &trace, Some(&control), Some(0), "enabled").unwrap();
    }

    #[test]
    fn an_unexplained_capability_fails_the_run() {
        let scenario = Scenario::AppleAssetFetch;
        let mut bare = summary(scenario);
        bare.disabled_capabilities.clear();
        bare.explained_denials.clear();
        let err =
            verify_scenario(scenario, &ready_trace(), Some(&bare), Some(0), "enabled").unwrap_err();
        assert!(matches!(err, HarnessFailure::CapabilityNotExplained { .. }));
    }

    #[test]
    fn an_explained_denial_satisfies_the_capability_rule() {
        let scenario = Scenario::FolderPlaceholder;
        let mut folder = summary(scenario);
        folder.disabled_capabilities.clear();
        folder.explained_denials = vec![ExplainedDenial {
            subsystem: Subsystem::FolderSyncProbe,
            purpose: EgressPurpose::Diagnostic,
            reason: DenyReason::LocalOnlyMode,
            destination_label: "https://folder.example.invalid:443".to_owned(),
            error_code: "EGRESS_DENIED".to_owned(),
            message: DenyReason::LocalOnlyMode.explanation().to_owned(),
            retryable: false,
            user_action: Some("DISABLE_LOCAL_ONLY_MODE".to_owned()),
        }];
        folder.decisions.clear();
        verify_scenario(scenario, &ready_trace(), Some(&folder), Some(0), "enabled").unwrap();
    }

    #[test]
    fn an_authorized_decision_fails_the_run() {
        let scenario = Scenario::UpdateCheck;
        let mut allowed = summary(scenario);
        allowed.decisions[0].allowed = true;
        allowed.decisions[0].deny_reason = None;
        let err = verify_scenario(scenario, &ready_trace(), Some(&allowed), Some(0), "enabled")
            .unwrap_err();
        assert!(matches!(
            err,
            HarnessFailure::GateAuthorizedInLocalOnly { .. }
        ));
    }

    #[test]
    fn local_import_must_really_commit_its_file() {
        let scenario = Scenario::LocalImport;
        let mut local = summary(scenario);
        local.decisions.clear();
        local.disabled_capabilities.clear();
        let err = verify_scenario(scenario, &ready_trace(), Some(&local), Some(0), "enabled")
            .unwrap_err();
        assert!(matches!(err, HarnessFailure::LocalWorkNotDone { .. }));

        local.local_artifact = Some(LocalArtifact {
            staging_path: "/w/staging/import.part".to_owned(),
            committed_path: "/w/library/import.bin".to_owned(),
            byte_length: 59,
            sha256: "00".repeat(32),
            verified: true,
        });
        verify_scenario(scenario, &ready_trace(), Some(&local), Some(0), "enabled").unwrap();
    }

    #[test]
    fn a_nonzero_exit_or_missing_summary_fails_the_run() {
        let scenario = Scenario::UpdateCheck;
        let err = verify_scenario(scenario, &ready_trace(), None, Some(0), "enabled").unwrap_err();
        assert!(matches!(err, HarnessFailure::ScenarioFailed { .. }));
        let err = verify_scenario(
            scenario,
            &ready_trace(),
            Some(&summary(scenario)),
            Some(1),
            "enabled",
        )
        .unwrap_err();
        assert!(matches!(err, HarnessFailure::ScenarioFailed { .. }));
    }
}
