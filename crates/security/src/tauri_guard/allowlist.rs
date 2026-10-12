//! The checked-in Tauri command allowlist and the Rust-side authorization check
//! (§6.3A, §15.1, §15.2).
//!
//! The manifest schema is documented on the [parent module](super).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorEnvelopeParts, SecurityError, COMMAND_NOT_AUTHORIZED};

/// The manifest, embedded at compile time so the guard has no runtime file
/// dependency and cannot be weakened by editing a file next to the binary.
pub const DEFAULT_MANIFEST_JSON: &str =
    include_str!("../../../../scripts/security/tauri-command-allowlist.json");

/// The only manifest schema version this build understands.
const SUPPORTED_VERSION: u32 = 1;

/// Maximum length of a command name we are willing to echo back in an error.
const MAX_ECHOED_NAME: usize = 64;

// ---------------------------------------------------------------------------
// Window labels
// ---------------------------------------------------------------------------

/// A Tauri window label (`WebviewWindow::label()`), e.g. `main`.
///
/// §2.4 requires restricting invoked Rust commands **by window**, so the label
/// is part of the authorization key rather than an informational field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowLabel(String);

impl WindowLabel {
    /// Wrap a label coming from the Tauri runtime.
    ///
    /// No validation happens here: the runtime decides what labels exist, and a
    /// label that is not in the manifest simply fails [`CommandAllowlist::authorize`].
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self(label.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `true` when the label is a plain literal we can compare safely
    /// (`[a-z][a-z0-9_-]*`). Manifest labels must satisfy this; capability
    /// files that use wildcards are rejected by the auditor.
    #[must_use]
    pub fn is_plain_literal(&self) -> bool {
        let mut chars = self.0.chars();
        match chars.next() {
            Some(c) if c.is_ascii_lowercase() => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    }
}

impl fmt::Display for WindowLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Anchored literal name patterns
// ---------------------------------------------------------------------------

/// An anchored **literal** pattern used by `forbidden_command_patterns`.
///
/// Deliberately not a general regular expression: the manifest is a security
/// control, and a mis-typed regex that silently matches nothing would weaken it
/// without any signal. Supported syntax is a leading `^`, a trailing `$`, and a
/// body of `[a-z0-9_]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamePattern {
    source: String,
    literal: String,
    anchored_start: bool,
    anchored_end: bool,
}

impl NamePattern {
    /// Parse one pattern, rejecting anything outside the supported subset.
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let bad = || ManifestError::UnsupportedPattern {
            pattern: source.to_owned(),
        };
        let mut body = source;
        let anchored_start = body.starts_with('^');
        if anchored_start {
            body = &body[1..];
        }
        let anchored_end = body.ends_with('$');
        if anchored_end {
            body = &body[..body.len() - 1];
        }
        if body.is_empty() {
            return Err(bad());
        }
        if !body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(bad());
        }
        Ok(Self {
            source: source.to_owned(),
            literal: body.to_owned(),
            anchored_start,
            anchored_end,
        })
    }

    /// The pattern exactly as written in the manifest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        match (self.anchored_start, self.anchored_end) {
            (true, true) => name == self.literal,
            (true, false) => name.starts_with(&self.literal),
            (false, true) => name.ends_with(&self.literal),
            (false, false) => name.contains(&self.literal),
        }
    }
}

impl Serialize for NamePattern {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.source)
    }
}

/// Match a permission identifier against a `forbidden_permissions` glob.
///
/// `*` matches any run of characters (including none); every other character is
/// literal. Matching is case-sensitive, as Tauri permission identifiers are.
#[must_use]
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return false;
    };
    if !value.starts_with(first) {
        return false;
    }
    let mut rest = &value[first.len()..];
    let tail: Vec<&str> = segments.collect();
    if tail.is_empty() {
        return rest.is_empty();
    }
    let last_index = tail.len() - 1;
    for (i, segment) in tail.iter().enumerate() {
        if i == last_index {
            // The final literal must terminate the value (unless it is empty,
            // which means the pattern ended in `*`).
            return segment.is_empty() || rest.ends_with(segment) && rest.len() >= segment.len();
        }
        match rest.find(segment) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

/// One entry of the §15.2 command table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// `snake_case` command name as registered in `tauri::generate_handler!`.
    pub name: String,
    /// Window labels permitted to invoke this command.
    pub windows: Vec<WindowLabel>,
    /// Must be `false`: §6.3A forbids returning secrets to the UI.
    pub returns_secret: bool,
    /// Specification clause the command originates from.
    pub spec: String,
    /// Optional security note carried into the evidence artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CommandSpec {
    /// `true` when `window` may invoke this command.
    #[must_use]
    pub fn permits_window(&self, window: &WindowLabel) -> bool {
        self.windows.iter().any(|w| w == window)
    }
}

/// The release posture flags recorded in the evidence artifact. Every flag must
/// be `false` in the manifest and in the audited `tauri.conf.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
// Four independent release-posture flags, mirroring the manifest field names
// one-for-one; grouping them into sub-structs would obscure the schema.
#[allow(clippy::struct_excessive_bools)]
pub struct ReleaseRequirements {
    /// Webview devtools must be off in a release build (§2.4, §18.5).
    pub devtools: bool,
    /// `withGlobalTauri` injects the full IPC surface into `window`; forbidden.
    pub with_global_tauri: bool,
    /// Remotely hosted UI content is prohibited (§6.3A).
    pub remote_window_urls: bool,
    /// Any `dangerous*` configuration key is prohibited.
    pub dangerous_keys: bool,
}

impl ReleaseRequirements {
    /// The flags that are set when they must not be, as stable finding labels.
    #[must_use]
    pub fn violations(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.devtools {
            out.push("devtools");
        }
        if self.with_global_tauri {
            out.push("with_global_tauri");
        }
        if self.remote_window_urls {
            out.push("remote_window_urls");
        }
        if self.dangerous_keys {
            out.push("dangerous_keys");
        }
        out
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    version: u32,
    commands: Vec<CommandSpec>,
    forbidden_permissions: Vec<String>,
    required_csp: BTreeMap<String, Vec<String>>,
    forbidden_command_patterns: Vec<String>,
    release_requirements: ReleaseRequirements,
}

/// The parsed, validated command allowlist.
#[derive(Debug, Clone, Serialize)]
pub struct CommandAllowlist {
    version: u32,
    commands: Vec<CommandSpec>,
    forbidden_permissions: Vec<String>,
    required_csp: BTreeMap<String, Vec<String>>,
    forbidden_command_patterns: Vec<NamePattern>,
    release_requirements: ReleaseRequirements,
}

impl CommandAllowlist {
    /// The embedded manifest, parsed once per process.
    ///
    /// Returns an error rather than panicking so a corrupted manifest fails
    /// closed at the call site — no command is authorized — instead of
    /// aborting the application. A malformed checked-in manifest is a CI
    /// failure caught by `tests/tauri_allowlist_contract.rs` and by
    /// `plaudon-tauri-audit`, so this error path is unreachable in a shipped
    /// build.
    pub fn load_default() -> Result<&'static Self, ManifestError> {
        static CACHED: LazyLock<Result<CommandAllowlist, ManifestError>> =
            LazyLock::new(|| CommandAllowlist::from_json(DEFAULT_MANIFEST_JSON));
        CACHED.as_ref().map_err(Clone::clone)
    }

    /// Parse and validate a manifest document.
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest =
            serde_json::from_str(json).map_err(|e| ManifestError::Malformed {
                detail: e.to_string(),
            })?;
        if raw.version != SUPPORTED_VERSION {
            return Err(ManifestError::UnsupportedVersion { found: raw.version });
        }
        if raw.commands.is_empty() {
            return Err(ManifestError::NoCommands);
        }
        if raw.required_csp.is_empty() {
            return Err(ManifestError::NoRequiredCsp);
        }

        let patterns = raw
            .forbidden_command_patterns
            .iter()
            .map(|p| NamePattern::parse(p))
            .collect::<Result<Vec<_>, _>>()?;

        let mut seen: Vec<&str> = Vec::with_capacity(raw.commands.len());
        for command in &raw.commands {
            if !is_snake_case_command(&command.name) {
                return Err(ManifestError::InvalidCommandName {
                    name: command.name.clone(),
                });
            }
            if seen.contains(&command.name.as_str()) {
                return Err(ManifestError::DuplicateCommand {
                    name: command.name.clone(),
                });
            }
            seen.push(&command.name);
            if command.returns_secret {
                return Err(ManifestError::SecretReturningCommand {
                    name: command.name.clone(),
                });
            }
            if command.windows.is_empty() {
                return Err(ManifestError::NoWindows {
                    name: command.name.clone(),
                });
            }
            for window in &command.windows {
                if !window.is_plain_literal() {
                    return Err(ManifestError::InvalidWindowLabel {
                        name: command.name.clone(),
                        window: window.to_string(),
                    });
                }
            }
            if command.spec.trim().is_empty() {
                return Err(ManifestError::MissingSpecReference {
                    name: command.name.clone(),
                });
            }
            if let Some(pattern) = patterns.iter().find(|p| p.matches(&command.name)) {
                return Err(ManifestError::ForbiddenCommandName {
                    name: command.name.clone(),
                    pattern: pattern.as_str().to_owned(),
                });
            }
        }

        for permission in &raw.forbidden_permissions {
            if permission.trim().is_empty() {
                return Err(ManifestError::EmptyForbiddenPermission);
            }
        }

        let violations = raw.release_requirements.violations();
        if let Some(flag) = violations.first() {
            return Err(ManifestError::ReleaseRequirementNotEnforced { flag });
        }

        Ok(Self {
            version: raw.version,
            commands: raw.commands,
            forbidden_permissions: raw.forbidden_permissions,
            required_csp: raw.required_csp,
            forbidden_command_patterns: patterns,
            release_requirements: raw.release_requirements,
        })
    }

    /// **The §6.3A runtime check.** Call this at the top of invoke dispatch,
    /// before touching any argument, with the label of the window the invoke
    /// arrived from.
    ///
    /// A command that is absent from the manifest, or present but not permitted
    /// for that window, is rejected here regardless of what the capability
    /// files say.
    pub fn authorize(
        &self,
        window: &WindowLabel,
        command: &str,
    ) -> Result<&CommandSpec, AuthorizationError> {
        let Some(spec) = self.commands.iter().find(|c| c.name == command) else {
            return Err(AuthorizationError::UnknownCommand {
                command: redact_name(command),
            });
        };
        if !spec.permits_window(window) {
            return Err(AuthorizationError::WindowNotPermitted {
                command: spec.name.clone(),
                window: redact_name(window.as_str()),
            });
        }
        Ok(spec)
    }

    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub fn commands(&self) -> &[CommandSpec] {
        &self.commands
    }

    /// Sorted command names, for diffing against the registered handler list.
    #[must_use]
    pub fn command_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.commands.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Every window label mentioned by any command, sorted and de-duplicated.
    #[must_use]
    pub fn known_windows(&self) -> Vec<&WindowLabel> {
        let mut windows: Vec<&WindowLabel> = self
            .commands
            .iter()
            .flat_map(|c| c.windows.iter())
            .collect();
        windows.sort_unstable();
        windows.dedup();
        windows
    }

    /// The first forbidden-permission glob matching `permission`, if any.
    #[must_use]
    pub fn forbidden_permission_match(&self, permission: &str) -> Option<&str> {
        self.forbidden_permissions
            .iter()
            .find(|pattern| glob_matches(pattern, permission))
            .map(String::as_str)
    }

    #[must_use]
    pub fn forbidden_permissions(&self) -> &[String] {
        &self.forbidden_permissions
    }

    #[must_use]
    pub fn required_csp(&self) -> &BTreeMap<String, Vec<String>> {
        &self.required_csp
    }

    #[must_use]
    pub fn forbidden_command_patterns(&self) -> &[NamePattern] {
        &self.forbidden_command_patterns
    }

    /// The first forbidden pattern matching `name`, if any.
    #[must_use]
    pub fn forbidden_name_match(&self, name: &str) -> Option<&NamePattern> {
        self.forbidden_command_patterns
            .iter()
            .find(|p| p.matches(name))
    }

    #[must_use]
    pub fn release_requirements(&self) -> ReleaseRequirements {
        self.release_requirements
    }
}

fn is_snake_case_command(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Keep an attacker-controlled name out of logs and envelopes unchanged: only
/// identifier characters survive, and the result is bounded.
fn redact_name(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(MAX_ECHOED_NAME)
        .collect();
    if filtered.is_empty() {
        "<invalid>".to_owned()
    } else {
        filtered
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A command invoke that the Rust side refuses (§6.3A).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizationError {
    /// The command is not in the allowlist at all.
    #[error("command `{command}` is not on the allowlist")]
    UnknownCommand { command: String },
    /// The command exists but not for the invoking window.
    #[error("command `{command}` is not permitted from window `{window}`")]
    WindowNotPermitted { command: String, window: String },
}

impl SecurityError for AuthorizationError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        let message = match self {
            AuthorizationError::UnknownCommand { .. } => {
                "This application action is not available.".to_owned()
            }
            AuthorizationError::WindowNotPermitted { .. } => {
                "This application action is not available from this window.".to_owned()
            }
        };
        ErrorEnvelopeParts::new(COMMAND_NOT_AUTHORIZED, message).retryable(false)
    }
}

/// The checked-in manifest could not be loaded or failed validation.
///
/// Mapped to [`COMMAND_NOT_AUTHORIZED`] because the operational consequence is
/// exactly that: without a valid allowlist, no command is authorized. A
/// dedicated `TAURI_MANIFEST_INVALID` code has been requested from the owner of
/// `error.rs`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("allowlist manifest is not valid JSON for the expected schema: {detail}")]
    Malformed { detail: String },
    #[error("allowlist manifest version {found} is not supported")]
    UnsupportedVersion { found: u32 },
    #[error("allowlist manifest declares no commands")]
    NoCommands,
    #[error("allowlist manifest declares no required CSP directives")]
    NoRequiredCsp,
    #[error("`{name}` is not a snake_case command name")]
    InvalidCommandName { name: String },
    #[error("command `{name}` is declared more than once")]
    DuplicateCommand { name: String },
    #[error("command `{name}` declares `returns_secret: true`, which §6.3A forbids")]
    SecretReturningCommand { name: String },
    #[error("command `{name}` declares no window")]
    NoWindows { name: String },
    #[error("command `{name}` declares the unusable window label `{window}`")]
    InvalidWindowLabel { name: String, window: String },
    #[error("command `{name}` has no spec reference")]
    MissingSpecReference { name: String },
    #[error("command `{name}` matches the forbidden pattern `{pattern}`")]
    ForbiddenCommandName { name: String, pattern: String },
    #[error("`forbidden_command_patterns` entry `{pattern}` uses unsupported syntax")]
    UnsupportedPattern { pattern: String },
    #[error("`forbidden_permissions` contains an empty entry")]
    EmptyForbiddenPermission,
    #[error("`release_requirements.{flag}` must be false")]
    ReleaseRequirementNotEnforced { flag: &'static str },
}

impl SecurityError for ManifestError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        ErrorEnvelopeParts::new(
            COMMAND_NOT_AUTHORIZED,
            "The application command allowlist could not be loaded, so no action is available."
                .to_owned(),
        )
        .retryable(false)
    }
}
