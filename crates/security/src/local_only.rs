//! D1.1 (part 2) — Local-only Mode as persisted application state, and the
//! cloud-synced-folder rule (spec §1.2(15), §16.2A, App. B.7, §5.4).
//!
//! [`crate::outbound::OutboundPolicy`] is the enforcement point. This module is
//! the *state* around it: the user's persisted choice, the explicit confirmation
//! required to leave Local-only Mode, the rebuild of the policy from settings,
//! and the honest classification of watched folders that a cloud-sync client may
//! upload behind our back.
//!
//! App. B.7 gives the user two independent controls — **import route** and
//! **processing destination** — and states that Local-only Mode *overrides both*.
//! That override lives here: [`NetworkPolicySettings::effective_policy`] ignores
//! route and destination preferences entirely when Local-only Mode is on.
//!
//! This module deliberately does not touch SQLite. Persistence implements
//! [`NetworkSettingsStore`]; `crates/persistence` owns the table.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::outbound::{
    ApprovalSource, EffectiveNetworkPolicy, NetworkGrant, NetworkMode, OutboundPolicy,
};

/// Where recordings may be acquired from (App. B.7 "Device import route").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportRoute {
    DirectBle,
    LegacyUsb,
    PlaudCloud,
    Files,
}

impl ImportRoute {
    /// Whether the route needs this application to initiate internet traffic.
    #[must_use]
    pub const fn requires_app_egress(self) -> bool {
        matches!(self, ImportRoute::PlaudCloud)
    }
}

/// Where inference runs (App. B.7 "Processing destination").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingDestination {
    LocalModels,
    AppleNative,
    OpenRouter,
}

impl ProcessingDestination {
    #[must_use]
    pub const fn requires_app_egress(self) -> bool {
        matches!(self, ProcessingDestination::OpenRouter)
    }
}

/// The user's persisted network choice.
///
/// Leaving Local-only Mode requires an explicit confirmation, which is captured
/// as a [`NetworkGrant`] rather than a bare boolean, so the decision is auditable
/// (§16.2A: "enabling egress is an explicit, recorded decision").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkPolicySettings {
    local_only: bool,
    grant: Option<NetworkGrant>,
    import_route: ImportRoute,
    processing_destination: ProcessingDestination,
    watched_folders: Vec<WatchedFolder>,
}

impl Default for NetworkPolicySettings {
    /// A fresh installation is local-only and file-based: nothing reaches the
    /// network until the user says so.
    fn default() -> Self {
        Self {
            local_only: true,
            grant: None,
            import_route: ImportRoute::Files,
            processing_destination: ProcessingDestination::LocalModels,
            watched_folders: Vec::new(),
        }
    }
}

/// Why a change to the network setting was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("leaving Local-only Mode requires an explicit confirmation from the user")]
    ConfirmationRequired,
    #[error("the recorded approval has no disclosure version")]
    MissingDisclosureVersion,
    #[error("settings could not be persisted: {0}")]
    Persistence(String),
}

impl crate::error::SecurityError for SettingsError {
    fn envelope_parts(&self) -> crate::error::ErrorEnvelopeParts {
        use crate::error::{ErrorEnvelopeParts, DISABLE_LOCAL_ONLY_MODE, EGRESS_DENIED};
        match self {
            SettingsError::ConfirmationRequired => ErrorEnvelopeParts::new(
                EGRESS_DENIED,
                "Turning off Local-only Mode needs an explicit confirmation.",
            )
            .action(DISABLE_LOCAL_ONLY_MODE),
            SettingsError::MissingDisclosureVersion => ErrorEnvelopeParts::new(
                EGRESS_DENIED,
                "The privacy disclosure version was missing, so the change was refused.",
            ),
            SettingsError::Persistence(_) => ErrorEnvelopeParts::new(
                EGRESS_DENIED,
                "The network setting could not be saved, so nothing was changed.",
            )
            .retryable(true),
        }
    }
}

impl NetworkPolicySettings {
    #[must_use]
    pub fn local_only() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_local_only(&self) -> bool {
        self.local_only
    }

    #[must_use]
    pub fn import_route(&self) -> ImportRoute {
        self.import_route
    }

    #[must_use]
    pub fn processing_destination(&self) -> ProcessingDestination {
        self.processing_destination
    }

    #[must_use]
    pub fn watched_folders(&self) -> &[WatchedFolder] {
        &self.watched_folders
    }

    #[must_use]
    pub fn grant(&self) -> Option<&NetworkGrant> {
        self.grant.as_ref()
    }

    /// Turn Local-only Mode on. Always allowed, never needs confirmation: the
    /// safe direction is free.
    pub fn enable_local_only(&mut self) {
        self.local_only = true;
        self.grant = None;
    }

    /// Turn Local-only Mode off.
    ///
    /// `confirmed` models the §15.2 `set_local_only_mode` contract ("enabled +
    /// explicit confirmation"). Without it the change is refused.
    pub fn disable_local_only(
        &mut self,
        confirmed: bool,
        grant: NetworkGrant,
    ) -> Result<(), SettingsError> {
        if !confirmed {
            return Err(SettingsError::ConfirmationRequired);
        }
        if grant.disclosure_version.trim().is_empty() {
            return Err(SettingsError::MissingDisclosureVersion);
        }
        self.local_only = false;
        self.grant = Some(grant);
        Ok(())
    }

    /// Set the import route. Preferences are stored even when Local-only Mode
    /// overrides them, so turning the mode off restores the user's choice.
    pub fn set_import_route(&mut self, route: ImportRoute) {
        self.import_route = route;
    }

    pub fn set_processing_destination(&mut self, destination: ProcessingDestination) {
        self.processing_destination = destination;
    }

    pub fn set_watched_folders(&mut self, folders: Vec<WatchedFolder>) {
        self.watched_folders = folders;
    }

    /// Build the enforcement object from the persisted state.
    ///
    /// This is the only place the two are connected, so the UI badge and the
    /// Rust boundary cannot disagree (§6.3(15): "'Local-only' is enforced at the
    /// Rust network policy boundary, not merely a UI badge").
    #[must_use]
    pub fn effective_policy(&self) -> OutboundPolicy {
        match (self.local_only, self.grant.clone()) {
            (false, Some(grant)) => OutboundPolicy::network_permitted(grant),
            // Local-only on, or network nominally permitted without a recorded
            // grant: deny. Fail closed.
            _ => OutboundPolicy::local_only(),
        }
    }

    /// What the user is told. Combines the enforced policy with the route and
    /// destination overrides and the watched-folder disclosure.
    #[must_use]
    pub fn describe(&self) -> LocalOnlyStatus {
        let policy = self.effective_policy();
        let effective = policy.describe();

        let route_overridden = self.local_only && self.import_route.requires_app_egress();
        let destination_overridden =
            self.local_only && self.processing_destination.requires_app_egress();

        let folders = self
            .watched_folders
            .iter()
            .map(|folder| folder.disclosure(self.local_only))
            .collect();

        LocalOnlyStatus {
            mode: effective.mode,
            selected_import_route: self.import_route,
            effective_import_route: if route_overridden {
                ImportRoute::Files
            } else {
                self.import_route
            },
            import_route_overridden: route_overridden,
            selected_processing_destination: self.processing_destination,
            effective_processing_destination: if destination_overridden {
                ProcessingDestination::LocalModels
            } else {
                self.processing_destination
            },
            processing_destination_overridden: destination_overridden,
            watched_folders: folders,
            network: effective,
        }
    }
}

/// The complete, user-facing picture of Local-only Mode. Returned by the
/// `set_local_only_mode` and `get_capabilities` commands of
/// `plaudon_app::command` (§15.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LocalOnlyStatus {
    pub mode: NetworkMode,
    pub selected_import_route: ImportRoute,
    pub effective_import_route: ImportRoute,
    /// True when Local-only Mode overrode the user's route choice (App. B.7).
    pub import_route_overridden: bool,
    pub selected_processing_destination: ProcessingDestination,
    pub effective_processing_destination: ProcessingDestination,
    pub processing_destination_overridden: bool,
    pub watched_folders: Vec<WatchedFolderDisclosure>,
    pub network: EffectiveNetworkPolicy,
}

/// Persistence seam. `crates/persistence` implements this; this crate never
/// opens a database.
pub trait NetworkSettingsStore: Send + Sync {
    fn load(&self) -> Result<NetworkPolicySettings, SettingsError>;
    fn save(&self, settings: &NetworkPolicySettings) -> Result<(), SettingsError>;
}

/// Loads the setting, enforces the confirmation rule, persists the change, and
/// hands out a fresh [`OutboundPolicy`] after every transition.
#[derive(Debug)]
pub struct NetworkPolicyService<S: NetworkSettingsStore + std::fmt::Debug> {
    store: S,
}

impl<S: NetworkSettingsStore + std::fmt::Debug> NetworkPolicyService<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Current state plus the enforcement object built from it.
    pub fn current(&self) -> Result<(NetworkPolicySettings, Arc<OutboundPolicy>), SettingsError> {
        let settings = self.store.load()?;
        let policy = Arc::new(settings.effective_policy());
        Ok((settings, policy))
    }

    /// The `set_local_only_mode` command body (§15.2).
    ///
    /// Returns the effective enforced policy, which the UI renders; it never
    /// returns a bare boolean, so a UI that forgets to re-read state cannot show
    /// a stale badge.
    pub fn set_local_only_mode(
        &self,
        enabled: bool,
        confirmed: bool,
        grant: Option<NetworkGrant>,
    ) -> Result<LocalOnlyStatus, SettingsError> {
        let mut settings = self.store.load()?;
        if enabled {
            settings.enable_local_only();
        } else {
            let grant = grant.unwrap_or_else(|| {
                NetworkGrant::new(ApprovalSource::UserSettingsConfirmation, 0, String::new())
            });
            settings.disable_local_only(confirmed, grant)?;
        }
        self.store.save(&settings)?;
        Ok(settings.describe())
    }
}

// ---------------------------------------------------------------------------
// Cloud-synced watched folders
// ---------------------------------------------------------------------------

/// A folder the user asked us to watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedFolder {
    pub path: PathBuf,
    pub backing: CloudSyncBacking,
}

impl WatchedFolder {
    /// Classify a folder by inspecting its path. The detection is a *hint*: we
    /// state it as such, because a sync client can be configured anywhere.
    #[must_use]
    pub fn detect(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let backing = detect_cloud_sync(&path);
        Self { path, backing }
    }

    #[must_use]
    pub fn disclosure(&self, local_only: bool) -> WatchedFolderDisclosure {
        let network_dependent = self.backing != CloudSyncBacking::None;
        WatchedFolderDisclosure {
            path: self.path.display().to_string(),
            backing: self.backing,
            // §16.2A: while Local-only Mode is on, such folders are disabled or
            // clearly identified as potentially network-dependent. We disable
            // them, which is the stronger of the two options, and say why.
            disabled: local_only && network_dependent,
            potentially_network_dependent: network_dependent,
            explanation: (match (local_only, network_dependent) {
                (true, true) => {
                    "This folder looks like it is synchronized by a cloud client. \
                     While Local-only Mode is on it is not watched, because that \
                     client could upload files independently of this application."
                }
                (false, true) => {
                    "This folder looks like it is synchronized by a cloud client. \
                     Files placed here may be uploaded by that client, which is \
                     outside this application's control."
                }
                (_, false) => "This folder does not appear to be cloud-synchronized.",
            })
            .to_owned(),
        }
    }
}

/// What the disclosure tells the user about one watched folder.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WatchedFolderDisclosure {
    pub path: String,
    pub backing: CloudSyncBacking,
    pub disabled: bool,
    pub potentially_network_dependent: bool,
    pub explanation: String,
}

/// A recognised consumer sync client. Detection is heuristic and labelled as
/// such; `Unknown` means "markers present but we do not recognise the vendor".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudSyncBacking {
    None,
    ICloudDrive,
    Dropbox,
    OneDrive,
    GoogleDrive,
    Box,
    Unknown,
}

/// Path-shape detection for the common desktop sync clients on macOS and
/// Windows. Pure function over the path so it is testable without a filesystem.
#[must_use]
pub fn detect_cloud_sync(path: &Path) -> CloudSyncBacking {
    let lowered = path.to_string_lossy().to_ascii_lowercase();
    // macOS iCloud Drive lives under the Mobile Documents container; Windows
    // uses iCloudDrive under the user profile.
    if lowered.contains("/library/mobile documents") || lowered.contains("\\icloud") {
        return CloudSyncBacking::ICloudDrive;
    }
    if lowered.contains("/library/cloudstorage/googledrive")
        || lowered.contains("google drive")
        || lowered.contains("googledrive")
    {
        return CloudSyncBacking::GoogleDrive;
    }
    if lowered.contains("/library/cloudstorage/onedrive") || lowered.contains("onedrive") {
        return CloudSyncBacking::OneDrive;
    }
    if lowered.contains("/library/cloudstorage/box") || lowered.contains("/box sync") {
        return CloudSyncBacking::Box;
    }
    if lowered.contains("dropbox") {
        return CloudSyncBacking::Dropbox;
    }
    if lowered.contains("/library/cloudstorage/") {
        // A provider we do not recognise, mounted through the macOS File
        // Provider API. Say "unknown", not "none".
        return CloudSyncBacking::Unknown;
    }
    CloudSyncBacking::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbound::DenyReason;
    use parking_lot::Mutex;

    #[derive(Debug, Default)]
    struct MemoryStore {
        inner: Mutex<NetworkPolicySettings>,
    }

    impl NetworkSettingsStore for MemoryStore {
        fn load(&self) -> Result<NetworkPolicySettings, SettingsError> {
            Ok(self.inner.lock().clone())
        }
        fn save(&self, settings: &NetworkPolicySettings) -> Result<(), SettingsError> {
            *self.inner.lock() = settings.clone();
            Ok(())
        }
    }

    fn grant() -> NetworkGrant {
        NetworkGrant::new(
            ApprovalSource::UserSettingsConfirmation,
            1_700_000_000,
            "privacy-v1",
        )
    }

    #[test]
    fn a_fresh_installation_is_local_only_and_file_based() {
        let settings = NetworkPolicySettings::default();
        assert!(settings.is_local_only());
        assert_eq!(settings.import_route(), ImportRoute::Files);
        assert_eq!(
            settings.processing_destination(),
            ProcessingDestination::LocalModels
        );
        assert_eq!(settings.effective_policy().mode(), NetworkMode::LocalOnly);
    }

    #[test]
    fn leaving_local_only_mode_requires_explicit_confirmation() {
        let mut settings = NetworkPolicySettings::default();
        assert_eq!(
            settings.disable_local_only(false, grant()).unwrap_err(),
            SettingsError::ConfirmationRequired
        );
        assert!(settings.is_local_only());
        settings.disable_local_only(true, grant()).unwrap();
        assert!(!settings.is_local_only());
        assert_eq!(
            settings.effective_policy().mode(),
            NetworkMode::NetworkPermitted
        );
    }

    #[test]
    fn a_grant_without_a_disclosure_version_is_refused() {
        let mut settings = NetworkPolicySettings::default();
        let bad = NetworkGrant::new(ApprovalSource::UserSettingsConfirmation, 0, "   ");
        assert_eq!(
            settings.disable_local_only(true, bad).unwrap_err(),
            SettingsError::MissingDisclosureVersion
        );
        assert!(settings.is_local_only());
    }

    #[test]
    fn local_only_mode_overrides_both_route_and_destination() {
        let mut settings = NetworkPolicySettings::default();
        settings.set_import_route(ImportRoute::PlaudCloud);
        settings.set_processing_destination(ProcessingDestination::OpenRouter);

        let status = settings.describe();
        assert!(status.import_route_overridden);
        assert_eq!(status.effective_import_route, ImportRoute::Files);
        assert!(status.processing_destination_overridden);
        assert_eq!(
            status.effective_processing_destination,
            ProcessingDestination::LocalModels
        );
        // The user's selection is preserved, not silently rewritten.
        assert_eq!(status.selected_import_route, ImportRoute::PlaudCloud);
        assert_eq!(
            status.selected_processing_destination,
            ProcessingDestination::OpenRouter
        );
    }

    #[test]
    fn turning_local_only_off_restores_the_users_route_and_destination() {
        let mut settings = NetworkPolicySettings::default();
        settings.set_import_route(ImportRoute::PlaudCloud);
        settings.set_processing_destination(ProcessingDestination::OpenRouter);
        settings.disable_local_only(true, grant()).unwrap();

        let status = settings.describe();
        assert!(!status.import_route_overridden);
        assert_eq!(status.effective_import_route, ImportRoute::PlaudCloud);
        assert!(!status.processing_destination_overridden);
        assert_eq!(
            status.effective_processing_destination,
            ProcessingDestination::OpenRouter
        );
    }

    #[test]
    fn network_permitted_without_a_recorded_grant_fails_closed() {
        // Construct the inconsistent state directly, as a corrupted settings row
        // would produce it.
        let settings = NetworkPolicySettings {
            local_only: false,
            grant: None,
            ..NetworkPolicySettings::default()
        };
        assert_eq!(settings.effective_policy().mode(), NetworkMode::LocalOnly);
    }

    #[test]
    fn service_round_trips_the_setting_and_returns_the_enforced_policy() {
        let service = NetworkPolicyService::new(MemoryStore::default());

        let (_, policy) = service.current().unwrap();
        assert_eq!(policy.mode(), NetworkMode::LocalOnly);

        let status = service
            .set_local_only_mode(false, true, Some(grant()))
            .unwrap();
        assert_eq!(status.mode, NetworkMode::NetworkPermitted);

        let (_, policy) = service.current().unwrap();
        assert_eq!(policy.mode(), NetworkMode::NetworkPermitted);

        let status = service.set_local_only_mode(true, false, None).unwrap();
        assert_eq!(status.mode, NetworkMode::LocalOnly);
        assert!(!status.network.cloud_cli_launchable);
    }

    #[test]
    fn service_refuses_to_disable_without_confirmation() {
        let service = NetworkPolicyService::new(MemoryStore::default());
        assert_eq!(
            service
                .set_local_only_mode(false, false, Some(grant()))
                .unwrap_err(),
            SettingsError::ConfirmationRequired
        );
        let (settings, policy) = service.current().unwrap();
        assert!(settings.is_local_only());
        assert_eq!(policy.mode(), NetworkMode::LocalOnly);
    }

    #[test]
    fn cloud_sync_detection_covers_the_common_clients() {
        let cases = [
            (
                "/Users/a/Library/Mobile Documents/com~apple~CloudDocs/Notes",
                CloudSyncBacking::ICloudDrive,
            ),
            (
                "/Users/a/Library/CloudStorage/GoogleDrive-a@b.com/My Drive",
                CloudSyncBacking::GoogleDrive,
            ),
            (
                "/Users/a/Library/CloudStorage/OneDrive-Personal/Audio",
                CloudSyncBacking::OneDrive,
            ),
            (
                "/Users/a/Library/CloudStorage/Box-Box/Audio",
                CloudSyncBacking::Box,
            ),
            ("/Users/a/Dropbox/Recordings", CloudSyncBacking::Dropbox),
            (
                "/Users/a/Library/CloudStorage/Mega-Personal/x",
                CloudSyncBacking::Unknown,
            ),
            ("C:\\Users\\a\\OneDrive\\Audio", CloudSyncBacking::OneDrive),
            ("/Users/a/Music/Recordings", CloudSyncBacking::None),
            ("/Volumes/External/Audio", CloudSyncBacking::None),
        ];
        for (path, expected) in cases {
            assert_eq!(detect_cloud_sync(Path::new(path)), expected, "for {path}");
        }
    }

    #[test]
    fn a_cloud_synced_watched_folder_is_disabled_and_explained_in_local_only_mode() {
        let mut settings = NetworkPolicySettings::default();
        settings.set_watched_folders(vec![
            WatchedFolder::detect("/Users/a/Dropbox/Recordings"),
            WatchedFolder::detect("/Users/a/Music/Recordings"),
        ]);

        let status = settings.describe();
        let dropbox = &status.watched_folders[0];
        assert_eq!(dropbox.backing, CloudSyncBacking::Dropbox);
        assert!(dropbox.disabled);
        assert!(dropbox.potentially_network_dependent);
        assert!(dropbox.explanation.contains("Local-only Mode"));

        let plain = &status.watched_folders[1];
        assert_eq!(plain.backing, CloudSyncBacking::None);
        assert!(!plain.disabled);
        assert!(!plain.potentially_network_dependent);
    }

    #[test]
    fn a_cloud_synced_folder_is_still_labelled_when_local_only_is_off() {
        let mut settings = NetworkPolicySettings::default();
        settings.set_watched_folders(vec![WatchedFolder::detect("/Users/a/Dropbox/Recordings")]);
        settings.disable_local_only(true, grant()).unwrap();

        let status = settings.describe();
        let dropbox = &status.watched_folders[0];
        assert!(
            !dropbox.disabled,
            "the folder works, but the dependency is disclosed"
        );
        assert!(dropbox.potentially_network_dependent);
        assert!(dropbox
            .explanation
            .contains("outside this application's control"));
    }

    #[test]
    fn status_serializes_for_the_command_response() {
        let mut settings = NetworkPolicySettings::default();
        settings.set_watched_folders(vec![WatchedFolder::detect("/Users/a/Dropbox/x")]);
        let json = serde_json::to_string(&settings.describe()).unwrap();
        assert!(json.contains("\"mode\":\"local_only\""));
        assert!(json.contains("\"backing\":\"dropbox\""));
        assert!(json.contains("not_guaranteed"));
    }

    #[test]
    fn local_only_status_still_denies_at_the_policy_even_with_a_cloud_route_selected() {
        let mut settings = NetworkPolicySettings::default();
        settings.set_import_route(ImportRoute::PlaudCloud);
        let policy = settings.effective_policy();
        let request = crate::outbound::EgressRequest::new(
            crate::outbound::Subsystem::PlaudCloudCli,
            crate::outbound::Destination::subprocess("plaud"),
            crate::outbound::EgressPurpose::AccountSync,
            crate::outbound::PayloadClass::Metadata,
        );
        assert_eq!(
            policy.authorize(&request).unwrap_err().reason,
            DenyReason::LocalOnlyMode
        );
    }
}
