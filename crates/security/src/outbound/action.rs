//! Per-action egress grants (issue #303, ADR-0003, ADR-0013 #4).
//!
//! Turning Local-only Mode off opens the gate for the subsystems the user
//! consented to; it does not by itself let the app download a model or
//! install a dependency. Each of those needs the user to approve *that exact
//! action*: one named host and one named file or package. An
//! [`ActionEgressGrant`] is that approval. It wraps the ordinary
//! [`OutboundPolicy`] with a [`NetworkGrant`] whose host list is the one host,
//! refuses any URL that is not the approved file, is written to the consent
//! log as a [`ConsentGrant`] that covers no dispatch, and is revoked the
//! moment the action ends.
//!
//! It is refused outright while Local-only Mode is on: approving a download
//! never turns Local-only Mode off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use super::subprocess::GatedProcessLauncher;
use super::{
    user_action_for, ApprovalSource, DenyReason, Destination, EgressDenied, EgressPurpose,
    EgressRequest, EgressTicket, NetworkGrant, OutboundPolicy, PayloadClass, Subsystem,
};
use crate::consent::{ConsentGrant, ConsentPurpose, GrantScope, ProviderRoute};

/// Disclosure version recorded on every action grant.
pub const ACTION_DISCLOSURE_VERSION: &str = "2026-10-14.action-egress";

/// What one action would contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionScope {
    /// Subsystem the action belongs to.
    pub subsystem: Subsystem,
    /// Egress purpose the gate sees.
    pub purpose: EgressPurpose,
    /// How the action is recorded in the consent log.
    pub consent_purpose: ConsentPurpose,
    /// The one host the action is requested from.
    pub host: String,
    /// The one file (a URL path) or package the action fetches.
    pub resource: String,
    /// A domain the host may hand the file over from by redirect, disclosed
    /// before the user approves. `None` allows no redirect off the host.
    pub delivery_host_suffix: Option<String>,
}

impl ActionScope {
    /// The scope of downloading one model file from `url`.
    ///
    /// The URL must be one the gate would accept at all: HTTPS, a public DNS
    /// name, no credentials. Its query and fragment are not part of what is
    /// approved, so a URL carrying either is refused.
    pub fn model_download(
        url: &str,
        delivery_host_suffix: Option<&str>,
    ) -> Result<Self, EgressDenied> {
        let destination = Destination::url(url)?;
        let parsed = url::Url::parse(url).map_err(|_| {
            refusal(
                DenyReason::MalformedDestination,
                Subsystem::ModelDistribution,
                "<unparseable-url>",
            )
        })?;
        let host = destination.host().unwrap_or_default().to_owned();
        if parsed.query().is_some() || parsed.fragment().is_some() || parsed.path().len() <= 1 {
            return Err(refusal(
                DenyReason::MalformedDestination,
                Subsystem::ModelDistribution,
                &destination.label(),
            ));
        }
        Ok(Self {
            subsystem: Subsystem::ModelDistribution,
            purpose: EgressPurpose::ModelDownload,
            consent_purpose: ConsentPurpose::ModelDownload,
            host,
            resource: parsed.path().to_owned(),
            delivery_host_suffix: delivery_host_suffix.map(str::to_ascii_lowercase),
        })
    }

    /// The scope of installing one package from `host`.
    #[must_use]
    pub fn dependency_install(host: &str, package: &str) -> Self {
        Self {
            subsystem: Subsystem::PlaudCloudCli,
            purpose: EgressPurpose::AssetDownload,
            consent_purpose: ConsentPurpose::DependencyInstall,
            host: host.to_ascii_lowercase(),
            resource: package.to_owned(),
            delivery_host_suffix: None,
        }
    }

    fn label(&self) -> String {
        format!("https://{}", self.host)
    }
}

/// What the user approved, as the view showed it to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionApproval {
    /// The host the user saw named.
    pub host: String,
    /// The file or package the user saw named.
    pub resource: String,
}

/// One approved action's egress.
#[derive(Debug)]
pub struct ActionEgressGrant {
    id: String,
    scope: ActionScope,
    policy: Arc<OutboundPolicy>,
    granted_at_unix: i64,
    revoked: AtomicBool,
    revoked_at_unix: Mutex<Option<i64>>,
}

impl ActionEgressGrant {
    /// Approve `scope`, provided the user approved exactly it and Local-only
    /// Mode is off.
    pub fn approve(
        local_only: bool,
        scope: ActionScope,
        approval: &ActionApproval,
        id: impl Into<String>,
        now_unix: i64,
    ) -> Result<Self, EgressDenied> {
        // Local-only Mode first, so its refusal names the real reason: no
        // approval could have changed the answer.
        if local_only {
            return Err(refusal(
                DenyReason::LocalOnlyMode,
                scope.subsystem,
                &scope.label(),
            ));
        }
        if !approval.host.eq_ignore_ascii_case(&scope.host) || approval.resource != scope.resource {
            return Err(refusal(
                DenyReason::ActionNotApproved,
                scope.subsystem,
                &scope.label(),
            ));
        }
        let mut hosts = vec![scope.host.clone()];
        if let Some(suffix) = &scope.delivery_host_suffix {
            hosts.push(suffix.clone());
        }
        let grant = NetworkGrant::new(
            ApprovalSource::UserSettingsConfirmation,
            now_unix,
            ACTION_DISCLOSURE_VERSION,
        )
        .with_allowed_hosts(hosts)
        .with_model_downloads_approved(scope.purpose == EgressPurpose::ModelDownload)
        .with_dependency_install_approved(
            scope.consent_purpose == ConsentPurpose::DependencyInstall,
        );
        Ok(Self {
            id: id.into(),
            scope,
            policy: Arc::new(OutboundPolicy::network_permitted(grant)),
            granted_at_unix: now_unix,
            revoked: AtomicBool::new(false),
            revoked_at_unix: Mutex::new(None),
        })
    }

    /// The grant's identifier, which is also its consent-log id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What the grant covers.
    #[must_use]
    pub fn scope(&self) -> &ActionScope {
        &self.scope
    }

    /// Whether the action has ended.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }

    fn ended(&self) -> EgressDenied {
        refusal(
            DenyReason::ActionNotApproved,
            self.scope.subsystem,
            &self.scope.label(),
        )
    }

    /// A ticket for requesting `url`, which must be the approved file on the
    /// approved host.
    pub fn authorize_url(&self, url: &str) -> Result<EgressTicket, EgressDenied> {
        if self.is_revoked() || self.scope.purpose != EgressPurpose::ModelDownload {
            return Err(self.ended());
        }
        let destination = Destination::url(url)?;
        let parsed = url::Url::parse(url).map_err(|_| self.ended())?;
        let same_host = destination.host() == Some(self.scope.host.as_str());
        let same_file = parsed.path() == self.scope.resource
            && parsed.query().is_none()
            && parsed.fragment().is_none();
        if !same_host || !same_file {
            return Err(refusal(
                DenyReason::ActionNotApproved,
                self.scope.subsystem,
                &destination.label(),
            ));
        }
        self.policy.authorize(&EgressRequest::new(
            self.scope.subsystem,
            destination,
            self.scope.purpose,
            PayloadClass::None,
        ))
    }

    /// Whether a redirect to `url` stays inside what the user approved.
    ///
    /// The approved host itself, or a server under the delivery domain the
    /// disclosure named. Anything else, including a lookalike that merely
    /// ends in the same letters, is refused.
    pub fn authorize_redirect(&self, url: &str) -> Result<(), EgressDenied> {
        if self.is_revoked() || self.scope.purpose != EgressPurpose::ModelDownload {
            return Err(self.ended());
        }
        let destination = Destination::url(url)?;
        let host = destination.host().unwrap_or_default();
        let on_host = host == self.scope.host;
        let on_delivery = self
            .scope
            .delivery_host_suffix
            .as_deref()
            .is_some_and(|suffix| host == suffix || host.ends_with(&format!(".{suffix}")));
        if !on_host && !on_delivery {
            return Err(refusal(
                DenyReason::ActionNotApproved,
                self.scope.subsystem,
                &destination.label(),
            ));
        }
        self.policy
            .authorize(&EgressRequest::new(
                self.scope.subsystem,
                destination,
                self.scope.purpose,
                PayloadClass::None,
            ))
            .map(|_| ())
    }

    /// The process launcher for an approved install, gated by this grant.
    pub fn launcher(&self) -> Result<GatedProcessLauncher, EgressDenied> {
        if self.is_revoked() || self.scope.consent_purpose != ConsentPurpose::DependencyInstall {
            return Err(self.ended());
        }
        Ok(GatedProcessLauncher::new(Arc::clone(&self.policy)))
    }

    /// The consent-log row for this grant.
    ///
    /// "Ask every time" with no content type: the row records the action and
    /// can never be matched as consent for a dispatch of user content.
    #[must_use]
    pub fn consent_record(&self) -> ConsentGrant {
        let mut record = ConsentGrant::new(
            self.id.clone(),
            GrantScope::AskEveryTime,
            self.scope.consent_purpose,
            ACTION_DISCLOSURE_VERSION,
            self.granted_at_unix,
        )
        .with_route(ProviderRoute::new(
            self.scope.host.clone(),
            self.scope.resource.clone(),
        ));
        record.revoked_at_unix = *self.revoked_at_unix.lock();
        record
    }

    /// End the action: nothing more may be authorized under this grant.
    /// Returns the revoked consent-log row.
    pub fn revoke(&self, now_unix: i64) -> ConsentGrant {
        {
            let mut revoked_at = self.revoked_at_unix.lock();
            if revoked_at.is_none() {
                *revoked_at = Some(now_unix);
            }
        }
        self.revoked.store(true, Ordering::SeqCst);
        self.consent_record()
    }
}

fn refusal(reason: DenyReason, subsystem: Subsystem, label: &str) -> EgressDenied {
    EgressDenied {
        reason,
        subsystem,
        destination_label: label.to_owned(),
        user_action: user_action_for(reason),
    }
}
