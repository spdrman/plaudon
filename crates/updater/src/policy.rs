//! The Local-only Mode gate in front of the update checker
//! (SPEC v1.3 §16.2A, §18.6A "Local-only" row, §22 item 15).
//!
//! §16.2A requires a single Rust-owned outbound policy that **every**
//! network-capable subsystem consults, the update checker explicitly among
//! them. §22 item 15 makes it a shipping condition: Local-only Mode must
//! demonstrably make no app-initiated update-check request.
//!
//! The gate is therefore placed *before* the transport, not inside it:
//! [`UpdateChecker::check_for_update`] returns [`UpdateCheck::Suppressed`]
//! having neither called [`UpdateTransport`] nor formatted the endpoint into
//! a request. [`UpdateTransport`] exists as a trait precisely so the suite can
//! count invocations and assert the count is zero.

use std::fmt;

use crate::config::UPDATE_MANIFEST_ENDPOINT;
use crate::error::UpdateError;
use crate::manifest::{Target, VerifiedManifest, Version};
use crate::verify::{check_version_policy, UpdateVerifier};

/// Whether this process may initiate outbound connections (§16.2A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressPolicy {
    /// Local-only Mode: the default. No app-initiated network activity.
    LocalOnly,
    /// The user has enabled network features, but automatic update checks are
    /// switched off. Still suppressed, with a different explanation.
    UpdatesDisabled,
    /// Update checks are permitted.
    Allowed,
}

impl EgressPolicy {
    /// Why a check would be suppressed, or `None` when it may proceed.
    #[must_use]
    pub const fn suppression(self) -> Option<SuppressionReason> {
        match self {
            Self::LocalOnly => Some(SuppressionReason::LocalOnlyMode),
            Self::UpdatesDisabled => Some(SuppressionReason::AutomaticUpdatesDisabled),
            Self::Allowed => None,
        }
    }
}

/// Why an update check did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    /// Local-only Mode is active (§16.2A).
    LocalOnlyMode,
    /// The user turned automatic update checks off.
    AutomaticUpdatesDisabled,
}

impl SuppressionReason {
    /// Human-readable explanation for the UI.
    ///
    /// §5.4 and §16.2A both require disabled capabilities to be *explained*
    /// rather than silently inert, so the Settings and About surfaces render
    /// this string verbatim next to the greyed-out "Check for updates"
    /// control.
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::LocalOnlyMode => {
                "Local-only Mode is on, so the app did not contact the update server. \
                 Turn Local-only Mode off in Settings to check for updates."
            }
            Self::AutomaticUpdatesDisabled => {
                "Automatic update checks are turned off, so the app did not contact the \
                 update server. You can check manually in Settings."
            }
        }
    }
}

impl fmt::Display for SuppressionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LocalOnlyMode => "local-only mode",
            Self::AutomaticUpdatesDisabled => "automatic update checks disabled",
        })
    }
}

/// A transport failure, reported without the URL that produced it (§15.4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The endpoint could not be reached.
    #[error("the update server could not be reached")]
    Unreachable,
    /// The endpoint answered with something other than the expected document.
    #[error("the update server returned an unexpected response")]
    UnexpectedResponse,
}

/// The network seam used by the update checker.
///
/// Implementations are supplied by the Tauri shell. The trait exists so that
/// §22 item 15 is testable as an *observable count of invocations* rather
/// than as an assertion about an opaque HTTP client.
pub trait UpdateTransport: fmt::Debug {
    /// Fetch a document. Called only when [`EgressPolicy::Allowed`].
    fn fetch(&self, url: &str) -> Result<Vec<u8>, TransportError>;
}

/// Outcome of an update check.
#[derive(Debug)]
pub enum UpdateCheck {
    /// A newer, fully verified release is available.
    Available(Box<VerifiedManifest>),
    /// The installed version is already the published one.
    UpToDate {
        /// The installed version.
        installed: Version,
    },
    /// The check did not happen, and no request was made (§22 item 15).
    Suppressed {
        /// Why the check was suppressed; carries the UI explanation.
        reason: SuppressionReason,
    },
}

/// Policy-gated update checker (§16.2A, §16.5).
#[derive(Debug)]
pub struct UpdateChecker<T: UpdateTransport, V: UpdateVerifier> {
    transport: T,
    verifier: V,
    policy: EgressPolicy,
    endpoint: &'static str,
    installed: Version,
    floor: Version,
    host: Target,
}

impl<T: UpdateTransport, V: UpdateVerifier> UpdateChecker<T, V> {
    /// Build a checker pinned to [`UPDATE_MANIFEST_ENDPOINT`].
    ///
    /// `installed` is the running version and `floor` the installation's
    /// current minimum-supported-version, both owned by
    /// [`crate::install::Installer`].
    pub fn new(
        transport: T,
        verifier: V,
        policy: EgressPolicy,
        installed: Version,
        floor: Version,
    ) -> Self {
        Self {
            transport,
            verifier,
            policy,
            endpoint: UPDATE_MANIFEST_ENDPOINT,
            installed,
            floor,
            host: Target::host(),
        }
    }

    /// Override the host target. Only the fixture suite needs this: a
    /// manifest built for the test runner's own target must still be
    /// rejected on every other target, so the suite pins one explicitly.
    #[must_use]
    pub const fn with_host_target(mut self, host: Target) -> Self {
        self.host = host;
        self
    }

    /// The endpoint this checker would contact. Reading it is not a request.
    #[must_use]
    pub const fn endpoint(&self) -> &'static str {
        self.endpoint
    }

    /// The active egress policy.
    #[must_use]
    pub const fn policy(&self) -> EgressPolicy {
        self.policy
    }

    /// Check for an update.
    ///
    /// When the policy suppresses checks this returns
    /// [`UpdateCheck::Suppressed`] **before** the transport is touched and
    /// before any URL is formatted (§16.2A, §22 item 15). Otherwise the
    /// fetched envelope goes through the full §16.5 verification path: pinned
    /// ed25519 signature, target match, then version policy.
    pub fn check_for_update(&self) -> Result<UpdateCheck, UpdateError> {
        if let Some(reason) = self.policy.suppression() {
            tracing::debug!(%reason, "update check suppressed before any network activity");
            return Ok(UpdateCheck::Suppressed { reason });
        }

        let bytes = self
            .transport
            .fetch(self.endpoint)
            .map_err(UpdateError::Transport)?;
        let verified = self.verifier.verify_manifest(&bytes)?;
        verified.document().check_target(self.host)?;

        if verified.version() == self.installed {
            return Ok(UpdateCheck::UpToDate {
                installed: self.installed,
            });
        }
        check_version_policy(verified.document(), self.installed, self.floor, false)?;
        if verified.version() < self.installed {
            return Err(UpdateError::Verify(
                crate::error::VerifyError::DowngradeBlocked {
                    offered: verified.version(),
                    installed: self.installed,
                    floor: self.floor,
                },
            ));
        }
        Ok(UpdateCheck::Available(Box::new(verified)))
    }

    /// Download and verify the package named by a verified manifest.
    ///
    /// The policy is re-checked here: a mode change between check and
    /// download must fail closed (§16.2A).
    pub fn download_package(&self, manifest: &VerifiedManifest) -> Result<Vec<u8>, UpdateError> {
        if let Some(reason) = self.policy.suppression() {
            return Err(UpdateError::Suppressed(reason));
        }
        let bytes = self
            .transport
            .fetch(&manifest.document().package_url)
            .map_err(UpdateError::Transport)?;
        self.verifier.verify_package(manifest, &bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[derive(Debug, Default)]
    struct CountingTransport {
        calls: Cell<usize>,
    }

    impl UpdateTransport for CountingTransport {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, TransportError> {
            self.calls.set(self.calls.get() + 1);
            Err(TransportError::Unreachable)
        }
    }

    #[derive(Debug)]
    struct NeverCalledVerifier;

    impl UpdateVerifier for NeverCalledVerifier {
        fn verify_manifest(
            &self,
            _bytes: &[u8],
        ) -> Result<VerifiedManifest, crate::error::VerifyError> {
            panic!("the verifier must not run when the check is suppressed");
        }

        fn verify_package(
            &self,
            _manifest: &VerifiedManifest,
            _package: &[u8],
        ) -> Result<(), crate::error::VerifyError> {
            panic!("the verifier must not run when the check is suppressed");
        }
    }

    #[test]
    fn local_only_mode_suppresses_without_touching_the_transport() {
        let checker = UpdateChecker::new(
            CountingTransport::default(),
            NeverCalledVerifier,
            EgressPolicy::LocalOnly,
            Version::new(1, 0, 0),
            Version::new(1, 0, 0),
        );
        let outcome = checker.check_for_update().expect("suppression is not an error");
        match outcome {
            UpdateCheck::Suppressed { reason } => {
                assert_eq!(reason, SuppressionReason::LocalOnlyMode);
                assert!(reason.explanation().contains("Local-only Mode"));
            }
            other => panic!("expected suppression, got {other:?}"),
        }
        assert_eq!(checker.transport.calls.get(), 0);
    }

    #[test]
    fn every_suppression_reason_explains_itself() {
        for reason in [
            SuppressionReason::LocalOnlyMode,
            SuppressionReason::AutomaticUpdatesDisabled,
        ] {
            let text = reason.explanation();
            assert!(text.len() > 40, "{reason} needs a real explanation");
            assert!(!text.contains("plaudon.app"), "no endpoint in UI copy");
        }
    }
}
