//! The opt-in gate (spec §18.3A: the suite "requires explicit opt-in and real
//! hardware").
//!
//! The gate is a pure function of an [`Env`], so the default-run behaviour is
//! testable hermetically: with the gate off every hardware check resolves to
//! [`GateDecision::Skip`], which the hardware tests print as `SKIPPED` and the
//! self-test asserts on, so a skip can never be mistaken for a silent pass.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Set to exactly `1` to opt in.
pub const GATE_ENV: &str = "PLAUDON_HARDWARE_TESTS";
/// Named device profile, `<source_type>:<model>:<auth_mode>`.
pub const PROFILE_ENV: &str = "PLAUDON_HARDWARE_PROFILE";
/// Observations file written by the run harness.
pub const OBSERVATIONS_ENV: &str = "PLAUDON_HARDWARE_OBSERVATIONS";
/// Where the evidence record is written.
pub const EVIDENCE_OUT_ENV: &str = "PLAUDON_HARDWARE_EVIDENCE_OUT";

/// Source of environment variables.
pub trait Env {
    /// Value of `key`, if set.
    fn get(&self, key: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// A fixed environment for hermetic tests.
#[derive(Debug, Clone, Default)]
pub struct MapEnv(BTreeMap<String, String>);

impl MapEnv {
    /// Build from pairs.
    #[must_use]
    pub fn new<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self(
            pairs
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        )
    }
}

impl Env for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

/// Why a run was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `PLAUDON_HARDWARE_TESTS` is unset or not `1`.
    GateOff,
}

/// Why an opted-in run cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    /// The gate is on but no device profile was named.
    #[error("{PROFILE_ENV} must name the device profile under test")]
    MissingProfile,
    /// The gate is on but no observations file was named.
    #[error("{OBSERVATIONS_ENV} must point at the run harness observations file")]
    MissingObservations,
}

/// Everything an opted-in run needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    /// Device profile name.
    pub profile: String,
    /// Observations file.
    pub observations: PathBuf,
    /// Evidence output path, when the runner chose one.
    pub evidence_out: Option<PathBuf>,
}

/// Outcome of evaluating the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Do not touch hardware.
    Skip(SkipReason),
    /// Run against hardware.
    Run(RunConfig),
}

impl GateDecision {
    /// Whether the suite is allowed to touch hardware.
    #[must_use]
    pub const fn is_run(&self) -> bool {
        matches!(self, Self::Run(_))
    }
}

/// Evaluate the gate.
///
/// Only the exact value `1` opts in; `true`, `yes` and the empty string do not,
/// so an accidental export cannot start a hardware run.
pub fn evaluate(env: &dyn Env) -> Result<GateDecision, GateError> {
    if env.get(GATE_ENV).as_deref() != Some("1") {
        return Ok(GateDecision::Skip(SkipReason::GateOff));
    }
    let profile = env
        .get(PROFILE_ENV)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(GateError::MissingProfile)?;
    let observations = env
        .get(OBSERVATIONS_ENV)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or(GateError::MissingObservations)?;
    let evidence_out = env
        .get(EVIDENCE_OUT_ENV)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from);
    Ok(GateDecision::Run(RunConfig {
        profile,
        observations,
        evidence_out,
    }))
}
