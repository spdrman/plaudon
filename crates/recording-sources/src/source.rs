//! The generic recording-source contract (spec 6.5, 7.6).

use std::path::Path;

use async_trait::async_trait;
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::media::{MediaIdentity, QualityClass};
use plaudon_domain::source::{SourceIdentityKey, SourceKind};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};

/// What a source can do, as observed rather than assumed (spec 6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCapabilities {
    /// Contract version of this payload.
    pub payload_version: u32,
    /// Validation status of this source integration.
    pub status: CapabilityStatus,
    /// Kind of source being described.
    pub source_kind: SourceKind,
    /// Whether the source can enumerate items at all.
    pub supports_listing: Tri,
    /// Whether enumeration is paginated with a usable cursor.
    pub supports_pagination: Tri,
    /// Whether items carry a stable external identifier.
    pub supports_stable_external_ids: Tri,
    /// Whether the source reports a remote version or modification marker.
    pub supports_remote_versions: Tri,
    /// Whether an interrupted acquisition can be resumed safely.
    pub supports_resume: Tri,
    /// Whether the source exposes vendor-generated transcripts or summaries.
    pub supports_vendor_artifacts: Tri,
    /// Maximum items a single enumeration page returns, when known.
    pub page_size: Known<u32>,
}

impl SourceCapabilities {
    /// Contract version understood by this build.
    pub const SUPPORTED_PAYLOAD_VERSION: u32 = 1;

    /// A capability set that claims nothing for `source_kind`.
    #[must_use]
    pub fn unknown(source_kind: SourceKind) -> Self {
        Self {
            payload_version: Self::SUPPORTED_PAYLOAD_VERSION,
            status: CapabilityStatus::Unknown,
            source_kind,
            supports_listing: Tri::Unknown,
            supports_pagination: Tri::Unknown,
            supports_stable_external_ids: Tri::Unknown,
            supports_remote_versions: Tri::Unknown,
            supports_resume: Tri::Unknown,
            supports_vendor_artifacts: Tri::Unknown,
            page_size: Known::Unknown,
        }
    }

    /// Effective status after contract-version negotiation.
    #[must_use]
    pub fn effective_status(&self) -> CapabilityStatus {
        if self.payload_version == Self::SUPPORTED_PAYLOAD_VERSION {
            self.status
        } else {
            CapabilityStatus::Unknown
        }
    }

    /// Whether the application may enumerate this source automatically.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        self.effective_status().is_selectable() && self.supports_listing.satisfies()
    }
}

/// One enumerated item at a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceItem {
    /// Identity triple for this item (spec 7.2).
    pub identity: SourceIdentityKey,
    /// Display name reported by the source. Advisory metadata only.
    pub display_name: String,
    /// Capture time reported by the source, when available.
    pub captured_at: Option<Timestamp>,
    /// Duration reported by the source, when available.
    pub duration_ms: Option<u64>,
    /// Byte length reported by the source. Untrusted until verified.
    pub declared_byte_length: Option<u64>,
    /// Remote version or modification marker, when available.
    pub remote_version: Option<String>,
    /// Fidelity class the source claims for this item.
    pub quality_class: QualityClass,
}

/// One page of enumeration results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePage {
    /// Items on this page.
    pub items: Vec<SourceItem>,
    /// Cursor for the next page, or `None` when enumeration is complete.
    pub next_cursor: Option<String>,
}

/// A verified asset written to a staging path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquiredAsset {
    /// Identity of the acquired bytes.
    pub identity: MediaIdentity,
    /// Fidelity class of the acquired bytes.
    pub quality_class: QualityClass,
    /// Path the bytes were staged to.
    pub staged_path: std::path::PathBuf,
}

/// Stable source errors (spec 7.6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The vendor command line tool is not installed.
    #[error("the vendor command line tool is not installed")]
    CliMissing,
    /// The detected tool version is outside the validated table.
    #[error("the vendor command line tool version is not validated")]
    CliUnsupported,
    /// An interactive login is required.
    #[error("authentication is required")]
    AuthenticationRequired,
    /// The user cancelled the login.
    #[error("authentication was cancelled")]
    AuthCanceled,
    /// The source is unreachable or reported itself unavailable.
    #[error("the source is unavailable")]
    SourceUnavailable,
    /// The remote side rate limited us.
    #[error("the source rate limited the request")]
    RemoteRateLimited,
    /// A temporary link expired.
    #[error("the download link expired")]
    LinkExpired,
    /// The requested item does not exist at the source.
    #[error("the requested item was not found at the source")]
    AssetNotFound,
    /// Source output did not match the validated contract.
    #[error("the source returned malformed output")]
    MalformedOutput,
    /// A transfer stopped before all bytes arrived.
    #[error("the download was interrupted")]
    DownloadInterrupted,
    /// Received bytes failed verification.
    #[error("the acquired bytes failed verification")]
    IntegrityFailed,
    /// The operation was cancelled.
    #[error("the operation was cancelled")]
    Cancelled,
    /// Local-only Mode switched the source off; nothing was launched or sent.
    #[error("local-only mode switched the source off")]
    LocalOnlyMode,
    /// The vendor tool is installed but the runtime it needs (for the PLAUD
    /// CLI, Node.js) could not be found, so the tool cannot start.
    #[error("the vendor command line tool is installed but its runtime is missing")]
    CliRuntimeMissing,
}

impl From<SourceError> for AppError {
    fn from(error: SourceError) -> Self {
        let code = match error {
            SourceError::CliMissing => AppErrorCode::CliMissing,
            SourceError::CliUnsupported => AppErrorCode::CliUnsupported,
            SourceError::AuthenticationRequired => AppErrorCode::AuthenticationRequired,
            SourceError::AuthCanceled => AppErrorCode::AuthCanceled,
            SourceError::SourceUnavailable => AppErrorCode::SourceUnavailable,
            SourceError::RemoteRateLimited => AppErrorCode::RemoteRateLimited,
            SourceError::LinkExpired => AppErrorCode::LinkExpired,
            SourceError::AssetNotFound => AppErrorCode::AssetNotFound,
            SourceError::MalformedOutput => AppErrorCode::MalformedOutput,
            SourceError::DownloadInterrupted => AppErrorCode::DownloadInterrupted,
            SourceError::IntegrityFailed => AppErrorCode::IntegrityFailed,
            SourceError::Cancelled => AppErrorCode::Canceled,
            SourceError::LocalOnlyMode => AppErrorCode::LocalOnlyMode,
            // The fix is a setup step on this computer (install the runtime),
            // which is what `CLI_MISSING` and its action already say. The
            // closed code set stays unchanged; the diagnostics copy names the
            // runtime.
            SourceError::CliRuntimeMissing => AppErrorCode::CliMissing,
        };
        Self::new(code, error.to_string())
    }
}

/// A source of recordings (spec 6.5).
#[async_trait]
pub trait RecordingSource: Send + Sync {
    /// Observed capabilities of this source.
    async fn capabilities(&self) -> SourceCapabilities;

    /// Enumerate one page of items.
    async fn list(&self, cursor: Option<&str>) -> Result<SourcePage, SourceError>;

    /// Acquire `item` into `target`, which must be a staging path.
    ///
    /// Implementations must never modify or delete the source bytes
    /// (spec 7.10, ADR-010).
    async fn acquire(&self, item: &SourceItem, target: &Path)
        -> Result<AcquiredAsset, SourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_source_capability_set_is_not_selectable() {
        let caps = SourceCapabilities::unknown(SourceKind::PlaudCloudCli);
        assert!(!caps.is_selectable());
        assert!(!caps.supports_pagination.satisfies());
    }

    #[test]
    fn a_supported_source_still_needs_declared_listing() {
        let mut caps = SourceCapabilities::unknown(SourceKind::PlaudCloudCli);
        caps.status = CapabilityStatus::Supported;
        assert!(
            !caps.is_selectable(),
            "listing support is not implied by validation status"
        );
        caps.supports_listing = Tri::Yes;
        assert!(caps.is_selectable());
    }

    #[test]
    fn an_unknown_payload_version_downgrades_the_status() {
        let mut caps = SourceCapabilities::unknown(SourceKind::PlaudCloudCli);
        caps.status = CapabilityStatus::Supported;
        caps.supports_listing = Tri::Yes;
        caps.payload_version = 7;
        assert_eq!(caps.effective_status(), CapabilityStatus::Unknown);
        assert!(!caps.is_selectable());
    }

    #[test]
    fn every_source_error_maps_to_its_spec_7_6_code() {
        let pairs = [
            (SourceError::CliMissing, AppErrorCode::CliMissing),
            (SourceError::CliUnsupported, AppErrorCode::CliUnsupported),
            (
                SourceError::AuthenticationRequired,
                AppErrorCode::AuthenticationRequired,
            ),
            (SourceError::AuthCanceled, AppErrorCode::AuthCanceled),
            (
                SourceError::SourceUnavailable,
                AppErrorCode::SourceUnavailable,
            ),
            (
                SourceError::RemoteRateLimited,
                AppErrorCode::RemoteRateLimited,
            ),
            (SourceError::LinkExpired, AppErrorCode::LinkExpired),
            (SourceError::AssetNotFound, AppErrorCode::AssetNotFound),
            (SourceError::MalformedOutput, AppErrorCode::MalformedOutput),
            (
                SourceError::DownloadInterrupted,
                AppErrorCode::DownloadInterrupted,
            ),
            (SourceError::IntegrityFailed, AppErrorCode::IntegrityFailed),
            (SourceError::Cancelled, AppErrorCode::Canceled),
            (SourceError::LocalOnlyMode, AppErrorCode::LocalOnlyMode),
            (SourceError::CliRuntimeMissing, AppErrorCode::CliMissing),
        ];
        for (error, expected) in pairs {
            assert_eq!(AppError::from(error.clone()).code(), expected, "{error:?}");
        }
    }
}
