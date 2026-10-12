//! Generated-output vocabulary shared by the pipeline and the repository
//! (spec 9.1, 9.4, 9.5).
//!
//! These types live in the domain rather than in the pipeline because both the
//! crate that produces an output and the crate that stores one need to agree
//! on them, and the dependency graph of spec 6.3(1) only allows that agreement
//! here.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::anchor::SourceAnchor;
use crate::ids::{StageRunId, TranscriptRevisionId};
use crate::media::Sha256Digest;
use crate::model::ModelIdentity;
use crate::time::Timestamp;

/// How the structured-output contract was enforced on the provider side (spec 9.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityMode {
    /// The provider accepted a JSON Schema response format.
    SchemaEnforced,
    /// The provider does not support response formats; JSON was requested in
    /// the prompt and bounded repair is permitted.
    PromptConstrained,
}

impl CompatibilityMode {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SchemaEnforced => "schema_enforced",
            Self::PromptConstrained => "prompt_constrained",
        }
    }

    /// Parse a persisted wire string.
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        match raw {
            "schema_enforced" => Some(Self::SchemaEnforced),
            "prompt_constrained" => Some(Self::PromptConstrained),
            _ => None,
        }
    }
}

/// A stage of the §9.1 pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    /// Canonicalize and hash the input revision.
    Normalize,
    /// Summarize one chronological chunk.
    SectionSummary,
    /// Consolidate section summaries.
    ConsolidatedSummary,
    /// Produce the structured record.
    StructuredExtraction,
    /// Resolve every anchor against the immutable revision.
    CitationVerification,
}

impl StageKind {
    /// Every stage in execution order.
    pub const ORDER: &'static [Self] = &[
        Self::Normalize,
        Self::SectionSummary,
        Self::ConsolidatedSummary,
        Self::StructuredExtraction,
        Self::CitationVerification,
    ];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normalize => "normalize",
            Self::SectionSummary => "section_summary",
            Self::ConsolidatedSummary => "consolidated_summary",
            Self::StructuredExtraction => "structured_extraction",
            Self::CitationVerification => "citation_verification",
        }
    }

    /// Parse a persisted wire string.
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        Self::ORDER
            .iter()
            .copied()
            .find(|stage| stage.as_str() == raw)
    }

    /// Version of this stage's implementation.
    ///
    /// Bumping a stage version changes the job key and therefore produces a
    /// new output revision rather than overwriting the previous one.
    #[must_use]
    pub const fn stage_version(self) -> &'static str {
        match self {
            Self::Normalize
            | Self::SectionSummary
            | Self::ConsolidatedSummary
            | Self::StructuredExtraction
            | Self::CitationVerification => "1.0.0",
        }
    }

    /// Whether this stage dispatches to a model.
    #[must_use]
    pub const fn is_inference(self) -> bool {
        matches!(
            self,
            Self::SectionSummary | Self::ConsolidatedSummary | Self::StructuredExtraction
        )
    }
}

/// Everything recorded about one executed stage (spec 9.1, 13.3 `tool_runs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageProvenance {
    /// Stage run identity.
    pub id: StageRunId,
    /// Position in the run, starting at zero.
    pub sequence: u32,
    /// Which stage ran.
    pub stage: StageKind,
    /// Version of the stage implementation.
    pub stage_version: String,
    /// Version of the pipeline as a whole.
    pub pipeline_version: String,
    /// Exact transcript revision the stage read.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Digest of the stage's canonical input.
    pub input_sha256: Sha256Digest,
    /// Digest of the stage's canonical output.
    pub output_sha256: Sha256Digest,
    /// Model that produced the output, for inference stages.
    pub model: Option<ModelIdentity>,
    /// How the structured-output contract was enforced, for inference stages.
    pub compatibility_mode: Option<CompatibilityMode>,
    /// Bounded repair attempts consumed by this stage.
    pub repair_attempts: u8,
    /// Version of the template's prompt text.
    pub prompt_version: String,
    /// Version of the prompt envelope this build assembles.
    pub prompt_envelope_version: String,
    /// Template identity.
    pub template_id: String,
    /// Template version.
    pub template_version: String,
    /// Version of the response schema the stage required.
    pub schema_version: String,
    /// Segment range the stage read, for chunked stages.
    pub segment_range: Option<Range<u32>>,
    /// Previous stage run whose output was reused instead of recomputed.
    pub reused_from: Option<StageRunId>,
    /// When the stage started.
    pub started_at: Timestamp,
    /// When the stage ended.
    pub ended_at: Timestamp,
}

impl StageProvenance {
    /// Whether every provenance field required by spec 9.1 is populated.
    ///
    /// Used by the completeness property test and by the repository before a
    /// row is written: a stage without provenance is never persisted.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.stage_version.is_empty()
            && !self.pipeline_version.is_empty()
            && !self.prompt_version.is_empty()
            && !self.prompt_envelope_version.is_empty()
            && !self.template_id.is_empty()
            && !self.template_version.is_empty()
            && !self.schema_version.is_empty()
            && (!self.stage.is_inference()
                || self.reused_from.is_some()
                || (self.model.is_some() && self.compatibility_mode.is_some()))
    }
}

/// One bullet of a section or consolidated summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryBullet {
    /// The bullet text.
    pub text: String,
    /// Anchors the model supplied for the bullet.
    #[serde(default)]
    pub anchors: Vec<SourceAnchor>,
}

/// The response shape of a section or consolidated summary stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryResponse {
    /// Bullets produced for this chunk.
    pub bullets: Vec<SummaryBullet>,
    /// First segment index the model reports having read.
    pub covered_first_segment: u32,
    /// Last segment index the model reports having read.
    pub covered_last_segment: u32,
}

/// A persisted section summary, reusable by a targeted regeneration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSummary {
    /// Stage run that produced it.
    pub stage_run_id: StageRunId,
    /// Chunk index within the plan.
    pub chunk_index: u32,
    /// Segment range the chunk contained.
    pub segments: Range<u32>,
    /// Digest of the exact segment text the chunk contained.
    pub input_sha256: Sha256Digest,
    /// The model's output.
    pub response: SummaryResponse,
}

impl SectionSummary {
    /// Whether the model reported reading the whole chunk it was given.
    #[must_use]
    pub fn covers_its_chunk(&self) -> bool {
        self.response.covered_first_segment <= self.segments.start
            && self.response.covered_last_segment + 1 >= self.segments.end
    }
}

/// How much of the transcript a finished output actually reflects (spec 9.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CoverageState {
    /// Every segment was read, and every model reported reading its chunk.
    Full,
    /// Every segment was sent, but a model reported reading less than it was
    /// given. The output is usable but is not claimed to be complete.
    ModelReportedPartial {
        /// Chunk indices whose model under-reported coverage.
        chunks: Vec<u32>,
    },
}

impl CoverageState {
    /// Whether the output may be presented as a complete summary.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ModelReportedPartial { .. } => "model_reported_partial",
        }
    }
}

/// Lifecycle status of a generated output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStatus {
    /// Every item passed validation and grounding.
    Complete,
    /// Usable, but items were rejected or claims need review.
    NeedsReview,
}

impl OutputStatus {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::NeedsReview => "needs_review",
        }
    }

    /// Parse a persisted wire string.
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        match raw {
            "complete" => Some(Self::Complete),
            "needs_review" => Some(Self::NeedsReview),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_kinds_round_trip_and_name_the_nine_one_pipeline() {
        assert_eq!(StageKind::ORDER.len(), 5);
        for stage in StageKind::ORDER {
            assert_eq!(StageKind::from_str_opt(stage.as_str()), Some(*stage));
            assert_eq!(stage.stage_version(), "1.0.0");
        }
        assert_eq!(StageKind::from_str_opt("nope"), None);
    }

    #[test]
    fn only_model_bearing_stages_are_inference_stages() {
        assert!(!StageKind::Normalize.is_inference());
        assert!(!StageKind::CitationVerification.is_inference());
        assert!(StageKind::SectionSummary.is_inference());
        assert!(StageKind::ConsolidatedSummary.is_inference());
        assert!(StageKind::StructuredExtraction.is_inference());
    }

    #[test]
    fn compatibility_modes_round_trip() {
        for mode in [
            CompatibilityMode::SchemaEnforced,
            CompatibilityMode::PromptConstrained,
        ] {
            assert_eq!(CompatibilityMode::from_str_opt(mode.as_str()), Some(mode));
        }
        assert_eq!(CompatibilityMode::from_str_opt("guessed"), None);
    }

    #[test]
    fn a_section_that_reports_a_narrower_range_does_not_cover_its_chunk() {
        let section = SectionSummary {
            stage_run_id: StageRunId::new(),
            chunk_index: 0,
            segments: 0..4,
            input_sha256: Sha256Digest::of(b"x"),
            response: SummaryResponse {
                bullets: Vec::new(),
                covered_first_segment: 0,
                covered_last_segment: 1,
            },
        };
        assert!(!section.covers_its_chunk());
    }

    #[test]
    fn a_section_reporting_its_whole_chunk_covers_it() {
        let section = SectionSummary {
            stage_run_id: StageRunId::new(),
            chunk_index: 0,
            segments: 2..5,
            input_sha256: Sha256Digest::of(b"x"),
            response: SummaryResponse {
                bullets: Vec::new(),
                covered_first_segment: 2,
                covered_last_segment: 4,
            },
        };
        assert!(section.covers_its_chunk());
    }

    #[test]
    fn an_inference_stage_without_a_model_or_a_reuse_link_is_incomplete() {
        let mut stage = StageProvenance {
            id: StageRunId::new(),
            sequence: 0,
            stage: StageKind::StructuredExtraction,
            stage_version: "1.0.0".into(),
            pipeline_version: "1.0.0".into(),
            transcript_revision_id: TranscriptRevisionId::new(),
            input_sha256: Sha256Digest::of(b"a"),
            output_sha256: Sha256Digest::of(b"b"),
            model: None,
            compatibility_mode: None,
            repair_attempts: 0,
            prompt_version: "1.0.0".into(),
            prompt_envelope_version: "1.0.0".into(),
            template_id: "general_meeting".into(),
            template_version: "1.0.0".into(),
            schema_version: "1.0.0".into(),
            segment_range: None,
            reused_from: None,
            started_at: Timestamp::from_millis(0),
            ended_at: Timestamp::from_millis(1),
        };
        assert!(!stage.is_complete());

        stage.reused_from = Some(StageRunId::new());
        assert!(stage.is_complete());

        stage.reused_from = None;
        stage.model = Some(ModelIdentity::new(crate::model::EngineId::new("e"), "m"));
        stage.compatibility_mode = Some(CompatibilityMode::SchemaEnforced);
        assert!(stage.is_complete());

        stage.template_version = String::new();
        assert!(!stage.is_complete(), "a version is never optional");
    }

    #[test]
    fn coverage_state_distinguishes_full_from_model_reported_partial() {
        assert!(CoverageState::Full.is_full());
        let partial = CoverageState::ModelReportedPartial { chunks: vec![1] };
        assert!(!partial.is_full());
        assert_eq!(partial.as_str(), "model_reported_partial");
    }
}
