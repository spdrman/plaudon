//! The consent preflight's refusal vocabulary and near-miss ranking (§16.3,
//! §4.4, §10.8).
//!
//! `tests/consent_dispatch_contract.rs` proves each axis refuses. This suite
//! covers what that contract does not: the diagnosis chosen when *several*
//! grants are near misses, the prompt a refusal hands back, the rendered
//! sentence of every error variant, the full retention-posture table, and the
//! ledger operations that answer "no" rather than mutating.

use std::collections::BTreeSet;

use plaudon_security::consent::{
    dispatch_preflight, ConsentError, ConsentGrant, ConsentLedger, ConsentPurpose,
    ConsentRepository, ContentType, DispatchRequest, GrantScope, PayloadClassNotConsentable,
    ProviderRoute, RequestScope, RetentionPosture, ScopeAxis, RELAY_OPENROUTER,
};
use plaudon_security::error::SecurityError;
use plaudon_security::outbound::{
    ApprovalSource, DenyReason, EgressPurpose, NetworkGrant, OutboundPolicy, PayloadClass,
};

const DISCLOSURE: &str = "privacy-disclosure-2026-02";
const NOW: i64 = 1_770_000_000;

fn permitted() -> OutboundPolicy {
    OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        NOW - 86_400,
        DISCLOSURE,
    ))
}

fn route() -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large")
        .with_downstream_provider("vendor-x")
}

fn other_route() -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, "vendor-y/model-small")
        .with_downstream_provider("vendor-y")
}

fn request() -> DispatchRequest {
    DispatchRequest::new(
        ConsentPurpose::Transcription,
        ContentType::AudioBytes,
        route(),
        "rec-1",
        DISCLOSURE,
    )
}

fn grant(id: &str) -> ConsentGrant {
    ConsentGrant::new(
        id,
        GrantScope::Recording {
            recording_id: "rec-1".to_owned(),
        },
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW - 60,
    )
    .with_content_type(ContentType::AudioBytes)
    .with_route(route())
}

/// With no grant at all the user is prompted, and the prompt carries everything
/// §4.4 requires the sheet to show — identifiers and labels, never content.
#[test]
fn the_prompt_handed_back_on_a_bare_refusal_carries_the_whole_sheet() {
    let policy = permitted();
    let req = request()
        .requiring_privacy_restriction(true)
        .with_resolved_retention(RetentionPosture::LimitedRetention { days: 30 });

    let err = dispatch_preflight(&policy, &[], &req, NOW).unwrap_err();
    let ConsentError::Required { prompt } = err else {
        panic!("expected a prompt, got {err:?}")
    };

    assert_eq!(prompt.purpose, ConsentPurpose::Transcription);
    assert_eq!(prompt.content_type, ContentType::AudioBytes);
    assert_eq!(prompt.route, route());
    assert_eq!(prompt.destination_label, "https://openrouter.ai:443");
    assert_eq!(
        prompt.resolved_retention,
        RetentionPosture::LimitedRetention { days: 30 }
    );
    assert!(prompt.requires_privacy_restriction);
    assert_eq!(prompt.disclosure_version, DISCLOSURE);
    assert_eq!(prompt.recording_id, "rec-1");
    assert_eq!(
        prompt.offered_scopes,
        vec![
            GrantScope::AskEveryTime,
            GrantScope::Recording {
                recording_id: "rec-1".to_owned()
            },
            GrantScope::ModelPolicy { route: route() },
        ],
        "the sheet offers the three §4.4 choices in order"
    );
}

/// A request whose posture was never resolved shows `Unknown` in the prompt
/// rather than an optimistic default.
#[test]
fn an_unresolved_posture_is_shown_as_unknown_in_the_prompt() {
    let policy = permitted();
    let err = dispatch_preflight(&policy, &[], &request(), NOW).unwrap_err();
    let ConsentError::Required { prompt } = err else {
        panic!("expected a prompt")
    };
    assert_eq!(prompt.resolved_retention, RetentionPosture::Unknown);
    assert!(!prompt.requires_privacy_restriction);
}

/// When several grants are near misses the most specific diagnosis wins, so the
/// user is told the thing that is closest to being fixable rather than the
/// first failure in iteration order.
#[test]
fn the_most_specific_near_miss_decides_the_reported_refusal() {
    let policy = permitted();
    let req = request();

    // Rank order: route < content type < scope < revoked < expired < disclosure.
    let wrong_route = grant("a-route").with_routes_replaced([other_route()]);
    let wrong_content = ConsentGrant::new(
        "b-content",
        GrantScope::Recording {
            recording_id: "rec-1".to_owned(),
        },
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW - 60,
    )
    .with_content_type(ContentType::TranscriptText)
    .with_route(route());
    let wrong_scope = grant("c-scope").with_routes_replaced([route()]);
    let wrong_scope = ConsentGrant {
        scope: GrantScope::Recording {
            recording_id: "rec-2".to_owned(),
        },
        ..wrong_scope
    };

    // Only the route mismatch is present: that is the diagnosis.
    let err =
        dispatch_preflight(&policy, std::slice::from_ref(&wrong_route), &req, NOW).unwrap_err();
    assert!(
        matches!(&err, ConsentError::ScopeMismatch { axis, grant_id }
            if *axis == ScopeAxis::Route && grant_id == "a-route"),
        "got {err:?}"
    );

    // Content type outranks route.
    let err = dispatch_preflight(
        &policy,
        &[wrong_route.clone(), wrong_content.clone()],
        &req,
        NOW,
    )
    .unwrap_err();
    assert!(
        matches!(&err, ConsentError::ScopeMismatch { axis, grant_id }
            if *axis == ScopeAxis::ContentType && grant_id == "b-content"),
        "got {err:?}"
    );

    // Scope outranks content type.
    let err = dispatch_preflight(
        &policy,
        &[
            wrong_route.clone(),
            wrong_content.clone(),
            wrong_scope.clone(),
        ],
        &req,
        NOW,
    )
    .unwrap_err();
    assert!(
        matches!(&err, ConsentError::ScopeMismatch { axis, grant_id }
            if *axis == ScopeAxis::RecordingScope && grant_id == "c-scope"),
        "got {err:?}"
    );

    // Revocation outranks every scope diagnosis.
    let mut revoked = grant("d-revoked");
    revoked.revoked_at_unix = Some(NOW - 10);
    let err = dispatch_preflight(
        &policy,
        &[
            wrong_route.clone(),
            wrong_content.clone(),
            wrong_scope.clone(),
            revoked.clone(),
        ],
        &req,
        NOW,
    )
    .unwrap_err();
    assert!(
        matches!(&err, ConsentError::Revoked { grant_id, revoked_at_unix }
            if grant_id == "d-revoked" && *revoked_at_unix == NOW - 10),
        "got {err:?}"
    );

    // Expiry outranks revocation of a *different* grant.
    let expired = grant("e-expired").with_expiry(NOW - 1);
    let err =
        dispatch_preflight(&policy, &[revoked.clone(), expired.clone()], &req, NOW).unwrap_err();
    assert!(
        matches!(&err, ConsentError::Expired { grant_id, expired_at_unix }
            if grant_id == "e-expired" && *expired_at_unix == NOW - 1),
        "got {err:?}"
    );

    // A stale disclosure outranks all of them.
    let stale = ConsentGrant::new(
        "f-disclosure",
        GrantScope::Recording {
            recording_id: "rec-1".to_owned(),
        },
        ConsentPurpose::Transcription,
        "privacy-disclosure-2024-01",
        NOW - 60,
    )
    .with_content_type(ContentType::AudioBytes)
    .with_route(route());
    let err = dispatch_preflight(&policy, &[revoked, expired, stale], &req, NOW).unwrap_err();
    assert!(
        matches!(&err, ConsentError::DisclosureChanged { grant_id, granted_version, current_version }
            if grant_id == "f-disclosure"
                && granted_version == "privacy-disclosure-2024-01"
                && current_version == DISCLOSURE),
        "got {err:?}"
    );
}

/// "Ask every time" outranks every near-miss diagnosis and produces the prompt,
/// because it is a recorded preference and not a failed approval.
#[test]
fn ask_every_time_outranks_every_near_miss_and_produces_the_prompt() {
    let policy = permitted();
    let ask = ConsentGrant::new(
        "ask",
        GrantScope::AskEveryTime,
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW - 60,
    )
    .with_content_type(ContentType::AudioBytes)
    .with_route(route());

    let stale = ConsentGrant::new(
        "stale",
        GrantScope::Recording {
            recording_id: "rec-1".to_owned(),
        },
        ConsentPurpose::Transcription,
        "privacy-disclosure-2024-01",
        NOW - 60,
    )
    .with_content_type(ContentType::AudioBytes)
    .with_route(route());

    let err = dispatch_preflight(&policy, &[stale, ask], &request(), NOW).unwrap_err();
    assert!(
        matches!(&err, ConsentError::Required { prompt } if prompt.recording_id == "rec-1"),
        "got {err:?}"
    );
    assert_eq!(err.envelope_parts().code.as_str(), "CONSENT_REQUIRED");
}

/// A grant for a different purpose is not a near miss at all: it never shapes
/// the reported error, which stays the bare prompt.
#[test]
fn a_grant_for_another_purpose_never_shapes_the_diagnosis() {
    let policy = permitted();
    let summarization = ConsentGrant::new(
        "summ",
        GrantScope::Recording {
            recording_id: "rec-9".to_owned(),
        },
        ConsentPurpose::Summarization,
        "privacy-disclosure-2024-01",
        NOW - 60,
    )
    .with_content_type(ContentType::TranscriptText)
    .with_route(other_route());

    let err = dispatch_preflight(&policy, &[summarization], &request(), NOW).unwrap_err();
    assert!(
        matches!(err, ConsentError::Required { .. }),
        "a different purpose must not be reported as a scope mismatch: {err:?}"
    );
}

/// Validating a credential is labelled as a credential payload at the gate
/// whatever content type the caller declared (§4.4(3)).
#[test]
fn credential_validation_is_labelled_as_a_credential_at_the_egress_gate() {
    let policy = permitted();
    let req = DispatchRequest::new(
        ConsentPurpose::CredentialValidation,
        ContentType::MetadataOnly,
        route(),
        "rec-1",
        DISCLOSURE,
    );
    assert_eq!(req.egress_purpose(), EgressPurpose::CredentialValidation);
    assert_eq!(req.payload_class(), PayloadClass::Metadata);

    let err = dispatch_preflight(&policy, &[], &req, NOW).unwrap_err();
    assert!(matches!(err, ConsentError::Required { .. }), "{err:?}");

    let records = policy.observer().records();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].payload,
        PayloadClass::Credential,
        "the key itself is what would be sent, so that is what is recorded"
    );
    assert_eq!(records[0].purpose, EgressPurpose::CredentialValidation);
    assert!(
        records[0].allowed,
        "the gate allowed it; consent refused it"
    );
}

/// Every consent purpose maps onto exactly one egress purpose, the three
/// content-bearing purposes share the inference dispatch, and the two
/// per-action purposes keep egress purposes of their own.
#[test]
fn every_consent_purpose_maps_onto_one_egress_purpose() {
    let cases = [
        (
            ConsentPurpose::CredentialValidation,
            EgressPurpose::CredentialValidation,
        ),
        (
            ConsentPurpose::Transcription,
            EgressPurpose::InferenceDispatch,
        ),
        (
            ConsentPurpose::Summarization,
            EgressPurpose::InferenceDispatch,
        ),
        (
            ConsentPurpose::TranscriptTool,
            EgressPurpose::InferenceDispatch,
        ),
        // The two per-action purposes (#303) map onto egress purposes the
        // relay subsystem refuses, so they can never ride a dispatch.
        (ConsentPurpose::ModelDownload, EgressPurpose::ModelDownload),
        (
            ConsentPurpose::DependencyInstall,
            EgressPurpose::AssetDownload,
        ),
    ];
    let mut labels = BTreeSet::new();
    for (consent, egress) in cases {
        let req = DispatchRequest::new(consent, ContentType::MetadataOnly, route(), "r", "d");
        assert_eq!(req.egress_purpose(), egress, "{consent}");
        assert!(labels.insert(consent.label()));
        assert_eq!(format!("{consent}"), consent.label());
    }
    assert_eq!(labels.len(), ConsentPurpose::ALL.len());
}

/// Every content type maps both ways against its payload class, and the three
/// non-consentable classes refuse with a named error rather than a near match.
#[test]
fn the_payload_class_bridge_refuses_the_three_unconsentable_classes() {
    for content in ContentType::ALL {
        let payload = PayloadClass::from(content);
        assert_eq!(ContentType::try_from(payload).unwrap(), content);
        assert_eq!(
            content.is_recording_derived(),
            content != ContentType::MetadataOnly
        );
        assert_eq!(format!("{content}"), content.label());
    }

    for payload in [
        PayloadClass::None,
        PayloadClass::Credential,
        PayloadClass::VoiceBiometric,
    ] {
        let error: PayloadClassNotConsentable = ContentType::try_from(payload)
            .expect_err("this class has no consent-facing content type");
        assert_eq!(error.payload, payload);
        assert_eq!(
            error.to_string(),
            format!(
                "payload class {} has no consent content type",
                payload.label()
            )
        );
        let parts = error.envelope_parts();
        assert_eq!(parts.code.as_str(), "CONSENT_SCOPE_MISMATCH");
        assert!(!parts.retryable);
    }
}

/// The retention table in full: an unresolved posture satisfies nothing and is
/// satisfied by nothing, in either role.
#[test]
fn the_retention_table_is_exhaustive_and_fails_closed_on_unknown() {
    use RetentionPosture::{LimitedRetention, ProviderDefault, Unknown, ZeroRetention};

    let postures = [
        ZeroRetention,
        LimitedRetention { days: 0 },
        LimitedRetention { days: 30 },
        LimitedRetention { days: 90 },
        ProviderDefault,
        Unknown,
    ];

    for required in postures {
        for resolved in postures {
            let expected = match (required, resolved) {
                (Unknown, _) | (_, Unknown) => false,
                (ZeroRetention, ZeroRetention) => true,
                (ZeroRetention, _) => false,
                (LimitedRetention { days: r }, LimitedRetention { days: a }) => a <= r,
                (LimitedRetention { .. }, ZeroRetention) => true,
                (LimitedRetention { .. }, ProviderDefault) => false,
                (ProviderDefault, _) => true,
            };
            assert_eq!(
                required.satisfied_by(resolved),
                expected,
                "{required:?} must {} be satisfied by {resolved:?}",
                if expected { "" } else { "not" }
            );
        }
        assert_eq!(required.is_resolved(), required != Unknown);
    }

    assert_eq!(ZeroRetention.kind_label(), "zero_retention");
    assert_eq!(
        LimitedRetention { days: 7 }.kind_label(),
        "limited_retention"
    );
    assert_eq!(ProviderDefault.kind_label(), "provider_default");
    assert_eq!(Unknown.kind_label(), "unknown");

    assert_eq!(
        LimitedRetention { days: 7 }.to_string(),
        "limited_retention:7d"
    );
    assert_eq!(ZeroRetention.to_string(), "zero_retention");
    assert_eq!(Unknown.to_string(), "unknown");
    assert_eq!(ProviderDefault.to_string(), "provider_default");
}

/// Every refusal renders a one-line sentence that names the rule and the grant,
/// and never carries a recording title or transcript text.
#[test]
fn every_consent_refusal_renders_a_safe_single_line() {
    let policy = permitted();

    let required = dispatch_preflight(&policy, &[], &request(), NOW).unwrap_err();
    assert_eq!(
        required.to_string(),
        "consent required for purpose transcription"
    );

    let mismatch = dispatch_preflight(
        &policy,
        &[grant("g-route").with_routes_replaced([other_route()])],
        &request(),
        NOW,
    )
    .unwrap_err();
    assert_eq!(
        mismatch.to_string(),
        "grant g-route does not cover axis route"
    );

    let mut revoked_grant = grant("g-rev");
    revoked_grant.revoked_at_unix = Some(NOW - 5);
    let revoked = dispatch_preflight(&policy, &[revoked_grant], &request(), NOW).unwrap_err();
    assert_eq!(revoked.to_string(), "grant g-rev was revoked");

    let expired = dispatch_preflight(
        &policy,
        &[grant("g-exp").with_expiry(NOW - 1)],
        &request(),
        NOW,
    )
    .unwrap_err();
    assert_eq!(expired.to_string(), "grant g-exp has expired");

    let stale = ConsentGrant::new(
        "g-old",
        GrantScope::Recording {
            recording_id: "rec-1".to_owned(),
        },
        ConsentPurpose::Transcription,
        "privacy-disclosure-2024-01",
        NOW - 60,
    )
    .with_content_type(ContentType::AudioBytes)
    .with_route(route());
    let changed = dispatch_preflight(&policy, &[stale], &request(), NOW).unwrap_err();
    assert_eq!(
        changed.to_string(),
        "grant g-old predates the current privacy disclosure"
    );

    let unresolved = dispatch_preflight(
        &policy,
        &[grant("g-ok")],
        &request().requiring_privacy_restriction(true),
        NOW,
    )
    .unwrap_err();
    assert_eq!(
        unresolved.to_string(),
        "retention posture unresolved for route openrouter|vendor-x/model-large|vendor-x"
    );
    assert_eq!(
        unresolved.envelope_parts().code.as_str(),
        "PRIVACY_POSTURE_UNRESOLVED"
    );

    let denied = dispatch_preflight(
        &OutboundPolicy::local_only(),
        &[grant("g-ok")],
        &request(),
        NOW,
    )
    .unwrap_err();
    assert!(denied
        .to_string()
        .starts_with("egress denied (local_only_mode)"));
    assert!(std::error::Error::source(&denied).is_none());
}

/// Each scope axis has a distinct label and a fixed-vocabulary explanation that
/// never interpolates a route, a model id or a recording.
#[test]
fn every_scope_axis_has_a_distinct_label_and_a_fixed_explanation() {
    let all = [
        ScopeAxis::Route,
        ScopeAxis::ContentType,
        ScopeAxis::RecordingScope,
        ScopeAxis::RetentionRequirement,
        ScopeAxis::RetentionPosture,
    ];
    let mut labels = BTreeSet::new();
    for axis in all {
        assert!(labels.insert(axis.label()));
        let explanation = axis.explanation();
        assert!(
            !explanation.is_empty(),
            "{} has no explanation",
            axis.label()
        );
        assert!(!explanation.contains("vendor-x"), "{explanation}");
        assert!(!explanation.contains("rec-"), "{explanation}");
        assert_eq!(
            serde_json::to_string(&axis).unwrap(),
            format!("\"{}\"", axis.label())
        );
    }
    assert_eq!(labels.len(), all.len());
}

/// A route whose relay is not pinned is refused by the gate, with no hostname
/// invented for it; a route with no discovered downstream provider says so.
#[test]
fn an_unpinned_relay_is_refused_and_an_undisclosed_provider_is_labelled() {
    let unknown = ProviderRoute::new("vendor-direct", "m");
    let denied = unknown.relay_destination().unwrap_err();
    assert_eq!(denied.reason, DenyReason::DestinationNotAllowlisted);
    assert_eq!(denied.destination_label, "relay:<unrecognised>");
    assert!(!unknown.has_known_downstream_provider());
    assert_eq!(unknown.label(), "vendor-direct|m|<undisclosed>");
    assert_eq!(format!("{unknown}"), unknown.label());

    let known = route();
    assert!(known.has_known_downstream_provider());
    assert_eq!(
        known.relay_destination().unwrap().label(),
        "https://openrouter.ai:443"
    );
    assert_eq!(
        plaudon_security::consent::relay_endpoint(RELAY_OPENROUTER),
        Some("https://openrouter.ai")
    );
    assert_eq!(plaudon_security::consent::relay_endpoint("nope"), None);
}

/// Scope coverage is decided on exact equality; "ask every time" covers nothing.
#[test]
fn grant_scope_coverage_is_exact_and_ask_every_time_covers_nothing() {
    let r = route();
    let other = other_route();
    let scope_request = RequestScope {
        recording_id: "rec-1",
        route: &r,
    };

    assert!(!GrantScope::AskEveryTime.covers(scope_request));
    assert_eq!(GrantScope::AskEveryTime.kind_label(), "ask_every_time");

    let recording = GrantScope::Recording {
        recording_id: "rec-1".to_owned(),
    };
    assert!(recording.covers(scope_request));
    assert_eq!(recording.kind_label(), "recording");
    assert!(!GrantScope::Recording {
        recording_id: "rec-2".to_owned()
    }
    .covers(scope_request));

    let policy_scope = GrantScope::ModelPolicy { route: r.clone() };
    assert!(policy_scope.covers(scope_request));
    assert_eq!(policy_scope.kind_label(), "model_policy");
    assert!(!GrantScope::ModelPolicy { route: other }.covers(scope_request));
}

/// A grant's own predicates: expiry is inclusive of the expiry second, and a
/// recorded revocation is final even when the clock moves backwards.
#[test]
fn grant_predicates_are_inclusive_and_a_revocation_is_final() {
    let base = grant("g").with_expiry(NOW);
    assert!(base.is_expired(NOW), "expiry is inclusive");
    assert!(base.is_expired(NOW + 1));
    assert!(!base.is_expired(NOW - 1));
    assert!(base.is_active(NOW - 1));
    assert!(!base.is_active(NOW));

    let never = grant("g2");
    assert!(!never.is_expired(i64::MAX));
    assert!(never.is_active(i64::MAX));

    let mut revoked = grant("g3");
    revoked.revoked_at_unix = Some(NOW);
    assert!(revoked.is_revoked());
    assert!(!revoked.is_active(i64::MIN));

    let model_policy = ConsentGrant::for_model_policy(
        "mp",
        route(),
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW,
    );
    assert!(model_policy.covers_route(&route()));
    assert!(!model_policy.covers_route(&other_route()));
    assert_eq!(
        model_policy.scope,
        GrantScope::ModelPolicy { route: route() }
    );

    // A grant whose route set was replaced still covers through its scope.
    let scoped_only = ConsentGrant {
        routes: BTreeSet::new(),
        ..model_policy
    };
    assert!(scoped_only.covers_route(&route()));

    let with_retention = grant("g4").with_required_retention(RetentionPosture::ZeroRetention);
    assert_eq!(
        with_retention.required_retention,
        Some(RetentionPosture::ZeroRetention)
    );
    let with_set = grant("g5").with_content_types(BTreeSet::from([
        ContentType::AudioBytes,
        ContentType::TranscriptText,
    ]));
    assert_eq!(with_set.content_types.len(), 2);
}

/// The ledger answers "no" rather than mutating: revoking an absent or already
/// revoked grant returns `false` and leaves the recorded timestamp alone.
#[test]
fn revoking_an_absent_or_already_revoked_grant_changes_nothing() {
    let mut ledger = ConsentLedger::new();
    assert!(ledger.is_empty(), "a fresh ledger holds nothing");
    assert_eq!(ledger.len(), 0);
    assert!(ledger.get("missing").is_none());
    assert!(!ledger.revoke("missing", NOW));

    assert!(ledger.grant(grant("g1")).is_none());
    assert!(
        ledger.grant(grant("g1")).is_some(),
        "replacing by id returns the previous grant"
    );
    assert_eq!(ledger.len(), 1);
    assert!(!ledger.is_empty(), "the grant was recorded");

    assert!(ledger.revoke("g1", NOW));
    assert!(
        !ledger.revoke("g1", NOW - 100),
        "the first revocation stands"
    );
    assert_eq!(ledger.get("g1").unwrap().revoked_at_unix, Some(NOW));

    assert!(
        ledger.active_grants(NOW).is_empty(),
        "a revoked grant is not active"
    );
    assert_eq!(ledger.all_grants().count(), 1, "history is never deleted");
    assert_eq!(ledger.snapshot().len(), 1);

    // Nothing covers a route that no grant names.
    assert_eq!(ledger.revoke_all_for_route(&other_route(), NOW), 0);
    // And an already-revoked grant is not revoked twice.
    assert_eq!(ledger.revoke_all_for_route(&route(), NOW), 0);
}

/// The persisted variants only touch the in-memory ledger after the repository
/// accepted the write, and they short-circuit before writing when there is
/// nothing to revoke.
#[test]
fn the_persisted_ledger_operations_short_circuit_before_writing() {
    let mut backing = ConsentLedger::new();
    let mut ledger = ConsentLedger::new();

    ledger
        .grant_persisted(&mut backing, grant("g1"))
        .expect("infallible");
    assert_eq!(backing.len(), 1);
    assert_eq!(ledger.len(), 1);

    assert!(!ledger
        .revoke_persisted(&mut backing, "missing", NOW)
        .expect("infallible"));
    assert!(ledger
        .revoke_persisted(&mut backing, "g1", NOW)
        .expect("infallible"));
    assert!(!ledger
        .revoke_persisted(&mut backing, "g1", NOW + 1)
        .expect("infallible"));
    assert_eq!(backing.get("g1").unwrap().revoked_at_unix, Some(NOW));

    let reloaded = ConsentLedger::load_from(&backing).expect("infallible");
    assert_eq!(reloaded.len(), 1);
    assert!(reloaded.get("g1").unwrap().is_revoked());

    let from_grants = ConsentLedger::from_grants([grant("a"), grant("b")]);
    assert_eq!(from_grants.len(), 2);
    assert_eq!(from_grants.active_grants(NOW).len(), 2);
    assert_eq!(
        from_grants.load_grants().expect("infallible").len(),
        2,
        "the ledger is itself a volatile repository"
    );
}

/// `revoke_all_for_route` revokes exactly the grants that name the route, once.
#[test]
fn revoking_a_route_touches_only_the_grants_that_name_it() {
    let mut ledger = ConsentLedger::from_grants([
        grant("on-route"),
        grant("off-route").with_routes_replaced([other_route()]),
        ConsentGrant::for_model_policy(
            "policy",
            route(),
            ConsentPurpose::Summarization,
            DISCLOSURE,
            NOW,
        ),
    ]);
    assert_eq!(ledger.revoke_all_for_route(&route(), NOW), 2);
    assert_eq!(ledger.revoke_all_for_route(&route(), NOW + 1), 0);
    assert!(!ledger.get("off-route").unwrap().is_revoked());
    assert_eq!(ledger.active_grants(NOW).len(), 1);
}
