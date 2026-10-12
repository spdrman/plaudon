//! Test-support credential store (§18.5 hermetic security tests).
//!
//! # This is not a production backend
//!
//! The module is compiled only under `cfg(test)` or the `test-support` cargo
//! feature, which nothing in the application dependency graph enables. It is
//! therefore not reachable from a release build even by mistake, and it is
//! **not** the "plaintext fallback" §16.4 prohibits: it never touches the
//! filesystem, it does not survive the process, and no `cfg`-selected code path
//! chooses it ([`crate::secrets::os_store`] has no branch that can return it).
//!
//! It exists so the locked-vault and unavailable-vault paths — the ones that
//! must never degrade into returning a value — are testable without locking a
//! developer's real keychain.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    validate_secret, Clock, SecretPresence, SecretRef, SecretScope, SecretStore, SecretStoreError,
    SecretString, StoreKind,
};

/// A deterministic clock for tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(i64);

impl FixedClock {
    #[must_use]
    pub const fn new(now_unix: i64) -> Self {
        Self(now_unix)
    }
}

impl Clock for FixedClock {
    fn now_unix(&self) -> i64 {
        self.0
    }
}

/// One vault entry: the secret plus the reference recorded when it was written.
struct Entry {
    secret: SecretString,
    reference: SecretRef,
}

/// An in-process stand-in for an OS credential vault.
pub struct InMemorySecretStore {
    entries: Mutex<BTreeMap<(String, String), Entry>>,
    locked: AtomicBool,
    unavailable: AtomicBool,
    clock: Arc<dyn Clock>,
}

impl InMemorySecretStore {
    /// An unlocked, available, empty store on a fixed clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(FixedClock::new(1_700_000_000)))
    }

    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            locked: AtomicBool::new(false),
            unavailable: AtomicBool::new(false),
            clock,
        }
    }

    /// Simulate a locked vault: every operation fails with
    /// [`SecretStoreError::Locked`] and no value is returned.
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
    }

    /// Simulate a vault that cannot be reached at all.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Number of stored entries, for assertions about cleanup.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.lock_entries().len()
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), Entry>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The gate every operation passes through first, mirroring a real vault
    /// where lock state is checked before the item is looked up.
    fn gate(&self) -> Result<(), SecretStoreError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Unavailable {
                store: StoreKind::InMemoryTestStore,
                reason: super::UnavailableReason::StoreNotAccessible { os_status: -1 },
            });
        }
        if self.locked.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Locked {
                store: StoreKind::InMemoryTestStore,
            });
        }
        Ok(())
    }

    fn key(scope: &SecretScope) -> (String, String) {
        (scope.service().to_owned(), scope.account())
    }
}

impl Default for InMemorySecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for InMemorySecretStore {
    /// Prints counts and flags only. A `Debug` that could print entries would
    /// defeat the point of the type it is helping to test.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemorySecretStore")
            .field("entries", &self.lock_entries().len())
            .field("locked", &self.locked.load(Ordering::SeqCst))
            .field("unavailable", &self.unavailable.load(Ordering::SeqCst))
            .field("clock", &self.clock)
            .finish()
    }
}

impl SecretStore for InMemorySecretStore {
    fn kind(&self) -> StoreKind {
        StoreKind::InMemoryTestStore
    }

    fn store(
        &self,
        scope: &SecretScope,
        secret: &SecretString,
    ) -> Result<SecretRef, SecretStoreError> {
        self.gate()?;
        validate_secret(secret)?;
        let reference = SecretRef::new(scope, secret, self.clock.now_unix());
        self.lock_entries().insert(
            Self::key(scope),
            Entry {
                secret: SecretString::new(secret.expose().to_owned()),
                reference: reference.clone(),
            },
        );
        Ok(reference)
    }

    fn load(&self, scope: &SecretScope) -> Result<SecretString, SecretStoreError> {
        self.gate()?;
        self.lock_entries()
            .get(&Self::key(scope))
            .map(|entry| SecretString::new(entry.secret.expose().to_owned()))
            .ok_or(SecretStoreError::NotFound)
    }

    fn delete(&self, scope: &SecretScope) -> Result<(), SecretStoreError> {
        self.gate()?;
        self.lock_entries()
            .remove(&Self::key(scope))
            .map(|_| ())
            .ok_or(SecretStoreError::NotFound)
    }

    fn status(&self, scope: &SecretScope) -> Result<SecretPresence, SecretStoreError> {
        self.gate()?;
        Ok(self
            .lock_entries()
            .get(&Self::key(scope))
            .map_or_else(SecretPresence::absent, |entry| {
                SecretPresence::present(entry.reference.clone())
            }))
    }
}
