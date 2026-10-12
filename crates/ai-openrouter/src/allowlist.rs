//! The pinned candidate list shipped with the build (spec 10.1, 10.2).
//!
//! Being pinned means exactly one thing: our wire-contract fixtures cover the
//! request family named in [`AllowlistEntry::candidate_family`] for this model
//! id, so a request for it can be *built*. It is not a verification claim. A
//! model becomes `Verified` only when an observed exchange is recorded
//! ([`crate::registry::ModelRegistry::apply_probe`]), which is what
//! "`Verified` means passing our actual capability smoke test on that API
//! contract, not appearing in a model search response" requires.
//!
//! The list is also the degraded state: when catalog discovery fails we fall
//! back to exactly these entries rather than to an empty or invented list.

use plaudon_ai_contracts::capabilities::RequestFamily;
use serde::{Deserialize, Serialize};

/// The embedded manifest, versioned alongside the crate.
const MANIFEST_JSON: &str = include_str!("../allowlist/v1.json");

/// Schema version this build understands.
pub const ALLOWLIST_SCHEMA_VERSION: u32 = 1;

/// Which stage a pinned model is a candidate for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTask {
    /// Speech-to-text.
    #[default]
    Transcription,
    /// Summaries, extraction and transcript tools.
    TextGeneration,
}

impl ModelTask {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ModelTask::Transcription => "transcription",
            ModelTask::TextGeneration => "text_generation",
        }
    }
}

/// One pinned candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    /// Provider-scoped model identifier.
    pub model_id: String,
    /// Name shown in the picker.
    pub display_name: String,
    /// Stage this entry is pinned for.
    #[serde(skip)]
    pub task: ModelTask,
    /// The family our fixtures cover for this model. A *candidate*, not an
    /// observation.
    pub candidate_family: RequestFamily,
    /// Identifier of the wire fixture backing that family.
    pub fixture_id: String,
    /// Why this entry is on the list. Never a capability claim.
    #[serde(default)]
    pub notes: String,
}

/// The parsed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist {
    schema_version: u32,
    pinned_on: String,
    entries: Vec<AllowlistEntry>,
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    schema_version: u32,
    pinned_on: String,
    #[serde(default)]
    transcription: Vec<AllowlistEntry>,
    #[serde(default)]
    text_generation: Vec<AllowlistEntry>,
}

/// Why the embedded manifest could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AllowlistError {
    /// The manifest did not parse.
    #[error("the pinned allowlist manifest could not be parsed")]
    Malformed {
        /// Parser detail. Contains no user data.
        detail: String,
    },
    /// The manifest is from a future build.
    #[error("the pinned allowlist manifest is schema version {found}, this build understands {ALLOWLIST_SCHEMA_VERSION}")]
    UnsupportedVersion {
        /// Version found in the file.
        found: u32,
    },
}

impl Allowlist {
    /// Parse the manifest compiled into this build.
    ///
    /// # Errors
    ///
    /// Returns [`AllowlistError`] when the embedded manifest is malformed or
    /// declares an unsupported schema version. Both are build defects; the
    /// caller fails closed with an empty catalog rather than inventing models.
    pub fn embedded() -> Result<Self, AllowlistError> {
        Self::parse(MANIFEST_JSON)
    }

    /// Parse a manifest from JSON.
    ///
    /// # Errors
    ///
    /// See [`Allowlist::embedded`].
    pub fn parse(raw: &str) -> Result<Self, AllowlistError> {
        let file: ManifestFile =
            serde_json::from_str(raw).map_err(|error| AllowlistError::Malformed {
                detail: error.to_string(),
            })?;
        if file.schema_version != ALLOWLIST_SCHEMA_VERSION {
            return Err(AllowlistError::UnsupportedVersion {
                found: file.schema_version,
            });
        }
        let mut entries = Vec::with_capacity(file.transcription.len() + file.text_generation.len());
        for mut entry in file.transcription {
            entry.task = ModelTask::Transcription;
            entries.push(entry);
        }
        for mut entry in file.text_generation {
            entry.task = ModelTask::TextGeneration;
            entries.push(entry);
        }
        Ok(Self {
            schema_version: file.schema_version,
            pinned_on: file.pinned_on,
            entries,
        })
    }

    /// Schema version of the parsed manifest.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Date the list was pinned, as `YYYY-MM-DD`.
    #[must_use]
    pub fn pinned_on(&self) -> &str {
        &self.pinned_on
    }

    /// Every entry.
    #[must_use]
    pub fn entries(&self) -> &[AllowlistEntry] {
        &self.entries
    }

    /// Entries pinned for one stage.
    pub fn for_task(&self, task: ModelTask) -> impl Iterator<Item = &AllowlistEntry> {
        self.entries.iter().filter(move |entry| entry.task == task)
    }

    /// The entry for one model and task, if it is pinned.
    #[must_use]
    pub fn entry(&self, model_id: &str, task: ModelTask) -> Option<&AllowlistEntry> {
        self.entries
            .iter()
            .find(|entry| entry.model_id == model_id && entry.task == task)
    }

    /// Whether a model id is pinned for a task.
    #[must_use]
    pub fn contains(&self, model_id: &str, task: ModelTask) -> bool {
        self.entry(model_id, task).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_manifest_parses_and_is_non_empty() {
        let list = Allowlist::embedded().expect("manifest parses");
        assert_eq!(list.schema_version(), ALLOWLIST_SCHEMA_VERSION);
        assert!(list.for_task(ModelTask::Transcription).count() >= 3);
        assert!(list.for_task(ModelTask::TextGeneration).count() >= 1);
    }

    #[test]
    fn every_pinned_entry_names_a_fixture_that_exists() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repository root")
            .join("tests/fixtures/openrouter");
        for entry in Allowlist::embedded().expect("manifest").entries() {
            let path = fixtures.join(format!("{}.json", entry.fixture_id));
            assert!(
                path.exists(),
                "{} names fixture {} which does not exist",
                entry.model_id,
                entry.fixture_id
            );
        }
    }

    #[test]
    fn transcription_entries_only_claim_families_that_have_a_builder() {
        for entry in Allowlist::embedded()
            .expect("manifest")
            .for_task(ModelTask::Transcription)
        {
            assert!(
                entry.candidate_family.has_builder(),
                "{} pinned with a family that cannot be built",
                entry.model_id
            );
        }
    }

    #[test]
    fn a_future_schema_version_is_refused_rather_than_guessed() {
        let error = Allowlist::parse(r#"{"schema_version":99,"pinned_on":"2026-01-01"}"#)
            .expect_err("refused");
        assert_eq!(error, AllowlistError::UnsupportedVersion { found: 99 });
    }

    #[test]
    fn a_malformed_manifest_is_an_error_not_an_empty_list() {
        assert!(matches!(
            Allowlist::parse("{"),
            Err(AllowlistError::Malformed { .. })
        ));
    }
}
