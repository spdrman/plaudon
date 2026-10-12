//! Release evidence bundle (spec 24, 18.8; task N2.3).
//!
//! Spec 24 is blunt: "A claimed supported configuration without its evidence
//! bundle is a release failure." So the bundle is assembled from CI artifacts
//! only, every entry carries a SHA-256, the manifest is signed, the whole
//! thing is scanned for secrets, and a `Supported` compatibility cell without
//! a matching entry fails the gate.
//!
//! Layout: CI drops artifacts into a staging directory, one subdirectory per
//! category id. The assembler walks it, so nothing can be added to the bundle
//! that was not produced by a job.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::sign::DetachedSignature;

/// Manifest schema version.
pub const MANIFEST_VERSION: u32 = 1;

/// Name of the signed manifest inside the bundle.
pub const MANIFEST_FILENAME: &str = "manifest.json";

/// Name of the detached manifest signature inside the bundle.
pub const SIGNATURE_FILENAME: &str = "manifest.sig.json";

/// One evidence category required by spec 24.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    /// Directory name under the staging root.
    pub id: &'static str,
    /// What spec 24 requires this category to contain.
    pub description: &'static str,
    /// Whether a release may proceed without it.
    pub required: bool,
}

/// Every category spec 24 enumerates, plus the two downstream teams feed in.
pub const CATEGORIES: &[Category] = &[
    Category {
        id: "build-metadata",
        description: "app, build and toolchain hashes per matrix leg (spec 24, 18.8)",
        required: true,
    },
    Category {
        id: "tested-os-versions",
        description: "OS builds the suites actually ran on (spec 24, 18.4)",
        required: true,
    },
    Category {
        id: "test-results",
        description: "unit, integration, chaos and E2E results (spec 18.8)",
        required: true,
    },
    Category {
        id: "sbom",
        description: "software bill of materials (spec 18.8)",
        required: true,
    },
    Category {
        id: "sca-license-audit",
        description: "advisory scan and OSS/model licence audit output (spec 18.8)",
        required: true,
    },
    Category {
        id: "risk-exceptions",
        description: "accepted risks with owner and expiry (spec 18.8)",
        required: true,
    },
    Category {
        id: "migration-restore",
        description: "database migration and restore results (spec 24)",
        required: true,
    },
    Category {
        id: "openrouter-contract-fixtures",
        description: "OpenRouter model-contract fixtures and capture date (spec 24)",
        required: true,
    },
    Category {
        id: "offline-model-artifacts",
        description: "offline model artifact checksum and licence (spec 24)",
        required: true,
    },
    Category {
        id: "security-findings",
        description: "security test findings and their disposition (spec 24)",
        required: true,
    },
    Category {
        id: "consent-egress-traces",
        description: "consent traces and Local-only Mode egress traces (spec 24, 22(15))",
        required: true,
    },
    Category {
        id: "device-compatibility",
        description: "every published PLAUD device compatibility record (spec 24)",
        required: true,
    },
    Category {
        id: "frozen-thresholds",
        description: "frozen numeric quality thresholds (spec 17.6)",
        required: true,
    },
    Category {
        id: "signing-verification",
        description: "post-build signature and notarization verification output (spec 16.5, N2.1)",
        required: true,
    },
    Category {
        id: "scenario-coverage",
        description: "spec 18.7 acceptance-scenario registry coverage (N1.2)",
        required: true,
    },
    Category {
        id: "coverage-report",
        description: "branch-coverage report and policy verdict (spec 18.9)",
        required: true,
    },
    Category {
        id: "benchmarks",
        description: "performance and scale results with machine details (spec 17.5)",
        required: true,
    },
    Category {
        id: "quality-metrics",
        description: "diarization and tool quality metrics (spec 17.3, 17.4)",
        required: false,
    },
    Category {
        id: "release-gate",
        description: "clean-machine install/update/rollback and restore verdict (spec 19 Phase 9)",
        required: true,
    },
];

/// One file in the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Category id this entry belongs to.
    pub category: String,
    /// Path relative to the bundle root.
    pub path: String,
    /// Size in bytes.
    pub bytes: u64,
    /// Lowercase hexadecimal SHA-256.
    pub sha256: String,
    /// Evidence id, used by compatibility cells to point at their proof.
    ///
    /// Derived from the file stem, so `device-compatibility/run-42.json`
    /// provides evidence id `run-42`.
    pub evidence_id: String,
}

/// The signed manifest over the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Commit the release was built from.
    pub commit: String,
    /// Application version.
    pub app_version: String,
    /// RFC 3339 assembly timestamp, honouring `SOURCE_DATE_EPOCH`.
    pub assembled_at: String,
    /// Every category and whether it was populated.
    pub categories: BTreeMap<String, CategoryStatus>,
    /// Every file, sorted by path.
    pub entries: Vec<Entry>,
}

/// Whether a category was populated and by how many files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryStatus {
    /// Spec description of the category.
    pub description: String,
    /// Whether a release may proceed without it.
    pub required: bool,
    /// Number of files present.
    pub file_count: usize,
}

/// Assembly, verification and gating failures.
#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    /// A required category had no files.
    #[error("evidence category `{id}` is empty: {description}")]
    MissingCategory {
        /// Category id.
        id: String,
        /// What it should have contained.
        description: String,
    },
    /// The staging directory contained a directory no category defines.
    #[error("staging directory contains unknown evidence category `{id}`")]
    UnknownCategory {
        /// Directory name found.
        id: String,
    },
    /// A file in the bundle appears to contain a secret.
    #[error("{path}:{line} looks like a secret ({pattern}); a release bundle must carry none")]
    SecretFound {
        /// Offending file.
        path: String,
        /// 1-based line.
        line: usize,
        /// Which pattern matched.
        pattern: String,
    },
    /// A manifest entry does not match the file on disk.
    #[error("{path}: {detail}")]
    Corrupt {
        /// Offending file.
        path: String,
        /// What differs.
        detail: String,
    },
    /// Input/output failure.
    #[error("io error on {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// Serialization failure.
    #[error("cannot parse {path}: {detail}")]
    Parse {
        /// Offending path.
        path: String,
        /// Parser message.
        detail: String,
    },
    /// Signing or signature verification failure.
    #[error(transparent)]
    Signature(#[from] crate::sign::SignError),
}

/// Literal patterns that must never appear in an evidence bundle (spec 16.4,
/// 24: no secrets, API keys, tokens or signed URLs).
pub const SECRET_PATTERNS: &[&str] = &[
    "sk-or-v1-",
    "sk-ant-",
    "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN OPENSSH PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
    "authorization: bearer ",
    "x-api-key:",
    "x-amz-signature=",
    "x-goog-signature=",
    "plaudon_release_signing_key=",
    "aws_secret_access_key",
    "?signature=",
    "&signature=",
];

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> EvidenceError + '_ {
    move |source| EvidenceError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Scans one text file for secret-looking content.
///
/// Binary files are skipped: a non-UTF-8 artifact cannot be line-scanned, and
/// pretending otherwise would produce a false sense of coverage. Binary
/// artifacts in the bundle are installers and model files, which carry their
/// own hash entry.
pub fn scan_for_secrets(path: &Path, relative: &str) -> Result<(), EvidenceError> {
    let bytes = std::fs::read(path).map_err(io(path))?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };
    for (index, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        for pattern in SECRET_PATTERNS {
            if lower.contains(pattern) {
                return Err(EvidenceError::SecretFound {
                    path: relative.to_owned(),
                    line: index + 1,
                    pattern: (*pattern).to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn evidence_id(relative: &str) -> String {
    Path::new(relative)
        .file_stem()
        .map_or_else(|| relative.to_owned(), |stem| stem.to_string_lossy().into_owned())
}

/// Assembles a manifest from a staging directory.
///
/// Every file under `staging/<category>/` becomes an entry. Nothing outside a
/// known category directory is accepted, so a stray file cannot smuggle itself
/// into a signed bundle.
pub fn assemble(
    staging: &Path,
    commit: &str,
    app_version: &str,
    assembled_at: &str,
) -> Result<Manifest, EvidenceError> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut categories: BTreeMap<String, CategoryStatus> = BTreeMap::new();

    let known: Vec<&str> = CATEGORIES.iter().map(|c| c.id).collect();
    for child in std::fs::read_dir(staging).map_err(io(staging))? {
        let child = child.map_err(io(staging))?;
        if !child.file_type().map_err(io(staging))?.is_dir() {
            continue;
        }
        let name = child.file_name().to_string_lossy().into_owned();
        if !known.contains(&name.as_str()) {
            return Err(EvidenceError::UnknownCategory { id: name });
        }
    }

    for category in CATEGORIES {
        let dir = staging.join(category.id);
        let mut count = 0_usize;
        if dir.is_dir() {
            for entry in walkdir::WalkDir::new(&dir).sort_by_file_name() {
                let entry = entry.map_err(|e| EvidenceError::Io {
                    path: dir.display().to_string(),
                    source: e.into(),
                })?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = format!(
                    "{}/{}",
                    category.id,
                    entry
                        .path()
                        .strip_prefix(&dir)
                        .unwrap_or(entry.path())
                        .to_string_lossy()
                        .replace('\\', "/")
                );
                scan_for_secrets(entry.path(), &relative)?;
                let metadata = entry.metadata().map_err(|e| EvidenceError::Io {
                    path: entry.path().display().to_string(),
                    source: e.into(),
                })?;
                entries.push(Entry {
                    category: category.id.to_owned(),
                    sha256: crate::hash::sha256_file(entry.path())
                        .map_err(io(entry.path()))?,
                    bytes: metadata.len(),
                    evidence_id: evidence_id(&relative),
                    path: relative,
                });
                count += 1;
            }
        }
        if category.required && count == 0 {
            return Err(EvidenceError::MissingCategory {
                id: category.id.to_owned(),
                description: category.description.to_owned(),
            });
        }
        categories.insert(
            category.id.to_owned(),
            CategoryStatus {
                description: category.description.to_owned(),
                required: category.required,
                file_count: count,
            },
        );
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Manifest {
        schema_version: MANIFEST_VERSION,
        commit: commit.to_owned(),
        app_version: app_version.to_owned(),
        assembled_at: assembled_at.to_owned(),
        categories,
        entries,
    })
}


/// Canonical bytes of a manifest: pretty JSON with a trailing newline, so the
/// signed bytes are reproducible and reviewable.
pub fn manifest_bytes(manifest: &Manifest) -> Vec<u8> {
    let mut json = serde_json::to_string_pretty(manifest).expect("manifest serializes");
    json.push('\n');
    json.into_bytes()
}

/// Writes the manifest and its detached signature into the staging directory,
/// turning it into a bundle.
pub fn write_bundle(
    staging: &Path,
    manifest: &Manifest,
    key: &ed25519_dalek::SigningKey,
) -> Result<DetachedSignature, EvidenceError> {
    let bytes = manifest_bytes(manifest);
    let signature = crate::sign::sign(key, &bytes);
    std::fs::write(staging.join(MANIFEST_FILENAME), &bytes)
        .map_err(io(&staging.join(MANIFEST_FILENAME)))?;
    let mut signature_json =
        serde_json::to_string_pretty(&signature).expect("signature serializes");
    signature_json.push('\n');
    std::fs::write(staging.join(SIGNATURE_FILENAME), signature_json)
        .map_err(io(&staging.join(SIGNATURE_FILENAME)))?;
    Ok(signature)
}

/// Verifies a bundle end to end: signature, then every entry's hash and size,
/// then a secret rescan, then required-category presence.
pub fn verify_bundle(bundle: &Path, trusted: &[String]) -> Result<Manifest, EvidenceError> {
    let manifest_path = bundle.join(MANIFEST_FILENAME);
    let bytes = std::fs::read(&manifest_path).map_err(io(&manifest_path))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| EvidenceError::Parse {
            path: manifest_path.display().to_string(),
            detail: e.to_string(),
        })?;

    let signature_path = bundle.join(SIGNATURE_FILENAME);
    let signature_bytes = std::fs::read(&signature_path).map_err(io(&signature_path))?;
    let signature: DetachedSignature =
        serde_json::from_slice(&signature_bytes).map_err(|e| EvidenceError::Parse {
            path: signature_path.display().to_string(),
            detail: e.to_string(),
        })?;
    crate::sign::verify(&signature, &bytes, trusted)?;

    for entry in &manifest.entries {
        let path = bundle.join(&entry.path);
        let metadata = std::fs::metadata(&path).map_err(io(&path))?;
        if metadata.len() != entry.bytes {
            return Err(EvidenceError::Corrupt {
                path: entry.path.clone(),
                detail: format!("size {} but manifest says {}", metadata.len(), entry.bytes),
            });
        }
        let sha256 = crate::hash::sha256_file(&path).map_err(io(&path))?;
        if sha256 != entry.sha256 {
            return Err(EvidenceError::Corrupt {
                path: entry.path.clone(),
                detail: "sha256 does not match the signed manifest".to_owned(),
            });
        }
        scan_for_secrets(&path, &entry.path)?;
    }

    for (id, status) in &manifest.categories {
        if status.required && status.file_count == 0 {
            return Err(EvidenceError::MissingCategory {
                id: id.clone(),
                description: status.description.clone(),
            });
        }
    }

    Ok(manifest)
}

/// Evidence ids a compatibility `Supported` cell may point at.
pub fn evidence_ids(manifest: &Manifest) -> Vec<String> {
    let mut ids: Vec<String> = manifest
        .entries
        .iter()
        .map(|entry| entry.evidence_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated_staging() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for category in CATEGORIES.iter().filter(|c| c.required) {
            let path = dir.path().join(category.id);
            std::fs::create_dir_all(&path).expect("mkdir");
            std::fs::write(path.join("record.json"), b"{\"ok\":true}\n").expect("write");
        }
        dir
    }

    fn key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[3_u8; 32])
    }

    #[test]
    fn a_complete_staging_directory_assembles_and_verifies() {
        let staging = populated_staging();
        let manifest = assemble(staging.path(), "abc123", "0.1.0", "2026-10-11T00:00:00Z")
            .expect("assembles");
        assert_eq!(
            manifest.entries.len(),
            CATEGORIES.iter().filter(|c| c.required).count()
        );
        let signature = write_bundle(staging.path(), &manifest, &key()).expect("writes");
        let verified =
            verify_bundle(staging.path(), &[signature.key_id.clone()]).expect("verifies");
        assert_eq!(verified, manifest);
    }

    #[test]
    fn a_missing_required_category_fails_assembly() {
        let staging = populated_staging();
        std::fs::remove_dir_all(staging.path().join("sbom")).expect("remove");
        let error = assemble(staging.path(), "abc", "0.1.0", "t").expect_err("must fail");
        assert!(matches!(error, EvidenceError::MissingCategory { ref id, .. } if id == "sbom"));
    }

    #[test]
    fn an_optional_category_may_be_absent() {
        let staging = populated_staging();
        let manifest = assemble(staging.path(), "abc", "0.1.0", "t").expect("assembles");
        assert_eq!(manifest.categories["quality-metrics"].file_count, 0);
    }

    #[test]
    fn an_unknown_staging_directory_is_refused() {
        let staging = populated_staging();
        std::fs::create_dir_all(staging.path().join("marketing-claims")).expect("mkdir");
        let error = assemble(staging.path(), "abc", "0.1.0", "t").expect_err("must fail");
        assert!(matches!(error, EvidenceError::UnknownCategory { .. }));
    }

    #[test]
    fn a_secret_bearing_artifact_fails_the_scan() {
        let staging = populated_staging();
        std::fs::write(
            staging.path().join("test-results/leak.log"),
            "request failed\nauthorization: Bearer sk-or-v1-deadbeef\n",
        )
        .expect("write");
        let error = assemble(staging.path(), "abc", "0.1.0", "t").expect_err("must fail");
        let EvidenceError::SecretFound { line, .. } = error else {
            panic!("expected a secret finding");
        };
        assert_eq!(line, 2);
    }

    #[test]
    fn a_signed_url_in_an_artifact_fails_the_scan() {
        let staging = populated_staging();
        std::fs::write(
            staging.path().join("test-results/url.log"),
            "https://cdn.example/x.wav?X-Amz-Signature=abc\n",
        )
        .expect("write");
        assert!(matches!(
            assemble(staging.path(), "abc", "0.1.0", "t"),
            Err(EvidenceError::SecretFound { .. })
        ));
    }

    #[test]
    fn a_tampered_entry_fails_verification() {
        let staging = populated_staging();
        let manifest = assemble(staging.path(), "abc", "0.1.0", "t").expect("assembles");
        let signature = write_bundle(staging.path(), &manifest, &key()).expect("writes");
        std::fs::write(staging.path().join("sbom/record.json"), b"{\"ok\":false}\n")
            .expect("write");
        let error =
            verify_bundle(staging.path(), &[signature.key_id.clone()]).expect_err("must fail");
        assert!(matches!(error, EvidenceError::Corrupt { .. }));
    }

    #[test]
    fn a_tampered_manifest_fails_signature_verification() {
        let staging = populated_staging();
        let manifest = assemble(staging.path(), "abc", "0.1.0", "t").expect("assembles");
        let signature = write_bundle(staging.path(), &manifest, &key()).expect("writes");
        let mut tampered = manifest.clone();
        tampered.commit = "forged".to_owned();
        std::fs::write(
            staging.path().join(MANIFEST_FILENAME),
            manifest_bytes(&tampered),
        )
        .expect("write");
        let error =
            verify_bundle(staging.path(), &[signature.key_id.clone()]).expect_err("must fail");
        assert!(matches!(
            error,
            EvidenceError::Signature(crate::sign::SignError::SignatureMismatch { .. })
        ));
    }

    #[test]
    fn a_bundle_signed_by_an_untrusted_key_is_rejected() {
        let staging = populated_staging();
        let manifest = assemble(staging.path(), "abc", "0.1.0", "t").expect("assembles");
        write_bundle(staging.path(), &manifest, &key()).expect("writes");
        let error = verify_bundle(staging.path(), &["0000000000000000".to_owned()])
            .expect_err("must fail");
        assert!(matches!(
            error,
            EvidenceError::Signature(crate::sign::SignError::UnknownKey { .. })
        ));
    }

    #[test]
    fn assembly_is_reproducible_for_the_same_inputs() {
        let staging = populated_staging();
        let first = assemble(staging.path(), "abc", "0.1.0", "t").expect("assembles");
        let second = assemble(staging.path(), "abc", "0.1.0", "t").expect("assembles");
        assert_eq!(manifest_bytes(&first), manifest_bytes(&second));
    }

    #[test]
    fn evidence_ids_come_from_file_stems() {
        let staging = populated_staging();
        std::fs::write(
            staging.path().join("device-compatibility/run-42.json"),
            b"{}\n",
        )
        .expect("write");
        let manifest = assemble(staging.path(), "abc", "0.1.0", "t").expect("assembles");
        assert!(evidence_ids(&manifest).contains(&"run-42".to_owned()));
    }
}
