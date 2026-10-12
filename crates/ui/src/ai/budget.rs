//! Cloud spending and the local budget (§10.4, A8).
//!
//! Three numbers that are never conflated: what the provider **finalized**,
//! what we **estimated**, and how many attempts produced **no derivable
//! cost** at all. An unknown is counted and shown as unknown; it is never
//! added in as zero, and a monthly total is never presented as authoritative
//! billing.
//!
//! The per-job and monthly caps are local. `CloudBudget::enforcement_note`
//! carries Rust's own wording about what a local cap can and cannot do, and
//! this panel renders that string rather than composing a promise of its own.

use egui::Ui;
use plaudon_app::cloud_presentation::{cost_presentation, format_money};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    CloudBudget, GetUsageResponse, MoneyAmount, SetCloudBudgetRequest, UsageTotals,
};
use plaudon_app::tone::Tone;

use super::{fact, AiOutput, Badge};

/// Spending, and the caps the user may set.
#[derive(Debug, Default)]
pub struct BudgetPanel {
    per_job_limit: String,
    monthly_limit: String,
    loaded_from: Option<String>,
}

impl BudgetPanel {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, usage: Option<&GetUsageResponse>) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Cloud spending").heading());

        let Some(usage) = usage else {
            ui.label("Spending has not been read yet.");
            return output;
        };
        fact(ui, "Period", &format!("{} to {}", usage.from, usage.to));
        totals(ui, &usage.totals);

        if !usage.by_model.is_empty() {
            ui.label(egui::RichText::new("By model").strong());
            for model in &usage.by_model {
                ui.group(|ui| {
                    fact(ui, "Model", &model.model_id);
                    fact(ui, "Attempts", &model.attempts.to_string());
                    fact(
                        ui,
                        "Provider-reported",
                        &optional_money(model.finalized.as_ref()),
                    );
                    fact(ui, "Estimated", &optional_money(model.estimated.as_ref()));
                    fact(
                        ui,
                        "Attempts with no derivable cost",
                        &model.unknown_count.to_string(),
                    );
                });
            }
        }

        if !usage.attempts.is_empty() {
            ui.label(egui::RichText::new("Attempts").strong());
            for attempt in &usage.attempts {
                ui.group(|ui| {
                    fact(ui, "Model", &attempt.model_id);
                    fact(ui, "Dispatch state", &attempt.dispatch_state);
                    let cost = cost_presentation(&attempt.cost);
                    Badge::new(cost.glyph, &cost.label, cost.tone).ui(ui);
                    fact(ui, "Requested", &attempt.created_at);
                    if attempt.possibly_billed {
                        Badge::new("!", "May have been billed", Tone::Warning)
                            .description(
                                "The provider may have accepted this request even though no \
                                 response reached us.",
                            )
                            .ui(ui);
                    }
                });
            }
        }

        output.extend(self.limits(ui, &usage.budget));
        output
    }

    fn limits(&mut self, ui: &mut Ui, budget: &CloudBudget) -> AiOutput {
        let mut output = AiOutput::default();
        ui.separator();
        ui.label(egui::RichText::new("Budget").strong());

        let fingerprint = format!(
            "{:?}|{:?}",
            budget.per_job_limit.as_ref().map(|m| &m.amount),
            budget.monthly_limit.as_ref().map(|m| &m.amount)
        );
        if self.loaded_from.as_deref() != Some(fingerprint.as_str()) {
            self.per_job_limit = budget
                .per_job_limit
                .as_ref()
                .map(|amount| amount.amount.clone())
                .unwrap_or_default();
            self.monthly_limit = budget
                .monthly_limit
                .as_ref()
                .map(|amount| amount.amount.clone())
                .unwrap_or_default();
            self.loaded_from = Some(fingerprint);
        }

        let month_to_date = cost_presentation(&budget.month_to_date);
        Badge::new(
            month_to_date.glyph,
            &month_to_date.label,
            month_to_date.tone,
        )
        .description(month_to_date.detail)
        .ui(ui);
        fact(ui, "Month to date", &month_to_date.label);
        fact(
            ui,
            "Enforced here",
            if budget.blocks_dispatch {
                "Yes: a run that would exceed a limit is refused before dispatch."
            } else {
                "No: no limit is calculable, so nothing is blocked here."
            },
        );
        ui.label(&budget.enforcement_note);

        ui.horizontal(|ui| {
            let label = ui.label("Per-job limit (USD)");
            ui.add(
                egui::TextEdit::singleline(&mut self.per_job_limit)
                    .hint_text("empty clears the limit")
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
        });
        ui.horizontal(|ui| {
            let label = ui.label("Monthly limit (USD)");
            ui.add(
                egui::TextEdit::singleline(&mut self.monthly_limit)
                    .hint_text("empty clears the limit")
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
        });
        if ui.button("Save budget").clicked() {
            output.command(AppCommand::SetCloudBudget(SetCloudBudgetRequest {
                per_job_limit: non_empty(&self.per_job_limit),
                monthly_limit: non_empty(&self.monthly_limit),
                currency: "USD".to_owned(),
            }));
        }
        output
    }
}

fn totals(ui: &mut Ui, totals: &UsageTotals) {
    fact(
        ui,
        "Provider-reported (finalized)",
        &optional_money(totals.finalized.as_ref()),
    );
    fact(ui, "Estimated", &optional_money(totals.estimated.as_ref()));
    fact(
        ui,
        "Attempts with no derivable cost",
        &totals.unknown_count.to_string(),
    );
    if totals.unknown_count > 0 {
        Badge::new("?", "Some spend is unknown", Tone::Neutral)
            .description(
                "These attempts are counted but carry no amount. They are not added in as zero, \
                 so the totals above are not the whole bill.",
            )
            .ui(ui);
    }
}

fn optional_money(amount: Option<&MoneyAmount>) -> String {
    amount.map_or_else(|| "Unknown".to_owned(), format_money)
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}
