//! The provenance of every tool run on one recording (§9.7, §13.2, §13.3).
//!
//! A grounded answer is only as trustworthy as the record of how it was
//! produced, so this list shows, per run and without interpretation: the tool
//! identity and the version that resolves to the exact definition that ran,
//! the engine and the model, the prompt and output-schema versions, the hash
//! of what was dispatched and of what was accepted, the status, the consent
//! posture the run happened under, and the cost.
//!
//! The cost goes through [`plaudon_app::cloud_presentation::cost_presentation`],
//! which is the only reason an unknown cost is never shown as zero (§10.4,
//! §13.2): a run whose provider reported no price says "Cost unknown", and the
//! panel has no arithmetic of its own to disagree with.
//!
//! Nothing on this list is actionable — it is evidence, not a control
//! surface — so the panel emits no intent.

use egui::Ui;
use plaudon_app::cloud_presentation::{cost_presentation, request_family_label};
use plaudon_app::dto::{ConsentPolicyDto, ListToolRunsResponse, ToolRunDto};
use plaudon_app::tone::Tone;

use crate::ai::{fact, Badge};

/// The tool-run provenance list.
#[derive(Debug, Default)]
pub struct ToolRunsPanel;

impl ToolRunsPanel {
    /// Draw the list.
    ///
    /// `runs` is the last `list_tool_runs` response, borrowed, or `None`
    /// while it has not been read.
    pub fn ui(&mut self, ui: &mut Ui, runs: Option<&ListToolRunsResponse>) {
        ui.label(egui::RichText::new("Tool runs").heading());
        ui.label(
            "What ran against this recording, under which consent, and what it cost. This is the \
             record a result can be checked against.",
        );
        let Some(response) = runs else {
            ui.label("Tool runs have not been read yet.");
            return;
        };
        if response.runs.is_empty() {
            ui.label("No tool has been run on this recording yet.");
            return;
        }
        for run in &response.runs {
            ui.push_id(("tool-run", run.run_id.as_str()), |ui| run_block(ui, run));
        }
    }
}

/// One recorded run.
fn run_block(ui: &mut Ui, run: &ToolRunDto) {
    ui.group(|ui| {
        ui.label(
            egui::RichText::new(format!("{} version {}", run.tool_id, run.tool_version)).strong(),
        );
        status_badge(ui, &run.status);
        fact(ui, "Run", &run.run_id);
        fact(ui, "Transcript revision", &run.transcript_revision_id);
        fact(ui, "Engine", &run.engine);
        fact(
            ui,
            "Model",
            &match run.model_version.as_deref() {
                Some(version) => format!("{} ({version})", run.model_id),
                // The engine reported no model version; saying so is the
                // whole point of a provenance record.
                None => format!("{} (no version reported)", run.model_id),
            },
        );
        fact(
            ui,
            "Request contract",
            request_family_label(&run.request_family),
        );
        fact(ui, "Prompt version", &run.prompt_version);
        fact(ui, "Output schema version", &run.schema_version);
        fact(ui, "Input hash (SHA-256)", &run.input_sha256);
        fact(
            ui,
            "Output hash (SHA-256)",
            run.output_sha256
                .as_deref()
                .unwrap_or("No output was accepted for this run."),
        );
        consent_block(ui, &run.consent_policy);

        let cost = cost_presentation(&run.cost);
        Badge::new(cost.glyph, &cost.label, cost.tone)
            .description(cost.detail)
            .ui(ui);

        fact(ui, "Started", &run.created_at);
        fact(
            ui,
            "Finished",
            run.finished_at
                .as_deref()
                .unwrap_or("This run has not finished."),
        );
    });
}

/// The consent posture the run happened under.
fn consent_block(ui: &mut Ui, policy: &ConsentPolicyDto) {
    if policy.egress_allowed {
        Badge::new("↗", "Allowed to leave this device", Tone::Warning).ui(ui);
    } else {
        Badge::new("⌂", "Kept on this device", Tone::Success).ui(ui);
    }
    fact(
        ui,
        "Privacy class",
        privacy_class_label(&policy.privacy_class),
    );
    fact(
        ui,
        "Consent grant",
        policy
            .consent_id
            .as_deref()
            .unwrap_or("No consent grant applied to this run."),
    );
}

/// The routing class of a run, in words (§10.8).
fn privacy_class_label(class: &str) -> &'static str {
    match class {
        "on_device" => "Ran entirely on this machine",
        "cloud_zero_retention" => "Left this machine over a documented zero-retention route",
        "cloud_retaining" => "Left this machine; the provider may retain the content",
        // §10.8 fails closed on an unresolved route, so an unrecognised class
        // is reported as unresolved rather than assumed to be benign.
        _ => "Routing could not be resolved",
    }
}

/// The run's status, as a glyph and a label (§5.3).
fn status_badge(ui: &mut Ui, status: &str) {
    match status {
        "succeeded" => Badge::new("✓", "Succeeded", Tone::Success)
            .description("The output satisfied the tool's schema and was stored.")
            .ui(ui),
        "rejected" => Badge::new("!", "Output rejected", Tone::Warning)
            .description(
                "The model answered, but the output did not satisfy the tool's schema, so \
                 nothing was stored.",
            )
            .ui(ui),
        "failed" => Badge::new("✖", "Failed", Tone::Danger)
            .description("The run did not complete.")
            .ui(ui),
        "canceled" => Badge::new("⊘", "Canceled", Tone::Neutral)
            .description("The run was stopped before it completed.")
            .ui(ui),
        other => Badge::new("?", "Status not recognised", Tone::Neutral)
            .description(other)
            .ui(ui),
    };
}
