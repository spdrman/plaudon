//! Refusal-side behaviour of the single egress gate (§16.2A, §7.3, §7.1).
//!
//! The inline table tests in `src/outbound/tests.rs` cover the happy decision
//! table. This suite drives the arms a successful request never reaches: every
//! destination-parser rejection, the private-hostname suffix list, the
//! subsystem/purpose decision table under a *permissive* grant, the scrubbed
//! subprocess environment, the CLI argument validator, and the vocabulary
//! (`label`/`explanation`/`Display`/serde) that only appears in an error.

use std::path::Path;
use std::sync::Arc;

use plaudon_security::error::{ErrorAction, SecurityError};
use plaudon_security::outbound::http::{GatedHttpClientFactory, Method, TransportError};
use plaudon_security::outbound::subprocess::{
    is_stripped_env, scrubbed_env, ArgError, CliArg, GatedProcessLauncher, SpawnError, MAX_ARG_LEN,
    STRIPPED_ENV_NAMES, STRIPPED_ENV_PREFIXES,
};
use plaudon_security::outbound::{
    default_allowed_hosts, ApprovalSource, DenyReason, Destination, EgressObserver, EgressPurpose,
    EgressRequest, EgressTicket, NetworkGrant, NetworkMode, OutboundPolicy, PayloadClass,
    RecordingObserver, Subsystem, ALWAYS_REFUSED, NOT_GUARANTEED, PLAUD_CLI_PROGRAM,
};

fn permissive_grant() -> NetworkGrant {
    NetworkGrant::new(ApprovalSource::TestHarness, 1_700_000_000, "privacy-v1")
        .with_model_downloads_approved(true)
        .with_telemetry_opted_in(true)
        .with_allowed_hosts(vec![
            "OpenRouter.AI".to_owned(),
            "plaud.ai".to_owned(),
            "models.example.invalid".to_owned(),
            "updates.example.invalid".to_owned(),
            "telemetry.example.invalid".to_owned(),
            "apple.example.invalid".to_owned(),
            "folder.example.invalid".to_owned(),
            "other.example.invalid".to_owned(),
        ])
}

fn net(host: &str) -> Destination {
    Destination::Network {
        scheme: "https".to_owned(),
        host: host.to_owned(),
        port: 443,
    }
}

/// Every reason the URL parser refuses, each asserted by its own variant, plus
/// the guarantee that no rejection label carries a path, query or credential.
#[test]
fn the_destination_parser_names_the_exact_rule_that_refused_each_url() {
    let cases: [(&str, DenyReason); 16] = [
        ("", DenyReason::MalformedDestination),
        ("://nope", DenyReason::MalformedDestination),
        ("https://", DenyReason::MalformedDestination),
        ("mailto:ops@plaud.ai", DenyReason::SchemeNotHttps),
        ("wss://openrouter.ai/socket", DenyReason::SchemeNotHttps),
        ("data:text/plain,hello", DenyReason::SchemeNotHttps),
        ("https://user@openrouter.ai/x", DenyReason::CredentialInUrl),
        ("https://:pw@openrouter.ai/x", DenyReason::CredentialInUrl),
        (
            "https://192.168.1.1/admin",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://[fe80::1]/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://localhost./x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://api.localhost/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://metadata.internal/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://router.home.arpa/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://1.0.0.127.in-addr.arpa/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https://b.a.9.ip6.arpa/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
    ];

    for (raw, expected) in cases {
        let denied = Destination::url(raw)
            .err()
            .unwrap_or_else(|| panic!("{raw} must be refused"));
        assert_eq!(denied.reason, expected, "wrong rule for {raw}");
        assert_eq!(denied.subsystem, Subsystem::Other("destination_parser"));
        assert_eq!(denied.user_action, None);
        let label = &denied.destination_label;
        assert!(
            !label.contains('?'),
            "label {label} leaked a query for {raw}"
        );
        assert!(
            !label.contains('@'),
            "label {label} leaked an authority for {raw}"
        );
        assert!(!label.contains("pw"), "label {label} leaked a password");
        assert!(!label.contains("admin"), "label {label} leaked a path");
    }
}

/// A scheme-rejected URL that has no host at all still produces a label, and
/// the label says so rather than rendering an empty authority.
#[test]
fn a_hostless_url_is_labelled_as_having_no_host() {
    let denied = Destination::url("mailto:ops@plaud.ai").unwrap_err();
    assert_eq!(denied.reason, DenyReason::SchemeNotHttps);
    assert_eq!(denied.destination_label, "mailto://<no-host>");
    assert!(!denied.destination_label.contains("ops"));
}

/// The host of an accepted URL is lower-cased, and the default port is filled in.
#[test]
fn an_accepted_url_is_normalised_and_keeps_only_scheme_host_port() {
    let dest = Destination::url("https://API.OpenRouter.AI/v1/models?key=sk-or-v1-x").unwrap();
    assert_eq!(dest.host(), Some("api.openrouter.ai"));
    assert_eq!(dest.label(), "https://api.openrouter.ai:443");

    let explicit = Destination::url("https://openrouter.ai:8443/v1").unwrap();
    assert_eq!(explicit.label(), "https://openrouter.ai:8443");

    let child = Destination::subprocess("plaud");
    assert_eq!(child.host(), None, "a subprocess has no network host");
    assert_eq!(child.label(), "subprocess:plaud");
}

/// Under a grant that approves everything a grant *can* approve, the
/// subsystem/purpose table is still enforced: only the combinations the gate
/// recognises are allowed, and everything else reports
/// `PurposeNotValidForSubsystem` rather than being guessed.
#[test]
fn the_permitted_mode_subsystem_purpose_table_is_exhaustive_and_fails_closed() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());

    let valid: &[(Subsystem, &[EgressPurpose])] = &[
        (
            Subsystem::PlaudCloudCli,
            &[EgressPurpose::AccountSync, EgressPurpose::Diagnostic],
        ),
        (
            Subsystem::OpenRouter,
            &[
                EgressPurpose::CredentialValidation,
                EgressPurpose::ModelCatalog,
                EgressPurpose::InferenceDispatch,
                EgressPurpose::Diagnostic,
            ],
        ),
        (
            Subsystem::ModelDistribution,
            &[
                EgressPurpose::ModelCatalog,
                EgressPurpose::ManifestFetch,
                EgressPurpose::ModelDownload,
            ],
        ),
        (
            Subsystem::UpdateChecker,
            &[EgressPurpose::UpdateCheck, EgressPurpose::ManifestFetch],
        ),
        (Subsystem::Telemetry, &[EgressPurpose::TelemetryFlush]),
        (Subsystem::AppleAssetFetch, &[EgressPurpose::AssetDownload]),
        (Subsystem::FolderSyncProbe, &[]),
        (Subsystem::Other("test_subsystem"), &[]),
    ];

    let mut allowed = 0usize;
    let mut refused = 0usize;
    let mut gated = 0usize;
    for (subsystem, permitted_purposes) in valid {
        for purpose in EgressPurpose::ALL {
            let destination = if *subsystem == Subsystem::PlaudCloudCli {
                Destination::subprocess(PLAUD_CLI_PROGRAM)
            } else {
                net(representative_host(*subsystem))
            };
            let request =
                EgressRequest::new(*subsystem, destination, purpose, PayloadClass::Metadata);
            let result = policy.authorize(&request);
            if *subsystem == Subsystem::PlaudCloudCli && purpose == EgressPurpose::AssetDownload {
                // The guided CLI install (#303) is a valid purpose for the
                // subsystem, but only a per-action grant can approve it, so a
                // standing grant refuses it with its own reason.
                let denied = result.expect_err("a standing grant never installs");
                assert_eq!(denied.reason, DenyReason::ActionNotApproved);
                assert_eq!(denied.user_action, None);
                gated += 1;
            } else if permitted_purposes.contains(&purpose) {
                assert!(
                    result.is_ok(),
                    "{} / {} should be allowed, got {result:?}",
                    subsystem.label(),
                    purpose.label()
                );
                allowed += 1;
            } else {
                let denied = result
                    .err()
                    .unwrap_or_else(|| panic!("{subsystem} / {} allowed", purpose.label()));
                assert_eq!(
                    denied.reason,
                    DenyReason::PurposeNotValidForSubsystem,
                    "{} / {} refused for the wrong reason",
                    subsystem.label(),
                    purpose.label()
                );
                assert_eq!(denied.user_action, None);
                refused += 1;
            }
        }
    }

    assert_eq!(allowed, 13);
    assert_eq!(gated, 1);
    assert_eq!(refused, 8 * EgressPurpose::ALL.len() - 13 - 1);
}

fn representative_host(subsystem: Subsystem) -> &'static str {
    match subsystem {
        Subsystem::OpenRouter => "openrouter.ai",
        Subsystem::PlaudCloudCli => "plaud.ai",
        Subsystem::ModelDistribution => "models.example.invalid",
        Subsystem::UpdateChecker => "updates.example.invalid",
        Subsystem::Telemetry => "telemetry.example.invalid",
        Subsystem::AppleAssetFetch => "apple.example.invalid",
        Subsystem::FolderSyncProbe => "folder.example.invalid",
        Subsystem::Other(_) => "other.example.invalid",
    }
}

/// Only the cloud CLI subsystem may name a subprocess destination, and only
/// with the pinned leaf name — including when it is reached through a path or
/// carries the Windows `.exe` suffix.
#[test]
fn a_subprocess_destination_is_refused_for_every_shape_but_the_pinned_cli() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());

    for program in ["plaud", "plaud.exe", "/opt/homebrew/bin/plaud"] {
        let request = EgressRequest::new(
            Subsystem::PlaudCloudCli,
            Destination::subprocess(program),
            EgressPurpose::AccountSync,
            PayloadClass::Metadata,
        );
        assert!(
            policy.authorize(&request).is_ok(),
            "{program} is the pinned CLI"
        );
    }

    for program in ["curl", "plaudx", "sh", "plaud.sh", "", ".."] {
        let request = EgressRequest::new(
            Subsystem::PlaudCloudCli,
            Destination::subprocess(program),
            EgressPurpose::AccountSync,
            PayloadClass::Metadata,
        );
        assert_eq!(
            policy.authorize(&request).unwrap_err().reason,
            DenyReason::DestinationNotAllowlisted,
            "{program:?} must not pass as the pinned CLI"
        );
    }

    // Every other subsystem is refused before the program name is even looked at.
    for subsystem in [
        Subsystem::OpenRouter,
        Subsystem::ModelDistribution,
        Subsystem::UpdateChecker,
        Subsystem::AppleAssetFetch,
    ] {
        let request = EgressRequest::new(
            subsystem,
            Destination::subprocess(PLAUD_CLI_PROGRAM),
            EgressPurpose::Diagnostic,
            PayloadClass::Metadata,
        );
        let denied = policy.authorize(&request).unwrap_err();
        assert!(
            matches!(
                denied.reason,
                DenyReason::SubsystemNotNetworkCapable | DenyReason::PurposeNotValidForSubsystem
            ),
            "{subsystem} got {:?}",
            denied.reason
        );
    }
}

/// `SubsystemNotNetworkCapable` is reachable with a purpose the subsystem does
/// accept, which proves the subprocess rule is what fired and not the purpose
/// table.
#[test]
fn a_valid_purpose_on_a_subprocess_destination_still_names_the_subsystem_rule() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());
    let request = EgressRequest::new(
        Subsystem::ModelDistribution,
        Destination::subprocess(PLAUD_CLI_PROGRAM),
        EgressPurpose::ModelDownload,
        PayloadClass::None,
    );
    let denied = policy.authorize(&request).unwrap_err();
    assert_eq!(denied.reason, DenyReason::SubsystemNotNetworkCapable);
    assert_eq!(denied.destination_label, "subprocess:plaud");
    assert_eq!(denied.user_action, None);
}

/// The two opt-in gates refuse while the grant withholds them and allow once it
/// records them, so the refusal is the grant's state and not an accident of the
/// host allowlist.
#[test]
fn the_opt_in_gates_flip_with_the_grant_and_carry_their_own_user_action() {
    let base = NetworkGrant::new(ApprovalSource::OnboardingChoice, 1, "privacy-v1")
        .with_allowed_hosts(vec![
            "models.example.invalid".to_owned(),
            "telemetry.example.invalid".to_owned(),
        ]);

    let download = EgressRequest::new(
        Subsystem::ModelDistribution,
        net("models.example.invalid"),
        EgressPurpose::ModelDownload,
        PayloadClass::None,
    );
    let flush = EgressRequest::new(
        Subsystem::Telemetry,
        net("telemetry.example.invalid"),
        EgressPurpose::TelemetryFlush,
        PayloadClass::Metadata,
    );

    let withheld = OutboundPolicy::network_permitted(base.clone());
    let denied = withheld.authorize(&download).unwrap_err();
    assert_eq!(denied.reason, DenyReason::ModelDownloadNotApproved);
    assert_eq!(
        denied.user_action.map(ErrorAction::as_str),
        Some("APPROVE_MODEL_DOWNLOAD")
    );
    let denied = withheld.authorize(&flush).unwrap_err();
    assert_eq!(denied.reason, DenyReason::TelemetryNotOptedIn);
    assert_eq!(
        denied.user_action.map(ErrorAction::as_str),
        Some("OPEN_CLOUD_CONSENT")
    );
    assert_eq!(
        denied.envelope_parts().action.map(ErrorAction::as_str),
        Some("OPEN_CLOUD_CONSENT")
    );

    let granted = OutboundPolicy::network_permitted(
        base.with_model_downloads_approved(true)
            .with_telemetry_opted_in(true),
    );
    assert!(granted.authorize(&download).is_ok());
    assert!(granted.authorize(&flush).is_ok());

    // Explicitly turning an approval back off is also honoured.
    let revoked = OutboundPolicy::network_permitted(
        NetworkGrant::new(ApprovalSource::UserSettingsConfirmation, 2, "privacy-v1")
            .with_allowed_hosts(vec!["models.example.invalid".to_owned()])
            .with_model_downloads_approved(true)
            .with_model_downloads_approved(false),
    );
    assert_eq!(
        revoked.authorize(&download).unwrap_err().reason,
        DenyReason::ModelDownloadNotApproved
    );
}

/// The allowlist is compared case-insensitively on both sides, and the suffix
/// rule only matches on a label boundary.
#[test]
fn the_host_allowlist_folds_case_but_not_label_boundaries() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());
    for host in ["OPENROUTER.AI", "Api.OpenRouter.Ai", "a.b.openrouter.ai"] {
        let request = EgressRequest::new(
            Subsystem::OpenRouter,
            net(host),
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        );
        assert!(policy.authorize(&request).is_ok(), "{host} must be allowed");
    }
    for host in [
        "xopenrouter.ai",
        "openrouter.ai.evil.invalid",
        "openrouter.aix",
    ] {
        let request = EgressRequest::new(
            Subsystem::OpenRouter,
            net(host),
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        );
        assert_eq!(
            policy.authorize(&request).unwrap_err().reason,
            DenyReason::DestinationNotAllowlisted,
            "{host} must not be allowed"
        );
    }
}

/// A `Destination::Network` built directly (not through the URL parser) is
/// re-checked by the gate, so a caller cannot bypass the scheme and
/// private-address rules by constructing the struct itself.
#[test]
fn a_hand_built_network_destination_is_re_checked_by_the_gate() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());
    let cases: [(&str, &str, DenyReason); 6] = [
        ("http", "openrouter.ai", DenyReason::SchemeNotHttps),
        ("HTTPS", "openrouter.ai", DenyReason::SchemeNotHttps),
        (
            "https",
            "localhost",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https",
            "db.internal",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        (
            "https",
            "169.254.169.254",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        ("https", "::1", DenyReason::PrivateOrLoopbackDestination),
    ];
    for (scheme, host, expected) in cases {
        let request = EgressRequest::new(
            Subsystem::OpenRouter,
            Destination::Network {
                scheme: scheme.to_owned(),
                host: host.to_owned(),
                port: 443,
            },
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        );
        assert_eq!(
            policy.authorize(&request).unwrap_err().reason,
            expected,
            "{scheme}://{host}"
        );
    }
}

/// `NetworkPermitted` without a recorded grant is not reachable through the
/// public constructors, and the snapshot of a local-only policy therefore
/// reports no reachable host at all.
#[test]
fn a_policy_snapshot_lists_hosts_only_when_a_grant_records_them() {
    let local = OutboundPolicy::local_only();
    assert!(local.grant().is_none());
    assert!(
        local.describe().allowed_hosts.is_empty(),
        "local-only mode reaches no host"
    );
    assert_eq!(local.describe().not_guaranteed.len(), NOT_GUARANTEED.len());

    let permitted = OutboundPolicy::network_permitted(permissive_grant());
    let described = permitted.describe();
    assert_eq!(described.mode, NetworkMode::NetworkPermitted);
    assert_eq!(described.allowed_hosts.len(), 8);
    assert!(
        described
            .allowed_hosts
            .contains(&"openrouter.ai".to_owned()),
        "with_allowed_hosts must lower-case: {:?}",
        described.allowed_hosts
    );
    assert!(described.cloud_cli_launchable);
    assert_eq!(described.always_refused.len(), ALWAYS_REFUSED.len());
    assert!(
        described.disabled_capabilities.is_empty(),
        "a fully permissive grant disables nothing: {:?}",
        described.disabled_capabilities
    );
}

/// The default host allowlist is the pinned vendor set and nothing else.
#[test]
fn the_default_grant_pins_the_vendor_hosts_and_withholds_both_opt_ins() {
    let grant = NetworkGrant::new(ApprovalSource::UserSettingsConfirmation, 7, "privacy-v1");
    assert_eq!(grant.allowed_hosts, default_allowed_hosts());
    assert!(!grant.model_downloads_approved);
    assert!(!grant.telemetry_opted_in);
    assert_eq!(grant.approved_at_unix, 7);
    assert_eq!(grant.approved_by, ApprovalSource::UserSettingsConfirmation);

    let policy = OutboundPolicy::network_permitted(grant);
    let request = EgressRequest::new(
        Subsystem::ModelDistribution,
        net("cdn.models.invalid"),
        EgressPurpose::ModelDownload,
        PayloadClass::None,
    );
    // The download opt-in is checked before the destination, so that is the
    // reason the user is given.
    assert_eq!(
        policy.authorize(&request).unwrap_err().reason,
        DenyReason::ModelDownloadNotApproved
    );
}

/// Every deny reason has a distinct stable label and a non-empty, fixed-vocabulary
/// explanation that never interpolates a destination.
#[test]
fn every_deny_reason_has_a_distinct_label_and_a_safe_explanation() {
    let all = [
        DenyReason::LocalOnlyMode,
        DenyReason::DestinationNotAllowlisted,
        DenyReason::SchemeNotHttps,
        DenyReason::PrivateOrLoopbackDestination,
        DenyReason::CredentialInUrl,
        DenyReason::MalformedDestination,
        DenyReason::SubsystemNotNetworkCapable,
        DenyReason::ModelDownloadNotApproved,
        DenyReason::TelemetryNotOptedIn,
        DenyReason::PurposeNotValidForSubsystem,
        DenyReason::VoiceBiometricNeverEgresses,
    ];
    let mut labels = std::collections::BTreeSet::new();
    for reason in all {
        assert!(
            labels.insert(reason.label()),
            "duplicate {}",
            reason.label()
        );
        let explanation = reason.explanation();
        assert!(
            !explanation.is_empty(),
            "{} has no explanation",
            reason.label()
        );
        assert!(!explanation.contains("://"), "{explanation}");
        assert!(!explanation.contains('?'), "{explanation}");
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(json, format!("\"{}\"", reason.label()));
        let back: DenyReason = serde_json::from_str(&json).unwrap();
        assert_eq!(back, reason);
    }
    assert_eq!(labels.len(), all.len());
}

/// Every purpose and payload class carries a stable label; the user-content and
/// never-egressable predicates are checked over the whole set rather than for
/// the one variant a happy path uses.
#[test]
fn every_purpose_and_payload_class_label_is_stable_and_distinct() {
    let mut purposes = std::collections::BTreeSet::new();
    for purpose in EgressPurpose::ALL {
        assert!(purposes.insert(purpose.label()));
        assert_eq!(
            serde_json::to_string(&purpose).unwrap(),
            format!("\"{}\"", purpose.label())
        );
    }
    assert_eq!(purposes.len(), EgressPurpose::ALL.len());

    for payload in PayloadClass::ALL {
        let expected_content = matches!(
            payload,
            PayloadClass::TranscriptText | PayloadClass::AudioBytes | PayloadClass::VoiceBiometric
        );
        assert_eq!(payload.is_user_content(), expected_content, "{payload:?}");
        assert_eq!(
            payload.never_egressable(),
            payload == PayloadClass::VoiceBiometric,
            "{payload:?}"
        );
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            format!("\"{}\"", payload.label())
        );
    }
}

/// A subsystem label round-trips through serde; an unknown label fails closed
/// rather than being revived as `Other`, which would carry a foreign string
/// into our logs.
#[test]
fn an_unknown_subsystem_label_fails_closed_on_deserialization() {
    for subsystem in Subsystem::ALL {
        let json = serde_json::to_string(&subsystem).unwrap();
        assert_eq!(json, format!("\"{}\"", subsystem.label()));
        assert_eq!(format!("{subsystem}"), subsystem.label());

        let decoded: Result<Subsystem, _> = serde_json::from_str(&json);
        if matches!(subsystem, Subsystem::Other(_)) {
            let err = decoded.expect_err("Other must not be revivable from a label");
            assert!(err.to_string().contains("unknown subsystem label"));
        } else {
            assert_eq!(decoded.unwrap(), subsystem);
        }
    }

    for raw in ["\"\"", "\"attacker\"", "\"OpenRouter\"", "\"telemetry \""] {
        let err = serde_json::from_str::<Subsystem>(raw)
            .expect_err("unknown labels must not deserialize");
        assert!(err.to_string().contains("unknown subsystem label"), "{raw}");
    }
}

/// The denial renders as a single line naming rule, subsystem and redacted
/// destination — and nothing else.
#[test]
fn a_denial_renders_as_rule_subsystem_and_redacted_destination() {
    let policy = OutboundPolicy::local_only();
    let request = EgressRequest::new(
        Subsystem::UpdateChecker,
        Destination::url("https://updates.example.invalid/v/latest?token=abc").unwrap(),
        EgressPurpose::UpdateCheck,
        PayloadClass::None,
    );
    let denied = policy.authorize(&request).unwrap_err();
    let rendered = denied.to_string();
    assert_eq!(
        rendered,
        "egress denied (local_only_mode) for update_checker -> https://updates.example.invalid:443"
    );
    assert!(!rendered.contains("token"));
    assert!(std::error::Error::source(&denied).is_none());
}

/// The observer ring never grows past its capacity, and a zero capacity is
/// clamped to one rather than producing a ring that drops everything.
#[test]
fn the_recording_observer_ring_clamps_a_zero_capacity_and_evicts_oldest_first() {
    let observer = Arc::new(RecordingObserver::new(0));
    let policy = OutboundPolicy::local_only().with_observer(observer.clone());
    for port in 1u16..=4 {
        let request = EgressRequest::new(
            Subsystem::OpenRouter,
            Destination::Network {
                scheme: "https".to_owned(),
                host: "openrouter.ai".to_owned(),
                port,
            },
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        );
        let _ = policy.authorize(&request);
    }
    let records = observer.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].destination_label, "https://openrouter.ai:4");
    assert_eq!(records[0].deny_reason, Some(DenyReason::LocalOnlyMode));
    assert!(!records[0].allowed);

    let default_ring = RecordingObserver::default();
    assert!(
        default_ring.records().is_empty(),
        "a fresh ring has recorded nothing"
    );
}

/// A ticket carries exactly the four attributed fields and no URL.
#[test]
fn an_issued_ticket_carries_only_attribution_fields() {
    let policy = OutboundPolicy::network_permitted(permissive_grant());
    let request = EgressRequest::new(
        Subsystem::OpenRouter,
        Destination::url("https://openrouter.ai/api/v1/chat?sig=SECRET").unwrap(),
        EgressPurpose::InferenceDispatch,
        PayloadClass::TranscriptText,
    );
    let ticket: EgressTicket = policy.authorize(&request).unwrap();
    assert_eq!(ticket.subsystem(), Subsystem::OpenRouter);
    assert_eq!(ticket.purpose(), EgressPurpose::InferenceDispatch);
    assert_eq!(ticket.payload(), PayloadClass::TranscriptText);
    assert_eq!(ticket.destination_label(), "https://openrouter.ai:443");
    assert!(!format!("{ticket:?}").contains("SECRET"));
}

// ---------------------------------------------------------------------------
// The HTTP chokepoint
// ---------------------------------------------------------------------------

/// Every parser rejection reached through the factory is re-attributed to the
/// calling subsystem and recorded once, so the audit trail names an owner even
/// for a URL the gate never saw.
#[test]
fn a_rejected_url_through_the_http_factory_is_attributed_and_recorded_once() {
    let policy = Arc::new(OutboundPolicy::network_permitted(permissive_grant()));
    let factory = GatedHttpClientFactory::new(Arc::clone(&policy));
    assert_eq!(factory.policy().mode(), NetworkMode::NetworkPermitted);

    for (url, expected) in [
        ("http://openrouter.ai/x", DenyReason::SchemeNotHttps),
        ("https://u:p@openrouter.ai/x", DenyReason::CredentialInUrl),
        (
            "https://127.0.0.1/x",
            DenyReason::PrivateOrLoopbackDestination,
        ),
        ("garbage", DenyReason::MalformedDestination),
    ] {
        let denied = factory
            .get(
                url,
                Subsystem::UpdateChecker,
                EgressPurpose::UpdateCheck,
                PayloadClass::None,
            )
            .unwrap_err();
        assert_eq!(denied.reason, expected, "{url}");
        assert_eq!(denied.subsystem, Subsystem::UpdateChecker);
    }

    let records = policy.observer().records();
    assert_eq!(records.len(), 4);
    assert!(records.iter().all(|r| !r.allowed));
    assert!(records
        .iter()
        .all(|r| r.subsystem == Subsystem::UpdateChecker));
}

/// A POST to a host outside the allowlist is denied by the gate, after the URL
/// parsed cleanly — the two refusal stages are distinguishable.
#[test]
fn a_well_formed_url_to_an_unlisted_host_is_denied_by_the_gate_not_the_parser() {
    let policy = Arc::new(OutboundPolicy::network_permitted(permissive_grant()));
    let factory = GatedHttpClientFactory::new(Arc::clone(&policy));
    let denied = factory
        .post(
            "https://exfil.attacker.invalid/collect",
            Subsystem::OpenRouter,
            EgressPurpose::InferenceDispatch,
            PayloadClass::AudioBytes,
        )
        .unwrap_err();
    assert_eq!(denied.reason, DenyReason::DestinationNotAllowlisted);
    assert_eq!(
        denied.destination_label,
        "https://exfil.attacker.invalid:443"
    );
    assert_eq!(policy.observer().records().len(), 1);
}

/// The request builder keeps the full URL available to a transport while the
/// accessors and `Debug` stay redaction-safe.
#[test]
fn a_gated_request_exposes_its_parts_to_a_transport_only() {
    let factory = GatedHttpClientFactory::new(Arc::new(OutboundPolicy::network_permitted(
        permissive_grant(),
    )));
    let request = factory
        .post(
            "https://openrouter.ai/api/v1/chat/completions?sig=SECRETSIG",
            Subsystem::OpenRouter,
            EgressPurpose::InferenceDispatch,
            PayloadClass::TranscriptText,
        )
        .unwrap()
        .with_header("content-type", "application/json")
        .with_body(b"{}".to_vec());

    assert_eq!(request.method(), Method::Post);
    assert_eq!(Method::Post.as_str(), "POST");
    assert_eq!(Method::Get.as_str(), "GET");
    assert!(request.url().contains("SECRETSIG"));
    assert_eq!(request.headers().len(), 1);
    assert_eq!(request.body(), Some(&b"{}"[..]));
    assert_eq!(request.ticket().payload(), PayloadClass::TranscriptText);

    let plain = factory
        .get(
            "https://openrouter.ai/api/v1/models",
            Subsystem::OpenRouter,
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        )
        .unwrap();
    assert_eq!(plain.body(), None);
    assert!(plain.headers().is_empty(), "no header was set");
}

/// The transport error vocabulary is distinct and self-describing; it is the
/// only failure a transport may report that is *not* a policy denial.
#[test]
fn transport_errors_are_distinct_from_policy_denials() {
    let messages = [
        TransportError::Failed("connection reset".to_owned()).to_string(),
        TransportError::Timeout.to_string(),
        TransportError::TooManyRedirects.to_string(),
    ];
    assert_eq!(messages[0], "transport failed: connection reset");
    assert_eq!(messages[1], "transport timed out");
    assert_eq!(messages[2], "too many redirects");
    for message in &messages {
        assert!(!message.contains("egress denied"), "{message}");
    }
}

// ---------------------------------------------------------------------------
// The subprocess chokepoint
// ---------------------------------------------------------------------------

/// Every argument rule, each asserted by its own variant.
#[test]
fn the_cli_argument_validator_names_the_exact_rule_it_refused_on() {
    assert_eq!(CliArg::Literal("list").render().unwrap(), "list");
    assert_eq!(CliArg::Number(-42).render().unwrap(), "-42");
    assert_eq!(
        CliArg::Identifier("rec-01:abc_1.v2".to_owned())
            .render()
            .unwrap(),
        "rec-01:abc_1.v2"
    );
    assert_eq!(
        CliArg::Path(std::path::PathBuf::from("/tmp/x.wav"))
            .render()
            .unwrap(),
        "/tmp/x.wav"
    );

    let cases: [(CliArg, ArgError); 7] = [
        (
            CliArg::Identifier("a".repeat(MAX_ARG_LEN + 1)),
            ArgError::TooLong,
        ),
        (
            CliArg::Identifier("rec\n--force".to_owned()),
            ArgError::ControlCharacter,
        ),
        (
            CliArg::Identifier("rec\0id".to_owned()),
            ArgError::ControlCharacter,
        ),
        (
            CliArg::Identifier("--output=/etc/passwd".to_owned()),
            ArgError::FlagInjection,
        ),
        (
            CliArg::Identifier("rec id; rm -rf /".to_owned()),
            ArgError::IllegalIdentifier,
        ),
        (
            CliArg::Path(std::path::PathBuf::from("relative/x.wav")),
            ArgError::RelativePath,
        ),
        (
            CliArg::Path(std::path::PathBuf::from("/tmp/a\u{7}b")),
            ArgError::ControlCharacter,
        ),
    ];
    for (arg, expected) in cases {
        assert_eq!(arg.render().unwrap_err(), expected, "{arg:?}");
    }

    // The flag check runs before the charset check, so `-x` reports injection.
    assert_eq!(
        CliArg::Identifier("-x".to_owned()).render().unwrap_err(),
        ArgError::FlagInjection
    );
    // An over-long absolute path is refused on length, not on charset.
    let long = format!("/{}", "a".repeat(MAX_ARG_LEN * 4));
    assert_eq!(
        CliArg::Path(std::path::PathBuf::from(long))
            .render()
            .unwrap_err(),
        ArgError::TooLong
    );
}

/// Local-only Mode refuses the launch before any argument is rendered, so an
/// invalid argument cannot mask the policy refusal.
#[test]
fn local_only_refuses_the_cloud_cli_before_arguments_are_validated() {
    let launcher = GatedProcessLauncher::new(Arc::new(OutboundPolicy::local_only()));
    let err = launcher
        .prepare_cloud_cli(
            Path::new("/usr/local/bin/plaud"),
            &[CliArg::Identifier("--force".to_owned())],
            EgressPurpose::AccountSync,
        )
        .unwrap_err();
    match err {
        SpawnError::Denied(denied) => {
            assert_eq!(denied.reason, DenyReason::LocalOnlyMode);
            assert_eq!(denied.destination_label, "subprocess:plaud");
        }
        other => panic!("expected a policy denial, got {other:?}"),
    }
}

/// With the network permitted, a bad argument is the refusal — and it names the
/// argument rule, not the policy.
#[test]
fn a_permitted_launch_still_refuses_an_injected_flag_argument() {
    let launcher = GatedProcessLauncher::new(Arc::new(OutboundPolicy::network_permitted(
        permissive_grant(),
    )));
    let err = launcher
        .prepare_cloud_cli(
            Path::new("/usr/local/bin/plaud"),
            &[
                CliArg::Literal("recordings"),
                CliArg::Identifier("--output=/etc/shadow".to_owned()),
            ],
            EgressPurpose::AccountSync,
        )
        .unwrap_err();
    match err {
        SpawnError::Arg(ArgError::FlagInjection) => {}
        other => panic!("expected a flag-injection refusal, got {other:?}"),
    }
}

/// A discovered program whose leaf is not the pinned binary is refused by the
/// gate even when the network is permitted.
#[test]
fn a_permitted_launch_refuses_a_program_that_is_not_the_pinned_cli() {
    let launcher = GatedProcessLauncher::new(Arc::new(OutboundPolicy::network_permitted(
        permissive_grant(),
    )));
    let err = launcher
        .prepare_cloud_cli(
            Path::new("/usr/bin/curl"),
            &[CliArg::Literal("list")],
            EgressPurpose::AccountSync,
        )
        .unwrap_err();
    match err {
        SpawnError::Denied(denied) => {
            assert_eq!(denied.reason, DenyReason::DestinationNotAllowlisted);
        }
        other => panic!("expected a policy denial, got {other:?}"),
    }

    // A purpose the CLI never performs is refused on the purpose rule.
    let err = launcher
        .prepare_cloud_cli(
            Path::new("/usr/local/bin/plaud"),
            &[CliArg::Literal("list")],
            EgressPurpose::ModelDownload,
        )
        .unwrap_err();
    match err {
        SpawnError::Denied(denied) => {
            assert_eq!(denied.reason, DenyReason::PurposeNotValidForSubsystem);
        }
        other => panic!("expected a policy denial, got {other:?}"),
    }
}

/// The child environment carries no proxy override, no loader override and no
/// secret-shaped variable, whatever the parent environment holds.
#[test]
fn the_child_environment_strips_proxy_loader_and_secret_variables() {
    for name in STRIPPED_ENV_NAMES {
        assert!(is_stripped_env(name), "{name} must be stripped");
    }
    for prefix in STRIPPED_ENV_PREFIXES {
        assert!(is_stripped_env(&format!("{prefix}SOMETHING")));
    }
    for name in [
        "MY_TOKEN",
        "some_secret_value",
        "VENDOR_API_KEY",
        "DB_PASSWORD",
        "DYLD_INSERT_LIBRARIES",
    ] {
        assert!(is_stripped_env(name), "{name} must be stripped");
    }
    for name in ["HOME", "PATH", "LANG", "TMPDIR", "L", ""] {
        assert!(!is_stripped_env(name), "{name} must survive");
    }

    let env = scrubbed_env();
    assert!(env.keys().all(|name| !is_stripped_env(name)));
}

/// The prepared command is built without a shell and with the scrubbed
/// environment actually installed.
#[test]
fn a_prepared_command_is_shell_free_and_uses_the_scrubbed_environment() {
    let launcher = GatedProcessLauncher::new(Arc::new(OutboundPolicy::network_permitted(
        permissive_grant(),
    )));
    let prepared = launcher
        .prepare_cloud_cli(
            Path::new("/usr/local/bin/plaud"),
            &[
                CliArg::Literal("recordings"),
                CliArg::Literal("list"),
                CliArg::Number(25),
                CliArg::Identifier("cursor:abc-1".to_owned()),
            ],
            EgressPurpose::AccountSync,
        )
        .unwrap();

    assert_eq!(prepared.program(), Path::new("/usr/local/bin/plaud"));
    assert_eq!(prepared.args().len(), 4);
    assert_eq!(prepared.ticket().subsystem(), Subsystem::PlaudCloudCli);
    assert_eq!(prepared.timeout().as_secs(), 120);
    assert_eq!(prepared.max_output_bytes(), 8 * 1024 * 1024);
    assert!(prepared.env().keys().all(|name| !is_stripped_env(name)));

    let command = prepared.into_command();
    let program = command.get_program().to_string_lossy().into_owned();
    assert!(!program.contains("sh"), "{program}");
    assert!(!program.contains("cmd"), "{program}");
    assert_eq!(command.get_args().count(), 4);
}
