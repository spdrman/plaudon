//! Unit tests for the consent scope algebra, the ledger and error mapping.
//! Hermetic: no network, no cloud, no device.

use std::convert::Infallible;

use super::{
    dispatch_preflight, ConsentError, ConsentGrant, ConsentLedger, ConsentPurpose,
    ConsentRepository, ContentType, DispatchRequest, GrantScope, PayloadClassNotConsentable,
    ProviderRoute, RequestScope, RetentionPosture, ScopeAxis, RELAY_OPENROUTER,
};
use crate::error::{SecurityError, CONSENT_REQUIRED, CONSENT_SCOPE_MISMATCH, EGRESS_DENIED};
use crate::outbound::{
    ApprovalSource, Destination, EgressObserver, NetworkGrant, OutboundPolicy, PayloadClass,
    RecordingObserver,
};
use std::sync::Arc;

const DISCLOSURE: &str = "privacy-disclosure-2026-02";
const NOW: i64 = 1_770_000_000;

fn route() -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large")
        .with_downstream_provider("vendor-x")
}

fn permitted() -> OutboundPolicy {
    OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        NOW - 10,
        DISCLOSURE,
    ))
}

// ---------------------------------------------------------------------------
// ContentType <-> PayloadClass
// ---------------------------------------------------------------------------

#[test]
fn content_type_maps_onto_payload_class_both_ways() {
    for content in ContentType::ALL {
        let payload = PayloadClass::from(content);
        assert_eq!(ContentType::try_from(payload).unwrap(), content);
    }
    assert_eq!(
        PayloadClass::from(ContentType::AudioBytes),
        PayloadClass::AudioBytes
    );
    assert_eq!(
        PayloadClass::from(ContentType::TranscriptText),
        PayloadClass::TranscriptText
    );
    assert_eq!(
        PayloadClass::from(ContentType::MetadataOnly),
        PayloadClass::Metadata
    );
}

#[test]
fn non_content_payload_classes_have_no_content_type() {
    for payload in [PayloadClass::None, PayloadClass::Credential] {
        let err = ContentType::try_from(payload).unwrap_err();
        assert_eq!(err, PayloadClassNotConsentable { payload });
        assert_eq!(
            err.envelope_parts().code.as_str(),
            CONSENT_SCOPE_MISMATCH.as_str()
        );
    }
}

#[test]
fn recording_derived_content_is_labelled_honestly() {
    assert!(ContentType::AudioBytes.is_recording_derived());
    assert!(ContentType::TranscriptText.is_recording_derived());
    assert!(!ContentType::MetadataOnly.is_recording_derived());
}

// ---------------------------------------------------------------------------
// ProviderRoute
// ---------------------------------------------------------------------------

#[test]
fn route_equality_requires_all_three_fields() {
    let base = route();
    assert_eq!(base, route());
    assert_ne!(
        base,
        ProviderRoute::new("other-relay", "vendor-x/model-large")
            .with_downstream_provider("vendor-x")
    );
    assert_ne!(
        base,
        ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-small")
            .with_downstream_provider("vendor-x")
    );
    assert_ne!(
        base,
        ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large")
            .with_downstream_provider("reseller-z")
    );
    // Unknown downstream provider is its own route, not a wildcard.
    assert_ne!(
        base,
        ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large")
    );
}

#[test]
fn unknown_downstream_provider_is_labelled_as_undisclosed() {
    let route = ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large");
    assert!(!route.has_known_downstream_provider());
    assert_eq!(
        route.label(),
        "openrouter|vendor-x/model-large|<undisclosed>"
    );
}

#[test]
fn known_relay_resolves_to_the_pinned_https_endpoint() {
    let destination = route().relay_destination().unwrap();
    assert_eq!(
        destination,
        Destination::url("https://openrouter.ai").unwrap()
    );
    assert_eq!(destination.label(), "https://openrouter.ai:443");
}

#[test]
fn unknown_relay_is_refused_without_a_guessed_hostname() {
    let denied = ProviderRoute::new("mystery-relay", "m")
        .relay_destination()
        .unwrap_err();
    assert_eq!(
        denied.envelope_parts().code.as_str(),
        EGRESS_DENIED.as_str()
    );
    assert!(!denied.destination_label.contains("mystery-relay"));
}

// ---------------------------------------------------------------------------
// RetentionPosture
// ---------------------------------------------------------------------------

#[test]
fn unknown_retention_never_satisfies_and_is_never_satisfied() {
    let all = [
        RetentionPosture::ZeroRetention,
        RetentionPosture::LimitedRetention { days: 30 },
        RetentionPosture::ProviderDefault,
        RetentionPosture::Unknown,
    ];
    for other in all {
        assert!(
            !RetentionPosture::Unknown.satisfied_by(other),
            "requirement Unknown vs {other:?}"
        );
        assert!(
            !other.satisfied_by(RetentionPosture::Unknown),
            "{other:?} vs resolved Unknown"
        );
    }
    assert!(!RetentionPosture::Unknown.is_resolved());
    assert!(RetentionPosture::ProviderDefault.is_resolved());
}

#[test]
fn retention_requirements_only_accept_equal_or_stronger_postures() {
    use RetentionPosture::{LimitedRetention, ProviderDefault, ZeroRetention};

    assert!(ZeroRetention.satisfied_by(ZeroRetention));
    assert!(!ZeroRetention.satisfied_by(LimitedRetention { days: 1 }));
    assert!(!ZeroRetention.satisfied_by(ProviderDefault));

    assert!(LimitedRetention { days: 30 }.satisfied_by(ZeroRetention));
    assert!(LimitedRetention { days: 30 }.satisfied_by(LimitedRetention { days: 30 }));
    assert!(LimitedRetention { days: 30 }.satisfied_by(LimitedRetention { days: 7 }));
    assert!(!LimitedRetention { days: 7 }.satisfied_by(LimitedRetention { days: 30 }));
    assert!(!LimitedRetention { days: 7 }.satisfied_by(ProviderDefault));

    assert!(ProviderDefault.satisfied_by(ProviderDefault));
    assert!(ProviderDefault.satisfied_by(ZeroRetention));
}

// ---------------------------------------------------------------------------
// GrantScope
// ---------------------------------------------------------------------------

#[test]
fn ask_every_time_covers_nothing() {
    let route = route();
    assert!(!GrantScope::AskEveryTime.covers(RequestScope {
        recording_id: "rec-1",
        route: &route
    }));
}

#[test]
fn recording_scope_matches_only_its_recording() {
    let route = route();
    let scope = GrantScope::Recording {
        recording_id: "rec-1".to_owned(),
    };
    assert!(scope.covers(RequestScope {
        recording_id: "rec-1",
        route: &route
    }));
    assert!(!scope.covers(RequestScope {
        recording_id: "rec-2",
        route: &route
    }));
}

#[test]
fn model_policy_scope_matches_only_its_exact_route() {
    let scope = GrantScope::ModelPolicy { route: route() };
    let same = route();
    let other = ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large");
    assert!(scope.covers(RequestScope {
        recording_id: "anything",
        route: &same
    }));
    assert!(!scope.covers(RequestScope {
        recording_id: "anything",
        route: &other
    }));
}

// ---------------------------------------------------------------------------
// ConsentGrant lifecycle
// ---------------------------------------------------------------------------

#[test]
fn model_policy_constructor_keeps_scope_and_route_set_in_sync() {
    let grant = ConsentGrant::for_model_policy(
        "g",
        route(),
        ConsentPurpose::Summarization,
        DISCLOSURE,
        NOW,
    );
    assert!(grant.routes.contains(&route()));
    assert!(grant.covers_route(&route()));
    assert_eq!(grant.scope, GrantScope::ModelPolicy { route: route() });
}

#[test]
fn a_recorded_revocation_is_final_even_if_the_clock_moves_backwards() {
    let mut grant = ConsentGrant::new(
        "g",
        GrantScope::AskEveryTime,
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW,
    );
    grant.revoked_at_unix = Some(NOW + 10_000);
    assert!(grant.is_revoked());
    assert!(!grant.is_active(NOW));
    assert!(!grant.is_active(NOW - 10_000));
}

#[test]
fn expiry_is_inclusive_of_the_expiry_second() {
    let grant = ConsentGrant::new(
        "g",
        GrantScope::AskEveryTime,
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW,
    )
    .with_expiry(NOW + 60);
    assert!(!grant.is_expired(NOW + 59));
    assert!(grant.is_expired(NOW + 60));
    assert!(grant.is_expired(NOW + 61));
}

// ---------------------------------------------------------------------------
// ConsentLedger
// ---------------------------------------------------------------------------

fn sample_grant(id: &str) -> ConsentGrant {
    ConsentGrant::for_model_policy(id, route(), ConsentPurpose::Summarization, DISCLOSURE, NOW)
        .with_content_type(ContentType::TranscriptText)
}

#[test]
fn ledger_revocation_is_idempotent_and_keeps_the_first_timestamp() {
    let mut ledger = ConsentLedger::new();
    ledger.grant(sample_grant("g"));

    assert!(ledger.revoke("g", NOW + 1));
    assert!(!ledger.revoke("g", NOW + 2));
    assert_eq!(ledger.get("g").unwrap().revoked_at_unix, Some(NOW + 1));
    assert!(!ledger.revoke("missing", NOW));
}

#[test]
fn revoked_grants_remain_in_the_ledger_as_evidence() {
    let mut ledger = ConsentLedger::new();
    ledger.grant(sample_grant("g"));
    ledger.revoke("g", NOW);

    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger.active_grants(NOW).len(), 0);
    assert_eq!(ledger.all_grants().count(), 1);
}

#[test]
fn revoke_all_for_route_also_catches_model_policy_scopes() {
    let mut ledger = ConsentLedger::new();
    // A grant whose route set is empty but whose scope names the route.
    let mut scoped = ConsentGrant::new(
        "scoped",
        GrantScope::ModelPolicy { route: route() },
        ConsentPurpose::Summarization,
        DISCLOSURE,
        NOW,
    );
    scoped.routes.clear();
    ledger.grant(scoped);

    assert_eq!(ledger.revoke_all_for_route(&route(), NOW), 1);
    assert_eq!(ledger.get("scoped").unwrap().revoked_at_unix, Some(NOW));
}

#[test]
fn ledger_implements_the_repository_seam() {
    let mut ledger = ConsentLedger::new();
    let grant = sample_grant("g");
    let result: Result<(), Infallible> = ledger.upsert_grant(&grant);
    result.unwrap();
    assert_eq!(ledger.load_grants().unwrap().len(), 1);
    ledger.mark_revoked("g", NOW).unwrap();
    assert!(ledger.get("g").unwrap().is_revoked());
}

#[test]
fn persisted_helpers_only_mutate_memory_after_the_repository_accepts() {
    /// A repository that refuses every write, to prove the ledger does not
    /// diverge from storage when persistence fails.
    #[derive(Debug, Default)]
    struct FailingRepository;

    #[derive(Debug, PartialEq, Eq)]
    struct StoreFull;

    impl ConsentRepository for FailingRepository {
        type Error = StoreFull;

        fn load_grants(&self) -> Result<Vec<ConsentGrant>, Self::Error> {
            Err(StoreFull)
        }
        fn upsert_grant(&mut self, _grant: &ConsentGrant) -> Result<(), Self::Error> {
            Err(StoreFull)
        }
        fn mark_revoked(&mut self, _id: &str, _at: i64) -> Result<(), Self::Error> {
            Err(StoreFull)
        }
    }

    let mut repository = FailingRepository;
    let mut ledger = ConsentLedger::new();
    assert_eq!(
        ledger
            .grant_persisted(&mut repository, sample_grant("g"))
            .unwrap_err(),
        StoreFull
    );
    assert!(
        ledger.is_empty(),
        "a failed write must not leave a phantom grant in memory"
    );

    // With an accepting repository the grant lands in both places.
    let mut accepting = ConsentLedger::new();
    let mut ledger = ConsentLedger::new();
    ledger
        .grant_persisted(&mut accepting, sample_grant("g"))
        .unwrap();
    assert_eq!(accepting.len(), 1);
    assert_eq!(ledger.len(), 1);

    assert!(ledger.revoke_persisted(&mut accepting, "g", NOW).unwrap());
    assert!(accepting.get("g").unwrap().is_revoked());
    assert!(!ledger.revoke_persisted(&mut accepting, "g", NOW).unwrap());
}

// ---------------------------------------------------------------------------
// Preflight wiring
// ---------------------------------------------------------------------------

#[test]
fn every_preflight_attempt_is_recorded_by_the_egress_observer() {
    let observer = Arc::new(RecordingObserver::new(16));
    let policy = permitted().with_observer(observer.clone());
    let request = DispatchRequest::new(
        ConsentPurpose::Summarization,
        ContentType::TranscriptText,
        route(),
        "rec-1",
        DISCLOSURE,
    );

    // Denied by consent, but the egress decision still happened and was logged.
    let err = dispatch_preflight(&policy, &[], &request, NOW).unwrap_err();
    assert_eq!(
        err.envelope_parts().code.as_str(),
        CONSENT_REQUIRED.as_str()
    );
    let records = observer.records();
    assert_eq!(records.len(), 1);
    assert!(records[0].allowed);
    assert_eq!(records[0].payload, PayloadClass::TranscriptText);
}

#[test]
fn credential_validation_is_labelled_as_a_credential_payload() {
    let policy = permitted();
    let grant = ConsentGrant::for_model_policy(
        "g",
        route(),
        ConsentPurpose::CredentialValidation,
        DISCLOSURE,
        NOW,
    )
    .with_content_type(ContentType::MetadataOnly);
    let request = DispatchRequest::new(
        ConsentPurpose::CredentialValidation,
        ContentType::MetadataOnly,
        route(),
        "rec-1",
        DISCLOSURE,
    );

    let approval = dispatch_preflight(&policy, &[grant], &request, NOW).unwrap();
    assert_eq!(approval.ticket.payload(), PayloadClass::Credential);
    assert_eq!(approval.ticket.purpose().label(), "credential_validation");
}

#[test]
fn the_most_specific_near_miss_decides_the_reported_error() {
    let policy = permitted();
    // One irrelevant revoked grant for another route, one relevant grant whose
    // content type does not cover the request.
    let mut revoked_elsewhere = ConsentGrant::for_model_policy(
        "elsewhere",
        ProviderRoute::new(RELAY_OPENROUTER, "vendor-y/model-small"),
        ConsentPurpose::Summarization,
        DISCLOSURE,
        NOW,
    )
    .with_content_type(ContentType::TranscriptText);
    revoked_elsewhere.revoked_at_unix = Some(NOW);

    let near_miss = ConsentGrant::for_model_policy(
        "near",
        route(),
        ConsentPurpose::Summarization,
        DISCLOSURE,
        NOW,
    )
    .with_content_type(ContentType::AudioBytes);

    let request = DispatchRequest::new(
        ConsentPurpose::Summarization,
        ContentType::TranscriptText,
        route(),
        "rec-1",
        DISCLOSURE,
    );

    let err =
        dispatch_preflight(&policy, &[revoked_elsewhere, near_miss], &request, NOW).unwrap_err();
    match err {
        ConsentError::ScopeMismatch { axis, grant_id } => {
            assert_eq!(axis, ScopeAxis::ContentType);
            assert_eq!(grant_id, "near");
        }
        other => panic!("expected a content-type scope mismatch, got {other:?}"),
    }
}

#[test]
fn consent_error_stays_small_enough_to_return_by_value() {
    // `Result<DispatchApproval, ConsentError>` is returned on every dispatch;
    // keeping the error under the clippy `result_large_err` threshold is a
    // deliberate design constraint, not an accident.
    assert!(std::mem::size_of::<ConsentError>() <= 128);
}

#[test]
fn scope_axis_explanations_are_fixed_vocabulary() {
    for axis in [
        ScopeAxis::Route,
        ScopeAxis::ContentType,
        ScopeAxis::RecordingScope,
        ScopeAxis::RetentionRequirement,
        ScopeAxis::RetentionPosture,
    ] {
        assert_ne!(axis.explanation().len(), 0);
        assert_ne!(axis.label().len(), 0);
        assert!(
            !axis.explanation().contains('{'),
            "{} must not be a format string",
            axis.label()
        );
    }
}
