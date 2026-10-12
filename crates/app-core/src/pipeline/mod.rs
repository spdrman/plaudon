//! The versioned summarization pipeline (spec 9.1, tasks I1.1 and I1.3).
//!
//! `transcript → normalize/segment → section summaries → consolidated summary
//! → structured extraction → citation verification → editable outputs`.
//!
//! Every stage records the exact transcript revision it read plus the model,
//! prompt, template, schema and stage versions it ran with, so a stored output
//! is replayable and a change to any one of them produces a distinguishable
//! new output revision rather than a silent overwrite.
//!
//! Three refusals are structural:
//!
//! * A model whose capability payload does not establish identity, structured
//!   output support and availability is never dispatched to.
//! * A chunk plan that cannot cover the whole transcript fails; nothing is
//!   truncated and then presented as a full summary.
//! * A cancelled or failed stage produces no output at all; there is no code
//!   path that returns a partial [`PipelineOutcome`].

pub mod grounding;
pub mod prompt;
pub mod staleness;

#[cfg(test)]
pub(crate) mod test_support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use plaudon_ai_contracts::capabilities::TextModelCapabilities;
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::template::OutputTemplate;
use plaudon_ai_contracts::text::{StructuredRequest, TextModel, TextModelError};
use plaudon_ai_contracts::validation::{
    AcceptedItem, CompatibilityMode, OutputValidator, RejectedItem, RepairPolicy, SpeakerDirectory,
    ValidationReport, ValidatorBuildError,
};
#[allow(unused_imports)]
use plaudon_domain::anchor::SourceAnchor;
use plaudon_domain::capability::Tri;
use plaudon_domain::claim::{Claim, GroundingSummary};
use plaudon_domain::ids::{OutputId, RecordingId, StageRunId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::ModelIdentity;
use plaudon_domain::output::OutputStatus;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::TranscriptRevision;
use plaudon_jobs::key::{ProcessingJobKey, ProcessingStage};
use plaudon_persistence::intelligence::GeneratedOutput;
use serde::{Deserialize, Serialize};

use crate::chunking::{
    plan_chunks_for_revision, ChunkError, ChunkPlan, ChunkPolicy, ContextBudget, Coverage,
};

/// Version of the pipeline itself. Embedded in every result (spec 9.1).
pub const PIPELINE_VERSION: &str = "1.0.0";

/// How many times a context overflow may trigger a re-plan with a smaller
/// budget before the run fails.
///
/// Spec 9.4: overflow is handled by re-chunking, never by dropping the tail.
/// The bound exists so a provider that always reports overflow cannot spin.
pub const MAX_CONTEXT_SHRINKS: u8 = 2;

/// A clock, so a pipeline run is deterministic under test.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Current instant.
    fn now(&self) -> Timestamp;
}

/// Wall-clock implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that advances by a fixed step on each read, for tests.
#[derive(Debug)]
pub struct SteppingClock {
    start_millis: i64,
    step_millis: i64,
    reads: std::sync::atomic::AtomicI64,
}

impl SteppingClock {
    /// Construct a clock starting at `start_millis`, advancing by `step_millis`.
    #[must_use]
    pub fn new(start_millis: i64, step_millis: i64) -> Self {
        Self {
            start_millis,
            step_millis,
            reads: std::sync::atomic::AtomicI64::new(0),
        }
    }
}

impl Clock for SteppingClock {
    fn now(&self) -> Timestamp {
        let reads = self
            .reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Timestamp::from_millis(self.start_millis + reads * self.step_millis)
    }
}

/// The §9.1 stage vocabulary and the provenance record each stage writes.
///
/// These live in [`plaudon_domain::output`] because the repository that
/// stores an output and the pipeline that produces one must agree on them,
/// and only the domain crate sits below both.
pub use plaudon_domain::output::{
    CoverageState, SectionSummary, StageKind, StageProvenance, SummaryBullet, SummaryResponse,
};

/// The job-key stage (§14.2) corresponding to a pipeline stage.
#[must_use]
pub const fn job_stage(stage: StageKind) -> ProcessingStage {
    match stage {
        StageKind::Normalize => ProcessingStage::Normalize,
        StageKind::SectionSummary => ProcessingStage::SectionSummary,
        StageKind::ConsolidatedSummary => ProcessingStage::ConsolidatedSummary,
        StageKind::StructuredExtraction => ProcessingStage::StructuredExtraction,
        StageKind::CitationVerification => ProcessingStage::CitationVerification,
    }
}

/// Inputs of one pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineRequest {
    /// Recording the output belongs to.
    pub recording_id: RecordingId,
    /// The exact immutable revision to summarize.
    pub revision: TranscriptRevision,
    /// Template to run.
    pub template: OutputTemplate,
    /// Resolved JSON Schema for the extraction stage.
    pub extraction_schema: serde_json::Value,
    /// Resolved JSON Schema for section and consolidated summaries.
    pub summary_schema: serde_json::Value,
    /// Probed capabilities of the selected text model.
    pub capabilities: TextModelCapabilities,
    /// Chunking parameters.
    pub chunk_policy: ChunkPolicy,
    /// Bounded repair policy.
    pub repair_policy: RepairPolicy,
    /// Speaker labels the recording actually has.
    pub speakers: SpeakerDirectory,
    /// Section summaries that may be reused instead of recomputed (I2.4).
    pub reusable_sections: Vec<SectionSummary>,
    /// Snapshot identity of the privacy policy resolved for this run (§10.8).
    pub privacy_policy_snapshot: Option<String>,
    /// Digest of the media the transcript was produced from, for the job key.
    pub audio_sha256: Sha256Digest,
}

impl PipelineRequest {
    /// Canonical digest of the transcript content this run consumes.
    #[must_use]
    pub fn transcript_digest(&self) -> Sha256Digest {
        canonical_transcript_digest(&self.revision)
    }

    /// Job key for the run as a whole (spec 14.2).
    ///
    /// Re-running with identical inputs and versions produces the same key, so
    /// the job engine deduplicates it; changing any version dimension produces
    /// a different key and therefore a new output.
    #[must_use]
    pub fn job_key(&self) -> ProcessingJobKey {
        self.stage_job_key(StageKind::StructuredExtraction)
    }

    /// Job key for one stage of the run.
    #[must_use]
    pub fn stage_job_key(&self, stage: StageKind) -> ProcessingJobKey {
        let mut normalization = BTreeMap::new();
        normalization.insert(
            "normalization_profile".to_owned(),
            self.revision.normalization_profile.clone(),
        );
        normalization.insert(
            "chunking_strategy".to_owned(),
            self.revision.chunking_strategy.clone(),
        );
        normalization.insert("pipeline_version".to_owned(), PIPELINE_VERSION.to_owned());
        normalization.insert("stage_version".to_owned(), stage.stage_version().to_owned());
        normalization.insert(
            "prompt_envelope_version".to_owned(),
            prompt::PROMPT_ENVELOPE_VERSION.to_owned(),
        );
        normalization.insert(
            "output_schema_version".to_owned(),
            self.template.output_schema_version.clone(),
        );
        normalization.insert(
            "transcript_sha256".to_owned(),
            self.transcript_digest().to_hex(),
        );

        let mut key = ProcessingJobKey::new(
            self.audio_sha256.to_hex(),
            job_stage(stage),
            self.capabilities.engine_id.as_str().to_owned(),
            self.capabilities.model.model_id.clone(),
        );
        key.transcript_revision = Some(self.revision.id.to_string());
        key.model_version
            .clone_from(&self.capabilities.model.model_version);
        key.normalization_config = normalization;
        key.prompt_template_version = Some(format!(
            "{}@{}+prompt:{}",
            self.template.id, self.template.version, self.template.prompt_version
        ));
        key.privacy_policy_snapshot
            .clone_from(&self.privacy_policy_snapshot);
        key
    }
}

/// Result of a completed pipeline run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineOutcome {
    /// Identity of the generated output revision.
    pub output_id: OutputId,
    /// Recording the output belongs to.
    pub recording_id: RecordingId,
    /// Exact transcript revision every stage read.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Version of the pipeline.
    pub pipeline_version: String,
    /// Template identity and version.
    pub template_id: String,
    /// Template version.
    pub template_version: String,
    /// Version of the template's prompt text.
    pub prompt_version: String,
    /// Version of the output schema.
    pub schema_version: String,
    /// Digest of the transcript content consumed.
    pub input_sha256: Sha256Digest,
    /// Digest of the canonical output body.
    pub output_sha256: Sha256Digest,
    /// The accepted items, grouped by template field.
    pub body: serde_json::Value,
    /// One claim per accepted item, with grounding state.
    pub claims: Vec<Claim>,
    /// Aggregate grounding statistics (§17.3).
    pub grounding: GroundingSummary,
    /// Items the validator rejected, reported rather than dropped.
    pub rejected: Vec<RejectedItem>,
    /// Coverage of the transcript.
    pub coverage: CoverageState,
    /// How the structured output contract was enforced.
    pub compatibility_mode: CompatibilityMode,
    /// Apparent instructions found inside transcript text (§9.6).
    pub detected_directives: Vec<prompt::DetectedDirective>,
    /// Per-stage provenance, in execution order.
    pub stages: Vec<StageProvenance>,
    /// Section summaries, retained for reuse by targeted regeneration.
    pub section_summaries: Vec<SectionSummary>,
    /// When the run finished.
    pub created_at: Timestamp,
}

impl PipelineOutcome {
    /// Whether every stage recorded complete provenance.
    #[must_use]
    pub fn provenance_is_complete(&self) -> bool {
        !self.stages.is_empty() && self.stages.iter().all(StageProvenance::is_complete)
    }

    /// Claims that may appear in a "verified decisions" view (spec 9.6).
    #[must_use]
    pub fn verified_claims(&self) -> Vec<&Claim> {
        self.claims
            .iter()
            .filter(|claim| claim.is_verified_view_eligible())
            .collect()
    }

    /// Claims presented as **Review required**.
    #[must_use]
    pub fn review_required_claims(&self) -> Vec<&Claim> {
        self.claims
            .iter()
            .filter(|claim| claim.review_required())
            .collect()
    }

    /// Whether the output needs human attention before it can be trusted.
    #[must_use]
    pub fn status(&self) -> OutputStatus {
        if self.rejected.is_empty()
            && self.coverage.is_full()
            && self.review_required_claims().is_empty()
        {
            OutputStatus::Complete
        } else {
            OutputStatus::NeedsReview
        }
    }

    /// Convert into the record the repository stores.
    ///
    /// `output_type` is the §13.2 `ai_outputs.output_type` discriminator, for
    /// example `meeting_summary`.
    #[must_use]
    pub fn to_generated_output(&self, output_type: impl Into<String>) -> GeneratedOutput {
        GeneratedOutput {
            id: self.output_id,
            recording_id: self.recording_id,
            transcript_revision_id: self.transcript_revision_id,
            output_type: output_type.into(),
            pipeline_version: self.pipeline_version.clone(),
            template_id: self.template_id.clone(),
            template_version: self.template_version.clone(),
            model_id: self
                .stages
                .iter()
                .rev()
                .find_map(|stage| stage.model.as_ref().map(ToString::to_string))
                .unwrap_or_default(),
            prompt_version: self.prompt_version.clone(),
            schema_version: self.schema_version.clone(),
            input_sha256: self.input_sha256,
            output_sha256: self.output_sha256,
            body: self.body.clone(),
            status: self.status(),
            coverage: self.coverage.clone(),
            compatibility_mode: self.compatibility_mode,
            grounding: self.grounding,
            rejected: self.rejected.clone(),
            directives: serde_json::to_value(&self.detected_directives)
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
            stages: self.stages.clone(),
            section_summaries: self.section_summaries.clone(),
            claims: self.claims.clone(),
            created_at: self.created_at,
        }
    }
}

/// Why a run did not produce an output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// The model's capability payload does not establish that it can be used.
    #[error("the selected text model is not verified for this configuration")]
    CapabilityUnverified,
    /// The model does not say whether it supports structured output.
    #[error("the selected text model's structured-output support is unknown")]
    RequestFamilyUnknown,
    /// Chunk planning failed.
    #[error("the transcript could not be chunked: {0}")]
    Chunking(#[from] ChunkError),
    /// The plan does not cover the whole transcript.
    #[error("chunking left {} segment range(s) unread", uncovered.len())]
    CoverageIncomplete {
        /// Ranges no chunk would have read.
        uncovered: Vec<Range<u32>>,
    },
    /// A model call failed.
    #[error("the text model failed: {0}")]
    Model(#[from] TextModelError),
    /// The model asked the application to act.
    #[error("the model response was refused: {0}")]
    UnsafeResponse(#[from] prompt::UnsafeResponse),
    /// A summary stage returned a body that is not a valid summary document.
    #[error("the {stage} stage returned a body that does not match its schema")]
    SummarySchema {
        /// Stage that produced it.
        stage: &'static str,
    },
    /// Extraction produced nothing usable after bounded repair.
    #[error("extraction produced no usable item after {attempts} repair attempt(s)")]
    ExtractionRejected {
        /// Repair attempts consumed.
        attempts: u8,
        /// Why each item was rejected.
        rejected: Vec<RejectedItem>,
    },
    /// The expected output schema does not compile.
    #[error("the template's output schema is unusable: {0}")]
    Validator(#[from] ValidatorBuildError),
    /// The run was cancelled; no partial output exists.
    #[error("the run was cancelled before it produced an output")]
    Cancelled,
}

/// The §9.1 pipeline.
#[derive(Clone)]
pub struct SummarizationPipeline {
    model: Arc<dyn TextModel>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for SummarizationPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummarizationPipeline")
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl SummarizationPipeline {
    /// Construct a pipeline over a text model.
    #[must_use]
    pub fn new(model: Arc<dyn TextModel>, clock: Arc<dyn Clock>) -> Self {
        Self { model, clock }
    }

    /// Run every stage, or fail without producing an output.
    pub async fn run(
        &self,
        request: PipelineRequest,
        ctx: &JobContext,
    ) -> Result<PipelineOutcome, PipelineError> {
        let compatibility_mode = preflight(&request.capabilities)?;
        let mut stages: Vec<StageProvenance> = Vec::new();
        let input_sha256 = request.transcript_digest();

        // ---------------------------------------------------- normalize
        let started = self.clock.now();
        let detected_directives = prompt::detect_directives(request.revision.segments());
        let normalize_id = StageRunId::new();
        stages.push(self.provenance(
            normalize_id,
            0,
            StageKind::Normalize,
            &request,
            input_sha256,
            input_sha256,
            None,
            None,
            0,
            None,
            None,
            started,
        ));
        Self::guard_cancel(ctx)?;

        // --------------------------------------------- section summaries
        let mut budget =
            ContextBudget::from_capabilities(&request.capabilities, &request.chunk_policy)?;
        let mut plan = Self::plan(&request, &budget)?;
        let mut shrinks = 0_u8;
        let (sections, section_stages) = loop {
            match self
                .summarize_sections(&request, &plan, compatibility_mode, ctx, stages.len())
                .await
            {
                Ok(result) => break result,
                Err(PipelineError::Model(TextModelError::ContextTooLarge))
                    if shrinks < MAX_CONTEXT_SHRINKS =>
                {
                    shrinks += 1;
                    budget = budget.shrunk(budget.context_tokens / 2);
                    plan = Self::plan(&request, &budget)?;
                }
                Err(error) => return Err(error),
            }
        };
        stages.extend(section_stages);
        Self::guard_cancel(ctx)?;

        // ------------------------------------------- consolidated summary
        let (consolidated, consolidate_stage) = self
            .consolidate(&request, &sections, compatibility_mode, ctx, stages.len())
            .await?;
        stages.push(consolidate_stage);
        Self::guard_cancel(ctx)?;

        // --------------------------------------------------- extraction
        let (report, extraction_stage) = self
            .extract(
                &request,
                &consolidated,
                compatibility_mode,
                ctx,
                stages.len(),
            )
            .await?;
        stages.push(extraction_stage);
        Self::guard_cancel(ctx)?;

        // ------------------------------------------ citation verification
        let verification_started = self.clock.now();
        let outcome =
            grounding::ground_items(&report.accepted, &request.revision, &request.template);
        let body = assemble_body(&request.template, &report.accepted);
        let output_sha256 = canonical_json_digest(&body);
        stages.push(self.provenance(
            StageRunId::new(),
            u32::try_from(stages.len()).unwrap_or(u32::MAX),
            StageKind::CitationVerification,
            &request,
            input_sha256,
            output_sha256,
            None,
            None,
            0,
            None,
            None,
            verification_started,
        ));

        let under_reporting: Vec<u32> = sections
            .iter()
            .filter(|section| !section.covers_its_chunk())
            .map(|section| section.chunk_index)
            .collect();
        let coverage = if under_reporting.is_empty() {
            CoverageState::Full
        } else {
            CoverageState::ModelReportedPartial {
                chunks: under_reporting,
            }
        };

        Ok(PipelineOutcome {
            output_id: OutputId::new(),
            recording_id: request.recording_id,
            transcript_revision_id: request.revision.id,
            pipeline_version: PIPELINE_VERSION.to_owned(),
            template_id: request.template.id.clone(),
            template_version: request.template.version.clone(),
            prompt_version: request.template.prompt_version.clone(),
            schema_version: request.template.output_schema_version.clone(),
            input_sha256,
            output_sha256,
            body,
            claims: outcome.claims,
            grounding: outcome.summary,
            rejected: report.rejected,
            coverage,
            compatibility_mode,
            detected_directives,
            stages,
            section_summaries: sections,
            created_at: self.clock.now(),
        })
    }

    fn plan(request: &PipelineRequest, budget: &ContextBudget) -> Result<ChunkPlan, PipelineError> {
        let plan = plan_chunks_for_revision(&request.revision, budget)?;
        match &plan.coverage {
            Coverage::Full => Ok(plan),
            Coverage::Incomplete { uncovered } => Err(PipelineError::CoverageIncomplete {
                uncovered: uncovered.clone(),
            }),
        }
    }

    async fn summarize_sections(
        &self,
        request: &PipelineRequest,
        plan: &ChunkPlan,
        compatibility_mode: CompatibilityMode,
        ctx: &JobContext,
        sequence_base: usize,
    ) -> Result<(Vec<SectionSummary>, Vec<StageProvenance>), PipelineError> {
        let mut sections = Vec::with_capacity(plan.chunks.len());
        let mut provenance = Vec::with_capacity(plan.chunks.len());

        for chunk in &plan.chunks {
            Self::guard_cancel(ctx)?;
            let started = self.clock.now();
            let segments: Vec<_> = request
                .revision
                .segments()
                .iter()
                .filter(|segment| chunk.segments.contains(&segment.seq_index))
                .cloned()
                .collect();
            let chunk_sha = canonical_segments_digest(&segments);
            let sequence = u32::try_from(sequence_base + provenance.len()).unwrap_or(u32::MAX);

            if let Some(reused) = request
                .reusable_sections
                .iter()
                .find(|candidate| candidate.input_sha256 == chunk_sha)
            {
                let id = StageRunId::new();
                provenance.push(self.provenance(
                    id,
                    sequence,
                    StageKind::SectionSummary,
                    request,
                    chunk_sha,
                    canonical_json_digest(
                        &serde_json::to_value(&reused.response).unwrap_or(serde_json::Value::Null),
                    ),
                    None,
                    None,
                    0,
                    Some(chunk.segments.clone()),
                    Some(reused.stage_run_id),
                    started,
                ));
                sections.push(SectionSummary {
                    stage_run_id: id,
                    chunk_index: chunk.index,
                    segments: chunk.segments.clone(),
                    input_sha256: chunk_sha,
                    response: reused.response.clone(),
                });
                continue;
            }

            let assembled = prompt::assemble(
                &request.template.instructions.system,
                &request.template.instructions.section_summary,
                &request.template.prompt_version,
                request.revision.id,
                &segments,
                &request.summary_schema,
            );
            let response = self
                .generate(request, &assembled, &request.summary_schema, ctx)
                .await?;
            prompt::scan_response(&response.body)?;
            let parsed: SummaryResponse =
                serde_json::from_value(response.body.clone()).map_err(|_| {
                    PipelineError::SummarySchema {
                        stage: "section_summary",
                    }
                })?;

            let id = StageRunId::new();
            provenance.push(self.provenance(
                id,
                sequence,
                StageKind::SectionSummary,
                request,
                chunk_sha,
                canonical_json_digest(&response.body),
                Some(request.capabilities.model.clone()),
                Some(compatibility_mode),
                response.repair_attempts,
                Some(chunk.segments.clone()),
                None,
                started,
            ));
            sections.push(SectionSummary {
                stage_run_id: id,
                chunk_index: chunk.index,
                segments: chunk.segments.clone(),
                input_sha256: chunk_sha,
                response: parsed,
            });
        }

        Ok((sections, provenance))
    }

    async fn consolidate(
        &self,
        request: &PipelineRequest,
        sections: &[SectionSummary],
        compatibility_mode: CompatibilityMode,
        ctx: &JobContext,
        sequence: usize,
    ) -> Result<(SummaryResponse, StageProvenance), PipelineError> {
        let started = self.clock.now();
        let rendered = render_sections(sections);
        let input_sha = Sha256Digest::of(rendered.as_bytes());
        let sequence = u32::try_from(sequence).unwrap_or(u32::MAX);

        // One section needs no consolidation; copying it is exact and costs
        // nothing, and the stage is still recorded so provenance stays total.
        if sections.len() == 1 {
            let response = sections[0].response.clone();
            let body = serde_json::to_value(&response).unwrap_or(serde_json::Value::Null);
            return Ok((
                response,
                self.provenance(
                    StageRunId::new(),
                    sequence,
                    StageKind::ConsolidatedSummary,
                    request,
                    input_sha,
                    canonical_json_digest(&body),
                    None,
                    None,
                    0,
                    None,
                    Some(sections[0].stage_run_id),
                    started,
                ),
            ));
        }

        let assembled = prompt::assemble_over_text(
            &request.template.instructions.system,
            &request.template.instructions.synthesis,
            &request.template.prompt_version,
            request.revision.id,
            "section summaries with their anchors",
            &rendered,
            &request.summary_schema,
        );
        let response = self
            .generate(request, &assembled, &request.summary_schema, ctx)
            .await?;
        prompt::scan_response(&response.body)?;
        let parsed: SummaryResponse =
            serde_json::from_value(response.body.clone()).map_err(|_| {
                PipelineError::SummarySchema {
                    stage: "consolidated_summary",
                }
            })?;

        Ok((
            parsed,
            self.provenance(
                StageRunId::new(),
                sequence,
                StageKind::ConsolidatedSummary,
                request,
                input_sha,
                canonical_json_digest(&response.body),
                Some(request.capabilities.model.clone()),
                Some(compatibility_mode),
                response.repair_attempts,
                None,
                None,
                started,
            ),
        ))
    }

    async fn extract(
        &self,
        request: &PipelineRequest,
        consolidated: &SummaryResponse,
        compatibility_mode: CompatibilityMode,
        ctx: &JobContext,
        sequence: usize,
    ) -> Result<(ValidationReport, StageProvenance), PipelineError> {
        let started = self.clock.now();
        let validator = OutputValidator::new(
            request.template.clone(),
            &request.extraction_schema,
            request.revision.id,
            request.speakers.clone(),
        )?;
        let rendered = render_bullets(&consolidated.bullets);
        let input_sha = Sha256Digest::of(rendered.as_bytes());

        let mut attempts = 0_u8;
        let mut instructions = request.template.instructions.extraction.clone();
        let mut last_body;
        let report = loop {
            Self::guard_cancel(ctx)?;
            let assembled = prompt::assemble_over_text(
                &request.template.instructions.system,
                &instructions,
                &request.template.prompt_version,
                request.revision.id,
                "the consolidated summary with its anchors",
                &rendered,
                &request.extraction_schema,
            );
            let response = self
                .generate(request, &assembled, &request.extraction_schema, ctx)
                .await?;
            prompt::scan_response(&response.body)?;
            last_body = response.body.clone();
            let report = validator.validate(&response.body, compatibility_mode, attempts);
            if report.is_clean() || !request.repair_policy.permits(attempts) {
                break report;
            }
            instructions = format!(
                "{}\n\n{}",
                request.template.instructions.extraction,
                report.repair_instruction()
            );
            attempts += 1;
        };

        if !report.is_usable() {
            return Err(PipelineError::ExtractionRejected {
                attempts,
                rejected: report.rejected,
            });
        }

        Ok((
            report,
            self.provenance(
                StageRunId::new(),
                u32::try_from(sequence).unwrap_or(u32::MAX),
                StageKind::StructuredExtraction,
                request,
                input_sha,
                canonical_json_digest(&last_body),
                Some(request.capabilities.model.clone()),
                Some(compatibility_mode),
                attempts,
                None,
                None,
                started,
            ),
        ))
    }

    async fn generate(
        &self,
        request: &PipelineRequest,
        assembled: &prompt::AssembledPrompt,
        schema: &serde_json::Value,
        ctx: &JobContext,
    ) -> Result<plaudon_ai_contracts::text::StructuredResponse, PipelineError> {
        Self::guard_cancel(ctx)?;
        let structured = StructuredRequest {
            prompt_version: format!(
                "{}+{}",
                assembled.prompt_version, assembled.envelope_version
            ),
            prompt: assembled.text.clone(),
            schema_version: request.template.output_schema_version.clone(),
            output_schema: schema.clone(),
            max_output_tokens: request.capabilities.max_output_tokens.get().copied(),
        };
        let response = self
            .model
            .generate_structured(structured, ctx.clone())
            .await?;
        Self::guard_cancel(ctx)?;
        Ok(response)
    }

    fn guard_cancel(ctx: &JobContext) -> Result<(), PipelineError> {
        if ctx.is_cancelled() {
            Err(PipelineError::Cancelled)
        } else {
            Ok(())
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn provenance(
        &self,
        id: StageRunId,
        sequence: u32,
        stage: StageKind,
        request: &PipelineRequest,
        input_sha256: Sha256Digest,
        output_sha256: Sha256Digest,
        model: Option<ModelIdentity>,
        compatibility_mode: Option<CompatibilityMode>,
        repair_attempts: u8,
        segment_range: Option<Range<u32>>,
        reused_from: Option<StageRunId>,
        started_at: Timestamp,
    ) -> StageProvenance {
        StageProvenance {
            id,
            sequence,
            stage,
            stage_version: stage.stage_version().to_owned(),
            pipeline_version: PIPELINE_VERSION.to_owned(),
            transcript_revision_id: request.revision.id,
            input_sha256,
            output_sha256,
            model,
            compatibility_mode,
            repair_attempts,
            prompt_version: request.template.prompt_version.clone(),
            prompt_envelope_version: prompt::PROMPT_ENVELOPE_VERSION.to_owned(),
            template_id: request.template.id.clone(),
            template_version: request.template.version.clone(),
            schema_version: request.template.output_schema_version.clone(),
            segment_range,
            reused_from,
            started_at,
            ended_at: self.clock.now(),
        }
    }
}

/// Decide the compatibility mode, or refuse to dispatch (spec 9.5, I1.1).
fn preflight(caps: &TextModelCapabilities) -> Result<CompatibilityMode, PipelineError> {
    if caps.model.model_id.trim().is_empty() || !caps.is_selectable() {
        return Err(PipelineError::CapabilityUnverified);
    }
    match caps.supports_structured_output {
        Tri::Yes => Ok(CompatibilityMode::SchemaEnforced),
        Tri::No => Ok(CompatibilityMode::PromptConstrained),
        Tri::Unknown => Err(PipelineError::RequestFamilyUnknown),
    }
}

/// Canonical digest of a revision's *content*.
///
/// Deliberately identity-free: it covers the ordered `(index, text)` pairs and
/// nothing else. That is what makes targeted regeneration safe and useful —
/// a new revision whose segment text is unchanged in a range has the same
/// digest for that range, so the section summary over it can be reused
/// instead of re-billed, while any edit to the text changes the digest.
/// Revision *identity* is tracked separately, by the dependency graph.
#[must_use]
pub fn canonical_transcript_digest(revision: &TranscriptRevision) -> Sha256Digest {
    canonical_segments_digest(revision.segments())
}

/// Canonical digest of an ordered slice of segments.
#[must_use]
pub fn canonical_segments_digest(
    segments: &[plaudon_domain::transcript::TranscriptSegment],
) -> Sha256Digest {
    let mut canonical = String::new();
    for segment in segments {
        let _ = writeln!(canonical, "{}\t{}", segment.seq_index, segment.text);
    }
    Sha256Digest::of(canonical.as_bytes())
}

/// Canonical digest of the segments of `revision` whose index is in `range`.
#[must_use]
pub fn canonical_range_digest(revision: &TranscriptRevision, range: &Range<u32>) -> Sha256Digest {
    let segments: Vec<_> = revision
        .segments()
        .iter()
        .filter(|segment| range.contains(&segment.seq_index))
        .cloned()
        .collect();
    canonical_segments_digest(&segments)
}

/// Digest of a JSON value in its canonical serialized form.
#[must_use]
pub fn canonical_json_digest(value: &serde_json::Value) -> Sha256Digest {
    Sha256Digest::of(canonical_json(value).as_bytes())
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(key, child)| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String(key.clone()),
                        canonical_json(child)
                    )
                })
                .collect();
            parts.sort();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn render_sections(sections: &[SectionSummary]) -> String {
    let mut text = String::new();
    for section in sections {
        let _ = writeln!(
            text,
            "## section {} (segments {}..{})",
            section.chunk_index, section.segments.start, section.segments.end
        );
        text.push_str(&render_bullets(&section.response.bullets));
    }
    text
}

fn render_bullets(bullets: &[SummaryBullet]) -> String {
    let mut text = String::new();
    for bullet in bullets {
        let _ = writeln!(text, "- {}", bullet.text);
        for anchor in &bullet.anchors {
            let _ = writeln!(
                text,
                "  anchor: revision {} segment {} chars {}..{} \"{}\"",
                anchor.revision_id,
                anchor.seq_index,
                anchor.char_start,
                anchor.char_end,
                anchor.excerpt
            );
        }
    }
    text
}

/// Rebuild the output body from accepted items only.
///
/// A rejected item never reaches the body: the UI shows it in the rejection
/// list instead, so a shortened list is explained rather than silent.
fn assemble_body(template: &OutputTemplate, accepted: &[AcceptedItem]) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    for field in &template.fields {
        let items: Vec<serde_json::Value> = accepted
            .iter()
            .filter(|item| item.field == field.key)
            .filter_map(|item| serde_json::to_value(&item.item).ok())
            .collect();
        object.insert(field.key.clone(), serde_json::Value::Array(items));
    }
    serde_json::Value::Object(object)
}

#[cfg(test)]
mod tests;
