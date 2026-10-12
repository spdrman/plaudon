//! D1.2 hermetic tests (§18.5 "Security tests", §22.9, §22.16).
//!
//! Nothing here touches a real keychain, the network, the filesystem or a real
//! device. The opt-in real-vault smoke tests at the bottom are `#[ignore]`d
//! unless the `real-keychain` feature is on.

use std::sync::Arc;

use super::macos::{
    map_osstatus, ERR_SEC_AUTH_FAILED, ERR_SEC_INTERACTION_NOT_ALLOWED,
    ERR_SEC_INTERACTION_REQUIRED, ERR_SEC_ITEM_NOT_FOUND, ERR_SEC_MISSING_ENTITLEMENT,
    ERR_SEC_NOT_AVAILABLE, ERR_SEC_NO_DEFAULT_KEYCHAIN, ERR_SEC_NO_SUCH_KEYCHAIN,
    ERR_SEC_USER_CANCELED,
};
use super::windows::{
    credential_target, map_win_error, wide_nul, ERROR_ACCESS_DENIED, ERROR_BAD_USERNAME,
    ERROR_NOT_FOUND, ERROR_NO_SUCH_LOGON_SESSION,
};
use super::{
    DeviceId, FixedClock, InMemorySecretStore, ScopeError, SecretPresence, SecretRef, SecretScope,
    SecretStore, SecretStoreError, SecretString, StoreKind, UnavailableReason,
    UnsupportedSecretStore, FINGERPRINT_HEX_LEN, MAX_DEVICE_ID_LEN, MAX_SECRET_BYTES,
    SECRET_SERVICE,
};
use crate::error::{
    SecurityError, OPEN_API_KEY_SETTINGS, SECRET_NOT_FOUND, SECRET_STORE_FAILED,
    SECRET_STORE_LOCKED, UNLOCK_CREDENTIAL_STORE,
};

/// A realistic OpenRouter key shape (`sk-or-v1-` + 64 hex).
const OPENROUTER_SHAPED_KEY: &str =
    "sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
/// A realistic 32-hex device authorization token.
const DEVICE_TOKEN_HEX: &str = "9f2c4ab7e51d3860af7b21cc90de45f1";

fn store_with_fixed_clock() -> InMemorySecretStore {
    InMemorySecretStore::with_clock(Arc::new(FixedClock::new(1_762_000_000)))
}

fn device_scope(id: &str) -> SecretScope {
    SecretScope::device(id).expect("test device id must be valid")
}

// ---------------------------------------------------------------------------
// Round trip
// ---------------------------------------------------------------------------

#[test]
fn round_trip_store_load_delete() {
    let store = store_with_fixed_clock();
    let scope = SecretScope::OpenRouterApiKey;
    let secret = SecretString::new(OPENROUTER_SHAPED_KEY);

    let reference = store.store(&scope, &secret).unwrap();
    assert_eq!(reference.service, SECRET_SERVICE);
    assert_eq!(reference.account, "openrouter.api_key");
    assert_eq!(reference.created_at_unix, 1_762_000_000);

    let loaded = store.load(&scope).unwrap();
    assert!(loaded.ct_eq(&secret));
    assert_eq!(loaded.len(), OPENROUTER_SHAPED_KEY.len());

    let presence = store.status(&scope).unwrap();
    assert_eq!(presence, SecretPresence::present(reference.clone()));

    store.delete(&scope).unwrap();
    assert_eq!(store.entry_count(), 0);
    assert_eq!(store.status(&scope).unwrap(), SecretPresence::absent());
}

#[test]
fn load_after_delete_is_not_found() {
    let store = store_with_fixed_clock();
    let scope = SecretScope::OpenRouterApiKey;
    store
        .store(&scope, &SecretString::new(OPENROUTER_SHAPED_KEY))
        .unwrap();
    store.delete(&scope).unwrap();

    assert!(matches!(
        store.load(&scope),
        Err(SecretStoreError::NotFound)
    ));
}

#[test]
fn delete_of_absent_scope_is_not_found_not_silent_success() {
    let store = store_with_fixed_clock();
    assert!(matches!(
        store.delete(&SecretScope::OpenRouterApiKey),
        Err(SecretStoreError::NotFound)
    ));
}

#[test]
fn storing_again_rotates_in_place() {
    let store = store_with_fixed_clock();
    let scope = SecretScope::OpenRouterApiKey;
    let first = store
        .store(&scope, &SecretString::new(OPENROUTER_SHAPED_KEY))
        .unwrap();
    let rotated = store
        .store(&scope, &SecretString::new("sk-or-v1-rotated-key-value"))
        .unwrap();

    assert_eq!(store.entry_count(), 1);
    assert_ne!(first.fingerprint, rotated.fingerprint);
}

#[test]
fn empty_and_oversized_secrets_are_refused() {
    let store = store_with_fixed_clock();
    let scope = SecretScope::OpenRouterApiKey;

    let empty = store.store(&scope, &SecretString::new(String::new()));
    assert!(matches!(empty, Err(SecretStoreError::Backend { .. })));

    let huge = SecretString::new("k".repeat(MAX_SECRET_BYTES + 1));
    assert!(matches!(
        store.store(&scope, &huge),
        Err(SecretStoreError::Backend { .. })
    ));
    assert_eq!(store.entry_count(), 0);
}

#[test]
fn secret_string_from_utf8_rejects_non_utf8_vault_bytes() {
    let result = SecretString::from_utf8(vec![0xff, 0xfe, 0x00]);
    assert!(matches!(result, Err(SecretStoreError::Backend { .. })));

    let ok = SecretString::from_utf8(OPENROUTER_SHAPED_KEY.as_bytes().to_vec()).unwrap();
    assert_eq!(ok.expose(), OPENROUTER_SHAPED_KEY);
}

// ---------------------------------------------------------------------------
// Locked / unavailable: never a fallback value
// ---------------------------------------------------------------------------

#[test]
fn locked_store_denies_every_operation_without_returning_a_value() {
    let store = store_with_fixed_clock();
    let scope = SecretScope::OpenRouterApiKey;
    let secret = SecretString::new(OPENROUTER_SHAPED_KEY);
    store.store(&scope, &secret).unwrap();

    store.set_locked(true);

    let load = store.load(&scope);
    assert!(matches!(
        load,
        Err(SecretStoreError::Locked {
            store: StoreKind::InMemoryTestStore
        })
    ));
    assert!(
        load.is_err(),
        "a locked vault must never yield a credential"
    );
    assert!(matches!(
        store.store(&scope, &secret),
        Err(SecretStoreError::Locked { .. })
    ));
    assert!(matches!(
        store.delete(&scope),
        Err(SecretStoreError::Locked { .. })
    ));
    assert!(matches!(
        store.status(&scope),
        Err(SecretStoreError::Locked { .. })
    ));

    let parts = SecretStoreError::Locked {
        store: StoreKind::MacosKeychain,
    }
    .envelope_parts();
    assert_eq!(parts.code, SECRET_STORE_LOCKED);
    assert_eq!(parts.action, Some(UNLOCK_CREDENTIAL_STORE));
    assert!(parts.retryable);
}

#[test]
fn unavailable_store_reports_locked_state_never_a_plaintext_downgrade() {
    let store = store_with_fixed_clock();
    let scope = device_scope("plaud-note-0001");
    store.set_unavailable(true);

    let load = store.load(&scope);
    assert!(matches!(load, Err(SecretStoreError::Unavailable { .. })));
    let err = load.unwrap_err();
    assert!(err.is_locked_state());

    let parts = err.envelope_parts();
    assert_eq!(
        parts.code, SECRET_STORE_LOCKED,
        "§16.4: actionable locked state, not a downgrade"
    );
    assert_eq!(parts.action, Some(UNLOCK_CREDENTIAL_STORE));
    assert!(parts.retryable);
}

#[test]
fn unsupported_platform_store_is_unavailable_and_never_returns_a_value() {
    let store = UnsupportedSecretStore::new();
    let scope = SecretScope::OpenRouterApiKey;
    let secret = SecretString::new(OPENROUTER_SHAPED_KEY);

    assert_eq!(store.kind(), StoreKind::Unsupported);
    for err in [
        store.store(&scope, &secret).err(),
        store.load(&scope).err(),
        store.delete(&scope).err(),
        store.status(&scope).err(),
    ] {
        let err = err.expect("every operation on an unsupported platform must fail");
        assert_eq!(
            err,
            SecretStoreError::Unavailable {
                store: StoreKind::Unsupported,
                reason: UnavailableReason::UnsupportedPlatform,
            }
        );
        assert_eq!(err.envelope_parts().code, SECRET_STORE_LOCKED);
    }
}

#[test]
fn not_found_and_backend_envelopes_use_their_own_codes() {
    let not_found = SecretStoreError::NotFound.envelope_parts();
    assert_eq!(not_found.code, SECRET_NOT_FOUND);
    assert_eq!(not_found.action, Some(OPEN_API_KEY_SETTINGS));
    assert!(!not_found.retryable);

    let backend = SecretStoreError::backend(StoreKind::MacosKeychain, "keychain write", -25_240);
    assert_eq!(backend.envelope_parts().code, SECRET_STORE_FAILED);
    assert!(!backend.is_locked_state());
}

// ---------------------------------------------------------------------------
// Per-device scoping (§7.7, §16.4: no universal token)
// ---------------------------------------------------------------------------

#[test]
fn device_secret_is_not_loadable_through_another_device_scope() {
    let store = store_with_fixed_clock();
    let device_a = device_scope("plaud-note-AAAA");
    let device_b = device_scope("plaud-note-BBBB");

    let secret = SecretString::new(DEVICE_TOKEN_HEX);
    store.store(&device_a, &secret).unwrap();

    assert!(matches!(
        store.load(&device_b),
        Err(SecretStoreError::NotFound)
    ));
    assert_eq!(store.status(&device_b).unwrap(), SecretPresence::absent());
    assert!(store.load(&device_a).unwrap().ct_eq(&secret));

    // And an OpenRouter key is not reachable through a device scope either.
    assert!(matches!(
        store.load(&SecretScope::OpenRouterApiKey),
        Err(SecretStoreError::NotFound)
    ));
}

#[test]
fn device_scope_rejects_empty_wildcard_reserved_and_illegal_identifiers() {
    assert_eq!(SecretScope::device("").unwrap_err(), ScopeError::Empty);
    for reserved in [
        "*",
        "all",
        "ALL",
        "any",
        "global",
        "default",
        "shared",
        "universal",
        "none",
    ] {
        assert_eq!(
            SecretScope::device(reserved).unwrap_err(),
            ScopeError::Reserved,
            "{reserved} must not denote a shared credential"
        );
    }
    for illegal in [
        "../x",
        "a/b",
        "a.b",
        "a b",
        "a\0b",
        "a\nb",
        "dev*",
        "d'; DROP TABLE--",
        "é",
    ] {
        assert_eq!(
            SecretScope::device(illegal).unwrap_err(),
            ScopeError::IllegalCharacter,
            "{illegal:?} must be rejected"
        );
    }
}

#[test]
fn device_scope_rejects_overlong_identifier() {
    let overlong = "a".repeat(MAX_DEVICE_ID_LEN + 1);
    assert_eq!(
        SecretScope::device(&overlong).unwrap_err(),
        ScopeError::TooLong
    );
    assert!(SecretScope::device(&"a".repeat(MAX_DEVICE_ID_LEN)).is_ok());
}

#[test]
fn device_scope_accepts_realistic_identities() {
    for id in ["plaud-note-0001", "AA:BB:CC:DD:EE:FF", "serial_12345", "a"] {
        let scope = SecretScope::device(id).unwrap();
        assert_eq!(scope.device_id(), Some(id));
        assert_eq!(scope.account(), format!("device.{id}.auth"));
    }
}

#[test]
fn scope_accounts_are_namespaced_and_distinct() {
    let openrouter = SecretScope::OpenRouterApiKey;
    let device = device_scope("plaud-note-0001");

    assert_eq!(openrouter.service(), SECRET_SERVICE);
    assert_eq!(device.service(), SECRET_SERVICE);
    assert_eq!(openrouter.account(), "openrouter.api_key");
    assert_ne!(openrouter.account(), device.account());
    assert_eq!(openrouter.device_id(), None);
    assert!(openrouter.label().contains("OpenRouter"));
    assert!(device.label().contains("plaud-note-0001"));
}

#[test]
fn device_id_validation_is_the_only_constructor() {
    assert!(DeviceId::new("plaud-note-0001").is_ok());
    assert!(DeviceId::new("*").is_err());
    // `DeviceId`'s field is private, so `SecretScope::DeviceAuthorization`
    // cannot be built with an unvalidated identifier anywhere in the program.
    // A universal device token is therefore unrepresentable, not merely
    // rejected at the boundary (§16.4).
    let id = DeviceId::new("plaud-note-0001").unwrap();
    let scope = SecretScope::DeviceAuthorization { device_id: id };
    assert_eq!(scope.device_id(), Some("plaud-note-0001"));
}

// ---------------------------------------------------------------------------
// Non-disclosure (§6.3A, §16.2 "redacted logs")
// ---------------------------------------------------------------------------

#[test]
fn secret_string_debug_is_redacted() {
    // A struct that derives Debug and contains a SecretString stays safe.
    #[derive(Debug)]
    struct Holder {
        // Never read at runtime; exists only so the derived `Debug` impl
        // above is compile-checked to redact `SecretString` fields.
        #[allow(dead_code)]
        label: &'static str,
        #[allow(dead_code)]
        secret: SecretString,
    }

    let secret = SecretString::new(OPENROUTER_SHAPED_KEY);
    let rendered = format!("{secret:?}");
    assert_eq!(rendered, "SecretString(<redacted>)");
    assert!(!rendered.contains(OPENROUTER_SHAPED_KEY));
    assert!(!rendered.contains("sk-or-v1-"));

    let holder = Holder {
        label: "openrouter",
        secret,
    };
    let rendered = format!("{holder:?}");
    assert!(rendered.contains("openrouter"));
    assert!(!rendered.contains(OPENROUTER_SHAPED_KEY));
}

#[test]
fn secret_store_error_debug_and_display_never_contain_secret_material() {
    let errors = [
        SecretStoreError::Locked {
            store: StoreKind::MacosKeychain,
        },
        SecretStoreError::Unavailable {
            store: StoreKind::WindowsCredentialManager,
            reason: UnavailableReason::StoreNotAccessible { os_status: 1312 },
        },
        SecretStoreError::NotFound,
        SecretStoreError::backend(StoreKind::MacosKeychain, "keychain read", -25_308),
        SecretStoreError::InvalidScope(ScopeError::Reserved),
    ];
    for err in &errors {
        for rendered in [
            format!("{err:?}"),
            format!("{err}"),
            err.envelope_parts().message,
        ] {
            assert!(!rendered.contains(OPENROUTER_SHAPED_KEY), "{rendered}");
            assert!(!rendered.contains(DEVICE_TOKEN_HEX), "{rendered}");
            assert!(!rendered.contains("sk-or-v1-"), "{rendered}");
        }
    }
}

#[test]
fn invalid_scope_error_maps_through_to_an_argument_error() {
    let err = SecretStoreError::from(ScopeError::IllegalCharacter);
    assert_eq!(
        err.envelope_parts().code,
        crate::error::COMMAND_ARGUMENT_INVALID
    );
}

#[test]
fn ct_eq_agrees_with_equality() {
    let a = SecretString::new(OPENROUTER_SHAPED_KEY);
    let b = SecretString::new(OPENROUTER_SHAPED_KEY);
    let c = SecretString::new(format!("{OPENROUTER_SHAPED_KEY}x"));
    let d = SecretString::new(OPENROUTER_SHAPED_KEY.replace("sk-or-v1-0", "sk-or-v1-1"));

    assert!(a.ct_eq(&b));
    assert!(!a.ct_eq(&c), "different length must not compare equal");
    assert!(
        !a.ct_eq(&d),
        "same length, different content must not compare equal"
    );
    assert!(SecretString::new(String::new()).ct_eq(&SecretString::new(String::new())));
    assert_eq!(a.is_empty(), a.expose().is_empty());
}

// ---------------------------------------------------------------------------
// Fingerprints: rotation detection without disclosure
// ---------------------------------------------------------------------------

#[test]
fn fingerprint_is_fixed_length_hex_and_reveals_nothing() {
    let scope = SecretScope::OpenRouterApiKey;
    let secret = SecretString::new(OPENROUTER_SHAPED_KEY);
    let fingerprint = SecretRef::fingerprint_of(&scope, &secret);

    assert_eq!(fingerprint.len(), FINGERPRINT_HEX_LEN);
    assert!(fingerprint
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

    // No substring of the secret survives into the digest, in either direction.
    assert!(!fingerprint.contains(OPENROUTER_SHAPED_KEY));
    assert!(!OPENROUTER_SHAPED_KEY.contains(&fingerprint));
    for window_len in [4, 8, 16] {
        let secret_bytes = OPENROUTER_SHAPED_KEY.as_bytes();
        for window in secret_bytes.windows(window_len) {
            let text = std::str::from_utf8(window).unwrap();
            assert!(
                !fingerprint.contains(text),
                "fingerprint leaked a {window_len}-byte run of the secret"
            );
        }
    }

    // The digest is independent of the secret's length, so it cannot be used
    // to narrow a guess by size.
    let short = SecretRef::fingerprint_of(&scope, &SecretString::new("x"));
    assert_eq!(short.len(), fingerprint.len());
}

#[test]
fn fingerprint_is_scope_bound_and_detects_rotation() {
    let secret = SecretString::new(DEVICE_TOKEN_HEX);
    let device_a = device_scope("plaud-note-AAAA");
    let device_b = device_scope("plaud-note-BBBB");

    // Same secret, different scope ⇒ different fingerprint: a reference cannot
    // be correlated across devices to show that two devices share a token.
    assert_ne!(
        SecretRef::fingerprint_of(&device_a, &secret),
        SecretRef::fingerprint_of(&device_b, &secret)
    );
    // Deterministic within a scope ⇒ usable as a rotation detector.
    assert_eq!(
        SecretRef::fingerprint_of(&device_a, &secret),
        SecretRef::fingerprint_of(&device_a, &SecretString::new(DEVICE_TOKEN_HEX))
    );

    let reference = SecretRef::new(&device_a, &secret, 1_762_000_000);
    assert!(reference.matches(&device_a, &secret));
    assert!(!reference.matches(
        &device_a,
        &SecretString::new("9f2c4ab7e51d3860af7b21cc90de45f2")
    ));
    assert!(!reference.matches(&device_b, &secret));
}

#[test]
fn secret_ref_json_is_the_only_serializable_carrier_and_leaks_nothing() {
    let store = store_with_fixed_clock();
    let scope = device_scope("plaud-note-0001");
    let secret = SecretString::new(DEVICE_TOKEN_HEX);
    let reference = store.store(&scope, &secret).unwrap();

    let json = serde_json::to_string(&reference).unwrap();
    assert!(!json.contains(DEVICE_TOKEN_HEX));
    assert!(!json.contains(OPENROUTER_SHAPED_KEY));
    assert!(json.contains("ai.plaudon.plaudon"));
    assert!(json.contains("device.plaud-note-0001.auth"));

    let presence_json = serde_json::to_string(&store.status(&scope).unwrap()).unwrap();
    assert!(!presence_json.contains(DEVICE_TOKEN_HEX));
    assert!(presence_json.contains("\"present\":true"));

    // Round trip: the persisted form is self-describing and contains no secret.
    let restored: SecretRef = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, reference);

    // There is no `Serialize` for `SecretString`, so a struct holding one
    // cannot be derived into a serializable record; `SecretRef` is the only
    // carrier that crosses the persistence boundary (§13.1, App. B.7).
    assert_eq!(
        std::mem::size_of_val(&secret),
        std::mem::size_of::<String>()
    );
}

// ---------------------------------------------------------------------------
// Platform error mapping (pure, runs on every target)
// ---------------------------------------------------------------------------

#[test]
fn macos_osstatus_maps_locked_states_to_locked_and_missing_items_to_not_found() {
    assert_eq!(
        map_osstatus("keychain read", ERR_SEC_ITEM_NOT_FOUND),
        SecretStoreError::NotFound
    );

    for status in [
        ERR_SEC_INTERACTION_NOT_ALLOWED,
        ERR_SEC_INTERACTION_REQUIRED,
        ERR_SEC_USER_CANCELED,
        ERR_SEC_AUTH_FAILED,
    ] {
        assert_eq!(
            map_osstatus("keychain read", status),
            SecretStoreError::Locked {
                store: StoreKind::MacosKeychain
            },
            "OSStatus {status} must be an actionable locked state"
        );
    }

    for status in [
        ERR_SEC_NOT_AVAILABLE,
        ERR_SEC_NO_SUCH_KEYCHAIN,
        ERR_SEC_NO_DEFAULT_KEYCHAIN,
        ERR_SEC_MISSING_ENTITLEMENT,
    ] {
        let err = map_osstatus("keychain read", status);
        assert_eq!(
            err,
            SecretStoreError::Unavailable {
                store: StoreKind::MacosKeychain,
                reason: UnavailableReason::StoreNotAccessible { os_status: status },
            }
        );
        assert_eq!(err.envelope_parts().code, SECRET_STORE_LOCKED);
    }

    let other = map_osstatus("keychain write", -25_299);
    assert!(matches!(other, SecretStoreError::Backend { .. }));
    assert_eq!(other.envelope_parts().code, SECRET_STORE_FAILED);
}

#[test]
fn windows_error_codes_map_to_the_same_taxonomy() {
    assert_eq!(
        map_win_error("credential read", ERROR_NOT_FOUND),
        SecretStoreError::NotFound
    );
    assert_eq!(
        map_win_error("credential read", ERROR_ACCESS_DENIED),
        SecretStoreError::Locked {
            store: StoreKind::WindowsCredentialManager
        }
    );
    let unavailable = map_win_error("credential read", ERROR_NO_SUCH_LOGON_SESSION);
    assert_eq!(unavailable.envelope_parts().code, SECRET_STORE_LOCKED);
    assert!(unavailable.is_locked_state());

    let backend = map_win_error("credential write", ERROR_BAD_USERNAME);
    assert_eq!(backend.envelope_parts().code, SECRET_STORE_FAILED);
}

#[test]
fn windows_target_name_and_utf16_encoding_are_namespaced_and_terminated() {
    let scope = device_scope("plaud-note-0001");
    let target = credential_target(&scope);
    assert_eq!(target, "ai.plaudon.plaudon:device.plaud-note-0001.auth");

    let wide = wide_nul(&target);
    assert_eq!(wide.len(), target.chars().count() + 1);
    assert_eq!(wide.last().copied(), Some(0));
    assert_eq!(String::from_utf16(&wide[..wide.len() - 1]).unwrap(), target);

    assert_ne!(credential_target(&SecretScope::OpenRouterApiKey), target);
}

// ---------------------------------------------------------------------------
// Subprocess contract (§16.4: never in command-line args or subprocess env)
// ---------------------------------------------------------------------------

#[test]
fn scrubbed_env_carries_no_secret_name_or_value() {
    use crate::outbound::subprocess::{is_stripped_env, scrubbed_env};

    // Names that could plausibly carry one of our credentials are stripped.
    for name in [
        "OPENROUTER_API_KEY",
        "PLAUDON_SECRET_OPENROUTER",
        "PLAUD_DEVICE_TOKEN",
        "PLAUDON_DEVICE_SECRET",
        "SOME_PASSWORD",
    ] {
        assert!(
            is_stripped_env(name),
            "{name} must never reach a child process"
        );
    }

    // Storing credentials does not put them into this process's environment,
    // so nothing a child inherits can carry one.
    let store = store_with_fixed_clock();
    store
        .store(
            &SecretScope::OpenRouterApiKey,
            &SecretString::new(OPENROUTER_SHAPED_KEY),
        )
        .unwrap();
    store
        .store(
            &device_scope("plaud-note-0001"),
            &SecretString::new(DEVICE_TOKEN_HEX),
        )
        .unwrap();

    for (name, value) in scrubbed_env() {
        assert!(!is_stripped_env(&name), "{name} survived scrubbing");
        assert_ne!(
            value, OPENROUTER_SHAPED_KEY,
            "{name} carries the OpenRouter key"
        );
        assert_ne!(value, DEVICE_TOKEN_HEX, "{name} carries the device token");
        assert!(
            !value.contains("sk-or-v1-"),
            "{name} carries an OpenRouter-shaped value"
        );
    }
}

/// A secret is not representable as a CLI argument.
///
/// Two independent proofs:
///
/// 1. the exhaustive match below fails to compile if a `CliArg` variant is ever
///    added, forcing a reviewer to consider whether the new variant could carry
///    credential material;
/// 2. `SecretString` does not implement `Into<CliArg>`, checked at compile time
///    through inherent-method specialization — an added `From<SecretString>`
///    impl would flip `REPRESENTABLE` and fail the assertion.
#[test]
fn secrets_are_not_cli_argument_representable() {
    use std::marker::PhantomData;

    use crate::outbound::subprocess::CliArg;

    fn variant_name(arg: &CliArg) -> &'static str {
        match arg {
            CliArg::Literal(_) => "literal",
            CliArg::Identifier(_) => "identifier",
            CliArg::Path(_) => "path",
            CliArg::Number(_) => "number",
        }
    }

    struct Probe<T>(PhantomData<T>);
    trait NotRepresentable {
        // Exists to be compile-time flipped by an added `From<SecretString>`
        // impl below, not to be read at runtime.
        #[allow(dead_code)]
        const REPRESENTABLE: bool = false;
        fn representable(&self) -> bool {
            false
        }
    }
    impl<T> NotRepresentable for Probe<T> {}
    impl<T: Into<CliArg>> Probe<T> {
        // Trait-shaped probe: the presence of this impl, not `self`, is the
        // compile-time proof that `T` converts into a `CliArg`.
        #[allow(clippy::unused_self)]
        fn representable(&self) -> bool {
            true
        }
    }

    assert_eq!(variant_name(&CliArg::Literal("list")), "literal");
    assert_eq!(variant_name(&CliArg::Number(1)), "number");
    assert!(
        !Probe::<SecretString>(PhantomData).representable(),
        "SecretString must never be convertible into a CliArg"
    );
    assert!(
        Probe::<CliArg>(PhantomData).representable(),
        "the probe itself must be able to detect a convertible type"
    );
}

#[test]
fn gated_command_argv_and_env_carry_no_secret() {
    use std::path::PathBuf;
    use std::sync::Arc as StdArc;

    use crate::outbound::subprocess::{CliArg, GatedProcessLauncher};
    use crate::outbound::{ApprovalSource, EgressPurpose, NetworkGrant, OutboundPolicy};

    let policy = OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        1_762_000_000,
        "disclosure-v1",
    ));
    let launcher = GatedProcessLauncher::new(StdArc::new(policy));
    let command = launcher
        .prepare_cloud_cli(
            &PathBuf::from("/usr/local/bin/plaud"),
            &[
                CliArg::Literal("list"),
                CliArg::Identifier("rec_0001".to_owned()),
            ],
            EgressPurpose::AccountSync,
        )
        .unwrap();

    for arg in command.args() {
        let rendered = arg.to_string_lossy().into_owned();
        assert!(!rendered.contains(OPENROUTER_SHAPED_KEY));
        assert!(!rendered.contains(DEVICE_TOKEN_HEX));
        assert!(!rendered.contains("sk-or-v1-"));
    }
    for (name, value) in command.env() {
        assert_ne!(value, OPENROUTER_SHAPED_KEY, "{name}");
        assert_ne!(value, DEVICE_TOKEN_HEX, "{name}");
    }
}

// ---------------------------------------------------------------------------
// Opt-in real-vault smoke tests (never run in default CI)
// ---------------------------------------------------------------------------

/// Round trip against the machine's real credential vault.
///
/// Gated on the `real-keychain` feature and `#[ignore]`d without it, because it
/// mutates the developer's keychain / Credential Manager and may raise an
/// unlock prompt. It uses a randomly-named device scope and deletes it again.
#[test]
#[cfg_attr(
    not(feature = "real-keychain"),
    ignore = "needs --features real-keychain"
)]
fn real_os_store_round_trip() {
    use super::os_store;

    if !cfg!(any(target_os = "macos", target_os = "windows")) {
        let store = os_store();
        assert_eq!(store.kind(), StoreKind::Unsupported);
        return;
    }

    // A unique scope so concurrent runs and leftovers cannot collide.
    let nonce = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };
    let scope = SecretScope::device(&format!("selftest-{nonce}")).unwrap();
    let secret = SecretString::new(format!("plaudon-selftest-{nonce}"));

    let store = os_store();
    assert_eq!(store.status(&scope).unwrap(), SecretPresence::absent());

    let reference = store.store(&scope, &secret).unwrap();
    assert_eq!(reference.account, scope.account());

    let loaded = store.load(&scope).unwrap();
    assert!(loaded.ct_eq(&secret));

    let presence = store.status(&scope).unwrap();
    assert!(presence.present);
    assert_eq!(
        presence.reference.map(|r| r.fingerprint),
        Some(SecretRef::fingerprint_of(&scope, &secret))
    );

    store.delete(&scope).unwrap();
    assert!(matches!(
        store.load(&scope),
        Err(SecretStoreError::NotFound)
    ));
    assert_eq!(store.status(&scope).unwrap(), SecretPresence::absent());
}

/// The real vault must not hand back a credential stored under a different
/// device identity (§7.7), checked against the actual OS store.
#[test]
#[cfg_attr(
    not(feature = "real-keychain"),
    ignore = "needs --features real-keychain"
)]
fn real_os_store_is_per_device_scoped() {
    use super::os_store;

    if !cfg!(any(target_os = "macos", target_os = "windows")) {
        return;
    }

    let nonce = format!("{:08x}", std::process::id());
    let device_a = SecretScope::device(&format!("selftest-a-{nonce}")).unwrap();
    let device_b = SecretScope::device(&format!("selftest-b-{nonce}")).unwrap();
    let store = os_store();

    store
        .store(&device_a, &SecretString::new(DEVICE_TOKEN_HEX))
        .unwrap();
    let cross = store.load(&device_b);
    let cleanup = store.delete(&device_a);

    assert!(matches!(cross, Err(SecretStoreError::NotFound)));
    cleanup.unwrap();
}
