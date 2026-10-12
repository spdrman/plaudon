//! The OpenRouter key (matrix row C3).
//!
//! The interface learns exactly one bit about the key — present or not —
//! because that is all `get_capabilities` returns (spec §16.4). The value the
//! user types lives in [`SecretDraft`], which:
//!
//! * is emptied in the same statement that raises `set_openrouter_key`, so it
//!   never survives the frame it was submitted in;
//! * prints as `<redacted>` in `Debug`, so no log line, panic message or test
//!   dump can carry it;
//! * is rendered through [`section::secret_field`], whose `TextEdit` is a
//!   password field, so the AccessKit value is masked too.

use std::fmt;

use egui::Ui;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::SetOpenRouterKeyRequest;
use plaudon_app::settings_presentation::{
    openrouter_key_status, OPENROUTER_KEY_NOTE, OPENROUTER_REMOVE_DISCLOSURE,
};
use plaudon_domain::error::ErrorEnvelope;

use super::section::Confirmation;
use super::{section, Section, SettingsData, SettingsIntent, SettingsOutput, SettingsTokens, Tone};

/// Field label when a key is already stored.
const REPLACE_LABEL: &str = "Replace OpenRouter API key";

/// Field label when none is.
const SET_LABEL: &str = "Set OpenRouter API key";

/// Submit button when a key is already stored.
const REPLACE_BUTTON: &str = "Replace key";

/// Submit button when none is.
const SET_BUTTON: &str = "Set key";

/// Destructive action, and the confirm button of its modal.
const REMOVE_BUTTON: &str = "Remove key";

/// Title of the removal confirmation.
const REMOVE_TITLE: &str = "Remove the OpenRouter key?";

/// Glyph on a failure badge, so the failure is not colour alone.
const FAILURE_GLYPH: &str = "✕";

/// The key as typed, which is never shown, never copied and never printed.
#[derive(Default)]
struct SecretDraft(String);

impl fmt::Debug for SecretDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl SecretDraft {
    /// Whether there is anything to submit.
    fn is_submittable(&self) -> bool {
        !self.0.trim().is_empty()
    }

    /// Hand the value over and leave the draft empty.
    fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

/// The OpenRouter key: presence, replacement and confirmed removal (row C3).
#[derive(Debug, Default)]
pub struct AiKeySection {
    /// What the user has typed this frame. Emptied on submit.
    draft: SecretDraft,
    /// Whether the removal confirmation is open.
    confirming_removal: bool,
    /// Whether a save is in flight.
    saving: bool,
    /// Whether a removal is in flight.
    removing: bool,
    /// The last rejected save.
    save_error: Option<ErrorEnvelope>,
    /// The last rejected removal.
    remove_error: Option<ErrorEnvelope>,
}

impl AiKeySection {
    /// Raise `set_openrouter_key` and clear the draft in the same breath.
    fn submit(&mut self, out: &mut SettingsOutput) {
        if !self.draft.is_submittable() {
            return;
        }
        self.saving = true;
        out.command(AppCommand::SetOpenRouterKey(SetOpenRouterKeyRequest {
            key: self.draft.take(),
        }));
    }
}

impl Section for AiKeySection {
    fn title(&self) -> &'static str {
        "AI key"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let present = data
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.openrouter_key_present);
        let status = openrouter_key_status(present);
        let tone = if present {
            Tone::Success
        } else {
            Tone::Neutral
        };
        section::status_badge(ui, tokens, tone, status.glyph, status.label);
        section::note(ui, tokens, OPENROUTER_KEY_NOTE);

        let field_label = if present { REPLACE_LABEL } else { SET_LABEL };
        let field = section::secret_field(ui, field_label, &mut self.draft.0);
        let committed = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if committed {
            self.submit(out);
        }

        ui.horizontal_wrapped(|ui| {
            let submit_label = if present { REPLACE_BUTTON } else { SET_BUTTON };
            let submit = ui
                .add_enabled_ui(!self.saving, |ui| {
                    section::primary_button(ui, tokens, submit_label)
                })
                .inner;
            if submit.clicked() {
                self.submit(out);
            }
            // The trigger stands down while its own modal is open, so the
            // confirm button is the only "Remove key" the keyboard can reach,
            // and while a removal is in flight, so it cannot be sent twice.
            if present
                && !self.confirming_removal
                && !self.removing
                && section::danger_button(ui, tokens, REMOVE_BUTTON).clicked()
            {
                self.confirming_removal = true;
            }
        });

        if let Some(envelope) = &self.save_error {
            section::status_badge(ui, tokens, Tone::Danger, FAILURE_GLYPH, &envelope.message);
        }
        if let Some(envelope) = &self.remove_error {
            section::status_badge(ui, tokens, Tone::Danger, FAILURE_GLYPH, &envelope.message);
        }

        if self.confirming_removal {
            match section::confirm_modal(
                ui,
                tokens,
                REMOVE_TITLE,
                OPENROUTER_REMOVE_DISCLOSURE,
                REMOVE_BUTTON,
            ) {
                Confirmation::Pending => {}
                Confirmation::Confirmed => {
                    self.confirming_removal = false;
                    self.removing = true;
                    out.command(AppCommand::RemoveOpenRouterKey);
                }
                Confirmation::Cancelled => self.confirming_removal = false,
            }
        }
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        let slot = match command {
            "set_openrouter_key" => {
                self.saving = false;
                &mut self.save_error
            }
            "remove_openrouter_key" => {
                self.removing = false;
                &mut self.remove_error
            }
            _ => return,
        };
        match result {
            Ok(_) => {
                *slot = None;
                // `openrouter_key_present` is the only thing the section
                // knows, and it has just changed.
                out.push(SettingsIntent::RefreshCapabilities);
            }
            Err(envelope) => *slot = Some(envelope.clone()),
        }
    }
}
