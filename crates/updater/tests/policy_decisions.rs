//! Every decision the update checker can reach (SPEC v1.3 §16.2A, §16.5,
//! §22 item 15).
//!
//! `tests/local_only.rs` owns the "no request at all" assertion. This suite
//! owns the other side: once a request *is* permitted, every answer the
//! server can give must land on a named outcome — a verified update, an
//! up-to-date installation, or a specific rejection — and a policy change
//! between check and download must fail closed.

mod support;

use std::sync::Mutex;

use plaudon_updater::config::UPDATE_MANIFEST_ENDPOINT;
use plaudon_updater::error::{UpdateError, VerifyError};
use plaudon_updater::manifest::{Target, Version};
use plaudon_updater::policy::{
    EgressPolicy, SuppressionReason, TransportError, UpdateCheck, UpdateChecker, UpdateTransport,
};
use plaudon_updater::testkeys::{
    package_bytes, test_signing_key, test_trusted_keys, FIXTURE_TARGET, FOREIGN_TARGET, TEST_KEY_ID,
};
use plaudon_updater::verify::Ed25519Verifier;

use support::{manifest_for, signed_envelope};

const V1: Version = Version::new(1, 0, 0);
const V2: Version = Version::new(1, 1, 0);

/// A transport that answers the manifest endpoint and the package URL from
/// two separate scripted slots, recording every request.
#[derive(Debug)]
struct ScriptedTransport {
    manifest: Result<Vec<u8>, TransportError>,
    package: Result<Vec<u8>, TransportError>,
    requested: Mutex<Vec<String>>,
}

impl ScriptedTransport {
    fn new(manifest: Result<Vec<u8>, TransportError>) -> Self {
        Self {
            manifest,
            package: Err(TransportError::Unreachable),
            requested: Mutex::new(Vec::new()),
        }
    }

    fn with_package(mut self, package: Result<Vec<u8>, TransportError>) -> Self {
        self.package = package;
        self
    }

    fn requested(&self) -> Vec<String> {
        self.requested.lock().expect("not poisoned").clone()
    }
}

impl UpdateTransport for ScriptedTransport {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        self.requested
            .lock()
            .expect("not poisoned")
            .push(url.to_owned());
        if url == UPDATE_MANIFEST_ENDPOINT {
            self.manifest.clone()
        } else {
            self.package.clone()
        }
    }
}

fn checker(
    transport: ScriptedTransport,
    policy: EgressPolicy,
    installed: Version,
    floor: Version,
) -> UpdateChecker<ScriptedTransport, Ed25519Verifier> {
    UpdateChecker::new(
        transport,
        Ed25519Verifier::new(test_trusted_keys()),
        policy,
        installed,
        floor,
    )
    .with_host_target(FIXTURE_TARGET)
}

/// A validly signed envelope for `version`, targeting `target`.
fn envelope_for(version: Version, floor: Version, target: Target) -> Vec<u8> {
    let package = package_bytes(version);
    let manifest = manifest_for(version, floor, target, &package, &test_signing_key());
    signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID).into_bytes()
}

#[test]
fn every_egress_policy_states_whether_it_suppresses() {
    assert_eq!(
        EgressPolicy::LocalOnly.suppression(),
        Some(SuppressionReason::LocalOnlyMode)
    );
    assert_eq!(
        EgressPolicy::UpdatesDisabled.suppression(),
        Some(SuppressionReason::AutomaticUpdatesDisabled)
    );
    assert_eq!(EgressPolicy::Allowed.suppression(), None);
}

#[test]
fn both_suppression_reasons_render_for_logs_and_for_the_ui() {
    assert_eq!(
        SuppressionReason::LocalOnlyMode.to_string(),
        "local-only mode"
    );
    assert_eq!(
        SuppressionReason::AutomaticUpdatesDisabled.to_string(),
        "automatic update checks disabled"
    );
    assert!(SuppressionReason::LocalOnlyMode
        .explanation()
        .contains("Local-only Mode"));
    assert!(SuppressionReason::AutomaticUpdatesDisabled
        .explanation()
        .contains("turned off"));
    for reason in [
        SuppressionReason::LocalOnlyMode,
        SuppressionReason::AutomaticUpdatesDisabled,
    ] {
        assert!(
            !reason.explanation().contains("updates.plaudon.app"),
            "UI copy must not name the endpoint"
        );
    }
}

#[test]
fn both_transport_failures_report_without_naming_the_endpoint() {
    for error in [
        TransportError::Unreachable,
        TransportError::UnexpectedResponse,
    ] {
        let text = error.to_string();
        assert_ne!(text, "", "a transport failure must explain itself");
        assert!(!text.contains("https://"), "{text}");
        assert!(!text.contains("plaudon.app"), "{text}");
    }
    assert_ne!(
        TransportError::Unreachable.to_string(),
        TransportError::UnexpectedResponse.to_string()
    );
}

#[test]
fn an_allowed_check_reports_an_available_update() {
    let transport = ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET)));
    let checker = checker(transport, EgressPolicy::Allowed, V1, V1);
    assert_eq!(checker.endpoint(), UPDATE_MANIFEST_ENDPOINT);
    assert_eq!(checker.policy(), EgressPolicy::Allowed);

    let outcome = checker.check_for_update().expect("the check runs");
    match outcome {
        UpdateCheck::Available(manifest) => {
            assert_eq!(manifest.version(), V2);
            assert_eq!(manifest.signing_key_id(), TEST_KEY_ID);
        }
        other => panic!("expected an available update, got {other:?}"),
    }
    assert_eq!(
        checker.transport().requested(),
        vec![UPDATE_MANIFEST_ENDPOINT]
    );
}

#[test]
fn an_installation_already_on_the_published_version_is_up_to_date() {
    let transport = ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET)));
    let checker = checker(transport, EgressPolicy::Allowed, V2, V1);
    match checker.check_for_update().expect("the check runs") {
        UpdateCheck::UpToDate { installed } => assert_eq!(installed, V2),
        other => panic!("expected up-to-date, got {other:?}"),
    }
}

#[test]
fn an_unreachable_server_is_a_transport_failure_not_a_rejection() {
    let transport = ScriptedTransport::new(Err(TransportError::Unreachable));
    let checker = checker(transport, EgressPolicy::Allowed, V1, V1);
    let error = checker.check_for_update().expect_err("unreachable");
    assert!(
        matches!(error, UpdateError::Transport(TransportError::Unreachable)),
        "got {error:?}"
    );
    assert!(error.is_retryable(), "a transport failure may be retried");
}

#[test]
fn an_unusable_response_body_is_a_malformed_manifest() {
    let transport = ScriptedTransport::new(Ok(b"<html>404</html>".to_vec()));
    let checker = checker(transport, EgressPolicy::Allowed, V1, V1);
    let error = checker.check_for_update().expect_err("not a manifest");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::MalformedManifest(_))
        ),
        "got {error:?}"
    );
}

#[test]
fn a_manifest_for_another_target_is_refused_before_any_version_comparison() {
    let transport = ScriptedTransport::new(Ok(envelope_for(V2, V1, FOREIGN_TARGET)));
    let checker = checker(transport, EgressPolicy::Allowed, V1, V1);
    let error = checker.check_for_update().expect_err("foreign target");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::UnsupportedTarget { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn a_published_release_below_the_floor_is_blocked_as_a_downgrade() {
    // Installed 1.1.0 with a floor of 1.1.0; the server offers 1.0.0.
    let transport = ScriptedTransport::new(Ok(envelope_for(V1, V1, FIXTURE_TARGET)));
    let checker = checker(transport, EgressPolicy::Allowed, V2, V2);
    let error = checker.check_for_update().expect_err("below the floor");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::DowngradeBlocked { offered, floor, .. })
                if offered == V1 && floor == V2
        ),
        "got {error:?}"
    );
}

#[test]
fn a_published_release_older_than_the_installed_one_is_blocked_above_the_floor() {
    // The floor permits 1.1.0, but the installation is already on 1.5.0, so
    // the second, explicit guard is the one that refuses.
    let installed = Version::new(1, 5, 0);
    let transport = ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET)));
    let checker = checker(transport, EgressPolicy::Allowed, installed, V1);
    let error = checker
        .check_for_update()
        .expect_err("older than installed");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::DowngradeBlocked { offered, installed: from, floor })
                if offered == V2 && from == installed && floor == V1
        ),
        "got {error:?}"
    );
}

#[test]
fn a_suppressed_policy_refuses_the_download_itself() {
    // Check under an allowed policy, then download under a suppressed one:
    // the mode change must fail closed without touching the transport.
    let transport = ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET)));
    let allowed = checker(transport, EgressPolicy::Allowed, V1, V1);
    let manifest = match allowed.check_for_update().expect("check runs") {
        UpdateCheck::Available(manifest) => manifest,
        other => panic!("expected an update, got {other:?}"),
    };

    for (policy, reason) in [
        (EgressPolicy::LocalOnly, SuppressionReason::LocalOnlyMode),
        (
            EgressPolicy::UpdatesDisabled,
            SuppressionReason::AutomaticUpdatesDisabled,
        ),
    ] {
        let suppressed = checker(
            ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET)))
                .with_package(Ok(package_bytes(V2))),
            policy,
            V1,
            V1,
        );
        let error = suppressed
            .download_package(&manifest)
            .expect_err("a suppressed policy must refuse the download");
        assert!(
            matches!(error, UpdateError::Suppressed(got) if got == reason),
            "got {error:?}"
        );
        assert!(
            suppressed.transport().requested().is_empty(),
            "a suppressed download must not reach the transport"
        );
    }
}

#[test]
fn a_download_verifies_the_bytes_it_received() {
    let good = ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET)))
        .with_package(Ok(package_bytes(V2)));
    let checker = checker(good, EgressPolicy::Allowed, V1, V1);
    let manifest = match checker.check_for_update().expect("check runs") {
        UpdateCheck::Available(manifest) => manifest,
        other => panic!("expected an update, got {other:?}"),
    };
    let package = checker
        .download_package(&manifest)
        .expect("the published package verifies");
    assert_eq!(package, package_bytes(V2));
    assert_eq!(
        checker.transport().requested(),
        vec![
            UPDATE_MANIFEST_ENDPOINT.to_owned(),
            manifest.document().package_url.clone()
        ]
    );
}

#[test]
fn a_download_that_does_not_match_the_manifest_is_rejected() {
    let manifest = {
        let probe = checker(
            ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET))),
            EgressPolicy::Allowed,
            V1,
            V1,
        );
        match probe.check_for_update().expect("check runs") {
            UpdateCheck::Available(manifest) => manifest,
            other => panic!("expected an update, got {other:?}"),
        }
    };

    // Truncated body.
    let mut short = package_bytes(V2);
    short.truncate(16);
    let truncating = checker(
        ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET))).with_package(Ok(short)),
        EgressPolicy::Allowed,
        V1,
        V1,
    );
    assert!(
        matches!(
            truncating.download_package(&manifest),
            Err(UpdateError::Verify(VerifyError::TruncatedPackage { .. }))
        ),
        "a short download must be reported as truncated"
    );

    // Full-length but substituted body.
    let mut hostile = package_bytes(V2);
    let last = hostile.len() - 1;
    hostile[last] ^= 0x01;
    let substituting = checker(
        ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET))).with_package(Ok(hostile)),
        EgressPolicy::Allowed,
        V1,
        V1,
    );
    assert!(
        matches!(
            substituting.download_package(&manifest),
            Err(UpdateError::Verify(VerifyError::DigestMismatch { .. }))
        ),
        "a substituted download must be reported as a digest mismatch"
    );

    // The package host is unreachable.
    let unreachable = checker(
        ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET)))
            .with_package(Err(TransportError::UnexpectedResponse)),
        EgressPolicy::Allowed,
        V1,
        V1,
    );
    assert!(
        matches!(
            unreachable.download_package(&manifest),
            Err(UpdateError::Transport(TransportError::UnexpectedResponse))
        ),
        "a failed package fetch is a transport error"
    );
}

#[test]
fn a_suppressed_check_never_formats_the_endpoint() {
    for policy in [EgressPolicy::LocalOnly, EgressPolicy::UpdatesDisabled] {
        let checker = checker(
            ScriptedTransport::new(Ok(envelope_for(V2, V1, FIXTURE_TARGET))),
            policy,
            V1,
            V1,
        );
        match checker
            .check_for_update()
            .expect("suppression is not an error")
        {
            UpdateCheck::Suppressed { reason } => {
                assert_eq!(Some(reason), policy.suppression());
            }
            other => panic!("expected suppression, got {other:?}"),
        }
        assert_eq!(checker.transport().requested(), Vec::<String>::new());
    }
}
