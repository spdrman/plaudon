//! The consent-cleared evaluation corpus register (spec 17.1, 17.6).
//!
//! > We will build a legally acquired, opt-in and synthetic audio corpus [...]
//! > We will not include confidential real client conversations in the public
//! > repository.
//!
//! Three rules are enforced by this module rather than by review:
//!
//! * **Clearance before measurement.** [`CorpusManifest::clear`] splits the
//!   manifest into items a run may use and items it may not. An item whose
//!   consent status does not grant clearance, or that carries no license, is
//!   rejected with a typed [`ClearanceRejection`] and excluded from the run.
//!   The rejection travels into the report, so exclusion is never silent.
//! * **Confidential client material is denied outright.** A row flagged
//!   `confidential_real_client` is rejected whatever else it claims.
//! * **Media is identified, not required.** An item names a path *or* a
//!   SHA-256 digest, because legally acquired media frequently cannot live in
//!   the repository. Nothing in this crate opens media: the runner consumes
//!   pre-produced hypothesis transcripts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::Sha256Digest;
use serde::{Deserialize, Serialize};

/// Schema version of the corpus manifest this build reads.
pub const SUPPORTED_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Recording length class covered by an item (spec 17.1: 1, 10, 60, 180 min).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationBucket {
    /// Roughly one minute of audio.
    #[serde(rename = "1min")]
    OneMinute,
    /// Roughly ten minutes of audio.
    #[serde(rename = "10min")]
    TenMinutes,
    /// Roughly one hour of audio.
    #[serde(rename = "60min")]
    SixtyMinutes,
    /// Roughly three hours of audio.
    #[serde(rename = "180min")]
    OneHundredEightyMinutes,
}

impl DurationBucket {
    /// Nominal length of the bucket in minutes.
    #[must_use]
    pub const fn minutes(self) -> u32 {
        match self {
            Self::OneMinute => 1,
            Self::TenMinutes => 10,
            Self::SixtyMinutes => 60,
            Self::OneHundredEightyMinutes => 180,
        }
    }

    /// The exact token used in manifests and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneMinute => "1min",
            Self::TenMinutes => "10min",
            Self::SixtyMinutes => "60min",
            Self::OneHundredEightyMinutes => "180min",
        }
    }
}

/// Acoustic condition of an item (spec 17.1 "noisy field recordings").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseClass {
    /// Close-mic studio or synthesized speech with no background.
    Clean,
    /// Ordinary room tone, no competing sources.
    Quiet,
    /// Office or meeting-room background, HVAC, keyboards.
    Office,
    /// Outdoor or construction-site capture with machinery and wind.
    Field,
}

/// A content characteristic the corpus must cover (spec 17.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentTag {
    /// Personal, company or place names.
    ProperNouns,
    /// Construction and other technical vocabulary.
    TechnicalVocabulary,
    /// Numbers, dates, currency amounts.
    NumbersDatesCurrency,
    /// Extended passages with no speech.
    QuietSections,
    /// Two-channel source material.
    Stereo,
    /// Deliberately low bitrate encoding.
    LowBitrate,
    /// Truncated or otherwise damaged encoding.
    InterruptedEncoding,
    /// More than one language inside a single recording.
    Multilingual,
    /// Marked regional accents.
    Accented,
}

/// Why the project believes it may use an item (spec 17.1, 17.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentStatus {
    /// Generated audio: no human subject, no consent to obtain.
    Synthetic,
    /// A human subject gave recorded, opt-in permission for evaluation use.
    OptInConsented,
    /// Published material under a license that permits this use.
    PublicLicensed,
    /// Provenance has not been established. Never usable.
    Unverified,
}

impl ConsentStatus {
    /// Whether this status alone permits evaluation use.
    ///
    /// Clearance still requires a license and an attribution; see
    /// [`CorpusItem::clearance`].
    #[must_use]
    pub const fn grants_clearance(self) -> bool {
        matches!(
            self,
            Self::Synthetic | Self::OptInConsented | Self::PublicLicensed
        )
    }

    /// The exact token used in manifests and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::OptInConsented => "opt_in_consented",
            Self::PublicLicensed => "public_licensed",
            Self::Unverified => "unverified",
        }
    }
}

impl std::fmt::Display for ConsentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the item's audio is identified.
///
/// Media that cannot be redistributed is referenced by digest only; the runner
/// never opens either form. The serialized form is a one-key mapping, either
/// `path:` or `sha256:`; declaring both, or neither, is a parse error rather
/// than a silent preference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MediaReferenceDocument", into = "MediaReferenceDocument")]
pub enum MediaReference {
    /// A path relative to the manifest directory.
    Path(PathBuf),
    /// A SHA-256 digest of the media bytes, held outside the repository.
    Sha256(Sha256Digest),
}

/// Serialized form of [`MediaReference`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaReferenceDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<Sha256Digest>,
}

impl TryFrom<MediaReferenceDocument> for MediaReference {
    type Error = String;

    fn try_from(document: MediaReferenceDocument) -> Result<Self, Self::Error> {
        match (document.path, document.sha256) {
            (Some(path), None) => Ok(Self::Path(path)),
            (None, Some(digest)) => Ok(Self::Sha256(digest)),
            (None, None) => Err("media must declare `path` or `sha256`".to_owned()),
            (Some(_), Some(_)) => {
                Err("media must declare exactly one of `path` or `sha256`".to_owned())
            }
        }
    }
}

impl From<MediaReference> for MediaReferenceDocument {
    fn from(reference: MediaReference) -> Self {
        match reference {
            MediaReference::Path(path) => Self {
                path: Some(path),
                sha256: None,
            },
            MediaReference::Sha256(digest) => Self {
                path: None,
                sha256: Some(digest),
            },
        }
    }
}

impl MediaReference {
    /// Path form, when the item ships its media.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path.as_path()),
            Self::Sha256(_) => None,
        }
    }

    /// Digest form, when the media lives outside the repository.
    #[must_use]
    pub const fn digest(&self) -> Option<Sha256Digest> {
        match self {
            Self::Path(_) => None,
            Self::Sha256(digest) => Some(*digest),
        }
    }
}

/// Where an item came from and who permitted its use (spec 17.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAttribution {
    /// Human-readable origin, for example a corpus name or a generator.
    pub source: String,
    /// Identifier of the signed consent or license record held off-repository.
    #[serde(default)]
    pub consent_record: Option<String>,
    /// Date the material was acquired, as `YYYY-MM-DD`.
    #[serde(default)]
    pub acquired_on: Option<String>,
    /// Set when the material is, or may be, a confidential real client
    /// conversation. Such an item is denied regardless of any other field.
    #[serde(default)]
    pub confidential_real_client: bool,
}

/// One corpus row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusItem {
    /// Stable identifier, unique inside the manifest, used to match
    /// reference and hypothesis transcripts.
    pub id: String,
    /// Media identity (path or digest).
    pub media: MediaReference,
    /// Nominal length class.
    pub duration_bucket: DurationBucket,
    /// Spoken locale.
    pub locale: LanguageTag,
    /// Number of distinct speakers.
    pub speaker_count: u16,
    /// Whether speakers talk over each other anywhere in the item.
    #[serde(default)]
    pub overlapping_speech: bool,
    /// Acoustic condition.
    pub noise_class: NoiseClass,
    /// Content characteristics covered by this item.
    #[serde(default)]
    pub content_tags: BTreeSet<ContentTag>,
    /// License identifier, SPDX where one exists. Absent means not cleared.
    #[serde(default)]
    pub license: Option<String>,
    /// Consent basis for use.
    pub consent: ConsentStatus,
    /// Provenance record.
    pub attribution: SourceAttribution,
    /// Reference transcript path, relative to the manifest directory.
    pub reference_transcript: PathBuf,
}

impl CorpusItem {
    /// Every reason this item may not be used, in evaluation order.
    ///
    /// An empty vector means the item is cleared.
    #[must_use]
    pub fn clearance(&self) -> Vec<ClearanceRejection> {
        let mut reasons = Vec::new();
        if self.attribution.confidential_real_client {
            reasons.push(ClearanceRejection::ConfidentialRealClient);
        }
        if !self.consent.grants_clearance() {
            reasons.push(ClearanceRejection::ConsentNotCleared {
                status: self.consent,
            });
        }
        match &self.license {
            None => reasons.push(ClearanceRejection::MissingLicense),
            Some(license) if license.trim().is_empty() => {
                reasons.push(ClearanceRejection::MissingLicense);
            }
            Some(_) => {}
        }
        if self.attribution.source.trim().is_empty() {
            reasons.push(ClearanceRejection::MissingSourceAttribution);
        }
        if self.reference_transcript.as_os_str().is_empty() {
            reasons.push(ClearanceRejection::MissingReferenceTranscript);
        }
        reasons
    }

    /// Whether the item may be measured.
    #[must_use]
    pub fn is_cleared(&self) -> bool {
        self.clearance().is_empty()
    }
}

/// A typed reason an item is excluded from a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ClearanceRejection {
    /// The consent status does not permit evaluation use.
    #[error("consent status `{status}` does not grant clearance")]
    ConsentNotCleared {
        /// The status the manifest declared.
        status: ConsentStatus,
    },
    /// No license was declared.
    #[error("no license declared")]
    MissingLicense,
    /// The row is flagged as confidential real client material.
    #[error("confidential real client material is never admitted (spec 17.1)")]
    ConfidentialRealClient,
    /// No source attribution was declared.
    #[error("no source attribution declared")]
    MissingSourceAttribution,
    /// No reference transcript was declared.
    #[error("no reference transcript declared")]
    MissingReferenceTranscript,
}

impl ClearanceRejection {
    /// Stable machine code, as published in the report.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ConsentNotCleared { .. } => "consent_not_cleared",
            Self::MissingLicense => "missing_license",
            Self::ConfidentialRealClient => "confidential_real_client",
            Self::MissingSourceAttribution => "missing_source_attribution",
            Self::MissingReferenceTranscript => "missing_reference_transcript",
        }
    }
}

/// An item the runner refused, with every reason it refused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRejection {
    /// Identifier of the rejected item.
    pub item_id: String,
    /// Every clearance rule the item failed.
    pub reasons: Vec<ClearanceRejection>,
}

/// The corpus manifest as committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Stable corpus identifier, cited by every report and gate decision.
    pub corpus_id: String,
    /// What this corpus is for, in one sentence.
    pub description: String,
    /// Note on how reference transcripts were produced and checked
    /// (spec 17.2 requires reference quality to be published).
    pub reference_quality: String,
    /// Corpus rows, in declaration order.
    pub items: Vec<CorpusItem>,
}

/// A manifest that could not be read or is internally inconsistent.
#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    /// The document is not parseable.
    #[error("corpus manifest is not parseable: {0}")]
    Unparseable(#[from] serde_norway::Error),
    /// The file could not be read.
    #[error("cannot read corpus manifest {path}: {source}")]
    Unreadable {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The schema version is not the one this build understands.
    #[error(
        "corpus manifest schema version {found} is not supported (this build reads {supported})"
    )]
    UnsupportedSchemaVersion {
        /// Version found in the document.
        found: u32,
        /// Version this build reads.
        supported: u32,
    },
    /// A required identifying field is blank.
    #[error("corpus manifest field `{0}` must not be empty")]
    MissingField(&'static str),
    /// The manifest declares no rows at all.
    #[error("corpus `{0}` declares no items")]
    Empty(String),
    /// Two rows share an identifier.
    #[error("corpus declares item id `{0}` more than once")]
    DuplicateItemId(String),
    /// A row claims overlap but only one speaker.
    #[error("item `{0}` marks overlapping speech with fewer than two speakers")]
    OverlapWithoutSpeakers(String),
    /// A row declares no speakers.
    #[error("item `{0}` declares zero speakers")]
    NoSpeakers(String),
}

/// The split of a manifest into usable and refused rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearedCorpus<'a> {
    accepted: Vec<&'a CorpusItem>,
    rejected: Vec<ItemRejection>,
}

impl<'a> ClearedCorpus<'a> {
    /// Rows a run may measure.
    #[must_use]
    pub fn accepted(&self) -> &[&'a CorpusItem] {
        &self.accepted
    }

    /// Rows the run refused, with typed reasons.
    #[must_use]
    pub fn rejected(&self) -> &[ItemRejection] {
        &self.rejected
    }

    /// Whether any row was refused.
    #[must_use]
    pub fn has_rejections(&self) -> bool {
        !self.rejected.is_empty()
    }
}

impl CorpusManifest {
    /// Parse and structurally validate a manifest document.
    ///
    /// YAML is a superset of JSON, so both committed forms parse here.
    /// Clearance is *not* applied: [`Self::clear`] does that, because a
    /// manifest is allowed to record a refused item and must keep doing so.
    pub fn from_document(document: &str) -> Result<Self, CorpusError> {
        let manifest: Self = serde_norway::from_str(document)?;
        manifest.validate_structure()?;
        Ok(manifest)
    }

    /// Read and validate a manifest from disk.
    pub fn load(path: &Path) -> Result<Self, CorpusError> {
        let text = std::fs::read_to_string(path).map_err(|source| CorpusError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_document(&text)
    }

    fn validate_structure(&self) -> Result<(), CorpusError> {
        if self.schema_version != SUPPORTED_MANIFEST_SCHEMA_VERSION {
            return Err(CorpusError::UnsupportedSchemaVersion {
                found: self.schema_version,
                supported: SUPPORTED_MANIFEST_SCHEMA_VERSION,
            });
        }
        if self.corpus_id.trim().is_empty() {
            return Err(CorpusError::MissingField("corpus_id"));
        }
        if self.reference_quality.trim().is_empty() {
            return Err(CorpusError::MissingField("reference_quality"));
        }
        if self.items.is_empty() {
            return Err(CorpusError::Empty(self.corpus_id.clone()));
        }
        let mut seen = BTreeSet::new();
        for item in &self.items {
            if item.id.trim().is_empty() {
                return Err(CorpusError::MissingField("items[].id"));
            }
            if !seen.insert(item.id.as_str()) {
                return Err(CorpusError::DuplicateItemId(item.id.clone()));
            }
            if item.speaker_count == 0 {
                return Err(CorpusError::NoSpeakers(item.id.clone()));
            }
            if item.overlapping_speech && item.speaker_count < 2 {
                return Err(CorpusError::OverlapWithoutSpeakers(item.id.clone()));
            }
        }
        Ok(())
    }

    /// Split the manifest into rows a run may measure and rows it must refuse.
    #[must_use]
    pub fn clear(&self) -> ClearedCorpus<'_> {
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for item in &self.items {
            let reasons = item.clearance();
            if reasons.is_empty() {
                accepted.push(item);
            } else {
                rejected.push(ItemRejection {
                    item_id: item.id.clone(),
                    reasons,
                });
            }
        }
        ClearedCorpus { accepted, rejected }
    }

    /// Content hash of the manifest, cited by reports and gate decisions.
    ///
    /// The hash is taken over the canonical JSON serialization of the parsed
    /// manifest, not over the file bytes, so reformatting or recommenting the
    /// YAML does not invalidate a stored evaluation record while any change to
    /// a declared field does.
    #[must_use]
    pub fn digest(&self) -> Sha256Digest {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        Sha256Digest::of(&canonical)
    }

    /// Count of accepted rows per locale, as published in the report.
    #[must_use]
    pub fn locale_breakdown(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for item in self.clear().accepted() {
            *counts.entry(item.locale.as_str().to_owned()).or_insert(0) += 1;
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> CorpusItem {
        CorpusItem {
            id: id.to_owned(),
            media: MediaReference::Sha256(Sha256Digest::of(id.as_bytes())),
            duration_bucket: DurationBucket::OneMinute,
            locale: LanguageTag::parse("en-US").expect("tag"),
            speaker_count: 1,
            overlapping_speech: false,
            noise_class: NoiseClass::Clean,
            content_tags: BTreeSet::new(),
            license: Some("CC0-1.0".to_owned()),
            consent: ConsentStatus::Synthetic,
            attribution: SourceAttribution {
                source: "generated by scripts/make-corpus.sh".to_owned(),
                consent_record: None,
                acquired_on: Some("2026-01-05".to_owned()),
                confidential_real_client: false,
            },
            reference_transcript: PathBuf::from("references/x.json"),
        }
    }

    fn manifest(items: Vec<CorpusItem>) -> CorpusManifest {
        CorpusManifest {
            schema_version: SUPPORTED_MANIFEST_SCHEMA_VERSION,
            corpus_id: "unit".to_owned(),
            description: "unit corpus".to_owned(),
            reference_quality: "hand-checked".to_owned(),
            items,
        }
    }

    #[test]
    fn a_synthetic_licensed_item_is_cleared() {
        assert_eq!(item("a").clearance(), Vec::new());
        assert!(item("a").is_cleared());
    }

    #[test]
    fn an_unlicensed_item_is_rejected_with_a_typed_reason() {
        let mut row = item("a");
        row.license = None;
        assert_eq!(row.clearance(), vec![ClearanceRejection::MissingLicense]);
        assert_eq!(
            ClearanceRejection::MissingLicense.to_string(),
            "no license declared"
        );
    }

    #[test]
    fn a_blank_license_string_is_not_a_license() {
        let mut row = item("a");
        row.license = Some("   ".to_owned());
        assert_eq!(row.clearance(), vec![ClearanceRejection::MissingLicense]);
    }

    #[test]
    fn an_unverified_item_is_rejected_even_when_licensed() {
        let mut row = item("a");
        row.consent = ConsentStatus::Unverified;
        assert_eq!(
            row.clearance(),
            vec![ClearanceRejection::ConsentNotCleared {
                status: ConsentStatus::Unverified
            }]
        );
    }

    #[test]
    fn a_confidential_client_item_is_denied_despite_consent_and_license() {
        let mut row = item("a");
        row.consent = ConsentStatus::OptInConsented;
        row.attribution.confidential_real_client = true;
        assert_eq!(
            row.clearance(),
            vec![ClearanceRejection::ConfidentialRealClient]
        );
    }

    #[test]
    fn every_failed_rule_is_reported_not_just_the_first() {
        let mut row = item("a");
        row.consent = ConsentStatus::Unverified;
        row.license = None;
        assert_eq!(
            row.clearance(),
            vec![
                ClearanceRejection::ConsentNotCleared {
                    status: ConsentStatus::Unverified
                },
                ClearanceRejection::MissingLicense
            ]
        );
    }

    #[test]
    fn clearing_splits_the_manifest_and_names_the_refused_row() {
        let mut bad = item("bad");
        bad.license = None;
        let manifest = manifest(vec![item("good"), bad]);
        let cleared = manifest.clear();
        assert_eq!(cleared.accepted().len(), 1);
        assert_eq!(cleared.accepted()[0].id, "good");
        assert_eq!(cleared.rejected().len(), 1);
        assert_eq!(cleared.rejected()[0].item_id, "bad");
        assert!(cleared.has_rejections());
    }

    #[test]
    fn a_duplicate_item_id_is_a_hard_manifest_error() {
        let manifest = manifest(vec![item("a"), item("a")]);
        let document = serde_norway::to_string(&manifest).expect("yaml");
        let error = CorpusManifest::from_document(&document).expect_err("duplicate");
        assert!(matches!(error, CorpusError::DuplicateItemId(id) if id == "a"));
    }

    #[test]
    fn an_unknown_schema_version_is_refused() {
        let mut manifest = manifest(vec![item("a")]);
        manifest.schema_version = 99;
        let document = serde_norway::to_string(&manifest).expect("yaml");
        let error = CorpusManifest::from_document(&document).expect_err("version");
        assert!(matches!(
            error,
            CorpusError::UnsupportedSchemaVersion {
                found: 99,
                supported: 1
            }
        ));
    }

    #[test]
    fn overlap_requires_two_speakers() {
        let mut row = item("a");
        row.overlapping_speech = true;
        let document = serde_norway::to_string(&manifest(vec![row])).expect("yaml");
        let error = CorpusManifest::from_document(&document).expect_err("overlap");
        assert!(matches!(error, CorpusError::OverlapWithoutSpeakers(id) if id == "a"));
    }

    #[test]
    fn an_unknown_consent_token_fails_to_parse_rather_than_defaulting() {
        let document = "\
schema_version: 1
corpus_id: unit
description: unit corpus
reference_quality: hand-checked
items:
  - id: a
    media:
      path: a.wav
    duration_bucket: 1min
    locale: en-US
    speaker_count: 1
    noise_class: clean
    license: CC0-1.0
    consent: probably_fine
    attribution:
      source: somewhere
    reference_transcript: references/a.json
";
        assert!(matches!(
            CorpusManifest::from_document(document),
            Err(CorpusError::Unparseable(_))
        ));
    }

    #[test]
    fn media_may_be_a_path_or_a_digest() {
        let path_item = "\
schema_version: 1
corpus_id: unit
description: unit corpus
reference_quality: hand-checked
items:
  - id: a
    media:
      path: audio/a.wav
    duration_bucket: 10min
    locale: de-DE
    speaker_count: 2
    noise_class: field
    content_tags: [proper_nouns, technical_vocabulary]
    license: CC-BY-4.0
    consent: public_licensed
    attribution:
      source: open corpus
    reference_transcript: references/a.json
  - id: b
    media:
      sha256: 3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532
    duration_bucket: 180min
    locale: en-GB
    speaker_count: 3
    overlapping_speech: true
    noise_class: office
    license: CC0-1.0
    consent: opt_in_consented
    attribution:
      source: volunteer session
      consent_record: consent-2026-0001
    reference_transcript: references/b.json
";
        let manifest = CorpusManifest::from_document(path_item).expect("parses");
        assert_eq!(
            manifest.items[0].media.path(),
            Some(Path::new("audio/a.wav"))
        );
        assert!(manifest.items[0].media.digest().is_none());
        assert!(manifest.items[1].media.digest().is_some());
        assert_eq!(manifest.items[1].duration_bucket.minutes(), 180);
        assert_eq!(
            manifest.locale_breakdown(),
            BTreeMap::from([("de-DE".to_owned(), 1), ("en-GB".to_owned(), 1)])
        );
    }

    #[test]
    fn the_digest_follows_declared_content_not_formatting() {
        let manifest = manifest(vec![item("a")]);
        let compact = serde_norway::to_string(&manifest).expect("yaml");
        let spaced = format!("# a comment\n{compact}\n\n");
        let left = CorpusManifest::from_document(&compact).expect("parses");
        let right = CorpusManifest::from_document(&spaced).expect("parses");
        assert_eq!(left.digest(), right.digest());

        let mut changed = manifest.clone();
        changed.items[0].speaker_count = 2;
        assert_ne!(left.digest(), changed.digest());
    }

    #[test]
    fn a_rejection_serializes_with_a_stable_code() {
        let json = serde_json::to_value(ClearanceRejection::ConsentNotCleared {
            status: ConsentStatus::Unverified,
        })
        .expect("json");
        assert_eq!(json["code"], "consent_not_cleared");
        assert_eq!(json["status"], "unverified");
        assert_eq!(
            ClearanceRejection::ConfidentialRealClient.code(),
            "confidential_real_client"
        );
    }
}
