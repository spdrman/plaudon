//! The dispatch preflight: the one function that decides whether user content
//! may leave the device for a cloud model (§16.3, §10.8).

use std::fmt;

use crate::error::{
    ErrorEnvelopeParts, SecurityError, CONSENT_DISCLOSURE_CHANGED, CONSENT_REQUIRED,
    CONSENT_REVOKED, CONSENT_SCOPE_MISMATCH, OPEN_CLOUD_CONSENT, PRIVACY_POSTURE_UNRESOLVED,
};
use crate::outbound::{
    EgressDenied, EgressPurpose, EgressRequest, EgressTicket, OutboundPolicy, PayloadClass,
    Subsystem,
};

use super::scope::{
    ConsentGrant, ConsentPurpose, ContentType, GrantScope, ProviderRoute, RequestScope,
    RetentionPosture,
};

/// One attempted cloud dispatch, fully described before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRequest {
    pub purpose: ConsentPurpose,
    pub content_type: ContentType,
    pub route: ProviderRoute,
    /// The retention posture resolved for this exact endpoint and route, or
    /// [`None`] when resolution was not even attempted (§10.8).
    pub resolved_retention: Option<RetentionPosture>,
    /// Opaque recording identifier. Never a title, never transcript text.
    pub recording_id: String,
    /// The user required a privacy restriction (e.g. "documented zero
    /// retention"), so an unresolved posture must fail closed.
    pub requires_privacy_restriction: bool,
    /// The disclosure version the app would show today.
    pub current_disclosure_version: String,
}

impl DispatchRequest {
    #[must_use]
    pub fn new(
        purpose: ConsentPurpose,
        content_type: ContentType,
        route: ProviderRoute,
        recording_id: impl Into<String>,
        current_disclosure_version: impl Into<String>,
    ) -> Self {
        Self {
            purpose,
            content_type,
            route,
            resolved_retention: None,
            recording_id: recording_id.into(),
            requires_privacy_restriction: false,
            current_disclosure_version: current_disclosure_version.into(),
        }
    }

    #[must_use]
    pub fn with_resolved_retention(mut self, resolved: RetentionPosture) -> Self {
        self.resolved_retention = Some(resolved);
        self
    }

    #[must_use]
    pub fn requiring_privacy_restriction(mut self, required: bool) -> Self {
        self.requires_privacy_restriction = required;
        self
    }

    /// The scope half of this request, for [`GrantScope::covers`].
    #[must_use]
    pub fn scope(&self) -> RequestScope<'_> {
        RequestScope {
            recording_id: &self.recording_id,
            route: &self.route,
        }
    }

    /// The egress purpose this dispatch maps to at the outbound gate.
    #[must_use]
    pub const fn egress_purpose(&self) -> EgressPurpose {
        match self.purpose {
            ConsentPurpose::CredentialValidation => EgressPurpose::CredentialValidation,
            ConsentPurpose::Transcription
            | ConsentPurpose::Summarization
            | ConsentPurpose::TranscriptTool => EgressPurpose::InferenceDispatch,
        }
    }

    /// The payload class the gate and the audit trail record.
    #[must_use]
    pub fn payload_class(&self) -> PayloadClass {
        PayloadClass::from(self.content_type)
    }
}

/// Everything the §4.4 consent sheet must show before this dispatch may be
/// approved. Carries identifiers and labels only — never transcript text, a
/// recording title or a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentPrompt {
    pub purpose: ConsentPurpose,
    pub content_type: ContentType,
    pub route: ProviderRoute,
    /// `scheme://host:port` of the relay, as the egress gate labels it.
    pub destination_label: String,
    /// The posture resolved for this exact route; [`RetentionPosture::Unknown`]
    /// when nothing could be resolved. Displayed as-is, never upgraded.
    pub resolved_retention: RetentionPosture,
    pub requires_privacy_restriction: bool,
    /// The disclosure version the user must be shown and agree to.
    pub disclosure_version: String,
    pub recording_id: String,
    /// The three §4.4 choices, in the order the sheet offers them.
    pub offered_scopes: Vec<GrantScope>,
}

impl ConsentPrompt {
    fn for_request(request: &DispatchRequest, destination_label: String) -> Self {
        Self {
            purpose: request.purpose,
            content_type: request.content_type,
            route: request.route.clone(),
            destination_label,
            resolved_retention: request
                .resolved_retention
                .unwrap_or(RetentionPosture::Unknown),
            requires_privacy_restriction: request.requires_privacy_restriction,
            disclosure_version: request.current_disclosure_version.clone(),
            recording_id: request.recording_id.clone(),
            offered_scopes: vec![
                GrantScope::AskEveryTime,
                GrantScope::Recording {
                    recording_id: request.recording_id.clone(),
                },
                GrantScope::ModelPolicy {
                    route: request.route.clone(),
                },
            ],
        }
    }
}

/// Which axis of the grant failed to cover the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeAxis {
    /// The exact relay/model/downstream-provider triple is not covered.
    Route,
    /// The kind of content (audio vs transcript vs metadata) is not covered.
    ContentType,
    /// The grant is bound to a different recording, or to a different policy route.
    RecordingScope,
    /// The request demands a privacy restriction the grant does not record.
    RetentionRequirement,
    /// The route's resolved posture is weaker than the grant requires.
    RetentionPosture,
}

impl ScopeAxis {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ScopeAxis::Route => "route",
            ScopeAxis::ContentType => "content_type",
            ScopeAxis::RecordingScope => "recording_scope",
            ScopeAxis::RetentionRequirement => "retention_requirement",
            ScopeAxis::RetentionPosture => "retention_posture",
        }
    }

    /// User-facing explanation. Fixed vocabulary only (§15.4).
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            ScopeAxis::Route => {
                "You approved a different model or provider, so this request needs a new approval."
            }
            ScopeAxis::ContentType => {
                "You approved a different kind of data for the cloud, so this request needs a new approval."
            }
            ScopeAxis::RecordingScope => {
                "Your approval covers a different recording, so this request needs a new approval."
            }
            ScopeAxis::RetentionRequirement => {
                "This request requires a data-retention guarantee that your approval does not cover."
            }
            ScopeAxis::RetentionPosture => {
                "The data-retention policy for this model and route is weaker than you required, so nothing was sent."
            }
        }
    }
}

/// Why a dispatch was not approved. Every variant maps to a stable §15.4 code.
#[derive(Debug)]
pub enum ConsentError {
    /// The outbound gate refused before consent was even considered (§16.2A).
    Egress(EgressDenied),
    /// No grant covers this dispatch; the user must be asked.
    Required { prompt: Box<ConsentPrompt> },
    /// A grant was close but does not cover the request on one axis.
    ScopeMismatch { axis: ScopeAxis, grant_id: String },
    /// The user withdrew this grant.
    Revoked {
        grant_id: String,
        revoked_at_unix: i64,
    },
    /// The grant has passed its expiry.
    Expired {
        grant_id: String,
        expired_at_unix: i64,
    },
    /// The privacy statement changed since approval (§4.4(5)).
    DisclosureChanged {
        grant_id: String,
        granted_version: String,
        current_version: String,
    },
    /// A privacy restriction was required but the posture for this exact
    /// endpoint and route could not be resolved (§10.8).
    PrivacyPostureUnresolved { route_label: String },
}

impl fmt::Display for ConsentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsentError::Egress(denied) => write!(f, "{denied}"),
            ConsentError::Required { prompt } => {
                write!(f, "consent required for purpose {}", prompt.purpose.label())
            }
            ConsentError::ScopeMismatch { axis, grant_id } => {
                write!(f, "grant {grant_id} does not cover axis {}", axis.label())
            }
            ConsentError::Revoked { grant_id, .. } => write!(f, "grant {grant_id} was revoked"),
            ConsentError::Expired { grant_id, .. } => write!(f, "grant {grant_id} has expired"),
            ConsentError::DisclosureChanged { grant_id, .. } => {
                write!(
                    f,
                    "grant {grant_id} predates the current privacy disclosure"
                )
            }
            ConsentError::PrivacyPostureUnresolved { route_label } => {
                write!(f, "retention posture unresolved for route {route_label}")
            }
        }
    }
}

impl std::error::Error for ConsentError {}

impl SecurityError for ConsentError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        match self {
            ConsentError::Egress(denied) => denied.envelope_parts(),
            ConsentError::Required { .. } => ErrorEnvelopeParts::new(
                CONSENT_REQUIRED,
                "Sending this to a cloud model needs your explicit approval first.",
            )
            .action(OPEN_CLOUD_CONSENT),
            ConsentError::ScopeMismatch { axis, .. } => {
                ErrorEnvelopeParts::new(CONSENT_SCOPE_MISMATCH, axis.explanation())
                    .action(OPEN_CLOUD_CONSENT)
            }
            ConsentError::Revoked { .. } => ErrorEnvelopeParts::new(
                CONSENT_REVOKED,
                "You revoked permission for this cloud destination, so nothing was sent.",
            )
            .action(OPEN_CLOUD_CONSENT),
            ConsentError::Expired { .. } => ErrorEnvelopeParts::new(
                CONSENT_REVOKED,
                "Your permission for this cloud destination has expired, so nothing was sent.",
            )
            .action(OPEN_CLOUD_CONSENT),
            ConsentError::DisclosureChanged { .. } => ErrorEnvelopeParts::new(
                CONSENT_DISCLOSURE_CHANGED,
                "The privacy disclosure changed since you approved this, so we need to ask again.",
            )
            .action(OPEN_CLOUD_CONSENT),
            ConsentError::PrivacyPostureUnresolved { .. } => ErrorEnvelopeParts::new(
                PRIVACY_POSTURE_UNRESOLVED,
                "We could not confirm the data-retention policy for this exact model and route, \
                 and you required one, so nothing was sent.",
            )
            .action(OPEN_CLOUD_CONSENT),
        }
    }
}

/// Permission to dispatch exactly one request.
///
/// Holding one proves **both** gates were passed: the outbound policy (the
/// [`EgressTicket`], which cannot be constructed outside
/// [`crate::outbound`]) and the consent preflight (the `grant_id`). The ticket
/// is not [`Clone`], so an approval authorizes a single attempt.
#[derive(Debug)]
pub struct DispatchApproval {
    /// The grant that covered this dispatch; record it on the inference attempt
    /// (§10.7 `local consent ID`).
    pub grant_id: String,
    pub ticket: EgressTicket,
    pub route: ProviderRoute,
    pub content_type: ContentType,
}

impl DispatchApproval {
    /// Consume the approval, yielding the ticket the transport requires.
    #[must_use]
    pub fn into_ticket(self) -> EgressTicket {
        self.ticket
    }
}

/// Decide whether one cloud dispatch may proceed.
///
/// The order is fixed and is part of the contract:
///
/// 1. **The egress gate.** [`OutboundPolicy::authorize`] runs first and
///    unconditionally, so Local-only Mode denies regardless of any grant
///    (§16.2A). The attempt is recorded by the policy's observer either way.
/// 2. **Grant selection.** A grant must be unrevoked, unexpired, for the same
///    purpose, cover the content type and the exact route, cover the request's
///    scope, and carry the current disclosure version (§16.3, §4.4(5)).
/// 3. **Privacy posture.** When the user required a privacy restriction, the
///    posture must have resolved for this exact endpoint and route and must
///    satisfy the grant's requirement (§10.8).
///
/// # Errors
///
/// Returns the [`ConsentError`] for the first gate that refused; see the
/// variants for the stable code each maps to.
pub fn dispatch_preflight(
    policy: &OutboundPolicy,
    grants: &[ConsentGrant],
    request: &DispatchRequest,
    now_unix: i64,
) -> Result<DispatchApproval, ConsentError> {
    // Step 1 — the egress gate, always, before anything else is considered.
    let destination = request
        .route
        .relay_destination()
        .map_err(ConsentError::Egress)?;
    let payload = if request.purpose == ConsentPurpose::CredentialValidation {
        // Validating a key sends the key itself; label it honestly regardless
        // of the content type the caller declared (§4.4(3)).
        PayloadClass::Credential
    } else {
        request.payload_class()
    };
    let egress_request = EgressRequest::new(
        Subsystem::OpenRouter,
        destination,
        request.egress_purpose(),
        payload,
    );
    let ticket = policy
        .authorize(&egress_request)
        .map_err(ConsentError::Egress)?;

    // Step 2 — scoped consent.
    let grant = select_grant(grants, request, now_unix, ticket.destination_label())?;

    // Step 3 — privacy posture for this exact endpoint and route.
    check_retention(grant, request)?;

    Ok(DispatchApproval {
        grant_id: grant.id.clone(),
        ticket,
        route: request.route.clone(),
        content_type: request.content_type,
    })
}

/// How far a grant got before it failed. A higher rank is a more specific
/// diagnosis, so the grant that got furthest decides the reported error.
type Rank = u8;

const RANK_ROUTE: Rank = 1;
const RANK_CONTENT_TYPE: Rank = 2;
const RANK_SCOPE: Rank = 3;
const RANK_REVOKED: Rank = 4;
const RANK_EXPIRED: Rank = 5;
const RANK_DISCLOSURE: Rank = 6;
const RANK_ASK_EVERY_TIME: Rank = 7;

fn select_grant<'g>(
    grants: &'g [ConsentGrant],
    request: &DispatchRequest,
    now_unix: i64,
    destination_label: &str,
) -> Result<&'g ConsentGrant, ConsentError> {
    let mut best: Option<(Rank, ConsentError)> = None;

    for grant in grants {
        // A grant for another purpose is not a near miss, it is a different
        // decision entirely; it never shapes the reported error.
        if grant.purpose != request.purpose {
            continue;
        }
        match evaluate_grant(grant, request, now_unix, destination_label) {
            Ok(()) => return Ok(grant),
            Err((rank, error)) => {
                if best.as_ref().is_none_or(|(best_rank, _)| rank > *best_rank) {
                    best = Some((rank, error));
                }
            }
        }
    }

    Err(best.map_or_else(
        || ConsentError::Required {
            prompt: Box::new(ConsentPrompt::for_request(
                request,
                destination_label.to_owned(),
            )),
        },
        |(_, error)| error,
    ))
}

fn evaluate_grant(
    grant: &ConsentGrant,
    request: &DispatchRequest,
    now_unix: i64,
    destination_label: &str,
) -> Result<(), (Rank, ConsentError)> {
    if !grant.routes.contains(&request.route) {
        return Err((
            RANK_ROUTE,
            ConsentError::ScopeMismatch {
                axis: ScopeAxis::Route,
                grant_id: grant.id.clone(),
            },
        ));
    }
    if !grant.content_types.contains(&request.content_type) {
        return Err((
            RANK_CONTENT_TYPE,
            ConsentError::ScopeMismatch {
                axis: ScopeAxis::ContentType,
                grant_id: grant.id.clone(),
            },
        ));
    }
    if !grant.scope.covers(request.scope()) {
        // "Ask every time" is a recorded preference, not a near miss: it must
        // produce the prompt rather than a scope-mismatch diagnosis (§4.4).
        if matches!(grant.scope, GrantScope::AskEveryTime) {
            return Err((
                RANK_ASK_EVERY_TIME,
                ConsentError::Required {
                    prompt: Box::new(ConsentPrompt::for_request(
                        request,
                        destination_label.to_owned(),
                    )),
                },
            ));
        }
        return Err((
            RANK_SCOPE,
            ConsentError::ScopeMismatch {
                axis: ScopeAxis::RecordingScope,
                grant_id: grant.id.clone(),
            },
        ));
    }
    if let Some(revoked_at_unix) = grant.revoked_at_unix {
        return Err((
            RANK_REVOKED,
            ConsentError::Revoked {
                grant_id: grant.id.clone(),
                revoked_at_unix,
            },
        ));
    }
    if grant.is_expired(now_unix) {
        let expired_at_unix = grant.expires_at_unix.unwrap_or(now_unix);
        return Err((
            RANK_EXPIRED,
            ConsentError::Expired {
                grant_id: grant.id.clone(),
                expired_at_unix,
            },
        ));
    }
    if grant.disclosure_version != request.current_disclosure_version {
        return Err((
            RANK_DISCLOSURE,
            ConsentError::DisclosureChanged {
                grant_id: grant.id.clone(),
                granted_version: grant.disclosure_version.clone(),
                current_version: request.current_disclosure_version.clone(),
            },
        ));
    }
    Ok(())
}

fn check_retention(grant: &ConsentGrant, request: &DispatchRequest) -> Result<(), ConsentError> {
    if !request.requires_privacy_restriction {
        return Ok(());
    }

    // §10.8: a policy must resolve for the exact endpoint and route. Neither a
    // missing resolution nor an explicitly unknown one may proceed.
    let Some(resolved) = request
        .resolved_retention
        .filter(|posture| posture.is_resolved())
    else {
        return Err(ConsentError::PrivacyPostureUnresolved {
            route_label: request.route.label(),
        });
    };

    let Some(required) = grant.required_retention else {
        return Err(ConsentError::ScopeMismatch {
            axis: ScopeAxis::RetentionRequirement,
            grant_id: grant.id.clone(),
        });
    };

    if required.satisfied_by(resolved) {
        Ok(())
    } else {
        Err(ConsentError::ScopeMismatch {
            axis: ScopeAxis::RetentionPosture,
            grant_id: grant.id.clone(),
        })
    }
}
