//! D2.1 contract tests for the Tauri least-privilege auditor (§2.4, §6.3A,
//! §15.1, §18.5).
//!
//! Each test builds a synthetic `apps/desktop/src-tauri` tree — `tauri.conf.json`,
//! capability files and Rust sources — inside a `TempDir` and asserts the
//! auditor's verdict. Nothing here touches the real application tree, the
//! network or the OS keychain.

use std::fs;
use std::path::Path;

use plaudon_security::tauri_guard::allowlist::CommandAllowlist;
use plaudon_security::tauri_guard::audit::{self, AuditError, AuditOptions, FindingCode};
use serde_json::{json, Value};
use tempfile::TempDir;

/// A small but representative slice of the §15.2 table, used as the manifest
/// for the synthetic apps so the fixtures stay readable.
const FIXTURE_MANIFEST: &str = r#"{
  "version": 1,
  "commands": [
    { "name": "get_capabilities", "windows": ["main"], "returns_secret": false, "spec": "§15.2" },
    { "name": "list_recordings", "windows": ["main"], "returns_secret": false, "spec": "§15.2" },
    { "name": "cancel_job", "windows": ["main"], "returns_secret": false, "spec": "§15.2" }
  ],
  "forbidden_permissions": ["shell:*", "http:*", "fs:allow-read-file", "fs:default"],
  "required_csp": {
    "default-src": ["'self'"],
    "script-src": ["'self'"],
    "object-src": ["'none'"],
    "base-uri": ["'none'"],
    "frame-src": ["'none'"],
    "form-action": ["'none'"],
    "connect-src": ["'self'", "ipc:", "http://ipc.localhost"]
  },
  "forbidden_command_patterns": ["^exec", "^run_shell", "^shell", "^http_", "^proxy", "^read_file$", "^write_file$", "^eval"],
  "release_requirements": {
    "devtools": false, "with_global_tauri": false,
    "remote_window_urls": false, "dangerous_keys": false
  }
}"#;

const CLEAN_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self' asset: http://asset.localhost; object-src 'none'; base-uri 'none'; \
frame-src 'none'; form-action 'none'; connect-src 'self' ipc: http://ipc.localhost";

fn manifest() -> CommandAllowlist {
    CommandAllowlist::from_json(FIXTURE_MANIFEST).expect("fixture manifest is valid")
}

fn clean_config() -> Value {
    json!({
        "productName": "Plaudon",
        "identifier": "ai.plaudon.desktop",
        "build": { "frontendDist": "../dist" },
        "app": {
            "withGlobalTauri": false,
            "windows": [{ "label": "main", "title": "Plaudon", "url": "index.html" }],
            "security": {
                "csp": CLEAN_CSP,
                "assetProtocol": { "enable": true, "scope": ["$APPDATA/media/*"] }
            }
        }
    })
}

fn clean_capability() -> Value {
    json!({
        "identifier": "main-capability",
        "windows": ["main"],
        "permissions": ["core:event:allow-listen", { "identifier": "core:window:allow-start-dragging" }]
    })
}

const CLEAN_SOURCE: &str = r"
mod commands;

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_capabilities,
            commands::library::list_recordings,
            commands::jobs::cancel_job,
        ])
        .run(tauri::generate_context!())
        .expect('error while running');
}
";

/// Build a synthetic repository root containing a workspace `Cargo.toml` and an
/// `apps/desktop/src-tauri` tree.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("write");
        let fixture = Self { dir };
        fs::create_dir_all(fixture.src_tauri().join("src")).expect("mkdir src");
        fs::create_dir_all(fixture.src_tauri().join("capabilities")).expect("mkdir capabilities");
        fixture.write_config(&clean_config());
        fixture.write_capability("main.json", &clean_capability());
        fixture.write_source("lib.rs", CLEAN_SOURCE);
        fixture
    }

    /// A root with no `apps/desktop/src-tauri` at all.
    fn without_app() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("write");
        Self { dir }
    }

    /// The real state of `main` before the desktop app lands: spec §6.4
    /// requires the directories to exist, so git tracks a `.gitkeep` in each,
    /// but there is no `tauri.conf.json` and no Rust source yet.
    fn with_gitkeep_placeholders_only() -> Self {
        let fixture = Self::without_app();
        fs::create_dir_all(fixture.src_tauri().join("src")).expect("mkdir src");
        fs::write(fixture.src_tauri().join(".gitkeep"), "").expect("write .gitkeep");
        fs::write(fixture.src_tauri().join("src/.gitkeep"), "").expect("write .gitkeep");
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn src_tauri(&self) -> std::path::PathBuf {
        self.dir.path().join("apps/desktop/src-tauri")
    }

    fn write_config(&self, config: &Value) {
        fs::write(
            self.src_tauri().join("tauri.conf.json"),
            serde_json::to_string_pretty(config).expect("serialize"),
        )
        .expect("write config");
    }

    fn write_capability(&self, name: &str, capability: &Value) {
        fs::write(
            self.src_tauri().join("capabilities").join(name),
            serde_json::to_string_pretty(capability).expect("serialize"),
        )
        .expect("write capability");
    }

    fn write_source(&self, name: &str, source: &str) {
        fs::write(self.src_tauri().join("src").join(name), source).expect("write source");
    }

    fn audit(&self) -> audit::AuditReport {
        audit::run(&AuditOptions::new(self.root()), &manifest()).expect("audit runs")
    }
}

fn codes(report: &audit::AuditReport) -> Vec<FindingCode> {
    report.findings.iter().map(|f| f.code).collect()
}

// ---------------------------------------------------------------------------

#[test]
fn a_least_privilege_app_passes() {
    let fixture = Fixture::new();
    let report = fixture.audit();
    assert!(
        report.passed(),
        "unexpected findings: {:#?}",
        report.findings
    );
    assert!(report.app_present);
    assert_eq!(
        report.registered_commands,
        ["cancel_job", "get_capabilities", "list_recordings"]
    );
    assert_eq!(report.granted_permissions.len(), 2);
    assert_eq!(report.capability_windows, ["main"]);
    assert_eq!(report.csp["object-src"], ["'none'"]);
    assert!(!report.release_flags.with_global_tauri);
    assert_ne!(
        report.not_covered.len(),
        0,
        "the report must state what it does not cover"
    );
}

#[test]
fn a_command_registered_but_missing_from_the_manifest_fails() {
    let fixture = Fixture::new();
    fixture.write_source(
        "lib.rs",
        r"
        fn main() {
            tauri::Builder::default()
                .invoke_handler(tauri::generate_handler![
                    get_capabilities,
                    commands::library::list_recordings,
                    commands::jobs::cancel_job,
                    commands::debug::run_shell_command,
                ]);
        }
        ",
    );
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::CommandNotInManifest));
    assert!(report
        .findings
        .iter()
        .any(|f| f.message.contains("run_shell_command")));
}

#[test]
fn a_manifest_command_that_is_not_registered_fails() {
    let fixture = Fixture::new();
    fixture.write_source(
        "lib.rs",
        r"
        fn main() {
            tauri::Builder::default()
                .invoke_handler(tauri::generate_handler![get_capabilities, list_recordings]);
        }
        ",
    );
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::ManifestCommandNotRegistered));
    assert!(report
        .findings
        .iter()
        .any(|f| f.message.contains("cancel_job")));
}

#[test]
fn a_shell_execute_permission_fails() {
    let fixture = Fixture::new();
    fixture.write_capability(
        "main.json",
        &json!({
            "identifier": "main-capability",
            "windows": ["main"],
            "permissions": ["core:event:allow-listen", "shell:allow-execute"]
        }),
    );
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::ForbiddenPermission));
    assert!(report
        .findings
        .iter()
        .any(|f| f.message.contains("shell:allow-execute")));
}

#[test]
fn a_weak_csp_missing_object_src_none_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["security"]["csp"] = json!(
        "default-src 'self'; script-src 'self'; base-uri 'none'; frame-src 'none'; \
form-action 'none'; connect-src 'self' ipc: http://ipc.localhost"
    );
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::CspDirectiveMissing));
    assert!(report
        .findings
        .iter()
        .any(|f| f.message.contains("object-src")));
}

#[test]
fn a_csp_source_that_is_present_but_incomplete_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["security"]["csp"] = json!(CLEAN_CSP.replace(" ipc: http://ipc.localhost", ""));
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::CspSourceMissing));
}

#[test]
fn a_csp_with_a_remote_origin_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["security"]["csp"] = json!(format!(
        "{CLEAN_CSP} https://cdn.example.com; img-src https://tracker.example"
    ));
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::CspRemoteOrigin));
}

#[test]
fn with_global_tauri_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["withGlobalTauri"] = json!(true);
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::WithGlobalTauri));
    assert!(report.release_flags.with_global_tauri);
}

#[test]
fn devtools_enabled_in_the_release_configuration_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["windows"][0]["devtools"] = json!(true);
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::DevtoolsEnabled));
    assert!(report.release_flags.devtools);
}

#[test]
fn a_dangerous_configuration_key_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["security"]["dangerousDisableAssetCspModification"] = json!(true);
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::DangerousKey));
    assert!(report.release_flags.dangerous_keys);
}

#[test]
fn a_remote_window_url_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["windows"][0]["url"] = json!("https://app.example.com/");
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::RemoteWindowUrl));
    assert!(report.release_flags.remote_window_urls);
}

#[test]
fn an_asset_protocol_recursive_scope_fails() {
    let fixture = Fixture::new();
    let mut config = clean_config();
    config["app"]["security"]["assetProtocol"]["scope"] = json!(["**"]);
    fixture.write_config(&config);
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::AssetProtocolWildcardScope));
}

#[test]
fn a_capability_targeting_an_unknown_window_fails() {
    let fixture = Fixture::new();
    fixture.write_capability(
        "rogue.json",
        &json!({
            "identifier": "rogue-capability",
            "windows": ["debug-console"],
            "permissions": ["core:event:allow-listen"]
        }),
    );
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::UnknownCapabilityWindow));
    assert!(report
        .findings
        .iter()
        .any(|f| f.message.contains("debug-console")));
}

#[test]
fn an_app_without_any_generate_handler_cannot_be_audited_and_fails() {
    let fixture = Fixture::new();
    fixture.write_source("lib.rs", "fn main() { println!(\"no commands here\"); }\n");
    let report = fixture.audit();
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::NoHandlerRegistered));
}

// ---------------------------------------------------------------------------
// Missing application
// ---------------------------------------------------------------------------

#[test]
fn a_missing_application_is_an_error_not_a_silent_pass() {
    let fixture = Fixture::without_app();
    let error = audit::run(&AuditOptions::new(fixture.root()), &manifest())
        .expect_err("absence must not look like success");
    assert!(matches!(error, AuditError::AppMissing { .. }));
    assert!(error.to_string().contains("not verifiable"));
}

#[test]
fn allow_missing_app_still_audits_the_manifest() {
    let fixture = Fixture::without_app();
    let report = audit::run(
        &AuditOptions::new(fixture.root()).allow_missing_app(true),
        &manifest(),
    )
    .expect("manifest-only audit runs");
    assert!(report.passed());
    assert!(!report.app_present);
    assert_eq!(report.manifest_commands.len(), 3);
    assert_eq!(report.registered_commands.len(), 0);
}

/// §6.4 requires `apps/desktop/src-tauri/` to exist in the repository layout,
/// so the directory is tracked with a `.gitkeep` long before the app lands.
/// Presence must therefore key off `tauri.conf.json`, not the directory node.
#[test]
fn a_gitkeep_only_src_tauri_directory_counts_as_absent() {
    let fixture = Fixture::with_gitkeep_placeholders_only();
    assert!(fixture.src_tauri().is_dir());
    assert!(!audit::app_is_present(fixture.root()));

    let error = audit::run(&AuditOptions::new(fixture.root()), &manifest())
        .expect_err("an empty placeholder directory is not an auditable app");
    assert!(matches!(error, AuditError::AppMissing { .. }));
    assert!(error.to_string().contains("tauri.conf.json"));

    let report = audit::run(
        &AuditOptions::new(fixture.root()).allow_missing_app(true),
        &manifest(),
    )
    .expect("manifest-only audit runs");
    assert!(report.passed());
    assert!(!report.app_present);
}

/// The escape hatch must never become a way to skip auditing an app that does
/// exist: once `tauri.conf.json` is there, `--allow-missing-app` changes
/// nothing.
#[test]
fn allow_missing_app_does_not_suppress_findings_for_an_app_that_exists() {
    let fixture = Fixture::with_gitkeep_placeholders_only();
    fixture.write_config(&clean_config());
    assert!(audit::app_is_present(fixture.root()));

    let report = audit::run(
        &AuditOptions::new(fixture.root()).allow_missing_app(true),
        &manifest(),
    )
    .expect("audit runs");
    assert!(report.app_present);
    assert!(!report.passed());
    assert!(codes(&report).contains(&FindingCode::NoHandlerRegistered));

    // …and the verdict is identical without the flag.
    let strict = audit::run(&AuditOptions::new(fixture.root()), &manifest()).expect("audit runs");
    assert_eq!(codes(&strict), codes(&report));
}

// ---------------------------------------------------------------------------
// The real, checked-in manifest
// ---------------------------------------------------------------------------

#[test]
fn the_checked_in_manifest_passes_its_own_audit_on_this_branch() {
    let allowlist = CommandAllowlist::load_default().expect("embedded manifest is valid");
    let fixture = Fixture::without_app();
    let report = audit::run(
        &AuditOptions::new(fixture.root()).allow_missing_app(true),
        allowlist,
    )
    .expect("manifest-only audit runs");
    assert!(
        report.passed(),
        "checked-in manifest findings: {:#?}",
        report.findings
    );
    // 40 §15.2 commands plus the ADR-documented additions. The exact membership
    // and the documentation rule are asserted by
    // `tauri_guard::tests::the_manifest_covers_the_spec_table_and_documents_every_addition`;
    // here we only pin that nothing is lost.
    assert!(
        report.manifest_commands.len() >= 40,
        "manifest shrank below the §15.2 table: {} commands",
        report.manifest_commands.len()
    );
}

#[test]
fn the_checked_in_manifest_and_the_real_repository_root_agree() {
    // `discover_root` must find the workspace that contains this test binary's
    // crate, so CI can run the auditor without passing `--root`.
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = AuditOptions::discover_root(here).expect("workspace root");
    assert!(root.join("Cargo.toml").is_file());
    assert!(root
        .join("scripts/security/tauri-command-allowlist.json")
        .is_file());
}
