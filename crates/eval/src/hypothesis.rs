//! Reference and hypothesis transcript documents consumed by the runner.
//!
//! The runner is an offline scorer (spec 17.2, 17.6): it reads transcripts a
//! separate process already produced and never executes an engine, opens
//! audio, or makes a network call. That separation is what makes an evaluation
//! reproducible from committed artefacts months later.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plaudon_domain::transcript::AlignmentOrigin;
use serde::{Deserialize, Serialize};

use crate::metrics::{RuntimeMeasurement, TimeSpanMs, TimedWord};

/// Schema version of the transcript documents this build reads.
pub const SUPPORTED_TRANSCRIPT_SCHEMA_VERSION: u32 = 1;

/// Where a hypothesis' times came from, in evaluation vocabulary.
///
/// This mirrors [`plaudon_domain::transcript::AlignmentOrigin`]. The domain
/// calls the no-times case `None`; evaluation reports call it `unavailable`,
/// because a report reader must not confuse "no alignment was produced" with
/// "the alignment measured zero error". The conversion is total, so a producer
/// can emit either vocabulary and still be scored identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisAlignment {
    /// Offsets reported by the engine itself.
    Native,
    /// Offsets produced by a separate, labelled alignment pass.
    Derived,
    /// No offsets exist. Timestamp error is not computable.
    #[serde(alias = "none")]
    Unavailable,
}

impl HypothesisAlignment {
    /// Whether timestamp-derived metrics may be computed at all.
    #[must_use]
    pub const fn carries_times(self) -> bool {
        matches!(self, Self::Native | Self::Derived)
    }

    /// The exact token used in reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Derived => "derived",
            Self::Unavailable => "unavailable",
        }
    }
}

impl From<AlignmentOrigin> for HypothesisAlignment {
    fn from(origin: AlignmentOrigin) -> Self {
        match origin {
            AlignmentOrigin::Native => Self::Native,
            AlignmentOrigin::Derived => Self::Derived,
            AlignmentOrigin::None => Self::Unavailable,
        }
    }
}

/// Where a hypothesis was produced, which decides whether the cloud opt-in
/// applies (spec 17.6, 4.x cost control).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deployment {
    /// Produced by an on-device engine.
    Local,
    /// Produced by a metered remote provider.
    Cloud,
}

impl Deployment {
    /// The exact token used in reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }
}

/// The human-verified transcript an item is scored against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceTranscript {
    /// Document schema version.
    pub schema_version: u32,
    /// Corpus item this reference belongs to.
    pub item_id: String,
    /// Verbatim reference text.
    pub text: String,
    /// Domain vocabulary that must survive transcription (spec 17.2).
    #[serde(default)]
    pub domain_terms: Vec<String>,
    /// Spans the reference marks as containing no speech.
    #[serde(default)]
    pub silence_spans_ms: Vec<TimeSpanMs>,
    /// Reference segment boundaries, in milliseconds.
    #[serde(default)]
    pub segment_boundaries_ms: Vec<u64>,
    /// How this reference was produced and checked.
    pub quality_note: String,
}

/// One engine's output for one corpus item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HypothesisDocument {
    /// Document schema version.
    pub schema_version: u32,
    /// Corpus item this hypothesis transcribes.
    pub item_id: String,
    /// Engine that produced it, for example `whisper.cpp`.
    pub engine_id: String,
    /// Model identifier inside that engine.
    pub model_id: String,
    /// Model revision, when the producer recorded one.
    #[serde(default)]
    pub model_version: Option<String>,
    /// Local or metered-cloud production.
    pub deployment: Deployment,
    /// Alignment origin of the times below.
    pub alignment_origin: HypothesisAlignment,
    /// Transcript text.
    pub text: String,
    /// Word-level times, when the engine produced them.
    #[serde(default)]
    pub words: Vec<TimedWord>,
    /// Segment boundaries, in milliseconds.
    #[serde(default)]
    pub segment_boundaries_ms: Vec<u64>,
    /// Runtime figures recorded by the producing process.
    #[serde(default)]
    pub runtime: Option<RuntimeMeasurement>,
}

impl HypothesisDocument {
    /// Word times, but only when the declared alignment permits them.
    ///
    /// A document that claims `unavailable` and still carries times is scored
    /// as if it had none: the declared origin is authoritative (spec 8.6A).
    #[must_use]
    pub fn timed_words(&self) -> &[TimedWord] {
        if self.alignment_origin.carries_times() {
            &self.words
        } else {
            &[]
        }
    }
}

/// A transcript document that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Unreadable {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The document is not valid JSON for its type.
    #[error("cannot parse {path}: {source}")]
    Unparseable {
        /// Path that failed.
        path: PathBuf,
        /// Underlying parse error.
        source: serde_json::Error,
    },
    /// The document declares a schema version this build does not read.
    #[error("{path} declares schema version {found}, this build reads {supported}")]
    UnsupportedSchemaVersion {
        /// Path that failed.
        path: PathBuf,
        /// Version found in the document.
        found: u32,
        /// Version this build reads.
        supported: u32,
    },
    /// Two documents claim the same item for the same engine.
    #[error("duplicate hypothesis for item `{item_id}` and engine `{engine_id}` in {path}")]
    DuplicateHypothesis {
        /// Item claimed twice.
        item_id: String,
        /// Engine claimed twice.
        engine_id: String,
        /// Path of the second document.
        path: PathBuf,
    },
    /// The hypothesis path names neither a file nor a directory.
    #[error("hypothesis path {0} is neither a file nor a directory")]
    NotFound(PathBuf),
}

impl ReferenceTranscript {
    /// Read and version-check a reference transcript.
    pub fn load(path: &Path) -> Result<Self, TranscriptError> {
        let document: Self = read_json(path)?;
        check_version(path, document.schema_version)?;
        Ok(document)
    }
}

impl HypothesisDocument {
    /// Read and version-check a hypothesis document.
    pub fn load(path: &Path) -> Result<Self, TranscriptError> {
        let document: Self = read_json(path)?;
        check_version(path, document.schema_version)?;
        Ok(document)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, TranscriptError> {
    let text = std::fs::read_to_string(path).map_err(|source| TranscriptError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| TranscriptError::Unparseable {
        path: path.to_path_buf(),
        source,
    })
}

fn check_version(path: &Path, found: u32) -> Result<(), TranscriptError> {
    if found == SUPPORTED_TRANSCRIPT_SCHEMA_VERSION {
        return Ok(());
    }
    Err(TranscriptError::UnsupportedSchemaVersion {
        path: path.to_path_buf(),
        found,
        supported: SUPPORTED_TRANSCRIPT_SCHEMA_VERSION,
    })
}

/// Every hypothesis document a run was handed, keyed by item and engine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HypothesisSet {
    documents: BTreeMap<(String, String), HypothesisDocument>,
}

impl HypothesisSet {
    /// Load one document, or every `*.json` document in a directory.
    ///
    /// Directory entries are visited in sorted order so that a run over the
    /// same inputs produces byte-identical output.
    pub fn load(path: &Path) -> Result<Self, TranscriptError> {
        let mut set = Self::default();
        if path.is_file() {
            set.insert(HypothesisDocument::load(path)?, path)?;
            return Ok(set);
        }
        if !path.is_dir() {
            return Err(TranscriptError::NotFound(path.to_path_buf()));
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|source| TranscriptError::Unreadable {
                path: path.to_path_buf(),
                source,
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|entry| entry.extension().is_some_and(|ext| ext == "json"))
            .collect();
        entries.sort();
        for entry in entries {
            set.insert(HypothesisDocument::load(&entry)?, &entry)?;
        }
        Ok(set)
    }

    /// Add a document, refusing a second claim on the same item and engine.
    pub fn insert(
        &mut self,
        document: HypothesisDocument,
        path: &Path,
    ) -> Result<(), TranscriptError> {
        let key = (document.item_id.clone(), document.engine_id.clone());
        if self.documents.contains_key(&key) {
            return Err(TranscriptError::DuplicateHypothesis {
                item_id: key.0,
                engine_id: key.1,
                path: path.to_path_buf(),
            });
        }
        self.documents.insert(key, document);
        Ok(())
    }

    /// The document for an item produced by a given engine, if any.
    #[must_use]
    pub fn get(&self, item_id: &str, engine_id: &str) -> Option<&HypothesisDocument> {
        self.documents
            .get(&(item_id.to_owned(), engine_id.to_owned()))
    }

    /// Engine identifiers present in the set, sorted.
    #[must_use]
    pub fn engines(&self) -> Vec<&str> {
        let mut engines: Vec<&str> = self
            .documents
            .keys()
            .map(|(_, engine)| engine.as_str())
            .collect();
        engines.sort_unstable();
        engines.dedup();
        engines
    }

    /// Number of documents held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    /// Whether the set holds no documents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(item: &str, engine: &str) -> HypothesisDocument {
        HypothesisDocument {
            schema_version: SUPPORTED_TRANSCRIPT_SCHEMA_VERSION,
            item_id: item.to_owned(),
            engine_id: engine.to_owned(),
            model_id: "small".to_owned(),
            model_version: None,
            deployment: Deployment::Local,
            alignment_origin: HypothesisAlignment::Native,
            text: "hello".to_owned(),
            words: vec![TimedWord {
                text: "hello".to_owned(),
                start_ms: 0,
                end_ms: 400,
            }],
            segment_boundaries_ms: vec![0],
            runtime: None,
        }
    }

    #[test]
    fn the_domain_no_times_case_maps_onto_unavailable() {
        assert_eq!(
            HypothesisAlignment::from(AlignmentOrigin::None),
            HypothesisAlignment::Unavailable
        );
        assert_eq!(
            HypothesisAlignment::from(AlignmentOrigin::Native),
            HypothesisAlignment::Native
        );
        assert_eq!(
            HypothesisAlignment::from(AlignmentOrigin::Derived),
            HypothesisAlignment::Derived
        );
        assert!(!HypothesisAlignment::Unavailable.carries_times());
    }

    #[test]
    fn the_domain_token_none_deserializes_as_unavailable() {
        let alignment: HypothesisAlignment = serde_json::from_str("\"none\"").expect("alias");
        assert_eq!(alignment, HypothesisAlignment::Unavailable);
        assert_eq!(
            serde_json::to_string(&HypothesisAlignment::Unavailable).expect("json"),
            "\"unavailable\""
        );
    }

    #[test]
    fn an_unavailable_alignment_hides_any_times_the_document_carries() {
        let mut doc = document("a", "whisper");
        doc.alignment_origin = HypothesisAlignment::Unavailable;
        assert!(doc.timed_words().is_empty());
        assert_eq!(document("a", "whisper").timed_words().len(), 1);
    }

    #[test]
    fn a_second_document_for_the_same_item_and_engine_is_refused() {
        let mut set = HypothesisSet::default();
        set.insert(document("a", "whisper"), Path::new("a.json"))
            .expect("first");
        set.insert(document("a", "apple"), Path::new("b.json"))
            .expect("other engine");
        let error = set
            .insert(document("a", "whisper"), Path::new("c.json"))
            .expect_err("duplicate");
        assert!(matches!(
            error,
            TranscriptError::DuplicateHypothesis { ref item_id, .. } if item_id == "a"
        ));
        assert_eq!(set.len(), 2);
        assert_eq!(set.engines(), vec!["apple", "whisper"]);
    }

    #[test]
    fn a_directory_of_documents_loads_in_sorted_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, item) in [("b.json", "two"), ("a.json", "one"), ("skip.txt", "x")] {
            let path = dir.path().join(name);
            if name.ends_with(".json") {
                std::fs::write(
                    &path,
                    serde_json::to_string(&document(item, "whisper")).expect("json"),
                )
                .expect("write");
            } else {
                std::fs::write(&path, "not json").expect("write");
            }
        }
        let set = HypothesisSet::load(dir.path()).expect("loads");
        assert_eq!(set.len(), 2);
        assert!(set.get("one", "whisper").is_some());
        assert!(set.get("two", "whisper").is_some());
        assert!(set.get("one", "apple").is_none());
    }

    #[test]
    fn an_unknown_document_version_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.json");
        let mut doc = document("a", "whisper");
        doc.schema_version = 7;
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("write");
        let error = HypothesisDocument::load(&path).expect_err("version");
        assert!(matches!(
            error,
            TranscriptError::UnsupportedSchemaVersion { found: 7, .. }
        ));
    }

    #[test]
    fn a_missing_path_is_a_typed_error() {
        let error = HypothesisSet::load(Path::new("/nonexistent/eval/path")).expect_err("missing");
        assert!(matches!(error, TranscriptError::NotFound(_)));
    }
}
