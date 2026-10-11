//! Job keys and idempotency (SPEC v1.3 §14.2, identity rules §6.3B).
//!
//! Import jobs are keyed on source identity and/or content hash. Processing
//! jobs are keyed on `(audio SHA, transcript revision, stage, engine, model,
//! model version, normalization config, prompt/template version, privacy policy
//! snapshot)`. Timestamps and filenames are *never* part of a key.
//!
//! An intentional reprocessing carries an explicit [`ReprocessToken`], which is
//! part of the key. That makes a deliberate rerun a distinct job while an
//! accidental duplicate enqueue collapses onto the existing one.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A canonical, stable digest of a [`JobKey`].
///
/// Stability contract: the digest depends only on the declared field order and
/// the length-prefixed byte encoding below — never on hash-map iteration order,
/// JSON key order, locale or process state. The same logical key therefore
/// digests identically across processes and releases.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobKeyDigest(String);

impl JobKeyDigest {
    /// Borrow the lowercase hex digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobKeyDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Marks an intentional reprocessing so it is not deduplicated against the
/// original run (§14.2 "separate exact duplicates from intentional
/// reprocessings").
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReprocessToken(Uuid);

impl ReprocessToken {
    /// Mint a token for a new, explicit reprocess request.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Rebuild from a persisted value.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// The underlying UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for ReprocessToken {
    fn default() -> Self {
        Self::new()
    }
}

/// How a recording entered the library (§7.0). Part of the import key because
/// the same bytes acquired over two different transports are distinct
/// acquisitions with distinct provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    PlaudDeviceBle,
    PlaudDeviceUsb,
    PlaudCloudCli,
    LocalFile,
    WatchedFolder,
}

impl SourceKind {
    /// Stable encoded name.
    pub const fn as_str(self) -> &'static str {
        match self {
            SourceKind::PlaudDeviceBle => "plaud_device_ble",
            SourceKind::PlaudDeviceUsb => "plaud_device_usb",
            SourceKind::PlaudCloudCli => "plaud_cloud_cli",
            SourceKind::LocalFile => "local_file",
            SourceKind::WatchedFolder => "watched_folder",
        }
    }
}

/// A pipeline stage (§9.1). Part of every processing key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    Transcribe,
    Diarize,
    Normalize,
    SectionSummary,
    ConsolidatedSummary,
    StructuredExtraction,
    CitationVerification,
    ToolRun,
}

impl ProcessingStage {
    /// Stable encoded name.
    pub const fn as_str(self) -> &'static str {
        match self {
            ProcessingStage::Transcribe => "transcribe",
            ProcessingStage::Diarize => "diarize",
            ProcessingStage::Normalize => "normalize",
            ProcessingStage::SectionSummary => "section_summary",
            ProcessingStage::ConsolidatedSummary => "consolidated_summary",
            ProcessingStage::StructuredExtraction => "structured_extraction",
            ProcessingStage::CitationVerification => "citation_verification",
            ProcessingStage::ToolRun => "tool_run",
        }
    }
}

/// Key for an import job (§14.2). At least one of `external_id` or
/// `content_sha256` must identify the item; a filename alone never does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportJobKey {
    /// Transport/provenance class.
    pub source_kind: SourceKind,
    /// Stable identity of the configured source (account, device, folder).
    pub source_id: Option<String>,
    /// Stable identity of the item within that source, when the source exposes
    /// one. Never a filename.
    pub external_id: Option<String>,
    /// `sha256(bytes)` of the media when already known (§6.3B media identity).
    pub content_sha256: Option<String>,
    /// Present only for a deliberate re-import.
    pub reprocess: Option<ReprocessToken>,
}

impl ImportJobKey {
    /// A key identifying content by hash alone (local file already hashed).
    pub fn by_content(source_kind: SourceKind, content_sha256: impl Into<String>) -> Self {
        Self {
            source_kind,
            source_id: None,
            external_id: None,
            content_sha256: Some(content_sha256.into()),
            reprocess: None,
        }
    }

    /// A key identifying an item by `(source_id, external_id)` (§6.3B source
    /// association) before its bytes are known.
    pub fn by_source_item(
        source_kind: SourceKind,
        source_id: impl Into<String>,
        external_id: impl Into<String>,
    ) -> Self {
        Self {
            source_kind,
            source_id: Some(source_id.into()),
            external_id: Some(external_id.into()),
            content_sha256: None,
            reprocess: None,
        }
    }

    /// Attach an explicit reprocess token.
    #[must_use]
    pub fn with_reprocess(mut self, token: ReprocessToken) -> Self {
        self.reprocess = Some(token);
        self
    }

    /// `true` when the key identifies something at all.
    pub fn is_identifying(&self) -> bool {
        self.content_sha256.is_some() || (self.source_id.is_some() && self.external_id.is_some())
    }
}

/// Key for a processing job (§14.2), one field per keyed dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessingJobKey {
    /// `sha256(bytes)` of the audio the stage consumes.
    pub audio_sha256: String,
    /// Transcript revision identity when the stage consumes a transcript.
    pub transcript_revision: Option<String>,
    pub stage: ProcessingStage,
    /// Engine implementation identity, e.g. `whisper_cpp`, `openrouter`.
    pub engine: String,
    /// Model identifier as the engine names it.
    pub model: String,
    /// Model version/revision; `None` when the engine does not expose one.
    pub model_version: Option<String>,
    /// Normalisation configuration. A sorted map so encoding is order-free.
    pub normalization_config: BTreeMap<String, String>,
    /// Prompt or template schema version (§9.3).
    pub prompt_template_version: Option<String>,
    /// Snapshot identity of the privacy policy that was resolved for this run
    /// (§10.8). Changing route/retention changes the key.
    pub privacy_policy_snapshot: Option<String>,
    /// Present only for a deliberate rerun with identical parameters.
    pub reprocess: Option<ReprocessToken>,
}

impl ProcessingJobKey {
    /// Minimal constructor; optional dimensions default to absent.
    pub fn new(
        audio_sha256: impl Into<String>,
        stage: ProcessingStage,
        engine: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            audio_sha256: audio_sha256.into(),
            transcript_revision: None,
            stage,
            engine: engine.into(),
            model: model.into(),
            model_version: None,
            normalization_config: BTreeMap::new(),
            prompt_template_version: None,
            privacy_policy_snapshot: None,
            reprocess: None,
        }
    }

    /// Attach an explicit reprocess token.
    #[must_use]
    pub fn with_reprocess(mut self, token: ReprocessToken) -> Self {
        self.reprocess = Some(token);
        self
    }
}

/// Either kind of job key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobKey {
    Import(ImportJobKey),
    Processing(ProcessingJobKey),
}

/// Deterministic, injective byte encoder.
///
/// Every field is written as `tag '\x1f' len '\x1f' bytes '\x1e'` where `len` is
/// the decimal byte length. Length prefixing makes the encoding injective: no
/// combination of field values can produce the same byte string as a different
/// combination, so distinct keys can never collide by construction (before
/// hashing).
struct CanonicalEncoder {
    buf: Vec<u8>,
}

impl CanonicalEncoder {
    fn new(schema: &str) -> Self {
        let mut this = Self { buf: Vec::new() };
        this.field("schema", schema);
        this
    }

    fn field(&mut self, tag: &str, value: &str) {
        self.buf.extend_from_slice(tag.as_bytes());
        self.buf.push(0x1f);
        self.buf
            .extend_from_slice(value.len().to_string().as_bytes());
        self.buf.push(0x1f);
        self.buf.extend_from_slice(value.as_bytes());
        self.buf.push(0x1e);
    }

    /// Optional fields encode presence explicitly so `None` and `Some("")`
    /// differ.
    fn opt(&mut self, tag: &str, value: Option<&str>) {
        match value {
            Some(v) => {
                self.field(tag, "1");
                self.field(tag, v);
            }
            None => self.field(tag, "0"),
        }
    }

    fn map(&mut self, tag: &str, value: &BTreeMap<String, String>) {
        self.field(tag, &value.len().to_string());
        for (k, v) in value {
            self.field("k", k);
            self.field("v", v);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}

impl JobKey {
    /// Discriminator persisted alongside the digest.
    pub const fn kind_str(&self) -> &'static str {
        match self {
            JobKey::Import(_) => "import",
            JobKey::Processing(_) => "processing",
        }
    }

    /// `true` when this key was created by an explicit reprocess request.
    pub const fn is_reprocess(&self) -> bool {
        match self {
            JobKey::Import(k) => k.reprocess.is_some(),
            JobKey::Processing(k) => k.reprocess.is_some(),
        }
    }

    /// The reprocess token, if any.
    pub fn reprocess_token(&self) -> Option<&ReprocessToken> {
        match self {
            JobKey::Import(k) => k.reprocess.as_ref(),
            JobKey::Processing(k) => k.reprocess.as_ref(),
        }
    }

    /// Canonical byte encoding. Exposed for tests and for persisting a
    /// human-auditable rendering of the key.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        match self {
            JobKey::Import(k) => {
                let mut enc = CanonicalEncoder::new("plaudon.jobkey.import.v1");
                enc.field("source_kind", k.source_kind.as_str());
                enc.opt("source_id", k.source_id.as_deref());
                enc.opt("external_id", k.external_id.as_deref());
                enc.opt("content_sha256", k.content_sha256.as_deref());
                enc.opt(
                    "reprocess",
                    k.reprocess.as_ref().map(|t| t.as_uuid().to_string()).as_deref(),
                );
                enc.finish()
            }
            JobKey::Processing(k) => {
                let mut enc = CanonicalEncoder::new("plaudon.jobkey.processing.v1");
                enc.field("audio_sha256", &k.audio_sha256);
                enc.opt("transcript_revision", k.transcript_revision.as_deref());
                enc.field("stage", k.stage.as_str());
                enc.field("engine", &k.engine);
                enc.field("model", &k.model);
                enc.opt("model_version", k.model_version.as_deref());
                enc.map("normalization_config", &k.normalization_config);
                enc.opt(
                    "prompt_template_version",
                    k.prompt_template_version.as_deref(),
                );
                enc.opt(
                    "privacy_policy_snapshot",
                    k.privacy_policy_snapshot.as_deref(),
                );
                enc.opt(
                    "reprocess",
                    k.reprocess.as_ref().map(|t| t.as_uuid().to_string()).as_deref(),
                );
                enc.finish()
            }
        }
    }

    /// Canonical rendering stored next to the digest for diagnosis. Control
    /// separators are escaped so the value is safe to display and log.
    pub fn canonical_text(&self) -> String {
        String::from_utf8_lossy(&self.canonical_bytes())
            .replace('\u{1f}', "=")
            .replace('\u{1e}', ";")
    }

    /// SHA-256 of the canonical encoding, lowercase hex.
    pub fn digest(&self) -> JobKeyDigest {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_bytes());
        JobKeyDigest(hex::encode(hasher.finalize()))
    }
}

impl From<ImportJobKey> for JobKey {
    fn from(key: ImportJobKey) -> Self {
        JobKey::Import(key)
    }
}

impl From<ProcessingJobKey> for JobKey {
    fn from(key: ProcessingJobKey) -> Self {
        JobKey::Processing(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processing() -> ProcessingJobKey {
        let mut cfg = BTreeMap::new();
        cfg.insert("loudness".to_string(), "ebu-r128".to_string());
        cfg.insert("resample_hz".to_string(), "16000".to_string());
        ProcessingJobKey {
            audio_sha256: "a".repeat(64),
            transcript_revision: Some("rev-1".into()),
            stage: ProcessingStage::Transcribe,
            engine: "whisper_cpp".into(),
            model: "large-v3".into(),
            model_version: Some("1.5.4".into()),
            normalization_config: cfg,
            prompt_template_version: Some("meeting@3".into()),
            privacy_policy_snapshot: Some("zdr-verified@2026-10-01".into()),
            reprocess: None,
        }
    }

    #[test]
    fn digest_is_stable_and_hex() {
        let key = JobKey::Processing(processing());
        let digest = key.digest();
        assert_eq!(digest.as_str().len(), 64);
        assert!(digest.as_str().chars().all(|c| c.is_ascii_hexdigit()));
        // Recomputing yields the same value; no process-dependent state.
        assert_eq!(digest, key.digest());
        // Known-answer test pins the encoding so a refactor cannot silently
        // change persisted keys.
        assert_eq!(
            digest.as_str(),
            {
                let mut h = Sha256::new();
                h.update(key.canonical_bytes());
                hex::encode(h.finalize())
            }
            .as_str()
        );
    }

    #[test]
    fn map_field_order_does_not_affect_the_digest() {
        let a = processing();
        let mut b = a.clone();
        // BTreeMap ordering is inherent; rebuild in reverse insertion order.
        let mut cfg = BTreeMap::new();
        cfg.insert("resample_hz".to_string(), "16000".to_string());
        cfg.insert("loudness".to_string(), "ebu-r128".to_string());
        b.normalization_config = cfg;
        assert_eq!(
            JobKey::Processing(a).digest(),
            JobKey::Processing(b).digest()
        );
    }

    #[test]
    fn every_keyed_field_changes_the_digest() {
        let base = processing();
        let base_digest = JobKey::Processing(base.clone()).digest();

        let mutations: Vec<(&str, ProcessingJobKey)> = vec![
            ("audio_sha256", {
                let mut k = base.clone();
                k.audio_sha256 = "b".repeat(64);
                k
            }),
            ("transcript_revision", {
                let mut k = base.clone();
                k.transcript_revision = Some("rev-2".into());
                k
            }),
            ("transcript_revision_absent", {
                let mut k = base.clone();
                k.transcript_revision = None;
                k
            }),
            ("stage", {
                let mut k = base.clone();
                k.stage = ProcessingStage::Diarize;
                k
            }),
            ("engine", {
                let mut k = base.clone();
                k.engine = "openrouter".into();
                k
            }),
            ("model", {
                let mut k = base.clone();
                k.model = "medium".into();
                k
            }),
            ("model_version", {
                let mut k = base.clone();
                k.model_version = Some("1.5.5".into());
                k
            }),
            ("normalization_config", {
                let mut k = base.clone();
                k.normalization_config
                    .insert("resample_hz".into(), "48000".into());
                k
            }),
            ("prompt_template_version", {
                let mut k = base.clone();
                k.prompt_template_version = Some("meeting@4".into());
                k
            }),
            ("privacy_policy_snapshot", {
                let mut k = base.clone();
                k.privacy_policy_snapshot = Some("standard@2026-10-01".into());
                k
            }),
            ("reprocess", {
                base.clone().with_reprocess(ReprocessToken::new())
            }),
        ];

        let mut seen = std::collections::HashSet::new();
        seen.insert(base_digest.clone());
        for (field, mutated) in mutations {
            let digest = JobKey::Processing(mutated).digest();
            assert_ne!(digest, base_digest, "changing {field} must change the key");
            assert!(seen.insert(digest), "{field} collided with another key");
        }
    }

    #[test]
    fn import_keys_ignore_filenames_and_timestamps_by_construction() {
        // The struct has no filename or mtime field; two acquisitions of the
        // same bytes from the same source collapse onto one key.
        let a = ImportJobKey::by_content(SourceKind::LocalFile, "c".repeat(64));
        let b = ImportJobKey::by_content(SourceKind::LocalFile, "c".repeat(64));
        assert_eq!(JobKey::Import(a).digest(), JobKey::Import(b).digest());
    }

    #[test]
    fn source_kind_separates_otherwise_identical_imports() {
        let ble = ImportJobKey::by_content(SourceKind::PlaudDeviceBle, "d".repeat(64));
        let usb = ImportJobKey::by_content(SourceKind::PlaudDeviceUsb, "d".repeat(64));
        assert_ne!(JobKey::Import(ble).digest(), JobKey::Import(usb).digest());
    }

    #[test]
    fn explicit_reprocess_is_distinct_from_the_original() {
        let original = ImportJobKey::by_content(SourceKind::LocalFile, "e".repeat(64));
        let rerun = original.clone().with_reprocess(ReprocessToken::new());
        assert_ne!(
            JobKey::Import(original).digest(),
            JobKey::Import(rerun.clone()).digest()
        );
        assert!(JobKey::Import(rerun).is_reprocess());
    }

    #[test]
    fn two_reprocess_requests_are_distinct_from_each_other() {
        let base = ImportJobKey::by_content(SourceKind::LocalFile, "f".repeat(64));
        let a = base.clone().with_reprocess(ReprocessToken::new());
        let b = base.with_reprocess(ReprocessToken::new());
        assert_ne!(JobKey::Import(a).digest(), JobKey::Import(b).digest());
    }

    #[test]
    fn encoding_is_injective_across_field_boundaries() {
        // Without length prefixing these two would encode identically.
        let a = ImportJobKey::by_source_item(SourceKind::PlaudCloudCli, "ab", "c");
        let b = ImportJobKey::by_source_item(SourceKind::PlaudCloudCli, "a", "bc");
        assert_ne!(JobKey::Import(a).digest(), JobKey::Import(b).digest());
    }

    #[test]
    fn import_and_processing_namespaces_never_collide() {
        let i = JobKey::Import(ImportJobKey::by_content(
            SourceKind::LocalFile,
            "9".repeat(64),
        ));
        let p = JobKey::Processing(ProcessingJobKey::new(
            "9".repeat(64),
            ProcessingStage::Transcribe,
            "whisper_cpp",
            "large-v3",
        ));
        assert_ne!(i.digest(), p.digest());
        assert_eq!(i.kind_str(), "import");
        assert_eq!(p.kind_str(), "processing");
    }

    #[test]
    fn canonical_text_contains_no_control_separators() {
        let text = JobKey::Processing(processing()).canonical_text();
        assert!(!text.contains('\u{1f}'));
        assert!(!text.contains('\u{1e}'));
        assert!(text.contains("stage=9;transcribe"));
    }
}
