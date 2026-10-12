//! macOS Keychain backend for [`crate::secrets`] (§16.4).
//!
//! Credentials are stored as **generic passwords** under the service
//! [`crate::secrets::SECRET_SERVICE`] and a per-scope account string, which is
//! the item class the Keychain intends for application credentials and the one
//! that participates in the user's normal lock/unlock and ACL flow.
//!
//! The status-code mapping ([`map_osstatus`]) is a pure function and is
//! compiled and unit-tested on every target, so the "a locked keychain is a
//! locked state, never a downgrade" rule is covered by hermetic CI rather than
//! only by an opt-in macOS job.

use super::{SecretStoreError, StoreKind, UnavailableReason};

// Apple `OSStatus` values from `<Security/SecBase.h>`. `security-framework-sys`
// does not re-export the interaction/cancel family, so they are declared here
// with their documented numeric values.

/// `errSecUserCanceled` — the user dismissed the unlock or allow-access prompt.
pub const ERR_SEC_USER_CANCELED: i32 = -128;
/// `errSecNotAvailable` — no keychain is available (e.g. no logged-in session).
pub const ERR_SEC_NOT_AVAILABLE: i32 = -25291;
/// `errSecAuthFailed` — authorization/authentication was refused.
pub const ERR_SEC_AUTH_FAILED: i32 = -25293;
/// `errSecNoSuchKeychain` — the referenced keychain does not exist.
pub const ERR_SEC_NO_SUCH_KEYCHAIN: i32 = -25294;
/// `errSecDuplicateItem` — an item with the same primary key already exists.
pub const ERR_SEC_DUPLICATE_ITEM: i32 = -25299;
/// `errSecItemNotFound` — no matching item.
pub const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
/// `errSecNoDefaultKeychain` — no default keychain is configured.
pub const ERR_SEC_NO_DEFAULT_KEYCHAIN: i32 = -25307;
/// `errSecInteractionNotAllowed` — the keychain is locked and UI is disallowed.
pub const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
/// `errSecInteractionRequired` — user interaction is required but unavailable.
pub const ERR_SEC_INTERACTION_REQUIRED: i32 = -25315;
/// `errSecMissingEntitlement` — the process lacks the keychain-access entitlement.
pub const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018;

/// Translate an Apple `OSStatus` into our credential-store error taxonomy.
///
/// The three outcomes that matter:
///
/// * `errSecItemNotFound` → [`SecretStoreError::NotFound`];
/// * locked keychain / declined prompt / failed auth →
///   [`SecretStoreError::Locked`], which the envelope renders as
///   `SECRET_STORE_LOCKED` + `UNLOCK_CREDENTIAL_STORE`;
/// * no keychain at all / missing entitlement →
///   [`SecretStoreError::Unavailable`], which maps to the *same* actionable
///   locked state (§16.4 forbids treating it as permission to downgrade).
///
/// `operation` is a `&'static str` from our own vocabulary, so the resulting
/// message cannot contain credential material.
#[must_use]
pub fn map_osstatus(operation: &'static str, status: i32) -> SecretStoreError {
    match status {
        ERR_SEC_ITEM_NOT_FOUND => SecretStoreError::NotFound,
        ERR_SEC_INTERACTION_NOT_ALLOWED
        | ERR_SEC_INTERACTION_REQUIRED
        | ERR_SEC_USER_CANCELED
        | ERR_SEC_AUTH_FAILED => SecretStoreError::Locked {
            store: StoreKind::MacosKeychain,
        },
        ERR_SEC_NOT_AVAILABLE
        | ERR_SEC_NO_SUCH_KEYCHAIN
        | ERR_SEC_NO_DEFAULT_KEYCHAIN
        | ERR_SEC_MISSING_ENTITLEMENT => SecretStoreError::Unavailable {
            store: StoreKind::MacosKeychain,
            reason: UnavailableReason::StoreNotAccessible { os_status: status },
        },
        other => SecretStoreError::backend(StoreKind::MacosKeychain, operation, other),
    }
}

#[cfg(target_os = "macos")]
pub use imp::MacosKeychainStore;

#[cfg(target_os = "macos")]
mod imp {
    use std::fmt;
    use std::sync::Arc;

    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    use super::map_osstatus;
    use crate::secrets::{
        validate_secret, Clock, SecretPresence, SecretRef, SecretScope, SecretStore,
        SecretStoreError, SecretString, StoreKind,
    };

    /// Credential storage backed by the macOS Keychain.
    pub struct MacosKeychainStore {
        clock: Arc<dyn Clock>,
    }

    impl MacosKeychainStore {
        #[must_use]
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self { clock }
        }

        fn read(scope: &SecretScope) -> Result<SecretString, SecretStoreError> {
            match get_generic_password(scope.service(), &scope.account()) {
                Ok(bytes) => SecretString::from_utf8(bytes),
                Err(err) => Err(map_osstatus("keychain read", err.code())),
            }
        }
    }

    impl fmt::Debug for MacosKeychainStore {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("MacosKeychainStore")
        }
    }

    impl SecretStore for MacosKeychainStore {
        fn kind(&self) -> StoreKind {
            StoreKind::MacosKeychain
        }

        fn store(
            &self,
            scope: &SecretScope,
            secret: &SecretString,
        ) -> Result<SecretRef, SecretStoreError> {
            validate_secret(secret)?;
            // `set_generic_password` updates in place when the item exists, so
            // rotation never leaves two entries for one scope.
            set_generic_password(scope.service(), &scope.account(), secret.expose_bytes())
                .map_err(|err| map_osstatus("keychain write", err.code()))?;
            Ok(SecretRef::new(scope, secret, self.clock.now_unix()))
        }

        fn load(&self, scope: &SecretScope) -> Result<SecretString, SecretStoreError> {
            Self::read(scope)
        }

        fn delete(&self, scope: &SecretScope) -> Result<(), SecretStoreError> {
            delete_generic_password(scope.service(), &scope.account())
                .map_err(|err| map_osstatus("keychain delete", err.code()))
        }

        fn status(&self, scope: &SecretScope) -> Result<SecretPresence, SecretStoreError> {
            // Reading the item is the only way to produce an honest
            // fingerprint; the plaintext is dropped (and zeroized) before this
            // function returns, and nothing derived from it but the opaque
            // reference escapes. A locked keychain propagates as `Locked`
            // rather than being flattened into "absent".
            match Self::read(scope) {
                Ok(secret) => {
                    let reference = SecretRef::new(scope, &secret, self.clock.now_unix());
                    drop(secret);
                    Ok(SecretPresence::present(reference))
                }
                Err(SecretStoreError::NotFound) => Ok(SecretPresence::absent()),
                Err(other) => Err(other),
            }
        }
    }
}
