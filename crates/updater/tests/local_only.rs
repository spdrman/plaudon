//! Local-only Mode makes zero app-initiated update requests
//! (SPEC v1.3 §16.2A, §18.6A "Local-only" row, §22 item 15).
//!
//! The assertion is an observed invocation count on the transport seam, not a
//! claim about an opaque HTTP client: the transport records every call and
//! the test requires the count to be exactly zero.

mod support;

use std::sync::Mutex;

use plaudon_updater::config::UPDATE_MANIFEST_ENDPOINT;
use plaudon_updater::error::UpdateError;
use plaudon_updater::manifest::Version;
use plaudon_updater::policy::{
    EgressPolicy, SuppressionReason, TransportError, UpdateCheck, UpdateChecker, UpdateTransport,
};
use plaudon_updater::testkeys::{test_trusted_keys, FixtureFlaw, FIXTURE_TARGET, UPDATE_VERSION};
use plaudon_updater::verify::Ed25519Verifier;

use support::load;

/// Records every URL it is asked for, so "no request" is an assertion about
/// observed behaviour rather than about intent.
#[derive(Debug)]
struct RecordingTransport {
    requested: Mutex<Vec<String>>,
    body: Vec<u8>,
}

impl RecordingTransport {
    fn serving(body: Vec<u8>) -> Self {
        Self {
            requested: Mutex::new(Vec::new()),
            body,
        }
    }

    fn requested(&self) -> Vec<String> {
        self.requested.lock().expect("not poisoned").clone()
    }
}

impl UpdateTransport for RecordingTransport {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        self.requested
            .lock()
            .expect("not poisoned")
            .push(url.to_owned());
        Ok(self.body.clone())
    }
}

fn checker(
    policy: EgressPolicy,
    transport: RecordingTransport,
) -> UpdateChecker<RecordingTransport, Ed25519Verifier> {
    UpdateChecker::new(
        transport,
        Ed25519Verifier::new(test_trusted_keys()),
        policy,
        Version::new(1, 0, 0),
        Version::new(1, 0, 0),
    )
    .with_host_target(FIXTURE_TARGET)
}

#[test]
fn local_only_mode_performs_zero_transport_invocations() {
    let update = load(FixtureFlaw::None.fixture_name());
    let checker = checker(
        EgressPolicy::LocalOnly,
        RecordingTransport::serving(update.envelope.into_bytes()),
    );

    let outcome = checker
        .check_for_update()
        .expect("suppression is an outcome, not a failure");
    match outcome {
        UpdateCheck::Suppressed { reason } => {
            assert_eq!(reason, SuppressionReason::LocalOnlyMode);
        }
        other => panic!("expected suppression, got {other:?}"),
    }

    assert_eq!(
        checker.transport().requested(),
        Vec::<String>::new(),
        "Local-only Mode must not initiate a single update request"
    );
}

#[test]
fn disabled_updates_also_suppress_without_a_request() {
    let update = load(FixtureFlaw::None.fixture_name());
    let checker = checker(
        EgressPolicy::UpdatesDisabled,
        RecordingTransport::serving(update.envelope.into_bytes()),
    );
    assert!(matches!(
        checker.check_for_update().expect("suppressed"),
        UpdateCheck::Suppressed {
            reason: SuppressionReason::AutomaticUpdatesDisabled
        }
    ));
    assert!(checker.transport().requested().is_empty());
}

#[test]
fn a_suppressed_download_fails_closed_after_a_mode_change() {
    // Check while allowed, then flip to Local-only before downloading.
    let update = load(FixtureFlaw::None.fixture_name());
    let allowed = checker(
        EgressPolicy::Allowed,
        RecordingTransport::serving(update.envelope.clone().into_bytes()),
    );
    let UpdateCheck::Available(manifest) = allowed.check_for_update().expect("check") else {
        panic!("the valid fixture must be offered as available");
    };
    assert_eq!(manifest.version(), UPDATE_VERSION);
    assert_eq!(allowed.transport().requested(), vec![UPDATE_MANIFEST_ENDPOINT]);

    let suppressed = checker(
        EgressPolicy::LocalOnly,
        RecordingTransport::serving(update.package),
    );
    let error = suppressed
        .download_package(&manifest)
        .expect_err("a download must fail closed under Local-only Mode");
    assert!(matches!(
        error,
        UpdateError::Suppressed(SuppressionReason::LocalOnlyMode)
    ));
    assert!(
        suppressed.transport().requested().is_empty(),
        "no package byte may be requested in Local-only Mode"
    );
}

#[test]
fn an_allowed_check_contacts_exactly_the_pinned_endpoint_once() {
    let update = load(FixtureFlaw::None.fixture_name());
    let checker = checker(
        EgressPolicy::Allowed,
        RecordingTransport::serving(update.envelope.into_bytes()),
    );
    checker.check_for_update().expect("check");
    assert_eq!(checker.transport().requested(), vec![UPDATE_MANIFEST_ENDPOINT]);
}

#[test]
fn the_suppression_explanation_is_user_facing_and_endpoint_free() {
    for reason in [
        SuppressionReason::LocalOnlyMode,
        SuppressionReason::AutomaticUpdatesDisabled,
    ] {
        let text = reason.explanation();
        assert!(text.contains("Settings"), "{reason} must tell the user where to go");
        assert!(!text.contains("https://"), "no URL in UI copy");
    }
}
