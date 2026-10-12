//! The single place where the update endpoint and the pinned signing keys
//! live (SPEC v1.3 §16.5, §2.4).
//!
//! Nothing else in the workspace may hard-code an update URL or a key. The
//! values below are asserted verbatim by `tests/config_drift.rs`, so changing
//! a key or redirecting the update channel cannot pass review unnoticed —
//! that test is the human-visible tripwire for the "fake download redirect"
//! adversary listed in §16.1.

/// HTTPS endpoint serving the signed manifest envelope.
///
/// Reachability is a transport concern; in Local-only Mode this string is
/// never even formatted into a request (§16.2A, §22 item 15).
pub const UPDATE_MANIFEST_ENDPOINT: &str =
    "https://updates.plaudon.app/stable/update-manifest.v1.json";

/// Id of the current release signing key.
///
/// Key ids are date-stamped so that a rotation is a visible, reviewable
/// change rather than a silent swap of 32 opaque bytes.
pub const RELEASE_SIGNING_KEY_ID: &str = "plaudon-release-2026a";

/// Base64 (standard alphabet) of the 32-byte ed25519 public key identified by
/// [`RELEASE_SIGNING_KEY_ID`].
///
/// Only the public half exists in this repository; the private half is held
/// by the release process and is never committed, which is also why the
/// fixture suite signs with its own clearly-marked test keys instead.
pub const RELEASE_SIGNING_KEY_BASE64: &str = "55tblbXehVERBNYhjyBO9M9Ta3dV+zr6cv7NWwaMLEg=";

/// One pinned verification key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedKeyEntry {
    /// Stable key id, matched against `signing_key_id` in the envelope.
    pub id: &'static str,
    /// Base64 of the 32-byte ed25519 public key.
    pub public_key_base64: &'static str,
}

/// The complete pinned key set compiled into this build (§16.5).
///
/// A manifest naming any other key id is rejected with
/// [`crate::error::VerifyError::UnknownKey`] before a single package byte is
/// fetched. The set is a list rather than a single key so that a rotation can
/// publish the next key one release *before* it is used.
pub const EMBEDDED_TRUSTED_KEYS: &[TrustedKeyEntry] = &[TrustedKeyEntry {
    id: RELEASE_SIGNING_KEY_ID,
    public_key_base64: RELEASE_SIGNING_KEY_BASE64,
}];
