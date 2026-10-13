//! The single bridge from `crates/security`'s typed errors to the application's
//! closed error taxonomy, `plaudon_domain::error::AppError` (spec §15.4).
//!
//! `crates/security` deliberately does not define a second user-facing envelope.
//! Each security error implements [`SecurityError`], which yields a
//! [`ErrorEnvelopeParts`] carrying a crate-local [`ErrorCode`]; this module is
//! the one place that turns those into `AppErrorCode` values.
//!
//! Division of responsibility:
//!
//! * the **code** decides the user-visible message, retryability and affordance
//!   — `plaudon_domain` owns all three, so the wording a user sees is the same
//!   wherever the error surfaces and the view layer never invents its own;
//! * the **security explanation** (e.g. *which* deny reason fired) is carried as
//!   `AppError::detail`, which §15.4 keeps out of the wire envelope and routes
//!   to the redacted diagnostics log instead.
//!
//! That split is deliberate: a user does not need to know that the egress gate
//! refused because the host was not allowlisted rather than because Local-only
//! Mode was on, but an engineer reading a diagnostic bundle does.

use plaudon_domain::error::{AppError, AppErrorCode};

use crate::error::{
    ErrorCode, SecurityError, ARCHIVE_ENTRY_REJECTED, COMMAND_ARGUMENT_INVALID,
    COMMAND_NOT_AUTHORIZED, CONSENT_DISCLOSURE_CHANGED, CONSENT_REQUIRED, CONSENT_REVOKED,
    CONSENT_SCOPE_MISMATCH, EGRESS_DENIED, HELPER_FAILED, HELPER_TIMEOUT, PATH_REJECTED,
    PRIVACY_POSTURE_UNRESOLVED, SECRET_NOT_FOUND, SECRET_STORE_FAILED, SECRET_STORE_LOCKED,
};

/// Every code this crate can produce. The mapping below must stay total over
/// this slice; [`tests::every_security_code_maps_to_a_domain_code`] enforces it.
pub const SECURITY_CODES: [ErrorCode; 15] = [
    EGRESS_DENIED,
    SECRET_STORE_LOCKED,
    SECRET_STORE_FAILED,
    SECRET_NOT_FOUND,
    CONSENT_REQUIRED,
    CONSENT_SCOPE_MISMATCH,
    CONSENT_REVOKED,
    CONSENT_DISCLOSURE_CHANGED,
    PRIVACY_POSTURE_UNRESOLVED,
    PATH_REJECTED,
    ARCHIVE_ENTRY_REJECTED,
    HELPER_FAILED,
    HELPER_TIMEOUT,
    COMMAND_NOT_AUTHORIZED,
    COMMAND_ARGUMENT_INVALID,
];

/// Translate a security code into the application taxonomy.
///
/// Returns `None` only for a code that is not in [`SECURITY_CODES`], which
/// cannot happen for errors produced by this crate; callers that handle a code
/// from elsewhere should fall back to [`AppErrorCode::InternalError`].
#[must_use]
pub fn to_app_error_code(code: ErrorCode) -> Option<AppErrorCode> {
    let mapped = match code.as_str() {
        // The gate refused network initiation. `EGRESS_DENIED` covers every
        // deny reason; `LOCAL_ONLY_MODE` is reserved for the case the user can
        // act on directly, which `app_error_for` selects below.
        "EGRESS_DENIED" => AppErrorCode::EgressDenied,

        // §16.4: an unavailable or locked store is a locked state, never a
        // downgrade, so both share the actionable code.
        "SECRET_STORE_LOCKED" => AppErrorCode::SecretStoreLocked,
        "SECRET_STORE_FAILED" => AppErrorCode::InternalError,
        "SECRET_NOT_FOUND" => AppErrorCode::RecordNotFound,

        // §16.3: every consent failure asks the user for the same thing — a
        // fresh, correctly scoped decision — so they share one code and differ
        // in `detail`.
        "CONSENT_REQUIRED"
        | "CONSENT_SCOPE_MISMATCH"
        | "CONSENT_REVOKED"
        | "CONSENT_DISCLOSURE_CHANGED" => AppErrorCode::ConsentRequired,

        // §10.8: the privacy posture of the exact endpoint/route did not
        // resolve, so nothing was sent.
        "PRIVACY_POSTURE_UNRESOLVED" => AppErrorCode::PolicyUnresolved,

        // §16.5, §13.5.
        "PATH_REJECTED" | "ARCHIVE_ENTRY_REJECTED" => AppErrorCode::ArchivePathRejected,

        // §6.3A, §14: a killed or crashed helper is recoverable and must never
        // be recorded as a completed job.
        "HELPER_FAILED" => AppErrorCode::HelperFailed,
        "HELPER_TIMEOUT" => AppErrorCode::Timeout,

        // §6.3A, §15.1.
        "COMMAND_NOT_AUTHORIZED" => AppErrorCode::CommandNotAuthorized,
        "COMMAND_ARGUMENT_INVALID" => AppErrorCode::InvalidArgument,

        _ => return None,
    };
    Some(mapped)
}

/// Convert any security error into an [`AppError`].
///
/// The envelope message produced by the security layer becomes the internal
/// `detail`; the user-facing text comes from the domain code.
pub fn app_error_for<E: SecurityError + ?Sized>(error: &E) -> AppError {
    let parts = error.envelope_parts();
    let code = to_app_error_code(parts.code).unwrap_or(AppErrorCode::InternalError);
    AppError::new(code, parts.message)
}

// ---------------------------------------------------------------------------
// `From` impls, so call sites can use `?` against `AppError` directly.
// ---------------------------------------------------------------------------

/// Egress denials keep their distinction: `LocalOnlyMode` is the one reason the
/// user can resolve by changing a setting, and §22(15) makes it a named state,
/// so it maps to `LOCAL_ONLY_MODE` rather than the generic `EGRESS_DENIED`.
impl From<crate::outbound::EgressDenied> for AppError {
    fn from(denied: crate::outbound::EgressDenied) -> Self {
        let code = if denied.reason == crate::outbound::DenyReason::LocalOnlyMode {
            AppErrorCode::LocalOnlyMode
        } else {
            AppErrorCode::EgressDenied
        };
        AppError::new(
            code,
            format!(
                "egress denied: reason={} subsystem={} destination={}",
                denied.reason.label(),
                denied.subsystem.label(),
                denied.destination_label
            ),
        )
    }
}

impl From<crate::local_only::SettingsError> for AppError {
    fn from(error: crate::local_only::SettingsError) -> Self {
        match error {
            // Leaving Local-only Mode without the §15.2 confirmation is exactly
            // what `CONFIRMATION_REQUIRED` describes.
            crate::local_only::SettingsError::ConfirmationRequired => AppError::new(
                AppErrorCode::ConfirmationRequired,
                "set_local_only_mode(false) without explicit confirmation",
            ),
            other => app_error_for(&other),
        }
    }
}

impl From<crate::secrets::SecretStoreError> for AppError {
    fn from(error: crate::secrets::SecretStoreError) -> Self {
        app_error_for(&error)
    }
}

impl From<crate::secrets::ScopeError> for AppError {
    fn from(error: crate::secrets::ScopeError) -> Self {
        app_error_for(&error)
    }
}

impl From<crate::consent::ConsentError> for AppError {
    fn from(error: crate::consent::ConsentError) -> Self {
        // A consent failure caused by the egress gate keeps the gate's code, so
        // "Local-only Mode denied it" is never reported as "you need consent".
        if let crate::consent::ConsentError::Egress(denied) = error {
            return AppError::from(denied);
        }
        app_error_for(&error)
    }
}

impl From<crate::fsguard::PathError> for AppError {
    fn from(error: crate::fsguard::PathError) -> Self {
        app_error_for(&error)
    }
}

impl From<crate::fsguard::ArchiveEntryError> for AppError {
    fn from(error: crate::fsguard::ArchiveEntryError) -> Self {
        app_error_for(&error)
    }
}

impl From<crate::sandbox::SandboxError> for AppError {
    fn from(error: crate::sandbox::SandboxError) -> Self {
        app_error_for(&error)
    }
}

impl From<crate::sandbox::SpecError> for AppError {
    fn from(error: crate::sandbox::SpecError) -> Self {
        app_error_for(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbound::{
        ApprovalSource, DenyReason, Destination, EgressPurpose, EgressRequest, NetworkGrant,
        OutboundPolicy, PayloadClass, Subsystem,
    };

    #[test]
    fn every_security_code_maps_to_a_domain_code() {
        for code in SECURITY_CODES {
            assert!(
                to_app_error_code(code).is_some(),
                "security code {code} has no domain mapping"
            );
        }
    }

    #[test]
    fn every_mapped_code_is_a_member_of_the_closed_domain_set() {
        for code in SECURITY_CODES {
            let mapped = to_app_error_code(code).expect("mapped");
            assert!(
                AppErrorCode::ALL.contains(&mapped),
                "{mapped} is not in AppErrorCode::ALL"
            );
        }
    }

    #[test]
    fn an_unknown_code_has_no_mapping_rather_than_a_guessed_one() {
        assert!(to_app_error_code(crate::error::ErrorCode::new("NOPE")).is_none());
    }

    #[test]
    fn local_only_denial_is_reported_as_local_only_mode() {
        let policy = OutboundPolicy::local_only();
        let denied = policy
            .authorize(&EgressRequest::new(
                Subsystem::OpenRouter,
                Destination::url("https://openrouter.ai/api/v1/models").unwrap(),
                EgressPurpose::ModelCatalog,
                PayloadClass::None,
            ))
            .unwrap_err();
        assert_eq!(denied.reason, DenyReason::LocalOnlyMode);

        let app: AppError = denied.into();
        assert_eq!(app.code(), AppErrorCode::LocalOnlyMode);
        assert!(app.code().action().is_some());
    }

    #[test]
    fn a_non_local_only_denial_is_reported_as_egress_denied() {
        let policy = OutboundPolicy::network_permitted(
            NetworkGrant::new(ApprovalSource::TestHarness, 0, "privacy-v1")
                .with_allowed_hosts(vec!["openrouter.ai".to_owned()]),
        );
        let denied = policy
            .authorize(&EgressRequest::new(
                Subsystem::OpenRouter,
                Destination::url("https://cdn.attacker.invalid/x").unwrap(),
                EgressPurpose::ModelCatalog,
                PayloadClass::None,
            ))
            .unwrap_err();
        assert_eq!(denied.reason, DenyReason::DestinationNotAllowlisted);

        let app: AppError = denied.into();
        assert_eq!(app.code(), AppErrorCode::EgressDenied);
    }

    #[test]
    fn the_security_explanation_lands_in_detail_not_in_the_user_message() {
        let policy = OutboundPolicy::local_only();
        let denied = policy
            .authorize(&EgressRequest::new(
                Subsystem::Telemetry,
                Destination::url("https://telemetry.example.invalid/x").unwrap(),
                EgressPurpose::TelemetryFlush,
                PayloadClass::Metadata,
            ))
            .unwrap_err();

        let app: AppError = denied.into();
        assert!(app.detail().contains("local_only_mode"));
        assert!(app.detail().contains("telemetry"));
        // The user-facing text is the domain's, not ours.
        assert_eq!(
            app.code().canonical_message(),
            AppErrorCode::LocalOnlyMode.canonical_message()
        );
    }

    #[test]
    fn detail_never_carries_a_url_path_or_query() {
        let policy = OutboundPolicy::local_only();
        let denied = policy
            .authorize(&EgressRequest::new(
                Subsystem::OpenRouter,
                Destination::url("https://openrouter.ai/v1/audio?sig=SECRETSIGNATURE").unwrap(),
                EgressPurpose::InferenceDispatch,
                PayloadClass::AudioBytes,
            ))
            .unwrap_err();
        let app: AppError = denied.into();
        assert!(
            !app.detail().contains("SECRETSIGNATURE"),
            "{}",
            app.detail()
        );
        assert!(!app.detail().contains("/v1/audio"), "{}", app.detail());
    }

    /// VP-110/VP-112: the refusal reaches the user as an error, and that error
    /// may not carry the thing it refused. The detail is the fixed vocabulary
    /// of labels, never a name, a score or a vector.
    #[test]
    fn a_voice_biometric_denial_carries_labels_and_no_biometric_content() {
        for policy in [
            OutboundPolicy::local_only(),
            OutboundPolicy::network_permitted(
                NetworkGrant::new(ApprovalSource::TestHarness, 0, "privacy-v1")
                    .with_allowed_hosts(vec!["openrouter.ai".to_owned()]),
            ),
        ] {
            let denied = policy
                .authorize(&EgressRequest::new(
                    Subsystem::OpenRouter,
                    Destination::url("https://openrouter.ai/api/v1/embeddings").unwrap(),
                    EgressPurpose::InferenceDispatch,
                    PayloadClass::VoiceBiometric,
                ))
                .unwrap_err();
            assert_eq!(denied.reason, DenyReason::VoiceBiometricNeverEgresses);

            let app: AppError = denied.into();
            // Not `LOCAL_ONLY_MODE`: that code offers the user a setting to
            // change, and no setting changes this one.
            assert_eq!(app.code(), AppErrorCode::EgressDenied);
            assert!(
                app.detail().contains("voice_biometric_never_egresses"),
                "{}",
                app.detail()
            );
            for forbidden in ["Dana", "0.97", "embedding=", "[0."] {
                assert!(
                    !app.detail().contains(forbidden),
                    "detail carried `{forbidden}`: {}",
                    app.detail()
                );
            }
        }
    }

    #[test]
    fn missing_confirmation_maps_to_confirmation_required() {
        let error = crate::local_only::SettingsError::ConfirmationRequired;
        let app: AppError = error.into();
        assert_eq!(app.code(), AppErrorCode::ConfirmationRequired);
    }

    #[test]
    fn a_locked_credential_store_is_retryable_and_actionable() {
        let mapped = to_app_error_code(SECRET_STORE_LOCKED).expect("mapped");
        assert_eq!(mapped, AppErrorCode::SecretStoreLocked);
        assert!(
            mapped.is_retryable(),
            "a locked keychain can be unlocked and retried"
        );
        assert!(mapped.action().is_some());
    }

    #[test]
    fn a_failed_helper_is_retryable_so_the_job_engine_can_persist_a_recoverable_state() {
        let mapped = to_app_error_code(HELPER_FAILED).expect("mapped");
        assert_eq!(mapped, AppErrorCode::HelperFailed);
        assert!(mapped.is_retryable());
        assert_ne!(
            mapped,
            AppErrorCode::InternalError,
            "a killed helper must be distinguishable from an internal bug (§14)"
        );
    }

    #[test]
    fn consent_failures_all_ask_for_the_same_user_action() {
        for code in [
            CONSENT_REQUIRED,
            CONSENT_SCOPE_MISMATCH,
            CONSENT_REVOKED,
            CONSENT_DISCLOSURE_CHANGED,
        ] {
            assert_eq!(to_app_error_code(code), Some(AppErrorCode::ConsentRequired));
        }
    }

    #[test]
    fn path_and_archive_rejections_share_the_archive_path_code() {
        assert_eq!(
            to_app_error_code(PATH_REJECTED),
            Some(AppErrorCode::ArchivePathRejected)
        );
        assert_eq!(
            to_app_error_code(ARCHIVE_ENTRY_REJECTED),
            Some(AppErrorCode::ArchivePathRejected)
        );
    }
}
