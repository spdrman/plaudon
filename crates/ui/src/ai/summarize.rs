//! The summarization panel (§5.2, §9.1, §9.2).
//!
//! Choose which summary stage to run, which model runs it, and start it. The
//! stage list is the text half of the pipeline — section summaries, the
//! consolidated summary, structured extraction and citation verification —
//! and the consent, estimate and Local-only rules are the shared ones in
//! [`super::stage`].
//!
//! **Templates.** A stored output names the template and template version it
//! was produced with, and those facts are shown. The panel still offers no
//! choice, and that is a property of the command it dispatches:
//! `start_processing` carries a recording, a stage, a model override, a cost
//! acknowledgement and a re-run flag, and no field that selects a template.
//! Templates themselves — the built-ins, a user's own copies, the editor, the
//! validation problems and the version history — are managed on the Templates
//! destination ([`crate::screens::templates`]), over the six commands
//! `list_templates`, `get_template`, `create_template_copy`, `save_template`,
//! `validate_template` and `list_template_versions`.

use egui::Ui;
use plaudon_app::cloud_presentation::ProcessingStage;
use plaudon_app::dto::{JobSummary, ListModelsResponse, ListOutputsResponse, OutputDto};

use super::stage::StageDispatch;
use super::{fact, AiOutput, JobCard, ModelPicker, RecordingContext};

/// The summary stages a user may run from this panel.
const SUMMARY_STAGES: &[ProcessingStage] = &[
    ProcessingStage::Normalize,
    ProcessingStage::SectionSummary,
    ProcessingStage::ConsolidatedSummary,
    ProcessingStage::StructuredExtraction,
    ProcessingStage::CitationVerification,
];

/// What this panel can and cannot say about templates.
///
/// Rendered next to the provenance of the latest output, so the absence of a
/// picker here reads as a statement about `start_processing` rather than as a
/// missing feature.
pub const TEMPLATE_PROVENANCE_NOTE: &str =
    "Each output records the template and template version it was produced with, which is what is \
     shown here. Starting a stage does not select a template: `start_processing` has no field for \
     one. Templates themselves — the built-ins, your own copies and their version history — are \
     managed on the Templates screen.";

/// Choosing a summary stage and a model, and watching the job.
#[derive(Debug)]
pub struct SummarizationPanel {
    stage: ProcessingStage,
    models: ModelPicker,
    dispatch: StageDispatch,
    job: JobCard,
}

impl Default for SummarizationPanel {
    fn default() -> Self {
        Self {
            stage: ProcessingStage::ConsolidatedSummary,
            models: ModelPicker::default(),
            dispatch: StageDispatch::default(),
            job: JobCard::default(),
        }
    }
}

impl SummarizationPanel {
    /// The stage the user selected.
    #[must_use]
    pub fn stage(&self) -> ProcessingStage {
        self.stage
    }

    /// Draw the panel.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        models: Option<&ListModelsResponse>,
        outputs: Option<&ListOutputsResponse>,
        job: Option<&JobSummary>,
        now_ms: i128,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Summarization").heading());
        ui.label("Only the stage you choose runs again. A model chosen here applies to this recording alone.");

        ui.label(egui::RichText::new("Stage").strong());
        for stage in SUMMARY_STAGES {
            if ui
                .radio_value(&mut self.stage, *stage, stage.label())
                .clicked()
            {
                self.models.clear_selection();
                self.dispatch.reset();
            }
        }

        ui.separator();
        ui.label(egui::RichText::new("Template").strong());
        match outputs.and_then(latest_output) {
            Some(latest) => {
                fact(ui, "Template", &latest.template_id);
                fact(ui, "Template version", &latest.template_version);
                fact(ui, "Pipeline version", &latest.pipeline_version);
            }
            None => {
                ui.label("No output has been generated for this recording yet.");
            }
        }
        ui.label(TEMPLATE_PROVENANCE_NOTE);

        ui.separator();
        output.extend(self.models.ui(
            ui,
            &format!("Model for {} on this recording", self.stage.label()),
            self.stage,
            models,
            context,
        ));

        if let Some(model) = self.models.selected().cloned() {
            self.dispatch.ui(
                ui,
                context,
                self.stage,
                &model,
                &format!("Run {}", self.stage.label().to_lowercase()),
                &mut output,
            );
        } else {
            ui.label("Estimated charge before dispatch: choose a model to see an estimate.");
        }

        if let Some(job) = job {
            ui.separator();
            ui.label(egui::RichText::new("This recording's summary job").heading());
            output.extend(self.job.ui(ui, job, now_ms));
        }
        output
    }
}

fn latest_output(response: &ListOutputsResponse) -> Option<&OutputDto> {
    response.outputs.last()
}
