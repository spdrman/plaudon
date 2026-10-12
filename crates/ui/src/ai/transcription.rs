//! The transcription panel (§5.2 processing sheet, §4.3, §4.4, §10.2).
//!
//! One place to choose how a recording is transcribed and to watch the job
//! that does it:
//!
//! * the **engine** comes from the capability inventory, with its validation
//!   status, the reason it is unavailable when it is, the models installed on
//!   this machine and whether it needs network egress. Apple's native engine,
//!   `whisper.cpp` and OpenRouter are rows of that inventory, not special
//!   cases in this file;
//! * a **cloud engine is refused outright while Local-only Mode is on**. The
//!   panel does not queue the work and does not let a cloud model be chosen,
//!   and it says why (§4.3);
//! * the estimated charge is shown **before** dispatch, the §4.4 consent sheet
//!   opens before the first cloud run, and a model chosen here can be stored
//!   as an override for this recording alone (§10.2);
//! * the running job is rendered by [`super::JobCard`], which carries the
//!   cancel disclosure, the retry gate and the outcome-unknown state.

use egui::Ui;
use plaudon_app::cloud_presentation::{ProcessingStage, LOCAL_ONLY_REFUSAL};
use plaudon_app::dto::{EngineStage, JobSummary, ListModelsResponse};
use plaudon_app::tone::Tone;

use super::stage::StageDispatch;
use super::{fact, AiOutput, Badge, JobCard, ModelPicker, RecordingContext};

/// Choosing an engine and a model, starting the job, and watching it.
#[derive(Debug, Default)]
pub struct TranscriptionPanel {
    engine_id: Option<String>,
    models: ModelPicker,
    job: JobCard,
    dispatch: StageDispatch,
}

impl TranscriptionPanel {
    /// The engine the user chose, if any.
    #[must_use]
    pub fn engine_id(&self) -> Option<&str> {
        self.engine_id.as_deref()
    }

    /// Draw the panel.
    ///
    /// `models` is the last `list_models` response for the chosen engine, and
    /// `job` the transcription job for this recording when one exists.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        models: Option<&ListModelsResponse>,
        job: Option<&JobSummary>,
        now_ms: i128,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Transcription").heading());

        self.engines(ui, context);
        let engine_id = self.engine_id.clone();

        if let Some(engine_id) = engine_id.as_deref() {
            if context.blocked_by_local_only(engine_id) {
                Badge::new("⛔", "Local-only Mode blocks this engine", Tone::Danger)
                    .description(LOCAL_ONLY_REFUSAL)
                    .ui(ui);
            }
            output.extend(self.models.ui(
                ui,
                &format!("Model for transcription on this recording ({engine_id})"),
                ProcessingStage::Transcribe,
                models,
                context,
            ));
        } else {
            ui.label("Choose an engine to see the models it offers.");
        }

        self.dispatch_controls(ui, context, &mut output);

        if let Some(job) = job {
            ui.separator();
            ui.label(egui::RichText::new("This recording's transcription job").heading());
            output.extend(self.job.ui(ui, job, now_ms));
        }
        output
    }

    fn engines(&mut self, ui: &mut Ui, context: RecordingContext<'_>) {
        let Some(capabilities) = context.capabilities else {
            ui.label("The engine inventory has not been read yet.");
            return;
        };
        ui.label(egui::RichText::new("Engine").strong());
        for engine in capabilities
            .engines
            .iter()
            .filter(|engine| engine.stage == EngineStage::Stt)
        {
            ui.push_id(engine.engine_id.clone(), |ui| {
                ui.group(|ui| {
                    let selected = self.engine_id.as_deref() == Some(engine.engine_id.as_str());
                    let blocked = context.local_only() && engine.requires_network;
                    if ui
                        .add_enabled(
                            !blocked,
                            egui::RadioButton::new(selected, engine.display_name.clone()),
                        )
                        .clicked()
                    {
                        self.engine_id = Some(engine.engine_id.clone());
                        self.models.clear_selection();
                        self.dispatch.reset();
                    }
                    fact(ui, "Engine id", &engine.engine_id);
                    fact(ui, "Status", &format!("{:?}", engine.status));
                    fact(
                        ui,
                        "Runs",
                        if engine.requires_network {
                            "Through a cloud provider"
                        } else {
                            "On this computer"
                        },
                    );
                    if let Some(reason) = engine.unavailable_reason.as_deref() {
                        Badge::new("!", "Unavailable", Tone::Warning)
                            .description(reason)
                            .ui(ui);
                    }
                    if engine.installed_models.is_empty() {
                        ui.label("No model is installed for this engine on this machine.");
                    } else {
                        for model in &engine.installed_models {
                            fact(
                                ui,
                                &model.display_name,
                                if model.installed {
                                    "Installed"
                                } else {
                                    "Not installed"
                                },
                            );
                        }
                    }
                    if let Some(bytes) = engine.download_bytes_required {
                        fact(
                            ui,
                            "Download required",
                            &format!("{bytes} bytes for the smallest usable model"),
                        );
                    }
                    if blocked {
                        Badge::new("⛔", "Blocked by Local-only Mode", Tone::Danger)
                            .description(LOCAL_ONLY_REFUSAL)
                            .ui(ui);
                    }
                });
            });
        }
    }

    fn dispatch_controls(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        output: &mut AiOutput,
    ) {
        let Some(model) = self.models.selected().cloned() else {
            ui.label("Estimated charge before dispatch: choose a model to see an estimate.");
            return;
        };
        self.dispatch.ui(
            ui,
            context,
            ProcessingStage::Transcribe,
            &model,
            "Transcribe this recording",
            output,
        );
    }
}
