//! `plaudon-egress-scenario` — the subject process of the D2.3 network-deny
//! fixture (spec §16.2A, §1.5, §18.6A "Local-only", §22(15)).
//!
//! Each scenario drives one subsystem through the **real**
//! `plaudon_security::outbound` gate in Local-only Mode and writes a JSON
//! summary of every policy decision to the file named by
//! `PLAUDON_EGRESS_DECISIONS`. The process is run under the egress monitor, so
//! the harness can compare what the policy *said* with what the process
//! *actually did* at the libc boundary.
//!
//! Exit status is 0 only when the scenario observed the outcome the spec
//! requires; anything else is a non-zero exit with the reason in the summary.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_security::error::SecurityError;
use plaudon_security::fsguard::{canonicalize_and_validate, PathError, PathPolicy, PrivateTempDir};
use plaudon_security::outbound::http::GatedHttpClientFactory;
use plaudon_security::outbound::subprocess::{CliArg, GatedProcessLauncher, SpawnError};
use plaudon_security::outbound::{
    DenyReason, DisabledCapability, EffectiveNetworkPolicy, EgressDenied, EgressPurpose,
    EgressRecord, NetworkMode, OutboundPolicy, PayloadClass, Subsystem,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-egress-scenario";

/// Where the scenario writes its JSON decision summary.
pub const DECISIONS_ENV: &str = "PLAUDON_EGRESS_DECISIONS";
/// Scratch directory the scenario may create files in.
pub const WORKDIR_ENV: &str = "PLAUDON_SCENARIO_WORKDIR";

/// An address in `TEST-NET-3` (RFC 5737), reserved for documentation and
/// guaranteed not to be a real service. Used only by the negative control.
pub const NEGATIVE_CONTROL_ADDR: &str = "203.0.113.1:443";
/// A name under the reserved `.invalid` TLD (RFC 6761), which can never resolve.
pub const NEGATIVE_CONTROL_HOST: &str = "egress-negative-control.invalid:443";

/// The scenarios the harness can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Local staged write + atomic rename. Must succeed with zero egress.
    LocalImport,
    /// §18.6A Cloud row / §22(15): `OpenRouter` dispatch.
    OpenRouterDispatch,
    /// §18.6A Local-only row: update checker.
    UpdateCheck,
    /// Model catalogue refresh.
    ModelRefresh,
    /// Model weight download.
    ModelDownload,
    /// §18.6A Local-only row: telemetry.
    TelemetryFlush,
    /// §18.6A Local-only row: Apple model/asset fetch.
    AppleAssetFetch,
    /// §18.6A Local-only row + §16.2A: the cloud CLI subprocess.
    CloudCli,
    /// §18.6A Local-only row: a cloud-synced folder placeholder.
    FolderPlaceholder,
    /// Deliberately bypasses the gate. Proves the fixture can fail.
    NegativeControl,
}

impl Scenario {
    pub const ALL: [Scenario; 10] = [
        Scenario::LocalImport,
        Scenario::OpenRouterDispatch,
        Scenario::UpdateCheck,
        Scenario::ModelRefresh,
        Scenario::ModelDownload,
        Scenario::TelemetryFlush,
        Scenario::AppleAssetFetch,
        Scenario::CloudCli,
        Scenario::FolderPlaceholder,
        Scenario::NegativeControl,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scenario::LocalImport => "local-import",
            Scenario::OpenRouterDispatch => "openrouter-dispatch",
            Scenario::UpdateCheck => "update-check",
            Scenario::ModelRefresh => "model-refresh",
            Scenario::ModelDownload => "model-download",
            Scenario::TelemetryFlush => "telemetry-flush",
            Scenario::AppleAssetFetch => "apple-asset-fetch",
            Scenario::CloudCli => "cloud-cli",
            Scenario::FolderPlaceholder => "folder-placeholder",
            Scenario::NegativeControl => "negative-control",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Scenario> {
        Scenario::ALL.into_iter().find(|s| s.name() == raw)
    }

    /// The negative control is the only scenario allowed to produce network
    /// attempts; it exists so the harness can prove it is able to fail.
    #[must_use]
    pub const fn is_negative_control(self) -> bool {
        matches!(self, Scenario::NegativeControl)
    }

    /// The subsystem whose capability must be visibly disabled **and
    /// explained** for this §18.6A hostile case.
    #[must_use]
    pub const fn hostile_subsystem(self) -> Option<Subsystem> {
        match self {
            Scenario::OpenRouterDispatch => Some(Subsystem::OpenRouter),
            Scenario::UpdateCheck => Some(Subsystem::UpdateChecker),
            Scenario::ModelRefresh | Scenario::ModelDownload => Some(Subsystem::ModelDistribution),
            Scenario::TelemetryFlush => Some(Subsystem::Telemetry),
            Scenario::AppleAssetFetch => Some(Subsystem::AppleAssetFetch),
            Scenario::CloudCli => Some(Subsystem::PlaudCloudCli),
            Scenario::FolderPlaceholder => Some(Subsystem::FolderSyncProbe),
            Scenario::LocalImport | Scenario::NegativeControl => None,
        }
    }
}

/// A refused attempt rendered with its §15.4 error envelope, so the summary
/// carries the same explanation the user would see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainedDenial {
    pub subsystem: Subsystem,
    pub purpose: EgressPurpose,
    pub reason: DenyReason,
    pub destination_label: String,
    pub error_code: String,
    pub message: String,
    pub retryable: bool,
    pub user_action: Option<String>,
}

impl ExplainedDenial {
    #[must_use]
    pub fn new(denied: &EgressDenied, purpose: EgressPurpose) -> ExplainedDenial {
        let parts = denied.envelope_parts();
        ExplainedDenial {
            subsystem: denied.subsystem,
            purpose,
            reason: denied.reason,
            destination_label: denied.destination_label.clone(),
            error_code: parts.code.to_string(),
            message: parts.message,
            retryable: parts.retryable,
            user_action: parts.action.map(|a| a.to_string()),
        }
    }
}

/// Evidence that the local (non-network) work really happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalArtifact {
    pub staging_path: String,
    pub committed_path: String,
    pub byte_length: u64,
    pub sha256: String,
    /// True when the staging file no longer exists and the committed file
    /// holds exactly the expected bytes.
    pub verified: bool,
}

/// One network attempt the negative control made on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassAttempt {
    pub kind: String,
    pub target: String,
    pub error: Option<String>,
    pub succeeded: bool,
}

/// What a scenario did, as JSON for the harness and the §24 evidence bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioSummary {
    pub scenario: String,
    /// True when the scenario observed the outcome the spec requires.
    pub ok: bool,
    pub outcome: String,
    pub mode: NetworkMode,
    pub notes: Vec<String>,
    /// Every decision the gate made, allow or deny (`policy.observer().records()`).
    pub decisions: Vec<EgressRecord>,
    /// Capabilities the policy reports as unavailable, each with a reason.
    pub disabled_capabilities: Vec<DisabledCapability>,
    /// Denials with their user-facing §15.4 envelope.
    pub explained_denials: Vec<ExplainedDenial>,
    /// What this egress boundary explicitly does not cover (§16.2A).
    pub not_guaranteed: Vec<String>,
    pub local_artifact: Option<LocalArtifact>,
    pub bypass_attempts: Vec<BypassAttempt>,
}

impl ScenarioSummary {
    fn new(
        scenario: Scenario,
        policy: &OutboundPolicy,
        effective: &EffectiveNetworkPolicy,
    ) -> Self {
        ScenarioSummary {
            scenario: scenario.name().to_owned(),
            ok: false,
            outcome: String::new(),
            mode: policy.mode(),
            notes: Vec::new(),
            decisions: policy.observer().records(),
            disabled_capabilities: effective.disabled_capabilities.clone(),
            explained_denials: Vec::new(),
            not_guaranteed: effective.not_guaranteed.clone(),
            local_artifact: None,
            bypass_attempts: Vec::new(),
        }
    }

    /// True when this summary explains why `subsystem` is unavailable, either
    /// as a described disabled capability or as a recorded denial.
    #[must_use]
    pub fn explains(&self, subsystem: Subsystem) -> bool {
        self.disabled_capabilities
            .iter()
            .any(|c| c.subsystem == subsystem && !c.explanation.trim().is_empty())
            || self
                .explained_denials
                .iter()
                .any(|d| d.subsystem == subsystem && !d.message.trim().is_empty())
    }
}

/// Everything a scenario needs from its environment.
#[derive(Debug, Clone)]
pub struct ScenarioContext {
    pub workdir: PathBuf,
}

impl ScenarioContext {
    /// Use `PLAUDON_SCENARIO_WORKDIR` when the harness provided one, otherwise
    /// a process-unique directory under the system temp dir.
    pub fn from_env() -> io::Result<ScenarioContext> {
        let workdir = match std::env::var_os(WORKDIR_ENV) {
            Some(dir) => PathBuf::from(dir),
            None => std::env::temp_dir().join(format!("plaudon-egress-{}", std::process::id())),
        };
        fs::create_dir_all(&workdir)?;
        Ok(ScenarioContext { workdir })
    }
}

/// Run one scenario. Never panics: I/O problems are reported in the summary.
#[must_use]
pub fn run(scenario: Scenario, ctx: &ScenarioContext) -> ScenarioSummary {
    let policy = Arc::new(OutboundPolicy::local_only());
    let effective = policy.describe();
    let mut summary = ScenarioSummary::new(scenario, &policy, &effective);

    match scenario {
        Scenario::LocalImport => run_local_import(ctx, &mut summary),
        Scenario::OpenRouterDispatch => run_http_case(
            &policy,
            &mut summary,
            Subsystem::OpenRouter,
            EgressPurpose::InferenceDispatch,
            PayloadClass::TranscriptText,
            "https://openrouter.ai/api/v1/chat/completions",
            true,
        ),
        Scenario::UpdateCheck => run_http_case(
            &policy,
            &mut summary,
            Subsystem::UpdateChecker,
            EgressPurpose::UpdateCheck,
            PayloadClass::None,
            "https://updates.example.invalid/plaudon/latest.json",
            false,
        ),
        Scenario::ModelRefresh => run_http_case(
            &policy,
            &mut summary,
            Subsystem::ModelDistribution,
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
            "https://models.example.invalid/catalog.json",
            false,
        ),
        Scenario::ModelDownload => run_http_case(
            &policy,
            &mut summary,
            Subsystem::ModelDistribution,
            EgressPurpose::ModelDownload,
            PayloadClass::None,
            "https://models.example.invalid/stt/small.bin",
            false,
        ),
        Scenario::TelemetryFlush => run_http_case(
            &policy,
            &mut summary,
            Subsystem::Telemetry,
            EgressPurpose::TelemetryFlush,
            PayloadClass::Metadata,
            "https://telemetry.example.invalid/v1/events",
            true,
        ),
        Scenario::AppleAssetFetch => run_http_case(
            &policy,
            &mut summary,
            Subsystem::AppleAssetFetch,
            EgressPurpose::AssetDownload,
            PayloadClass::None,
            "https://apple.example.invalid/speech/en-US.asset",
            false,
        ),
        Scenario::CloudCli => run_cloud_cli(&policy, &mut summary),
        Scenario::FolderPlaceholder => run_folder_placeholder(&policy, ctx, &mut summary),
        Scenario::NegativeControl => run_negative_control(ctx, &mut summary),
    }

    // Refresh the decision list: the calls above went through the gate, which
    // records every allow and every deny.
    summary.decisions = policy.observer().records();
    summary
}

// ---------------------------------------------------------------------------
// Scenario bodies
// ---------------------------------------------------------------------------

/// Stage a file privately and commit it with an atomic rename through the real
/// `plaudon_security::fsguard` path (§16.5, §7.4, §6.3B). No network, no
/// subprocess.
fn run_local_import(ctx: &ScenarioContext, summary: &mut ScenarioSummary) {
    summary.notes.push(
        "staged through plaudon_security::fsguard::PrivateTempDir and committed with \
         StagedFile::commit_by_rename"
            .to_owned(),
    );

    let payload: &[u8] = b"plaudon local import fixture: 1 kHz tone placeholder bytes\n";
    match stage_and_commit(ctx, payload) {
        Ok(artifact) => {
            summary.ok = artifact.verified;
            summary.outcome = if artifact.verified {
                "committed a local file with zero egress".to_owned()
            } else {
                "local commit could not be verified".to_owned()
            };
            summary.local_artifact = Some(artifact);
        }
        Err(err) => {
            summary.ok = false;
            summary.outcome = format!("local import failed: {err}");
        }
    }
}

/// Why the local import could not complete. Both variants are real failures;
/// neither is swallowed.
#[derive(Debug)]
enum LocalImportError {
    Io(io::Error),
    Path(PathError),
}

impl std::fmt::Display for LocalImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalImportError::Io(err) => write!(f, "{err}"),
            LocalImportError::Path(err) => write!(f, "{err}"),
        }
    }
}

impl From<io::Error> for LocalImportError {
    fn from(err: io::Error) -> Self {
        LocalImportError::Io(err)
    }
}

impl From<PathError> for LocalImportError {
    fn from(err: PathError) -> Self {
        LocalImportError::Path(err)
    }
}

fn stage_and_commit(
    ctx: &ScenarioContext,
    payload: &[u8],
) -> Result<LocalArtifact, LocalImportError> {
    use std::io::Write;

    let staging_base = ctx.workdir.join("staging");
    let library_dir = ctx.workdir.join("library");
    fs::create_dir_all(&staging_base)?;
    fs::create_dir_all(&library_dir)?;

    let private = PrivateTempDir::create(Some(&staging_base), "plaudon-egress-import")?;
    let mut staged = private.stage_file("import")?;
    staged.write_all(payload)?;
    let staging_path = staged.path().as_path().to_path_buf();

    let policy = PathPolicy::for_roots([library_dir.clone()]);
    let target = canonicalize_and_validate(&policy, &library_dir.join("import.bin"))?;
    let committed_path = staged.commit_by_rename(&target)?;

    let committed = fs::read(committed_path.as_path())?;
    let digest = hex::encode(Sha256::digest(&committed));
    let expected = hex::encode(Sha256::digest(payload));
    let verified = committed == payload
        && digest == expected
        && !staging_path.exists()
        && committed_path.as_path().is_file();

    Ok(LocalArtifact {
        staging_path: staging_path.display().to_string(),
        committed_path: committed_path.as_path().display().to_string(),
        byte_length: committed.len() as u64,
        sha256: digest,
        verified,
    })
}

/// Attempt a real gated HTTP request and require that it is refused.
fn run_http_case(
    policy: &Arc<OutboundPolicy>,
    summary: &mut ScenarioSummary,
    subsystem: Subsystem,
    purpose: EgressPurpose,
    payload: PayloadClass,
    url: &str,
    post: bool,
) {
    let factory = GatedHttpClientFactory::new(Arc::clone(policy));
    let attempt = if post {
        factory.post(url, subsystem, purpose, payload)
    } else {
        factory.get(url, subsystem, purpose, payload)
    };

    match attempt {
        Ok(request) => {
            summary.ok = false;
            summary.outcome = format!(
                "POLICY FAILURE: gate authorized {} in Local-only Mode ({})",
                subsystem.label(),
                request.ticket().destination_label()
            );
        }
        Err(denied) => {
            let explained = ExplainedDenial::new(&denied, purpose);
            summary.ok = denied.reason == DenyReason::LocalOnlyMode;
            summary.outcome = format!(
                "{} refused: {} — {}",
                subsystem.label(),
                explained.reason.label(),
                explained.message
            );
            if !summary.ok {
                summary.notes.push(format!(
                    "expected deny reason local_only_mode, observed {}",
                    explained.reason.label()
                ));
            }
            summary.explained_denials.push(explained);
        }
    }
}

/// §16.2A: the cloud CLI must never be launched in Local-only Mode. The
/// launcher refuses before a `Command` even exists; the monitor independently
/// proves no `exec` happened.
fn run_cloud_cli(policy: &Arc<OutboundPolicy>, summary: &mut ScenarioSummary) {
    let launcher = GatedProcessLauncher::new(Arc::clone(policy));
    let program = PathBuf::from("/usr/local/bin/plaud");
    let args = [CliArg::Literal("recordings"), CliArg::Literal("list")];

    match launcher.prepare_cloud_cli(&program, &args, EgressPurpose::AccountSync) {
        Ok(command) => {
            summary.ok = false;
            summary.outcome = format!(
                "POLICY FAILURE: launcher prepared {} in Local-only Mode",
                command.program().display()
            );
        }
        Err(SpawnError::Denied(denied)) => {
            let explained = ExplainedDenial::new(&denied, EgressPurpose::AccountSync);
            summary.ok = denied.reason == DenyReason::LocalOnlyMode;
            summary.outcome = format!(
                "cloud CLI launch refused: {} — {}",
                explained.reason.label(),
                explained.message
            );
            summary.explained_denials.push(explained);
            summary.notes.push(
                "no std::process::Command was constructed; the monitor's exec trace is the \
                 independent proof that no cloud CLI process ran"
                    .to_owned(),
            );
        }
        Err(other) => {
            summary.ok = false;
            summary.outcome = format!("unexpected launcher error: {other}");
        }
    }
}

/// §16.2A/§18.6A: a cloud-synced folder placeholder must leave the capability
/// disabled and labelled, never silently hydrated.
///
/// Hydration is performed by the sync client, not by this process, so the
/// honest behaviour under test is: recognise the placeholder, do **not** read
/// its contents (reading is what would make the sync client fetch it), and
/// report an explained disabled capability.
fn run_folder_placeholder(
    policy: &Arc<OutboundPolicy>,
    ctx: &ScenarioContext,
    summary: &mut ScenarioSummary,
) {
    let watched = ctx.workdir.join("watched");
    let placeholder = watched.join("2026-03-04 Standup.m4a.icloud");
    if let Err(err) = fs::create_dir_all(&watched).and_then(|()| fs::write(&placeholder, b"")) {
        summary.ok = false;
        summary.outcome = format!("could not create the placeholder fixture: {err}");
        return;
    }

    let recognised = is_cloud_placeholder(&placeholder);
    if !recognised {
        summary.ok = false;
        summary.outcome = String::from("placeholder was not recognised as cloud-backed");
        return;
    }
    summary.notes.push(format!(
        "recognised cloud placeholder {} by name; contents were never read",
        placeholder.display()
    ));

    // Ask the gate whether this app may probe the sync backend. It may not:
    // a watched folder is inspected locally and has no legitimate egress.
    let request = plaudon_security::outbound::EgressRequest::new(
        Subsystem::FolderSyncProbe,
        plaudon_security::outbound::Destination::Network {
            scheme: "https".to_owned(),
            host: "folder.example.invalid".to_owned(),
            port: 443,
        },
        EgressPurpose::Diagnostic,
        PayloadClass::None,
    );
    match policy.authorize(&request) {
        Ok(_) => {
            summary.ok = false;
            summary.outcome = String::from(
                "POLICY FAILURE: gate authorized a cloud-folder probe in Local-only Mode",
            );
        }
        Err(denied) => {
            let explained = ExplainedDenial::new(&denied, EgressPurpose::Diagnostic);
            summary.ok = true;
            summary.outcome = format!(
                "cloud-synced folder capability disabled and labelled: {}",
                explained.message
            );
            summary.explained_denials.push(explained);
            summary.notes.push(
                "a cloud-sync client acting on a user-selected folder is outside this \
                 application's egress boundary; the folder is disabled or labelled rather than \
                 silently used (§16.2A)"
                    .to_owned(),
            );
        }
    }
}

/// Recognise a cloud-sync placeholder by its marker name. Deliberately a
/// name-only check: stat-ing or opening the file is what triggers hydration.
#[must_use]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let extension_is = |want: &str| {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(want))
    };
    extension_is("icloud")
        || extension_is("cloudf")
        || extension_is("gdoc")
        || name.ends_with(":Zone.Identifier")
        || (name.starts_with('.') && name.to_ascii_lowercase().contains(".icloud"))
}

/// Deliberately bypass the gate. This scenario exists only so the harness can
/// prove it is capable of detecting real traffic; if the monitor ever reports
/// nothing here, the fixture is broken.
fn run_negative_control(ctx: &ScenarioContext, summary: &mut ScenarioSummary) {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    summary.notes.push(
        "this scenario intentionally bypasses plaudon_security::outbound; it is the fixture's \
         self-test, not application behaviour"
            .to_owned(),
    );

    let mut attempts = Vec::new();

    match NEGATIVE_CONTROL_ADDR.parse() {
        Ok(addr) => {
            let result = TcpStream::connect_timeout(&addr, Duration::from_secs(2));
            attempts.push(BypassAttempt {
                kind: "tcp_connect".to_owned(),
                target: NEGATIVE_CONTROL_ADDR.to_owned(),
                error: result.as_ref().err().map(ToString::to_string),
                succeeded: result.is_ok(),
            });
        }
        Err(err) => attempts.push(BypassAttempt {
            kind: "tcp_connect".to_owned(),
            target: NEGATIVE_CONTROL_ADDR.to_owned(),
            error: Some(err.to_string()),
            succeeded: false,
        }),
    }

    let lookup = NEGATIVE_CONTROL_HOST.to_socket_addrs();
    attempts.push(BypassAttempt {
        kind: "dns_lookup".to_owned(),
        target: NEGATIVE_CONTROL_HOST.to_owned(),
        error: lookup.as_ref().err().map(ToString::to_string),
        succeeded: lookup.is_ok(),
    });

    // Also prove the exec trace is not vacuous: run a stand-in executable whose
    // leaf name is exactly the pinned cloud-CLI name. The harness requires zero
    // such records in every other scenario, so this is what makes that
    // assertion meaningful.
    match spawn_fake_cloud_cli(ctx) {
        Ok(status) => attempts.push(BypassAttempt {
            kind: "exec_fake_cloud_cli".to_owned(),
            target: plaudon_security::outbound::PLAUD_CLI_PROGRAM.to_owned(),
            error: None,
            succeeded: status,
        }),
        Err(err) => attempts.push(BypassAttempt {
            kind: "exec_fake_cloud_cli".to_owned(),
            target: plaudon_security::outbound::PLAUD_CLI_PROGRAM.to_owned(),
            error: Some(err.to_string()),
            succeeded: false,
        }),
    }

    let executed = attempts
        .iter()
        .any(|a| a.kind == "exec_fake_cloud_cli" && a.succeeded);
    summary.bypass_attempts = attempts;
    // The control succeeds when it *made* the attempts; whether they connected
    // is irrelevant (and must not depend on the CI machine having a network).
    summary.ok = summary.bypass_attempts.len() == 3 && executed;
    summary.outcome = if summary.ok {
        "made 2 un-gated network attempts and 1 un-gated exec".to_owned()
    } else {
        "negative control could not make its attempts".to_owned()
    };
}

/// Write and run a stand-in for the cloud CLI, named `plaud`.
fn spawn_fake_cloud_cli(ctx: &ScenarioContext) -> io::Result<bool> {
    let bin_dir = ctx.workdir.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let program = bin_dir.join(plaudon_security::outbound::PLAUD_CLI_PROGRAM);
    fs::write(&program, "#!/bin/sh\nexit 0\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    }
    let status = std::process::Command::new(&program).status()?;
    Ok(status.success())
}

/// Serialize a summary to the path in `PLAUDON_EGRESS_DECISIONS`, if set.
pub fn write_summary(summary: &ScenarioSummary) -> io::Result<Option<PathBuf>> {
    let Some(path) = std::env::var_os(DECISIONS_ENV) else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(summary).map_err(io::Error::other)?;
    fs::write(&path, json)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn package_name_matches_manifest() {
        assert_eq!(PACKAGE_NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn scenario_names_round_trip_and_are_unique() {
        let mut seen = Vec::new();
        for scenario in Scenario::ALL {
            assert_eq!(Scenario::parse(scenario.name()), Some(scenario));
            assert!(
                !seen.contains(&scenario.name()),
                "duplicate {}",
                scenario.name()
            );
            seen.push(scenario.name());
        }
        assert_eq!(Scenario::parse("not-a-scenario"), None);
    }

    #[test]
    fn every_hostile_local_only_case_names_a_subsystem() {
        for scenario in Scenario::ALL {
            let expected = !matches!(scenario, Scenario::LocalImport | Scenario::NegativeControl);
            assert_eq!(
                scenario.hostile_subsystem().is_some(),
                expected,
                "{}",
                scenario.name()
            );
        }
    }

    #[test]
    fn local_import_commits_a_file_and_records_no_egress() {
        let dir = std::env::temp_dir().join(format!("plaudon-egress-ut-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ctx = ScenarioContext {
            workdir: dir.clone(),
        };
        fs::create_dir_all(&dir).unwrap();

        let summary = run(Scenario::LocalImport, &ctx);
        assert!(summary.ok, "{}", summary.outcome);
        let artifact = summary.local_artifact.clone().unwrap();
        assert!(artifact.verified);
        assert!(Path::new(&artifact.committed_path).is_file());
        assert!(!Path::new(&artifact.staging_path).exists());
        assert_eq!(artifact.byte_length, 59);
        assert!(
            summary.decisions.is_empty(),
            "local import must not touch the gate"
        );
        assert_eq!(summary.mode, NetworkMode::LocalOnly);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gated_scenarios_are_denied_and_explained() {
        let dir = std::env::temp_dir().join(format!("plaudon-egress-ut2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ctx = ScenarioContext {
            workdir: dir.clone(),
        };
        fs::create_dir_all(&dir).unwrap();

        for scenario in Scenario::ALL {
            let Some(subsystem) = scenario.hostile_subsystem() else {
                continue;
            };
            let summary = run(scenario, &ctx);
            assert!(summary.ok, "{}: {}", scenario.name(), summary.outcome);
            assert!(
                summary.explains(subsystem),
                "{} must explain why {} is unavailable",
                scenario.name(),
                subsystem.label()
            );
            assert!(
                summary.decisions.iter().all(|d| !d.allowed),
                "{} must not produce an allowed decision",
                scenario.name()
            );
            assert_ne!(summary.not_guaranteed.len(), 0);
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn placeholder_detection_is_name_based() {
        assert!(is_cloud_placeholder(Path::new("/w/a.m4a.icloud")));
        assert!(is_cloud_placeholder(Path::new("/w/.a.m4a.icloud.tmp")));
        assert!(!is_cloud_placeholder(Path::new("/w/a.m4a")));
        assert!(!is_cloud_placeholder(Path::new("/w")));
    }
}
