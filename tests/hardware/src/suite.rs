//! Turns observations into an [`EvidenceRecord`] and executes one named check
//! under the opt-in gate.

use plaudon_plaud_protocol::opcode::ReadOnlyOpcode;
use plaudon_source_plaud_device::evidence::{
    CheckOutcome, CheckResult, EvidenceRecord, EvidenceSource,
};
use plaudon_source_plaud_device::register::CompatibilityRegister;
use time::macros::format_description;
use time::OffsetDateTime;

use crate::catalogue::{self, CheckSpec, Derivation, CATALOGUE, DISCONNECT_STAGES};
use crate::emitter;
use crate::gate::{self, Env, GateDecision, GateError, SkipReason};
use crate::guard::{self, REFUSED_DETAIL};
use crate::host::HostInfo;
use crate::observations::{Observations, ObservationsError};
use crate::profile::{self, ProfileError, ResolvedProfile};

/// Why an opted-in run could not produce a result.
#[derive(Debug, thiserror::Error)]
pub enum SuiteError {
    /// The gate is on but misconfigured.
    #[error(transparent)]
    Gate(#[from] GateError),
    /// The profile is unknown.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// The observations could not be loaded.
    #[error(transparent)]
    Observations(#[from] ObservationsError),
    /// The committed register is unreadable.
    #[error("cannot load the committed compatibility register: {0}")]
    Register(String),
    /// The evidence could not be written.
    #[error(transparent)]
    Emit(#[from] emitter::EmitError),
    /// The requested check is not in the catalogue.
    #[error("{0} is not a check of this suite")]
    UnknownCheck(String),
}

/// What executing one check did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Execution {
    /// The gate was off; hardware was not touched and nothing was recorded.
    Skipped(SkipReason),
    /// The check ran; the result is recorded.
    Ran(CheckResult),
}

/// Today's date, `YYYY-MM-DD`, UTC.
#[must_use]
pub fn today() -> String {
    OffsetDateTime::now_utc()
        .date()
        .format(format_description!("[year]-[month]-[day]"))
        .expect("a calendar date always formats")
}

/// A fresh run identifier.
#[must_use]
pub fn new_run_id(host_os: &str) -> String {
    format!("hw-{host_os}-{}", uuid::Uuid::new_v4().simple())
}

/// Parameters of one assembly.
#[derive(Debug, Clone)]
pub struct Assembly<'a> {
    /// Identifier of this run.
    pub test_run_id: String,
    /// Run date, `YYYY-MM-DD`.
    pub recorded_at: String,
    /// Where the observations came from.
    pub source: EvidenceSource,
    /// Resolved profile.
    pub profile: &'a ResolvedProfile,
    /// Host.
    pub host: &'a HostInfo,
    /// Observations.
    pub observations: &'a Observations,
}

fn allowlisted(emitted: &str) -> bool {
    let normalise = |text: &str| -> String {
        text.chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase()
    };
    let wanted = normalise(emitted);
    ReadOnlyOpcode::ALL.iter().any(|opcode| {
        serde_json::to_value(opcode)
            .ok()
            .and_then(|value| value.as_str().map(normalise))
            .is_some_and(|name| name == wanted)
    })
}

fn result(id: &str, outcome: CheckOutcome, detail: impl Into<String>) -> CheckResult {
    CheckResult::new(id, outcome, detail)
}

fn observed(observations: &Observations, id: &str) -> CheckResult {
    match observations.checks.get(id) {
        None => result(id, CheckOutcome::Skipped, "not observed in this run"),
        Some(check) => match guard::check_detail(&check.detail) {
            Ok(()) => result(id, check.outcome, check.detail.clone()),
            Err(_) => result(id, CheckOutcome::Failed, REFUSED_DETAIL),
        },
    }
}

fn evaluate(spec: &CheckSpec, observations: &Observations, results: &[CheckResult]) -> CheckResult {
    let id = spec.id;
    let verified = observations
        .transfer
        .as_ref()
        .is_some_and(plaudon_source_plaud_device::evidence::TransferEvidence::is_verified);
    match spec.derivation {
        Derivation::Observed => observed(observations, id),
        Derivation::FromOpcodeTrace => {
            let trace = &observations.emitted_opcodes;
            if trace.is_empty() {
                return result(id, CheckOutcome::Skipped, "no opcode trace was captured");
            }
            let forbidden = trace.iter().any(|emitted| {
                EvidenceRecord::FORBIDDEN_OPCODES
                    .iter()
                    .any(|name| emitted.eq_ignore_ascii_case(name))
            });
            if forbidden {
                return result(id, CheckOutcome::Failed, "a forbidden opcode was emitted");
            }
            if let Some(unknown) = trace.iter().find(|emitted| !allowlisted(emitted)) {
                let named = if guard::check_detail(unknown).is_ok() && unknown.len() <= 40 {
                    unknown.as_str()
                } else {
                    "(unprintable)"
                };
                return result(
                    id,
                    CheckOutcome::Failed,
                    format!("opcode {named} is outside the read-only allowlist"),
                );
            }
            result(
                id,
                CheckOutcome::Passed,
                format!(
                    "{} opcodes emitted, all on the read-only allowlist",
                    trace.len()
                ),
            )
        }
        Derivation::FromDisconnectStages => {
            let stage = |stage: &&str| results.iter().find(|r| r.id == **stage).map(|r| r.outcome);
            let outcomes: Vec<_> = DISCONNECT_STAGES.iter().map(stage).collect();
            if outcomes.contains(&Some(CheckOutcome::Failed)) {
                result(
                    id,
                    CheckOutcome::Failed,
                    "at least one disconnect stage failed",
                )
            } else if outcomes.iter().all(|o| *o == Some(CheckOutcome::Passed)) {
                result(
                    id,
                    CheckOutcome::Passed,
                    format!("all {} stages passed", DISCONNECT_STAGES.len()),
                )
            } else {
                result(
                    id,
                    CheckOutcome::Skipped,
                    "not every disconnect stage was observed",
                )
            }
        }
        Derivation::FromTransfer => match &observations.transfer {
            None => result(
                id,
                CheckOutcome::Skipped,
                "no completed transfer was reported",
            ),
            Some(_) if verified => result(
                id,
                CheckOutcome::Passed,
                "terminal marker, declared length and digest all agree",
            ),
            Some(_) => result(
                id,
                CheckOutcome::Failed,
                "the transfer lacks a terminal marker, a matching declared length or a digest",
            ),
        },
        Derivation::ObservedNeedsVerifiedTransfer => {
            let base = observed(observations, id);
            if base.outcome == CheckOutcome::Passed && !verified {
                result(
                    id,
                    CheckOutcome::Failed,
                    "claimed without a verified transfer",
                )
            } else {
                base
            }
        }
        Derivation::ObservedNeedsNetworkDenied => {
            let base = observed(observations, id);
            if base.outcome == CheckOutcome::Passed && !observations.network_denied_run {
                result(
                    id,
                    CheckOutcome::Failed,
                    "claimed without a proven network-denied run",
                )
            } else {
                base
            }
        }
    }
}

/// Compute every check result from `observations`.
#[must_use]
pub fn evaluate_all(observations: &Observations) -> Vec<CheckResult> {
    let mut results: Vec<CheckResult> = Vec::with_capacity(CATALOGUE.len());
    for spec in CATALOGUE {
        let next = evaluate(spec, observations, &results);
        results.push(next);
    }
    results
}

/// Assemble the evidence record.
#[must_use]
pub fn assemble(parameters: &Assembly<'_>) -> EvidenceRecord {
    let cell = &parameters.profile.cell;
    let observations = parameters.observations;
    EvidenceRecord {
        schema_version: EvidenceRecord::SCHEMA_VERSION,
        test_run_id: parameters.test_run_id.clone(),
        recorded_at: parameters.recorded_at.clone(),
        source: parameters.source,
        source_type: cell.source_type.clone(),
        vendor: cell.vendor.clone(),
        device_model: cell.device_model.clone(),
        observed_firmware: observations.observed_firmware.clone(),
        host_os: parameters.host.os.clone(),
        host_os_build: parameters.host.build.clone(),
        transport: cell.transport.clone(),
        auth_mode: cell.auth_mode.clone(),
        provisioning_route: cell.provisioning_method.clone(),
        transfer: observations.transfer.clone(),
        non_destructive_scan_observed: observations.non_destructive_scan_observed,
        emitted_opcodes: observations.emitted_opcodes.clone(),
        network_denied_run: observations.network_denied_run,
        checks: evaluate_all(observations),
    }
}

fn committed_register() -> Result<CompatibilityRegister, SuiteError> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compatibility.yaml");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| SuiteError::Register(format!("{}: {error}", path.display())))?;
    CompatibilityRegister::from_yaml(&text).map_err(|error| SuiteError::Register(error.to_string()))
}

/// Build the record for an opted-in run from the process environment.
pub fn record_for_run(config: &gate::RunConfig) -> Result<EvidenceRecord, SuiteError> {
    let host = HostInfo::capture();
    let profile = profile::resolve(&config.profile, &host.os, &committed_register()?)?;
    let observations = Observations::load(&config.observations)?;
    Ok(assemble(&Assembly {
        test_run_id: new_run_id(&host.os),
        recorded_at: today(),
        source: EvidenceSource::Hardware,
        profile: &profile,
        host: &host,
        observations: &observations,
    }))
}

/// Execute one named check under the gate.
///
/// With the gate off this returns [`Execution::Skipped`] without reading any
/// file or touching any device.
pub fn execute(id: &str, env: &dyn Env) -> Result<Execution, SuiteError> {
    if catalogue::find(id).is_none() {
        return Err(SuiteError::UnknownCheck(id.to_owned()));
    }
    match gate::evaluate(env)? {
        GateDecision::Skip(reason) => Ok(Execution::Skipped(reason)),
        GateDecision::Run(config) => {
            let record = record_for_run(&config)?;
            let found = record
                .checks
                .into_iter()
                .find(|check| check.id == id)
                .expect("every catalogue check is evaluated");
            Ok(Execution::Ran(found))
        }
    }
}

/// Execute the evidence emission under the gate: write the record at the
/// runner-chosen path.
pub fn emit(env: &dyn Env) -> Result<Option<EvidenceRecord>, SuiteError> {
    match gate::evaluate(env)? {
        GateDecision::Skip(_) => Ok(None),
        GateDecision::Run(config) => {
            let record = record_for_run(&config)?;
            if let Some(path) = &config.evidence_out {
                emitter::write_record(path, &record)?;
            }
            Ok(Some(record))
        }
    }
}
