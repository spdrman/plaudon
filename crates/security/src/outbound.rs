//! D1.1 — the single Rust-owned outbound egress gate (spec §16.2A, §1.2(15), App. B.7).
//!
//! Every network-capable subsystem — the PLAUD Cloud CLI integration, the
//! OpenRouter HTTP client, the model manager, the update checker, telemetry and
//! any future local helper — must obtain an [`EgressTicket`] from
//! [`OutboundPolicy::authorize`] before it initiates anything. The ticket has a
//! private constructor, so no code outside this module can fabricate one, and
//! both the HTTP transport trait ([`http::HttpTransport`]) and the subprocess
//! launcher ([`subprocess::GatedCommand`]) demand one.
//!
//! The default policy is [`NetworkMode::LocalOnly`]: network initiation is
//! refused. Leaving that state requires an explicit, recorded [`NetworkGrant`].
//!
//! # Honest scope
//!
//! This gate governs **app-initiated** traffic from this process. It makes no
//! claim about unrelated OS, browser, platform-store or user-selected
//! cloud-synced-folder traffic; see [`EffectiveNetworkPolicy::not_guaranteed`].

use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::error::{
    ErrorEnvelopeParts, SecurityError, APPROVE_MODEL_DOWNLOAD, DISABLE_LOCAL_ONLY_MODE,
    EGRESS_DENIED, OPEN_CLOUD_CONSENT,
};

/// Whether the application is permitted to initiate outbound network traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// Local-only Mode (§1.2(15)). Every app-controlled internet operation is
    /// refused at this boundary. Bluetooth and local disk access are unaffected.
    LocalOnly,
    /// Network initiation is permitted, still subject to per-request checks.
    NetworkPermitted,
}

impl NetworkMode {
    #[must_use]
    pub const fn is_local_only(self) -> bool {
        matches!(self, NetworkMode::LocalOnly)
    }
}

impl Default for NetworkMode {
    /// Deny by default (§16.2A: "The default local-only policy refuses network
    /// initiation").
    fn default() -> Self {
        NetworkMode::LocalOnly
    }
}

/// The internal subsystem asking to initiate egress. Used for attribution in
/// the audit trail and in the D2.3 egress evidence artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subsystem {
    /// The official PLAUD Cloud CLI, launched as a separate executable.
    PlaudCloudCli,
    /// Our OpenRouter REST client.
    OpenRouter,
    /// Model manifest/catalogue/weight distribution.
    ModelDistribution,
    /// Application update checker.
    UpdateChecker,
    /// Opt-in telemetry.
    Telemetry,
    /// Apple on-device model/asset download requested through the bridge.
    AppleAssetFetch,
    /// Probing whether a user-selected watched folder is backed by a cloud sync
    /// client (§16.2A: such folders are disabled or labelled, never silently used).
    FolderSyncProbe,
    /// Any other subsystem. The label is a `&'static str` so it cannot carry
    /// user data into logs.
    Other(&'static str),
}

impl Subsystem {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Subsystem::PlaudCloudCli => "plaud_cloud_cli",
            Subsystem::OpenRouter => "openrouter",
            Subsystem::ModelDistribution => "model_distribution",
            Subsystem::UpdateChecker => "update_checker",
            Subsystem::Telemetry => "telemetry",
            Subsystem::AppleAssetFetch => "apple_asset_fetch",
            Subsystem::FolderSyncProbe => "folder_sync_probe",
            Subsystem::Other(label) => label,
        }
    }

    /// Every variant, for exhaustive decision-table tests.
    pub const ALL: [Subsystem; 8] = [
        Subsystem::PlaudCloudCli,
        Subsystem::OpenRouter,
        Subsystem::ModelDistribution,
        Subsystem::UpdateChecker,
        Subsystem::Telemetry,
        Subsystem::AppleAssetFetch,
        Subsystem::FolderSyncProbe,
        Subsystem::Other("test_subsystem"),
    ];
}

impl fmt::Display for Subsystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl serde::Serialize for Subsystem {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

impl<'de> serde::Deserialize<'de> for Subsystem {
    /// Fails closed on an unrecognised label rather than inventing an
    /// `Other` variant with a leaked string.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Subsystem::ALL
            .into_iter()
            .find(|s| !matches!(s, Subsystem::Other(_)) && s.label() == raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown subsystem label: {raw}")))
    }
}

/// Why the subsystem wants to reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressPurpose {
    CredentialValidation,
    ModelCatalog,
    ModelDownload,
    ManifestFetch,
    InferenceDispatch,
    AccountSync,
    UpdateCheck,
    TelemetryFlush,
    AssetDownload,
    Diagnostic,
}

impl EgressPurpose {
    pub const ALL: [EgressPurpose; 10] = [
        EgressPurpose::CredentialValidation,
        EgressPurpose::ModelCatalog,
        EgressPurpose::ModelDownload,
        EgressPurpose::ManifestFetch,
        EgressPurpose::InferenceDispatch,
        EgressPurpose::AccountSync,
        EgressPurpose::UpdateCheck,
        EgressPurpose::TelemetryFlush,
        EgressPurpose::AssetDownload,
        EgressPurpose::Diagnostic,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            EgressPurpose::CredentialValidation => "credential_validation",
            EgressPurpose::ModelCatalog => "model_catalog",
            EgressPurpose::ModelDownload => "model_download",
            EgressPurpose::ManifestFetch => "manifest_fetch",
            EgressPurpose::InferenceDispatch => "inference_dispatch",
            EgressPurpose::AccountSync => "account_sync",
            EgressPurpose::UpdateCheck => "update_check",
            EgressPurpose::TelemetryFlush => "telemetry_flush",
            EgressPurpose::AssetDownload => "asset_download",
            EgressPurpose::Diagnostic => "diagnostic",
        }
    }
}

/// What kind of user data the request body would carry. Drives the §16.3
/// consent-scope check and the privacy labelling shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadClass {
    /// No user-derived content at all (e.g. a version probe).
    None,
    /// Non-content metadata: model IDs, counts, durations.
    Metadata,
    /// Transcript text derived from the user's recording.
    TranscriptText,
    /// Raw or transcoded audio bytes.
    AudioBytes,
    /// A credential being validated against the provider.
    Credential,
}

impl PayloadClass {
    pub const ALL: [PayloadClass; 5] = [
        PayloadClass::None,
        PayloadClass::Metadata,
        PayloadClass::TranscriptText,
        PayloadClass::AudioBytes,
        PayloadClass::Credential,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PayloadClass::None => "none",
            PayloadClass::Metadata => "metadata",
            PayloadClass::TranscriptText => "transcript_text",
            PayloadClass::AudioBytes => "audio_bytes",
            PayloadClass::Credential => "credential",
        }
    }

    /// True when the payload contains content derived from the user's recording.
    #[must_use]
    pub const fn is_user_content(self) -> bool {
        matches!(
            self,
            PayloadClass::TranscriptText | PayloadClass::AudioBytes
        )
    }
}

/// Where the request would go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// An outbound network endpoint.
    Network {
        scheme: String,
        host: String,
        port: u16,
    },
    /// A child process that may perform its own networking outside our HTTP
    /// client (§16.2A: the cloud CLI is a separate executable).
    Subprocess { program: String },
}

impl Destination {
    /// Parse and validate an absolute URL into a network destination.
    ///
    /// Rejects, before any DNS lookup: non-`https` schemes, embedded
    /// credentials (`user:pass@`), IP-literal hosts, loopback/private/
    /// link-local/unique-local hostnames, and empty hosts (§7.3, §16.2 SSRF row).
    pub fn url(raw: &str) -> Result<Destination, EgressDenied> {
        let parsed = url::Url::parse(raw).map_err(|_| EgressDenied {
            reason: DenyReason::MalformedDestination,
            subsystem: Subsystem::Other("destination_parser"),
            destination_label: "<unparseable-url>".to_owned(),
            user_action: None,
        })?;

        let label = redacted_destination_label(&parsed);

        if parsed.scheme() != "https" {
            return Err(EgressDenied {
                reason: DenyReason::SchemeNotHttps,
                subsystem: Subsystem::Other("destination_parser"),
                destination_label: label,
                user_action: None,
            });
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(EgressDenied {
                reason: DenyReason::CredentialInUrl,
                subsystem: Subsystem::Other("destination_parser"),
                destination_label: label,
                user_action: None,
            });
        }
        let host = match parsed.host() {
            Some(url::Host::Domain(d)) if !d.is_empty() => d.to_ascii_lowercase(),
            Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => {
                return Err(EgressDenied {
                    reason: DenyReason::PrivateOrLoopbackDestination,
                    subsystem: Subsystem::Other("destination_parser"),
                    destination_label: label,
                    user_action: None,
                })
            }
            _ => {
                return Err(EgressDenied {
                    reason: DenyReason::MalformedDestination,
                    subsystem: Subsystem::Other("destination_parser"),
                    destination_label: label,
                    user_action: None,
                })
            }
        };
        if is_local_hostname(&host) {
            return Err(EgressDenied {
                reason: DenyReason::PrivateOrLoopbackDestination,
                subsystem: Subsystem::Other("destination_parser"),
                destination_label: label,
                user_action: None,
            });
        }

        Ok(Destination::Network {
            scheme: parsed.scheme().to_owned(),
            host,
            port: parsed.port_or_known_default().unwrap_or(443),
        })
    }

    /// A subprocess destination for a program that may itself reach the network.
    #[must_use]
    pub fn subprocess(program: impl Into<String>) -> Destination {
        Destination::Subprocess {
            program: program.into(),
        }
    }

    /// A label safe to place in logs and error envelopes: scheme + host + port,
    /// never a path, query string or credential (§15.4, §16.2 "Signed URL leak").
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Destination::Network { scheme, host, port } => format!("{scheme}://{host}:{port}"),
            Destination::Subprocess { program } => format!("subprocess:{program}"),
        }
    }

    #[must_use]
    pub fn host(&self) -> Option<&str> {
        match self {
            Destination::Network { host, .. } => Some(host),
            Destination::Subprocess { .. } => None,
        }
    }
}

fn redacted_destination_label(parsed: &url::Url) -> String {
    match parsed.host_str() {
        Some(host) => format!("{}://{}", parsed.scheme(), host.to_ascii_lowercase()),
        None => format!("{}://<no-host>", parsed.scheme()),
    }
}

/// Hostnames that resolve to this machine or a private network and therefore
/// must never be treated as a legitimate vendor endpoint.
// `.local` here is an mDNS/private-network hostname suffix, not a file
// extension; the caller already lower-cases `host`, so this is correct.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_local_hostname(host: &str) -> bool {
    host == "localhost"
        || host == "localhost."
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
        || host.ends_with(".in-addr.arpa")
        || host.ends_with(".ip6.arpa")
}

/// A fully described egress attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRequest {
    pub subsystem: Subsystem,
    pub destination: Destination,
    pub purpose: EgressPurpose,
    pub payload: PayloadClass,
}

impl EgressRequest {
    #[must_use]
    pub fn new(
        subsystem: Subsystem,
        destination: Destination,
        purpose: EgressPurpose,
        payload: PayloadClass,
    ) -> Self {
        Self {
            subsystem,
            destination,
            purpose,
            payload,
        }
    }
}

/// Proof that [`OutboundPolicy::authorize`] allowed a specific request.
///
/// The only constructor is private to this module, so an HTTP transport or a
/// subprocess launcher that requires a ticket cannot be driven without passing
/// the policy. `Clone` is deliberately not implemented: a ticket authorizes one
/// attempt.
#[derive(Debug)]
#[non_exhaustive]
pub struct EgressTicket {
    destination_label: String,
    subsystem: Subsystem,
    purpose: EgressPurpose,
    payload: PayloadClass,
    /// Unforgeable outside this module.
    _seal: Seal,
}

#[derive(Debug)]
struct Seal;

impl EgressTicket {
    #[must_use]
    pub fn destination_label(&self) -> &str {
        &self.destination_label
    }
    #[must_use]
    pub fn subsystem(&self) -> Subsystem {
        self.subsystem
    }
    #[must_use]
    pub fn purpose(&self) -> EgressPurpose {
        self.purpose
    }
    #[must_use]
    pub fn payload(&self) -> PayloadClass {
        self.payload
    }
}

/// Why an egress attempt was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// Local-only Mode is on (§1.2(15)).
    LocalOnlyMode,
    /// The host is not on the pinned vendor allowlist (§7.3).
    DestinationNotAllowlisted,
    /// Only `https` is permitted.
    SchemeNotHttps,
    /// Loopback, private, link-local or IP-literal destination.
    PrivateOrLoopbackDestination,
    /// Credentials embedded in the URL.
    CredentialInUrl,
    /// The URL could not be parsed.
    MalformedDestination,
    /// This subsystem is never allowed to use the network in this build.
    SubsystemNotNetworkCapable,
    /// The user has not approved model downloads (§2.1(4)).
    ModelDownloadNotApproved,
    /// Telemetry is opt-in and has not been enabled (§16.2 "Excessive telemetry").
    TelemetryNotOptedIn,
    /// The subsystem/purpose combination is nonsensical and is refused rather
    /// than guessed.
    PurposeNotValidForSubsystem,
}

impl DenyReason {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            DenyReason::LocalOnlyMode => "local_only_mode",
            DenyReason::DestinationNotAllowlisted => "destination_not_allowlisted",
            DenyReason::SchemeNotHttps => "scheme_not_https",
            DenyReason::PrivateOrLoopbackDestination => "private_or_loopback_destination",
            DenyReason::CredentialInUrl => "credential_in_url",
            DenyReason::MalformedDestination => "malformed_destination",
            DenyReason::SubsystemNotNetworkCapable => "subsystem_not_network_capable",
            DenyReason::ModelDownloadNotApproved => "model_download_not_approved",
            DenyReason::TelemetryNotOptedIn => "telemetry_not_opted_in",
            DenyReason::PurposeNotValidForSubsystem => "purpose_not_valid_for_subsystem",
        }
    }

    /// Actionable, user-facing explanation. Fixed vocabulary only — never
    /// interpolates a URL, credential or user content (§15.4).
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            DenyReason::LocalOnlyMode => {
                "Local-only Mode is on, so the app refused to start a network connection. \
                 Bluetooth and local files still work."
            }
            DenyReason::DestinationNotAllowlisted => {
                "The destination is not on the verified vendor host list, so the request was refused."
            }
            DenyReason::SchemeNotHttps => "Only https destinations are allowed.",
            DenyReason::PrivateOrLoopbackDestination => {
                "The destination resolves to this machine or a private network and was refused."
            }
            DenyReason::CredentialInUrl => {
                "The destination embedded credentials in the address and was refused."
            }
            DenyReason::MalformedDestination => "The destination address could not be understood.",
            DenyReason::SubsystemNotNetworkCapable => {
                "This part of the app is not allowed to use the network."
            }
            DenyReason::ModelDownloadNotApproved => {
                "Model downloads need your explicit approval before any bytes are fetched."
            }
            DenyReason::TelemetryNotOptedIn => {
                "Diagnostics sharing is off, so nothing was sent."
            }
            DenyReason::PurposeNotValidForSubsystem => {
                "The request did not match an operation this part of the app may perform."
            }
        }
    }
}

/// A refused egress attempt, carrying only redacted labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressDenied {
    pub reason: DenyReason,
    pub subsystem: Subsystem,
    /// `scheme://host:port` or `subprocess:<program>`. Never a path or query.
    pub destination_label: String,
    pub user_action: Option<crate::error::ErrorAction>,
}

impl fmt::Display for EgressDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "egress denied ({}) for {} -> {}",
            self.reason.label(),
            self.subsystem.label(),
            self.destination_label
        )
    }
}

impl std::error::Error for EgressDenied {}

impl SecurityError for EgressDenied {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        let parts =
            ErrorEnvelopeParts::new(EGRESS_DENIED, self.reason.explanation()).retryable(false);
        match self.user_action {
            Some(action) => parts.action(action),
            None => parts,
        }
    }
}

/// The outcome of a policy evaluation, recorded for the audit trail whether it
/// allowed or denied.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EgressRecord {
    pub subsystem: Subsystem,
    pub destination_label: String,
    pub purpose: EgressPurpose,
    pub payload: PayloadClass,
    pub allowed: bool,
    pub deny_reason: Option<DenyReason>,
}

/// Sink for every policy decision. The D2.3 harness and the UI "why is this
/// disabled" surface both read from it.
pub trait EgressObserver: Send + Sync + fmt::Debug {
    fn record(&self, record: EgressRecord);
    /// Everything recorded so far, oldest first.
    fn records(&self) -> Vec<EgressRecord>;
}

/// Default observer: an in-memory bounded ring.
#[derive(Debug)]
pub struct RecordingObserver {
    inner: Mutex<Vec<EgressRecord>>,
    capacity: usize,
}

impl RecordingObserver {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            capacity: capacity.max(1),
        }
    }
}

impl Default for RecordingObserver {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl EgressObserver for RecordingObserver {
    fn record(&self, record: EgressRecord) {
        let mut guard = self.inner.lock();
        if guard.len() == self.capacity {
            guard.remove(0);
        }
        guard.push(record);
    }

    fn records(&self) -> Vec<EgressRecord> {
        self.inner.lock().clone()
    }
}

/// How the user approved leaving Local-only Mode. Stored so the decision is
/// auditable rather than implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    /// The user toggled Local-only Mode off in settings with confirmation.
    UserSettingsConfirmation,
    /// First-run onboarding where the user chose a cloud-capable configuration.
    OnboardingChoice,
    /// A test harness. Never produced by production code paths.
    TestHarness,
}

/// The explicit, recorded decision that permits network initiation.
///
/// Constructing one is the *only* way to leave Local-only Mode, and every field
/// is required, so "enabling egress is an explicit, recorded decision" is a type
/// obligation rather than a convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkGrant {
    /// Who approved it.
    pub approved_by: ApprovalSource,
    /// Unix epoch seconds at approval time.
    pub approved_at_unix: i64,
    /// Version of the privacy disclosure that was shown (§4.4(5)).
    pub disclosure_version: String,
    /// Model weights may only be fetched when the user explicitly approved a
    /// download (§2.1(4), §2.1(13)).
    pub model_downloads_approved: bool,
    /// Telemetry is opt-in (§16.2).
    pub telemetry_opted_in: bool,
    /// Pinned vendor hosts. An empty list means "no host is reachable".
    pub allowed_hosts: Vec<String>,
}

impl NetworkGrant {
    /// A grant covering the V1 vendor hosts with downloads and telemetry off.
    #[must_use]
    pub fn new(
        approved_by: ApprovalSource,
        approved_at_unix: i64,
        disclosure_version: impl Into<String>,
    ) -> Self {
        Self {
            approved_by,
            approved_at_unix,
            disclosure_version: disclosure_version.into(),
            model_downloads_approved: false,
            telemetry_opted_in: false,
            allowed_hosts: default_allowed_hosts(),
        }
    }

    #[must_use]
    pub fn with_model_downloads_approved(mut self, approved: bool) -> Self {
        self.model_downloads_approved = approved;
        self
    }

    #[must_use]
    pub fn with_telemetry_opted_in(mut self, opted_in: bool) -> Self {
        self.telemetry_opted_in = opted_in;
        self
    }

    #[must_use]
    pub fn with_allowed_hosts(mut self, hosts: Vec<String>) -> Self {
        self.allowed_hosts = hosts.into_iter().map(|h| h.to_ascii_lowercase()).collect();
        self
    }

    fn host_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.allowed_hosts
            .iter()
            .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
    }
}

/// Pinned V1 vendor hosts (§7.3 host allowlist, §10.1 OpenRouter).
#[must_use]
pub fn default_allowed_hosts() -> Vec<String> {
    vec![
        "openrouter.ai".to_owned(),
        "plaud.ai".to_owned(),
        "support.plaud.ai".to_owned(),
    ]
}

/// A snapshot of the enforced policy, suitable for returning from the
/// `set_local_only_mode` command and for rendering "why is this disabled".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectiveNetworkPolicy {
    pub mode: NetworkMode,
    /// Capabilities refused under the current mode, with their explanations.
    pub disabled_capabilities: Vec<DisabledCapability>,
    /// Hosts reachable at all under the current mode.
    pub allowed_hosts: Vec<String>,
    /// Whether the cloud CLI subprocess may be launched at all (§16.2A).
    pub cloud_cli_launchable: bool,
    /// Explicit statement of what this boundary does **not** guarantee
    /// (§16.2A: no unverifiable promises about unrelated processes).
    pub not_guaranteed: Vec<String>,
}

/// A capability the UI must show as unavailable, with an honest reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DisabledCapability {
    pub subsystem: Subsystem,
    pub purpose: EgressPurpose,
    pub reason: DenyReason,
    pub explanation: String,
}

/// Statements about traffic this gate cannot and does not control.
pub const NOT_GUARANTEED: [&str; 5] = [
    "Operating-system update agents, platform stores and OS telemetry are outside this application's egress control.",
    "Other applications and browser activity on this machine are outside this application's egress control.",
    "A user-selected folder backed by a cloud-sync client may cause that client to transfer data; such folders are disabled or labelled while Local-only Mode is on.",
    "Legacy PLAUD BLE sessions may be unencrypted on the radio link; disabling internet access does not add radio-layer confidentiality.",
    "In-process memory cannot be perfectly protected from privileged malware on the same machine.",
];

/// The single egress gate.
#[derive(Debug)]
pub struct OutboundPolicy {
    mode: NetworkMode,
    grant: Option<NetworkGrant>,
    observer: Arc<dyn EgressObserver>,
}

impl Default for OutboundPolicy {
    fn default() -> Self {
        Self::local_only()
    }
}

impl OutboundPolicy {
    /// Deny-by-default policy. This is what the application constructs at
    /// startup before any user setting is loaded.
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            mode: NetworkMode::LocalOnly,
            grant: None,
            observer: Arc::new(RecordingObserver::default()),
        }
    }

    /// Permit network initiation under an explicit, recorded grant.
    #[must_use]
    pub fn network_permitted(grant: NetworkGrant) -> Self {
        Self {
            mode: NetworkMode::NetworkPermitted,
            grant: Some(grant),
            observer: Arc::new(RecordingObserver::default()),
        }
    }

    /// Replace the audit sink (used by the D2.3 harness and by diagnostics).
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn EgressObserver>) -> Self {
        self.observer = observer;
        self
    }

    #[must_use]
    pub fn mode(&self) -> NetworkMode {
        self.mode
    }

    #[must_use]
    pub fn grant(&self) -> Option<&NetworkGrant> {
        self.grant.as_ref()
    }

    #[must_use]
    pub fn observer(&self) -> &dyn EgressObserver {
        self.observer.as_ref()
    }

    /// Authorize one egress attempt.
    ///
    /// Every decision — allow or deny — is written to the observer before it is
    /// returned, so a subsystem cannot make an unlogged attempt through the gate.
    pub fn authorize(&self, request: &EgressRequest) -> Result<EgressTicket, EgressDenied> {
        let decision = self.decide(request);
        let destination_label = request.destination.label();
        self.observer.record(EgressRecord {
            subsystem: request.subsystem,
            destination_label: destination_label.clone(),
            purpose: request.purpose,
            payload: request.payload,
            allowed: decision.is_ok(),
            deny_reason: decision.as_ref().err().copied(),
        });

        match decision {
            Ok(()) => Ok(EgressTicket {
                destination_label,
                subsystem: request.subsystem,
                purpose: request.purpose,
                payload: request.payload,
                _seal: Seal,
            }),
            Err(reason) => Err(EgressDenied {
                reason,
                subsystem: request.subsystem,
                destination_label,
                user_action: user_action_for(reason),
            }),
        }
    }

    /// Pure decision function, no logging. Kept separate so the decision table
    /// can be exhaustively tested.
    fn decide(&self, request: &EgressRequest) -> Result<(), DenyReason> {
        // 1. Local-only Mode refuses unconditionally. This check is first so no
        //    later rule can accidentally permit traffic (§1.2(15), §6.3(15)).
        if self.mode.is_local_only() {
            return Err(DenyReason::LocalOnlyMode);
        }

        let Some(grant) = self.grant.as_ref() else {
            // NetworkPermitted without a grant is unreachable through the public
            // constructors; refuse rather than assume.
            return Err(DenyReason::LocalOnlyMode);
        };

        // 2. Purpose must make sense for the subsystem. Unknown combinations
        //    fail closed rather than being guessed (§2.5B).
        if !purpose_valid_for(request.subsystem, request.purpose) {
            return Err(DenyReason::PurposeNotValidForSubsystem);
        }

        // 3. Opt-in gates that apply even when the network is permitted.
        if request.purpose == EgressPurpose::ModelDownload && !grant.model_downloads_approved {
            return Err(DenyReason::ModelDownloadNotApproved);
        }
        if request.subsystem == Subsystem::Telemetry && !grant.telemetry_opted_in {
            return Err(DenyReason::TelemetryNotOptedIn);
        }

        // 4. Destination checks.
        match &request.destination {
            Destination::Network {
                scheme,
                host,
                port: _,
            } => {
                if scheme != "https" {
                    return Err(DenyReason::SchemeNotHttps);
                }
                if is_local_hostname(host) || host.parse::<std::net::IpAddr>().is_ok() {
                    return Err(DenyReason::PrivateOrLoopbackDestination);
                }
                if !grant.host_allowed(host) {
                    return Err(DenyReason::DestinationNotAllowlisted);
                }
                Ok(())
            }
            Destination::Subprocess { program } => {
                // Only the cloud CLI subsystem may launch a network-capable
                // subprocess, and only with the pinned program name.
                if request.subsystem != Subsystem::PlaudCloudCli {
                    return Err(DenyReason::SubsystemNotNetworkCapable);
                }
                if !is_allowlisted_cli_program(program) {
                    return Err(DenyReason::DestinationNotAllowlisted);
                }
                Ok(())
            }
        }
    }

    /// Snapshot for the UI and for the `set_local_only_mode` command response.
    #[must_use]
    pub fn describe(&self) -> EffectiveNetworkPolicy {
        let probes: [(Subsystem, EgressPurpose, PayloadClass); 7] = [
            (
                Subsystem::PlaudCloudCli,
                EgressPurpose::AccountSync,
                PayloadClass::Metadata,
            ),
            (
                Subsystem::OpenRouter,
                EgressPurpose::InferenceDispatch,
                PayloadClass::AudioBytes,
            ),
            (
                Subsystem::OpenRouter,
                EgressPurpose::ModelCatalog,
                PayloadClass::None,
            ),
            (
                Subsystem::ModelDistribution,
                EgressPurpose::ModelDownload,
                PayloadClass::None,
            ),
            (
                Subsystem::UpdateChecker,
                EgressPurpose::UpdateCheck,
                PayloadClass::None,
            ),
            (
                Subsystem::Telemetry,
                EgressPurpose::TelemetryFlush,
                PayloadClass::Metadata,
            ),
            (
                Subsystem::AppleAssetFetch,
                EgressPurpose::AssetDownload,
                PayloadClass::None,
            ),
        ];

        let mut disabled = Vec::new();
        for (subsystem, purpose, payload) in probes {
            let destination = if subsystem == Subsystem::PlaudCloudCli {
                Destination::subprocess(PLAUD_CLI_PROGRAM)
            } else {
                Destination::Network {
                    scheme: "https".to_owned(),
                    host: representative_host(subsystem).to_owned(),
                    port: 443,
                }
            };
            let request = EgressRequest::new(subsystem, destination, purpose, payload);
            if let Err(reason) = self.decide(&request) {
                disabled.push(DisabledCapability {
                    subsystem,
                    purpose,
                    reason,
                    explanation: reason.explanation().to_owned(),
                });
            }
        }

        let cloud_cli_launchable = self
            .decide(&EgressRequest::new(
                Subsystem::PlaudCloudCli,
                Destination::subprocess(PLAUD_CLI_PROGRAM),
                EgressPurpose::AccountSync,
                PayloadClass::Metadata,
            ))
            .is_ok();

        EffectiveNetworkPolicy {
            mode: self.mode,
            disabled_capabilities: disabled,
            allowed_hosts: match (self.mode, self.grant.as_ref()) {
                (NetworkMode::LocalOnly, _) | (_, None) => Vec::new(),
                (NetworkMode::NetworkPermitted, Some(grant)) => grant.allowed_hosts.clone(),
            },
            cloud_cli_launchable,
            not_guaranteed: NOT_GUARANTEED.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
}

fn representative_host(subsystem: Subsystem) -> &'static str {
    match subsystem {
        Subsystem::OpenRouter => "openrouter.ai",
        Subsystem::PlaudCloudCli => "plaud.ai",
        Subsystem::ModelDistribution => "models.example.invalid",
        Subsystem::UpdateChecker => "updates.example.invalid",
        Subsystem::Telemetry => "telemetry.example.invalid",
        Subsystem::AppleAssetFetch => "apple.example.invalid",
        Subsystem::FolderSyncProbe => "folder.example.invalid",
        Subsystem::Other(_) => "other.example.invalid",
    }
}

/// The pinned cloud-CLI program name (§7.1: hardcoded executable name selected
/// during discovery, never an arbitrary user-supplied path).
pub const PLAUD_CLI_PROGRAM: &str = "plaud";

fn is_allowlisted_cli_program(program: &str) -> bool {
    // Compare the file name only; a discovered absolute path is acceptable but
    // the leaf must be the pinned binary name.
    let leaf = std::path::Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    let leaf = leaf.strip_suffix(".exe").unwrap_or(leaf);
    leaf == PLAUD_CLI_PROGRAM
}

fn purpose_valid_for(subsystem: Subsystem, purpose: EgressPurpose) -> bool {
    use EgressPurpose::{
        AccountSync, AssetDownload, CredentialValidation, Diagnostic, InferenceDispatch,
        ManifestFetch, ModelCatalog, ModelDownload, TelemetryFlush, UpdateCheck,
    };
    match subsystem {
        Subsystem::PlaudCloudCli => matches!(purpose, AccountSync | Diagnostic),
        Subsystem::OpenRouter => matches!(
            purpose,
            CredentialValidation | ModelCatalog | InferenceDispatch | Diagnostic
        ),
        Subsystem::ModelDistribution => {
            matches!(purpose, ModelCatalog | ManifestFetch | ModelDownload)
        }
        Subsystem::UpdateChecker => matches!(purpose, UpdateCheck | ManifestFetch),
        Subsystem::Telemetry => matches!(purpose, TelemetryFlush),
        Subsystem::AppleAssetFetch => matches!(purpose, AssetDownload),
        // Exhaustive, individually-listed arms are intentional: adding a new
        // `Subsystem` variant must force a compile error here rather than
        // silently falling through a merged `_ => false` arm.
        #[allow(clippy::match_same_arms)]
        Subsystem::FolderSyncProbe => false,
        Subsystem::Other(_) => false,
    }
}

fn user_action_for(reason: DenyReason) -> Option<crate::error::ErrorAction> {
    match reason {
        DenyReason::LocalOnlyMode => Some(DISABLE_LOCAL_ONLY_MODE),
        DenyReason::ModelDownloadNotApproved => Some(APPROVE_MODEL_DOWNLOAD),
        DenyReason::TelemetryNotOptedIn => Some(OPEN_CLOUD_CONSENT),
        _ => None,
    }
}

pub mod http;
pub mod subprocess;

#[cfg(test)]
mod tests;
