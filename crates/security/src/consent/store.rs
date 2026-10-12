//! Holding and revoking grants.
//!
//! [`ConsentLedger`] is an in-memory collection with no persistence opinion.
//! [`ConsentRepository`] is the seam `crates/persistence` implements, so the
//! security crate never depends on a database and contains no SQL.

use std::collections::BTreeMap;
use std::convert::Infallible;

use super::scope::{ConsentGrant, ProviderRoute};

/// Durable storage for consent grants, implemented outside this crate.
///
/// The associated error type belongs to the implementor: `crates/security`
/// deliberately does not define a storage error, because it has no storage.
pub trait ConsentRepository {
    /// Storage failure type owned by the implementor.
    type Error;

    /// Every grant, revoked ones included: revocation history is evidence and
    /// is never deleted by load/store round-tripping.
    ///
    /// # Errors
    ///
    /// Implementation-defined storage failure.
    fn load_grants(&self) -> Result<Vec<ConsentGrant>, Self::Error>;

    /// Insert or replace a grant by [`ConsentGrant::id`].
    ///
    /// # Errors
    ///
    /// Implementation-defined storage failure.
    fn upsert_grant(&mut self, grant: &ConsentGrant) -> Result<(), Self::Error>;

    /// Record a revocation. Must be idempotent and must not resurrect a grant.
    ///
    /// # Errors
    ///
    /// Implementation-defined storage failure.
    fn mark_revoked(&mut self, grant_id: &str, at_unix: i64) -> Result<(), Self::Error>;
}

/// An in-memory set of grants, keyed by id and ordered deterministically.
///
/// The ledger holds revoked grants rather than dropping them: the dispatch
/// preflight distinguishes "never granted" (`CONSENT_REQUIRED`) from "you
/// revoked this" (`CONSENT_REVOKED`), and §18.6A requires that a revocation
/// mid-queue is observable rather than silently equivalent to absence.
#[derive(Debug, Default, Clone)]
pub struct ConsentLedger {
    grants: BTreeMap<String, ConsentGrant>,
}

impl ConsentLedger {
    #[must_use]
    pub fn new() -> Self {
        Self {
            grants: BTreeMap::new(),
        }
    }

    /// Build a ledger from grants already loaded from somewhere else.
    #[must_use]
    pub fn from_grants(grants: impl IntoIterator<Item = ConsentGrant>) -> Self {
        Self {
            grants: grants.into_iter().map(|g| (g.id.clone(), g)).collect(),
        }
    }

    /// Load every grant through a repository.
    ///
    /// # Errors
    ///
    /// Propagates the repository's error.
    pub fn load_from<R: ConsentRepository + ?Sized>(repository: &R) -> Result<Self, R::Error> {
        Ok(Self::from_grants(repository.load_grants()?))
    }

    /// Record a grant, replacing any existing one with the same id. Returns the
    /// replaced grant, if there was one.
    pub fn grant(&mut self, grant: ConsentGrant) -> Option<ConsentGrant> {
        self.grants.insert(grant.id.clone(), grant)
    }

    /// Record a grant and persist it in the same step.
    ///
    /// # Errors
    ///
    /// Propagates the repository's error; the in-memory ledger is only updated
    /// after the repository accepted the write.
    pub fn grant_persisted<R: ConsentRepository + ?Sized>(
        &mut self,
        repository: &mut R,
        grant: ConsentGrant,
    ) -> Result<(), R::Error> {
        repository.upsert_grant(&grant)?;
        self.grants.insert(grant.id.clone(), grant);
        Ok(())
    }

    /// Revoke one grant. Returns `true` when a grant was found and this call
    /// recorded the revocation; `false` when there is no such grant or it was
    /// already revoked. The earliest revocation timestamp is kept.
    pub fn revoke(&mut self, id: &str, at_unix: i64) -> bool {
        match self.grants.get_mut(id) {
            Some(grant) if grant.revoked_at_unix.is_none() => {
                grant.revoked_at_unix = Some(at_unix);
                true
            }
            _ => false,
        }
    }

    /// Revoke one grant and persist the revocation in the same step.
    ///
    /// # Errors
    ///
    /// Propagates the repository's error; the in-memory ledger is only updated
    /// after the repository accepted the write.
    pub fn revoke_persisted<R: ConsentRepository + ?Sized>(
        &mut self,
        repository: &mut R,
        id: &str,
        at_unix: i64,
    ) -> Result<bool, R::Error> {
        if self
            .grants
            .get(id)
            .is_none_or(|g| g.revoked_at_unix.is_some())
        {
            return Ok(false);
        }
        repository.mark_revoked(id, at_unix)?;
        Ok(self.revoke(id, at_unix))
    }

    /// Revoke every grant that covers `route`, including model-policy grants
    /// scoped to it (§10.8: changing the route invalidates the grant).
    /// Returns how many grants this call newly revoked.
    pub fn revoke_all_for_route(&mut self, route: &ProviderRoute, at_unix: i64) -> usize {
        let mut revoked = 0;
        for grant in self.grants.values_mut() {
            if grant.revoked_at_unix.is_none() && grant.covers_route(route) {
                grant.revoked_at_unix = Some(at_unix);
                revoked += 1;
            }
        }
        revoked
    }

    /// Grants that are neither revoked nor expired at `now_unix`. Says nothing
    /// about whether any of them covers a particular dispatch.
    #[must_use]
    pub fn active_grants(&self, now_unix: i64) -> Vec<&ConsentGrant> {
        self.grants
            .values()
            .filter(|grant| grant.is_active(now_unix))
            .collect()
    }

    /// An owned snapshot suitable for passing to
    /// [`super::dispatch_preflight`], which takes a plain slice so it stays
    /// independent of how grants are stored.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ConsentGrant> {
        self.grants.values().cloned().collect()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ConsentGrant> {
        self.grants.get(id)
    }

    /// Every grant, revoked and expired ones included.
    pub fn all_grants(&self) -> impl Iterator<Item = &ConsentGrant> {
        self.grants.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.grants.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }
}

/// The ledger is itself a (volatile) repository. Useful in tests and for the
/// pre-persistence boot path; it cannot fail, hence [`Infallible`].
impl ConsentRepository for ConsentLedger {
    type Error = Infallible;

    fn load_grants(&self) -> Result<Vec<ConsentGrant>, Self::Error> {
        Ok(self.snapshot())
    }

    fn upsert_grant(&mut self, grant: &ConsentGrant) -> Result<(), Self::Error> {
        self.grants.insert(grant.id.clone(), grant.clone());
        Ok(())
    }

    fn mark_revoked(&mut self, grant_id: &str, at_unix: i64) -> Result<(), Self::Error> {
        self.revoke(grant_id, at_unix);
        Ok(())
    }
}
