//! Credential access for the OpenRouter clients (spec 10.1, 16.2).
//!
//! The key is read from OS secure storage for the duration of one authenticated
//! network operation and is never copied into a log line, an error envelope, a
//! database column, an environment variable or a process argument. The only
//! thing a caller can do with a [`ApiKey`] is attach it to a request header; it
//! has no `Debug`, `Display`, `Serialize` or `Clone` that could leak it, and it
//! is zeroized on drop.

use std::sync::Arc;

use plaudon_security::secrets::{SecretScope, SecretStore, SecretStoreError};
use zeroize::Zeroizing;

/// The Authorization header name. Named here so the redaction tests can assert
/// against exactly the header the clients attach.
pub const AUTHORIZATION_HEADER: &str = "Authorization";

/// An OpenRouter API key held in memory for one operation.
///
/// Deliberately not `Debug`, not `Clone`, not `Serialize`.
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    /// Wrap a raw key.
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(Zeroizing::new(raw))
    }

    /// The `Authorization` header value. The only way the secret leaves this
    /// type, and the result is itself zeroized when dropped.
    #[must_use]
    pub fn bearer_header(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("Bearer {}", self.0.as_str()))
    }

    /// Whether the key is syntactically usable at all. Does not prove the
    /// provider accepts it, and deliberately says nothing about balance
    /// (spec 4.4(3)).
    #[must_use]
    pub fn is_non_empty(&self) -> bool {
        !self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Why a credential could not be produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// No key has been stored yet.
    #[error("no OpenRouter API key is stored")]
    Missing,
    /// The OS credential store refused or is locked. There is no plaintext
    /// fallback (ADR-0014).
    #[error("the OS credential store is unavailable")]
    StoreUnavailable {
        /// Non-secret rendering of the store error.
        detail: String,
    },
    /// A key exists but is empty or whitespace.
    #[error("the stored OpenRouter API key is empty")]
    Empty,
}

/// Source of the OpenRouter credential.
///
/// A trait so tests can supply a key without an OS keychain, and so the clients
/// never see the storage mechanism.
pub trait ApiKeySource: Send + Sync + std::fmt::Debug {
    /// Fetch the key for one operation.
    fn api_key(&self) -> Result<ApiKey, CredentialError>;

    /// Whether a key is present, without reading it.
    fn is_present(&self) -> bool {
        matches!(self.api_key(), Ok(key) if key.is_non_empty())
    }
}

/// The OS credential store, read through `plaudon-security`.
#[derive(Debug, Clone)]
pub struct VaultApiKey {
    store: Arc<dyn SecretStore>,
    scope: SecretScope,
}

impl VaultApiKey {
    /// Read the OpenRouter key from `store`.
    #[must_use]
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self {
            store,
            scope: SecretScope::OpenRouterApiKey,
        }
    }
}

impl ApiKeySource for VaultApiKey {
    fn api_key(&self) -> Result<ApiKey, CredentialError> {
        match self.store.load(&self.scope) {
            Ok(secret) => {
                let key = ApiKey::new(secret.expose().to_owned());
                if key.is_non_empty() {
                    Ok(key)
                } else {
                    Err(CredentialError::Empty)
                }
            }
            Err(SecretStoreError::NotFound) => Err(CredentialError::Missing),
            Err(error) => Err(CredentialError::StoreUnavailable {
                detail: error.to_string(),
            }),
        }
    }
}

/// A fixed key, for tests and for the opt-in real-provider probe.
#[derive(Debug, Clone)]
pub struct StaticApiKey(String);

impl StaticApiKey {
    /// Wrap a literal key.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }
}

impl ApiKeySource for StaticApiKey {
    fn api_key(&self) -> Result<ApiKey, CredentialError> {
        if self.0.trim().is_empty() {
            return Err(CredentialError::Empty);
        }
        Ok(ApiKey::new(self.0.clone()))
    }
}

/// A source with no key at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoApiKey;

impl ApiKeySource for NoApiKey {
    fn api_key(&self) -> Result<ApiKey, CredentialError> {
        Err(CredentialError::Missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_never_renders_itself() {
        let key = ApiKey::new("sk-or-v1-secret".to_owned());
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("secret"), "{rendered}");
        assert_eq!(rendered, "ApiKey(<redacted>)");
    }

    #[test]
    fn the_header_value_is_the_only_exit() {
        let key = ApiKey::new("sk-or-v1-secret".to_owned());
        assert_eq!(key.bearer_header().as_str(), "Bearer sk-or-v1-secret");
    }

    #[test]
    fn an_absent_key_is_a_typed_error_not_an_empty_string() {
        assert_eq!(NoApiKey.api_key().unwrap_err(), CredentialError::Missing);
        assert!(!NoApiKey.is_present());
        assert_eq!(
            StaticApiKey::new("   ").api_key().unwrap_err(),
            CredentialError::Empty
        );
    }
}
