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
use super::{EgressDenied, EgressPurpose, EgressTicket, OutboundPolicy, Subsystem};
use crate::consent::{ConsentGrant, ConsentPurpose};

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
    pub fn model_download(
        url: &str,
        delivery_host_suffix: Option<&str>,
    ) -> Result<Self, EgressDenied> {
        let _ = (url, delivery_host_suffix);
        unimplemented!("issue #303: per-action download scope")
    }

    /// The scope of installing one package from `host`.
    #[must_use]
    pub fn dependency_install(host: &str, package: &str) -> Self {
        let _ = (host, package);
        unimplemented!("issue #303: per-action install scope")
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
        let _ = (local_only, scope, approval, id.into(), now_unix);
        unimplemented!("issue #303: approve a per-action grant")
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

    /// A ticket for requesting `url`, which must be the approved file on the
    /// approved host.
    pub fn authorize_url(&self, url: &str) -> Result<EgressTicket, EgressDenied> {
        let _ = (url, &self.policy, self.granted_at_unix);
        unimplemented!("issue #303: authorize the approved url")
    }

    /// Whether a redirect to `url` stays inside what the user approved.
    pub fn authorize_redirect(&self, url: &str) -> Result<(), EgressDenied> {
        let _ = url;
        unimplemented!("issue #303: authorize a redirect hop")
    }

    /// The process launcher for an approved install, gated by this grant.
    pub fn launcher(&self) -> Result<GatedProcessLauncher, EgressDenied> {
        unimplemented!("issue #303: launcher under a per-action grant")
    }

    /// The consent-log row for this grant.
    #[must_use]
    pub fn consent_record(&self) -> ConsentGrant {
        let _ = &self.revoked_at_unix;
        unimplemented!("issue #303: consent record of a per-action grant")
    }

    /// End the action: nothing more may be authorized under this grant.
    /// Returns the revoked consent-log row.
    pub fn revoke(&self, now_unix: i64) -> ConsentGrant {
        let _ = now_unix;
        unimplemented!("issue #303: revoke a per-action grant")
    }
}
