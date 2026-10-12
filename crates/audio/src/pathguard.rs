//! Path hardening for untrusted import paths (§7.4, §16.5, F1.1).
//!
//! "We will resolve and validate symlinks/junctions instead of trusting them."
//! Every externally supplied media path passes through [`resolve_media_path`]
//! before any byte is read. The guard resolves the link chain, then validates
//! the *resolved* target — a symlink is not rejected for being a symlink, it is
//! rejected when its target is unreadable, is not a regular file, or escapes
//! the allowed roots.

use std::path::{Component, Path, PathBuf};

use crate::error::AudioError;

/// Hard ceiling on media size accepted by the importer. Beyond this we refuse
/// before reading rather than discovering it during decode.
pub const MAX_MEDIA_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Policy applied to an untrusted path.
#[derive(Debug, Clone, Default)]
pub struct PathPolicy {
    /// When non-empty, the resolved path must live under one of these roots.
    /// Manual import leaves this empty: the user picked the file themselves.
    pub allowed_roots: Vec<PathBuf>,
    /// Reject when the resolved file exceeds this many bytes.
    pub max_bytes: Option<u64>,
}

impl PathPolicy {
    /// Manual import: any user-picked location, bounded size.
    #[must_use]
    pub fn user_selected() -> Self {
        Self {
            allowed_roots: Vec::new(),
            max_bytes: Some(MAX_MEDIA_BYTES),
        }
    }

    /// Archive/staging access: confined to the given roots.
    #[must_use]
    pub fn confined_to(roots: Vec<PathBuf>) -> Self {
        Self {
            allowed_roots: roots,
            max_bytes: Some(MAX_MEDIA_BYTES),
        }
    }
}

/// A path that has been canonicalised and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMediaPath {
    /// Fully resolved, symlink-free absolute path.
    resolved: PathBuf,
    /// The path as supplied, retained as metadata only (§13.4: "preserve
    /// original names as metadata only").
    supplied: PathBuf,
    byte_length: u64,
    followed_symlink: bool,
}

impl ResolvedMediaPath {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.resolved
    }

    #[must_use]
    pub fn supplied_path(&self) -> &Path {
        &self.supplied
    }

    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    #[must_use]
    pub const fn followed_symlink(&self) -> bool {
        self.followed_symlink
    }

    /// Lowercased extension of the *supplied* name, which is what the user
    /// sees and what an attacker controls. Empty when there is none.
    #[must_use]
    pub fn supplied_extension(&self) -> String {
        self.supplied
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    /// The original file name with path separators, control characters and
    /// leading dots/dashes neutralised, for display and archive metadata only.
    #[must_use]
    pub fn sanitized_display_name(&self) -> String {
        let raw = self
            .supplied
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("recording");
        sanitize_file_name(raw)
    }
}

/// Neutralise a hostile file name for display/metadata use.
///
/// This never produces a path: separators, `..`, NUL, control characters and
/// Windows reserved device names are all removed or replaced.
#[must_use]
pub fn sanitize_file_name(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    out = out.trim().trim_matches('.').trim().to_string();
    while out.starts_with('-') {
        out.remove(0);
    }
    if out.is_empty() {
        return "recording".to_string();
    }

    let stem_upper = out
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem_upper.as_str()) {
        out.insert(0, '_');
    }

    // Bound the length so a 4 KiB name cannot blow a path limit downstream.
    const MAX_NAME_CHARS: usize = 180;
    if out.chars().count() > MAX_NAME_CHARS {
        out = out.chars().take(MAX_NAME_CHARS).collect();
    }
    out
}

/// Resolve and validate an untrusted media path.
pub fn resolve_media_path(
    supplied: &Path,
    policy: &PathPolicy,
) -> Result<ResolvedMediaPath, AudioError> {
    if has_traversal_component(supplied) {
        return Err(AudioError::PathRejected {
            reason: format!("path contains a `..` component: {}", supplied.display()),
        });
    }

    let symlink_metadata =
        std::fs::symlink_metadata(supplied).map_err(|source| AudioError::PathRejected {
            reason: format!("cannot stat {}: {source}", supplied.display()),
        })?;
    let followed_symlink = symlink_metadata.file_type().is_symlink();

    // `canonicalize` resolves the entire link chain and fails on a dangling
    // target, which is exactly the fail-closed behaviour we want.
    let resolved = std::fs::canonicalize(supplied).map_err(|source| AudioError::PathRejected {
        reason: format!(
            "cannot resolve {}{}: {source}",
            supplied.display(),
            if followed_symlink { " (symlink)" } else { "" }
        ),
    })?;

    let metadata = std::fs::metadata(&resolved).map_err(|source| AudioError::PathRejected {
        reason: format!(
            "cannot stat resolved target {}: {source}",
            resolved.display()
        ),
    })?;

    if !metadata.is_file() {
        return Err(AudioError::PathRejected {
            reason: format!(
                "resolved target is not a regular file: {}",
                resolved.display()
            ),
        });
    }

    if !policy.allowed_roots.is_empty() {
        let permitted = policy.allowed_roots.iter().any(|root| {
            std::fs::canonicalize(root)
                .is_ok_and(|canonical_root| resolved.starts_with(canonical_root))
        });
        if !permitted {
            return Err(AudioError::PathRejected {
                reason: format!(
                    "resolved target {} is outside every allowed root",
                    resolved.display()
                ),
            });
        }
    }

    let byte_length = metadata.len();
    if byte_length == 0 {
        return Err(AudioError::ZeroLength {
            path: resolved.clone(),
        });
    }
    if let Some(max_bytes) = policy.max_bytes {
        if byte_length > max_bytes {
            return Err(AudioError::AllocationLimitExceeded {
                requested: byte_length,
                limit: max_bytes,
            });
        }
    }

    // Confirm readability now rather than failing deep inside the decoder.
    std::fs::File::open(&resolved).map_err(|source| AudioError::PathRejected {
        reason: format!("cannot open {}: {source}", resolved.display()),
    })?;

    Ok(ResolvedMediaPath {
        resolved,
        supplied: supplied.to_path_buf(),
        byte_length,
        followed_symlink,
    })
}

fn has_traversal_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn resolves_a_plain_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.wav", b"RIFF----WAVE");
        let resolved = resolve_media_path(&path, &PathPolicy::user_selected()).unwrap();
        assert_eq!(resolved.byte_length(), 12);
        assert!(!resolved.followed_symlink());
        assert_eq!(resolved.supplied_extension(), "wav");
    }

    #[test]
    fn empty_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "empty.wav", b"");
        let error = resolve_media_path(&path, &PathPolicy::user_selected()).unwrap_err();
        assert_eq!(error.code(), "MEDIA_EMPTY");
    }

    #[test]
    fn directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let error = resolve_media_path(dir.path(), &PathPolicy::user_selected()).unwrap_err();
        assert_eq!(error.code(), "MEDIA_PATH_REJECTED");
    }

    #[test]
    fn traversal_component_is_rejected_before_any_io() {
        let error = resolve_media_path(
            Path::new("/tmp/../etc/passwd"),
            &PathPolicy::user_selected(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "MEDIA_PATH_REJECTED");
        assert!(format!("{error}").contains(".."));
    }

    #[cfg(unix)]
    #[test]
    fn valid_symlink_is_resolved_and_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_file(dir.path(), "real.wav", b"RIFF----WAVE");
        let link = dir.path().join("link.wav");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let resolved = resolve_media_path(&link, &PathPolicy::user_selected()).unwrap();
        assert!(resolved.followed_symlink());
        assert_eq!(
            resolved.path().file_name().unwrap().to_str().unwrap(),
            "real.wav"
        );
        assert_eq!(resolved.supplied_path(), link);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("dangling.wav");
        std::os::unix::fs::symlink(dir.path().join("missing.wav"), &link).unwrap();
        let error = resolve_media_path(&link, &PathPolicy::user_selected()).unwrap_err();
        assert_eq!(error.code(), "MEDIA_PATH_REJECTED");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_allowed_roots_is_rejected() {
        let outside = tempfile::tempdir().unwrap();
        let inside = tempfile::tempdir().unwrap();
        let target = write_file(outside.path(), "secret.wav", b"RIFF----WAVE");
        let link = inside.path().join("innocent.wav");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let policy = PathPolicy::confined_to(vec![inside.path().to_path_buf()]);
        let error = resolve_media_path(&link, &policy).unwrap_err();
        assert_eq!(error.code(), "MEDIA_PATH_REJECTED");
        assert!(format!("{error}").contains("outside every allowed root"));
    }

    #[test]
    fn oversized_file_is_rejected_by_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "big.wav", &vec![0u8; 1024]);
        let policy = PathPolicy {
            allowed_roots: Vec::new(),
            max_bytes: Some(512),
        };
        let error = resolve_media_path(&path, &policy).unwrap_err();
        assert_eq!(error.code(), "MEDIA_ALLOCATION_LIMIT");
    }

    #[test]
    fn hostile_names_are_sanitized_without_becoming_paths() {
        // Separators become underscores and the leading dot run is stripped,
        // so no traversal component survives.
        assert_eq!(sanitize_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_file_name("a\u{0}b.wav"), "a_b.wav");
        assert_eq!(sanitize_file_name("   "), "recording");
        assert_eq!(sanitize_file_name("..."), "recording");
        assert_eq!(sanitize_file_name("--rf"), "rf");
        assert_eq!(sanitize_file_name("CON.wav"), "_CON.wav");
        assert_eq!(sanitize_file_name("C:\\x\\y.wav"), "C__x_y.wav");
        assert_ne!(sanitize_file_name(&"x".repeat(5000)).len(), 0);
        assert!(sanitize_file_name(&"x".repeat(5000)).chars().count() <= 180);
    }

    #[test]
    fn sanitized_names_never_contain_separators() {
        for raw in [
            "../a",
            "a/../b",
            "a\\b",
            "/abs/path",
            "\u{202e}gnp.wav",
            "nul",
        ] {
            let sanitized = sanitize_file_name(raw);
            assert!(!sanitized.contains('/'), "{raw} -> {sanitized}");
            assert!(!sanitized.contains('\\'), "{raw} -> {sanitized}");
        }
    }
}
