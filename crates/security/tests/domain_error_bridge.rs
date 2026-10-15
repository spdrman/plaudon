//! The one bridge from `crates/security` errors into the application taxonomy
//! (§15.4).
//!
//! The inline tests in `src/domain_map.rs` cover the code table and the egress
//! special case. This suite drives every `From<…> for AppError` impl, because
//! each one is a branch that only runs when the corresponding subsystem
//! actually refuses something.

use std::path::Path;
use std::time::Duration;

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_security::consent::{
    dispatch_preflight, ConsentError, ConsentPurpose, ContentType, DispatchRequest, ProviderRoute,
    RELAY_OPENROUTER,
};
use plaudon_security::domain_map::{app_error_for, to_app_error_code, SECURITY_CODES};
use plaudon_security::error::SecurityError;
use plaudon_security::fsguard::{
    validate_archive_entry, validate_relative, ArchiveEntryKind, ArchiveEntryMeta, ArchivePolicy,
    PathPolicy,
};
use plaudon_security::local_only::SettingsError;
use plaudon_security::outbound::{
    ApprovalSource, Destination, EgressPurpose, EgressRequest, NetworkGrant, OutboundPolicy,
    PayloadClass, Subsystem,
};
use plaudon_security::sandbox::{SandboxError, SandboxSpec, TimeoutBudget};
use plaudon_security::secrets::{ScopeError, SecretScope, SecretStoreError, StoreKind};

/// A settings failure that is not the §15.2 confirmation case falls through to
/// the generic envelope mapping rather than claiming a confirmation is missing.
#[test]
fn a_settings_failure_that_is_not_about_confirmation_keeps_the_egress_code() {
    let confirmation: AppError = SettingsError::ConfirmationRequired.into();
    assert_eq!(confirmation.code(), AppErrorCode::ConfirmationRequired);
    assert!(confirmation.detail().contains("set_local_only_mode"));

    for other in [
        SettingsError::MissingDisclosureVersion,
        SettingsError::Persistence("disk full".to_owned()),
    ] {
        let mapped: AppError = other.clone().into();
        assert_eq!(mapped.code(), AppErrorCode::EgressDenied, "{other:?}");
        assert!(
            !mapped.detail().contains("disk full"),
            "the internal detail is the envelope message, not the raw cause: {}",
            mapped.detail()
        );
    }
}

/// A locked credential store reaches the taxonomy as the actionable locked
/// state; a hard backend failure reaches it as an internal error; a missing
/// entry as a not-found.
#[test]
fn every_credential_store_failure_family_reaches_its_own_domain_code() {
    let cases = [
        (
            SecretStoreError::Locked {
                store: StoreKind::MacosKeychain,
            },
            AppErrorCode::SecretStoreLocked,
        ),
        (
            SecretStoreError::Unavailable {
                store: StoreKind::Unsupported,
                reason: plaudon_security::secrets::UnavailableReason::UnsupportedPlatform,
            },
            AppErrorCode::SecretStoreLocked,
        ),
        (SecretStoreError::NotFound, AppErrorCode::RecordNotFound),
        (
            SecretStoreError::backend(StoreKind::MacosKeychain, "keychain write", -1),
            AppErrorCode::InternalError,
        ),
        (
            SecretStoreError::InvalidScope(ScopeError::Reserved),
            AppErrorCode::InvalidArgument,
        ),
    ];
    for (error, expected) in cases {
        let mapped: AppError = error.clone().into();
        assert_eq!(mapped.code(), expected, "{error:?}");
        assert!(!mapped.detail().is_empty(), "{error:?} lost its detail");
    }

    // A scope rejection converts directly too, without passing through a store.
    let scope_error = SecretScope::device("*").unwrap_err();
    let mapped: AppError = scope_error.into();
    assert_eq!(mapped.code(), AppErrorCode::InvalidArgument);
}

/// A consent failure caused by the egress gate keeps the gate's code, so
/// "Local-only Mode denied it" is never reported as "you need consent".
#[test]
fn a_consent_failure_caused_by_the_gate_keeps_the_gate_code() {
    let route =
        ProviderRoute::new(RELAY_OPENROUTER, "vendor-x/model").with_downstream_provider("vendor-x");
    let request = DispatchRequest::new(
        ConsentPurpose::Transcription,
        ContentType::AudioBytes,
        route,
        "rec-1",
        "privacy-v1",
    );

    let gated = dispatch_preflight(&OutboundPolicy::local_only(), &[], &request, 0).unwrap_err();
    assert!(matches!(gated, ConsentError::Egress(_)));
    let mapped: AppError = gated.into();
    assert_eq!(mapped.code(), AppErrorCode::LocalOnlyMode);

    let permitted = OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        0,
        "privacy-v1",
    ));
    let needs_consent = dispatch_preflight(&permitted, &[], &request, 0).unwrap_err();
    assert!(matches!(needs_consent, ConsentError::Required { .. }));
    let mapped: AppError = needs_consent.into();
    assert_eq!(mapped.code(), AppErrorCode::ConsentRequired);
}

/// A rejected path and a rejected archive entry share the one archive/path code
/// and carry their rule in the detail.
#[test]
fn path_and_archive_rejections_convert_with_their_rule_in_the_detail() {
    let path_error = validate_relative(&PathPolicy::default(), "../escape").unwrap_err();
    let mapped: AppError = path_error.into();
    assert_eq!(mapped.code(), AppErrorCode::ArchivePathRejected);
    assert!(mapped.detail().contains("parent_traversal"));

    let mut policy = ArchivePolicy::new("/managed/restore");
    let entry_error = validate_archive_entry(
        &ArchiveEntryMeta::of_kind("evil", ArchiveEntryKind::Symlink),
        &mut policy,
    )
    .unwrap_err();
    let mapped: AppError = entry_error.into();
    assert_eq!(mapped.code(), AppErrorCode::ArchivePathRejected);
    assert!(mapped.detail().contains("link_entry"));
}

/// A timed-out helper is a timeout; every other helper failure is a helper
/// failure, so the job engine never records a killed helper as completed.
#[test]
fn helper_failures_split_between_timeout_and_helper_failed() {
    let timeout: AppError = SandboxError::Timeout {
        budget: TimeoutBudget::WallClock,
        elapsed_ms: 10,
    }
    .into();
    assert_eq!(timeout.code(), AppErrorCode::Timeout);

    for error in [
        SandboxError::Killed { signal: 9 },
        SandboxError::Crashed { code: 1 },
        SandboxError::Oom {
            detail: "rss breach".to_owned(),
        },
        SandboxError::Protocol {
            detail: "trailing garbage".to_owned(),
        },
        SandboxError::SpawnFailed {
            detail: "NotFound".to_owned(),
        },
    ] {
        let mapped: AppError = error.clone().into();
        assert_eq!(mapped.code(), AppErrorCode::HelperFailed, "{error:?}");
    }

    assert_eq!(
        AppError::from(SandboxError::Timeout {
            budget: TimeoutBudget::Cpu,
            elapsed_ms: 1,
        })
        .code(),
        AppErrorCode::Timeout
    );
    assert_eq!(TimeoutBudget::WallClock.to_string(), "wall-clock");
    assert_eq!(TimeoutBudget::Cpu.to_string(), "cpu-time");
}

/// A rejected sandbox spec splits the same way its envelope does: a bad grant
/// is a path problem the user can act on, a bad budget is a helper problem.
#[test]
fn a_rejected_sandbox_spec_splits_between_path_and_helper_codes() {
    let relative = SandboxSpec::builder()
        .read_path("relative/input.wav")
        .build()
        .unwrap_err();
    let mapped: AppError = relative.into();
    assert_eq!(mapped.code(), AppErrorCode::ArchivePathRejected);

    let tiny = SandboxSpec::builder()
        .memory_bytes(1024)
        .build()
        .unwrap_err();
    let mapped: AppError = tiny.into();
    assert_eq!(mapped.code(), AppErrorCode::HelperFailed);

    let no_cpu = SandboxSpec::builder().cpu_seconds(0).build().unwrap_err();
    assert_eq!(AppError::from(no_cpu).code(), AppErrorCode::HelperFailed);

    let wall = SandboxSpec::builder()
        .wall_clock(Duration::from_millis(1))
        .build()
        .unwrap_err();
    assert_eq!(AppError::from(wall).code(), AppErrorCode::HelperFailed);

    let missing_parent = SandboxSpec::builder()
        .read_path(Path::new("/definitely/not/here/plaudon-cov/input.wav"))
        .build()
        .unwrap_err();
    assert_eq!(
        AppError::from(missing_parent).code(),
        AppErrorCode::ArchivePathRejected
    );
}

/// The code table is total over everything this crate can produce, and
/// `app_error_for` carries the security explanation into the detail while the
/// user-facing wording stays the domain's.
#[test]
fn the_code_table_is_total_and_the_explanation_lands_in_the_detail() {
    for code in SECURITY_CODES {
        let mapped = to_app_error_code(code).unwrap_or_else(|| panic!("{code} has no mapping"));
        assert!(
            AppErrorCode::ALL.contains(&mapped),
            "{mapped} is outside the closed domain set"
        );
    }
    assert_eq!(SECURITY_CODES.len(), 15);

    let error = validate_relative(&PathPolicy::default(), "..").unwrap_err();
    let mapped = app_error_for(&error);
    assert_eq!(mapped.code(), AppErrorCode::ArchivePathRejected);
    assert_eq!(mapped.detail(), error.envelope_parts().message);
    assert_eq!(
        mapped.code().canonical_message(),
        AppErrorCode::ArchivePathRejected.canonical_message(),
        "the user-facing wording belongs to the domain, not to us"
    );
}

/// The gate's own conversion distinguishes the one reason the user can act on
/// from every other refusal.
#[test]
fn the_gate_conversion_separates_the_actionable_reason_from_the_rest() {
    let local = OutboundPolicy::local_only();
    let denied = local
        .authorize(&EgressRequest::new(
            Subsystem::OpenRouter,
            Destination::url("https://openrouter.ai/api/v1/models").unwrap(),
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        ))
        .unwrap_err();
    assert_eq!(AppError::from(denied).code(), AppErrorCode::LocalOnlyMode);

    let permitted = OutboundPolicy::network_permitted(
        NetworkGrant::new(ApprovalSource::TestHarness, 0, "privacy-v1")
            .with_allowed_hosts(vec!["openrouter.ai".to_owned()]),
    );
    let denied = permitted
        .authorize(&EgressRequest::new(
            Subsystem::OpenRouter,
            Destination::url("https://exfil.attacker.invalid/x").unwrap(),
            EgressPurpose::ModelCatalog,
            PayloadClass::None,
        ))
        .unwrap_err();
    let mapped = AppError::from(denied);
    assert_eq!(mapped.code(), AppErrorCode::EgressDenied);
    assert!(mapped.detail().contains("destination_not_allowlisted"));
    assert!(!mapped.detail().contains("/x"));
}
