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
//! One scenario, `voice-profile-match`, also repeats its attempts against a
//! second policy with the network permitted (VP-110, TM-044): a speaker
//! embedding is refused in both modes, so the summary shows a refusal that
//! does not depend on Local-only Mode.
//!
//! Exit status is 0 only when the scenario observed the outcome the spec
//! requires; anything else is a non-zero exit with the reason in the summary.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_security::consent::ContentType;
use plaudon_security::error::SecurityError;
use plaudon_security::fsguard::{canonicalize_and_validate, PathError, PathPolicy, PrivateTempDir};
use plaudon_security::outbound::http::GatedHttpClientFactory;
use plaudon_security::outbound::subprocess::{CliArg, GatedProcessLauncher, SpawnError};
use plaudon_security::outbound::{
    ApprovalSource, DenyReason, Destination, DisabledCapability, EffectiveNetworkPolicy,
    EgressDenied, EgressPurpose, EgressRecord, EgressRequest, NetworkGrant, NetworkMode,
    OutboundPolicy, PayloadClass, Subsystem, PLAUD_CLI_PROGRAM,
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
    /// VP-110 / TM-044: a voice-profile enrol-and-match flow, followed by
    /// deliberate attempts to push the speaker embedding out.
    VoiceProfileMatch,
    /// Deliberately bypasses the gate. Proves the fixture can fail.
    NegativeControl,
}

impl Scenario {
    pub const ALL: [Scenario; 11] = [
        Scenario::LocalImport,
        Scenario::OpenRouterDispatch,
        Scenario::UpdateCheck,
        Scenario::ModelRefresh,
        Scenario::ModelDownload,
        Scenario::TelemetryFlush,
        Scenario::AppleAssetFetch,
        Scenario::CloudCli,
        Scenario::FolderPlaceholder,
        Scenario::VoiceProfileMatch,
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
            Scenario::VoiceProfileMatch => "voice-profile-match",
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
            // The embedding is attempted at three seams; `OpenRouter` is the
            // one the summary must explain, and the one §18.6A lists as the
            // Cloud row.
            Scenario::VoiceProfileMatch => Some(Subsystem::OpenRouter),
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
        Scenario::VoiceProfileMatch => run_voice_profile_match(&policy, ctx, &mut summary),
        Scenario::NegativeControl => run_negative_control(ctx, &mut summary),
    }

    // Refresh the decision list: the calls above went through the gate, which
    // records every allow and every deny. A scenario that also drove a second
    // policy — a different network mode — has already pushed those decisions
    // into the summary, so they are kept, after this policy's own.
    let mut decisions = policy.observer().records();
    decisions.append(&mut summary.decisions);
    summary.decisions = decisions;
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
    match stage_and_commit(ctx, "import", payload) {
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
    stem: &str,
    payload: &[u8],
) -> Result<LocalArtifact, LocalImportError> {
    use std::io::Write;

    let staging_base = ctx.workdir.join("staging");
    let library_dir = ctx.workdir.join("library");
    fs::create_dir_all(&staging_base)?;
    fs::create_dir_all(&library_dir)?;

    let private = PrivateTempDir::create(Some(&staging_base), "plaudon-egress-import")?;
    let mut staged = private.stage_file(stem)?;
    staged.write_all(payload)?;
    let staging_path = staged.path().as_path().to_path_buf();

    let policy = PathPolicy::for_roots([library_dir.clone()]);
    let target = canonicalize_and_validate(&policy, &library_dir.join(format!("{stem}.bin")))?;
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

/// What the gate said about one HTTP attempt.
///
/// An enum rather than a `Result` because a ticket is not the success case
/// here: every scenario in this fixture requires the refusal.
enum HttpAttempt {
    /// The gate handed out a ticket for this destination label.
    Authorized(String),
    /// The gate refused, rendered with its §15.4 envelope.
    Refused(ExplainedDenial),
}

/// Drive one gated HTTP attempt through the same `GatedHttpClientFactory`
/// seam the production subsystems use.
fn gated_http_attempt(
    policy: &Arc<OutboundPolicy>,
    subsystem: Subsystem,
    purpose: EgressPurpose,
    payload: PayloadClass,
    url: &str,
    post: bool,
) -> HttpAttempt {
    let factory = GatedHttpClientFactory::new(Arc::clone(policy));
    let attempt = if post {
        factory.post(url, subsystem, purpose, payload)
    } else {
        factory.get(url, subsystem, purpose, payload)
    };

    match attempt {
        Ok(request) => HttpAttempt::Authorized(request.ticket().destination_label().to_owned()),
        Err(denied) => HttpAttempt::Refused(ExplainedDenial::new(&denied, purpose)),
    }
}

/// Attempt a real gated HTTP request and require that it is refused because
/// Local-only Mode is on.
fn run_http_case(
    policy: &Arc<OutboundPolicy>,
    summary: &mut ScenarioSummary,
    subsystem: Subsystem,
    purpose: EgressPurpose,
    payload: PayloadClass,
    url: &str,
    post: bool,
) {
    match gated_http_attempt(policy, subsystem, purpose, payload, url, post) {
        HttpAttempt::Authorized(destination_label) => {
            summary.ok = false;
            summary.outcome = format!(
                "POLICY FAILURE: gate authorized {} in Local-only Mode ({destination_label})",
                subsystem.label(),
            );
        }
        HttpAttempt::Refused(explained) => {
            summary.ok = explained.reason == DenyReason::LocalOnlyMode;
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

/// The seams a speaker embedding could plausibly be pushed out of: the
/// `OpenRouter` HTTPS dispatch, the telemetry/diagnostics upload and the PLAUD
/// cloud CLI subprocess.
const BIOMETRIC_SEAMS: usize = 3;

/// Dimension of the stand-in speaker embedding this scenario enrols.
const EMBEDDING_DIM: usize = 32;

/// Cosine score above which the probe counts as the enrolled speaker. Only the
/// local half of the scenario depends on it.
const MATCH_THRESHOLD: f32 = 0.6;

/// §16.2A + VOICE_PROFILES_SPEC VP-110 / TM-044: a speaker embedding never
/// leaves this machine, in any mode.
///
/// Two halves:
///
/// 1. an enrol-and-match flow runs end to end **locally** — it reaches no
///    egress seam, so the gate records nothing for it;
/// 2. the same embedding is then deliberately offered to three egress seams,
///    first under this scenario's Local-only policy and then under a second
///    policy with the network permitted by a deliberately permissive grant
///    (vendor hosts allowed, model downloads approved, telemetry opted in).
///
/// Every one of the six attempts must be refused with
/// `voice_biometric_never_egresses` — the one reason that does not depend on
/// the network mode — and the decisions of both policies are recorded.
fn run_voice_profile_match(
    policy: &Arc<OutboundPolicy>,
    ctx: &ScenarioContext,
    summary: &mut ScenarioSummary,
) {
    let enrolment = match enrol_and_match(ctx) {
        Ok(outcome) => outcome,
        Err(err) => {
            summary.ok = false;
            summary.outcome = format!("local voice-profile enrol and match failed: {err}");
            return;
        }
    };
    summary.notes.push(format!(
        "enrolled a speaker and matched a probe locally: probe scored {:.3} against the enrolled \
         profile and the impostor scored {:.3}; the flow reached no egress seam",
        enrolment.probe_score, enrolment.impostor_score
    ));
    // Sampled before the deliberate attempts below, so it measures the
    // enrol-and-match flow alone.
    let local_stage_decisions = policy.observer().records().len();
    let verified = enrolment.artifact.verified;
    summary.local_artifact = Some(enrolment.artifact);

    let local_only_reasons = attempt_biometric_egress(policy, summary, "local-only mode");

    let grant = NetworkGrant::new(ApprovalSource::TestHarness, 0, "egress-fixture-disclosure")
        .with_model_downloads_approved(true)
        .with_telemetry_opted_in(true)
        .with_allowed_hosts(vec![
            "openrouter.ai".to_owned(),
            "telemetry.example.invalid".to_owned(),
            "plaud.ai".to_owned(),
        ]);
    let permitted = Arc::new(OutboundPolicy::network_permitted(grant));
    let permitted_policy = permitted.describe();
    let permitted_reasons = attempt_biometric_egress(&permitted, summary, "network permitted");
    // `run` prepends this scenario's own Local-only decisions to these.
    summary.decisions.extend(permitted.observer().records());

    let local_only_policy = policy.describe();
    let mut problems = Vec::new();

    if local_stage_decisions != 0 {
        problems.push(format!(
            "the local enrol-and-match flow reached the egress gate {local_stage_decisions} time(s)"
        ));
    }
    if !verified {
        problems.push("the enrolled profile was not committed to local storage".to_owned());
    }
    if enrolment.probe_score <= MATCH_THRESHOLD || enrolment.impostor_score >= MATCH_THRESHOLD {
        problems.push(format!(
            "the local match did not separate the speakers: probe {:.3}, impostor {:.3}",
            enrolment.probe_score, enrolment.impostor_score
        ));
    }

    for (mode_label, reasons) in [
        ("local-only mode", &local_only_reasons),
        ("network permitted", &permitted_reasons),
    ] {
        if reasons.len() != BIOMETRIC_SEAMS {
            problems.push(format!(
                "{mode_label}: {} of {BIOMETRIC_SEAMS} seams refused the embedding",
                reasons.len()
            ));
        }
        for reason in reasons {
            if *reason != DenyReason::VoiceBiometricNeverEgresses {
                problems.push(format!(
                    "{mode_label}: expected voice_biometric_never_egresses, observed {}",
                    reason.label()
                ));
            }
        }
    }

    // Without this the refusal above could be a side effect of a grant that
    // permits nothing: under the same grant the gate reports the cloud CLI as
    // launchable and a non-empty host allowlist.
    if !permitted_policy.cloud_cli_launchable || permitted_policy.allowed_hosts.is_empty() {
        problems.push(
            "the permissive grant did not actually permit anything, so the refusal proves nothing"
                .to_owned(),
        );
    }
    for (mode_label, effective) in [
        ("local-only mode", &local_only_policy),
        ("network permitted", &permitted_policy),
    ] {
        if !effective.always_refused.iter().any(|entry| {
            entry.payload == PayloadClass::VoiceBiometric
                && entry.reason == DenyReason::VoiceBiometricNeverEgresses
                && !entry.explanation.trim().is_empty()
        }) {
            problems.push(format!(
                "{mode_label}: the policy snapshot does not list voice_biometric as always refused"
            ));
        }
    }

    // The other half of VP-110: a grant cannot name what has no content type.
    let consentable = ContentType::try_from(PayloadClass::VoiceBiometric).is_ok();
    if consentable {
        problems.push(
            "the payload class has a consent content type, so a consent grant could name it"
                .to_owned(),
        );
    }

    summary.notes.push(format!(
        "with the network permitted the same policy reports the cloud CLI as launchable and {} \
         allowed host(s), and still refuses the embedding",
        permitted_policy.allowed_hosts.len()
    ));
    summary.notes.push(
        "checked: ContentType::try_from(PayloadClass::VoiceBiometric) is an error, so no consent \
         grant can name this payload class"
            .to_owned(),
    );
    summary.notes.push(
        "what this shows: the gate refuses a voice_biometric payload at the OpenRouter HTTPS, \
         telemetry and cloud-CLI seams, in Local-only Mode and with the network permitted, and \
         the local enrol-and-match flow does not reach the gate at all. It does not show that a \
         future seam added outside plaudon_security::outbound would be covered, and it says \
         nothing about other processes on this machine"
            .to_owned(),
    );

    summary.ok = problems.is_empty();
    summary.outcome = if summary.ok {
        format!(
            "voice_biometric refused at all {BIOMETRIC_SEAMS} seams in Local-only Mode and all \
             {BIOMETRIC_SEAMS} with the network permitted: {}",
            DenyReason::VoiceBiometricNeverEgresses.label()
        )
    } else {
        let detail = problems.join("; ");
        summary.notes.extend(problems);
        format!("POLICY FAILURE: {detail}")
    };
}

/// Offer the speaker embedding to every egress seam under `policy` and record
/// what the gate said. Returns the reason given for each refusal, in order.
fn attempt_biometric_egress(
    policy: &Arc<OutboundPolicy>,
    summary: &mut ScenarioSummary,
    mode_label: &str,
) -> Vec<DenyReason> {
    let mut reasons = Vec::with_capacity(BIOMETRIC_SEAMS);

    let http_seams: [(Subsystem, EgressPurpose, &str); 2] = [
        (
            Subsystem::OpenRouter,
            EgressPurpose::InferenceDispatch,
            "https://openrouter.ai/api/v1/chat/completions",
        ),
        (
            Subsystem::Telemetry,
            EgressPurpose::TelemetryFlush,
            "https://telemetry.example.invalid/v1/events",
        ),
    ];
    for (subsystem, purpose, url) in http_seams {
        match gated_http_attempt(
            policy,
            subsystem,
            purpose,
            PayloadClass::VoiceBiometric,
            url,
            true,
        ) {
            HttpAttempt::Authorized(destination_label) => summary.notes.push(format!(
                "POLICY FAILURE ({mode_label}): the gate authorized {} to carry a voice_biometric \
                 payload to {destination_label}",
                subsystem.label()
            )),
            HttpAttempt::Refused(explained) => {
                reasons.push(explained.reason);
                summary.notes.push(format!(
                    "{mode_label}: {} {} refused — {}",
                    subsystem.label(),
                    purpose.label(),
                    explained.reason.label()
                ));
                summary.explained_denials.push(explained);
            }
        }
    }

    // The cloud CLI seam. `GatedProcessLauncher::prepare_cloud_cli` pins its
    // payload class to `metadata`, so an embedding cannot be handed to it
    // through that signature at all; what is driven here is the same
    // `OutboundPolicy::authorize` call the launcher makes, with the payload
    // class a caller would have to use to ship an embedding to the CLI.
    let request = EgressRequest::new(
        Subsystem::PlaudCloudCli,
        Destination::subprocess(PLAUD_CLI_PROGRAM),
        EgressPurpose::AccountSync,
        PayloadClass::VoiceBiometric,
    );
    match policy.authorize(&request) {
        Ok(ticket) => summary.notes.push(format!(
            "POLICY FAILURE ({mode_label}): the gate authorized the cloud CLI to carry a \
             voice_biometric payload to {}",
            ticket.destination_label()
        )),
        Err(denied) => {
            let explained = ExplainedDenial::new(&denied, EgressPurpose::AccountSync);
            reasons.push(explained.reason);
            summary.notes.push(format!(
                "{mode_label}: {} {} refused — {}",
                explained.subsystem.label(),
                EgressPurpose::AccountSync.label(),
                explained.reason.label()
            ));
            summary.explained_denials.push(explained);
        }
    }

    reasons
}

/// What the local half of the voice-profile scenario produced.
struct EnrolmentOutcome {
    artifact: LocalArtifact,
    probe_score: f32,
    impostor_score: f32,
}

/// Enrol one speaker and match a probe against them, entirely locally.
///
/// The vectors are derived from fixture bytes with SHA-256, not from audio: no
/// model runs here, and this scenario is about where a speaker embedding may
/// go, not about how well the real matcher scores. The enrolled vector is
/// committed through the same `fsguard` staging path the local import uses, so
/// the summary carries evidence that biometric bytes really were written —
/// locally, and nowhere else.
fn enrol_and_match(ctx: &ScenarioContext) -> Result<EnrolmentOutcome, LocalImportError> {
    let enrolled = embedding(b"plaudon voice profile fixture: speaker A enrolment");
    let session_noise = embedding(b"plaudon voice profile fixture: session noise");
    let probe = blend(&enrolled, &session_noise, 0.08);
    let impostor = embedding(b"plaudon voice profile fixture: speaker B");

    let probe_score = cosine(&enrolled, &probe);
    let impostor_score = cosine(&enrolled, &impostor);

    let mut bytes = Vec::with_capacity(EMBEDDING_DIM * 4);
    for value in enrolled {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let artifact = stage_and_commit(ctx, "voice-profile", &bytes)?;

    Ok(EnrolmentOutcome {
        artifact,
        probe_score,
        impostor_score,
    })
}

/// A deterministic unit vector derived from `seed`.
fn embedding(seed: &[u8]) -> [f32; EMBEDDING_DIM] {
    let mut values = [0.0f32; EMBEDDING_DIM];
    for (index, slot) in values.iter_mut().enumerate() {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        hasher.update((index as u32).to_le_bytes());
        let digest = hasher.finalize();
        let raw = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
        *slot = (f64::from(raw) / f64::from(u32::MAX)).mul_add(2.0, -1.0) as f32;
    }
    normalise(&mut values);
    values
}

/// `base` nudged toward `other`, as a within-speaker probe is nudged by the
/// session it was recorded in.
fn blend(
    base: &[f32; EMBEDDING_DIM],
    other: &[f32; EMBEDDING_DIM],
    weight: f32,
) -> [f32; EMBEDDING_DIM] {
    let mut values = [0.0f32; EMBEDDING_DIM];
    for (slot, (from, toward)) in values.iter_mut().zip(base.iter().zip(other.iter())) {
        *slot = from * (1.0 - weight) + toward * weight;
    }
    normalise(&mut values);
    values
}

fn normalise(values: &mut [f32; EMBEDDING_DIM]) {
    let norm = values
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if norm > f64::EPSILON {
        for value in values.iter_mut() {
            *value = (f64::from(*value) / norm) as f32;
        }
    }
}

/// Cosine similarity of two unit vectors.
fn cosine(left: &[f32; EMBEDDING_DIM], right: &[f32; EMBEDDING_DIM]) -> f32 {
    left.iter()
        .zip(right.iter())
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum::<f64>() as f32
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

    fn voice_profile_summary(tag: &str) -> (PathBuf, ScenarioSummary) {
        let dir = std::env::temp_dir().join(format!("plaudon-egress-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ctx = ScenarioContext {
            workdir: dir.clone(),
        };
        let summary = run(Scenario::VoiceProfileMatch, &ctx);
        (dir, summary)
    }

    #[test]
    fn a_voice_biometric_attempt_is_refused_in_both_modes() {
        let (dir, summary) = voice_profile_summary("ut-vp-modes");
        assert!(summary.ok, "{}", summary.outcome);
        assert_eq!(summary.mode, NetworkMode::LocalOnly);

        // Three seams under this scenario's Local-only policy, the same three
        // under a second policy with the network permitted.
        assert_eq!(summary.decisions.len(), 2 * BIOMETRIC_SEAMS);
        assert_eq!(summary.explained_denials.len(), 2 * BIOMETRIC_SEAMS);
        for seam in [
            "openrouter.ai",
            "telemetry.example.invalid",
            PLAUD_CLI_PROGRAM,
        ] {
            let hits = summary
                .decisions
                .iter()
                .filter(|d| d.destination_label.contains(seam))
                .count();
            assert_eq!(hits, 2, "{seam} must be attempted in both modes");
        }

        let note_count = |prefix: &str| {
            summary
                .notes
                .iter()
                .filter(|note| note.starts_with(prefix))
                .count()
        };
        assert_eq!(note_count("local-only mode: "), BIOMETRIC_SEAMS);
        assert_eq!(note_count("network permitted: "), BIOMETRIC_SEAMS);
        assert!(summary.explains(Subsystem::OpenRouter));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_voice_biometric_refusal_names_its_own_reason_and_allows_nothing() {
        let (dir, summary) = voice_profile_summary("ut-vp-reason");
        assert!(summary.ok, "{}", summary.outcome);

        for decision in &summary.decisions {
            assert!(!decision.allowed, "{decision:?}");
            assert_eq!(decision.payload, PayloadClass::VoiceBiometric);
            assert_eq!(
                decision.deny_reason,
                Some(DenyReason::VoiceBiometricNeverEgresses),
                "{decision:?}"
            );
        }
        for denial in &summary.explained_denials {
            assert_eq!(denial.reason, DenyReason::VoiceBiometricNeverEgresses);
            assert_eq!(
                denial.message,
                DenyReason::VoiceBiometricNeverEgresses.explanation()
            );
            assert!(!denial.retryable);
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn enrol_and_match_separates_the_speakers_and_writes_only_locally() {
        let dir =
            std::env::temp_dir().join(format!("plaudon-egress-ut-vp-local-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ctx = ScenarioContext {
            workdir: dir.clone(),
        };

        let outcome = enrol_and_match(&ctx).unwrap();
        assert!(outcome.artifact.verified);
        assert_eq!(outcome.artifact.byte_length, (EMBEDDING_DIM * 4) as u64);
        let committed = Path::new(&outcome.artifact.committed_path);
        assert!(committed.is_file());
        assert_eq!(
            committed.file_name().and_then(|name| name.to_str()),
            Some("voice-profile.bin")
        );
        // The library path is canonical, so compare against the canonical dir.
        assert!(committed.starts_with(fs::canonicalize(&dir).unwrap()));
        assert!(!Path::new(&outcome.artifact.staging_path).exists());
        assert!(
            outcome.probe_score > MATCH_THRESHOLD,
            "probe scored {}",
            outcome.probe_score
        );
        assert!(
            outcome.impostor_score < MATCH_THRESHOLD,
            "impostor scored {}",
            outcome.impostor_score
        );

        fs::remove_dir_all(&dir).unwrap();
    }
}
