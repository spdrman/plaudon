//! Windows Credential Manager backend for [`crate::secrets`] (§16.4).
//!
//! Credentials are stored as `CRED_TYPE_GENERIC` entries whose target name is
//! `"<service>:<account>"`, written with `CredWriteW`, read with `CredReadW`
//! and removed with `CredDeleteW`.
//!
//! # Persistence choice
//!
//! We use `CRED_PERSIST_LOCAL_MACHINE`, not `CRED_PERSIST_ENTERPRISE`.
//!
//! Both store the credential in the **signed-in user's** credential set, so
//! neither is readable by another local user without that user's privileges.
//! They differ in roaming: `CRED_PERSIST_ENTERPRISE` replicates the credential
//! to every machine the user signs into with a roaming profile, while
//! `CRED_PERSIST_LOCAL_MACHINE` keeps it on this computer. App. B.7 requires
//! device authorization secrets to stay in the local vault of the machine that
//! was provisioned, and §7.7 binds them to one verified device identity, so
//! silently replicating them across a fleet is exactly wrong. The cost is that
//! a user with a roaming profile re-enters the `OpenRouter` key on each machine;
//! that is the correct trade.
//!
//! `CRED_PERSIST_SESSION` is also rejected: a credential that evaporates on
//! logoff would make the app re-prompt constantly and would tempt a caller into
//! caching it somewhere worse.
//!
//! The pure helpers in this module ([`map_win_error`], [`credential_target`],
//! [`wide_nul`]) are compiled and unit-tested on every target; only the FFI
//! implementation is `cfg(target_os = "windows")`.

use super::{SecretScope, SecretStoreError, StoreKind, UnavailableReason};

/// `ERROR_NOT_FOUND` — no credential with that target name.
pub const ERROR_NOT_FOUND: u32 = 1168;
/// `ERROR_NO_SUCH_LOGON_SESSION` — no credential set is available to this process.
pub const ERROR_NO_SUCH_LOGON_SESSION: u32 = 1312;
/// `ERROR_ACCESS_DENIED` — the credential set refused the operation.
pub const ERROR_ACCESS_DENIED: u32 = 5;
/// `ERROR_INVALID_PARAMETER` — malformed `CREDENTIALW`.
pub const ERROR_INVALID_PARAMETER: u32 = 87;
/// `ERROR_INVALID_FLAGS` — unsupported flag combination.
pub const ERROR_INVALID_FLAGS: u32 = 1004;
/// `ERROR_BAD_USERNAME` — the user name field was rejected.
pub const ERROR_BAD_USERNAME: u32 = 2202;

/// `CRED_MAX_CREDENTIAL_BLOB_SIZE`. Windows rejects a larger blob outright, so
/// we check it before the call and report a precise error instead of a bare
/// `ERROR_INVALID_PARAMETER`.
pub const CRED_MAX_CREDENTIAL_BLOB_SIZE: usize = 5 * 512;

/// The Credential Manager target name for a scope.
///
/// Both halves come from our own namespace and a validated
/// [`crate::secrets::DeviceId`], so the target name cannot be influenced into
/// colliding with another application's entry.
#[must_use]
pub fn credential_target(scope: &SecretScope) -> String {
    format!("{}:{}", scope.service(), scope.account())
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for the `W` APIs.
#[must_use]
pub fn wide_nul(value: &str) -> Vec<u16> {
    let mut buffer: Vec<u16> = value.encode_utf16().collect();
    buffer.push(0);
    buffer
}

/// Translate a Win32 error code into our credential-store error taxonomy.
///
/// `ERROR_NOT_FOUND` is the only "absent" code. `ERROR_ACCESS_DENIED` becomes
/// [`SecretStoreError::Locked`] because the remedy is a user action (sign in,
/// grant access), and `ERROR_NO_SUCH_LOGON_SESSION` becomes
/// [`SecretStoreError::Unavailable`] — which §16.4 still renders as an
/// actionable locked state, never as licence to store the value elsewhere.
#[must_use]
pub fn map_win_error(operation: &'static str, code: u32) -> SecretStoreError {
    match code {
        ERROR_NOT_FOUND => SecretStoreError::NotFound,
        ERROR_ACCESS_DENIED => SecretStoreError::Locked {
            store: StoreKind::WindowsCredentialManager,
        },
        ERROR_NO_SUCH_LOGON_SESSION => SecretStoreError::Unavailable {
            store: StoreKind::WindowsCredentialManager,
            reason: UnavailableReason::StoreNotAccessible {
                os_status: i32::try_from(code).unwrap_or(i32::MAX),
            },
        },
        other => SecretStoreError::backend(
            StoreKind::WindowsCredentialManager,
            operation,
            i32::try_from(other).unwrap_or(i32::MAX),
        ),
    }
}

#[cfg(target_os = "windows")]
pub use imp::WindowsCredentialStore;

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod imp {
    use std::fmt;
    use std::sync::Arc;

    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };
    use zeroize::Zeroize;

    use super::{credential_target, map_win_error, wide_nul, CRED_MAX_CREDENTIAL_BLOB_SIZE};
    use crate::secrets::{
        validate_secret, Clock, SecretPresence, SecretRef, SecretScope, SecretStore,
        SecretStoreError, SecretString, StoreKind,
    };

    /// Credential storage backed by the Windows Credential Manager.
    pub struct WindowsCredentialStore {
        clock: Arc<dyn Clock>,
    }

    impl WindowsCredentialStore {
        #[must_use]
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self { clock }
        }

        fn read(scope: &SecretScope) -> Result<SecretString, SecretStoreError> {
            let target = wide_nul(&credential_target(scope));
            let mut credential: *mut CREDENTIALW = std::ptr::null_mut();

            // SAFETY: `target` is a NUL-terminated UTF-16 buffer that outlives
            // the call, and `credential` is a valid out-pointer. On success the
            // callee allocates a buffer we release with `CredFree` below.
            let ok =
                unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut credential) };
            if ok == 0 {
                // SAFETY: no preconditions.
                let code = unsafe { GetLastError() };
                return Err(map_win_error("credential read", code));
            }
            if credential.is_null() {
                return Err(SecretStoreError::Backend {
                    detail: "credential read returned a null credential".to_owned(),
                });
            }

            // SAFETY: `credential` is non-null and was produced by `CredReadW`,
            // so it points at an initialised `CREDENTIALW` whose blob pointer
            // and length describe one allocation.
            let mut blob = unsafe {
                let view = &*credential;
                if view.CredentialBlob.is_null() || view.CredentialBlobSize == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(
                        view.CredentialBlob,
                        view.CredentialBlobSize as usize,
                    )
                    .to_vec()
                }
            };
            // SAFETY: `credential` came from `CredReadW` and has not been freed.
            unsafe { CredFree(credential.cast()) };

            if blob.is_empty() {
                return Err(SecretStoreError::NotFound);
            }
            let result = SecretString::from_utf8(std::mem::take(&mut blob));
            blob.zeroize();
            result
        }
    }

    impl fmt::Debug for WindowsCredentialStore {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("WindowsCredentialStore")
        }
    }

    impl SecretStore for WindowsCredentialStore {
        fn kind(&self) -> StoreKind {
            StoreKind::WindowsCredentialManager
        }

        fn store(
            &self,
            scope: &SecretScope,
            secret: &SecretString,
        ) -> Result<SecretRef, SecretStoreError> {
            validate_secret(secret)?;
            if secret.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE {
                return Err(SecretStoreError::Backend {
                    detail: format!(
                        "credential exceeds the Credential Manager blob limit of \
                         {CRED_MAX_CREDENTIAL_BLOB_SIZE} bytes"
                    ),
                });
            }

            let mut target = wide_nul(&credential_target(scope));
            let mut user_name = wide_nul(&scope.account());
            let blob_len = u32::try_from(secret.len()).unwrap_or(u32::MAX);
            let blob = secret.expose_bytes();

            let credential = CREDENTIALW {
                Flags: 0,
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                Comment: std::ptr::null_mut(),
                LastWritten: windows_sys::Win32::Foundation::FILETIME {
                    dwLowDateTime: 0,
                    dwHighDateTime: 0,
                },
                CredentialBlobSize: blob_len,
                CredentialBlob: blob.as_ptr().cast_mut(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount: 0,
                Attributes: std::ptr::null_mut(),
                TargetAlias: std::ptr::null_mut(),
                UserName: user_name.as_mut_ptr(),
            };

            // SAFETY: every pointer in `credential` refers to a live buffer
            // owned by this frame and outliving the call; `CredWriteW` only
            // reads them. The blob pointer is only read, never mutated.
            let ok = unsafe { CredWriteW(&raw const credential, 0) };
            if ok == 0 {
                // SAFETY: no preconditions.
                let code = unsafe { GetLastError() };
                return Err(map_win_error("credential write", code));
            }
            Ok(SecretRef::new(scope, secret, self.clock.now_unix()))
        }

        fn load(&self, scope: &SecretScope) -> Result<SecretString, SecretStoreError> {
            Self::read(scope)
        }

        fn delete(&self, scope: &SecretScope) -> Result<(), SecretStoreError> {
            let target = wide_nul(&credential_target(scope));
            // SAFETY: `target` is a NUL-terminated UTF-16 buffer outliving the call.
            let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
            if ok == 0 {
                // SAFETY: no preconditions.
                let code = unsafe { GetLastError() };
                return Err(map_win_error("credential delete", code));
            }
            Ok(())
        }

        fn status(&self, scope: &SecretScope) -> Result<SecretPresence, SecretStoreError> {
            // See the macOS backend: the plaintext is read, fingerprinted and
            // dropped; only the opaque reference escapes, and an inaccessible
            // credential set propagates rather than being reported as "absent".
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
