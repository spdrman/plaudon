//! The model picker (§5.2, §8.6, §8.6A, §10.2).
//!
//! Every model the backend returned is listed — an unavailable one shows the
//! reason instead of disappearing — with the request family it was shown to
//! implement, its verification state and the machine reason behind it, its
//! pricing unit, its resolved privacy route and its retention posture. A model
//! id the user typed is listed as not verified and cannot be chosen, because
//! nothing about it has been tested (§10.2).
//!
//! The ADR-0022 capability check is the only way a model becomes verified, and
//! it is a real billable request, so it is gated behind the §4.4 consent sheet
//! and an explicit confirmation of the cost.

use egui::Ui;
use plaudon_app::cloud_presentation::{
    consent_covers_capability_check, cost_presentation, custom_model_card, degraded_reason_label,
    estimate_dispatch_cost, pricing_label, privacy_route_label, request_family_label,
    retention_label, tri_state_label, unselectable_reason, verification_presentation,
    ConsentPurpose, ContentType, ProcessingStage, CAPABILITY_CHECK_CLIP_MS,
    CAPABILITY_CHECK_DISCLOSURE,
};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{ListModelsResponse, ModelCard, VerifyModelRequest};
use plaudon_app::tone::Tone;

use super::{fact, AiOutput, Badge, ConsentSheet, RecordingContext};

/// Choosing a model, and checking one.
#[derive(Debug, Default)]
pub struct ModelPicker {
    selected: Option<ModelCard>,
    custom_id: String,
    custom_cards: Vec<ModelCard>,
    check_target: Option<ModelCard>,
    confirming_check: bool,
    consent: ConsentSheet,
}

impl ModelPicker {
    /// The model the user chose, if any.
    #[must_use]
    pub fn selected(&self) -> Option<&ModelCard> {
        self.selected.as_ref()
    }

    /// Forget the chosen model, for instance because the stage changed.
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Draw the picker.
    ///
    /// `models` is the last `list_models` response, or `None` while it is
    /// still in flight.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        legend: &str,
        stage: ProcessingStage,
        models: Option<&ListModelsResponse>,
        context: RecordingContext<'_>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new(legend).heading());

        match models {
            None => {
                ui.label("Loading models…");
            }
            Some(response) => {
                if let Some(reason) = response.degraded_reason.as_deref() {
                    Badge::new("⚑", "Pinned allowlist", Tone::Warning)
                        .description(&format!(
                            "Showing the pinned allowlist because {}. This list is not the live \
                             provider catalogue.",
                            degraded_reason_label(reason)
                        ))
                        .ui(ui);
                } else {
                    ui.label(format!(
                        "Live provider catalogue, retrieved {}.",
                        response
                            .catalog_fetched_at
                            .as_deref()
                            .unwrap_or("at an unrecorded time")
                    ));
                }

                if response.models.is_empty() && self.custom_cards.is_empty() {
                    ui.label("No model is offered for this stage yet.");
                }

                let default_model_id = response.default_model_id.clone();
                let cards: Vec<ModelCard> = response
                    .models
                    .iter()
                    .cloned()
                    .chain(self.custom_cards.iter().cloned())
                    .collect();
                for model in &cards {
                    self.model_row(
                        ui,
                        model,
                        default_model_id.as_deref() == Some(model.model_id.as_str()),
                        context,
                        &mut output,
                    );
                }
            }
        }

        self.custom_model_form(ui, models);
        self.capability_check(ui, stage, context, &mut output);
        output
    }

    fn model_row(
        &mut self,
        ui: &mut Ui,
        model: &ModelCard,
        is_default: bool,
        context: RecordingContext<'_>,
        output: &mut AiOutput,
    ) {
        let verification = verification_presentation(model);
        let blocked = unselectable_reason(model);
        let local_only_block = context.blocked_by_local_only(&model.engine);

        ui.push_id(model.model_id.clone(), |ui| {
            ui.group(|ui| {
                let selected = self.selected.as_ref().is_some_and(|chosen| {
                    chosen.model_id == model.model_id && chosen.engine == model.engine
                });
                let choosable = blocked.is_none() && !local_only_block;
                if ui
                    .add_enabled(
                        choosable,
                        egui::RadioButton::new(selected, model.display_name.clone()),
                    )
                    .clicked()
                {
                    self.selected = Some(model.clone());
                }

                Badge::new(verification.glyph, verification.label, verification.tone)
                    .description(verification.reason)
                    .ui(ui);
                if is_default {
                    Badge::new("★", "Task default", Tone::Neutral).ui(ui);
                }

                fact(ui, "Model id", &model.model_id);
                fact(ui, "Engine", &model.engine);
                fact(
                    ui,
                    "Request family",
                    request_family_label(&model.request_family),
                );
                fact(ui, "Price", &pricing_label(&model.pricing));
                fact(ui, "Provider route", &privacy_route_label(&model.privacy));
                fact(ui, "Retention", &retention_label(&model.privacy));
                fact(
                    ui,
                    "Segment timings",
                    tri_state_label(model.features.segments),
                );
                fact(
                    ui,
                    "Word timings",
                    tri_state_label(model.features.word_timestamps),
                );
                fact(
                    ui,
                    "Speaker turns",
                    tri_state_label(model.features.diarization),
                );
                fact(
                    ui,
                    "Language detection",
                    tri_state_label(model.features.language_detection),
                );

                if let Some(reason) = blocked.as_deref() {
                    Badge::new("!", "Not selectable", Tone::Warning)
                        .description(reason)
                        .ui(ui);
                }
                if local_only_block {
                    Badge::new("⛔", "Blocked by Local-only Mode", Tone::Danger)
                        .description(
                            "This model would send content off the device, so it cannot be \
                             chosen while Local-only Mode is on.",
                        )
                        .ui(ui);
                }

                if model.verification == "not_verified"
                    && ui
                        .button(format!("Run capability check on {}", model.model_id))
                        .clicked()
                {
                    self.check_target = Some(model.clone());
                    self.confirming_check = false;
                    if consent_covers_capability_check(context.consents(), &model.model_id) {
                        self.confirming_check = true;
                    } else {
                        self.consent.open();
                    }
                }
                let _ = output;
            });
        });
    }

    fn custom_model_form(&mut self, ui: &mut Ui, models: Option<&ListModelsResponse>) {
        let custom_label = ui.label("Advanced: custom model ID");
        ui.label(
            "A model id you type has never been tested here, so it is always listed as Not \
             verified and cannot be dispatched to.",
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.custom_id)
                    .hint_text("vendor/model")
                    .desired_width(220.0),
            )
            .labelled_by(custom_label.id);
            if ui.button("Add custom model ID").clicked() {
                let trimmed = self.custom_id.trim().to_owned();
                let known = models.is_some_and(|response| {
                    response
                        .models
                        .iter()
                        .any(|model| model.model_id == trimmed)
                }) || self
                    .custom_cards
                    .iter()
                    .any(|model| model.model_id == trimmed);
                if !trimmed.is_empty() && !known {
                    self.custom_cards
                        .push(custom_model_card(&trimmed, "openrouter"));
                }
                self.custom_id.clear();
            }
        });
    }

    fn capability_check(
        &mut self,
        ui: &mut Ui,
        stage: ProcessingStage,
        context: RecordingContext<'_>,
        output: &mut AiOutput,
    ) {
        let Some(target) = self.check_target.clone() else {
            return;
        };
        let estimate = estimate_dispatch_cost(&target, Some(CAPABILITY_CHECK_CLIP_MS));

        if self
            .consent
            .ui(
                ui,
                &target,
                ConsentPurpose::Transcription,
                ContentType::AudioBytes,
                None,
                &estimate,
                output,
            )
            .is_some_and(|decision| decision.authorized)
        {
            self.confirming_check = true;
        }

        if !self.confirming_check {
            return;
        }
        let mut close = false;
        egui::Modal::new(egui::Id::new("capability-check")).show(ui.ctx(), |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Run the capability check on {}?",
                    target.display_name
                ))
                .heading(),
            );
            ui.label(CAPABILITY_CHECK_DISCLOSURE);
            let cost = cost_presentation(&estimate);
            fact(ui, "Estimated charge for the check", &cost.label);
            ui.label(cost.detail);
            if context.blocked_by_local_only(&target.engine) {
                Badge::new("⛔", "Blocked by Local-only Mode", Tone::Danger)
                    .description(
                        "The check is a real cloud request, so it cannot run while Local-only \
                         Mode is on.",
                    )
                    .ui(ui);
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui
                    .add_enabled(
                        !context.blocked_by_local_only(&target.engine),
                        egui::Button::new("Send the test clip and accept the cost"),
                    )
                    .clicked()
                {
                    output.command(AppCommand::VerifyModel(VerifyModelRequest {
                        model_id: target.model_id.clone(),
                        task: stage.verification_task().to_owned(),
                        cost_acknowledged: true,
                    }));
                    close = true;
                }
            });
        });
        if close {
            self.confirming_check = false;
            self.check_target = None;
        }
    }
}
