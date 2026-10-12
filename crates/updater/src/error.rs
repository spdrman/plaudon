//! Typed updater errors.
//!
//! Per SPEC v1.3 §15.4 there is exactly one user-facing error taxonomy:
//! `plaudon_domain::error::{AppError, AppErrorCode}`. This module defines the
//! updater's internal rejections and maps every one of them onto that
//! taxonomy; it never defines a second envelope. Per §16.5 ("updater manifests
//! and packages must pass cryptographic verification") every rejection is
//! fail-closed: the caller is told *which* check failed, and nothing on disk
//! has changed.
//!
//! Messages never carry the update endpoint, a signed URL or raw signature
//! material, so an envelope built from them stays inside the §15.4 redaction
//! rules.

use plaudon_domain::error::{AppError, AppErrorCode};

use crate::manifest::Version;

/// Why an update artifact was refused.
///
/// Every variant is a hard rejection (§16.5, §18.5 "reject unsigned or
/// corrupted application updates"). No variant is retryable with the same
/// bytes, because the same bytes will fail the same check again.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The manifest envelope carried no detached signature at all.
    #[error("update manifest carries no detached signature")]
    UnsignedManifest,

    /// The manifest named a key id that is not in the pinned trusted set
    /// (§16.5; the set lives in [`crate::config`]).
    #[error("update manifest is signed by unpinned key id `{key_id}`")]
    UnknownKey {
        /// Key id as claimed by the manifest. Never a key value.
        key_id: String,
    },

    /// A detached ed25519 signature did not verify against the pinned key.
    #[error("ed25519 signature over the {subject} did not verify")]
    SignatureMismatch {
        /// Which artifact failed: the manifest bytes or the package bytes.
        subject: SignatureSubject,
    },

    /// The package bytes do not hash to the digest named by the manifest.
    #[error("package digest mismatch: manifest declared {expected}, bytes hash to {actual}")]
    DigestMismatch {
        /// Lowercase hex SHA-256 declared by the manifest.
        expected: String,
        /// Lowercase hex SHA-256 of the bytes we actually hold.
        actual: String,
    },

    /// Fewer package bytes arrived than the manifest declares. Separated from
    /// [`VerifyError::DigestMismatch`] so an interrupted download is
    /// distinguishable from a hostile substitution (§18.6A "BLE transfer" /
    /// "Archive" rows apply the same rule to updates).
    #[error("package truncated: manifest declared {expected} bytes, {actual} present")]
    TruncatedPackage {
        /// Byte length declared by the manifest.
        expected: u64,
        /// Byte length actually present.
        actual: u64,
    },

    /// The manifest envelope or its inner document could not be parsed, or it
    /// violated a structural invariant (§18.5 fuzzing of output schemas).
    #[error("malformed update manifest: {0}")]
    MalformedManifest(String),

    /// Applying the artifact would move the installation below the floor it is
    /// allowed to run at (§16.5 "rollback must not weaken signature
    /// checking"; the same rule forbids using rollback as a downgrade hole).
    #[error("downgrade blocked: {offered} is not permitted from {installed} (floor {floor})")]
    DowngradeBlocked {
        /// Version the artifact would install.
        offered: Version,
        /// Version currently active.
        installed: Version,
        /// Lowest version the installation may run.
        floor: Version,
    },

    /// The manifest targets a different platform/architecture than this build.
    #[error("manifest targets {offered}, this build is {host}")]
    UnsupportedTarget {
        /// Target named by the manifest.
        offered: String,
        /// Target of the running build.
        host: String,
    },
}

/// Which artifact an ed25519 signature covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureSubject {
    /// The exact bytes of the manifest document inside its envelope.
    Manifest,
    /// The package payload bytes.
    Package,
}

impl std::fmt::Display for SignatureSubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest => f.write_str("manifest"),
            Self::Package => f.write_str("package"),
        }
    }
}

impl VerifyError {
    /// Stable code from the shared closed set (§15.4).
    ///
    /// Signature, digest and truncation failures are all integrity failures;
    /// a parse failure is malformed output; a blocked downgrade and a foreign
    /// target are invalid arguments for *this* installation.
    #[must_use]
    pub const fn code(&self) -> AppErrorCode {
        match self {
            Self::UnsignedManifest
            | Self::UnknownKey { .. }
            | Self::SignatureMismatch { .. }
            | Self::DigestMismatch { .. }
            | Self::TruncatedPackage { .. } => AppErrorCode::IntegrityFailed,
            Self::MalformedManifest(_) => AppErrorCode::MalformedOutput,
            Self::DowngradeBlocked { .. } | Self::UnsupportedTarget { .. } => {
                AppErrorCode::InvalidArgument
            }
        }
    }
}

impl From<VerifyError> for AppError {
    /// Maps a rejection onto the shared taxonomy. The detail is the `Display`
    /// form, which by construction contains no key material, no signature and
    /// no endpoint.
    fn from(error: VerifyError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

/// Errors raised by the install/rollback state machine.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// An artifact was refused by verification (§16.5).
    #[error(transparent)]
    Verify(#[from] VerifyError),

    /// The update check was refused before any network activity (§16.2A,
    /// §22 item 15).
    #[error("update check suppressed: {0}")]
    Suppressed(crate::policy::SuppressionReason),

    /// The update server could not be reached or answered unusably. The URL
    /// is deliberately absent from the message (§15.4).
    #[error(transparent)]
    Transport(#[from] crate::policy::TransportError),

    /// A simulated or real interruption stopped the state machine. The
    /// installation is still launchable; see [`crate::install::Installer::open`].
    #[error("install interrupted at {0}")]
    Interrupted(crate::install::InterruptPoint),

    /// Rollback was requested but no previous version is retained.
    #[error("no retained previous version to roll back to")]
    NoRetainedVersion,

    /// The on-disk journal could not be read or was structurally invalid.
    #[error("update journal is unreadable: {0}")]
    JournalCorrupt(String),

    /// A slot that the journal claims is installable has no usable payload.
    #[error("installed slot `{slot}` is incomplete")]
    SlotIncomplete {
        /// Slot name recorded in the journal.
        slot: String,
    },

    /// Filesystem failure while staging, swapping or journalling.
    #[error("filesystem failure during {context}")]
    Io {
        /// Static description of the step that failed.
        context: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },

    /// The journal could not be encoded or decoded as JSON.
    #[error("could not encode the update journal")]
    Encoding {
        /// Underlying serialization error.
        #[source]
        source: serde_json::Error,
    },
}

impl UpdateError {
    /// Convenience constructor for filesystem failures.
    pub fn io(context: &'static str, source: std::io::Error) -> Self {
        Self::Io { context, source }
    }

    /// `true` when re-running the identical operation may succeed.
    ///
    /// Verification rejections never are: the same bytes fail the same check.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Verify(_)
            | Self::Suppressed(_)
            | Self::NoRetainedVersion
            | Self::Encoding { .. } => false,
            Self::Transport(_)
            | Self::Interrupted(_)
            | Self::JournalCorrupt(_)
            | Self::SlotIncomplete { .. }
            | Self::Io { .. } => true,
        }
    }

    /// Stable code from the shared closed set (§15.4).
    #[must_use]
    pub fn code(&self) -> AppErrorCode {
        match self {
            Self::Verify(inner) => inner.code(),
            Self::Suppressed(_) => AppErrorCode::LocalOnlyMode,
            Self::Transport(_) => AppErrorCode::SourceUnavailable,
            Self::Interrupted(_) => AppErrorCode::Canceled,
            Self::NoRetainedVersion => AppErrorCode::RecordNotFound,
            Self::JournalCorrupt(_) | Self::SlotIncomplete { .. } => AppErrorCode::IntegrityFailed,
            Self::Io { .. } | Self::Encoding { .. } => AppErrorCode::StorageFailure,
        }
    }
}

impl From<UpdateError> for AppError {
    /// Maps updater errors onto the shared taxonomy (§15.4).
    fn from(error: UpdateError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}
