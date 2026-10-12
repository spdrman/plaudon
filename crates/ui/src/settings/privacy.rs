//! Local-only mode (matrix row C2).
//!
//! The toggle is the single egress switch. Two things are structural here:
//!
//! * The change is sent with `confirmed: true`, because spec §15.2 rejects an
//!   unconfirmed egress change outright; the toggle *is* the confirmation.
//! * The enforced policy — the description and the reachable hosts — is only
//!   rendered after the gate has answered. Before that the interface knows the
//!   stored preference and nothing about what is actually enforced, and
//!   composing a policy sentence from the preference would be inventing it.

use egui::Ui;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::SetLocalOnlyModeRequest;
use plaudon_app::settings_presentation::{
    enforced_policy_lines, local_only_status, LOCAL_ONLY_PROSE, LOCAL_ONLY_TOGGLE,
};
use plaudon_domain::error::ErrorEnvelope;

use super::{section, Section, SettingsData, SettingsIntent, SettingsOutput, SettingsTokens, Tone};

/// Label of the retry offered after a rejected change.
const RETRY: &str = "Try changing local-only mode again";

/// Glyph on the failure badge, so the failure is not colour alone.
const FAILURE_GLYPH: &str = "✕";

/// Local-only mode and the policy the gate reports (matrix row C2).
#[derive(Debug, Default)]
pub struct PrivacySection {
    /// The mode the last request asked for, so a retry asks for the same
    /// thing rather than for the opposite of whatever is on screen now.
    attempted: Option<bool>,
    /// Whether a change is in flight.
    pending: bool,
    /// The last rejection, kept until a change succeeds.
    error: Option<ErrorEnvelope>,
}

impl PrivacySection {
    /// Raise the egress change, with the explicit confirmation §15.2 requires.
    fn request(&mut self, enabled: bool, out: &mut SettingsOutput) {
        self.attempted = Some(enabled);
        self.pending = true;
        out.command(AppCommand::SetLocalOnlyMode(SetLocalOnlyModeRequest {
            enabled,
            confirmed: true,
        }));
    }
}

impl Section for PrivacySection {
    fn title(&self) -> &'static str {
        "Privacy & network"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        section::prose(ui, tokens, LOCAL_ONLY_PROSE);

        // The enforced answer wins over the stored preference; before either
        // is known the safe reading is that nothing may leave the machine.
        let mut enabled = data
            .enforced_policy
            .as_ref()
            .map(|policy| policy.enabled)
            .or_else(|| data.preferences().map(|p| p.local_only_mode))
            .unwrap_or(true);

        ui.horizontal_wrapped(|ui| {
            let toggle = ui.add_enabled(
                !self.pending,
                egui::Checkbox::new(&mut enabled, LOCAL_ONLY_TOGGLE),
            );
            if toggle.changed() {
                self.request(enabled, out);
            }
            let status = local_only_status(enabled);
            let tone = if enabled {
                Tone::Success
            } else {
                Tone::Warning
            };
            section::status_badge(ui, tokens, tone, status.glyph, status.label);
        });

        // Only after the gate has answered. `enforced_policy_lines` returns an
        // empty vector until then, which is what keeps the claim honest.
        for line in enforced_policy_lines(data.enforced_policy.as_ref()) {
            section::note(ui, tokens, &line);
        }

        if let Some(envelope) = &self.error {
            section::status_badge(ui, tokens, Tone::Danger, FAILURE_GLYPH, &envelope.message);
            if section::primary_button(ui, tokens, RETRY).clicked() {
                let retry = self.attempted.unwrap_or(!enabled);
                self.request(retry, out);
            }
        }
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        if command != "set_local_only_mode" {
            return;
        }
        self.pending = false;
        match result {
            Ok(_) => {
                self.error = None;
                // Which engines are usable depends on the egress policy, so
                // the capability snapshot is stale the moment it changes.
                out.push(SettingsIntent::RefreshCapabilities);
            }
            Err(envelope) => self.error = Some(envelope.clone()),
        }
    }
}
