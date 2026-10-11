//! Dependency policy for this crate (spec 5.5).
//!
//! Spec 5.5 is explicit about *how* completion notices reach the user: macOS
//! Notification Center and Windows toasts, "not SMTP, Bark push or browser
//! notification infrastructure". The only way to keep that true over time is to
//! keep the delivery machinery out of the dependency list, so the crate's own
//! manifest is treated as a policy document and checked by a test.
//!
//! The check is intentionally simple and hermetic: it reads
//! `crates/notifications/Cargo.toml` and refuses any dependency whose name
//! matches [`FORBIDDEN_DEPENDENCIES`]. It runs against the declared
//! dependencies, which is where such a dependency would have to be introduced
//! deliberately; a workspace-wide transitive audit belongs to the licensing and
//! vulnerability scan in spec 22(8).

/// Dependency names this crate must never declare: HTTP clients and servers,
/// mail/SMTP clients, push-service SDKs and web/browser notification shims.
pub const FORBIDDEN_DEPENDENCIES: &[&str] = &[
    // HTTP clients
    "reqwest",
    "hyper",
    "hyper-util",
    "ureq",
    "isahc",
    "surf",
    "attohttpc",
    "curl",
    "curl-sys",
    "http-client",
    "awc",
    "minreq",
    // HTTP servers / listeners
    "axum",
    "actix-web",
    "warp",
    "tiny_http",
    "rouille",
    "hyper-server",
    // Mail / SMTP
    "lettre",
    "mail-send",
    "smtp",
    "async-smtp",
    "samotop",
    // Push services and web notification shims
    "fcm",
    "a2",
    "web-push",
    "pusher",
    "bark",
    "web-sys",
    "wasm-bindgen",
    "notify-rust",
];

/// Extract the dependency names declared by a `Cargo.toml`.
///
/// Recognises `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`
/// and their `[target.'cfg(...)'.*-dependencies]` forms, in both the
/// `name = ...` and `[dependencies.name]` spellings. Comments and values are
/// ignored, so a denied name inside a comment does not trip the check.
#[must_use]
pub fn declared_dependencies(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_dependency_table = false;

    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(header) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            let header = header.trim();
            if let Some(name) = dependency_table_entry(header) {
                names.push(name);
                in_dependency_table = false;
            } else {
                in_dependency_table = is_dependency_table(header);
            }
            continue;
        }
        if !in_dependency_table || line.is_empty() {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            let key = key.trim().trim_matches('"');
            if !key.is_empty() {
                names.push(key.to_owned());
            }
        }
    }

    names
}

/// Whether `header` names a dependency table, e.g. `dependencies` or
/// `target.'cfg(target_os = "macos")'.dependencies`.
fn is_dependency_table(header: &str) -> bool {
    last_segment(header).is_some_and(|segment| segment.ends_with("dependencies"))
}

/// Whether `header` is a per-dependency table such as `[dependencies.serde]`,
/// returning the dependency name.
fn dependency_table_entry(header: &str) -> Option<String> {
    let (table, name) = header.rsplit_once('.')?;
    if is_dependency_table(table) {
        Some(name.trim().trim_matches('"').to_owned())
    } else {
        None
    }
}

fn last_segment(header: &str) -> Option<&str> {
    // Split on '.' outside of quotes, so `target.'cfg(a.b)'.dependencies` works.
    let mut segment_start = 0;
    let mut quote: Option<char> = None;
    for (index, ch) in header.char_indices() {
        match (quote, ch) {
            (Some(open), _) if ch == open => quote = None,
            (None, '\'' | '"') => quote = Some(ch),
            (None, '.') => segment_start = index + 1,
            _ => {}
        }
    }
    header.get(segment_start..).map(str::trim)
}

/// Names from `manifest` that violate [`FORBIDDEN_DEPENDENCIES`].
#[must_use]
pub fn forbidden_in(manifest: &str) -> Vec<String> {
    declared_dependencies(manifest)
        .into_iter()
        .filter(|name| FORBIDDEN_DEPENDENCIES.contains(&name.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = include_str!("../Cargo.toml");

    #[test]
    fn this_crate_declares_no_remote_delivery_dependency() {
        let offenders = forbidden_in(MANIFEST);
        assert!(offenders.is_empty(), "spec 5.5 forbids {offenders:?} in plaudon-notifications");
    }

    #[test]
    fn the_real_manifest_is_parsed_not_silently_empty() {
        let declared = declared_dependencies(MANIFEST);
        for expected in ["plaudon-domain", "async-trait", "serde", "tokio", "tracing", "regex"] {
            assert!(declared.iter().any(|name| name == expected), "missing {expected} in {declared:?}");
        }
        // Platform tables are parsed on every host, not only the matching one.
        assert!(declared.iter().any(|name| name == "objc2-user-notifications"));
        assert!(declared.iter().any(|name| name == "tauri-winrt-notification"));
    }

    #[test]
    fn the_denylist_actually_catches_violations() {
        let manifest = r#"
[package]
name = "example"

[dependencies]
reqwest = { version = "0.12", features = ["json"] }
serde = "1"

[dependencies.lettre]
version = "0.11"

[target.'cfg(target_os = "windows")'.dependencies]
web-push = "0.10"
"#;
        let mut offenders = forbidden_in(manifest);
        offenders.sort();
        assert_eq!(offenders, vec!["lettre", "reqwest", "web-push"]);
    }

    #[test]
    fn comments_and_values_are_not_mistaken_for_dependencies() {
        let manifest = r#"
[dependencies]
# reqwest is forbidden here (spec 5.5)
tokio = { version = "1", features = ["reqwest-like-name"] }
"#;
        assert!(forbidden_in(manifest).is_empty());
        assert_eq!(declared_dependencies(manifest), vec!["tokio".to_owned()]);
    }

    #[test]
    fn non_dependency_tables_are_ignored() {
        let manifest = r#"
[package]
reqwest = "not a dependency"

[lints.clippy]
all = "deny"
"#;
        assert!(declared_dependencies(manifest).is_empty());
    }
}
