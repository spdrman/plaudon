//! The axes a consent grant is scoped along (§16.3): purpose, content type,
//! model/provider route, retention posture, and the §4.4 user choice.

use std::collections::BTreeSet;
use std::fmt;

use crate::error::{ErrorEnvelopeParts, SecurityError, CONSENT_SCOPE_MISMATCH};
use crate::outbound::{DenyReason, Destination, EgressDenied, PayloadClass, Subsystem};

/// Why user content would leave the device. The *purpose* half of a grant.
///
/// A grant for one purpose never satisfies another: summarizing a transcript is
/// not the same decision as uploading audio for transcription (§16.3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ConsentPurpose {
    /// Speech-to-text on audio the user recorded or imported.
    Transcription,
    /// Summary/extraction over an existing transcript.
    Summarization,
    /// A transcript tool or ask-about-recording conversation (§9.7, §9.8).
    TranscriptTool,
    /// Validating the user's API key with a low-risk request (§4.4(3)).
    CredentialValidation,
}

impl ConsentPurpose {
    /// Every variant, for exhaustive decision-table tests.
    pub const ALL: [ConsentPurpose; 4] = [
        ConsentPurpose::Transcription,
        ConsentPurpose::Summarization,
        ConsentPurpose::TranscriptTool,
        ConsentPurpose::CredentialValidation,
    ];

    /// Stable label. Fixed vocabulary, safe for logs and error envelopes.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ConsentPurpose::Transcription => "transcription",
            ConsentPurpose::Summarization => "summarization",
            ConsentPurpose::TranscriptTool => "transcript_tool",
            ConsentPurpose::CredentialValidation => "credential_validation",
        }
    }
}

impl fmt::Display for ConsentPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// What would actually leave the device. The *content* half of a grant.
///
/// This is the consent-facing projection of [`PayloadClass`]: the payload
/// classes that carry no consentable user content ([`PayloadClass::None`]),
/// that are a credential rather than recording-derived content
/// ([`PayloadClass::Credential`]), or that may never leave the machine at all
/// ([`PayloadClass::VoiceBiometric`], VP-110) have no [`ContentType`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    /// Raw or transcoded audio bytes.
    AudioBytes,
    /// Transcript text derived from the recording.
    TranscriptText,
    /// Non-content metadata only: model ids, durations, counts.
    MetadataOnly,
}

impl ContentType {
    pub const ALL: [ContentType; 3] = [
        ContentType::AudioBytes,
        ContentType::TranscriptText,
        ContentType::MetadataOnly,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ContentType::AudioBytes => "audio_bytes",
            ContentType::TranscriptText => "transcript_text",
            ContentType::MetadataOnly => "metadata_only",
        }
    }

    /// True when the bytes are derived from the user's recording.
    #[must_use]
    pub const fn is_recording_derived(self) -> bool {
        matches!(self, ContentType::AudioBytes | ContentType::TranscriptText)
    }
}

impl fmt::Display for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl From<ContentType> for PayloadClass {
    fn from(value: ContentType) -> Self {
        match value {
            ContentType::AudioBytes => PayloadClass::AudioBytes,
            ContentType::TranscriptText => PayloadClass::TranscriptText,
            ContentType::MetadataOnly => PayloadClass::Metadata,
        }
    }
}

/// A [`PayloadClass`] that has no consent-facing [`ContentType`].
///
/// Returned by `TryFrom<PayloadClass> for ContentType` instead of silently
/// mapping to a nearby variant, which would widen a consent scope by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadClassNotConsentable {
    pub payload: PayloadClass,
}

impl fmt::Display for PayloadClassNotConsentable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "payload class {} has no consent content type",
            self.payload.label()
        )
    }
}

impl std::error::Error for PayloadClassNotConsentable {}

impl SecurityError for PayloadClassNotConsentable {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        ErrorEnvelopeParts::new(
            CONSENT_SCOPE_MISMATCH,
            "That request does not carry a kind of content a consent grant can cover.",
        )
    }
}

impl TryFrom<PayloadClass> for ContentType {
    type Error = PayloadClassNotConsentable;

    /// [`PayloadClass::VoiceBiometric`] is listed here beside the classes that
    /// simply have no content type, and that is what makes it unconsentable:
    /// a grant is built from a [`ContentType`], so there is no value a caller
    /// could put in a [`ConsentGrant`] that would cover a biometric payload.
    fn try_from(value: PayloadClass) -> Result<Self, Self::Error> {
        match value {
            PayloadClass::AudioBytes => Ok(ContentType::AudioBytes),
            PayloadClass::TranscriptText => Ok(ContentType::TranscriptText),
            PayloadClass::Metadata => Ok(ContentType::MetadataOnly),
            PayloadClass::None | PayloadClass::Credential | PayloadClass::VoiceBiometric => {
                Err(PayloadClassNotConsentable { payload: value })
            }
        }
    }
}

/// The relay identifier for `OpenRouter` (§10.1). The relay is the endpoint we
/// actually connect to; the downstream provider is who runs the model.
pub const RELAY_OPENROUTER: &str = "openrouter";

/// Relay identifier → pinned https endpoint. Unknown relays are refused by the
/// egress gate rather than guessed into a hostname (§2.5B fail-closed).
const RELAY_ENDPOINTS: &[(&str, &str)] = &[(RELAY_OPENROUTER, "https://openrouter.ai")];

/// The https endpoint for a known relay identifier, if any.
#[must_use]
pub fn relay_endpoint(relay: &str) -> Option<&'static str> {
    RELAY_ENDPOINTS
        .iter()
        .find(|(name, _)| *name == relay)
        .map(|(_, url)| *url)
}

/// Exactly which destination the content would reach (§10.8).
///
/// Two routes are equal only when **all three** fields match. A downstream
/// provider that could not be discovered is [`None`], and `None` never matches
/// a named provider in either direction: an undiscoverable destination must not
/// inherit a grant the user gave for a destination they could see, and a grant
/// taken while the provider was unknown must not silently cover a named one.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ProviderRoute {
    /// The relay we connect to, e.g. `"openrouter"`.
    pub relay: String,
    /// The model identifier as presented by the relay.
    pub model_id: String,
    /// The downstream inference provider, when discoverable (§10.8).
    pub downstream_provider: Option<String>,
}

impl ProviderRoute {
    /// A route whose downstream provider is not (yet) discoverable.
    #[must_use]
    pub fn new(relay: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            relay: relay.into(),
            model_id: model_id.into(),
            downstream_provider: None,
        }
    }

    #[must_use]
    pub fn with_downstream_provider(mut self, provider: impl Into<String>) -> Self {
        self.downstream_provider = Some(provider.into());
        self
    }

    /// True when the downstream inference provider was resolved. A `false` here
    /// is displayed to the user as "provider not disclosed", never as a vendor
    /// name we guessed.
    #[must_use]
    pub fn has_known_downstream_provider(&self) -> bool {
        self.downstream_provider.is_some()
    }

    /// Log- and envelope-safe label. Contains only catalogue identifiers the
    /// user chose from a list; never transcript text or a recording title.
    #[must_use]
    pub fn label(&self) -> String {
        let downstream = self
            .downstream_provider
            .as_deref()
            .unwrap_or("<undisclosed>");
        format!("{}|{}|{}", self.relay, self.model_id, downstream)
    }

    /// The egress destination for this route's relay.
    ///
    /// # Errors
    ///
    /// Returns [`EgressDenied`] when the relay identifier is not one of the
    /// pinned relays, or when the pinned endpoint fails URL validation.
    pub fn relay_destination(&self) -> Result<Destination, EgressDenied> {
        match relay_endpoint(&self.relay) {
            Some(url) => Destination::url(url),
            None => Err(EgressDenied {
                reason: DenyReason::DestinationNotAllowlisted,
                subsystem: Subsystem::OpenRouter,
                destination_label: "relay:<unrecognised>".to_owned(),
                user_action: None,
            }),
        }
    }
}

impl fmt::Display for ProviderRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// What the route's operator does with the content after the request (§10.5).
///
/// [`RetentionPosture::Unknown`] is a real, distinct state — not a default and
/// not an optimistic guess. It never satisfies a requirement, in either role
/// (§2.5B / §8.3 tri-state discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPosture {
    /// Documented zero retention for this exact endpoint and route.
    ZeroRetention,
    /// Documented retention bounded by `days`.
    LimitedRetention { days: u32 },
    /// The provider's published default applies and was resolved.
    ProviderDefault,
    /// Could not be resolved for this exact endpoint and route.
    Unknown,
}

impl RetentionPosture {
    /// True unless the posture is [`RetentionPosture::Unknown`].
    #[must_use]
    pub const fn is_resolved(self) -> bool {
        !matches!(self, RetentionPosture::Unknown)
    }

    /// Kind label without the day count, for logs and metrics.
    #[must_use]
    pub const fn kind_label(self) -> &'static str {
        match self {
            RetentionPosture::ZeroRetention => "zero_retention",
            RetentionPosture::LimitedRetention { .. } => "limited_retention",
            RetentionPosture::ProviderDefault => "provider_default",
            RetentionPosture::Unknown => "unknown",
        }
    }

    /// Does `self`, read as a *requirement*, accept `resolved` as the posture
    /// actually resolved for the route?
    ///
    /// Fail-closed rules: an unresolved requirement accepts nothing, and an
    /// unresolved resolution satisfies nothing.
    #[must_use]
    // The arms are kept one-per-case, including the several that return the
    // same answer, because this match *is* the published policy table.
    #[allow(clippy::match_same_arms)]
    pub const fn satisfied_by(self, resolved: RetentionPosture) -> bool {
        match (self, resolved) {
            // Nothing is satisfied by, or satisfies, an unresolved posture.
            (RetentionPosture::Unknown, _) | (_, RetentionPosture::Unknown) => false,
            (RetentionPosture::ZeroRetention, RetentionPosture::ZeroRetention) => true,
            (RetentionPosture::ZeroRetention, _) => false,
            (
                RetentionPosture::LimitedRetention { days: required },
                RetentionPosture::LimitedRetention { days: actual },
            ) => actual <= required,
            (RetentionPosture::LimitedRetention { .. }, RetentionPosture::ZeroRetention) => true,
            (RetentionPosture::LimitedRetention { .. }, RetentionPosture::ProviderDefault) => false,
            // "Whatever the provider publishes" is satisfied by any resolved posture.
            (RetentionPosture::ProviderDefault, _) => true,
        }
    }
}

impl fmt::Display for RetentionPosture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetentionPosture::LimitedRetention { days } => write!(f, "limited_retention:{days}d"),
            other => f.write_str(other.kind_label()),
        }
    }
}

/// The breadth the user chose in the §4.4 consent sheet.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantScope {
    /// "Ask every time". Recorded as a preference, never as an approval: it
    /// always sends the dispatch back to the user.
    AskEveryTime,
    /// "Allow for this recording".
    Recording { recording_id: String },
    /// "Allow for this model/policy", revokeable in settings.
    ModelPolicy { route: ProviderRoute },
}

/// The scope half of a dispatch, borrowed from the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestScope<'a> {
    pub recording_id: &'a str,
    pub route: &'a ProviderRoute,
}

impl GrantScope {
    /// Does this scope cover the requested recording/route pair?
    ///
    /// [`GrantScope::AskEveryTime`] covers nothing, by construction.
    #[must_use]
    pub fn covers(&self, request: RequestScope<'_>) -> bool {
        match self {
            GrantScope::AskEveryTime => false,
            GrantScope::Recording { recording_id } => recording_id == request.recording_id,
            GrantScope::ModelPolicy { route } => route == request.route,
        }
    }

    /// Stable kind label for logs; never includes the recording id.
    #[must_use]
    pub const fn kind_label(&self) -> &'static str {
        match self {
            GrantScope::AskEveryTime => "ask_every_time",
            GrantScope::Recording { .. } => "recording",
            GrantScope::ModelPolicy { .. } => "model_policy",
        }
    }
}

/// One scoped capability the user granted (§16.3).
///
/// A grant is only ever *narrowing*: every axis must match for a dispatch to be
/// covered. Widening requires a new grant, i.e. a new prompt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConsentGrant {
    /// Stable identifier, recorded on the inference attempt (§10.7 `local consent ID`).
    pub id: String,
    pub scope: GrantScope,
    pub purpose: ConsentPurpose,
    pub content_types: BTreeSet<ContentType>,
    /// The exact routes this grant covers. Authoritative for route matching,
    /// including for a [`GrantScope::ModelPolicy`] grant.
    pub routes: BTreeSet<ProviderRoute>,
    /// The retention restriction the user demanded, when they demanded one.
    pub required_retention: Option<RetentionPosture>,
    /// Version of the privacy statement shown at approval time (§4.4(5)).
    pub disclosure_version: String,
    pub granted_at_unix: i64,
    pub expires_at_unix: Option<i64>,
    pub revoked_at_unix: Option<i64>,
}

impl ConsentGrant {
    /// A grant with no content types and no routes yet; add them with
    /// [`ConsentGrant::with_content_type`] and [`ConsentGrant::with_route`].
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        scope: GrantScope,
        purpose: ConsentPurpose,
        disclosure_version: impl Into<String>,
        granted_at_unix: i64,
    ) -> Self {
        Self {
            id: id.into(),
            scope,
            purpose,
            content_types: BTreeSet::new(),
            routes: BTreeSet::new(),
            required_retention: None,
            disclosure_version: disclosure_version.into(),
            granted_at_unix,
            expires_at_unix: None,
            revoked_at_unix: None,
        }
    }

    /// "Allow for this model/policy" (§4.4): the route appears both as the
    /// scope and in the covered route set, so the two can never drift apart.
    #[must_use]
    pub fn for_model_policy(
        id: impl Into<String>,
        route: ProviderRoute,
        purpose: ConsentPurpose,
        disclosure_version: impl Into<String>,
        granted_at_unix: i64,
    ) -> Self {
        let mut grant = Self::new(
            id,
            GrantScope::ModelPolicy {
                route: route.clone(),
            },
            purpose,
            disclosure_version,
            granted_at_unix,
        );
        grant.routes.insert(route);
        grant
    }

    #[must_use]
    pub fn with_content_type(mut self, content_type: ContentType) -> Self {
        self.content_types.insert(content_type);
        self
    }

    #[must_use]
    pub fn with_content_types(mut self, content_types: BTreeSet<ContentType>) -> Self {
        self.content_types = content_types;
        self
    }

    #[must_use]
    pub fn with_route(mut self, route: ProviderRoute) -> Self {
        self.routes.insert(route);
        self
    }

    /// Replace the covered routes entirely.
    #[must_use]
    pub fn with_routes_replaced(mut self, routes: impl IntoIterator<Item = ProviderRoute>) -> Self {
        self.routes = routes.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_required_retention(mut self, required: RetentionPosture) -> Self {
        self.required_retention = Some(required);
        self
    }

    #[must_use]
    pub fn with_expiry(mut self, expires_at_unix: i64) -> Self {
        self.expires_at_unix = Some(expires_at_unix);
        self
    }

    /// A recorded revocation is final: we do not compare it against `now`, so a
    /// clock that moves backwards cannot re-enable a grant the user withdrew.
    #[must_use]
    pub const fn is_revoked(&self) -> bool {
        self.revoked_at_unix.is_some()
    }

    #[must_use]
    pub const fn is_expired(&self, now_unix: i64) -> bool {
        match self.expires_at_unix {
            Some(expiry) => expiry <= now_unix,
            None => false,
        }
    }

    /// Neither revoked nor expired. Says nothing about scope coverage.
    #[must_use]
    pub const fn is_active(&self, now_unix: i64) -> bool {
        !self.is_revoked() && !self.is_expired(now_unix)
    }

    /// Does this grant cover `route`, either through its route set or because
    /// it is the model-policy grant for that exact route?
    #[must_use]
    pub fn covers_route(&self, route: &ProviderRoute) -> bool {
        if self.routes.contains(route) {
            return true;
        }
        matches!(&self.scope, GrantScope::ModelPolicy { route: scoped } if scoped == route)
    }
}
