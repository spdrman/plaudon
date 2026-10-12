//! D2.1 — Tauri least privilege: the command allowlist, the Rust-side
//! authorization check, argument validation, response scanning and the CI
//! auditor (§2.4, §6.3A, §15.1, §15.2, §16.2 "Unauthorized frontend file
//! access" / "Local automation escape", §18.5).
//!
//! # Why this module exists
//!
//! §6.3A is explicit that *"Tauri plugin capability declarations alone are
//! insufficient assurance because command registration and window permissions
//! must both be constrained and audited"*, and that *"each Tauri command will
//! authorize its requested operation in Rust, regardless of frontend
//! validation"*. This module therefore provides two independent controls:
//!
//! 1. **Runtime**: [`CommandAllowlist::authorize`], called at the top of invoke
//!    dispatch. A forged `window.__TAURI__.core.invoke("…")` from compromised
//!    or modified frontend code is rejected in Rust even if the capability
//!    files would have permitted it.
//! 2. **Build time**: the [`audit`] module and the `plaudon-tauri-audit`
//!    binary, which diff the commands actually registered in
//!    `tauri::generate_handler![…]` against the checked-in manifest, reject
//!    forbidden plugin permissions, verify the CSP, and reject
//!    `withGlobalTauri`, `dangerous*` keys, remote window URLs and release
//!    devtools. It emits the §24 release-evidence JSON artifact.
//!
//! The desktop application (`apps/desktop/**`) is owned by another team; this
//! module deliberately has **no dependency on Tauri itself**, so the audit runs
//! in plain `cargo test` and in CI without a webview toolchain.
//!
//! # Manifest schema (`scripts/security/tauri-command-allowlist.json`)
//!
//! The manifest is the single source of truth and is embedded into this crate
//! with `include_str!`, so [`CommandAllowlist::load_default`] has no runtime
//! file dependency.
//!
//! ```json
//! {
//!   "version": 1,
//!   "commands": [
//!     { "name": "get_capabilities", "windows": ["main"],
//!       "returns_secret": false, "spec": "§15.2", "note": "optional" }
//!   ],
//!   "forbidden_permissions": ["shell:*", "http:default"],
//!   "required_csp": { "object-src": ["'none'"] },
//!   "forbidden_command_patterns": ["^exec", "^read_file$"],
//!   "release_requirements": {
//!     "devtools": false, "with_global_tauri": false,
//!     "remote_window_urls": false, "dangerous_keys": false
//!   }
//! }
//! ```
//!
//! Field rules, all enforced by [`CommandAllowlist::from_json`] (unknown fields
//! are rejected, so a typo fails closed rather than silently widening the
//! surface):
//!
//! * `version` — must be `1`. An unknown version is an error, never a
//!   best-effort parse.
//! * `commands[].name` — `snake_case`, `[a-z][a-z0-9_]*`, unique, and must not
//!   match any `forbidden_command_patterns` entry.
//! * `commands[].windows` — non-empty list of window labels
//!   (`[a-z][a-z0-9_-]*`). The union of these labels is the set of windows a
//!   capability file may target.
//! * `commands[].returns_secret` — must be `false` for every command. §6.3A
//!   forbids returning raw bearer tokens, signed download URLs, device secrets
//!   or platform-bridge pointers to the UI, so a `true` here is a manifest
//!   error, not a configuration option. The field exists so the prohibition is
//!   visible and mechanically checked rather than implicit.
//! * `commands[].spec` — the specification clause the command comes from.
//! * `forbidden_permissions` — glob patterns (`*` matches any run of
//!   characters) matched against every permission identifier granted by any
//!   capability file.
//! * `required_csp` — directive → required source list. Each directive must be
//!   present in `app.security.csp` and must include at least those sources.
//! * `forbidden_command_patterns` — **anchored literal** patterns, not general
//!   regular expressions: an optional leading `^`, an optional trailing `$`,
//!   and a body restricted to `[a-z0-9_]`. Any other metacharacter is rejected
//!   at load time so a pattern can never silently fail to match.
//! * `release_requirements` — all four flags must be `false`; they exist to
//!   make the release posture explicit in the evidence artifact.
//!
//! # Honest scope
//!
//! * The auditor inspects the **source tree** (`tauri.conf.json`, capability
//!   JSON, Rust sources). It does not inspect a signed installer, notarization
//!   ticket or bundled entitlements; §2.4 requires that inspection too, and it
//!   is a separate packaging-stage check.
//! * [`ApprovedPathHandle`] proves that a path passed through this crate's
//!   approval function with a recorded origin and timestamp. It is a
//!   *structural* barrier — there is no `From<String>`, `FromStr` or
//!   `Deserialize` route into the type — not a cryptographic proof that an OS
//!   dialog produced the path.
//! * [`response::assert_no_secret_in_response`] is a shape check. It reliably
//!   catches the known secret formats this application handles; it cannot prove
//!   the absence of an arbitrary secret encoding.

pub mod allowlist;
pub mod audit;
pub mod response;
pub mod validate;

#[cfg(test)]
mod tests;

pub use allowlist::{
    AuthorizationError, CommandAllowlist, CommandSpec, ManifestError, NamePattern,
    ReleaseRequirements, WindowLabel, DEFAULT_MANIFEST_JSON,
};
pub use audit::{
    app_is_present, is_local_custom_protocol_origin, AuditError, AuditOptions, AuditReport,
    Finding, FindingCode, Severity,
};
pub use response::{assert_no_secret_in_response, ResponseLeakError, SecretShape};
pub use validate::{
    reject_raw_path, validate_bounded_string, validate_cursor, validate_enum, validate_identifier,
    ApprovedPathHandle, ArgumentError, SelectionOrigin,
};
