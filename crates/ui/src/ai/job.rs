//! One job, rendered: state badge, progress, cost, and the controls the
//! state and the failure allow.
//!
//! Every rule shown here is [`plaudon_app::job_presentation`]'s: which states
//! may be cancelled, which may be retried, which retry needs a cost
//! acknowledgement first (§14.3, §14.7), and which corrective controls a §5.4
//! condition requires. This file decides nothing; it draws.

use egui::{ProgressBar, Ui};
use plaudon_app::cloud_presentation::cost_presentation;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{CancelJobRequest, JobSummary, RetryJobRequest};
use plaudon_app::job_presentation::{
    envelope_action_control, error_guidance, job_elapsed_label, job_kind_label, job_progress,
    job_state_presentation, ControlIntent, JobProgress, CANCEL_DISCLOSURE,
};
use plaudon_app::tone::Tone;

use super::{fact, AiIntent, AiOutput, Badge};

/// Frame-to-frame state of the job controls: which job is waiting for a
/// cancellation confirmation, and whether its retry cost was acknowledged.
#[derive(Debug, Default)]
pub struct JobCard {
    confirming_cancel: Option<String>,
    cost_acknowledged: bool,
}

impl JobCard {
    /// Draw one job row.
    ///
    /// `now_ms` is the wall clock the elapsed label is measured against; the
    /// caller passes it so a test is not at the mercy of the host's clock.
    pub fn ui(&mut self, ui: &mut Ui, job: &JobSummary, now_ms: i128) -> AiOutput {
        let mut output = AiOutput::default();
        let job_id = job.job_id.to_string();
        let presentation = job_state_presentation(job.state);

        ui.push_id(job_id.clone(), |ui| {
            ui.group(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} — {}", job_kind_label(job.kind), job.label))
                        .heading(),
                );
                Badge::new(presentation.glyph, presentation.label, presentation.tone)
                    .description(presentation.description)
                    .ui(ui);

                if let Some(stage) = job.stage.as_deref() {
                    fact(ui, "Stage", stage);
                }
                Self::progress(ui, job, presentation.label);
                fact(
                    ui,
                    "Elapsed",
                    job_elapsed_label(
                        job.started_at.as_deref(),
                        job.finished_at.as_deref(),
                        now_ms,
                    )
                    .trim_start_matches("Elapsed ")
                    .trim_start_matches("Elapsed"),
                );

                let cost = cost_presentation(&job.cost);
                Badge::new(cost.glyph, &cost.label, cost.tone)
                    .description(cost.detail)
                    .ui(ui);

                if let Some(request_id) = job.provider_request_id.as_deref() {
                    fact(ui, "Provider request id", request_id);
                }

                Self::failure(ui, job, &mut output);
                self.controls(ui, job, &job_id, &mut output);
            });
        });

        output
    }

    fn progress(ui: &mut Ui, job: &JobSummary, state_label: &str) {
        match job_progress(job.state, job.progress) {
            JobProgress::Fraction(fraction) => {
                let percent = (fraction * 100.0).round() as u32;
                ui.add(
                    ProgressBar::new(fraction).text(format!("{state_label}: {percent}% complete")),
                );
            }
            JobProgress::Indeterminate => {
                // §J3: the stage reported no ratio, so the bar animates and the
                // text says so. It never shows a percentage it does not have.
                ui.add(
                    ProgressBar::new(0.0)
                        .animate(true)
                        .text(format!("{state_label}: progress not reported")),
                );
            }
            JobProgress::Finished => {}
        }
    }

    fn failure(ui: &mut Ui, job: &JobSummary, output: &mut AiOutput) {
        let Some(envelope) = job.error.as_ref() else {
            return;
        };
        ui.label(egui::RichText::new(&envelope.message).color(ui.visuals().error_fg_color));

        if let Some(guidance) = error_guidance(envelope.code) {
            ui.label(egui::RichText::new(guidance.title).strong());
            if guidance.show_capacity {
                ui.label(
                    "Compare the space this job needs with the space the library volume has left \
                     in storage settings.",
                );
            }
            for control in guidance.controls {
                if control.intent == ControlIntent::Retry && guidance.suppress_retry {
                    continue;
                }
                if ui.button(control.label).clicked() {
                    match control.intent {
                        ControlIntent::Retry => {
                            output.command(AppCommand::RetryJob(RetryJobRequest {
                                job_id: job.job_id.to_string(),
                                cost_acknowledged: false,
                            }));
                        }
                        // "Keep queued" is the absence of an action: the job is
                        // left exactly as it is, which is why it dispatches
                        // nothing at all.
                        ControlIntent::KeepQueued => {}
                        intent => output.push(AiIntent::Navigate(intent)),
                    }
                }
            }
        } else if let Some(control) = envelope_action_control(envelope.action, envelope.retryable) {
            if ui.button(control.label).clicked() {
                match control.intent {
                    ControlIntent::Retry => output.command(AppCommand::RetryJob(RetryJobRequest {
                        job_id: job.job_id.to_string(),
                        cost_acknowledged: false,
                    })),
                    intent => output.push(AiIntent::Navigate(intent)),
                }
            }
        }
    }

    fn controls(&mut self, ui: &mut Ui, job: &JobSummary, job_id: &str, output: &mut AiOutput) {
        let presentation = job_state_presentation(job.state);
        let suppress_retry = job
            .error
            .as_ref()
            .and_then(|envelope| error_guidance(envelope.code))
            .is_some_and(|guidance| guidance.suppress_retry);

        ui.horizontal_wrapped(|ui| {
            if presentation.can_cancel && ui.button("Cancel job").clicked() {
                self.confirming_cancel = Some(job_id.to_owned());
            }

            if presentation.can_retry && !suppress_retry {
                let needs_ack = presentation.requires_cost_ack || job.cost_acknowledgement_required;
                if needs_ack {
                    ui.checkbox(
                        &mut self.cost_acknowledged,
                        "I understand this request may already have been billed and that retrying \
                         may be billed again",
                    );
                }
                let enabled = !needs_ack || self.cost_acknowledged;
                if ui
                    .add_enabled(enabled, egui::Button::new("Retry job"))
                    .clicked()
                {
                    output.command(AppCommand::RetryJob(RetryJobRequest {
                        job_id: job_id.to_owned(),
                        cost_acknowledged: needs_ack,
                    }));
                    self.cost_acknowledged = false;
                }
            }
        });

        if presentation.requires_cost_ack {
            Badge::new(
                "??",
                "Outcome unknown",
                Tone::Warning,
            )
            .description(
                "This request is never retried automatically, because the provider may already \
                 have accepted and billed it.",
            )
            .ui(ui);
        }

        if self.confirming_cancel.as_deref() == Some(job_id) {
            let mut close = false;
            egui::Modal::new(egui::Id::new(("cancel-job", job_id))).show(ui.ctx(), |ui| {
                ui.label(egui::RichText::new("Cancel this job?").heading());
                ui.label(CANCEL_DISCLOSURE);
                ui.horizontal(|ui| {
                    if ui.button("Keep running").clicked() {
                        close = true;
                    }
                    if ui.button("Cancel the job").clicked() {
                        output.command(AppCommand::CancelJob(CancelJobRequest {
                            job_id: job_id.to_owned(),
                        }));
                        close = true;
                    }
                });
            });
            if close {
                self.confirming_cancel = None;
            }
        }
    }
}
