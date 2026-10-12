//! D1.2 — OS credential storage for the `OpenRouter` key and device secrets
//! (spec §16.4, §13.1, §6.3A, §7.7, App. B.7, §22.9, §22.16).
//!
//! # The rule this module exists to enforce
//!
//! > "We will permit no plaintext fallback for direct-device credentials in
//! > production; an unavailable secure store yields an actionable locked state,
//! > not a silent downgrade." (§16.4)
//!
//! There is therefore **no file-backed production backend in this crate at
//! all** — not behind a feature, not behind a `cfg`, not in a debug profile.
//! The only backends that exist are [`macos::MacosKeychainStore`],
//! [`windows::WindowsCredentialStore`], the always-failing
//! [`UnsupportedSecretStore`], and a `#[cfg]`-gated in-memory store that cannot
//! be compiled into a release binary. "No plaintext fallback" is a property of
//! the type graph, not of a review checklist.
//!
//! # What crosses which boundary
//!
//! | Value | May live in | Must never live in |
//! |---|---|---|
//! | [`SecretString`] | process memory, the OS vault | SQLite, backups, logs, argv, env, the UI |
//! | [`SecretRef`] | SQLite, portable backups, logs | — |
//! | [`SecretPresence`] | a command response shown in the UI (§15.2 "status only") | — |
//!
//! §6.3A: "The UI will never receive raw bearer tokens […] or device secrets."
//! [`SecretStore::status`] is the accessor a command handler may surface to the
//! interface, and it structurally cannot return secret material.
//!
//! §7.7 / §16.4: "We will prohibit universal/global BLE tokens shared across
//! devices." [`SecretScope::DeviceAuthorization`] carries a validated
//! [`DeviceId`]; there is no variant without one, and wildcard/reserved ids are
//! rejected by [`DeviceId::new`], so a universal device token is not
//! representable in this type system.
//!
//! # Honest limits
//!
//! * In-process memory cannot be perfectly protected from privileged malware
//!   (§16.4). [`SecretString`] zeroizes on drop, which is a best-effort control:
//!   it cannot erase copies the allocator, the OS swap file or a core dump may
//!   already hold.
//! * A [`SecretRef::fingerprint`] is a truncated salted digest. It detects
//!   rotation. It is **not** a secrecy guarantee against an attacker who can
//!   already guess the plaintext — a guess can always be confirmed. It is
//!   adequate for the high-entropy values in scope here (API keys, device
//!   tokens) and is never used as an authenticator.
//! * We depend on the platform vault's own protections. We do not claim the
//!   macOS Keychain or Windows Credential Manager is unbreakable, only that we
//!   never write credentials anywhere else.

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{
    ErrorEnvelopeParts, SecurityError, COMMAND_ARGUMENT_INVALID, OPEN_API_KEY_SETTINGS,
    SECRET_NOT_FOUND, SECRET_STORE_FAILED, SECRET_STORE_LOCKED, UNLOCK_CREDENTIAL_STORE,
};

pub mod macos;
pub mod windows;

#[cfg(any(test, feature = "test-support"))]
pub mod memory;

#[cfg(any(test, feature = "test-support"))]
pub use memory::{FixedClock, InMemorySecretStore};

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Keychain/Credential-Manager service string. Reverse-DNS namespaced so our
/// entries are attributable in the user's vault UI and cannot collide with
/// another application's generic passwords.
pub const SECRET_SERVICE: &str = "ai.plaudon.plaudon";

/// Upper bound on a stored credential. An `OpenRouter` key is ~73 bytes and a
/// device authorization blob is tens of bytes; anything near this ceiling is a
/// caller bug, and the bound keeps a hostile value from being funnelled into
/// the OS vault.
pub const MAX_SECRET_BYTES: usize = 8 * 1024;

/// Upper bound on a device identifier (§7.7 identities are short opaque ids).
pub const MAX_DEVICE_ID_LEN: usize = 64;

/// Bytes of SHA-256 output retained in a [`SecretRef::fingerprint`].
pub const FINGERPRINT_BYTES: usize = 16;

/// Length of the hex-encoded fingerprint, i.e. `2 * FINGERPRINT_BYTES`.
pub const FINGERPRINT_HEX_LEN: usize = FINGERPRINT_BYTES * 2;

/// Domain separator mixed into every fingerprint, so a digest from this
/// construction can never be confused with one from another part of the app.
const FINGERPRINT_DOMAIN: &[u8] = b"plaudon.secret-fingerprint.v1";

/// Field separator inside digest inputs; `0x1f` cannot occur in a service or
/// account string, which makes the concatenation unambiguous.
const FIELD_SEPARATOR: u8 = 0x1f;

/// Device identifiers that must never be accepted, because each of them would
/// express "one credential for every device" — the exact thing §16.4 prohibits.
/// Compared case-insensitively.
const RESERVED_DEVICE_IDS: [&str; 8] = [
    "*",
    "all",
    "any",
    "global",
    "default",
    "shared",
    "universal",
    "none",
];

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// Injected time source. Decision and record-stamping code takes a `Clock` so
/// tests are deterministic and never read the wall clock.
pub trait Clock: fmt::Debug + Send + Sync {
    /// Unix epoch seconds.
    fn now_unix(&self) -> i64;
}

/// The production clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// SecretString
// ---------------------------------------------------------------------------

/// A credential held in process memory.
///
/// The type is deliberately hostile to accidental disclosure:
///
/// * no `Display`, no `serde::Serialize`, no `Clone`;
/// * `Debug` prints `SecretString(<redacted>)`, so a `#[derive(Debug)]` on any
///   struct that contains one stays safe;
/// * the only way to read the plaintext is [`SecretString::expose`], whose name
///   makes every call site grep-able and reviewable;
/// * the buffer is zeroized on drop.
///
/// Zeroization is best effort. It cannot reach copies already made by the
/// allocator, by `String` reallocation before the value reached this type, by
/// OS swap, or by a core dump (§16.4).
pub struct SecretString {
    inner: Zeroizing<String>,
}

impl SecretString {
    /// Wrap a credential.
    ///
    /// Prefer moving an owned `String` in: passing a `&str` necessarily leaves
    /// the caller's copy in place for this type to be unable to clear.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            inner: Zeroizing::new(value.into()),
        }
    }

    /// Wrap bytes read back from an OS vault, zeroizing them either way.
    ///
    /// Returns [`SecretStoreError::Backend`] if the stored bytes are not UTF-8,
    /// which means something other than this crate wrote the entry.
    pub fn from_utf8(mut bytes: Vec<u8>) -> Result<Self, SecretStoreError> {
        let parsed = std::str::from_utf8(&bytes).map(str::to_owned);
        bytes.zeroize();
        match parsed {
            Ok(text) => Ok(Self::new(text)),
            Err(_) => Err(SecretStoreError::Backend {
                detail: "stored credential is not valid UTF-8".to_owned(),
            }),
        }
    }

    /// The single, deliberately-named plaintext accessor.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.inner
    }

    /// The plaintext as bytes, for vault APIs that take a byte blob.
    #[must_use]
    pub fn expose_bytes(&self) -> &[u8] {
        self.inner.as_bytes()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Compare two secrets without an early exit on the first differing byte.
    ///
    /// The running time depends on the shorter of the two lengths but not on
    /// *where* the values diverge, so a comparison cannot be used as an oracle
    /// to recover the secret byte by byte. The lengths themselves are not
    /// hidden; that is an accepted limit, not an oversight.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        ct_eq_bytes(self.expose_bytes(), other.expose_bytes())
    }
}

/// Constant-time-in-content byte comparison, shared by secrets and fingerprints.
fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    let mut diff = u8::from(a.len() != b.len());
    let shared = a.len().min(b.len());
    for i in 0..shared {
        diff |= a[i] ^ b[i];
    }
    std::hint::black_box(diff) == 0
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

// ---------------------------------------------------------------------------
// Scopes
// ---------------------------------------------------------------------------

/// Why a device identifier was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    #[error("device identifier is empty")]
    Empty,
    #[error("device identifier exceeds {MAX_DEVICE_ID_LEN} characters")]
    TooLong,
    #[error("device identifier contains characters outside [A-Za-z0-9:_-]")]
    IllegalCharacter,
    #[error("device identifier is reserved and would denote a shared credential")]
    Reserved,
}

impl SecurityError for ScopeError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        ErrorEnvelopeParts::new(COMMAND_ARGUMENT_INVALID, self.to_string())
    }
}

/// A validated device identifier.
///
/// The inner string is private and the only constructor validates, so an
/// invalid or wildcard device id does not exist anywhere in the program — not
/// just "is rejected at the boundary".
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    /// Validate and wrap a device identifier.
    ///
    /// Accepts non-empty `[A-Za-z0-9:_-]{1,MAX_DEVICE_ID_LEN}` that is not one
    /// of [`RESERVED_DEVICE_IDS`]. The charset excludes `.` and `/`, so path
    /// traversal shapes such as `../x` are rejected before they can reach a
    /// keychain account string.
    ///
    /// The reserved check runs *before* the charset check so that `*` reports
    /// the reason that matters — "this would be a shared credential" — rather
    /// than an incidental "illegal character".
    pub fn new(raw: &str) -> Result<Self, ScopeError> {
        if raw.is_empty() {
            return Err(ScopeError::Empty);
        }
        if raw.len() > MAX_DEVICE_ID_LEN {
            return Err(ScopeError::TooLong);
        }
        if RESERVED_DEVICE_IDS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(raw))
        {
            return Err(ScopeError::Reserved);
        }
        if !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
        {
            return Err(ScopeError::IllegalCharacter);
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({})", self.0)
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a stored secret is for.
///
/// Scopes are closed: a caller cannot invent a new kind of credential, and a
/// device credential is always bound to one verified device identity (§7.7).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretScope {
    /// The user's `OpenRouter` API key (§10.1, §15.2).
    OpenRouterApiKey,
    /// Authorization material for one specific PLAUD device (§7.7, App. B.5).
    DeviceAuthorization { device_id: DeviceId },
}

impl SecretScope {
    /// Build a device-authorization scope, validating the identifier.
    pub fn device(device_id: &str) -> Result<Self, ScopeError> {
        Ok(Self::DeviceAuthorization {
            device_id: DeviceId::new(device_id)?,
        })
    }

    /// The OS vault service string. One namespace for the whole application.
    #[must_use]
    pub fn service(&self) -> &'static str {
        SECRET_SERVICE
    }

    /// The OS vault account string. Distinct per scope, so no two scopes can
    /// ever resolve to the same vault entry.
    #[must_use]
    pub fn account(&self) -> String {
        match self {
            SecretScope::OpenRouterApiKey => "openrouter.api_key".to_owned(),
            SecretScope::DeviceAuthorization { device_id } => {
                format!("device.{device_id}.auth")
            }
        }
    }

    /// The device this scope is bound to, if any.
    #[must_use]
    pub fn device_id(&self) -> Option<&str> {
        match self {
            SecretScope::OpenRouterApiKey => None,
            SecretScope::DeviceAuthorization { device_id } => Some(device_id.as_str()),
        }
    }

    /// A log- and UI-safe label. Contains no secret material.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            SecretScope::OpenRouterApiKey => "OpenRouter API key".to_owned(),
            SecretScope::DeviceAuthorization { device_id } => {
                format!("device authorization for {device_id}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SecretRef
// ---------------------------------------------------------------------------

/// The opaque reference that §13.1 and App. B.7 allow to be persisted.
///
/// This is the **only** serializable carrier in this module. SQLite columns,
/// portable backups and diagnostics hold a `SecretRef`; the secret itself never
/// leaves the OS vault and process memory.
///
/// `fingerprint` is the first [`FINGERPRINT_BYTES`] bytes of
/// `SHA-256(scope_salt || 0x1f || secret)`, hex encoded, where `scope_salt` is
/// itself `SHA-256(domain || 0x1f || service || 0x1f || account)`. It is enough
/// to tell "the key was rotated" from "same key as before"; it carries no
/// prefix, length or substring of the secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretRef {
    /// OS vault service string this credential lives under.
    pub service: String,
    /// OS vault account string this credential lives under.
    pub account: String,
    /// Salted, truncated SHA-256 digest; rotation detector, never a credential.
    pub fingerprint: String,
    /// Unix epoch seconds at which *this reference value* was produced.
    ///
    /// For a reference returned by [`SecretStore::store`] that is the moment
    /// the credential was written. A backend that can recall the original
    /// write time (the in-memory test store) returns it from
    /// [`SecretStore::status`]; the macOS and Windows backends do not read the
    /// vault's own creation attribute, so a reference they produce in `status`
    /// is stamped at observation time. Compare [`SecretRef::fingerprint`], not
    /// this field, to decide whether a credential changed.
    pub created_at_unix: i64,
}

impl SecretRef {
    /// Build the reference recorded when a secret is written to the vault.
    #[must_use]
    pub fn new(scope: &SecretScope, secret: &SecretString, created_at_unix: i64) -> Self {
        Self {
            service: scope.service().to_owned(),
            account: scope.account(),
            fingerprint: Self::fingerprint_of(scope, secret),
            created_at_unix,
        }
    }

    /// Compute the scope-salted fingerprint of a secret.
    #[must_use]
    pub fn fingerprint_of(scope: &SecretScope, secret: &SecretString) -> String {
        let salt = scope_salt(scope);
        let mut hasher = Sha256::new();
        hasher.update(salt);
        hasher.update([FIELD_SEPARATOR]);
        hasher.update(secret.expose_bytes());
        let digest = hasher.finalize();
        hex::encode(&digest[..FINGERPRINT_BYTES])
    }

    /// Whether `secret` is the same value this reference was created from.
    /// Used to answer "has the user rotated the key?" without reading the vault
    /// entry back into a comparison that leaks timing on content.
    #[must_use]
    pub fn matches(&self, scope: &SecretScope, secret: &SecretString) -> bool {
        ct_eq_bytes(
            self.fingerprint.as_bytes(),
            Self::fingerprint_of(scope, secret).as_bytes(),
        )
    }
}

/// Per-scope salt, so the same API key stored under two scopes fingerprints
/// differently and a fingerprint cannot be correlated across scopes.
fn scope_salt(scope: &SecretScope) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update([FIELD_SEPARATOR]);
    hasher.update(scope.service().as_bytes());
    hasher.update([FIELD_SEPARATOR]);
    hasher.update(scope.account().as_bytes());
    hasher.finalize().into()
}

/// What [`SecretStore::status`] may tell the UI: whether a credential exists,
/// and its opaque reference. Never the credential (§6.3A, §15.2 "status only").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretPresence {
    pub present: bool,
    pub reference: Option<SecretRef>,
}

impl SecretPresence {
    #[must_use]
    pub fn absent() -> Self {
        Self {
            present: false,
            reference: None,
        }
    }

    #[must_use]
    pub fn present(reference: SecretRef) -> Self {
        Self {
            present: true,
            reference: Some(reference),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Which credential store produced a result. Used only for attribution in
/// messages; the variants are a closed set of `&'static str` labels so no user
/// data can enter an error string through this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreKind {
    MacosKeychain,
    WindowsCredentialManager,
    /// A `#[cfg]`-gated store that exists only in tests and test-support builds.
    InMemoryTestStore,
    /// A target with no OS credential store integration.
    Unsupported,
}

impl StoreKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            StoreKind::MacosKeychain => "macOS Keychain",
            StoreKind::WindowsCredentialManager => "Windows Credential Manager",
            StoreKind::InMemoryTestStore => "in-memory test store",
            StoreKind::Unsupported => "unsupported platform credential store",
        }
    }
}

impl fmt::Display for StoreKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a store is unavailable. A closed enum rather than free text, so an
/// unavailability message structurally cannot carry credential material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// This build target has no OS credential store integration. There is no
    /// plaintext alternative: the operation fails (§16.4).
    UnsupportedPlatform,
    /// The vault exists but refused access (no default keychain, no logon
    /// session, missing entitlement). The numeric status is an OS error code.
    StoreNotAccessible { os_status: i32 },
}

impl fmt::Display for UnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnavailableReason::UnsupportedPlatform => {
                f.write_str("this platform has no supported OS credential store")
            }
            UnavailableReason::StoreNotAccessible { os_status } => {
                write!(
                    f,
                    "the OS credential store refused access (status {os_status})"
                )
            }
        }
    }
}

/// Failure of a credential-store operation.
///
/// Every variant is constructed from fixed vocabulary plus non-secret labels
/// and numeric OS status codes, so neither `Display` nor the derived `Debug`
/// can carry credential material into a log or a crash report (§16.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretStoreError {
    /// The vault is present but locked, or the user declined the unlock prompt.
    #[error("the {store} is locked")]
    Locked { store: StoreKind },
    /// The vault could not be reached at all. Still reported to the user as an
    /// actionable locked state — never as permission to fall back to plaintext.
    #[error("the {store} is unavailable: {reason}")]
    Unavailable {
        store: StoreKind,
        reason: UnavailableReason,
    },
    /// No entry exists for the requested scope.
    #[error("no credential is stored for this scope")]
    NotFound,
    /// The vault reported a hard failure.
    #[error("credential store operation failed: {detail}")]
    Backend { detail: String },
    /// The scope itself was invalid, so no vault call was attempted.
    #[error("invalid credential scope: {0}")]
    InvalidScope(#[from] ScopeError),
}

impl SecretStoreError {
    /// Build a backend failure from a fixed operation label and an OS status.
    #[must_use]
    pub fn backend(store: StoreKind, operation: &'static str, os_status: i32) -> Self {
        Self::Backend {
            detail: format!("{store} {operation} returned status {os_status}"),
        }
    }

    /// Whether this is the "locked or unavailable" family the UI renders as a
    /// single actionable state (§16.4).
    #[must_use]
    pub const fn is_locked_state(&self) -> bool {
        matches!(
            self,
            SecretStoreError::Locked { .. } | SecretStoreError::Unavailable { .. }
        )
    }
}

impl SecurityError for SecretStoreError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        match self {
            // `Unavailable` deliberately shares `Locked`'s code and action:
            // §16.4 requires an unreachable store to surface as an actionable
            // locked state and never as a silent downgrade to weaker storage.
            SecretStoreError::Locked { .. } | SecretStoreError::Unavailable { .. } => {
                ErrorEnvelopeParts::new(SECRET_STORE_LOCKED, self.to_string())
                    .retryable(true)
                    .action(UNLOCK_CREDENTIAL_STORE)
            }
            SecretStoreError::NotFound => {
                ErrorEnvelopeParts::new(SECRET_NOT_FOUND, self.to_string())
                    .action(OPEN_API_KEY_SETTINGS)
            }
            SecretStoreError::Backend { .. } => {
                ErrorEnvelopeParts::new(SECRET_STORE_FAILED, self.to_string())
            }
            SecretStoreError::InvalidScope(inner) => inner.envelope_parts(),
        }
    }
}

// ---------------------------------------------------------------------------
// The store trait
// ---------------------------------------------------------------------------

/// The credential-storage boundary.
///
/// Note what is **absent**: there is no `list`, no `export`, no `path`, and no
/// method that hands a secret to a serializable value. The widest read is
/// [`SecretStore::load`], which returns a [`SecretString`] that cannot be
/// serialized or displayed.
pub trait SecretStore: fmt::Debug + Send + Sync {
    /// Which backend this is, for attribution in errors and diagnostics.
    fn kind(&self) -> StoreKind;

    /// Write (or replace) the credential for `scope` and return the opaque
    /// reference the database may persist.
    fn store(
        &self,
        scope: &SecretScope,
        secret: &SecretString,
    ) -> Result<SecretRef, SecretStoreError>;

    /// Read the credential for `scope`.
    fn load(&self, scope: &SecretScope) -> Result<SecretString, SecretStoreError>;

    /// Remove the credential for `scope`. Removing a credential that is not
    /// there is an error, so callers cannot mistake "nothing happened" for
    /// "revoked".
    fn delete(&self, scope: &SecretScope) -> Result<(), SecretStoreError>;

    /// Report whether a credential exists, plus its opaque reference.
    ///
    /// This is the only method whose result may be serialized to the frontend
    /// (§15.2: `set_openrouter_key` / `remove_openrouter_key` return "status
    /// only"). It may need the vault unlocked, and returns
    /// [`SecretStoreError::Locked`] if it is not — it never reports "absent"
    /// to paper over a locked vault, because that would invite the caller to
    /// re-prompt for a credential that is in fact already stored.
    fn status(&self, scope: &SecretScope) -> Result<SecretPresence, SecretStoreError>;
}

/// Validate a secret before it is handed to a platform vault API.
pub(crate) fn validate_secret(secret: &SecretString) -> Result<(), SecretStoreError> {
    if secret.is_empty() {
        return Err(SecretStoreError::Backend {
            detail: "refusing to store an empty credential".to_owned(),
        });
    }
    if secret.len() > MAX_SECRET_BYTES {
        return Err(SecretStoreError::Backend {
            detail: format!("credential exceeds {MAX_SECRET_BYTES} bytes"),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unsupported-platform backend
// ---------------------------------------------------------------------------

/// The backend [`os_store`] returns on a target with no OS credential vault
/// integration.
///
/// It is a real, always-failing store rather than a plaintext file: §16.4 gives
/// us exactly two options for an unavailable secure store — an actionable
/// locked state, or a silent downgrade — and the second is prohibited. Keeping
/// this type compiled on every target is what lets the test suite prove the
/// behaviour instead of asserting it only on an exotic CI runner.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedSecretStore;

impl UnsupportedSecretStore {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    const fn unavailable() -> SecretStoreError {
        SecretStoreError::Unavailable {
            store: StoreKind::Unsupported,
            reason: UnavailableReason::UnsupportedPlatform,
        }
    }
}

impl SecretStore for UnsupportedSecretStore {
    fn kind(&self) -> StoreKind {
        StoreKind::Unsupported
    }

    fn store(
        &self,
        _scope: &SecretScope,
        _secret: &SecretString,
    ) -> Result<SecretRef, SecretStoreError> {
        Err(Self::unavailable())
    }

    fn load(&self, _scope: &SecretScope) -> Result<SecretString, SecretStoreError> {
        Err(Self::unavailable())
    }

    fn delete(&self, _scope: &SecretScope) -> Result<(), SecretStoreError> {
        Err(Self::unavailable())
    }

    fn status(&self, _scope: &SecretScope) -> Result<SecretPresence, SecretStoreError> {
        Err(Self::unavailable())
    }
}

// ---------------------------------------------------------------------------
// Platform selection
// ---------------------------------------------------------------------------

/// The platform credential store, with the production clock.
#[must_use]
pub fn os_store() -> Box<dyn SecretStore> {
    os_store_with_clock(Arc::new(SystemClock))
}

/// The platform credential store with an injected clock.
///
/// macOS → Keychain, Windows → Credential Manager, anything else →
/// [`UnsupportedSecretStore`], which fails every operation with
/// [`SecretStoreError::Unavailable`]. There is no third branch, and in
/// particular no file-backed one, in any build profile.
#[must_use]
pub fn os_store_with_clock(clock: Arc<dyn Clock>) -> Box<dyn SecretStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacosKeychainStore::new(clock))
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsCredentialStore::new(clock))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        drop(clock);
        Box::new(UnsupportedSecretStore::new())
    }
}
