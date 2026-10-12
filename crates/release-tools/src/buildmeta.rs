//! Reproducible build metadata (spec 18.8).
//!
//! Every CI run emits one of these documents. The release evidence bundle
//! (spec 24) consumes it, so a published artifact can always be traced to the
//! commit, toolchain and runner that produced it.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Schema version of the build metadata document.
pub const BUILD_METADATA_VERSION: u32 = 1;

/// Build provenance for one CI run on one runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildMetadata {
    /// Document schema version.
    pub schema_version: u32,
    /// Application version from the workspace manifest.
    pub app_version: String,
    /// Full commit SHA the artifacts were built from.
    pub commit: String,
    /// Whether the working tree had uncommitted changes. A dirty tree is
    /// recorded, never hidden; the release gate refuses dirty builds.
    pub dirty: bool,
    /// `rustc --version --verbose` first line.
    pub rustc: String,
    /// Pinned toolchain channel from `rust-toolchain.toml`.
    pub toolchain_channel: String,
    /// Target triple the artifacts were produced for.
    pub target_triple: String,
    /// Host operating system identifier (`macos`, `windows`).
    pub host_os: String,
    /// Host OS build string as reported by the runner.
    pub host_os_build: String,
    /// RFC 3339 build timestamp, or the value of `SOURCE_DATE_EPOCH` rendered
    /// as RFC 3339 when the caller pins it for reproducibility.
    pub built_at: String,
    /// Hashes of the artifacts produced by this run.
    pub artifacts: Vec<ArtifactHash>,
}

/// One produced artifact and its content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactHash {
    /// Path relative to the artifact staging directory.
    pub path: String,
    /// Size in bytes.
    pub bytes: u64,
    /// Lowercase hexadecimal SHA-256.
    pub sha256: String,
}

/// Errors raised while collecting build metadata.
#[derive(Debug, thiserror::Error)]
pub enum BuildMetadataError {
    /// A required command was unavailable or failed.
    #[error("{command} failed: {detail}")]
    Command {
        /// The command that failed.
        command: String,
        /// Why it failed.
        detail: String,
    },
    /// The repository layout did not contain an expected file.
    #[error("io error on {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A manifest could not be parsed.
    #[error("cannot parse {path}: {detail}")]
    Parse {
        /// Offending path.
        path: String,
        /// Parser message.
        detail: String,
    },
}

fn run(command: &str, args: &[&str], cwd: &Path) -> Result<String, BuildMetadataError> {
    let output = Command::new(command)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| BuildMetadataError::Command {
            command: command.to_owned(),
            detail: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(BuildMetadataError::Command {
            command: format!("{command} {}", args.join(" ")),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn read_to_string(path: &Path) -> Result<String, BuildMetadataError> {
    std::fs::read_to_string(path).map_err(|source| BuildMetadataError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn host_os_build() -> String {
    if cfg!(target_os = "macos") {
        Command::new("sw_vers")
            .arg("-buildVersion")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map_or_else(
                || "unknown".to_owned(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
            )
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "ver"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map_or_else(
                || "unknown".to_owned(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
            )
    } else {
        Command::new("uname")
            .arg("-sr")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map_or_else(
                || "unknown".to_owned(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
            )
    }
}

/// Resolves the build timestamp, honouring `SOURCE_DATE_EPOCH` so a rebuild of
/// the same commit produces the same document.
fn built_at() -> String {
    built_at_from(std::env::var("SOURCE_DATE_EPOCH").ok().as_deref())
}

fn built_at_from(source_date_epoch: Option<&str>) -> String {
    let now = source_date_epoch
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .and_then(|secs| time::OffsetDateTime::from_unix_timestamp(secs).ok())
        .unwrap_or_else(time::OffsetDateTime::now_utc);
    now.replace_nanosecond(0)
        .unwrap_or(now)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Collects build metadata for `repo_root`, hashing every file found under
/// `artifact_dir` when one is supplied.
pub fn collect(
    repo_root: &Path,
    artifact_dir: Option<&Path>,
) -> Result<BuildMetadata, BuildMetadataError> {
    let commit = run("git", &["rev-parse", "HEAD"], repo_root)?;
    let status = run("git", &["status", "--porcelain"], repo_root)?;
    let rustc = run("rustc", &["--version"], repo_root)?;
    let target_triple = run("rustc", &["-vV"], repo_root)?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());

    let toolchain_path = repo_root.join("rust-toolchain.toml");
    let toolchain: toml::Value = read_to_string(&toolchain_path)?.parse().map_err(|e| {
        BuildMetadataError::Parse {
            path: toolchain_path.display().to_string(),
            detail: format!("{e}"),
        }
    })?;
    let toolchain_channel = toolchain
        .get("toolchain")
        .and_then(|t| t.get("channel"))
        .and_then(toml::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    let manifest_path = repo_root.join("Cargo.toml");
    let manifest: toml::Value =
        read_to_string(&manifest_path)?
            .parse()
            .map_err(|e| BuildMetadataError::Parse {
                path: manifest_path.display().to_string(),
                detail: format!("{e}"),
            })?;
    let app_version = manifest
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    let artifacts = match artifact_dir {
        Some(dir) if dir.is_dir() => hash_tree(dir)?,
        _ => Vec::new(),
    };

    Ok(BuildMetadata {
        schema_version: BUILD_METADATA_VERSION,
        app_version,
        commit,
        dirty: !status.is_empty(),
        rustc,
        toolchain_channel,
        target_triple,
        host_os: std::env::consts::OS.to_owned(),
        host_os_build: host_os_build(),
        built_at: built_at(),
        artifacts,
    })
}

/// Hashes every file under `dir`, sorted by relative path for determinism.
pub fn hash_tree(dir: &Path) -> Result<Vec<ArtifactHash>, BuildMetadataError> {
    let mut artifacts = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.map_err(|e| BuildMetadataError::Io {
            path: dir.display().to_string(),
            source: e.into(),
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = entry.metadata().map_err(|e| BuildMetadataError::Io {
            path: entry.path().display().to_string(),
            source: e.into(),
        })?;
        let sha256 =
            crate::hash::sha256_file(entry.path()).map_err(|source| BuildMetadataError::Io {
                path: entry.path().display().to_string(),
                source,
            })?;
        artifacts.push(ArtifactHash {
            path: relative,
            bytes: metadata.len(),
            sha256,
        });
    }
    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_a_tree_is_sorted_and_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("nested")).expect("mkdir");
        std::fs::write(dir.path().join("b.txt"), b"beta").expect("write");
        std::fs::write(dir.path().join("a.txt"), b"alpha").expect("write");
        std::fs::write(dir.path().join("nested/c.txt"), b"gamma").expect("write");

        let first = hash_tree(dir.path()).expect("hash");
        let second = hash_tree(dir.path()).expect("hash");
        assert_eq!(first, second);
        let paths: Vec<&str> = first.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "b.txt", "nested/c.txt"]);
        assert_eq!(first[0].sha256, crate::hash::sha256_bytes(b"alpha"));
        assert_eq!(first[0].bytes, 5);
    }

    #[test]
    fn source_date_epoch_pins_the_timestamp() {
        assert_eq!(
            built_at_from(Some("1700000000")),
            "2023-11-14T22:13:20Z",
            "a pinned epoch must render deterministically for reproducible builds"
        );
    }

    #[test]
    fn an_unparsable_epoch_falls_back_to_the_clock_rather_than_a_fake_date() {
        let stamped = built_at_from(Some("not-a-number"));
        assert!(stamped.ends_with('Z'), "expected RFC 3339 UTC, got {stamped}");
        assert_ne!(stamped, "2023-11-14T22:13:20Z");
    }
}
