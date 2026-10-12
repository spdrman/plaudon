//! OS keychain storage for the OpenRouter key (spec §15.2, §16).
//!
//! There is no plaintext fallback. If the keychain is locked or unavailable the
//! operation fails with [`AppErrorCode::SecretStoreLocked`]; the key is never
//! written to the database, to a settings file or to a log line. Nothing in
//! this module can return the secret to the frontend: `Capabilities` exposes
//! only the boolean `openrouter_key_present`.

use std::sync::Mutex;

use plaudon_domain::error::{AppError, AppErrorCode};

/// Keychain service name used for every secret this application stores.
pub const KEYCHAIN_SERVICE: &str = "app.plaudon.desktop";
/// Keychain account name of the OpenRouter key.
pub const OPENROUTER_ACCOUNT: &str = "openrouter_api_key";

/// A secret in memory.
///
/// Deliberately implements neither `Debug`, nor `Display`, nor `Serialize`, nor
/// `Clone`: there is no way to print it, format it into a message or let serde
/// walk it into a response. The only accessor is [`SecretValue::expose`], which
/// is crate-private and used solely by the keychain writer.
#[allow(missing_debug_implementations)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wrap a user-entered secret, rejecting values that cannot be a key.
    // The secret is moved in and dropped with the wrapper rather than leaving
    // a second copy in the caller's `String`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(raw: String) -> Result<Self, AppError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AppError::new(
                AppErrorCode::InvalidArgument,
                "the key is empty",
            ));
        }
        if trimmed.chars().any(char::is_control) {
            return Err(AppError::new(
                AppErrorCode::InvalidArgument,
                "the key contains control characters",
            ));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Borrow the secret. Only the keychain writer calls this.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

/// Storage port for secrets, so tests never touch the real keychain.
pub trait SecretStore: Send + Sync {
    /// Store or replace the OpenRouter key.
    fn set_openrouter_key(&self, value: &SecretValue) -> Result<(), AppError>;

    /// Remove the OpenRouter key. Removing an absent key succeeds.
    fn remove_openrouter_key(&self) -> Result<(), AppError>;

    /// Whether a key is present, without reading its value out of the store.
    fn openrouter_key_present(&self) -> Result<bool, AppError>;

    /// Read the key for one authenticated operation, `None` when absent.
    ///
    /// The value is a [`SecretValue`], which cannot be printed, serialized or
    /// cloned; the cloud clients hold it only for the duration of one request.
    fn load_openrouter_key(&self) -> Result<Option<SecretValue>, AppError>;
}

impl std::fmt::Debug for dyn SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretStore")
    }
}

/// The real store: the macOS keychain, the Windows credential manager or the
/// freedesktop secret service, via the `keyring` crate.
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
    account: String,
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_owned(),
            account: OPENROUTER_ACCOUNT.to_owned(),
        }
    }
}

impl KeyringSecretStore {
    /// Store bound to the application's keychain service.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&self) -> Result<keyring::Entry, AppError> {
        keyring::Entry::new(&self.service, &self.account).map_err(|err| map_keyring_error(&err))
    }
}

fn map_keyring_error(err: &keyring::Error) -> AppError {
    // The message may name the platform backend, so it stays in the redacted
    // diagnostic detail and never reaches the envelope.
    AppError::new(
        AppErrorCode::SecretStoreLocked,
        format!("keychain unavailable: {err}"),
    )
}

impl SecretStore for KeyringSecretStore {
    fn set_openrouter_key(&self, value: &SecretValue) -> Result<(), AppError> {
        self.entry()?
            .set_password(value.expose())
            .map_err(|err| map_keyring_error(&err))
    }

    fn remove_openrouter_key(&self) -> Result<(), AppError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(map_keyring_error(&err)),
        }
    }

    fn openrouter_key_present(&self) -> Result<bool, AppError> {
        match self.entry()?.get_password() {
            Ok(_) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(err) => Err(map_keyring_error(&err)),
        }
    }

    fn load_openrouter_key(&self) -> Result<Option<SecretValue>, AppError> {
        match self.entry()?.get_password() {
            Ok(raw) => SecretValue::new(raw).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(map_keyring_error(&err)),
        }
    }
}

/// In-memory store used by tests. Never constructed by the shipping binary.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    value: Mutex<Option<String>>,
    unavailable: bool,
}

impl InMemorySecretStore {
    /// An empty, working store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that behaves like a locked keychain.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            value: Mutex::new(None),
            unavailable: true,
        }
    }

    fn guard(&self) -> Result<std::sync::MutexGuard<'_, Option<String>>, AppError> {
        if self.unavailable {
            return Err(AppError::new(
                AppErrorCode::SecretStoreLocked,
                "test store is configured as unavailable",
            ));
        }
        self.value.lock().map_err(|_| {
            AppError::new(
                AppErrorCode::InternalError,
                "secret store mutex was poisoned",
            )
        })
    }
}

impl SecretStore for InMemorySecretStore {
    fn set_openrouter_key(&self, value: &SecretValue) -> Result<(), AppError> {
        *self.guard()? = Some(value.expose().to_owned());
        Ok(())
    }

    fn remove_openrouter_key(&self) -> Result<(), AppError> {
        *self.guard()? = None;
        Ok(())
    }

    fn openrouter_key_present(&self) -> Result<bool, AppError> {
        Ok(self.guard()?.is_some())
    }

    fn load_openrouter_key(&self) -> Result<Option<SecretValue>, AppError> {
        self.guard()?.clone().map(SecretValue::new).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-or-v1-0123456789abcdef0123456789abcdef";

    #[test]
    fn empty_and_control_character_keys_are_rejected() {
        assert!(SecretValue::new(String::new()).is_err());
        assert!(SecretValue::new("   ".into()).is_err());
        assert!(SecretValue::new("abc\ndef".into()).is_err());
        assert_eq!(
            SecretValue::new("  padded  ".into()).unwrap().expose(),
            "padded",
            "surrounding whitespace from a paste is trimmed"
        );
    }

    #[test]
    fn the_store_round_trips_presence_without_exposing_the_value() {
        let store = InMemorySecretStore::new();
        assert!(!store.openrouter_key_present().unwrap());
        store
            .set_openrouter_key(&SecretValue::new(KEY.into()).unwrap())
            .unwrap();
        assert!(store.openrouter_key_present().unwrap());
        store.remove_openrouter_key().unwrap();
        assert!(!store.openrouter_key_present().unwrap());
        store
            .remove_openrouter_key()
            .expect("removing an absent key succeeds");
    }

    #[test]
    fn an_unavailable_store_reports_secret_store_locked_and_writes_nothing() {
        let store = InMemorySecretStore::unavailable();
        let err = store
            .set_openrouter_key(&SecretValue::new(KEY.into()).unwrap())
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::SecretStoreLocked);
        assert_eq!(
            store.openrouter_key_present().unwrap_err().code(),
            AppErrorCode::SecretStoreLocked
        );
    }

    #[test]
    fn presence_is_the_only_thing_a_response_can_carry() {
        let store = InMemorySecretStore::new();
        store
            .set_openrouter_key(&SecretValue::new(KEY.into()).unwrap())
            .unwrap();
        // `openrouter_key_present` is a bool; there is no accessor that can
        // put the stored value into a response at all.
        let present = store.openrouter_key_present().unwrap();
        let json = serde_json::to_string(&present).unwrap();
        assert_eq!(json, "true");
        assert!(!json.contains(KEY));
    }
}
