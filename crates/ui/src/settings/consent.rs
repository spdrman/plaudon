//! The cloud consent register (matrix row A9).
//!
//! Spec §4.4 #6 and §16.3: a withdrawn grant stays listed, badged revoked with
//! the time it happened, and the register never implies that revoking recalls
//! content already sent. Both obligations live in
//! [`plaudon_app::settings_presentation::consent_rows`] and
//! [`CONSENT_REVOCATION_NOTE`]; this section renders them and adds nothing.

use egui::{RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::RevokeCloudConsentRequest;
use plaudon_app::settings_presentation::{consent_rows, CONSENT_EMPTY, CONSENT_REVOCATION_NOTE};
use plaudon_domain::error::ErrorEnvelope;

use super::{section, Section, SettingsData, SettingsIntent, SettingsOutput, SettingsTokens, Tone};

/// Shown before `get_capabilities` has answered.
///
/// Not [`CONSENT_EMPTY`]: "no cloud consent has been granted" is a claim about
/// the record, and the record has not been read yet.
const REGISTER_UNREAD: &str = "The consent register has not been read yet.";

/// Glyph on a failure badge, so the failure is not colour alone.
const FAILURE_GLYPH: &str = "✕";

/// The consent register, with forward-looking revocation (matrix row A9).
#[derive(Debug, Default)]
pub struct ConsentSection {
    /// The grant whose revocation is in flight.
    revoking: Option<String>,
    /// The last rejected revocation.
    error: Option<ErrorEnvelope>,
}

impl Section for ConsentSection {
    fn title(&self) -> &'static str {
        "Cloud consent"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        section::note(ui, tokens, CONSENT_REVOCATION_NOTE);

        let Some(capabilities) = data.capabilities.as_ref() else {
            section::note(ui, tokens, REGISTER_UNREAD);
            return;
        };
        let rows = consent_rows(&capabilities.cloud_consents);
        if rows.is_empty() {
            section::note(ui, tokens, CONSENT_EMPTY);
            return;
        }

        for row in rows {
            ui.push_id(&row.consent_id, |ui| {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&row.model_id).strong().color(tokens.text()));
                    let tone = if row.active {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    };
                    section::status_badge(ui, tokens, tone, row.status_glyph, row.status_label);
                });
                section::facts(ui, tokens, &row.facts);
                if row.active {
                    let label = format!("Revoke consent for {}", row.model_id);
                    let in_flight = self.revoking.as_deref() == Some(row.consent_id.as_str());
                    let revoke = ui
                        .add_enabled_ui(!in_flight, |ui| section::danger_button(ui, tokens, &label))
                        .inner;
                    if revoke.clicked() {
                        self.revoking = Some(row.consent_id.clone());
                        out.command(AppCommand::RevokeCloudConsent(RevokeCloudConsentRequest {
                            consent_id: row.consent_id.clone(),
                        }));
                    }
                }
            });
        }

        if let Some(envelope) = &self.error {
            section::status_badge(ui, tokens, Tone::Danger, FAILURE_GLYPH, &envelope.message);
        }
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        if command != "revoke_cloud_consent" {
            return;
        }
        self.revoking = None;
        match result {
            Ok(_) => {
                self.error = None;
                // The register lives in `get_capabilities`; re-read it so the
                // grant shows its revocation time rather than disappearing.
                out.push(SettingsIntent::RefreshCapabilities);
            }
            Err(envelope) => self.error = Some(envelope.clone()),
        }
    }
}
