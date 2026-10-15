//! Credential-store failure modes and the vocabulary they report (§16.4,
//! §7.7, §6.3A).
//!
//! The rule under test is that an unavailable or locked vault yields an
//! actionable locked state and **never** a value — so every one of these cases
//! asserts the specific error, and that no `SecretString` came back.

use std::sync::Arc;

use plaudon_security::error::SecurityError;
use plaudon_security::secrets::{
    os_store, os_store_with_clock, Clock, DeviceId, ScopeError, SecretPresence, SecretRef,
    SecretScope, SecretStore, SecretStoreError, SecretString, StoreKind, SystemClock,
    UnavailableReason, UnsupportedSecretStore, MAX_DEVICE_ID_LEN, SECRET_SERVICE,
};

fn scopes() -> Vec<SecretScope> {
    vec![
        SecretScope::OpenRouterApiKey,
        SecretScope::VoiceProfileKey,
        SecretScope::device("plaud-note-pin-0001").expect("a realistic device id"),
    ]
}

/// A target with no OS vault refuses every operation with the same
/// unavailability, and `status` in particular never answers "absent" — which
/// would invite the caller to re-prompt for a credential that may exist.
#[test]
fn the_unsupported_store_refuses_every_operation_without_returning_a_value() {
    let store = UnsupportedSecretStore::new();
    assert_eq!(store.kind(), StoreKind::Unsupported);
    assert_eq!(UnsupportedSecretStore.kind(), StoreKind::Unsupported);

    let expected = SecretStoreError::Unavailable {
        store: StoreKind::Unsupported,
        reason: UnavailableReason::UnsupportedPlatform,
    };

    for scope in scopes() {
        assert_eq!(
            store
                .store(&scope, &SecretString::new("sk-or-v1-abc"))
                .unwrap_err(),
            expected
        );
        assert_eq!(store.load(&scope).unwrap_err(), expected);
        assert_eq!(store.delete(&scope).unwrap_err(), expected);
        assert_eq!(store.status(&scope).unwrap_err(), expected);
    }

    assert!(
        expected.is_locked_state(),
        "§16.4: an actionable locked state"
    );
    let parts = expected.envelope_parts();
    assert_eq!(parts.code.as_str(), "SECRET_STORE_LOCKED");
    assert!(parts.retryable);
    assert_eq!(
        parts.action.map(plaudon_security::ErrorAction::as_str),
        Some("UNLOCK_CREDENTIAL_STORE")
    );
}

/// Each failure family keeps its own code: only the locked family is
/// retryable-and-actionable, a hard backend failure is neither, and a missing
/// entry sends the user to the key settings instead.
#[test]
fn each_store_failure_family_keeps_its_own_code_and_affordance() {
    let locked = SecretStoreError::Locked {
        store: StoreKind::MacosKeychain,
    };
    assert!(locked.is_locked_state());
    assert_eq!(locked.to_string(), "the macOS Keychain is locked");
    assert_eq!(locked.envelope_parts().code.as_str(), "SECRET_STORE_LOCKED");

    let unavailable = SecretStoreError::Unavailable {
        store: StoreKind::WindowsCredentialManager,
        reason: UnavailableReason::StoreNotAccessible { os_status: -25308 },
    };
    assert!(unavailable.is_locked_state());
    assert_eq!(
        unavailable.to_string(),
        "the Windows Credential Manager is unavailable: \
         the OS credential store refused access (status -25308)"
    );
    assert_eq!(
        unavailable.envelope_parts().code.as_str(),
        "SECRET_STORE_LOCKED",
        "§16.4: an unreachable store is reported as locked, never as a downgrade"
    );

    let not_found = SecretStoreError::NotFound;
    assert!(!not_found.is_locked_state());
    let parts = not_found.envelope_parts();
    assert_eq!(parts.code.as_str(), "SECRET_NOT_FOUND");
    assert_eq!(
        parts.action.map(plaudon_security::ErrorAction::as_str),
        Some("OPEN_API_KEY_SETTINGS")
    );

    let backend = SecretStoreError::backend(StoreKind::MacosKeychain, "SecItemAdd", -25299);
    assert!(!backend.is_locked_state());
    assert_eq!(
        backend.to_string(),
        "credential store operation failed: macOS Keychain SecItemAdd returned status -25299"
    );
    let parts = backend.envelope_parts();
    assert_eq!(parts.code.as_str(), "SECRET_STORE_FAILED");
    assert!(!parts.retryable);
    assert_eq!(parts.action, None);

    let invalid = SecretStoreError::from(ScopeError::Reserved);
    assert!(!invalid.is_locked_state());
    assert_eq!(
        invalid.envelope_parts().code.as_str(),
        "COMMAND_ARGUMENT_INVALID"
    );
    assert_eq!(
        invalid.to_string(),
        "invalid credential scope: device identifier is reserved and would denote a shared credential"
    );
}

/// Every backend label is distinct and contains no path, user name or secret.
#[test]
fn every_store_kind_has_a_distinct_label() {
    let kinds = [
        StoreKind::MacosKeychain,
        StoreKind::WindowsCredentialManager,
        StoreKind::InMemoryTestStore,
        StoreKind::Unsupported,
    ];
    let mut labels = std::collections::BTreeSet::new();
    for kind in kinds {
        assert!(labels.insert(kind.label()));
        assert_eq!(format!("{kind}"), kind.label());
    }
    assert_eq!(labels.len(), kinds.len());

    assert_eq!(
        UnavailableReason::UnsupportedPlatform.to_string(),
        "this platform has no supported OS credential store"
    );
    assert_eq!(
        UnavailableReason::StoreNotAccessible { os_status: 0 }.to_string(),
        "the OS credential store refused access (status 0)"
    );
}

/// Every rejected device identifier names its own rule, and the reserved check
/// runs first so `*` reports "this would be a shared credential".
#[test]
fn every_rejected_device_identifier_names_its_own_rule() {
    assert_eq!(DeviceId::new("").unwrap_err(), ScopeError::Empty);
    assert_eq!(
        DeviceId::new(&"a".repeat(MAX_DEVICE_ID_LEN + 1)).unwrap_err(),
        ScopeError::TooLong
    );
    assert!(
        DeviceId::new(&"a".repeat(MAX_DEVICE_ID_LEN)).is_ok(),
        "the cap is inclusive"
    );

    for reserved in [
        "*",
        "all",
        "any",
        "global",
        "default",
        "shared",
        "universal",
        "none",
        "ALL",
        "Global",
    ] {
        assert_eq!(
            DeviceId::new(reserved).unwrap_err(),
            ScopeError::Reserved,
            "{reserved} would denote a shared credential (§7.7)"
        );
    }

    for illegal in ["../x", "a/b", "a.b", "a b", "a\0b", "émile", "a;b", "a\n"] {
        assert_eq!(
            DeviceId::new(illegal).unwrap_err(),
            ScopeError::IllegalCharacter,
            "{illegal:?} must be refused"
        );
    }

    for legal in [
        "plaud-note-01",
        "AB:CD:EF:01:23:45",
        "device_1",
        "0",
        "a-_:",
    ] {
        let id = DeviceId::new(legal).expect("legal identifier");
        assert_eq!(id.as_str(), legal);
        assert_eq!(format!("{id}"), legal);
        assert_eq!(format!("{id:?}"), format!("DeviceId({legal})"));
    }

    // Every rule maps onto the argument-validation envelope.
    for error in [
        ScopeError::Empty,
        ScopeError::TooLong,
        ScopeError::IllegalCharacter,
        ScopeError::Reserved,
    ] {
        let parts = error.envelope_parts();
        assert_eq!(parts.code.as_str(), "COMMAND_ARGUMENT_INVALID");
        assert_eq!(parts.message, error.to_string());
        assert!(!parts.retryable);
    }
}

/// No two scopes resolve to the same vault entry, and no scope's label or
/// account string can carry a credential.
#[test]
fn no_two_scopes_resolve_to_the_same_vault_entry() {
    let mut accounts = std::collections::BTreeSet::new();
    for scope in scopes() {
        assert_eq!(scope.service(), SECRET_SERVICE);
        assert!(accounts.insert(scope.account()), "{scope:?} collides");
        assert!(!scope.label().is_empty(), "{scope:?} has no label");
    }
    assert_eq!(accounts.len(), 3);

    assert_eq!(
        SecretScope::OpenRouterApiKey.account(),
        "openrouter.api_key"
    );
    assert_eq!(SecretScope::OpenRouterApiKey.device_id(), None);
    assert_eq!(
        SecretScope::VoiceProfileKey.account(),
        "voiceprofiles.data_key"
    );
    assert_eq!(SecretScope::VoiceProfileKey.device_id(), None);

    let device = SecretScope::device("plaud-01").unwrap();
    assert_eq!(device.account(), "device.plaud-01.auth");
    assert_eq!(device.device_id(), Some("plaud-01"));
    assert_eq!(device.label(), "device authorization for plaud-01");

    assert_eq!(
        SecretScope::device("*").unwrap_err(),
        ScopeError::Reserved,
        "a wildcard device scope is not representable"
    );
}

/// A secret compares in constant time over its content, prints redacted, and
/// never round-trips through a `Display` or a serializer.
#[test]
fn a_secret_string_never_renders_its_content() {
    let secret = SecretString::new("sk-or-v1-0123456789abcdef".to_owned());
    assert_eq!(format!("{secret:?}"), "SecretString(<redacted>)");
    assert!(!format!("{secret:?}").contains("sk-or-v1"));
    assert_eq!(secret.len(), 25);
    assert!(!secret.is_empty(), "a 25-byte secret is not empty");
    assert_eq!(secret.expose_bytes(), secret.expose().as_bytes());

    let same = SecretString::new("sk-or-v1-0123456789abcdef");
    let different_tail = SecretString::new("sk-or-v1-0123456789abcdeF");
    let different_head = SecretString::new("Xk-or-v1-0123456789abcdef");
    let shorter = SecretString::new("sk-or-v1");
    assert!(secret.ct_eq(&same));
    assert!(!secret.ct_eq(&different_tail));
    assert!(!secret.ct_eq(&different_head));
    assert!(!secret.ct_eq(&shorter));
    assert!(!shorter.ct_eq(&secret));

    let empty = SecretString::new("");
    assert!(empty.is_empty(), "an empty secret reports itself as empty");
    assert!(empty.ct_eq(&SecretString::new("")));
}

/// Bytes read back from a vault must be UTF-8; anything else means something
/// other than this crate wrote the entry, and it is a backend failure rather
/// than a silently lossy decode.
#[test]
fn vault_bytes_that_are_not_utf8_are_a_backend_failure() {
    let good = SecretString::from_utf8(b"sk-or-v1-abc".to_vec()).expect("valid utf-8");
    assert_eq!(good.expose(), "sk-or-v1-abc");

    let error = SecretString::from_utf8(vec![0xFF, 0xFE, 0x00]).expect_err("not utf-8");
    assert_eq!(
        error,
        SecretStoreError::Backend {
            detail: "stored credential is not valid UTF-8".to_owned()
        }
    );
    assert_eq!(error.envelope_parts().code.as_str(), "SECRET_STORE_FAILED");
}

/// A reference detects rotation without carrying the credential, and a
/// fingerprint computed under one scope never matches another.
#[test]
fn a_reference_detects_rotation_and_is_not_portable_between_scopes() {
    let scope = SecretScope::OpenRouterApiKey;
    let secret = SecretString::new("sk-or-v1-0123456789abcdef");
    let reference = SecretRef::new(&scope, &secret, 1_700_000_000);

    assert_eq!(reference.service, SECRET_SERVICE);
    assert_eq!(reference.account, "openrouter.api_key");
    assert_eq!(reference.created_at_unix, 1_700_000_000);
    assert_eq!(
        reference.fingerprint.len(),
        plaudon_security::secrets::FINGERPRINT_HEX_LEN
    );
    assert!(reference.fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(
        !reference.fingerprint.contains("0123456789abcdef"),
        "the fingerprint must not contain a substring of the secret"
    );

    assert!(reference.matches(&scope, &secret));
    assert!(
        !reference.matches(&scope, &SecretString::new("sk-or-v1-0123456789abcdee")),
        "a rotated key is detected"
    );

    // The same plaintext under another scope fingerprints differently, so a
    // reference cannot be correlated across scopes.
    let other = SecretScope::VoiceProfileKey;
    assert_ne!(
        SecretRef::fingerprint_of(&scope, &secret),
        SecretRef::fingerprint_of(&other, &secret)
    );
    assert!(!reference.matches(&other, &secret));

    // The reference is the only serializable carrier, and it carries no secret.
    let json = serde_json::to_string(&reference).expect("serializable");
    assert!(!json.contains("sk-or-v1"));
    let back: SecretRef = serde_json::from_str(&json).expect("round trip");
    assert_eq!(back, reference);
}

/// Presence is a status, not a value: the absent form carries nothing at all.
#[test]
fn presence_is_a_status_and_never_a_credential() {
    let absent = SecretPresence::absent();
    assert!(!absent.present);
    assert_eq!(absent.reference, None);

    let reference = SecretRef::new(
        &SecretScope::OpenRouterApiKey,
        &SecretString::new("sk-or-v1-abc"),
        1,
    );
    let present = SecretPresence::present(reference.clone());
    assert!(present.present);
    assert_eq!(present.reference, Some(reference));

    let json = serde_json::to_string(&present).expect("serializable");
    assert!(!json.contains("sk-or-v1-abc"));
}

/// The platform selector returns a real backend with no plaintext branch, and
/// takes an injectable clock so nothing in this crate reads the wall clock
/// implicitly.
#[test]
fn the_platform_selector_has_no_plaintext_branch() {
    let expected = if cfg!(target_os = "macos") {
        StoreKind::MacosKeychain
    } else if cfg!(target_os = "windows") {
        StoreKind::WindowsCredentialManager
    } else {
        StoreKind::Unsupported
    };
    assert_eq!(os_store().kind(), expected);

    #[derive(Debug)]
    struct Frozen;
    impl Clock for Frozen {
        fn now_unix(&self) -> i64 {
            42
        }
    }
    assert_eq!(os_store_with_clock(Arc::new(Frozen)).kind(), expected);
    assert_eq!(Frozen.now_unix(), 42);

    // The production clock is monotone with the epoch and never negative.
    assert!(SystemClock.now_unix() > 1_700_000_000);
}

/// The size bounds are enforced before any vault API is reached.
#[cfg(feature = "test-support")]
#[test]
fn an_empty_or_oversized_credential_is_refused_before_the_vault_is_touched() {
    use plaudon_security::secrets::{InMemorySecretStore, MAX_SECRET_BYTES};

    let store = InMemorySecretStore::new();
    let scope = SecretScope::OpenRouterApiKey;

    assert_eq!(
        store.store(&scope, &SecretString::new("")).unwrap_err(),
        SecretStoreError::Backend {
            detail: "refusing to store an empty credential".to_owned()
        }
    );
    assert_eq!(
        store
            .store(&scope, &SecretString::new("a".repeat(MAX_SECRET_BYTES + 1)))
            .unwrap_err(),
        SecretStoreError::Backend {
            detail: format!("credential exceeds {MAX_SECRET_BYTES} bytes")
        }
    );
    assert!(
        store
            .store(&scope, &SecretString::new("a".repeat(MAX_SECRET_BYTES)))
            .is_ok(),
        "the cap is inclusive"
    );
    assert_eq!(store.entry_count(), 1);
    assert_eq!(store.kind(), StoreKind::InMemoryTestStore);
}

/// A locked vault and an unreachable vault both refuse every operation, and in
/// particular `status` does not report "absent" over a credential that exists.
#[cfg(feature = "test-support")]
#[test]
fn a_locked_or_unreachable_vault_never_downgrades_to_a_value() {
    use plaudon_security::secrets::{FixedClock, InMemorySecretStore};

    let store = InMemorySecretStore::with_clock(Arc::new(FixedClock::new(1_762_000_000)));
    let scope = SecretScope::OpenRouterApiKey;
    let reference = store
        .store(&scope, &SecretString::new("sk-or-v1-abc"))
        .expect("stored while the vault is open");
    assert_eq!(reference.created_at_unix, 1_762_000_000);
    assert!(store.status(&scope).unwrap().present);

    store.set_locked(true);
    let locked = SecretStoreError::Locked {
        store: StoreKind::InMemoryTestStore,
    };
    assert_eq!(store.load(&scope).unwrap_err(), locked);
    assert_eq!(store.status(&scope).unwrap_err(), locked);
    assert_eq!(store.delete(&scope).unwrap_err(), locked);
    assert_eq!(
        store
            .store(&scope, &SecretString::new("sk-or-v1-def"))
            .unwrap_err(),
        locked
    );

    // Unavailability is checked before the lock, because it is the stronger
    // statement about the vault.
    store.set_unavailable(true);
    let unavailable = SecretStoreError::Unavailable {
        store: StoreKind::InMemoryTestStore,
        reason: UnavailableReason::StoreNotAccessible { os_status: -1 },
    };
    assert_eq!(store.load(&scope).unwrap_err(), unavailable);
    assert_eq!(store.status(&scope).unwrap_err(), unavailable);

    store.set_unavailable(false);
    store.set_locked(false);
    assert!(store.load(&scope).is_ok(), "the credential survived");
    assert_eq!(store.entry_count(), 1);

    // `Debug` prints counts and flags, never entries.
    let rendered = format!("{store:?}");
    assert!(!rendered.contains("sk-or-v1"), "{rendered}");
    assert!(rendered.contains("entries: 1"), "{rendered}");

    // Deleting a credential that is not there is an error, not a silent success.
    assert!(store.delete(&scope).is_ok());
    assert_eq!(
        store.delete(&scope).unwrap_err(),
        SecretStoreError::NotFound
    );
    assert_eq!(store.load(&scope).unwrap_err(), SecretStoreError::NotFound);
    assert!(!store.status(&scope).unwrap().present);
    assert_eq!(InMemorySecretStore::default().entry_count(), 0);
}
