//! Source association identity (spec 6.3B, 7.0, 7.2, 7.10).
//!
//! An external recording identifier is never a global key. Identity is the
//! triple `(source_kind, source_account_scope, external_recording_id)`, so the
//! same vendor identifier seen under two accounts yields two distinct
//! associations and two distinct logical meetings.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{RecordingId, SourceId, SourceItemId};
use crate::time::Timestamp;

/// The mutually identifiable source kinds of spec 7.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Direct BLE transfer from a PLAUD device. Never uses PLAUD HTTP endpoints.
    PlaudDeviceBle,
    /// Read-only import from a mounted legacy PLAUD Note volume.
    PlaudDeviceUsb,
    /// The official PLAUD Cloud CLI connector.
    PlaudCloudCli,
    /// A file the user selected.
    LocalFile,
    /// A watched folder.
    WatchedFolder,
    /// A source kind this build does not understand.
    #[serde(other)]
    Unknown,
}

impl SourceKind {
    /// Human-facing provenance label (spec 7.0).
    #[must_use]
    pub const fn provenance_label(self) -> &'static str {
        match self {
            Self::PlaudDeviceBle => "Device BLE (compressed)",
            Self::PlaudDeviceUsb => "Device USB (original WAV where available)",
            Self::PlaudCloudCli => "PLAUD Cloud",
            Self::LocalFile => "Local file",
            Self::WatchedFolder => "Watched folder",
            Self::Unknown => "Unknown source",
        }
    }

    /// Whether this source kind performs application-initiated network calls.
    #[must_use]
    pub const fn uses_network(self) -> bool {
        matches!(self, Self::PlaudCloudCli)
    }
}

/// The account or device scope an external identifier belongs to.
///
/// Two PLAUD accounts, or two physical devices, are separate scopes even when
/// they report identical external identifiers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceAccountScope(String);

impl SourceAccountScope {
    /// Construct a scope from a stable, non-secret discriminator.
    #[must_use]
    pub fn new(scope: impl Into<String>) -> Self {
        Self(scope.into())
    }

    /// The scope discriminator.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceAccountScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A vendor-assigned recording identifier, verbatim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExternalRecordingId(String);

impl ExternalRecordingId {
    /// Wrap a vendor identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExternalRecordingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The globally unique key of a source association (spec 7.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceIdentityKey {
    /// Which kind of source produced the item.
    pub source_kind: SourceKind,
    /// Account or device scope the external identifier belongs to.
    pub account_scope: SourceAccountScope,
    /// Vendor identifier, when the source exposes a stable one.
    pub external_id: Option<ExternalRecordingId>,
}

impl SourceIdentityKey {
    /// Construct a key.
    #[must_use]
    pub fn new(
        source_kind: SourceKind,
        account_scope: SourceAccountScope,
        external_id: Option<ExternalRecordingId>,
    ) -> Self {
        Self { source_kind, account_scope, external_id }
    }

    /// Whether this key can be used for idempotent re-import.
    ///
    /// Without a stable external identifier the importer must fall back to
    /// content identity; a filename is never sufficient identity (spec 7.10).
    #[must_use]
    pub const fn is_stable(&self) -> bool {
        self.external_id.is_some()
    }
}

/// Import lifecycle of a source item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    /// Seen during enumeration, not yet acquired.
    Discovered,
    /// Acquisition in progress.
    Acquiring,
    /// Acquired, verified and committed.
    Imported,
    /// Acquisition failed; the item may be retried.
    Failed,
    /// The user declined to import this item.
    Skipped,
    /// Status not understood by this build.
    #[serde(other)]
    Unknown,
}

/// Remote-side state observed for a source item (spec 7.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteState {
    /// Visible at the source.
    Present,
    /// Reported as trashed at the source.
    Trashed,
    /// No longer enumerated by the source.
    Absent,
    /// Not established.
    #[serde(other)]
    Unknown,
}

impl RemoteState {
    /// Whether this remote state grants authority to delete local content.
    ///
    /// Spec 7.10: it never does.
    #[must_use]
    pub const fn grants_local_deletion(self) -> bool {
        false
    }
}

/// An association between a logical recording and one source-side item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAssociation {
    /// Row identity.
    pub id: SourceItemId,
    /// Configured source this item came from.
    pub source_id: SourceId,
    /// The identity triple of spec 7.2.
    pub identity: SourceIdentityKey,
    /// Logical recording this item belongs to.
    pub recording_id: RecordingId,
    /// Import lifecycle.
    pub import_status: ImportStatus,
    /// Remote-side state at last enumeration.
    pub remote_state: RemoteState,
    /// Remote version hint, when exposed.
    pub remote_version: Option<String>,
    /// Last time the item was enumerated.
    pub last_seen_at: Option<Timestamp>,
    /// Reason a local tombstone exists, if the user deleted the recording.
    pub tombstone_reason: Option<String>,
}

impl SourceAssociation {
    /// Whether a sync pass may recreate this item.
    ///
    /// Spec 7.10: a user deletion must not be silently undone by sync.
    #[must_use]
    pub const fn may_be_recreated_by_sync(&self) -> bool {
        self.tombstone_reason.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scope: &str, external: &str) -> SourceIdentityKey {
        SourceIdentityKey::new(
            SourceKind::PlaudCloudCli,
            SourceAccountScope::new(scope),
            Some(ExternalRecordingId::new(external)),
        )
    }

    #[test]
    fn the_same_external_id_under_two_accounts_is_two_identities() {
        assert_ne!(key("account-a", "rec-1"), key("account-b", "rec-1"));
    }

    #[test]
    fn the_same_external_id_from_two_source_kinds_is_two_identities() {
        let cli = key("account-a", "rec-1");
        let mut device = cli.clone();
        device.source_kind = SourceKind::PlaudDeviceBle;
        assert_ne!(cli, device);
    }

    #[test]
    fn a_key_without_an_external_id_is_not_stable() {
        let anonymous = SourceIdentityKey::new(
            SourceKind::LocalFile,
            SourceAccountScope::new("local"),
            None,
        );
        assert!(!anonymous.is_stable());
        assert!(key("a", "b").is_stable());
    }

    #[test]
    fn remote_state_never_grants_local_deletion() {
        for state in [RemoteState::Present, RemoteState::Trashed, RemoteState::Absent, RemoteState::Unknown] {
            assert!(!state.grants_local_deletion());
        }
    }

    #[test]
    fn unknown_source_kinds_decode_without_claiming_capability() {
        let kind: SourceKind = serde_json::from_str("\"telepathy\"").unwrap();
        assert_eq!(kind, SourceKind::Unknown);
        assert!(!kind.uses_network());
        assert_eq!(kind.provenance_label(), "Unknown source");
    }

    #[test]
    fn only_the_cloud_cli_source_uses_the_network() {
        assert!(SourceKind::PlaudCloudCli.uses_network());
        for kind in [SourceKind::PlaudDeviceBle, SourceKind::PlaudDeviceUsb, SourceKind::LocalFile] {
            assert!(!kind.uses_network(), "{kind:?} must not use the network");
        }
    }
}
