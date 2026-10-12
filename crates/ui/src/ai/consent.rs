//! The cloud consent sheet (§4.4).
//!
//! Before the first cloud operation the sheet discloses, in one place: the
//! model and the provider route it resolves to, what exactly leaves the
//! device, the retention posture, the estimated charge when it is measurable,
//! an editable per-run spending cap, and the §10.5 caveat that zero retention
//! is not retroactive. The user then picks one of the three §4.4 scopes.
//!
//! Two refusals are built in and neither can be clicked past:
//!
//! * **Fail-closed zero retention.** When the user requires documented zero
//!   retention and the resolved route does not document it, the sheet refuses
//!   the grant instead of sending the content (§4.4, §10.8).
//! * **Ask every time authorises nothing.** It stores a preference and says so;
//!   the caller must not treat it as permission to dispatch.
//!
//! The versioned disclosure id is stored with the decision, so a later audit
//! can say which words the user agreed to.

use egui::Ui;
use plaudon_app::cloud_presentation::{
    consent_purpose_label, content_type_label, cost_presentation, privacy_route_label,
    retention_label, ConsentScopeKind, CONSENT_DISCLOSURE_VERSION, ZDR_CAVEAT,
};
use plaudon_app::cloud_presentation::{ConsentPurpose, ContentType};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    ConsentScopeDto, CostValue, GrantCloudConsentRequest, ModelCard, SetCloudBudgetRequest,
};
use plaudon_app::tone::Tone;

use super::{fact, AiOutput, Badge};

/// What the user decided, once the grant command has been emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentDecision {
    /// Scope the grant was stored with.
    pub scope: ConsentScopeKind,
    /// Whether that scope authorises the pending dispatch. `false` for
    /// `ask_every_time`: the caller must not retry the operation.
    pub authorized: bool,
}

/// The §4.4 disclosure sheet.
#[derive(Debug)]
pub struct ConsentSheet {
    open: bool,
    scope: ConsentScopeKind,
    cap: String,
    require_zero_retention: bool,
}

impl Default for ConsentSheet {
    fn default() -> Self {
        Self {
            open: false,
            // The scope that authorises nothing is the default: a sheet that
            // opens pre-armed would be consent by inertia.
            scope: ConsentScopeKind::AskEveryTime,
            cap: String::new(),
            require_zero_retention: false,
        }
    }
}

impl ConsentSheet {
    /// Open the sheet, resetting the previous decision.
    pub fn open(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
    }

    /// Close without granting anything.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Whether the sheet is on screen.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Draw the sheet.
    ///
    /// `recording_id` is required to offer the per-recording scope; `None`
    /// hides that choice, which is what a capability check does.
    /// Returns the decision on the frame the grant is dispatched.
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        model: &ModelCard,
        purpose: ConsentPurpose,
        content_type: ContentType,
        recording_id: Option<&str>,
        estimated_cost: &CostValue,
        output: &mut AiOutput,
    ) -> Option<ConsentDecision> {
        if !self.open {
            return None;
        }
        let mut decision = None;
        let mut close = false;

        egui::Modal::new(egui::Id::new("cloud-consent-sheet")).show(ui.ctx(), |ui| {
            ui.label(egui::RichText::new("Send this content to a cloud provider?").heading());
            ui.label(format!(
                "{} would be performed by a model reached through {}. {}",
                consent_purpose_label(purpose),
                model.privacy.relay,
                content_type_label(content_type),
            ));

            fact(
                ui,
                "Model",
                &format!("{} ({})", model.display_name, model.model_id),
            );
            fact(ui, "Provider route", &privacy_route_label(&model.privacy));
            fact(ui, "Data transferred", content_type_label(content_type));
            fact(ui, "Retention posture", &retention_label(&model.privacy));

            let cost = cost_presentation(estimated_cost);
            fact(ui, "Estimated charge", &cost.label);
            ui.label(cost.detail);

            ui.separator();
            let cap_label = ui.label("Spending cap for a single run (USD)");
            ui.add(
                egui::TextEdit::singleline(&mut self.cap)
                    .hint_text("Leave empty to keep the current limit"),
            )
            .labelled_by(cap_label.id);
            ui.label("Saved as your per-job budget.");
            ui.checkbox(
                &mut self.require_zero_retention,
                "Require documented zero retention. If it cannot be confirmed for this exact \
                 route, the request is refused rather than sent.",
            );
            ui.label(ZDR_CAVEAT);

            let fail_closed =
                self.require_zero_retention && !model.privacy.zero_retention_available;
            if fail_closed {
                Badge::new("!", "Zero retention cannot be confirmed", Tone::Danger)
                    .description(
                        "This route does not document zero retention, so the request is refused. \
                         Nothing has been sent.",
                    )
                    .ui(ui);
            }

            ui.separator();
            ui.label(egui::RichText::new("How long should this apply?").strong());
            for scope in ConsentScopeKind::ALL {
                if *scope == ConsentScopeKind::Recording && recording_id.is_none() {
                    continue;
                }
                ui.radio_value(&mut self.scope, *scope, scope.label());
                ui.label(scope.description());
            }

            ui.label(format!("Disclosure version {CONSENT_DISCLOSURE_VERSION}"));

            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                let approve_label = if self.scope.authorizes() {
                    "Approve and continue"
                } else {
                    "Save preference without sending"
                };
                if ui
                    .add_enabled(!fail_closed, egui::Button::new(approve_label))
                    .clicked()
                {
                    let cap = self.cap.trim();
                    if !cap.is_empty() {
                        output.command(AppCommand::SetCloudBudget(SetCloudBudgetRequest {
                            per_job_limit: Some(cap.to_owned()),
                            monthly_limit: None,
                            currency: "USD".to_owned(),
                        }));
                    }
                    output.command(AppCommand::GrantCloudConsent(GrantCloudConsentRequest {
                        purpose: purpose.label().to_owned(),
                        content_type: content_type.label().to_owned(),
                        scope: ConsentScopeDto {
                            kind: self.scope.as_str().to_owned(),
                            recording_id: match self.scope {
                                ConsentScopeKind::Recording => recording_id.map(str::to_owned),
                                _ => None,
                            },
                        },
                        model_id: model.model_id.clone(),
                        downstream_provider: model.privacy.downstream_provider.clone(),
                        require_zero_retention: self.require_zero_retention,
                        disclosure_version: CONSENT_DISCLOSURE_VERSION.to_owned(),
                    }));
                    decision = Some(ConsentDecision {
                        scope: self.scope,
                        authorized: self.scope.authorizes(),
                    });
                    close = true;
                }
            });
        });

        if close {
            self.open = false;
        }
        decision
    }
}
