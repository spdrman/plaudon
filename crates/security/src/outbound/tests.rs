//! D1.1 tests: exhaustive decision table, deny-by-default, and the invariant
//! that no input combination yields `Allow` while Local-only Mode is on.

use super::*;
use crate::error::SecurityError;

fn permissive_grant() -> NetworkGrant {
    NetworkGrant::new(ApprovalSource::TestHarness, 1_700_000_000, "privacy-v1")
        .with_model_downloads_approved(true)
        .with_telemetry_opted_in(true)
        .with_allowed_hosts(vec![
            "openrouter.ai".to_owned(),
            "plaud.ai".to_owned(),
            "models.example.invalid".to_owned(),
            "updates.example.invalid".to_owned(),
            "telemetry.example.invalid".to_owned(),
            "apple.example.invalid".to_owned(),
            "other.example.invalid".to_owned(),
            "folder.example.invalid".to_owned(),
        ])
}

/// Every destination the decision table is exercised against.
fn destinations() -> Vec<Destination> {
    vec![
        Destination::Network {
            scheme: "https".to_owned(),
            host: "openrouter.ai".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "plaud.ai".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "models.example.invalid".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "updates.example.invalid".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "telemetry.example.invalid".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "apple.example.invalid".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "attacker.example.invalid".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "http".to_owned(),
            host: "openrouter.ai".to_owned(),
            port: 80,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "localhost".to_owned(),
            port: 443,
        },
        Destination::Network {
            scheme: "https".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 443,
        },
        Destination::subprocess("plaud"),
        Destination::subprocess("plaud.exe"),
        Destination::subprocess("/opt/homebrew/bin/plaud"),
        Destination::subprocess("curl"),
    ]
}

/// The full cross product used by the exhaustive table tests.
fn all_requests() -> Vec<EgressRequest> {
    let mut out = Vec::new();
    for subsystem in Subsystem::ALL {
        for purpose in EgressPurpose::ALL {
            for payload in PayloadClass::ALL {
                for destination in destinations() {
                    out.push(EgressRequest::new(subsystem, destination, purpose, payload));
                }
            }
        }
    }
    out
}

#[test]
fn default_policy_is_local_only() {
    assert_eq!(OutboundPolicy::default().mode(), NetworkMode::LocalOnly);
    assert_eq!(NetworkMode::default(), NetworkMode::LocalOnly);
    assert!(OutboundPolicy::default().grant().is_none());
}

/// §1.2(15), §16.2A: no input combination yields `Allow` while Local-only.
#[test]
fn local_only_denies_every_request_in_the_cross_product() {
    let policy = OutboundPolicy::local_only();
    let requests = all_requests();
    assert!(
        requests.len() > 4000,
        "cross product unexpectedly small: {}",
        requests.len()
    );

    for request in &requests {
        let denied = policy
            .authorize(request)
            .err()
            .unwrap_or_else(|| panic!("local-only allowed {request:?}"));
        assert_eq!(
            denied.reason,
            DenyReason::LocalOnlyMode,
            "wrong reason for {request:?}"
        );
    }

    let records = policy.observer().records();
    assert_eq!(records.len().min(4096), records.len());
    assert!(
        records.iter().all(|r| !r.allowed),
        "an allow was recorded in local-only mode"
    );
}

/// Every decision, allow or deny, must be observable for the D2.3 evidence trace.
#[test]
fn every_decision_is_recorded() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());
    let allowed = EgressRequest::new(
        Subsystem::OpenRouter,
        Destination::Network {
            scheme: "https".to_owned(),
            host: "openrouter.ai".to_owned(),
            port: 443,
        },
        EgressPurpose::ModelCatalog,
        PayloadClass::None,
    );
    let denied = EgressRequest::new(
        Subsystem::OpenRouter,
        Destination::Network {
            scheme: "https".to_owned(),
            host: "attacker.example.invalid".to_owned(),
            port: 443,
        },
        EgressPurpose::ModelCatalog,
        PayloadClass::None,
    );

    policy.authorize(&allowed).unwrap();
    policy.authorize(&denied).unwrap_err();

    let records = policy.observer().records();
    assert_eq!(records.len(), 2);
    assert!(records[0].allowed);
    assert_eq!(records[0].destination_label, "https://openrouter.ai:443");
    assert!(!records[1].allowed);
    assert_eq!(
        records[1].deny_reason,
        Some(DenyReason::DestinationNotAllowlisted)
    );
}

// Exhaustive table test over every destination class and network mode;
// splitting it would hide the decision table it is meant to document.
#[allow(clippy::too_many_lines)]
#[test]
fn decision_table_per_destination_class_and_mode() {
    struct Case {
        name: &'static str,
        subsystem: Subsystem,
        purpose: EgressPurpose,
        destination: Destination,
        expected_permitted: Result<(), DenyReason>,
    }

    let net = |host: &str| Destination::Network {
        scheme: "https".to_owned(),
        host: host.to_owned(),
        port: 443,
    };

    let cases = [
        Case {
            name: "openrouter inference to allowlisted host",
            subsystem: Subsystem::OpenRouter,
            purpose: EgressPurpose::InferenceDispatch,
            destination: net("openrouter.ai"),
            expected_permitted: Ok(()),
        },
        Case {
            name: "openrouter subdomain is covered by the allowlist",
            subsystem: Subsystem::OpenRouter,
            purpose: EgressPurpose::InferenceDispatch,
            destination: net("api.openrouter.ai"),
            expected_permitted: Ok(()),
        },
        Case {
            name: "openrouter cannot sync accounts",
            subsystem: Subsystem::OpenRouter,
            purpose: EgressPurpose::AccountSync,
            destination: net("openrouter.ai"),
            expected_permitted: Err(DenyReason::PurposeNotValidForSubsystem),
        },
        Case {
            name: "model distribution to an unlisted CDN",
            subsystem: Subsystem::ModelDistribution,
            purpose: EgressPurpose::ModelDownload,
            destination: net("cdn.attacker.invalid"),
            expected_permitted: Err(DenyReason::DestinationNotAllowlisted),
        },
        Case {
            name: "update checker",
            subsystem: Subsystem::UpdateChecker,
            purpose: EgressPurpose::UpdateCheck,
            destination: net("updates.example.invalid"),
            expected_permitted: Ok(()),
        },
        Case {
            name: "telemetry",
            subsystem: Subsystem::Telemetry,
            purpose: EgressPurpose::TelemetryFlush,
            destination: net("telemetry.example.invalid"),
            expected_permitted: Ok(()),
        },
        Case {
            name: "cloud cli subprocess",
            subsystem: Subsystem::PlaudCloudCli,
            purpose: EgressPurpose::AccountSync,
            destination: Destination::subprocess("plaud"),
            expected_permitted: Ok(()),
        },
        Case {
            name: "arbitrary subprocess is never network-capable",
            subsystem: Subsystem::ModelDistribution,
            purpose: EgressPurpose::ModelDownload,
            destination: Destination::subprocess("curl"),
            expected_permitted: Err(DenyReason::SubsystemNotNetworkCapable),
        },
        Case {
            name: "folder sync probe has no legitimate egress purpose",
            subsystem: Subsystem::FolderSyncProbe,
            purpose: EgressPurpose::Diagnostic,
            destination: net("folder.example.invalid"),
            expected_permitted: Err(DenyReason::PurposeNotValidForSubsystem),
        },
        Case {
            name: "plaintext http",
            subsystem: Subsystem::OpenRouter,
            purpose: EgressPurpose::ModelCatalog,
            destination: Destination::Network {
                scheme: "http".to_owned(),
                host: "openrouter.ai".to_owned(),
                port: 80,
            },
            expected_permitted: Err(DenyReason::SchemeNotHttps),
        },
        Case {
            name: "loopback destination",
            subsystem: Subsystem::OpenRouter,
            purpose: EgressPurpose::ModelCatalog,
            destination: net("localhost"),
            expected_permitted: Err(DenyReason::PrivateOrLoopbackDestination),
        },
        Case {
            name: "ip literal destination",
            subsystem: Subsystem::OpenRouter,
            purpose: EgressPurpose::ModelCatalog,
            destination: net("10.0.0.7"),
            expected_permitted: Err(DenyReason::PrivateOrLoopbackDestination),
        },
    ];

    let local = OutboundPolicy::local_only();
    let permitted = OutboundPolicy::network_permitted(permissive_grant());

    for case in cases {
        let request = EgressRequest::new(
            case.subsystem,
            case.destination.clone(),
            case.purpose,
            PayloadClass::Metadata,
        );

        let local_result = local.authorize(&request);
        assert!(
            matches!(&local_result, Err(d) if d.reason == DenyReason::LocalOnlyMode),
            "[{}] local-only should deny, got {local_result:?}",
            case.name
        );

        let permitted_result = permitted.authorize(&request);
        match (case.expected_permitted, &permitted_result) {
            (Ok(()), Ok(_)) => {}
            (Err(expected), Err(actual)) => {
                assert_eq!(actual.reason, expected, "[{}] wrong deny reason", case.name);
            }
            (expected, actual) => {
                panic!("[{}] expected {expected:?}, got {actual:?}", case.name)
            }
        }
    }
}

#[test]
fn model_download_requires_explicit_user_approval_even_when_network_is_permitted() {
    let grant = NetworkGrant::new(ApprovalSource::UserSettingsConfirmation, 0, "privacy-v1")
        .with_allowed_hosts(vec!["models.example.invalid".to_owned()]);
    let policy = OutboundPolicy::network_permitted(grant);
    let request = EgressRequest::new(
        Subsystem::ModelDistribution,
        Destination::Network {
            scheme: "https".to_owned(),
            host: "models.example.invalid".to_owned(),
            port: 443,
        },
        EgressPurpose::ModelDownload,
        PayloadClass::None,
    );
    let denied = policy.authorize(&request).unwrap_err();
    assert_eq!(denied.reason, DenyReason::ModelDownloadNotApproved);
    assert_eq!(
        denied
            .envelope_parts()
            .action
            .map(super::super::error::ErrorAction::as_str),
        Some("APPROVE_MODEL_DOWNLOAD")
    );
}

#[test]
fn telemetry_requires_opt_in_even_when_network_is_permitted() {
    let grant = NetworkGrant::new(ApprovalSource::UserSettingsConfirmation, 0, "privacy-v1")
        .with_allowed_hosts(vec!["telemetry.example.invalid".to_owned()]);
    let policy = OutboundPolicy::network_permitted(grant);
    let request = EgressRequest::new(
        Subsystem::Telemetry,
        Destination::Network {
            scheme: "https".to_owned(),
            host: "telemetry.example.invalid".to_owned(),
            port: 443,
        },
        EgressPurpose::TelemetryFlush,
        PayloadClass::Metadata,
    );
    assert_eq!(
        policy.authorize(&request).unwrap_err().reason,
        DenyReason::TelemetryNotOptedIn
    );
}

#[test]
fn destination_url_parsing_rejects_hostile_inputs() {
    let cases: [(&str, DenyReason); 9] = [
        ("http://openrouter.ai/x", DenyReason::SchemeNotHttps),
        ("ftp://openrouter.ai/x", DenyReason::SchemeNotHttps),
        ("file:///etc/passwd", DenyReason::SchemeNotHttps),
        (
            "https://user:pw@openrouter.ai/x",
            DenyReason::CredentialInUrl,
        ),
        (
            "https://127.0.0.1/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        ("https://[::1]/x", DenyReason::PrivateOrLoopbackDestination),
        (
            "https://localhost/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://printer.local/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        ("not a url", DenyReason::MalformedDestination),
    ];
    for (raw, expected) in cases {
        let err = Destination::url(raw).unwrap_err();
        assert_eq!(err.reason, expected, "for {raw}");
        assert!(
            !err.destination_label.contains("pw"),
            "label leaked a credential: {}",
            err.destination_label
        );
    }
}

#[test]
fn destination_url_keeps_no_path_or_query_in_the_label() {
    let dest = Destination::url("https://openrouter.ai/v1/audio?sig=SECRETSIG&x=1").unwrap();
    let label = dest.label();
    assert_eq!(label, "https://openrouter.ai:443");
    assert!(!label.contains("SECRETSIG"));
}

#[test]
fn describe_lists_disabled_capabilities_in_local_only_mode() {
    let policy = OutboundPolicy::local_only();
    let described = policy.describe();
    assert_eq!(described.mode, NetworkMode::LocalOnly);
    assert!(
        !described.cloud_cli_launchable,
        "cloud CLI must not be launchable (§22.15)"
    );
    assert_eq!(described.allowed_hosts.len(), 0);

    for subsystem in [
        Subsystem::PlaudCloudCli,
        Subsystem::OpenRouter,
        Subsystem::ModelDistribution,
        Subsystem::UpdateChecker,
        Subsystem::Telemetry,
        Subsystem::AppleAssetFetch,
    ] {
        let entry = described
            .disabled_capabilities
            .iter()
            .find(|c| c.subsystem == subsystem)
            .unwrap_or_else(|| panic!("{subsystem} missing from disabled capability list"));
        assert_eq!(entry.reason, DenyReason::LocalOnlyMode);
        assert_ne!(entry.explanation.len(), 0);
    }
}

#[test]
fn describe_is_honest_about_what_is_not_guaranteed() {
    let described = OutboundPolicy::local_only().describe();
    assert_eq!(described.not_guaranteed.len(), NOT_GUARANTEED.len());
    let joined = described.not_guaranteed.join(" ").to_lowercase();
    assert!(joined.contains("operating-system"));
    assert!(joined.contains("cloud-sync"));
    assert!(joined.contains("radio"));
    // No overclaiming language anywhere in the policy description.
    for claim in [
        "guarantees no network",
        "blocks all network traffic",
        "fully isolated",
    ] {
        assert!(
            !joined.contains(claim),
            "overclaiming phrase present: {claim}"
        );
    }
}

#[test]
fn describe_round_trips_through_json_for_the_command_response() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());
    let described = policy.describe();
    let json = serde_json::to_string(&described).unwrap();
    let back: EffectiveNetworkPolicy = serde_json::from_str(&json).unwrap();
    assert_eq!(back.mode, described.mode);
    assert_eq!(back.cloud_cli_launchable, described.cloud_cli_launchable);
    assert!(back.cloud_cli_launchable);
}

#[test]
fn denied_envelope_carries_no_destination_detail_or_secret() {
    let policy = OutboundPolicy::local_only();
    let request = EgressRequest::new(
        Subsystem::OpenRouter,
        Destination::url("https://openrouter.ai/v1/x?key=sk-or-v1-abcdef").unwrap(),
        EgressPurpose::InferenceDispatch,
        PayloadClass::AudioBytes,
    );
    let denied = policy.authorize(&request).unwrap_err();
    let parts = denied.envelope_parts();
    assert_eq!(parts.code.as_str(), "EGRESS_DENIED");
    assert!(!parts.retryable);
    assert!(!parts.message.contains("sk-or-v1"));
    assert!(!parts.message.contains('?'));
    assert_eq!(
        parts.action.map(super::super::error::ErrorAction::as_str),
        Some("DISABLE_LOCAL_ONLY_MODE")
    );
}

#[test]
fn switching_to_local_only_mode_closes_previously_open_capabilities() {
    let permitted = OutboundPolicy::network_permitted(permissive_grant());
    let request = EgressRequest::new(
        Subsystem::OpenRouter,
        Destination::url("https://openrouter.ai/api/v1/models").unwrap(),
        EgressPurpose::ModelCatalog,
        PayloadClass::None,
    );
    assert!(permitted.authorize(&request).is_ok());

    let local = OutboundPolicy::local_only();
    assert_eq!(
        local.authorize(&request).unwrap_err().reason,
        DenyReason::LocalOnlyMode
    );
}

#[test]
fn empty_allowlist_means_no_host_is_reachable() {
    let grant = NetworkGrant::new(ApprovalSource::TestHarness, 0, "privacy-v1")
        .with_allowed_hosts(Vec::new());
    let policy = OutboundPolicy::network_permitted(grant);
    let request = EgressRequest::new(
        Subsystem::OpenRouter,
        Destination::url("https://openrouter.ai/api/v1/models").unwrap(),
        EgressPurpose::ModelCatalog,
        PayloadClass::None,
    );
    assert_eq!(
        policy.authorize(&request).unwrap_err().reason,
        DenyReason::DestinationNotAllowlisted
    );
}

#[test]
fn allowlist_suffix_match_cannot_be_spoofed_by_a_lookalike_domain() {
    let grant = NetworkGrant::new(ApprovalSource::TestHarness, 0, "privacy-v1")
        .with_allowed_hosts(vec!["openrouter.ai".to_owned()]);
    let policy = OutboundPolicy::network_permitted(grant);
    for host in [
        "notopenrouter.ai",
        "openrouter.ai.attacker.invalid",
        "evilopenrouter.ai",
    ] {
        let request = EgressRequest::new(
            Subsystem::OpenRouter,
            Destination::Network {
                scheme: "https".to_owned(),
                host: host.to_owned(),
                port: 443,
            },
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        );
        assert_eq!(
            policy.authorize(&request).unwrap_err().reason,
            DenyReason::DestinationNotAllowlisted,
            "{host} should not match the allowlist"
        );
    }
}

#[test]
fn observer_is_bounded_and_keeps_the_most_recent_records() {
    let observer = std::sync::Arc::new(RecordingObserver::new(3));
    let policy = OutboundPolicy::local_only().with_observer(observer.clone());
    for i in 0..10u16 {
        let request = EgressRequest::new(
            Subsystem::OpenRouter,
            Destination::Network {
                scheme: "https".to_owned(),
                host: "openrouter.ai".to_owned(),
                port: 1000 + i,
            },
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        );
        let _ = policy.authorize(&request);
    }
    let records = observer.records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].destination_label, "https://openrouter.ai:1009");
}
