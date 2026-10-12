//! Trusted model manifest (spec §12 — task F2.2).
//!
//! §12 requires the manager to list "sizes, languages, licenses, hardware
//! recommendations, checksums, version, source and installation progress", and
//! that models are "verified against a trusted manifest". Everything a
//! manifest claims is validated here *before* any byte is fetched or written,
//! because a manifest is the attacker's cheapest way into the install path:
//! a hostile `file_name` becomes a path traversal, a hostile `size_bytes`
//! becomes a disk-filling denial of service.

use std::collections::BTreeSet;

use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::Sha256Digest;
use serde::{Deserialize, Serialize};

/// Manifest schema version this build understands.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Hard ceiling on a declared model size. Larger than any whisper.cpp weight
/// that exists, small enough that a forged manifest cannot ask us to reserve
/// an absurd amount of disk.
pub const MAX_DECLARED_MODEL_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// A validated model identifier.
///
/// Constrained to a conservative character set so it can be used as a path
/// component without further escaping.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelId(String);

impl ModelId {
    /// Parse and validate an identifier.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ManifestError> {
        let raw = raw.into();
        if raw.is_empty() || raw.len() > 64 {
            return Err(ManifestError::InvalidIdentifier { value: raw });
        }
        let first_is_alphanumeric = raw
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let rest_is_safe = raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'));
        if !first_is_alphanumeric || !rest_is_safe || raw.contains("..") {
            return Err(ManifestError::InvalidIdentifier { value: raw });
        }
        Ok(Self(raw))
    }

    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ModelId {
    type Error = ManifestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ModelId> for String {
    fn from(value: ModelId) -> Self {
        value.0
    }
}

/// Where a model's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ModelSource {
    /// Fetched over the network. Always subject to [`crate::models::ModelFetchPolicy`].
    Remote {
        /// HTTPS URL of the weights.
        url: String,
    },
    /// Supplied by a redistributable offline pack (F2.3). Never networked.
    Pack {
        /// Path of the entry inside the pack, relative to the pack root.
        entry_path: String,
    },
}

impl ModelSource {
    /// Whether obtaining this source requires network egress.
    #[must_use]
    pub const fn requires_network(&self) -> bool {
        matches!(self, ModelSource::Remote { .. })
    }
}

/// Licence facts shown in the manager and audited at pack-build time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLicense {
    /// SPDX identifier, e.g. `MIT`.
    pub spdx_id: String,
    /// Human-readable licence name.
    pub name: String,
    /// Whether the licence permits us to redistribute the weights inside an
    /// offline pack (F2.3). `false` means "download only".
    pub redistributable: bool,
    /// Leaf file name of the licence text shipped alongside the weights.
    pub text_file: String,
}

/// Hardware guidance shown before install (§12, §14.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareRecommendation {
    /// Below this, installation is refused.
    pub min_ram_bytes: u64,
    /// Below this, installation warns but proceeds.
    pub recommended_ram_bytes: u64,
    /// Short non-secret note, e.g. "runs in real time on Apple Silicon".
    pub notes: String,
}

/// One model the manifest offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    /// Stable identifier, unique within the manifest.
    pub id: ModelId,
    /// Engine that can load these weights, e.g. `whisper.cpp`.
    pub engine: String,
    /// Weight version; part of the on-disk path (§13.4).
    pub version: String,
    /// Name shown in the manager.
    pub display_name: String,
    /// Leaf file name the weights are stored under.
    pub file_name: String,
    /// Exact size in bytes.
    pub size_bytes: u64,
    /// Expected digest of the weights.
    pub sha256: Sha256Digest,
    /// Languages the model covers. Empty means "not stated", which the UI must
    /// show as unknown rather than as "all".
    #[serde(default)]
    pub languages: Vec<LanguageTag>,
    /// Licence facts.
    pub license: ModelLicense,
    /// Where the bytes come from.
    pub source: ModelSource,
    /// Hardware guidance.
    pub hardware: HardwareRecommendation,
}

impl ModelEntry {
    /// Directory this model installs into, relative to the model root.
    ///
    /// Matches the §13.4 layout `models/<engine>/<model-version>/`.
    #[must_use]
    pub fn relative_dir(&self) -> String {
        format!("{}/{}", self.engine, self.version)
    }

    /// Path of the weights relative to the model root.
    #[must_use]
    pub fn relative_path(&self) -> String {
        format!("{}/{}", self.relative_dir(), self.file_name)
    }
}

/// The trusted manifest document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelManifest {
    /// Schema version of this document.
    pub schema_version: u32,
    /// Models on offer.
    pub models: Vec<ModelEntry>,
}

impl ModelManifest {
    /// Look up an entry by identifier.
    #[must_use]
    pub fn find(&self, id: &ModelId) -> Option<&ModelEntry> {
        self.models.iter().find(|entry| &entry.id == id)
    }
}

/// Why a manifest was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// The document is not valid JSON, or does not match the schema.
    #[error("manifest could not be parsed: {0}")]
    Malformed(String),

    /// The schema version is newer or older than this build understands.
    #[error(
        "manifest schema version {found} is not supported (this build understands {supported})"
    )]
    UnsupportedSchemaVersion {
        /// Version the document declares.
        found: u32,
        /// Version this build implements.
        supported: u32,
    },

    /// An identifier is not in the safe character set.
    #[error("`{value}` is not a valid identifier")]
    InvalidIdentifier {
        /// The offending value.
        value: String,
    },

    /// Two entries share an identifier.
    #[error("model id `{id}` appears more than once")]
    DuplicateId {
        /// The repeated identifier.
        id: String,
    },

    /// A file name is not a safe leaf name.
    #[error("`{value}` is not a safe file name: {reason}")]
    UnsafeFileName {
        /// The offending value.
        value: String,
        /// Why it was refused.
        reason: &'static str,
    },

    /// A remote source is not an acceptable HTTPS URL.
    #[error("`{url}` is not an acceptable source URL: {reason}")]
    UnsafeSourceUrl {
        /// The offending URL.
        url: String,
        /// Why it was refused.
        reason: &'static str,
    },

    /// A declared size exceeds the hard ceiling.
    #[error("declared size {declared} bytes exceeds the {limit} byte ceiling")]
    DeclaredSizeTooLarge {
        /// Size the manifest declared.
        declared: u64,
        /// The enforced ceiling.
        limit: u64,
    },

    /// A declared size is zero.
    #[error("model `{id}` declares a zero size")]
    ZeroSize {
        /// The offending identifier.
        id: String,
    },
}

/// Parse and fully validate a manifest document.
pub fn parse_manifest(bytes: &[u8]) -> Result<ModelManifest, ManifestError> {
    let manifest: ModelManifest = serde_json::from_slice(bytes)
        .map_err(|error| ManifestError::Malformed(error.to_string()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Validate an already-deserialised manifest.
pub fn validate_manifest(manifest: &ModelManifest) -> Result<(), ManifestError> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(ManifestError::UnsupportedSchemaVersion {
            found: manifest.schema_version,
            supported: MANIFEST_SCHEMA_VERSION,
        });
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for entry in &manifest.models {
        if !seen.insert(entry.id.as_str()) {
            return Err(ManifestError::DuplicateId {
                id: entry.id.to_string(),
            });
        }
        // `engine` and `version` become path components, so they get the same
        // treatment as the identifier.
        ModelId::parse(entry.engine.clone())?;
        ModelId::parse(entry.version.clone())?;
        validate_leaf_name(&entry.file_name)?;
        validate_leaf_name(&entry.license.text_file)?;

        if entry.size_bytes == 0 {
            return Err(ManifestError::ZeroSize {
                id: entry.id.to_string(),
            });
        }
        if entry.size_bytes > MAX_DECLARED_MODEL_BYTES {
            return Err(ManifestError::DeclaredSizeTooLarge {
                declared: entry.size_bytes,
                limit: MAX_DECLARED_MODEL_BYTES,
            });
        }

        match &entry.source {
            ModelSource::Remote { url } => validate_source_url(url)?,
            ModelSource::Pack { entry_path } => validate_relative_path(entry_path)?,
        }
    }
    Ok(())
}

/// A safe leaf file name: no separators, no traversal, no control characters.
pub(crate) fn validate_leaf_name(value: &str) -> Result<(), ManifestError> {
    let unsafe_reason = if value.is_empty() {
        Some("empty")
    } else if value.len() > 128 {
        Some("too long")
    } else if value.contains('/') || value.contains('\\') {
        Some("contains a path separator")
    } else if value == "." || value == ".." || value.contains("..") {
        Some("contains a traversal component")
    } else if value.starts_with('.') {
        Some("is a dotfile")
    } else if value.chars().any(char::is_control) {
        Some("contains a control character")
    } else if value.contains(':') {
        Some("contains a drive or stream separator")
    } else {
        None
    };
    match unsafe_reason {
        Some(reason) => Err(ManifestError::UnsafeFileName {
            value: value.to_string(),
            reason,
        }),
        None => Ok(()),
    }
}

/// A safe relative path inside a pack: every component is a safe leaf name.
pub(crate) fn validate_relative_path(value: &str) -> Result<(), ManifestError> {
    if value.is_empty() {
        return Err(ManifestError::UnsafeFileName {
            value: value.to_string(),
            reason: "empty",
        });
    }
    if value.starts_with('/') || value.starts_with('\\') {
        return Err(ManifestError::UnsafeFileName {
            value: value.to_string(),
            reason: "is absolute",
        });
    }
    for component in value.split(['/', '\\']) {
        validate_leaf_name(component)?;
    }
    Ok(())
}

/// Only plain HTTPS origins are acceptable sources.
fn validate_source_url(url: &str) -> Result<(), ManifestError> {
    let reject = |reason: &'static str| ManifestError::UnsafeSourceUrl {
        url: url.to_string(),
        reason,
    };
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(reject("scheme must be https"));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(reject("host is empty"));
    }
    if authority.contains('@') {
        return Err(reject("credentials in the URL are not permitted"));
    }
    let host = authority.split(':').next().unwrap_or(authority);
    if host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok() {
        return Err(reject(
            "host must be a public DNS name, not an address literal",
        ));
    }
    if url.chars().any(char::is_control) {
        return Err(reject("contains a control character"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> ModelEntry {
        ModelEntry {
            id: ModelId::parse("whisper-tiny-en").expect("id"),
            engine: "whisper.cpp".into(),
            version: "v1".into(),
            display_name: "Whisper Tiny (English)".into(),
            file_name: "ggml-tiny.en.bin".into(),
            size_bytes: 77_704_715,
            sha256: Sha256Digest::of(b"weights"),
            languages: vec![LanguageTag::parse("en").expect("tag")],
            license: ModelLicense {
                spdx_id: "MIT".into(),
                name: "MIT License".into(),
                redistributable: true,
                text_file: "MIT.txt".into(),
            },
            source: ModelSource::Remote {
                url: "https://example.org/ggml-tiny.en.bin".into(),
            },
            hardware: HardwareRecommendation {
                min_ram_bytes: 512 * 1024 * 1024,
                recommended_ram_bytes: 2 * 1024 * 1024 * 1024,
                notes: "runs in real time on a modern laptop CPU".into(),
            },
        }
    }

    fn manifest(models: Vec<ModelEntry>) -> ModelManifest {
        ModelManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            models,
        }
    }

    #[test]
    fn a_well_formed_manifest_validates() {
        assert_eq!(validate_manifest(&manifest(vec![entry()])), Ok(()));
    }

    #[test]
    fn every_required_field_round_trips_through_json() {
        let document = serde_json::to_vec(&manifest(vec![entry()])).expect("serialise");
        let parsed = parse_manifest(&document).expect("parse");
        let model = &parsed.models[0];
        // §12 requires all of these to be displayable.
        assert_eq!(model.size_bytes, 77_704_715);
        assert_eq!(model.languages.len(), 1);
        assert_eq!(model.license.spdx_id, "MIT");
        assert_eq!(model.sha256, Sha256Digest::of(b"weights"));
        assert_eq!(model.version, "v1");
        assert!(matches!(model.source, ModelSource::Remote { .. }));
        assert_eq!(model.hardware.min_ram_bytes, 512 * 1024 * 1024);
    }

    #[test]
    fn an_unknown_schema_version_is_rejected() {
        let mut document = manifest(vec![entry()]);
        document.schema_version = MANIFEST_SCHEMA_VERSION + 1;
        assert_eq!(
            validate_manifest(&document),
            Err(ManifestError::UnsupportedSchemaVersion {
                found: MANIFEST_SCHEMA_VERSION + 1,
                supported: MANIFEST_SCHEMA_VERSION,
            })
        );
    }

    #[test]
    fn duplicate_identifiers_are_rejected() {
        let document = manifest(vec![entry(), entry()]);
        assert_eq!(
            validate_manifest(&document),
            Err(ManifestError::DuplicateId {
                id: "whisper-tiny-en".into()
            })
        );
    }

    #[test]
    fn hostile_identifiers_are_rejected() {
        for hostile in ["../escape", "/abs", "a/b", "..", "", "UPPER", "a b", "ß"] {
            assert!(
                ModelId::parse(hostile).is_err(),
                "`{hostile}` should not be a valid identifier"
            );
        }
        assert!(ModelId::parse("whisper-large-v3.q5_0").is_ok());
    }

    #[test]
    fn hostile_file_names_are_rejected() {
        for hostile in [
            "../../etc/passwd",
            "/etc/passwd",
            "a/b.bin",
            "a\\b.bin",
            "..",
            ".hidden",
            "C:stream",
            "",
        ] {
            let mut document = manifest(vec![entry()]);
            document.models[0].file_name = hostile.into();
            assert!(
                matches!(
                    validate_manifest(&document),
                    Err(ManifestError::UnsafeFileName { .. })
                ),
                "`{hostile}` should be rejected"
            );
        }
    }

    #[test]
    fn a_hostile_licence_file_name_is_rejected() {
        let mut document = manifest(vec![entry()]);
        document.models[0].license.text_file = "../../etc/shadow".into();
        assert!(matches!(
            validate_manifest(&document),
            Err(ManifestError::UnsafeFileName { .. })
        ));
    }

    #[test]
    fn non_https_and_credentialled_urls_are_rejected() {
        for hostile in [
            "http://example.org/model.bin",
            "file:///etc/passwd",
            "https://user:pass@example.org/m.bin",
            "https://127.0.0.1/m.bin",
            "https://localhost/m.bin",
            "https:///m.bin",
            "ftp://example.org/m.bin",
        ] {
            let mut document = manifest(vec![entry()]);
            document.models[0].source = ModelSource::Remote {
                url: hostile.into(),
            };
            assert!(
                matches!(
                    validate_manifest(&document),
                    Err(ManifestError::UnsafeSourceUrl { .. })
                ),
                "`{hostile}` should be rejected"
            );
        }
    }

    #[test]
    fn an_oversized_declared_size_is_rejected() {
        let mut document = manifest(vec![entry()]);
        document.models[0].size_bytes = MAX_DECLARED_MODEL_BYTES + 1;
        assert!(matches!(
            validate_manifest(&document),
            Err(ManifestError::DeclaredSizeTooLarge { .. })
        ));
    }

    #[test]
    fn a_zero_declared_size_is_rejected() {
        let mut document = manifest(vec![entry()]);
        document.models[0].size_bytes = 0;
        assert!(matches!(
            validate_manifest(&document),
            Err(ManifestError::ZeroSize { .. })
        ));
    }

    #[test]
    fn a_traversal_pack_entry_path_is_rejected() {
        for hostile in ["../outside.bin", "/abs.bin", "models/../../x.bin", ""] {
            let mut document = manifest(vec![entry()]);
            document.models[0].source = ModelSource::Pack {
                entry_path: hostile.into(),
            };
            assert!(
                matches!(
                    validate_manifest(&document),
                    Err(ManifestError::UnsafeFileName { .. })
                ),
                "`{hostile}` should be rejected"
            );
        }
        let mut document = manifest(vec![entry()]);
        document.models[0].source = ModelSource::Pack {
            entry_path: "models/whisper.cpp/v1/ggml-tiny.en.bin".into(),
        };
        assert_eq!(validate_manifest(&document), Ok(()));
    }

    #[test]
    fn malformed_json_is_a_typed_error() {
        assert!(matches!(
            parse_manifest(b"{not json"),
            Err(ManifestError::Malformed(_))
        ));
    }

    #[test]
    fn install_paths_follow_the_spec_13_4_layout() {
        let entry = entry();
        assert_eq!(entry.relative_dir(), "whisper.cpp/v1");
        assert_eq!(entry.relative_path(), "whisper.cpp/v1/ggml-tiny.en.bin");
    }

    #[test]
    fn pack_sources_never_require_network() {
        assert!(!ModelSource::Pack {
            entry_path: "a.bin".into()
        }
        .requires_network());
        assert!(ModelSource::Remote {
            url: "https://example.org/a.bin".into()
        }
        .requires_network());
    }
}
