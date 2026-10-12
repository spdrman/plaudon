//! The build-time Tauri least-privilege audit (§2.4, §6.3A, §18.5, §24).
//!
//! §6.3A: *"Tauri plugin capability declarations alone are insufficient
//! assurance because command registration and window permissions must both be
//! constrained and audited."* This module performs that audit against the
//! source tree and produces the machine-readable evidence artifact consumed by
//! the §24 release-evidence bundle.
//!
//! What is audited, and from where:
//!
//! | Input | Check |
//! |---|---|
//! | `apps/desktop/src-tauri/src/**/*.rs` | every `tauri::generate_handler![…]` list is diffed against the manifest in **both** directions |
//! | `apps/desktop/src-tauri/capabilities/*.json` | granted permissions vs. `forbidden_permissions`; target windows vs. manifest windows |
//! | `apps/desktop/src-tauri/tauri.conf.json` | CSP directives/sources, remote origins, `withGlobalTauri`, `dangerous*` keys, remote window URLs, release devtools, asset-protocol scope |
//!
//! # Honest scope
//!
//! This audits the **source tree**. §2.4 additionally requires inspecting the
//! final bundled entitlements, installer signature and update validation; those
//! are packaging-stage checks on a built artifact and are out of scope here.
//! A pass therefore means "the configuration committed to this repository is
//! least-privilege", not "the shipped installer is".

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::allowlist::{CommandAllowlist, WindowLabel};

/// Relative location of the Tauri application inside the repository.
pub const APP_SRC_TAURI: &str = "apps/desktop/src-tauri";

/// CSP source tokens that address the local webview/IPC transport and are
/// therefore not remote origins. Everything else matching `http://`/`https://`
/// is a remote origin and is rejected (§6.3A: no remotely hosted UI content).
pub const LOCAL_CSP_ORIGINS: &[&str] = &[
    "http://ipc.localhost",
    "http://asset.localhost",
    "ipc:",
    "asset:",
];

/// CSP source tokens that defeat the purpose of having a CSP.
const UNSAFE_CSP_SOURCES: &[&str] = &["*", "'unsafe-inline'", "'unsafe-eval'", "data:", "blob:"];

// ---------------------------------------------------------------------------
// Options and errors
// ---------------------------------------------------------------------------

/// Inputs to [`run`].
#[derive(Debug, Clone)]
pub struct AuditOptions {
    /// Repository root. `apps/desktop/src-tauri` is resolved beneath it.
    pub root: PathBuf,
    /// Audit the manifest alone when the application has not landed yet.
    pub allow_missing_app: bool,
}

impl AuditOptions {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            allow_missing_app: false,
        }
    }

    #[must_use]
    pub fn allow_missing_app(mut self, allow: bool) -> Self {
        self.allow_missing_app = allow;
        self
    }

    /// Walk up from `start` looking for the workspace `Cargo.toml`.
    pub fn discover_root(start: &Path) -> Result<PathBuf, AuditError> {
        let joined = if start.is_absolute() {
            start.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| AuditError::Io {
                    path: start.display().to_string(),
                    detail: e.to_string(),
                })?
                .join(start)
        };
        // Canonicalize so the evidence artifact records a real path rather than
        // one containing `.` or `..` components.
        let mut current = joined.canonicalize().unwrap_or(joined);
        loop {
            let candidate = current.join("Cargo.toml");
            if let Ok(text) = fs::read_to_string(&candidate) {
                if text
                    .lines()
                    .any(|line| line.trim_start().starts_with("[workspace]"))
                {
                    return Ok(current);
                }
            }
            if !current.pop() {
                return Err(AuditError::WorkspaceRootNotFound {
                    start: start.display().to_string(),
                });
            }
        }
    }
}

/// A condition that stops the audit from producing a verdict at all. Distinct
/// from a [`Finding`], which *is* a verdict.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuditError {
    /// The Tauri application is not in the tree, so least privilege is **not
    /// yet verifiable**. Never silently a pass.
    ///
    /// Presence is decided by `tauri.conf.json`, not by the directory node:
    /// spec §6.4 requires `apps/desktop/src-tauri/` to exist in the repository
    /// layout, so an empty (or `.gitkeep`-only) directory means the app has
    /// not landed yet.
    #[error("{APP_SRC_TAURI} has no tauri.conf.json under `{root}`: Tauri least privilege is not verifiable on this tree (pass --allow-missing-app to audit the manifest alone)")]
    AppMissing { root: String },
    #[error("no workspace Cargo.toml found at or above `{start}`")]
    WorkspaceRootNotFound { start: String },
    #[error("cannot read `{path}`: {detail}")]
    Io { path: String, detail: String },
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// Stable machine-readable identifier of an audit finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FindingCode {
    /// A command is registered in `generate_handler!` but absent from the manifest.
    CommandNotInManifest,
    /// A manifest command is not registered in any `generate_handler!`.
    ManifestCommandNotRegistered,
    /// The same command is registered more than once.
    DuplicateRegistration,
    /// No `generate_handler!` invocation was found at all.
    NoHandlerRegistered,
    /// A manifest command name matches a `forbidden_command_patterns` entry.
    ManifestCommandForbidden,
    /// A capability grants a permission matching `forbidden_permissions`.
    ForbiddenPermission,
    /// A capability targets a window the manifest does not know.
    UnknownCapabilityWindow,
    /// `app.security.csp` is absent or empty.
    CspMissing,
    /// A required CSP directive is absent.
    CspDirectiveMissing,
    /// A required CSP source is absent from a present directive.
    CspSourceMissing,
    /// A CSP source names a remote origin.
    CspRemoteOrigin,
    /// A CSP source defeats the policy (`*`, `'unsafe-inline'`, …).
    CspUnsafeSource,
    /// `withGlobalTauri` is enabled.
    WithGlobalTauri,
    /// A configuration key beginning with `dangerous` is present.
    DangerousKey,
    /// A window or the frontend dist points at a remote URL.
    RemoteWindowUrl,
    /// Devtools are enabled in the committed configuration.
    DevtoolsEnabled,
    /// The asset protocol is enabled with a `**` scope entry.
    AssetProtocolWildcardScope,
    /// A file the audit depends on could not be read.
    InputUnreadable,
    /// A file the audit depends on is not valid JSON / not the expected shape.
    InputMalformed,
}

/// How a finding is treated by the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Fails the audit.
    Error,
}

/// One audit verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub code: FindingCode,
    pub severity: Severity,
    /// Repository-relative file, config pointer or symbol the finding is about.
    pub locus: String,
    /// Human-readable explanation. Contains no file contents beyond the
    /// identifier that triggered the finding.
    pub message: String,
    /// Specification clause the rule comes from.
    pub spec: &'static str,
}

impl Finding {
    fn error(
        code: FindingCode,
        locus: impl Into<String>,
        message: impl Into<String>,
        spec: &'static str,
    ) -> Self {
        Self {
            code,
            severity: Severity::Error,
            locus: locus.into(),
            message: message.into(),
            spec,
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{:?}] {}: {} ({})",
            self.code, self.locus, self.message, self.spec
        )
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// One permission grant observed in a capability file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrantedPermission {
    /// Repository-relative capability file.
    pub capability_file: String,
    /// The capability's `identifier`, if present.
    pub capability: Option<String>,
    /// The permission identifier as written.
    pub permission: String,
}

/// The release posture observed in `tauri.conf.json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
// Independent observed posture flags recorded verbatim in the evidence
// artifact; see `ReleaseRequirements`.
#[allow(clippy::struct_excessive_bools)]
pub struct ObservedReleaseFlags {
    pub devtools: bool,
    pub with_global_tauri: bool,
    pub remote_window_urls: bool,
    pub dangerous_keys: bool,
    pub asset_protocol_enabled: bool,
    pub asset_protocol_wildcard_scope: bool,
}

/// The §24 release-evidence artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditReport {
    /// Schema version of this report.
    pub report_version: u32,
    /// Audited repository root.
    pub root: String,
    /// Whether `apps/desktop/src-tauri` was present.
    pub app_present: bool,
    /// Manifest schema version.
    pub manifest_version: u32,
    /// Allowlisted command names, sorted.
    pub manifest_commands: Vec<String>,
    /// Command names found in `generate_handler!`, sorted and de-duplicated.
    pub registered_commands: Vec<String>,
    /// Rust sources that contained a `generate_handler!` invocation.
    pub handler_sources: Vec<String>,
    /// Every permission granted by every capability file.
    pub granted_permissions: Vec<GrantedPermission>,
    /// Window labels targeted by capability files.
    pub capability_windows: Vec<String>,
    /// `app.security.csp` normalized to directive → sources.
    pub csp: BTreeMap<String, Vec<String>>,
    /// Observed release posture.
    pub release_flags: ObservedReleaseFlags,
    /// All findings; a non-empty list means the audit failed.
    pub findings: Vec<Finding>,
    /// Checks this report does **not** cover (§2.4).
    pub not_covered: &'static [&'static str],
}

impl AuditReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }
}

const NOT_COVERED: &[&str] = &[
    "bundled application entitlements of a built artifact (§2.4)",
    "installer signature and notarization (§2.4)",
    "update manifest signature validation (§16.5)",
    "runtime behaviour of the shipped webview",
];

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Is a Tauri application actually present under `root`?
///
/// Keyed off `apps/desktop/src-tauri/tauri.conf.json`, deliberately **not** off
/// the directory node: spec §6.4 fixes the repository layout, so
/// `apps/desktop/src-tauri/` exists (with a tracked placeholder) from the first
/// commit. An empty directory means "not landed yet", and reporting that as an
/// audited app would be a false pass.
///
/// Once the config is there the app counts as present even if nothing else is,
/// so `--allow-missing-app` can never be used to skip auditing a real app: the
/// missing handler list and the unreadable capability files become findings.
#[must_use]
pub fn app_is_present(root: &Path) -> bool {
    root.join(APP_SRC_TAURI).join("tauri.conf.json").is_file()
}

/// Run the audit. Returns a report whose `findings` list is the verdict.
pub fn run(
    options: &AuditOptions,
    allowlist: &CommandAllowlist,
) -> Result<AuditReport, AuditError> {
    let src_tauri = options.root.join(APP_SRC_TAURI);
    let app_present = app_is_present(&options.root);
    if !app_present && !options.allow_missing_app {
        return Err(AuditError::AppMissing {
            root: options.root.display().to_string(),
        });
    }

    let mut report = AuditReport {
        report_version: 1,
        root: options.root.display().to_string(),
        app_present,
        manifest_version: allowlist.version(),
        manifest_commands: allowlist
            .command_names()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        registered_commands: Vec::new(),
        handler_sources: Vec::new(),
        granted_permissions: Vec::new(),
        capability_windows: Vec::new(),
        csp: BTreeMap::new(),
        release_flags: ObservedReleaseFlags::default(),
        findings: Vec::new(),
        not_covered: NOT_COVERED,
    };

    audit_manifest_itself(allowlist, &mut report);

    if !app_present {
        return Ok(report);
    }

    audit_commands(&src_tauri, &options.root, allowlist, &mut report);
    audit_capabilities(&src_tauri, &options.root, allowlist, &mut report);
    audit_config(&src_tauri, &options.root, allowlist, &mut report);

    report.findings.sort_by(|a, b| {
        (a.locus.as_str(), a.message.as_str()).cmp(&(b.locus.as_str(), b.message.as_str()))
    });
    Ok(report)
}

/// The manifest is self-checked even when the app is absent, so
/// `--allow-missing-app` is still a meaningful gate.
fn audit_manifest_itself(allowlist: &CommandAllowlist, report: &mut AuditReport) {
    for command in allowlist.commands() {
        if let Some(pattern) = allowlist.forbidden_name_match(&command.name) {
            report.findings.push(Finding::error(
                FindingCode::ManifestCommandForbidden,
                format!("manifest:{}", command.name),
                format!(
                    "command `{}` matches forbidden pattern `{}`; §15.1 prohibits generic shell, HTTP-proxy and arbitrary-path commands",
                    command.name,
                    pattern.as_str()
                ),
                "§15.1",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Command registration
// ---------------------------------------------------------------------------

fn audit_commands(
    src_tauri: &Path,
    root: &Path,
    allowlist: &CommandAllowlist,
    report: &mut AuditReport,
) {
    let src_dir = src_tauri.join("src");
    let mut rust_files = Vec::new();
    collect_files(&src_dir, "rs", &mut rust_files);
    rust_files.sort();

    let mut registered: Vec<String> = Vec::new();
    let mut duplicates: BTreeSet<String> = BTreeSet::new();
    for file in &rust_files {
        let Ok(source) = fs::read_to_string(file) else {
            report.findings.push(Finding::error(
                FindingCode::InputUnreadable,
                relative(root, file),
                "Rust source could not be read, so its command registrations are unknown",
                "§6.3A",
            ));
            continue;
        };
        let names = parse_generate_handler(&source);
        if names.is_empty() {
            continue;
        }
        report.handler_sources.push(relative(root, file));
        for name in names {
            if registered.contains(&name) {
                duplicates.insert(name.clone());
            } else {
                registered.push(name);
            }
        }
    }

    for duplicate in &duplicates {
        report.findings.push(Finding::error(
            FindingCode::DuplicateRegistration,
            format!("generate_handler:{duplicate}"),
            format!("command `{duplicate}` is registered more than once"),
            "§6.3A",
        ));
    }

    registered.sort();
    report.registered_commands.clone_from(&registered);

    if registered.is_empty() {
        report.findings.push(Finding::error(
            FindingCode::NoHandlerRegistered,
            relative(root, &src_dir),
            "no `tauri::generate_handler![…]` invocation was found; the command surface cannot be audited",
            "§6.3A",
        ));
        return;
    }

    let manifest: BTreeSet<&str> = allowlist.command_names().into_iter().collect();
    for name in &registered {
        if !manifest.contains(name.as_str()) {
            report.findings.push(Finding::error(
                FindingCode::CommandNotInManifest,
                format!("generate_handler:{name}"),
                format!(
                    "command `{name}` is registered with Tauri but is not in scripts/security/tauri-command-allowlist.json"
                ),
                "§6.3A",
            ));
        }
    }
    let registered_set: BTreeSet<&str> = registered.iter().map(String::as_str).collect();
    for name in &manifest {
        if !registered_set.contains(name) {
            report.findings.push(Finding::error(
                FindingCode::ManifestCommandNotRegistered,
                format!("manifest:{name}"),
                format!(
                    "command `{name}` is on the allowlist but is not registered in any `tauri::generate_handler!`"
                ),
                "§15.2",
            ));
        }
    }
}

/// Extract the command names from every `generate_handler!` invocation in one
/// Rust source file.
///
/// Handles multi-line lists, line and block comments, `module::command` paths,
/// trailing commas, `[]`/`()`/`{}` macro delimiters and several invocations per
/// file. A list item that is not a plain path is returned verbatim so the diff
/// fails loudly rather than silently dropping it.
#[must_use]
pub fn parse_generate_handler(source: &str) -> Vec<String> {
    let code = strip_comments(source);
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = code[cursor..].find("generate_handler!") {
        let at = cursor + rel + "generate_handler!".len();
        cursor = at;
        let bytes = code.as_bytes();
        let mut index = at;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let Some(open) = bytes.get(index).copied() else {
            break;
        };
        let close = match open {
            b'[' => b']',
            b'(' => b')',
            b'{' => b'}',
            _ => continue,
        };
        let mut depth = 0i32;
        let start = index + 1;
        let mut end = None;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == open {
                depth += 1;
            } else if byte == close {
                depth -= 1;
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            index += 1;
        }
        let Some(end) = end else { break };
        for item in code[start..end].split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let name = item.rsplit("::").next().unwrap_or(item).trim();
            out.push(name.to_owned());
        }
        cursor = end;
    }
    out
}

/// Remove comments and the *contents* of string and raw-string literals,
/// leaving an empty `""` in their place.
///
/// Both removals matter for this scanner: a `//` inside a literal must not be
/// mistaken for a comment, and a `generate_handler!` mentioned inside a string
/// (a log line, a doc example) must not be mistaken for a registration.
///
/// Operates on `&str` slices at character boundaries throughout, so it is
/// UTF-8 safe for sources with non-ASCII literals or doc comments.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while !rest.is_empty() {
        if rest.starts_with("//") {
            let end = rest.find('\n').unwrap_or(rest.len());
            rest = &rest[end..];
            continue;
        }
        if rest.starts_with("/*") {
            out.push(' ');
            rest = &rest[block_comment_len(rest)..];
            continue;
        }
        let after_identifier_char = out
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !after_identifier_char {
            if let Some(len) = raw_string_len(rest) {
                out.push_str("\"\"");
                rest = &rest[len..];
                continue;
            }
        }
        if rest.starts_with('"') {
            out.push_str("\"\"");
            rest = &rest[string_literal_len(rest)..];
            continue;
        }
        let Some(ch) = rest.chars().next() else { break };
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Byte length of the nested block comment starting at `rest`, including both
/// delimiters. An unterminated comment consumes the remainder.
fn block_comment_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut index = 0usize;
    let mut depth = 0usize;
    while index + 1 < bytes.len() {
        if bytes[index] == b'/' && bytes[index + 1] == b'*' {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    rest.len()
}

/// Byte length of a `"…"` literal including both quotes, honouring `\` escapes.
fn string_literal_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut index = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return (index + 1).min(bytes.len()),
            _ => index += 1,
        }
    }
    rest.len()
}

/// Byte length of a raw string literal (`r"…"`, `r#"…"#`, …) if `rest` starts
/// with one.
fn raw_string_len(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    if bytes.first() != Some(&b'r') {
        return None;
    }
    let mut index = 1usize;
    let mut hashes = 0usize;
    while bytes.get(index) == Some(&b'#') {
        hashes += 1;
        index += 1;
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    let body_start = index + 1;
    let terminator = format!("\"{}", "#".repeat(hashes));
    Some(
        rest[body_start..]
            .find(&terminator)
            .map_or(rest.len(), |at| body_start + at + terminator.len()),
    )
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

fn audit_capabilities(
    src_tauri: &Path,
    root: &Path,
    allowlist: &CommandAllowlist,
    report: &mut AuditReport,
) {
    let dir = src_tauri.join("capabilities");
    if !dir.is_dir() {
        return;
    }
    let mut files = Vec::new();
    collect_files(&dir, "json", &mut files);
    let mut unsupported = Vec::new();
    collect_files_excluding_extension(&dir, "json", &mut unsupported);
    files.sort();
    unsupported.sort();

    for file in &unsupported {
        report.findings.push(Finding::error(
            FindingCode::InputMalformed,
            relative(root, file),
            "capability files must be JSON so they can be audited; this file cannot be inspected",
            "§6.3A",
        ));
    }

    let known_windows: BTreeSet<&str> = allowlist
        .known_windows()
        .into_iter()
        .map(WindowLabel::as_str)
        .collect();
    let mut windows = BTreeSet::new();

    for file in &files {
        let locus = relative(root, file);
        let Ok(text) = fs::read_to_string(file) else {
            report.findings.push(Finding::error(
                FindingCode::InputUnreadable,
                locus,
                "capability file could not be read",
                "§6.3A",
            ));
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            report.findings.push(Finding::error(
                FindingCode::InputMalformed,
                locus,
                "capability file is not valid JSON",
                "§6.3A",
            ));
            continue;
        };
        audit_capability_windows(&value, &locus, &known_windows, &mut windows, report);
        audit_capability_permissions(&value, &locus, allowlist, report);
    }

    report.capability_windows = windows.into_iter().collect();
}

/// Check the windows one capability targets, recording every label seen.
fn audit_capability_windows(
    capability: &Value,
    locus: &str,
    known_windows: &BTreeSet<&str>,
    seen: &mut BTreeSet<String>,
    report: &mut AuditReport,
) {
    for window in capability
        .get("windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(label) = window.as_str() else {
            report.findings.push(Finding::error(
                FindingCode::InputMalformed,
                locus.to_owned(),
                "capability `windows` entries must be strings",
                "§2.4",
            ));
            continue;
        };
        seen.insert(label.to_owned());
        if !known_windows.contains(label) {
            report.findings.push(Finding::error(
                FindingCode::UnknownCapabilityWindow,
                locus.to_owned(),
                format!(
                    "capability targets window `{label}`, which no allowlisted command declares; §2.4 requires commands to be restricted by window"
                ),
                "§2.4",
            ));
        }
    }
}

/// Check the permissions one capability grants, recording every grant.
fn audit_capability_permissions(
    capability: &Value,
    locus: &str,
    allowlist: &CommandAllowlist,
    report: &mut AuditReport,
) {
    let capability_id = capability
        .get("identifier")
        .and_then(Value::as_str)
        .map(str::to_owned);
    for permission in capability
        .get("permissions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let identifier = match permission {
            Value::String(s) => Some(s.clone()),
            Value::Object(map) => map
                .get("identifier")
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        };
        let Some(identifier) = identifier else {
            report.findings.push(Finding::error(
                FindingCode::InputMalformed,
                locus.to_owned(),
                "capability `permissions` entry has no identifier",
                "§6.3A",
            ));
            continue;
        };
        if let Some(pattern) = allowlist.forbidden_permission_match(&identifier) {
            report.findings.push(Finding::error(
                FindingCode::ForbiddenPermission,
                locus.to_owned(),
                format!(
                    "permission `{identifier}` is forbidden (matches `{pattern}`); §6.3A prohibits the arbitrary shell plugin and filesystem-wide privileges"
                ),
                "§6.3A",
            ));
        }
        report.granted_permissions.push(GrantedPermission {
            capability_file: locus.to_owned(),
            capability: capability_id.clone(),
            permission: identifier,
        });
    }
}

// ---------------------------------------------------------------------------
// tauri.conf.json
// ---------------------------------------------------------------------------

fn audit_config(
    src_tauri: &Path,
    root: &Path,
    allowlist: &CommandAllowlist,
    report: &mut AuditReport,
) {
    let path = src_tauri.join("tauri.conf.json");
    let locus = relative(root, &path);
    let Ok(text) = fs::read_to_string(&path) else {
        report.findings.push(Finding::error(
            FindingCode::InputUnreadable,
            locus,
            "tauri.conf.json is missing or unreadable, so the CSP and release flags cannot be verified",
            "§2.4",
        ));
        return;
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        report.findings.push(Finding::error(
            FindingCode::InputMalformed,
            locus,
            "tauri.conf.json is not valid JSON",
            "§2.4",
        ));
        return;
    };

    audit_csp(&config, &locus, allowlist, report);
    audit_release_flags(&config, &locus, report);
}

fn audit_csp(config: &Value, locus: &str, allowlist: &CommandAllowlist, report: &mut AuditReport) {
    let csp_value = config.pointer("/app/security/csp");
    let directives = match csp_value {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(value) => parse_csp(value),
    };
    report.csp = directives.clone();

    if directives.is_empty() {
        report.findings.push(Finding::error(
            FindingCode::CspMissing,
            format!("{locus}#/app/security/csp"),
            "no Content-Security-Policy is configured; §6.3A requires a strict CSP",
            "§6.3A",
        ));
        return;
    }

    for (directive, required) in allowlist.required_csp() {
        let Some(present) = directives.get(directive) else {
            report.findings.push(Finding::error(
                FindingCode::CspDirectiveMissing,
                format!("{locus}#/app/security/csp"),
                format!("CSP directive `{directive}` is missing"),
                "§6.3A",
            ));
            continue;
        };
        for source in required {
            if !present.iter().any(|s| s == source) {
                report.findings.push(Finding::error(
                    FindingCode::CspSourceMissing,
                    format!("{locus}#/app/security/csp"),
                    format!(
                        "CSP directive `{directive}` is missing the required source `{source}`"
                    ),
                    "§6.3A",
                ));
            }
        }
    }

    for (directive, sources) in &directives {
        for source in sources {
            if is_remote_origin(source) {
                report.findings.push(Finding::error(
                    FindingCode::CspRemoteOrigin,
                    format!("{locus}#/app/security/csp"),
                    format!(
                        "CSP directive `{directive}` allows the remote origin `{source}`; §6.3A prohibits remotely hosted UI content"
                    ),
                    "§6.3A",
                ));
            }
            if UNSAFE_CSP_SOURCES.contains(&source.as_str()) {
                report.findings.push(Finding::error(
                    FindingCode::CspUnsafeSource,
                    format!("{locus}#/app/security/csp"),
                    format!(
                        "CSP directive `{directive}` allows `{source}`, which defeats the policy"
                    ),
                    "§6.3A",
                ));
            }
        }
    }
}

/// Normalize `app.security.csp`, which Tauri accepts either as a policy string
/// or as a directive → source(s) map.
#[must_use]
pub fn parse_csp(value: &Value) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    match value {
        Value::String(policy) => {
            for part in policy.split(';') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let mut tokens = part.split_whitespace();
                let Some(directive) = tokens.next() else {
                    continue;
                };
                out.entry(directive.to_ascii_lowercase())
                    .or_default()
                    .extend(tokens.map(str::to_owned));
            }
        }
        Value::Object(map) => {
            for (directive, sources) in map {
                let list = match sources {
                    Value::String(single) => single.split_whitespace().map(str::to_owned).collect(),
                    Value::Array(items) => items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    _ => Vec::new(),
                };
                out.entry(directive.to_ascii_lowercase())
                    .or_default()
                    .extend(list);
            }
        }
        _ => {}
    }
    out
}

/// `true` when a CSP source names an origin outside the local webview transport.
#[must_use]
pub fn is_remote_origin(source: &str) -> bool {
    let lowered = source.to_ascii_lowercase();
    let looks_remote = lowered.starts_with("http://")
        || lowered.starts_with("https://")
        || lowered.starts_with("ws://")
        || lowered.starts_with("wss://")
        || lowered.starts_with("//");
    looks_remote && !LOCAL_CSP_ORIGINS.iter().any(|allowed| lowered == *allowed)
}

fn audit_release_flags(config: &Value, locus: &str, report: &mut AuditReport) {
    let mut flags = ObservedReleaseFlags::default();

    for (pointer, value) in collect_entries(config) {
        let key = pointer.rsplit('/').next().unwrap_or_default();
        let lowered_key = key.to_ascii_lowercase();
        if lowered_key.starts_with("dangerous") {
            flags.dangerous_keys = true;
            report.findings.push(Finding::error(
                FindingCode::DangerousKey,
                format!("{locus}#{pointer}"),
                format!("configuration key `{key}` is prohibited"),
                "§6.3A",
            ));
        }
        if key == "withGlobalTauri" && value.as_bool() == Some(true) {
            flags.with_global_tauri = true;
            report.findings.push(Finding::error(
                FindingCode::WithGlobalTauri,
                format!("{locus}#{pointer}"),
                "`withGlobalTauri` injects the IPC surface into `window`; it must be false",
                "§6.3A",
            ));
        }
        if key == "devtools" && value.as_bool() == Some(true) {
            flags.devtools = true;
            report.findings.push(Finding::error(
                FindingCode::DevtoolsEnabled,
                format!("{locus}#{pointer}"),
                "webview devtools must be disabled in the committed release configuration",
                "§2.4",
            ));
        }
    }

    for (index, window) in config
        .pointer("/app/windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(url) = window.get("url").and_then(Value::as_str) {
            if is_remote_origin(url) {
                flags.remote_window_urls = true;
                report.findings.push(Finding::error(
                    FindingCode::RemoteWindowUrl,
                    format!("{locus}#/app/windows/{index}/url"),
                    format!("window URL `{url}` is remote; §6.3A prohibits remotely hosted UI"),
                    "§6.3A",
                ));
            }
        }
    }

    if let Some(dist) = config
        .pointer("/build/frontendDist")
        .and_then(Value::as_str)
    {
        if is_remote_origin(dist) {
            flags.remote_window_urls = true;
            report.findings.push(Finding::error(
                FindingCode::RemoteWindowUrl,
                format!("{locus}#/build/frontendDist"),
                format!("`frontendDist` points at the remote origin `{dist}`"),
                "§6.3A",
            ));
        }
    }

    audit_asset_protocol(config, locus, &mut flags, report);

    report.release_flags = flags;
}

/// §16.2 "Unauthorized frontend file access": a `**` asset-protocol scope hands
/// the webview recursive filesystem reads, which defeats the backend path
/// checks entirely.
fn audit_asset_protocol(
    config: &Value,
    locus: &str,
    flags: &mut ObservedReleaseFlags,
    report: &mut AuditReport,
) {
    let enabled = config
        .pointer("/app/security/assetProtocol/enable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    flags.asset_protocol_enabled = enabled;
    if !enabled {
        return;
    }
    let scope: Vec<String> = match config.pointer("/app/security/assetProtocol/scope") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s.clone()),
                Value::Object(map) => map.get("path").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            })
            .collect(),
        Some(Value::Object(map)) => map
            .get("allow")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    if scope.iter().any(|entry| entry.contains("**")) {
        flags.asset_protocol_wildcard_scope = true;
        report.findings.push(Finding::error(
            FindingCode::AssetProtocolWildcardScope,
            format!("{locus}#/app/security/assetProtocol/scope"),
            "the asset protocol is enabled with a `**` scope entry, which grants the webview recursive filesystem reads",
            "§16.2",
        ));
    }
}

/// Every `(json_pointer, value)` pair in the document, depth-first.
fn collect_entries(root: &Value) -> Vec<(String, &Value)> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), root)];
    while let Some((pointer, value)) = stack.pop() {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let child_pointer = format!("{pointer}/{key}");
                    out.push((child_pointer.clone(), child));
                    stack.push((child_pointer, child));
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    let child_pointer = format!("{pointer}/{index}");
                    stack.push((child_pointer, child));
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn collect_files(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, extension, out);
        } else if path.extension().is_some_and(|e| e == extension) {
            out.push(path);
        }
    }
}

fn collect_files_excluding_extension(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_excluding_extension(&path, extension, out);
        } else if path.extension().is_none_or(|e| e != extension) {
            out.push(path);
        }
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}
