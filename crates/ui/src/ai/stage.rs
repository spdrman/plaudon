//! Dispatching one pipeline stage for one recording.
//!
//! Shared by the transcription and summarization panels, because the rules are
//! the stage's, not the panel's: show the estimate before dispatch (§4.4),
//! refuse a cloud engine while Local-only Mode is on (§4.3), open the §4.4
//! consent sheet before the first cloud run and dispatch only after a scope
//! that actually authorises it, require an explicit acknowledgement when the
//! charge cannot be estimated (§10.4), and store a per-recording model
//! override without touching the task default (§10.2).

use egui::Ui;
use plaudon_app::cloud_presentation::{
    consent_covers, cost_presentation, estimate_dispatch_cost, ProcessingStage, LOCAL_ONLY_REFUSAL,
};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    CostConfidence, ModelCard, ModelOverride, ModelPreferenceScope, SetModelPreferenceRequest,
    StartProcessingRequest,
};
use plaudon_app::tone::Tone;

use super::{fact, AiOutput, Badge, ConsentSheet, RecordingContext};

/// The estimate, the consent gate and the two dispatch controls of one stage.
#[derive(Debug, Default)]
pub struct StageDispatch {
    consent: ConsentSheet,
    cost_acknowledged: bool,
    override_saved: Option<String>,
    start_after_consent: bool,
}

impl StageDispatch {
    /// Forget the per-recording override note, for instance when the stage or
    /// the model changed.
    pub fn reset(&mut self) {
        self.override_saved = None;
        self.cost_acknowledged = false;
        self.start_after_consent = false;
    }

    /// Draw the estimate, the gates and the controls.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        stage: ProcessingStage,
        model: &ModelCard,
        start_label: &str,
        output: &mut AiOutput,
    ) {
        let estimate = estimate_dispatch_cost(model, context.duration_ms);
        let cost = cost_presentation(&estimate);
        fact(ui, "Estimated charge before dispatch", &cost.label);
        ui.label(cost.detail);

        let blocked = context.blocked_by_local_only(&model.engine);
        let is_cloud = !context.engine_is_local(&model.engine);
        let needs_consent = is_cloud
            && !consent_covers(
                context.consents(),
                &model.model_id,
                Some(context.recording_id),
            );
        if needs_consent && !blocked {
            Badge::new("!", "Consent is required before this runs", Tone::Warning)
                .description(
                    "Nothing has been sent. The disclosure opens before anything leaves this \
                     device.",
                )
                .ui(ui);
        }

        let unknown_cost = estimate.confidence == CostConfidence::Unknown;
        if unknown_cost && is_cloud {
            ui.checkbox(
                &mut self.cost_acknowledged,
                "I accept that the charge for this run cannot be estimated in advance",
            );
        }
        let acknowledged = !unknown_cost || !is_cloud || self.cost_acknowledged;

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !blocked,
                    egui::Button::new("Use this model for this recording only"),
                )
                .clicked()
            {
                output.command(AppCommand::SetModelPreference(SetModelPreferenceRequest {
                    stage: stage.as_str().to_owned(),
                    scope: ModelPreferenceScope {
                        kind: "recording".to_owned(),
                        recording_id: Some(context.recording_id.to_owned()),
                    },
                    engine: model.engine.clone(),
                    model_id: Some(model.model_id.clone()),
                }));
                self.override_saved = Some(model.model_id.clone());
            }

            if ui
                .add_enabled(!blocked && acknowledged, egui::Button::new(start_label))
                .clicked()
            {
                if needs_consent {
                    self.consent.open();
                    self.start_after_consent = true;
                } else {
                    output.command(start_command(
                        context,
                        stage,
                        model,
                        unknown_cost && self.cost_acknowledged,
                    ));
                }
            }
        });

        if let Some(stored) = self.override_saved.as_deref() {
            ui.label(format!(
                "This recording will use {stored} for {}. The task default is unchanged.",
                stage.label()
            ));
        }
        if blocked {
            Badge::new("⛔", "Refused", Tone::Danger)
                .description(LOCAL_ONLY_REFUSAL)
                .ui(ui);
        }

        // A stage with no consent purpose can never leave the device (the
        // speaker stages of VP-110), so there is no grant to ask for and the
        // dialog is not offered.
        let (Some(purpose), Some(content)) = (stage.consent_purpose(), stage.content_type()) else {
            return;
        };
        if let Some(decision) = self.consent.ui(
            ui,
            model,
            purpose,
            content,
            Some(context.recording_id),
            &estimate,
            output,
        ) {
            if decision.authorized && self.start_after_consent {
                output.command(start_command(
                    context,
                    stage,
                    model,
                    unknown_cost && self.cost_acknowledged,
                ));
            } else {
                // `Ask every time` stores a preference and authorises nothing,
                // so the run the user asked for does not happen and the panel
                // says so rather than silently dropping it.
                ui.label(
                    "Preference saved. \"Ask every time\" does not authorise this run, so \
                     nothing was sent.",
                );
            }
            self.start_after_consent = false;
        }
    }
}

fn start_command(
    context: RecordingContext<'_>,
    stage: ProcessingStage,
    model: &ModelCard,
    cost_acknowledged: bool,
) -> AppCommand {
    AppCommand::StartProcessing(StartProcessingRequest {
        recording_id: context.recording_id.to_owned(),
        stage: stage.as_str().to_owned(),
        model_override: Some(ModelOverride {
            engine: model.engine.clone(),
            model_id: model.model_id.clone(),
        }),
        cost_acknowledged,
        rerun: true,
    })
}
