//! Every updater rejection maps onto the shared §15.4 taxonomy, says which
//! check failed, and leaks nothing (SPEC v1.3 §15.4, §16.5, §18.5).
//!
//! `error.rs` is the one module whose whole job is classification, so the
//! suite enumerates *every* variant rather than the handful that the
//! happy-path suites happen to construct: a new variant that forgets its code
//! or its retryability is a compile error here, not a silent default.

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_updater::error::{SignatureSubject, UpdateError, VerifyError};
use plaudon_updater::install::InterruptPoint;
use plaudon_updater::manifest::Version;
use plaudon_updater::policy::{SuppressionReason, TransportError};

const V1: Version = Version::new(1, 0, 0);
const V2: Version = Version::new(1, 1, 0);

/// One instance of every [`VerifyError`] variant, in declaration order.
fn every_verify_error() -> Vec<VerifyError> {
    vec![
        VerifyError::UnsignedManifest,
        VerifyError::UnknownKey {
            key_id: "attacker-2026".to_owned(),
        },
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Manifest,
        },
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Package,
        },
        VerifyError::DigestMismatch {
            expected: "ab".repeat(32),
            actual: "cd".repeat(32),
        },
        VerifyError::TruncatedPackage {
            expected: 512,
            actual: 448,
        },
        VerifyError::MalformedManifest("package_size is zero".to_owned()),
        VerifyError::DowngradeBlocked {
            offered: Version::new(0, 9, 0),
            installed: V2,
            floor: V1,
        },
        VerifyError::UnsupportedTarget {
            offered: "windows/x86_64".to_owned(),
            host: "mac_os/aarch64".to_owned(),
        },
    ]
}

/// One instance of every [`UpdateError`] variant, in declaration order.
fn every_update_error() -> Vec<UpdateError> {
    vec![
        UpdateError::Verify(VerifyError::UnsignedManifest),
        UpdateError::Suppressed(SuppressionReason::LocalOnlyMode),
        UpdateError::Transport(TransportError::Unreachable),
        UpdateError::Interrupted(InterruptPoint::BeforeCommit),
        UpdateError::NoRetainedVersion,
        UpdateError::JournalCorrupt("expected value at line 1".to_owned()),
        UpdateError::SlotIncomplete {
            slot: "b".to_owned(),
        },
        UpdateError::io(
            "write journal",
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        ),
        UpdateError::Encoding {
            source: serde_json::from_str::<u8>("\"not a number\"").expect_err("not a u8"),
        },
    ]
}

#[test]
fn every_verify_error_classifies_itself() {
    let expected = [
        AppErrorCode::IntegrityFailed,
        AppErrorCode::IntegrityFailed,
        AppErrorCode::IntegrityFailed,
        AppErrorCode::IntegrityFailed,
        AppErrorCode::IntegrityFailed,
        AppErrorCode::IntegrityFailed,
        AppErrorCode::MalformedOutput,
        AppErrorCode::InvalidArgument,
        AppErrorCode::InvalidArgument,
    ];
    let errors = every_verify_error();
    assert_eq!(errors.len(), expected.len());
    for (error, code) in errors.iter().zip(expected) {
        assert_eq!(error.code(), code, "{error} is classified wrongly");
    }
}

#[test]
fn every_verify_error_converts_into_the_shared_envelope_without_leaking() {
    for error in every_verify_error() {
        let code = error.code();
        let message = error.to_string();
        let app: AppError = error.into();
        assert_eq!(app.code(), code);
        assert_eq!(app.detail(), message);
        assert!(
            !app.detail().contains("updates.plaudon.app"),
            "a rejection must not carry the update endpoint: {}",
            app.detail()
        );
        assert!(
            !app.detail().contains("BEGIN"),
            "a rejection must not carry key material: {}",
            app.detail()
        );
    }
}

#[test]
fn verify_error_messages_name_the_failing_check() {
    assert_eq!(
        VerifyError::UnsignedManifest.to_string(),
        "update manifest carries no detached signature"
    );
    assert_eq!(
        VerifyError::UnknownKey {
            key_id: "attacker-2026".to_owned()
        }
        .to_string(),
        "update manifest is signed by unpinned key id `attacker-2026`"
    );
    assert_eq!(
        VerifyError::TruncatedPackage {
            expected: 512,
            actual: 448
        }
        .to_string(),
        "package truncated: manifest declared 512 bytes, 448 present"
    );
    assert_eq!(
        VerifyError::DowngradeBlocked {
            offered: Version::new(0, 9, 0),
            installed: V2,
            floor: V1
        }
        .to_string(),
        "downgrade blocked: 0.9.0 is not permitted from 1.1.0 (floor 1.0.0)"
    );
    assert_eq!(
        VerifyError::UnsupportedTarget {
            offered: "windows/x86_64".to_owned(),
            host: "mac_os/aarch64".to_owned()
        }
        .to_string(),
        "manifest targets windows/x86_64, this build is mac_os/aarch64"
    );
    assert_eq!(
        VerifyError::MalformedManifest("package_size is zero".to_owned()).to_string(),
        "malformed update manifest: package_size is zero"
    );
}

#[test]
fn both_signature_subjects_render_distinctly() {
    assert_eq!(SignatureSubject::Manifest.to_string(), "manifest");
    assert_eq!(SignatureSubject::Package.to_string(), "package");
    assert_ne!(SignatureSubject::Manifest, SignatureSubject::Package);
    assert_eq!(
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Manifest
        }
        .to_string(),
        "ed25519 signature over the manifest did not verify"
    );
    assert_eq!(
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Package
        }
        .to_string(),
        "ed25519 signature over the package did not verify"
    );
}

#[test]
fn a_digest_mismatch_reports_both_digests() {
    let error = VerifyError::DigestMismatch {
        expected: "ab".repeat(32),
        actual: "cd".repeat(32),
    };
    let message = error.to_string();
    assert!(message.contains(&"ab".repeat(32)), "{message}");
    assert!(message.contains(&"cd".repeat(32)), "{message}");
}

#[test]
fn every_update_error_classifies_itself() {
    let expected = [
        AppErrorCode::IntegrityFailed,
        AppErrorCode::LocalOnlyMode,
        AppErrorCode::SourceUnavailable,
        AppErrorCode::Canceled,
        AppErrorCode::RecordNotFound,
        AppErrorCode::IntegrityFailed,
        AppErrorCode::IntegrityFailed,
        AppErrorCode::StorageUnavailable,
        AppErrorCode::StorageUnavailable,
    ];
    let errors = every_update_error();
    assert_eq!(errors.len(), expected.len());
    for (error, code) in errors.iter().zip(expected) {
        assert_eq!(error.code(), code, "{error} is classified wrongly");
    }
}

#[test]
fn only_the_transient_update_errors_are_retryable() {
    let expected = [
        false, // a rejected artifact fails the same check again
        false, // suppression is a policy decision, not a glitch
        true,  // the server may come back
        true,  // the machine may stay up next time
        false, // there is still no retained version
        true,  // the journal may be repaired
        true,  // the slot may be rewritten
        true,  // the filesystem may recover
        false, // the same value will not encode next time either
    ];
    let errors = every_update_error();
    assert_eq!(errors.len(), expected.len());
    for (error, retryable) in errors.iter().zip(expected) {
        assert_eq!(
            error.is_retryable(),
            retryable,
            "{error} has the wrong retry classification"
        );
    }
    // An operation-level retry is *not* the same judgement as the taxonomy's
    // user-facing one: a corrupt journal is worth re-reading from the
    // installer's point of view while still being a terminal integrity
    // failure in the UI (§15.4).
    assert!(!AppErrorCode::IntegrityFailed.is_retryable());
    assert!(UpdateError::JournalCorrupt(String::new()).is_retryable());
}

#[test]
fn every_update_error_converts_into_the_shared_envelope() {
    for error in every_update_error() {
        let code = error.code();
        let message = error.to_string();
        assert!(!message.is_empty(), "every rejection explains itself");
        let app: AppError = error.into();
        assert_eq!(app.code(), code);
        assert_eq!(app.detail(), message);
        assert!(
            !app.detail().contains("updates.plaudon.app"),
            "no endpoint in error detail: {}",
            app.detail()
        );
    }
}

#[test]
fn update_error_messages_name_the_step_that_failed() {
    assert_eq!(
        UpdateError::Interrupted(InterruptPoint::BeforeRollbackCommit).to_string(),
        "install interrupted at before_rollback_commit"
    );
    assert_eq!(
        UpdateError::SlotIncomplete {
            slot: "b".to_owned()
        }
        .to_string(),
        "installed slot `b` is incomplete"
    );
    assert_eq!(
        UpdateError::NoRetainedVersion.to_string(),
        "no retained previous version to roll back to"
    );
    assert_eq!(
        UpdateError::Suppressed(SuppressionReason::AutomaticUpdatesDisabled).to_string(),
        "update check suppressed: automatic update checks disabled"
    );
    assert_eq!(
        UpdateError::JournalCorrupt("truncated".to_owned()).to_string(),
        "update journal is unreadable: truncated"
    );
    // The transparent variants keep the inner wording verbatim.
    assert_eq!(
        UpdateError::Transport(TransportError::UnexpectedResponse).to_string(),
        TransportError::UnexpectedResponse.to_string()
    );
    assert_eq!(
        UpdateError::Verify(VerifyError::UnsignedManifest).to_string(),
        VerifyError::UnsignedManifest.to_string()
    );
}

#[test]
fn the_io_constructor_keeps_its_context_and_its_source() {
    let error = UpdateError::io(
        "write journal",
        std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );
    assert_eq!(error.to_string(), "filesystem failure during write journal");
    let source = std::error::Error::source(&error).expect("the os error is preserved");
    assert!(
        source.to_string().to_lowercase().contains("permission"),
        "the underlying cause must survive: {source}"
    );
    assert!(matches!(error, UpdateError::Io { context, .. } if context == "write journal"));
}

#[test]
fn a_verify_error_promotes_into_an_update_error_through_from() {
    let promoted: UpdateError = VerifyError::TruncatedPackage {
        expected: 512,
        actual: 0,
    }
    .into();
    assert!(matches!(
        promoted,
        UpdateError::Verify(VerifyError::TruncatedPackage {
            expected: 512,
            actual: 0
        })
    ));
    assert_eq!(promoted.code(), AppErrorCode::IntegrityFailed);
    assert!(!promoted.is_retryable());

    let promoted: UpdateError = TransportError::Unreachable.into();
    assert!(matches!(
        promoted,
        UpdateError::Transport(TransportError::Unreachable)
    ));
    assert!(promoted.is_retryable());
}

#[test]
fn verify_errors_compare_by_value() {
    assert_eq!(
        VerifyError::UnknownKey {
            key_id: "k".to_owned()
        },
        VerifyError::UnknownKey {
            key_id: "k".to_owned()
        }
    );
    assert_ne!(
        VerifyError::UnknownKey {
            key_id: "k".to_owned()
        },
        VerifyError::UnknownKey {
            key_id: "other".to_owned()
        }
    );
    assert_ne!(
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Manifest
        },
        VerifyError::SignatureMismatch {
            subject: SignatureSubject::Package
        }
    );
    // Cloning a rejection must not change its classification.
    let error = VerifyError::MalformedManifest("x".to_owned());
    assert_eq!(error.clone().code(), error.code());
    assert!(format!("{error:?}").contains("MalformedManifest"));
}
