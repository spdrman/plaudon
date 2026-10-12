//! Signed update, verified rollback and the Local-only suppression gate
//! (task N2.2; spec 2.4, 16.2A, 16.5, 22 items 8 and 15).
//!
//! The verification, A/B install and rollback logic lives in
//! [`plaudon_updater`]. This module is the shell's half: it owns the
//! installation root, derives the egress policy from the application's
//! Local-only setting, and adapts the application's policy-gated HTTP
//! transport to the updater's network seam.
//!
//! Two properties are enforced here rather than assumed:
//!
//! * **No app-initiated update check in Local-only Mode.** The egress policy
//!   is derived from the setting, and the checker returns `Suppressed` before
//!   it touches the transport. [`UpdateService::check`] therefore cannot emit
//!   a request, and the tests assert an invocation count of zero.
//! * **Rollback verifies as strictly as a forward update.** The shell never
//!   calls a separate "trusted" path; it calls
//!   [`plaudon_updater::install::Installer::rollback`], which re-runs the same
//!   verifier.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_security::outbound::http::{
    GatedHttpClientFactory, HttpTransport as GatedHttpTransport,
};
use plaudon_security::outbound::{EgressPurpose, PayloadClass, Subsystem};
use plaudon_updater::config::{RELEASE_SIGNING_KEY_ID, UPDATE_MANIFEST_ENDPOINT};
use plaudon_updater::error::UpdateError;
use plaudon_updater::install::Installer;
use plaudon_updater::manifest::{Target, Version};
use plaudon_updater::policy::{
    EgressPolicy, TransportError, UpdateCheck, UpdateChecker, UpdateTransport,
};
use plaudon_updater::verify::Ed25519Verifier;

/// Directory name of the update installation root inside the app data
/// directory.
pub const UPDATE_ROOT_DIR: &str = "updates";

/// The updater's network seam, implemented over the application's single
/// policy-gated HTTP client.
///
/// Every fetch goes through [`GatedHttpClientFactory`], so an update request
/// is subject to exactly the same `OutboundPolicy` decision as any other
/// subsystem; there is no private socket here.
pub struct GatedUpdateTransport {
    factory: Arc<GatedHttpClientFactory>,
    client: Arc<dyn GatedHttpTransport>,
}

// `HttpTransport` is implemented by concrete client crates and is not required
// to be `Debug`; the updater's seam is, so the opaque client is described
// rather than formatted.
impl std::fmt::Debug for GatedUpdateTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatedUpdateTransport")
            .field("factory", &self.factory)
            .field("client", &"<policy-gated http client>")
            .finish()
    }
}

impl GatedUpdateTransport {
    /// Builds a transport from the shared factory and the concrete HTTP
    /// client the shell installs.
    #[must_use]
    pub fn new(factory: Arc<GatedHttpClientFactory>, client: Arc<dyn GatedHttpTransport>) -> Self {
        Self { factory, client }
    }
}

impl UpdateTransport for GatedUpdateTransport {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        let request = self
            .factory
            .get(
                url,
                Subsystem::UpdateChecker,
                EgressPurpose::ManifestFetch,
                PayloadClass::None,
            )
            .map_err(|_| TransportError::Unreachable)?;
        let response = self
            .client
            .send(&request)
            .map_err(|_| TransportError::Unreachable)?;
        if response.status != 200 {
            return Err(TransportError::UnexpectedResponse);
        }
        Ok(response.body)
    }
}

/// The shell's update service.
#[derive(Debug)]
pub struct UpdateService {
    installer: Installer,
    policy: EgressPolicy,
}

impl UpdateService {
    /// Opens the installation root, running crash recovery.
    ///
    /// A corrupt or absent journal is not a crash: on a first launch there is
    /// nothing to recover, and the caller bootstraps the root from the
    /// shipped artifacts instead.
    pub fn open(app_data_dir: &Path, local_only: bool) -> Result<Self, UpdateError> {
        let root = Self::root_for(app_data_dir);
        let verifier = Ed25519Verifier::embedded();
        let installer = Installer::open(&root, Arc::new(verifier), Target::host())?;
        Ok(Self {
            installer,
            policy: Self::policy_for(local_only),
        })
    }

    /// Installation root inside the application data directory.
    #[must_use]
    pub fn root_for(app_data_dir: &Path) -> PathBuf {
        app_data_dir.join(UPDATE_ROOT_DIR)
    }

    /// Derives the egress policy from the Local-only setting.
    ///
    /// Spec 22(15): with Local-only Mode on, the application performs no
    /// update check at all.
    #[must_use]
    pub const fn policy_for(local_only: bool) -> EgressPolicy {
        if local_only {
            EgressPolicy::LocalOnly
        } else {
            EgressPolicy::Allowed
        }
    }

    /// Updates the policy after the user changes the Local-only setting.
    pub const fn set_local_only(&mut self, local_only: bool) {
        self.policy = Self::policy_for(local_only);
    }

    /// The active egress policy.
    #[must_use]
    pub const fn policy(&self) -> EgressPolicy {
        self.policy
    }

    /// Version that will launch next.
    pub fn installed_version(&self) -> Result<Version, UpdateError> {
        Ok(self.installer.launchable()?.version)
    }

    /// Checks for an update through `transport`.
    ///
    /// Under Local-only Mode the checker returns `Suppressed` without
    /// touching `transport`.
    pub fn check<T: UpdateTransport>(&self, transport: T) -> Result<UpdateCheck, UpdateError> {
        let launchable = self.installer.launchable()?;
        let checker = UpdateChecker::new(
            transport,
            Ed25519Verifier::embedded(),
            self.policy,
            launchable.version,
            self.installer.journal().floor,
        );
        checker.check_for_update()
    }

    /// Applies a downloaded update. The bytes are verified inside the
    /// installer; this never pre-trusts them.
    pub fn apply(&mut self, envelope: &[u8], package: &[u8]) -> Result<Version, UpdateError> {
        self.installer.install(envelope, package)
    }

    /// Rolls back to the retained previous version, re-running the identical
    /// verification path (spec 16.5).
    pub fn rollback(&mut self) -> Result<Version, UpdateError> {
        self.installer.rollback()
    }

    /// Human-readable explanation for the UI when the check was suppressed.
    #[must_use]
    pub fn suppression_explanation(check: &UpdateCheck) -> Option<&'static str> {
        match check {
            UpdateCheck::Suppressed { reason } => Some(reason.explanation()),
            _ => None,
        }
    }
}

/// Maps an updater failure onto the application error taxonomy (spec 15.4).
#[must_use]
pub fn to_app_error(error: &UpdateError) -> AppError {
    let code = match error {
        UpdateError::Suppressed(_) => AppErrorCode::LocalOnlyMode,
        UpdateError::Transport(_) => AppErrorCode::EgressDenied,
        _ => AppErrorCode::IntegrityFailed,
    };
    AppError::new(code, error.to_string())
}

/// The pinned endpoint and key, re-exported so a drift test in this crate
/// fails when either changes without review (spec 16.5).
#[must_use]
pub const fn pinned_update_configuration() -> (&'static str, &'static str) {
    (UPDATE_MANIFEST_ENDPOINT, RELEASE_SIGNING_KEY_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A transport that counts invocations and never performs I/O.
    #[derive(Debug, Default)]
    struct CountingTransport {
        calls: AtomicUsize,
    }

    impl UpdateTransport for CountingTransport {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, TransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(TransportError::Unreachable)
        }
    }

    #[test]
    fn local_only_mode_maps_to_the_suppressing_policy() {
        assert_eq!(UpdateService::policy_for(true), EgressPolicy::LocalOnly);
        assert_eq!(UpdateService::policy_for(false), EgressPolicy::Allowed);
    }

    #[test]
    fn a_suppressed_check_carries_an_explanation_for_the_ui() {
        let check = UpdateCheck::Suppressed {
            reason: EgressPolicy::LocalOnly
                .suppression()
                .expect("local-only suppresses"),
        };
        let explanation =
            UpdateService::suppression_explanation(&check).expect("an explanation exists");
        assert!(
            !explanation.trim().is_empty(),
            "a greyed-out control must say why (spec 5.4)"
        );
    }

    #[test]
    fn the_update_configuration_is_pinned_and_visible_in_review() {
        let (endpoint, key_id) = pinned_update_configuration();
        assert_eq!(
            endpoint, "https://updates.plaudon.app/stable/update-manifest.v1.json",
            "a changed update endpoint must be a reviewable diff (spec 16.5)"
        );
        assert_eq!(key_id, "plaudon-release-2026a");
        assert!(endpoint.starts_with("https://"));
    }

    #[test]
    fn the_update_root_lives_under_the_application_data_directory() {
        let root = UpdateService::root_for(Path::new("/tmp/library"));
        assert_eq!(root, Path::new("/tmp/library/updates"));
    }

    #[test]
    fn a_transport_is_never_invoked_while_local_only_mode_is_active() {
        // The checker is the component under test; it must short-circuit
        // before the transport, so an installer is not needed to prove it.
        let checker = UpdateChecker::new(
            CountingTransport::default(),
            Ed25519Verifier::embedded(),
            EgressPolicy::LocalOnly,
            Version::new(0, 1, 0),
            Version::new(0, 1, 0),
        );
        let check = checker
            .check_for_update()
            .expect("a suppressed check is not an error");
        assert!(matches!(check, UpdateCheck::Suppressed { .. }));
        assert_eq!(
            checker.transport().calls.load(Ordering::SeqCst),
            0,
            "spec 22(15): Local-only Mode performs no app-initiated update check"
        );
    }
}
