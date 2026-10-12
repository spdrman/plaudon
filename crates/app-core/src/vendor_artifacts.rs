//! Optional import of vendor-generated transcripts and summaries
//! (spec §7.5, §1.2(11), App. A.2 "Vendor-export transcript appears").
//!
//! # Why this module is off by default
//!
//! §7.5 permits importing PLAUD-generated transcripts **only** where an
//! *official* CLI or web-export mechanism produces them, and is explicit that
//! we never infer such a path exists merely because an unofficial API returns
//! the same data. That is a product gate, not a code style preference, so it
//! is expressed as a value — [`VendorImportCapability`] — that the application
//! constructs with [`VendorImportCapability::production_default`] and that
//! refuses every import until an export path has actually been verified
//! (§18.6). Only a test, or a future build that carries real evidence, can
//! construct an enabled capability.
//!
//! # What an import may and may not do
//!
//! | Rule | Spec |
//! |---|---|
//! | A vendor revision is tagged `source_vendor` and never passes as ours | §7.5 |
//! | No cost ledger entry is created: nothing was dispatched | §7.5, §10.7 |
//! | A human-corrected transcript is never replaced, demoted or shadowed | App. A.2 |
//! | The original bytes stay attributable by digest, length and export name | §7.5 |
//!
//! The importer writes exactly two kinds of row: a `transcript_revisions` row
//! (through the EPIC F repository, so a vendor revision is stored by the same
//! code path as any other) and a `source_export_artifacts` row. It never
//! touches `usage_events` or `inference_attempts`, because no inference was
//! attempted and no money was spent.

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_domain::ids::{RecordingId, SourceItemId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{RevisionOrigin, RevisionStatus};
use plaudon_persistence::db::Database;
use plaudon_persistence::error::PersistenceResult;
use plaudon_persistence::repo::{
    SourceExportArtifact, SourceExportArtifactRepository, TranscriptProvenanceRepository,
};
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use uuid::Uuid;

/// The engine label recorded for an imported vendor artifact.
///
/// It is deliberately not the name of any transcription engine we run: a
/// reader must be able to tell at a glance that nothing was transcribed here.
pub const VENDOR_EXPORT_ENGINE: &str = "vendor_export";

/// Kinds of artifact an official export can produce (spec §7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VendorArtifactKind {
    /// A transcript of the recording.
    Transcript,
    /// A generated summary.
    Summary,
    /// Generated keywords or tags.
    Tags,
}

impl VendorArtifactKind {
    /// Column value stored in `source_export_artifacts.artifact_kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::Summary => "summary",
            Self::Tags => "tags",
        }
    }
}

/// How we came to be holding the artifact.
///
/// §7.5: an unofficial access path is never promoted to an official one, so
/// the provenance of the *mechanism* travels with the artifact and is checked
/// at import time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorExportPath {
    /// Produced by a documented export command of the official CLI.
    OfficialCli {
        /// Release the export command was verified against (§18.6).
        release: String,
    },
    /// Produced by the vendor's own web or mobile export feature and placed on
    /// disk by the user.
    OfficialUserExport,
    /// Anything else, including an undocumented web API.
    Unofficial {
        /// Non-secret description of where it came from, for the refusal text.
        description: String,
    },
}

impl VendorExportPath {
    /// Whether this path is one §7.5 permits importing from.
    #[must_use]
    pub const fn is_official(&self) -> bool {
        matches!(self, Self::OfficialCli { .. } | Self::OfficialUserExport)
    }
}

/// A vendor artifact we have observed alongside a source item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorArtifactRef {
    /// What the artifact is.
    pub kind: VendorArtifactKind,
    /// Vendor that produced it, for example `plaud`.
    pub source_vendor: String,
    /// Digest of the exported bytes, lowercase hex.
    pub content_sha256: String,
    /// Length of those bytes.
    pub byte_length: u64,
    /// Name the export used. Metadata only, never identity (§13.4).
    pub original_filename: Option<String>,
    /// Mechanism that produced it.
    pub export_path: VendorExportPath,
}

impl VendorArtifactRef {
    /// A transcript artifact described by its own bytes.
    #[must_use]
    pub fn transcript(
        vendor: impl Into<String>,
        bytes: &[u8],
        export_path: VendorExportPath,
    ) -> Self {
        Self {
            kind: VendorArtifactKind::Transcript,
            source_vendor: vendor.into(),
            content_sha256: Sha256Digest::of(bytes).to_hex(),
            byte_length: bytes.len() as u64,
            original_filename: None,
            export_path,
        }
    }

    /// Attach the export's own file name as metadata.
    #[must_use]
    pub fn with_original_filename(mut self, name: impl Into<String>) -> Self {
        self.original_filename = Some(name.into());
        self
    }
}

/// The three §7.5 behaviours a user may choose per source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VendorImportMode {
    /// Keep the vendor revision and do not run local AI over the audio.
    ImportVendorOnly,
    /// Keep the vendor revision *and* our own, side by side.
    RetainBoth,
    /// Import audio only. The conservative default (§7.5, §3.1).
    #[default]
    SkipVendorContent,
}

impl VendorImportMode {
    /// Column value stored in `source_export_artifacts.import_mode`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImportVendorOnly => "import_vendor_only",
            Self::RetainBoth => "retain_both",
            Self::SkipVendorContent => "skip_vendor_content",
        }
    }
}

/// Whether a supported official export path has actually been verified.
///
/// §18.6 requires the feature to stay disabled until that verification exists.
/// The production constructor is [`Self::production_default`] and is disabled;
/// enabling the feature requires naming the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorImportCapability {
    verified_evidence: Option<String>,
}

impl Default for VendorImportCapability {
    fn default() -> Self {
        Self::production_default()
    }
}

impl VendorImportCapability {
    /// What this build ships with: no verified export path, so no imports.
    #[must_use]
    pub const fn production_default() -> Self {
        Self {
            verified_evidence: None,
        }
    }

    /// Enable the feature, naming the evidence that an official export path
    /// exists and was tested (§2.5B, §18.6).
    #[must_use]
    pub fn verified_official_export(evidence: impl Into<String>) -> Self {
        Self {
            verified_evidence: Some(evidence.into()),
        }
    }

    /// Whether vendor content may be imported at all.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.verified_evidence.is_some()
    }

    /// The evidence record backing an enabled capability.
    #[must_use]
    pub fn evidence(&self) -> Option<&str> {
        self.verified_evidence.as_deref()
    }
}

/// Why an artifact was not imported. Every variant carries a fixed explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorImportRefusal {
    /// No official export path has been verified for this build (§18.6).
    CapabilityNotVerified,
    /// The artifact came from an unofficial mechanism (§7.5).
    UnofficialExportPath,
    /// The user's mode for this source is `skip_vendor_content`.
    ModeSkipsVendorContent,
    /// A human-corrected transcript exists and the mode would displace it.
    HumanCorrectedTranscriptExists,
    /// The supplied bytes do not match the digest or length we were told.
    ContentDigestMismatch,
}

impl VendorImportRefusal {
    /// User-facing explanation, from a fixed vocabulary (§15.4).
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::CapabilityNotVerified => {
                "Importing vendor transcripts is disabled because no supported official export path has been verified for this build."
            }
            Self::UnofficialExportPath => {
                "This transcript did not come from a supported official export, so it was not imported."
            }
            Self::ModeSkipsVendorContent => {
                "This source is set to import audio only, so the vendor transcript was recorded but not imported."
            }
            Self::HumanCorrectedTranscriptExists => {
                "You have corrected this transcript. Choose 'keep both' if you also want the vendor version."
            }
            Self::ContentDigestMismatch => {
                "The exported file does not match the digest reported for it, so it was not imported."
            }
        }
    }
}

/// What an import attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorImportOutcome {
    /// A vendor revision was created.
    Imported {
        /// The new revision.
        revision_id: TranscriptRevisionId,
        /// The `source_export_artifacts` row describing the original bytes.
        artifact_id: Uuid,
        /// Whether a human-corrected revision exists alongside it and remains
        /// the preferred one.
        human_edit_retained: bool,
    },
    /// Nothing was imported. The artifact is still recorded, so the UI can
    /// explain that a vendor transcript exists and why it was not used.
    Refused {
        /// Which rule refused.
        refusal: VendorImportRefusal,
        /// The `source_export_artifacts` row, when the artifact was recorded.
        artifact_id: Option<Uuid>,
    },
}

/// One import attempt, fully described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorImportRequest {
    /// Recording the artifact belongs to.
    pub recording_id: RecordingId,
    /// Association the artifact was exported alongside.
    pub source_item_id: SourceItemId,
    /// The artifact itself.
    pub artifact: VendorArtifactRef,
    /// The user's configured behaviour for this source.
    pub mode: VendorImportMode,
    /// Whether an official export path has been verified.
    pub capability: VendorImportCapability,
}

/// Imports vendor-generated artifacts as explicitly labelled revisions.
#[derive(Debug, Clone)]
pub struct VendorArtifactImporter {
    revisions: TranscriptRepository,
    provenance: TranscriptProvenanceRepository,
    artifacts: SourceExportArtifactRepository,
}

impl VendorArtifactImporter {
    /// Build an importer over an open database.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            revisions: TranscriptRepository::new(db.clone()),
            provenance: TranscriptProvenanceRepository::new(db.clone()),
            artifacts: SourceExportArtifactRepository::new(db),
        }
    }

    /// Import one transcript artifact, or refuse it with a stated reason.
    ///
    /// `body` is the exported text exactly as it was on disk. It is hashed and
    /// checked against the digest the caller reported before anything is
    /// written, so a mismatched or truncated export cannot become a revision.
    pub async fn import_transcript(
        &self,
        request: &VendorImportRequest,
        body: &str,
        at: Timestamp,
    ) -> PersistenceResult<VendorImportOutcome> {
        let bytes = body.as_bytes();
        let digest = Sha256Digest::of(bytes);
        if digest.to_hex() != request.artifact.content_sha256
            || bytes.len() as u64 != request.artifact.byte_length
        {
            // Nothing is recorded: we cannot attest to bytes we did not get.
            return Ok(VendorImportOutcome::Refused {
                refusal: VendorImportRefusal::ContentDigestMismatch,
                artifact_id: None,
            });
        }

        if !request.capability.is_enabled() {
            let artifact_id = self.record_artifact(request, None, at).await?;
            return Ok(VendorImportOutcome::Refused {
                refusal: VendorImportRefusal::CapabilityNotVerified,
                artifact_id: Some(artifact_id),
            });
        }
        if !request.artifact.export_path.is_official() {
            let artifact_id = self.record_artifact(request, None, at).await?;
            return Ok(VendorImportOutcome::Refused {
                refusal: VendorImportRefusal::UnofficialExportPath,
                artifact_id: Some(artifact_id),
            });
        }
        if request.mode == VendorImportMode::SkipVendorContent {
            let artifact_id = self.record_artifact(request, None, at).await?;
            return Ok(VendorImportOutcome::Refused {
                refusal: VendorImportRefusal::ModeSkipsVendorContent,
                artifact_id: Some(artifact_id),
            });
        }

        // App. A.2: a vendor export never silently replaces human-corrected
        // text. `retain_both` is the only mode that may sit beside one, and
        // even then the human edit stays preferred.
        let human_edit = self.provenance.has_human_edit(request.recording_id).await?;
        if human_edit && request.mode != VendorImportMode::RetainBoth {
            let artifact_id = self.record_artifact(request, None, at).await?;
            return Ok(VendorImportOutcome::Refused {
                refusal: VendorImportRefusal::HumanCorrectedTranscriptExists,
                artifact_id: Some(artifact_id),
            });
        }

        let segments: Vec<TranscriptSegmentDraft> = body
            .lines()
            .enumerate()
            .map(|(index, line)| {
                TranscriptSegmentDraft::text_only(u32::try_from(index).unwrap_or(u32::MAX), line)
            })
            .collect();
        let engine = EngineId::new(VENDOR_EXPORT_ENGINE);
        let model = ModelIdentity::new(
            engine.clone(),
            format!("{}:export", request.artifact.source_vendor),
        );
        // A vendor export carries no timing we verified, so the revision is
        // explicitly seek-free rather than silently timestamped (§8.6A).
        let draft = TranscriptDraft::text_only(engine, model, digest, segments);
        let context = RevisionContext {
            recording_id: request.recording_id,
            parent_id: None,
            origin: RevisionOrigin::Machine,
            status: RevisionStatus::Complete,
            created_at: at,
            provenance: None,
        };
        let revision_id = self.revisions.save_draft(&draft, &context).await?;
        self.provenance
            .set_source_vendor(revision_id, &request.artifact.source_vendor)
            .await?;
        let artifact_id = self.record_artifact(request, Some(at), at).await?;

        Ok(VendorImportOutcome::Imported {
            revision_id,
            artifact_id,
            human_edit_retained: human_edit,
        })
    }

    /// The `source_export_artifacts` row for one artifact, written whether or
    /// not the artifact was imported.
    async fn record_artifact(
        &self,
        request: &VendorImportRequest,
        imported_at: Option<Timestamp>,
        at: Timestamp,
    ) -> PersistenceResult<Uuid> {
        let existing = self
            .artifacts
            .list_for_item(request.source_item_id)
            .await?
            .into_iter()
            .find(|row| {
                row.artifact_kind == request.artifact.kind.as_str()
                    && row.content_sha256 == request.artifact.content_sha256
            });
        let id = existing.map_or_else(Uuid::now_v7, |row| row.id);
        let artifact = SourceExportArtifact {
            id,
            source_item_id: request.source_item_id,
            artifact_kind: request.artifact.kind.as_str().to_string(),
            source_vendor: request.artifact.source_vendor.clone(),
            content_sha256: request.artifact.content_sha256.clone(),
            byte_length: request.artifact.byte_length,
            original_filename: request.artifact.original_filename.clone(),
            imported_at,
            import_mode: request.mode.as_str().to_string(),
            created_at: at,
        };
        self.artifacts.record(&artifact).await?;
        Ok(id)
    }

    /// Artifacts recorded for one association, imported or refused.
    pub async fn artifacts_for(
        &self,
        source_item_id: SourceItemId,
    ) -> PersistenceResult<Vec<SourceExportArtifact>> {
        self.artifacts.list_for_item(source_item_id).await
    }

    /// Artifacts recorded for every association of one recording, which is
    /// what a provenance badge in the UI needs.
    pub async fn artifacts_for_recording(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<Vec<SourceExportArtifact>> {
        self.artifacts.list_for_recording(recording_id).await
    }

    /// Provenance view used to assert that a human edit stays preferred.
    #[must_use]
    pub fn provenance(&self) -> &TranscriptProvenanceRepository {
        &self.provenance
    }
}
