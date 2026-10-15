//! Local-only Mode as persisted state: the refusal side (§1.2(15), §16.2A,
//! App. B.7, §15.2).
//!
//! The inline tests in `src/local_only.rs` cover the happy transitions against
//! an infallible in-memory store. This suite drives what happens when the store
//! itself fails, when the caller omits the grant, and the parts of the
//! cloud-sync classifier and the route/destination override table that a
//! success path never reaches.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use plaudon_security::error::SecurityError;
use plaudon_security::local_only::{
    detect_cloud_sync, CloudSyncBacking, ImportRoute, NetworkPolicyService, NetworkPolicySettings,
    NetworkSettingsStore, ProcessingDestination, SettingsError, WatchedFolder,
};
use plaudon_security::outbound::{ApprovalSource, DenyReason, NetworkGrant, NetworkMode};

fn grant() -> NetworkGrant {
    NetworkGrant::new(
        ApprovalSource::UserSettingsConfirmation,
        1_700_000_000,
        "privacy-v1",
    )
}

/// A store whose failures are injectable, so the error arms of the service are
/// reachable without a database.
#[derive(Debug, Default)]
struct FlakyStore {
    settings: Mutex<NetworkPolicySettings>,
    fail_load: Mutex<Option<String>>,
    fail_save: Mutex<Option<String>>,
    saves: Arc<Mutex<usize>>,
}

impl FlakyStore {
    fn failing_load(detail: &str) -> Self {
        let store = Self::default();
        *store.fail_load.lock() = Some(detail.to_owned());
        store
    }

    fn failing_save(detail: &str) -> Self {
        let store = Self::default();
        *store.fail_save.lock() = Some(detail.to_owned());
        store
    }

    /// A handle on the write counter that survives moving the store into the
    /// service, which takes ownership of it.
    fn save_counter(&self) -> Arc<Mutex<usize>> {
        Arc::clone(&self.saves)
    }
}

impl NetworkSettingsStore for FlakyStore {
    fn load(&self) -> Result<NetworkPolicySettings, SettingsError> {
        match self.fail_load.lock().clone() {
            Some(detail) => Err(SettingsError::Persistence(detail)),
            None => Ok(self.settings.lock().clone()),
        }
    }

    fn save(&self, settings: &NetworkPolicySettings) -> Result<(), SettingsError> {
        if let Some(detail) = self.fail_save.lock().clone() {
            return Err(SettingsError::Persistence(detail));
        }
        *self.settings.lock() = settings.clone();
        *self.saves.lock() += 1;
        Ok(())
    }
}

/// A store that cannot be read refuses both `current` and the setter, and the
/// refusal is reported as a retryable persistence failure, never as a silent
/// fall back to "network permitted".
#[test]
fn a_store_that_cannot_be_read_refuses_every_entry_point() {
    let service = NetworkPolicyService::new(FlakyStore::failing_load("disk i/o error"));

    let err = service.current().unwrap_err();
    assert_eq!(err, SettingsError::Persistence("disk i/o error".to_owned()));
    assert_eq!(
        err.to_string(),
        "settings could not be persisted: disk i/o error"
    );

    let parts = err.envelope_parts();
    assert_eq!(parts.code.as_str(), "EGRESS_DENIED");
    assert!(parts.retryable, "a persistence failure is worth retrying");
    assert_eq!(parts.action, None);
    assert!(parts.message.contains("could not be saved"));
    assert!(
        !parts.message.contains("disk i/o error"),
        "the internal detail must not reach the user-facing sentence: {}",
        parts.message
    );

    assert_eq!(
        service.set_local_only_mode(true, false, None).unwrap_err(),
        SettingsError::Persistence("disk i/o error".to_owned())
    );
    assert_eq!(
        service
            .set_local_only_mode(false, true, Some(grant()))
            .unwrap_err(),
        SettingsError::Persistence("disk i/o error".to_owned())
    );
}

/// A store that cannot be written leaves the previous state in force: nothing
/// is reported as changed when the change was not recorded.
#[test]
fn a_store_that_cannot_be_written_reports_the_failure_and_changes_nothing() {
    let service = NetworkPolicyService::new(FlakyStore::failing_save("read-only volume"));
    let err = service
        .set_local_only_mode(false, true, Some(grant()))
        .unwrap_err();
    assert_eq!(
        err,
        SettingsError::Persistence("read-only volume".to_owned())
    );

    let (settings, policy) = service.current().unwrap();
    assert!(settings.is_local_only());
    assert_eq!(policy.mode(), NetworkMode::LocalOnly);
}

/// Disabling Local-only Mode without supplying a grant is refused on the
/// disclosure version, because the synthesised fallback grant deliberately has
/// an empty one (§4.4(5)).
#[test]
fn disabling_without_a_grant_is_refused_on_the_missing_disclosure_version() {
    let store = FlakyStore::default();
    let service = NetworkPolicyService::new(store);

    assert_eq!(
        service.set_local_only_mode(false, true, None).unwrap_err(),
        SettingsError::MissingDisclosureVersion
    );
    // Confirmation is still checked first.
    assert_eq!(
        service.set_local_only_mode(false, false, None).unwrap_err(),
        SettingsError::ConfirmationRequired
    );

    let (settings, _) = service.current().unwrap();
    assert!(settings.is_local_only());
}

/// Nothing is persisted on a refusal, and exactly one write happens on success.
#[test]
fn a_refused_transition_writes_nothing_and_an_accepted_one_writes_once() {
    let store = FlakyStore::default();
    let saves = store.save_counter();
    let service = NetworkPolicyService::new(store);

    assert!(service.set_local_only_mode(false, false, None).is_err());
    assert_eq!(*saves.lock(), 0, "a refused transition must not persist");

    assert!(service.set_local_only_mode(false, true, None).is_err());
    assert_eq!(*saves.lock(), 0, "a blank disclosure must not persist");

    let status = service
        .set_local_only_mode(false, true, Some(grant()))
        .unwrap();
    assert_eq!(status.mode, NetworkMode::NetworkPermitted);
    assert_eq!(*saves.lock(), 1);

    // Re-enabling the safe direction needs neither confirmation nor a grant.
    let status = service.set_local_only_mode(true, false, None).unwrap();
    assert_eq!(status.mode, NetworkMode::LocalOnly);
    assert_eq!(*saves.lock(), 2);
}

/// Every whitespace-only disclosure version is refused, and the recorded grant
/// is only installed once the version survives the check.
#[test]
fn a_blank_disclosure_version_is_refused_in_every_whitespace_form() {
    for blank in ["", " ", "\t", "\n", "   \t \n "] {
        let mut settings = NetworkPolicySettings::local_only();
        let bad = NetworkGrant::new(ApprovalSource::OnboardingChoice, 0, blank);
        assert_eq!(
            settings.disable_local_only(true, bad).unwrap_err(),
            SettingsError::MissingDisclosureVersion,
            "{blank:?} must not count as a disclosure version"
        );
        assert!(settings.is_local_only());
        assert!(settings.grant().is_none());
    }

    let mut settings = NetworkPolicySettings::local_only();
    settings.disable_local_only(true, grant()).unwrap();
    assert!(!settings.is_local_only());
    assert_eq!(
        settings.grant().map(|g| g.disclosure_version.as_str()),
        Some("privacy-v1")
    );

    // Re-enabling the safe direction drops the recorded grant, so a later
    // inconsistent write cannot resurrect egress.
    settings.enable_local_only();
    assert!(settings.is_local_only());
    assert!(settings.grant().is_none());
    assert_eq!(settings.effective_policy().mode(), NetworkMode::LocalOnly);
}

/// Each refusal maps to its own §15.4 envelope: confirmation and disclosure are
/// not retryable, persistence is.
#[test]
fn each_settings_refusal_carries_its_own_envelope() {
    let confirmation = SettingsError::ConfirmationRequired.envelope_parts();
    assert_eq!(confirmation.code.as_str(), "EGRESS_DENIED");
    assert!(!confirmation.retryable);
    assert_eq!(
        confirmation
            .action
            .map(plaudon_security::ErrorAction::as_str),
        Some("DISABLE_LOCAL_ONLY_MODE")
    );
    assert_eq!(
        SettingsError::ConfirmationRequired.to_string(),
        "leaving Local-only Mode requires an explicit confirmation from the user"
    );

    let disclosure = SettingsError::MissingDisclosureVersion.envelope_parts();
    assert!(!disclosure.retryable);
    assert_eq!(disclosure.action, None);
    assert_eq!(
        SettingsError::MissingDisclosureVersion.to_string(),
        "the recorded approval has no disclosure version"
    );

    let persistence = SettingsError::Persistence("x".to_owned()).envelope_parts();
    assert!(persistence.retryable);
    assert_eq!(persistence.action, None);
}

/// Only the two cloud-dependent selections are overridden; the other route and
/// destination values pass through untouched in both modes.
#[test]
fn only_the_network_dependent_route_and_destination_are_overridden() {
    assert!(ImportRoute::PlaudCloud.requires_app_egress());
    for route in [
        ImportRoute::DirectBle,
        ImportRoute::LegacyUsb,
        ImportRoute::Files,
    ] {
        assert!(!route.requires_app_egress(), "{route:?}");

        let mut settings = NetworkPolicySettings::local_only();
        settings.set_import_route(route);
        let status = settings.describe();
        assert!(!status.import_route_overridden);
        assert_eq!(status.effective_import_route, route);
    }

    assert!(ProcessingDestination::OpenRouter.requires_app_egress());
    for destination in [
        ProcessingDestination::LocalModels,
        ProcessingDestination::AppleNative,
    ] {
        assert!(!destination.requires_app_egress(), "{destination:?}");

        let mut settings = NetworkPolicySettings::local_only();
        settings.set_processing_destination(destination);
        let status = settings.describe();
        assert!(!status.processing_destination_overridden);
        assert_eq!(status.effective_processing_destination, destination);
    }
}

/// The accessors report the stored selection even while it is overridden, so
/// turning the mode off restores the user's choice rather than a default.
#[test]
fn the_stored_selection_survives_the_override() {
    let mut settings = NetworkPolicySettings::local_only();
    settings.set_import_route(ImportRoute::PlaudCloud);
    settings.set_processing_destination(ProcessingDestination::OpenRouter);
    assert_eq!(settings.import_route(), ImportRoute::PlaudCloud);
    assert_eq!(
        settings.processing_destination(),
        ProcessingDestination::OpenRouter
    );
    assert!(
        settings.watched_folders().is_empty(),
        "no folder was configured"
    );

    let status = settings.describe();
    assert!(status.import_route_overridden);
    assert!(status.processing_destination_overridden);
    assert_eq!(status.effective_import_route, ImportRoute::Files);
    assert_eq!(
        status.effective_processing_destination,
        ProcessingDestination::LocalModels
    );
}

/// Every sync-client shape the classifier recognises, including the ones a
/// macOS-only happy path never reaches, plus the explicit "unknown provider"
/// state that must not be reported as "none".
#[test]
fn the_cloud_sync_classifier_covers_every_recognised_shape() {
    let cases: [(&str, CloudSyncBacking); 16] = [
        (
            "/Users/a/Library/Mobile Documents/com~apple~CloudDocs/x",
            CloudSyncBacking::ICloudDrive,
        ),
        (
            "C:\\Users\\a\\iCloudDrive\\Audio",
            CloudSyncBacking::ICloudDrive,
        ),
        (
            "/Users/a/Library/CloudStorage/GoogleDrive-a@b.com/My Drive",
            CloudSyncBacking::GoogleDrive,
        ),
        ("/Users/a/Google Drive/Audio", CloudSyncBacking::GoogleDrive),
        ("/Users/a/GoogleDrive/Audio", CloudSyncBacking::GoogleDrive),
        (
            "/Users/a/Library/CloudStorage/OneDrive-Personal/x",
            CloudSyncBacking::OneDrive,
        ),
        ("C:\\Users\\a\\OneDrive\\Audio", CloudSyncBacking::OneDrive),
        (
            "/Users/a/Library/CloudStorage/Box-Box/x",
            CloudSyncBacking::Box,
        ),
        ("/Users/a/Box Sync/Audio", CloudSyncBacking::Box),
        ("/Users/a/Dropbox/Recordings", CloudSyncBacking::Dropbox),
        (
            "C:\\Users\\a\\Dropbox (Personal)\\x",
            CloudSyncBacking::Dropbox,
        ),
        (
            "/Users/a/Library/CloudStorage/Mega-Personal/x",
            CloudSyncBacking::Unknown,
        ),
        ("/Users/a/Music/Recordings", CloudSyncBacking::None),
        ("/Volumes/External/Audio", CloudSyncBacking::None),
        ("", CloudSyncBacking::None),
        ("relative/path", CloudSyncBacking::None),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            detect_cloud_sync(Path::new(raw)),
            expected,
            "wrong backing for {raw}"
        );
    }
}

/// All four disclosure combinations of (mode, network-dependent) produce a
/// distinct, honest sentence; only the cloud-backed folder is ever disabled,
/// and only while Local-only Mode is on.
#[test]
fn the_watched_folder_disclosure_covers_all_four_mode_and_backing_combinations() {
    let synced = WatchedFolder::detect("/Users/a/Dropbox/Recordings");
    let plain = WatchedFolder::detect(PathBuf::from("/Users/a/Music/Recordings"));
    assert_eq!(synced.backing, CloudSyncBacking::Dropbox);
    assert_eq!(plain.backing, CloudSyncBacking::None);

    let synced_locked = synced.disclosure(true);
    assert!(synced_locked.disabled);
    assert!(synced_locked.potentially_network_dependent);
    assert!(synced_locked.explanation.contains("Local-only Mode"));

    let synced_open = synced.disclosure(false);
    assert!(!synced_open.disabled);
    assert!(synced_open.potentially_network_dependent);
    assert!(synced_open
        .explanation
        .contains("outside this application's control"));

    for local_only in [true, false] {
        let disclosure = plain.disclosure(local_only);
        assert!(!disclosure.disabled);
        assert!(!disclosure.potentially_network_dependent);
        assert_eq!(
            disclosure.explanation,
            "This folder does not appear to be cloud-synchronized."
        );
        assert_eq!(disclosure.backing, CloudSyncBacking::None);
        assert!(disclosure.path.ends_with("Recordings"));
    }
}

/// A persisted state that says "network permitted" but carries no grant is an
/// inconsistent row; the enforcement object fails closed and the reported
/// status says Local-only, so a corrupted settings table cannot open egress.
#[test]
fn the_described_status_matches_the_enforced_policy_for_every_reachable_state() {
    let local = NetworkPolicySettings::local_only();
    assert_eq!(local.describe().mode, NetworkMode::LocalOnly);
    assert_eq!(local.effective_policy().mode(), NetworkMode::LocalOnly);
    assert!(!local.describe().network.cloud_cli_launchable);

    let mut permitted = NetworkPolicySettings::local_only();
    permitted.disable_local_only(true, grant()).unwrap();
    assert_eq!(permitted.describe().mode, NetworkMode::NetworkPermitted);
    assert_eq!(
        permitted.effective_policy().mode(),
        NetworkMode::NetworkPermitted
    );
    assert!(permitted.describe().network.cloud_cli_launchable);
    assert!(permitted
        .describe()
        .network
        .disabled_capabilities
        .iter()
        .any(|c| c.reason == DenyReason::ModelDownloadNotApproved));
}

/// The service hands back a *fresh* enforcement object after each transition,
/// so a cached policy cannot keep a revoked capability alive.
#[test]
fn every_transition_yields_a_freshly_built_enforcement_object() {
    let service = NetworkPolicyService::new(FlakyStore::default());

    let (_, before) = service.current().unwrap();
    assert_eq!(before.mode(), NetworkMode::LocalOnly);

    service
        .set_local_only_mode(false, true, Some(grant()))
        .unwrap();
    let (_, during) = service.current().unwrap();
    assert_eq!(during.mode(), NetworkMode::NetworkPermitted);
    assert_eq!(
        before.mode(),
        NetworkMode::LocalOnly,
        "the earlier object is not mutated in place"
    );

    let status = service.set_local_only_mode(true, false, None).unwrap();
    assert_eq!(status.mode, NetworkMode::LocalOnly);
    let (_, after) = service.current().unwrap();
    assert_eq!(after.mode(), NetworkMode::LocalOnly);
    assert!(after.grant().is_none());
}
