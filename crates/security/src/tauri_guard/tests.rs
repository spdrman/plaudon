//! D2.1 unit tests: the manifest is the §15.2 table, authorization fails
//! closed, arguments and responses are validated in Rust, and the source/config
//! scanners the auditor depends on are correct.

use std::collections::BTreeSet;

use serde_json::json;

use super::allowlist::{glob_matches, CommandAllowlist, ManifestError, NamePattern, WindowLabel};
use super::audit::{
    is_local_custom_protocol_origin, is_remote_origin, parse_csp, parse_generate_handler,
};
use super::response::{assert_no_secret_in_response, secret_shape_of, SecretShape};
use super::validate::{
    reject_raw_path, validate_bounded_string, validate_cursor, validate_enum, validate_identifier,
    ApprovedPathHandle, ArgumentError, SelectionOrigin,
};
use crate::error::{SecurityError, COMMAND_ARGUMENT_INVALID, COMMAND_NOT_AUTHORIZED};

/// The §15.2 command table, transcribed from the specification. If the
/// specification table changes, this list changes with it — and the manifest
/// must follow.
const SPEC_15_2_COMMANDS: &[&str] = &[
    "get_capabilities",
    "list_recordings",
    "import_files",
    "list_nearby_devices",
    "device_probe",
    "device_authorize",
    "device_list_recordings",
    "device_import",
    "device_disconnect",
    "device_sync_policy",
    "set_local_only_mode",
    "plaud_probe",
    "plaud_connect",
    "plaud_list",
    "plaud_import",
    "source_sync",
    "source_add_folder",
    "source_reconcile",
    "list_source_artifacts",
    "get_recording",
    "get_transcript",
    "save_transcript_revision",
    "list_models",
    "start_processing",
    "list_jobs",
    "cancel_job",
    "retry_job",
    "set_openrouter_key",
    "remove_openrouter_key",
    "grant_cloud_consent",
    "revoke_cloud_consent",
    "export_recording",
    "get_usage",
    "list_transcript_tools",
    "run_transcript_tool",
    "list_recording_conversation",
    "create_portable_backup",
    "inspect_restore_archive",
    "restore_portable_archive",
    "set_notification_settings",
];

fn allowlist() -> &'static CommandAllowlist {
    CommandAllowlist::load_default().expect("embedded manifest must be valid")
}

fn main_window() -> WindowLabel {
    WindowLabel::new("main")
}

// ---------------------------------------------------------------------------
// Manifest ↔ §15.2
// ---------------------------------------------------------------------------

/// The manifest must cover the whole §15.2 table, and anything beyond it must
/// be deliberate: a command that is not in the spec carries an ADR reference in
/// its `spec` field and a `note` explaining why it exists and what it returns.
///
/// This is what stops the allowlist becoming a dumping ground. A new command
/// cannot be added silently — it either matches the spec table or it names the
/// decision record that introduced it.
#[test]
fn the_manifest_covers_the_spec_table_and_documents_every_addition() {
    let names: BTreeSet<&str> = allowlist().command_names().into_iter().collect();
    let spec: BTreeSet<&str> = SPEC_15_2_COMMANDS.iter().copied().collect();

    let missing: Vec<_> = spec.difference(&names).copied().collect();
    assert!(
        missing.is_empty(),
        "§15.2 commands absent from the manifest: {missing:?}"
    );

    for command in allowlist().commands() {
        if spec.contains(command.name.as_str()) {
            assert_eq!(
                command.spec, "§15.2",
                "{} is in the spec table and must cite §15.2",
                command.name
            );
            continue;
        }
        assert!(
            command.spec.starts_with("ADR-"),
            "{} is not in §15.2, so its `spec` must name the ADR that introduced it (got {:?})",
            command.name,
            command.spec
        );
        let note = command.note.as_deref().unwrap_or_default();
        assert!(
            note.len() >= 40,
            "{} is not in §15.2, so it must carry a substantive `note` explaining why it exists",
            command.name
        );
    }

    assert_eq!(allowlist().version(), 1);
}

#[test]
fn no_allowlisted_command_may_return_a_secret() {
    for command in allowlist().commands() {
        assert!(
            !command.returns_secret,
            "{} declares returns_secret",
            command.name
        );
        assert_ne!(
            command.spec.trim(),
            "",
            "{} has no spec reference",
            command.name
        );
    }
}

#[test]
fn no_allowlisted_command_name_matches_a_forbidden_pattern() {
    for command in allowlist().commands() {
        assert!(
            allowlist().forbidden_name_match(&command.name).is_none(),
            "{} matches a forbidden pattern",
            command.name
        );
    }
}

#[test]
fn the_spec_table_contains_no_generic_shell_http_or_path_command() {
    for name in SPEC_15_2_COMMANDS {
        assert!(
            allowlist().forbidden_name_match(name).is_none(),
            "§15.2 command `{name}` is a generic escape hatch"
        );
    }
}

#[test]
fn every_command_declares_the_main_window() {
    for command in allowlist().commands() {
        assert!(
            command.permits_window(&main_window()),
            "{} excludes `main`",
            command.name
        );
    }
    assert_eq!(allowlist().known_windows(), vec![&main_window()]);
}

#[test]
fn forbidden_permissions_cover_the_shell_http_and_filesystem_plugins() {
    for permission in [
        "shell:allow-execute",
        "shell:allow-open",
        "http:default",
        "fs:allow-read-file",
    ] {
        assert!(
            allowlist().forbidden_permission_match(permission).is_some(),
            "{permission} must be forbidden"
        );
    }
    assert!(allowlist()
        .forbidden_permission_match("core:event:allow-listen")
        .is_none());
}

// ---------------------------------------------------------------------------
// Manifest validation fails closed
// ---------------------------------------------------------------------------

fn minimal_manifest(commands: &str) -> String {
    format!(
        r#"{{ "version": 1,
             "commands": [{commands}],
             "forbidden_permissions": ["shell:*"],
             "required_csp": {{ "default-src": ["'self'"] }},
             "forbidden_command_patterns": ["^exec"],
             "release_requirements": {{ "devtools": false, "with_global_tauri": false,
                                        "remote_window_urls": false, "dangerous_keys": false }} }}"#
    )
}

fn command_entry(name: &str, returns_secret: bool) -> String {
    format!(
        r#"{{ "name": "{name}", "windows": ["main"], "returns_secret": {returns_secret}, "spec": "§15.2" }}"#
    )
}

#[test]
fn an_unparseable_manifest_authorizes_zero_commands() {
    let broken = CommandAllowlist::from_json("{ this is not json");
    assert!(matches!(broken, Err(ManifestError::Malformed { .. })));
    let authorized = broken.map_or(0, |a| a.commands().len());
    assert_eq!(
        authorized, 0,
        "a broken manifest must authorize nothing, not everything"
    );
}

#[test]
fn manifest_rejects_duplicate_commands() {
    let json = minimal_manifest(&format!(
        "{}, {}",
        command_entry("list_jobs", false),
        command_entry("list_jobs", false)
    ));
    assert!(matches!(
        CommandAllowlist::from_json(&json),
        Err(ManifestError::DuplicateCommand { .. })
    ));
}

#[test]
fn manifest_rejects_a_secret_returning_command() {
    let json = minimal_manifest(&command_entry("get_recording", true));
    assert!(matches!(
        CommandAllowlist::from_json(&json),
        Err(ManifestError::SecretReturningCommand { .. })
    ));
}

#[test]
fn manifest_rejects_a_forbidden_command_name() {
    let json = minimal_manifest(&command_entry("exec_shell", false));
    assert!(matches!(
        CommandAllowlist::from_json(&json),
        Err(ManifestError::ForbiddenCommandName { .. })
    ));
}

#[test]
fn manifest_rejects_an_unknown_schema_version() {
    let json = minimal_manifest(&command_entry("list_jobs", false))
        .replace("\"version\": 1", "\"version\": 7");
    assert!(matches!(
        CommandAllowlist::from_json(&json),
        Err(ManifestError::UnsupportedVersion { found: 7 })
    ));
}

#[test]
fn manifest_rejects_unknown_fields() {
    let json = minimal_manifest(&command_entry("list_jobs", false)).replace(
        "\"spec\": \"§15.2\"",
        "\"spec\": \"§15.2\", \"allow_anything\": true",
    );
    assert!(matches!(
        CommandAllowlist::from_json(&json),
        Err(ManifestError::Malformed { .. })
    ));
}

#[test]
fn manifest_rejects_a_regex_pattern_we_cannot_evaluate() {
    let json = minimal_manifest(&command_entry("list_jobs", false))
        .replace("\"^exec\"", "\"^(exec|shell).*$\"");
    assert!(matches!(
        CommandAllowlist::from_json(&json),
        Err(ManifestError::UnsupportedPattern { .. })
    ));
}

// ---------------------------------------------------------------------------
// authorize()
// ---------------------------------------------------------------------------

#[test]
fn authorize_accepts_an_allowlisted_command_from_its_window() {
    let spec = allowlist()
        .authorize(&main_window(), "list_recordings")
        .expect("allowlisted");
    assert_eq!(spec.name, "list_recordings");
}

#[test]
fn authorize_rejects_an_unknown_command() {
    let error = allowlist()
        .authorize(&main_window(), "exec_shell")
        .expect_err("a forged invoke must be rejected in Rust");
    assert_eq!(error.envelope_parts().code, COMMAND_NOT_AUTHORIZED);
    assert!(!error.envelope_parts().retryable);
}

#[test]
fn authorize_rejects_an_allowlisted_command_from_the_wrong_window() {
    let error = allowlist()
        .authorize(&WindowLabel::new("rogue-popup"), "list_recordings")
        .expect_err("window is not declared for this command");
    assert_eq!(error.envelope_parts().code, COMMAND_NOT_AUTHORIZED);
}

#[test]
fn authorize_does_not_echo_hostile_text_into_the_envelope() {
    let error = allowlist()
        .authorize(&main_window(), "<img src=x onerror=alert(1)>")
        .expect_err("rejected");
    let parts = error.envelope_parts();
    assert!(
        !parts.message.contains('<'),
        "envelope echoed markup: {}",
        parts.message
    );
}

// ---------------------------------------------------------------------------
// Patterns and globs
// ---------------------------------------------------------------------------

#[test]
fn name_patterns_respect_their_anchors() {
    let prefix = NamePattern::parse("^exec").expect("valid");
    assert!(prefix.matches("exec_anything"));
    assert!(!prefix.matches("safe_exec"));

    let exact = NamePattern::parse("^read_file$").expect("valid");
    assert!(exact.matches("read_file"));
    assert!(!exact.matches("read_file_chunk"));

    assert!(NamePattern::parse("^(a|b)$").is_err());
    assert!(NamePattern::parse("^$").is_err());
}

#[test]
fn permission_globs_match_plugin_namespaces() {
    assert!(glob_matches("shell:*", "shell:allow-execute"));
    assert!(glob_matches("shell:*", "shell:"));
    assert!(!glob_matches("shell:*", "core:shell:allow-execute"));
    assert!(glob_matches("http:default", "http:default"));
    assert!(!glob_matches("http:default", "http:defaults"));
    assert!(glob_matches("*", "anything"));
    assert!(glob_matches(
        "core:*:allow-print",
        "core:webview:allow-print"
    ));
}

// ---------------------------------------------------------------------------
// Argument validation (§15.1)
// ---------------------------------------------------------------------------

#[test]
fn identifiers_accept_uuids_and_reject_injection() {
    assert!(validate_identifier("job_id", "9f1c0f2e-5f3a-4a0d-9b9b-1f0a2b3c4d5e").is_ok());
    assert!(matches!(
        validate_identifier("job_id", ""),
        Err(ArgumentError::Empty { field: "job_id" })
    ));
    assert!(matches!(
        validate_identifier("job_id", "../../etc/passwd"),
        Err(ArgumentError::DisallowedCharacter { .. })
    ));
    assert!(matches!(
        validate_identifier("job_id", "id; rm -rf /"),
        Err(ArgumentError::DisallowedCharacter { .. })
    ));
    assert!(matches!(
        validate_identifier("job_id", &"a".repeat(129)),
        Err(ArgumentError::TooLong { max: 128, .. })
    ));
}

#[test]
fn bounded_strings_reject_control_characters_and_overlong_input() {
    assert!(validate_bounded_string("title", "Quarterly review — notes", 64).is_ok());
    assert!(matches!(
        validate_bounded_string("title", "bell\u{7}", 64),
        Err(ArgumentError::DisallowedCharacter { .. })
    ));
    assert!(matches!(
        validate_bounded_string("title", "ααααα", 4),
        Err(ArgumentError::TooLong { max: 4, .. })
    ));
    assert!(validate_bounded_string("title", "line\nbreak\tok", 64).is_ok());
}

#[test]
fn cursors_are_opaque_and_bounded() {
    assert!(validate_cursor("cursor", "eyJvZmZzZXQiOjQyfQ==").is_ok());
    assert!(matches!(
        validate_cursor("cursor", "cursor with space"),
        Err(ArgumentError::DisallowedCharacter { .. })
    ));
    assert!(matches!(
        validate_cursor("cursor", ""),
        Err(ArgumentError::Empty { .. })
    ));
}

#[test]
fn enums_return_our_own_static_vocabulary() {
    const FORMATS: &[&str] = &["markdown", "pdf", "docx"];
    assert_eq!(
        validate_enum("format", "pdf", FORMATS).expect("accepted"),
        "pdf"
    );
    assert!(matches!(
        validate_enum("format", "PDF", FORMATS),
        Err(ArgumentError::NotAccepted { .. })
    ));
}

#[test]
fn raw_paths_are_refused_wherever_a_path_is_not_expected() {
    for hostile in [
        "/etc/passwd",
        "~/Library/Keychains/login.keychain",
        "C:\\Windows\\System32\\config\\SAM",
        "\\\\server\\share\\x",
        "../../../secret",
        "file:///etc/passwd",
        "asset://localhost/etc/passwd",
        "sub/dir",
    ] {
        assert!(
            matches!(
                reject_raw_path("name", hostile),
                Err(ArgumentError::RawPath { .. })
            ),
            "{hostile} was accepted"
        );
    }
    assert!(reject_raw_path("name", "quarterly-review").is_ok());
}

#[test]
fn argument_errors_map_to_the_stable_envelope_code() {
    let error = validate_identifier("job_id", "../x").expect_err("rejected");
    assert_eq!(error.envelope_parts().code, COMMAND_ARGUMENT_INVALID);
}

// ---------------------------------------------------------------------------
// ApprovedPathHandle (§15.1)
// ---------------------------------------------------------------------------

#[test]
fn an_approved_handle_records_its_origin_and_canonical_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("meeting.wav");
    std::fs::write(&file, b"RIFF").expect("write");

    let handle = ApprovedPathHandle::from_dialog_selection(
        SelectionOrigin::OpenFileDialog,
        &file,
        1_700_000_000,
    )
    .expect("approved");
    assert_eq!(handle.origin(), SelectionOrigin::OpenFileDialog);
    assert_eq!(handle.approved_at_unix(), 1_700_000_000);
    assert_eq!(handle.path(), file.canonicalize().expect("canonical"));
    assert!(handle
        .require_origin(SelectionOrigin::OpenFileDialog)
        .is_ok());
    assert!(handle
        .require_origin(SelectionOrigin::SaveFileDialog)
        .is_err());
}

/// Regression (D2.4 fuzz corpus): validators are handed untrusted, lossily
/// decoded text. Prefix checks must never slice a `&str` at a byte index that
/// falls inside a multi-byte character.
#[test]
fn validators_do_not_panic_on_multibyte_or_replacement_characters() {
    for hostile in [
        "a\u{2067}b",
        "\u{FFFD}\u{FFFD}\u{FFFD}rec",
        "a\u{2066}b\u{2069}c",
        "é",
        "\u{10FFFF}",
        "fil\u{2067}",
        "\u{1F600}",
    ] {
        let _ = reject_raw_path("title", hostile);
        let _ = validate_identifier("job_id", hostile);
        let _ = validate_bounded_string("title", hostile, 64);
        let _ = validate_cursor("cursor", hostile);
        let _ = secret_shape_of(hostile);
    }
    assert!(reject_raw_path("title", "a\u{2067}b").is_ok());
    assert!(matches!(
        reject_raw_path("title", "file:\u{2067}"),
        Err(ArgumentError::RawPath { .. })
    ));
}

#[test]
fn a_frontend_supplied_path_string_cannot_become_an_approved_handle() {
    // There is no `From<String>`/`FromStr`/`Deserialize` route into the type
    // (see the compile_fail doctests on `ApprovedPathHandle`); the only
    // constructor additionally refuses a location that does not resolve, so a
    // fabricated path from the webview cannot be laundered into a handle.
    let fabricated = std::path::Path::new("/definitely/not/here/plaudon-test-does-not-exist");
    let error = ApprovedPathHandle::from_dialog_selection(
        SelectionOrigin::OpenFileDialog,
        fabricated,
        1_700_000_000,
    )
    .expect_err("must not resolve");
    assert!(matches!(error, ArgumentError::UnresolvableSelection { .. }));
}

#[test]
fn a_save_destination_may_not_exist_yet_but_its_parent_must() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("backup.plaudon");
    let handle = ApprovedPathHandle::from_dialog_selection(
        SelectionOrigin::SaveFileDialog,
        &target,
        1_700_000_100,
    )
    .expect("parent resolves");
    assert_eq!(handle.path().file_name(), target.file_name());

    let missing_parent = dir.path().join("nope").join("backup.plaudon");
    assert!(ApprovedPathHandle::from_dialog_selection(
        SelectionOrigin::SaveFileDialog,
        &missing_parent,
        1_700_000_100
    )
    .is_err());
}

// ---------------------------------------------------------------------------
// Response scanning (§6.3A)
// ---------------------------------------------------------------------------

#[test]
fn the_scanner_recognises_each_documented_secret_shape() {
    assert_eq!(
        secret_shape_of("Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature"),
        Some(SecretShape::BearerToken)
    );
    assert_eq!(
        secret_shape_of("sk-or-v1-9f2a7c4e1b6d8a30f5c7e9b1d3a5f7c9"),
        Some(SecretShape::ApiKey)
    );
    assert_eq!(
        secret_shape_of(
            "https://files.plaud.example/recording.wav?X-Amz-Signature=deadbeefcafe&X-Amz-Expires=900"
        ),
        Some(SecretShape::SignedUrl)
    );
    assert_eq!(
        secret_shape_of("9f2a7c4e1b6d8a30f5c7e9b1d3a5f7c9"),
        Some(SecretShape::DeviceToken)
    );
    assert_eq!(
        secret_shape_of("0x00007fb4c0a1e2f0"),
        Some(SecretShape::RawPointer)
    );
    assert_eq!(
        secret_shape_of("Quarterly review with the design team"),
        None
    );
}

#[test]
fn a_response_carrying_a_bearer_token_is_rejected() {
    let payload = json!({ "device": { "auth": "Bearer eyJhbGciOiJIUzI1NiJ9.abcdefghijklmnop" } });
    let error = assert_no_secret_in_response(&payload).expect_err("leak");
    assert_eq!(error.shape, SecretShape::BearerToken);
    assert_eq!(error.pointer, "/device/auth");
    assert_eq!(error.envelope_parts().code, COMMAND_NOT_AUTHORIZED);
}

#[test]
fn a_response_carrying_an_openrouter_key_is_rejected() {
    let payload = json!({ "settings": { "value": "sk-or-v1-0a1b2c3d4e5f60718293a4b5c6d7e8f90" } });
    let error = assert_no_secret_in_response(&payload).expect_err("leak");
    assert_eq!(error.shape, SecretShape::ApiKey);
}

#[test]
fn a_response_carrying_a_signed_url_is_rejected() {
    let payload = json!({
        "items": [
            { "id": "a-1" },
            { "href": "https://cdn.plaud.example/a.wav?Expires=1700&Signature=abc123&Key-Pair-Id=K1" }
        ]
    });
    let error = assert_no_secret_in_response(&payload).expect_err("leak");
    assert_eq!(error.shape, SecretShape::SignedUrl);
    assert_eq!(error.pointer, "/items/1/href");
}

#[test]
fn a_response_carrying_a_device_token_is_rejected() {
    let payload = json!({ "device": { "handshake": "a1b2c3d4e5f60718293a4b5c6d7e8f90" } });
    let error = assert_no_secret_in_response(&payload).expect_err("leak");
    assert_eq!(error.shape, SecretShape::DeviceToken);
}

#[test]
fn a_response_carrying_a_secret_field_name_or_pointer_is_rejected() {
    let named = json!({ "vault": { "api_key": "short" } });
    assert_eq!(
        assert_no_secret_in_response(&named)
            .expect_err("leak")
            .shape,
        SecretShape::SensitiveField
    );
    let pointer = json!({ "bridge": { "raw_pointer": 140_697_320_000_u64 } });
    assert_eq!(
        assert_no_secret_in_response(&pointer)
            .expect_err("leak")
            .shape,
        SecretShape::RawPointer
    );
}

#[test]
fn a_clean_response_passes_including_a_sha256_media_identity() {
    let payload = json!({
        "recording": {
            "id": "9f1c0f2e-5f3a-4a0d-9b9b-1f0a2b3c4d5e",
            "title": "Quarterly review",
            "media_sha256": "3b1f8c2d4e5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e",
            "duration_seconds": 3600,
            "vault_reference": "plaudon/device/9f1c0f2e",
            "has_token": false
        },
        "jobs": [{ "id": "job-1", "state": "completed" }]
    });
    assert!(assert_no_secret_in_response(&payload).is_ok());
}

// ---------------------------------------------------------------------------
// Source and config scanners used by the auditor
// ---------------------------------------------------------------------------

#[test]
fn generate_handler_parsing_handles_real_world_shapes() {
    let source = r#"
        // tauri::generate_handler![not_a_real_command]
        fn main() {
            let banner = "generate_handler![in_a_string]";
            tauri::Builder::default()
                .invoke_handler(tauri::generate_handler![
                    get_capabilities,
                    commands::library::list_recordings, // inline comment
                    /* block
                       comment */
                    crate::commands::jobs::cancel_job,
                ])
                .run(ctx)
                .expect(banner);
        }
    "#;
    assert_eq!(
        parse_generate_handler(source),
        vec!["get_capabilities", "list_recordings", "cancel_job"]
    );
}

#[test]
fn generate_handler_parsing_finds_several_invocations_and_other_delimiters() {
    let source = r"
        let a = tauri::generate_handler!(get_usage, list_jobs);
        let b = tauri::generate_handler!{ retry_job }
    ";
    assert_eq!(
        parse_generate_handler(source),
        vec!["get_usage", "list_jobs", "retry_job"]
    );
}

#[test]
fn generate_handler_parsing_ignores_a_commented_out_registration() {
    let source = r"
        /* tauri::generate_handler![ghost_command] */
        tauri::generate_handler![list_models]
    ";
    assert_eq!(parse_generate_handler(source), vec!["list_models"]);
}

#[test]
fn csp_parses_both_the_string_and_the_object_form() {
    let from_string = parse_csp(&json!("default-src 'self'; object-src 'none'"));
    assert_eq!(from_string["default-src"], vec!["'self'"]);
    assert_eq!(from_string["object-src"], vec!["'none'"]);

    let from_object = parse_csp(&json!({
        "default-src": "'self'",
        "connect-src": ["'self'", "ipc:", "http://ipc.localhost"]
    }));
    assert_eq!(
        from_object["connect-src"],
        vec!["'self'", "ipc:", "http://ipc.localhost"]
    );
}

#[test]
fn remote_origins_are_detected_and_local_ipc_origins_are_not() {
    assert!(is_remote_origin("https://cdn.example.com"));
    assert!(is_remote_origin("http://evil.test:8080"));
    assert!(is_remote_origin("wss://example.com"));
    assert!(!is_remote_origin("http://ipc.localhost"));
    assert!(!is_remote_origin("http://asset.localhost"));
    assert!(!is_remote_origin("'self'"));
    assert!(!is_remote_origin("ipc:"));
}

/// An application that registers its own Rust-side scheme (EPIC E serves media
/// over `plaudon-media://` instead of enabling the asset protocol) must be able
/// to name it in `media-src`. Tauri exposes such a scheme as
/// `http://<scheme>.localhost` on Windows, which is served by our own handler
/// and never leaves the machine, so it is not a remote origin.
#[test]
fn a_tauri_custom_protocol_localhost_origin_is_not_remote() {
    assert!(is_local_custom_protocol_origin(
        "http://plaudon-media.localhost"
    ));
    assert!(!is_remote_origin("http://plaudon-media.localhost"));
    assert!(!is_remote_origin("HTTP://PLAUDON-MEDIA.LOCALHOST"));
}

/// The exemption is narrow on purpose: anything that could actually address a
/// different host, port or path is still a remote origin.
#[test]
fn the_custom_protocol_exemption_does_not_widen_into_a_bypass() {
    for hostile in [
        "http://evil.localhost.attacker.test",
        "http://plaudon-media.localhost:8080",
        "http://plaudon-media.localhost/steal",
        "http://user:pw@plaudon-media.localhost",
        "http://a.b.localhost",
        "http://.localhost",
        "https://plaudon-media.localhost",
        "http://localhost",
        "http://127.0.0.1",
    ] {
        assert!(
            !is_local_custom_protocol_origin(hostile),
            "{hostile} must not be treated as a custom-protocol origin"
        );
        assert!(is_remote_origin(hostile), "{hostile} must count as remote");
    }
}
