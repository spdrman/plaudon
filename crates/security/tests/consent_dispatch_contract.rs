//! Contract tests for D1.3: scoped consent grants and the dispatch preflight.
//!
//! Spec references: §16.3 (consent is a scoped capability, not a global
//! checkbox), §4.4 (Ask every time / Allow for this recording / Allow for this
//! model-policy, versioned disclosure), §10.8 (a privacy policy must resolve for
//! the exact endpoint and route; changing model/route/data type/retention
//! invalidates the grant), §18.6A Cloud cluster (consent revoked mid-queue),
//! §16.2A (composition with Local-only Mode), §22.3.
//!
//! Every test here is hermetic: no network, no cloud, no device. The egress gate
//! is exercised through [`OutboundPolicy`], which never opens a socket.

use std::collections::BTreeSet;

use plaudon_security::consent::{
    dispatch_preflight, ConsentError, ConsentGrant, ConsentLedger, ConsentPurpose,
    ConsentRepository, ContentType, DispatchRequest, GrantScope, ProviderRoute, RetentionPosture,
    ScopeAxis, RELAY_OPENROUTER,
};
use plaudon_security::error::{
    SecurityError, CONSENT_DISCLOSURE_CHANGED, CONSENT_REQUIRED, CONSENT_REVOKED,
    CONSENT_SCOPE_MISMATCH, EGRESS_DENIED, PRIVACY_POSTURE_UNRESOLVED,
};
use plaudon_security::outbound::{
    ApprovalSource, DenyReason, NetworkGrant, OutboundPolicy, PayloadClass,
};

const DISCLOSURE: &str = "privacy-disclosure-2026-02";
const OLD_DISCLOSURE: &str = "privacy-disclosure-2025-11";
const NOW: i64 = 1_770_000_000;

fn permitted_policy() -> OutboundPolicy {
    OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        NOW - 86_400,
        DISCLOSURE,
    ))
}

fn route_a() -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large")
        .with_downstream_provider("vendor-x")
}

fn route_b() -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, "vendor-y/model-small")
        .with_downstream_provider("vendor-y")
}

/// Same model id as [`route_a`], different downstream inference provider.
fn route_a_other_downstream() -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large")
        .with_downstream_provider("reseller-z")
}

/// Same model id as [`route_a`], downstream provider not discoverable (§10.8).
fn route_a_unknown_downstream() -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model-large")
}

fn grant_for_recording(id: &str, recording: &str, content: ContentType) -> ConsentGrant {
    ConsentGrant::new(
        id,
        GrantScope::Recording {
            recording_id: recording.to_owned(),
        },
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW - 60,
    )
    .with_content_type(content)
    .with_route(route_a())
}

fn request(content: ContentType, route: ProviderRoute, recording: &str) -> DispatchRequest {
    DispatchRequest::new(
        ConsentPurpose::Transcription,
        content,
        route,
        recording,
        DISCLOSURE,
    )
}

fn code_of(err: &ConsentError) -> &'static str {
    err.envelope_parts().code.as_str()
}

// ---------------------------------------------------------------------------
// Step 1: the egress gate always runs first (§16.2A composition)
// ---------------------------------------------------------------------------

#[test]
fn local_only_mode_denies_even_with_a_perfectly_matching_grant() {
    let policy = OutboundPolicy::local_only();
    let grants = vec![grant_for_recording("g1", "rec-1", ContentType::AudioBytes)];
    let req = request(ContentType::AudioBytes, route_a(), "rec-1");

    let err = dispatch_preflight(&policy, &grants, &req, NOW).unwrap_err();
    match &err {
        ConsentError::Egress(denied) => assert_eq!(denied.reason, DenyReason::LocalOnlyMode),
        other => panic!("expected an egress denial, got {other:?}"),
    }
    assert_eq!(code_of(&err), EGRESS_DENIED.as_str());
}

#[test]
fn egress_gate_runs_before_consent_so_local_only_is_not_bypassable_by_grant_count() {
    let policy = OutboundPolicy::local_only();
    // No grants at all: the error must still be EGRESS_DENIED, not CONSENT_REQUIRED,
    // because the gate is step 1 and cannot be reordered.
    let req = request(ContentType::TranscriptText, route_a(), "rec-1");
    let err = dispatch_preflight(&policy, &[], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), EGRESS_DENIED.as_str());
}

#[test]
fn unknown_relay_is_refused_by_the_egress_gate_rather_than_guessed() {
    let policy = permitted_policy();
    let route = ProviderRoute::new("some-other-relay", "vendor-x/model-large");
    let grant = ConsentGrant::new(
        "g1",
        GrantScope::ModelPolicy {
            route: route.clone(),
        },
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW - 60,
    )
    .with_content_type(ContentType::TranscriptText)
    .with_route(route.clone());
    let req = request(ContentType::TranscriptText, route, "rec-1");

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), EGRESS_DENIED.as_str());
}

#[test]
fn approval_carries_the_egress_ticket() {
    let policy = permitted_policy();
    let grants = vec![grant_for_recording("g1", "rec-1", ContentType::AudioBytes)];
    let req = request(ContentType::AudioBytes, route_a(), "rec-1");

    let approval = dispatch_preflight(&policy, &grants, &req, NOW).unwrap();
    assert_eq!(approval.grant_id, "g1");
    assert_eq!(approval.content_type, ContentType::AudioBytes);
    assert_eq!(approval.route, route_a());
    assert_eq!(approval.ticket.payload(), PayloadClass::AudioBytes);
    assert_eq!(
        approval.ticket.destination_label(),
        "https://openrouter.ai:443"
    );
}

// ---------------------------------------------------------------------------
// §16.3: an API key is never consent
// ---------------------------------------------------------------------------

#[test]
fn api_key_presence_is_not_consent() {
    // `crates/security/src/secrets.rs` is owned by a sibling task, so this test
    // models key presence locally rather than coupling to an API that may not
    // exist yet. The point of the test is the implication, not the store: a
    // configured credential contributes nothing to the preflight decision.
    struct ApiKeyPresence {
        configured: bool,
    }
    let key = ApiKeyPresence { configured: true };
    assert!(
        key.configured,
        "the fixture models a user who has saved an OpenRouter key"
    );

    let policy = permitted_policy();
    let req = request(ContentType::TranscriptText, route_a(), "rec-1");
    let err = dispatch_preflight(&policy, &[], &req, NOW).unwrap_err();

    assert_eq!(code_of(&err), CONSENT_REQUIRED.as_str());
    match err {
        ConsentError::Required { prompt } => {
            assert_eq!(prompt.route, route_a());
            assert_eq!(prompt.content_type, ContentType::TranscriptText);
            assert_eq!(prompt.disclosure_version, DISCLOSURE);
            assert_eq!(
                prompt.offered_scopes.len(),
                3,
                "§4.4 offers exactly three choices"
            );
        }
        other => panic!("expected ConsentError::Required, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// §4.4 scopes
// ---------------------------------------------------------------------------

#[test]
fn ask_every_time_never_satisfies_a_preflight_automatically() {
    let policy = permitted_policy();
    let grant = ConsentGrant::new(
        "g-ask",
        GrantScope::AskEveryTime,
        ConsentPurpose::Transcription,
        DISCLOSURE,
        NOW - 60,
    )
    .with_content_type(ContentType::TranscriptText)
    .with_route(route_a());
    let req = request(ContentType::TranscriptText, route_a(), "rec-1");

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_REQUIRED.as_str());
    match err {
        ConsentError::Required { prompt } => {
            assert_eq!(prompt.recording_id, "rec-1");
            assert!(prompt.offered_scopes.contains(&GrantScope::AskEveryTime));
            assert!(prompt.offered_scopes.contains(&GrantScope::Recording {
                recording_id: "rec-1".to_owned()
            }));
            assert!(prompt
                .offered_scopes
                .contains(&GrantScope::ModelPolicy { route: route_a() }));
        }
        other => panic!("expected ConsentError::Required, got {other:?}"),
    }
}

#[test]
fn recording_grant_does_not_cover_another_recording() {
    let policy = permitted_policy();
    let grants = vec![grant_for_recording("g1", "rec-1", ContentType::AudioBytes)];
    let req = request(ContentType::AudioBytes, route_a(), "rec-2");

    let err = dispatch_preflight(&policy, &grants, &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_SCOPE_MISMATCH.as_str());
    assert!(matches!(
        err,
        ConsentError::ScopeMismatch {
            axis: ScopeAxis::RecordingScope,
            ..
        }
    ));
}

#[test]
fn model_policy_grant_covers_every_recording_on_that_route() {
    let policy = permitted_policy();
    let grant = ConsentGrant::for_model_policy(
        "g-policy",
        route_a(),
        ConsentPurpose::Summarization,
        DISCLOSURE,
        NOW - 60,
    )
    .with_content_type(ContentType::TranscriptText);

    for recording in ["rec-1", "rec-2", "rec-3"] {
        let req = DispatchRequest::new(
            ConsentPurpose::Summarization,
            ContentType::TranscriptText,
            route_a(),
            recording,
            DISCLOSURE,
        );
        let approval =
            dispatch_preflight(&policy, std::slice::from_ref(&grant), &req, NOW).unwrap();
        assert_eq!(approval.grant_id, "g-policy");
    }
}

// ---------------------------------------------------------------------------
// Content-type and route axes (§10.8)
// ---------------------------------------------------------------------------

#[test]
fn transcript_grant_does_not_authorize_an_audio_upload() {
    let policy = permitted_policy();
    let grants = vec![grant_for_recording(
        "g1",
        "rec-1",
        ContentType::TranscriptText,
    )];
    let req = request(ContentType::AudioBytes, route_a(), "rec-1");

    let err = dispatch_preflight(&policy, &grants, &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_SCOPE_MISMATCH.as_str());
    assert!(matches!(
        err,
        ConsentError::ScopeMismatch {
            axis: ScopeAxis::ContentType,
            ..
        }
    ));
}

#[test]
fn grant_for_route_a_does_not_authorize_route_b() {
    let policy = permitted_policy();
    let grants = vec![grant_for_recording("g1", "rec-1", ContentType::AudioBytes)];
    let req = request(ContentType::AudioBytes, route_b(), "rec-1");

    let err = dispatch_preflight(&policy, &grants, &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_SCOPE_MISMATCH.as_str());
    assert!(matches!(
        err,
        ConsentError::ScopeMismatch {
            axis: ScopeAxis::Route,
            ..
        }
    ));
}

#[test]
fn same_model_different_downstream_provider_is_a_different_route() {
    let policy = permitted_policy();
    let grants = vec![grant_for_recording("g1", "rec-1", ContentType::AudioBytes)];

    for route in [route_a_other_downstream(), route_a_unknown_downstream()] {
        let req = request(ContentType::AudioBytes, route.clone(), "rec-1");
        let err = dispatch_preflight(&policy, &grants, &req, NOW).unwrap_err();
        assert_eq!(
            code_of(&err),
            CONSENT_SCOPE_MISMATCH.as_str(),
            "route {} must not reuse the grant for {}",
            route.label(),
            route_a().label()
        );
    }
}

#[test]
fn unknown_downstream_provider_grant_does_not_cover_a_named_one() {
    let policy = permitted_policy();
    // The grant was taken while the downstream provider was not discoverable.
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes)
        .with_routes_replaced([route_a_unknown_downstream()]);
    let req = request(ContentType::AudioBytes, route_a(), "rec-1");

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_SCOPE_MISMATCH.as_str());
}

#[test]
fn purpose_mismatch_requires_a_fresh_prompt() {
    let policy = permitted_policy();
    let grants = vec![grant_for_recording(
        "g1",
        "rec-1",
        ContentType::TranscriptText,
    )];
    let req = DispatchRequest::new(
        ConsentPurpose::TranscriptTool,
        ContentType::TranscriptText,
        route_a(),
        "rec-1",
        DISCLOSURE,
    );

    let err = dispatch_preflight(&policy, &grants, &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_REQUIRED.as_str());
}

// ---------------------------------------------------------------------------
// Validity: expiry, revocation, disclosure version
// ---------------------------------------------------------------------------

#[test]
fn expired_grant_fails_the_preflight() {
    let policy = permitted_policy();
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes).with_expiry(NOW - 1);
    let req = request(ContentType::AudioBytes, route_a(), "rec-1");

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_REVOKED.as_str());
    assert!(matches!(err, ConsentError::Expired { .. }));
}

#[test]
fn revoked_grant_fails_the_preflight() {
    let policy = permitted_policy();
    let mut grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes);
    grant.revoked_at_unix = Some(NOW - 1);
    let req = request(ContentType::AudioBytes, route_a(), "rec-1");

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_REVOKED.as_str());
    assert!(matches!(err, ConsentError::Revoked { .. }));
}

#[test]
fn changing_the_disclosure_version_forces_a_reprompt() {
    let policy = permitted_policy();
    let grant = ConsentGrant::new(
        "g1",
        GrantScope::Recording {
            recording_id: "rec-1".to_owned(),
        },
        ConsentPurpose::Transcription,
        OLD_DISCLOSURE,
        NOW - 600,
    )
    .with_content_type(ContentType::AudioBytes)
    .with_route(route_a());
    let req = request(ContentType::AudioBytes, route_a(), "rec-1");

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_DISCLOSURE_CHANGED.as_str());
    match err {
        ConsentError::DisclosureChanged {
            granted_version,
            current_version,
            ..
        } => {
            assert_eq!(granted_version, OLD_DISCLOSURE);
            assert_eq!(current_version, DISCLOSURE);
        }
        other => panic!("expected ConsentError::DisclosureChanged, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// §10.8 retention / privacy posture
// ---------------------------------------------------------------------------

#[test]
fn unresolved_retention_fails_closed_when_a_restriction_is_required() {
    let policy = permitted_policy();
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes)
        .with_required_retention(RetentionPosture::ZeroRetention);

    for resolved in [None, Some(RetentionPosture::Unknown)] {
        let mut req = request(ContentType::AudioBytes, route_a(), "rec-1");
        req.requires_privacy_restriction = true;
        req.resolved_retention = resolved;

        let err = dispatch_preflight(&policy, std::slice::from_ref(&grant), &req, NOW).unwrap_err();
        assert_eq!(
            code_of(&err),
            PRIVACY_POSTURE_UNRESOLVED.as_str(),
            "resolved={resolved:?}"
        );
    }
}

#[test]
fn resolved_but_insufficient_retention_is_a_scope_mismatch() {
    let policy = permitted_policy();
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes)
        .with_required_retention(RetentionPosture::ZeroRetention);
    let mut req = request(ContentType::AudioBytes, route_a(), "rec-1");
    req.requires_privacy_restriction = true;
    req.resolved_retention = Some(RetentionPosture::ProviderDefault);

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_SCOPE_MISMATCH.as_str());
    assert!(matches!(
        err,
        ConsentError::ScopeMismatch {
            axis: ScopeAxis::RetentionPosture,
            ..
        }
    ));
}

#[test]
fn a_grant_without_a_retention_requirement_cannot_satisfy_a_restricted_request() {
    let policy = permitted_policy();
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes);
    let mut req = request(ContentType::AudioBytes, route_a(), "rec-1");
    req.requires_privacy_restriction = true;
    req.resolved_retention = Some(RetentionPosture::ZeroRetention);

    let err = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap_err();
    assert_eq!(code_of(&err), CONSENT_SCOPE_MISMATCH.as_str());
    assert!(matches!(
        err,
        ConsentError::ScopeMismatch {
            axis: ScopeAxis::RetentionRequirement,
            ..
        }
    ));
}

#[test]
fn zero_retention_route_satisfies_a_zero_retention_requirement() {
    let policy = permitted_policy();
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes)
        .with_required_retention(RetentionPosture::ZeroRetention);
    let mut req = request(ContentType::AudioBytes, route_a(), "rec-1");
    req.requires_privacy_restriction = true;
    req.resolved_retention = Some(RetentionPosture::ZeroRetention);

    let approval = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap();
    assert_eq!(approval.grant_id, "g1");
}

#[test]
fn unresolved_retention_is_tolerated_when_no_restriction_was_required() {
    let policy = permitted_policy();
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes);
    let mut req = request(ContentType::AudioBytes, route_a(), "rec-1");
    req.resolved_retention = Some(RetentionPosture::Unknown);

    let approval = dispatch_preflight(&policy, &[grant], &req, NOW).unwrap();
    assert_eq!(approval.grant_id, "g1");
}

// ---------------------------------------------------------------------------
// §18.6A Cloud cluster: consent revoked mid-queue
// ---------------------------------------------------------------------------

#[test]
fn revocation_mid_queue_stops_the_remaining_dispatches() {
    let policy = permitted_policy();
    let mut ledger = ConsentLedger::new();
    ledger.grant(
        ConsentGrant::for_model_policy(
            "g-queue",
            route_a(),
            ConsentPurpose::Summarization,
            DISCLOSURE,
            NOW - 600,
        )
        .with_content_type(ContentType::TranscriptText),
    );

    let queue = ["rec-1", "rec-2", "rec-3"];
    let mut outcomes: Vec<Result<String, &'static str>> = Vec::new();

    for (index, recording) in queue.iter().enumerate() {
        let req = DispatchRequest::new(
            ConsentPurpose::Summarization,
            ContentType::TranscriptText,
            route_a(),
            *recording,
            DISCLOSURE,
        );
        let grants = ledger.snapshot();
        let outcome =
            dispatch_preflight(&policy, &grants, &req, NOW + i64::try_from(index).unwrap());
        outcomes.push(match outcome {
            Ok(approval) => Ok(approval.grant_id),
            Err(err) => Err(code_of(&err)),
        });

        if index == 0 {
            // The user revokes consent while dispatch 2 and 3 are still queued.
            assert!(ledger.revoke("g-queue", NOW));
        }
    }

    assert_eq!(outcomes[0], Ok("g-queue".to_owned()));
    assert_eq!(outcomes[1], Err(CONSENT_REVOKED.as_str()));
    assert_eq!(outcomes[2], Err(CONSENT_REVOKED.as_str()));
    assert!(
        ledger.active_grants(NOW + 10).is_empty(),
        "a revoked grant must not stay active under any 'already approved' notion"
    );
}

#[test]
fn revoke_all_for_route_only_touches_that_route() {
    let mut ledger = ConsentLedger::new();
    ledger.grant(ConsentGrant::for_model_policy(
        "a",
        route_a(),
        ConsentPurpose::Summarization,
        DISCLOSURE,
        NOW - 10,
    ));
    ledger.grant(ConsentGrant::for_model_policy(
        "b",
        route_b(),
        ConsentPurpose::Summarization,
        DISCLOSURE,
        NOW - 10,
    ));
    ledger.grant(
        grant_for_recording("c", "rec-1", ContentType::AudioBytes), // routes = [route_a]
    );

    assert_eq!(ledger.revoke_all_for_route(&route_a(), NOW), 2);
    let active: Vec<&str> = ledger
        .active_grants(NOW)
        .iter()
        .map(|g| g.id.as_str())
        .collect();
    assert_eq!(active, vec!["b"]);
    // Idempotent: a second sweep revokes nothing new.
    assert_eq!(ledger.revoke_all_for_route(&route_a(), NOW + 5), 0);
}

#[test]
fn ledger_round_trips_through_the_repository_trait() {
    let mut ledger = ConsentLedger::new();
    let grant = grant_for_recording("g1", "rec-1", ContentType::AudioBytes);
    ledger.upsert_grant(&grant).unwrap();
    ledger.mark_revoked("g1", NOW).unwrap();

    let restored = ConsentLedger::load_from(&ledger).unwrap();
    assert_eq!(restored.get("g1").unwrap().revoked_at_unix, Some(NOW));
    assert_eq!(restored.active_grants(NOW).len(), 0);
}

// ---------------------------------------------------------------------------
// Table test over the grant × request matrix
// ---------------------------------------------------------------------------

// Exhaustive table test over the grant/request mismatch matrix; splitting it
// would hide the table it is meant to document.
#[allow(clippy::too_many_lines)]
#[test]
fn mismatch_matrix_maps_each_axis_to_its_own_error_code() {
    struct Row {
        name: &'static str,
        mutate_grant: fn(ConsentGrant) -> ConsentGrant,
        mutate_request: fn(DispatchRequest) -> DispatchRequest,
        expected: &'static str,
    }

    let rows = [
        Row {
            name: "everything matches",
            mutate_grant: |g| g,
            mutate_request: |r| r,
            expected: "OK",
        },
        Row {
            name: "purpose differs",
            mutate_grant: |g| g,
            mutate_request: |mut r| {
                r.purpose = ConsentPurpose::TranscriptTool;
                r
            },
            expected: CONSENT_REQUIRED.as_str(),
        },
        Row {
            name: "content type not in grant",
            mutate_grant: |g| g,
            mutate_request: |mut r| {
                r.content_type = ContentType::MetadataOnly;
                r
            },
            expected: CONSENT_SCOPE_MISMATCH.as_str(),
        },
        Row {
            name: "route not in grant",
            mutate_grant: |g| g,
            mutate_request: |mut r| {
                r.route = route_b();
                r
            },
            expected: CONSENT_SCOPE_MISMATCH.as_str(),
        },
        Row {
            name: "downstream provider differs",
            mutate_grant: |g| g,
            mutate_request: |mut r| {
                r.route = route_a_other_downstream();
                r
            },
            expected: CONSENT_SCOPE_MISMATCH.as_str(),
        },
        Row {
            name: "recording outside scope",
            mutate_grant: |g| g,
            mutate_request: |mut r| {
                r.recording_id = "rec-other".to_owned();
                r
            },
            expected: CONSENT_SCOPE_MISMATCH.as_str(),
        },
        Row {
            name: "grant expired",
            mutate_grant: |g| g.with_expiry(NOW - 1),
            mutate_request: |r| r,
            expected: CONSENT_REVOKED.as_str(),
        },
        Row {
            name: "grant revoked",
            mutate_grant: |mut g| {
                g.revoked_at_unix = Some(NOW - 1);
                g
            },
            mutate_request: |r| r,
            expected: CONSENT_REVOKED.as_str(),
        },
        Row {
            name: "disclosure version changed",
            mutate_grant: |mut g| {
                g.disclosure_version = OLD_DISCLOSURE.to_owned();
                g
            },
            mutate_request: |r| r,
            expected: CONSENT_DISCLOSURE_CHANGED.as_str(),
        },
        Row {
            name: "retention required but unresolved",
            mutate_grant: |g| g.with_required_retention(RetentionPosture::ZeroRetention),
            mutate_request: |mut r| {
                r.requires_privacy_restriction = true;
                r.resolved_retention = None;
                r
            },
            expected: PRIVACY_POSTURE_UNRESOLVED.as_str(),
        },
        Row {
            name: "retention resolved but weaker than required",
            mutate_grant: |g| {
                g.with_required_retention(RetentionPosture::LimitedRetention { days: 7 })
            },
            mutate_request: |mut r| {
                r.requires_privacy_restriction = true;
                r.resolved_retention = Some(RetentionPosture::LimitedRetention { days: 30 });
                r
            },
            expected: CONSENT_SCOPE_MISMATCH.as_str(),
        },
        Row {
            name: "ask every time",
            mutate_grant: |mut g| {
                g.scope = GrantScope::AskEveryTime;
                g
            },
            mutate_request: |r| r,
            expected: CONSENT_REQUIRED.as_str(),
        },
    ];

    let policy = permitted_policy();
    for row in rows {
        let grant = (row.mutate_grant)(grant_for_recording("g1", "rec-1", ContentType::AudioBytes));
        let req = (row.mutate_request)(request(ContentType::AudioBytes, route_a(), "rec-1"));
        let outcome = dispatch_preflight(&policy, &[grant], &req, NOW);
        let actual = match &outcome {
            Ok(_) => "OK",
            Err(err) => code_of(err),
        };
        assert_eq!(actual, row.expected, "row: {}", row.name);
    }
}

// ---------------------------------------------------------------------------
// Exhaustive cross product: Ok implies the grant is a superset on every axis
// ---------------------------------------------------------------------------

// Exhaustive cross product over route/retention/content-type axes; splitting
// it would hide the superset property it is meant to document.
#[allow(clippy::too_many_lines)]
#[test]
fn approval_implies_a_superset_grant_on_every_axis() {
    let policy = permitted_policy();

    let routes = [route_a(), route_b(), route_a_unknown_downstream()];
    let retentions = [
        None,
        Some(RetentionPosture::ZeroRetention),
        Some(RetentionPosture::LimitedRetention { days: 30 }),
        Some(RetentionPosture::ProviderDefault),
        Some(RetentionPosture::Unknown),
    ];
    let recordings = ["rec-1", "rec-2"];
    let disclosures = [DISCLOSURE, OLD_DISCLOSURE];

    // A deliberately heterogeneous ledger: one per-recording grant, one
    // model-policy grant with a retention requirement, one ask-every-time
    // preference, one expired grant and one revoked grant.
    let mut audio_and_text = BTreeSet::new();
    audio_and_text.insert(ContentType::AudioBytes);
    audio_and_text.insert(ContentType::TranscriptText);

    let grants = vec![
        ConsentGrant::new(
            "per-recording",
            GrantScope::Recording {
                recording_id: "rec-1".to_owned(),
            },
            ConsentPurpose::Transcription,
            DISCLOSURE,
            NOW - 600,
        )
        .with_content_types(audio_and_text.clone())
        .with_route(route_a()),
        ConsentGrant::for_model_policy(
            "model-policy",
            route_b(),
            ConsentPurpose::Summarization,
            DISCLOSURE,
            NOW - 600,
        )
        .with_content_type(ContentType::TranscriptText)
        .with_required_retention(RetentionPosture::LimitedRetention { days: 30 }),
        ConsentGrant::new(
            "ask",
            GrantScope::AskEveryTime,
            ConsentPurpose::TranscriptTool,
            DISCLOSURE,
            NOW - 600,
        )
        .with_content_type(ContentType::TranscriptText)
        .with_route(route_a()),
        ConsentGrant::for_model_policy(
            "expired",
            route_a_unknown_downstream(),
            ConsentPurpose::CredentialValidation,
            DISCLOSURE,
            NOW - 600,
        )
        .with_content_type(ContentType::MetadataOnly)
        .with_expiry(NOW - 1),
        {
            let mut g = ConsentGrant::for_model_policy(
                "revoked",
                route_a(),
                ConsentPurpose::Summarization,
                DISCLOSURE,
                NOW - 600,
            )
            .with_content_type(ContentType::TranscriptText);
            g.revoked_at_unix = Some(NOW - 5);
            g
        },
    ];

    let mut checked = 0_usize;
    let mut approved = 0_usize;

    for purpose in ConsentPurpose::ALL {
        for content_type in ContentType::ALL {
            for route in &routes {
                for resolved_retention in retentions {
                    for requires_privacy_restriction in [false, true] {
                        for recording_id in recordings {
                            for disclosure in disclosures {
                                checked += 1;
                                let mut req = DispatchRequest::new(
                                    purpose,
                                    content_type,
                                    route.clone(),
                                    recording_id,
                                    disclosure,
                                );
                                req.resolved_retention = resolved_retention;
                                req.requires_privacy_restriction = requires_privacy_restriction;

                                let Ok(approval) = dispatch_preflight(&policy, &grants, &req, NOW)
                                else {
                                    continue;
                                };
                                approved += 1;

                                let grant = grants
                                    .iter()
                                    .find(|g| g.id == approval.grant_id)
                                    .expect("approval must name a grant in the ledger");

                                assert_eq!(grant.purpose, req.purpose, "purpose axis");
                                assert!(
                                    grant.content_types.contains(&req.content_type),
                                    "content-type axis"
                                );
                                assert!(grant.routes.contains(&req.route), "route axis");
                                assert!(grant.scope.covers(req.scope()), "scope axis");
                                assert_eq!(
                                    grant.disclosure_version, req.current_disclosure_version,
                                    "disclosure axis"
                                );
                                assert!(grant.revoked_at_unix.is_none(), "revocation axis");
                                assert!(grant.is_active(NOW), "validity axis");
                                assert!(
                                    !matches!(grant.scope, GrantScope::AskEveryTime),
                                    "ask-every-time must never auto-approve"
                                );
                                if req.requires_privacy_restriction {
                                    let resolved = req
                                        .resolved_retention
                                        .expect("restricted request must have resolved retention");
                                    assert!(resolved.is_resolved(), "retention must be resolved");
                                    let required = grant
                                        .required_retention
                                        .expect("restricted request needs a grant requirement");
                                    assert!(
                                        required.satisfied_by(resolved),
                                        "retention axis: {required:?} vs {resolved:?}"
                                    );
                                }
                                assert_eq!(
                                    approval.ticket.payload(),
                                    PayloadClass::from(req.content_type)
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    assert_eq!(checked, 4 * 3 * 3 * 5 * 2 * 2 * 2);
    assert!(
        approved > 0,
        "the cross product must contain approvable requests"
    );
}

// ---------------------------------------------------------------------------
// Error-envelope hygiene (§15.4, §16.6)
// ---------------------------------------------------------------------------

#[test]
fn error_messages_never_carry_content_identifiers_or_credentials() {
    let policy = permitted_policy();
    let secretish = [
        "rec-1",
        "sk-live-abcdef",
        "Quarterly board meeting",
        "vendor-x/model-large",
    ];

    let grant = grant_for_recording("g1", "rec-1", ContentType::TranscriptText);
    let cases = vec![
        dispatch_preflight(
            &policy,
            &[],
            &request(ContentType::AudioBytes, route_a(), "rec-1"),
            NOW,
        ),
        dispatch_preflight(
            &policy,
            std::slice::from_ref(&grant),
            &request(ContentType::AudioBytes, route_a(), "rec-1"),
            NOW,
        ),
        dispatch_preflight(
            &OutboundPolicy::local_only(),
            std::slice::from_ref(&grant),
            &request(ContentType::TranscriptText, route_a(), "rec-1"),
            NOW,
        ),
    ];

    for case in cases {
        let err = case.unwrap_err();
        let parts = err.envelope_parts();
        for needle in secretish {
            assert!(
                !parts.message.contains(needle),
                "message {:?} leaked {needle:?}",
                parts.message
            );
        }
        assert_ne!(parts.message.len(), 0);
    }
}
