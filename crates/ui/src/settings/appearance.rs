//! Appearance and contrast (matrix row C1).
//!
//! Two radio groups over the stored [`Preferences`]. `set_appearance` carries
//! both fields, so a choice is only offered once the stored preferences have
//! arrived: sending a guessed contrast alongside a chosen appearance would
//! silently overwrite the other setting.
//!
//! [`Preferences`]: plaudon_app::dto::Preferences

use egui::{RichText, Ui};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{Appearance, Contrast, SetAppearanceRequest};
use plaudon_app::settings_presentation::{appearance_label, contrast_label};

use super::{section, Section, SettingsData, SettingsOutput, SettingsTokens};

/// Shown while `get_capabilities` has not answered.
///
/// Nothing is selected and nothing is clickable in that state: the interface
/// does not know which theme is stored, and showing "Follow the system"
/// selected would be a guess.
const PREFERENCES_UNREAD: &str = "The stored preferences have not been read yet.";

/// Legend of the appearance group, as the React fieldset had it.
const APPEARANCE_LEGEND: &str = "Appearance";

/// Legend of the contrast group.
const CONTRAST_LEGEND: &str = "Contrast";

/// Appearance and contrast choices (matrix row C1).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceSection;

/// A group legend: the Rust form of the `<legend>` the React fieldsets used.
fn legend(ui: &mut Ui, tokens: &dyn SettingsTokens, text: &str) {
    ui.label(RichText::new(text).strong().color(tokens.text()));
}

impl Section for AppearanceSection {
    fn title(&self) -> &'static str {
        "Appearance"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let preferences = data.preferences();
        if preferences.is_none() {
            section::note(ui, tokens, PREFERENCES_UNREAD);
        }
        let stored_appearance = preferences.map(|preferences| preferences.appearance);
        let stored_contrast = preferences.map(|preferences| preferences.contrast);

        ui.add_enabled_ui(preferences.is_some(), |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    legend(ui, tokens, APPEARANCE_LEGEND);
                    for appearance in [Appearance::System, Appearance::Light, Appearance::Dark] {
                        let selected = stored_appearance == Some(appearance);
                        let clicked = ui.radio(selected, appearance_label(appearance)).clicked();
                        let request = stored_contrast.map(|contrast| SetAppearanceRequest {
                            appearance,
                            contrast,
                        });
                        if let (true, Some(request)) = (clicked, request) {
                            out.command(AppCommand::SetAppearance(request));
                        }
                    }
                });
                ui.add_space(24.0);
                ui.vertical(|ui| {
                    legend(ui, tokens, CONTRAST_LEGEND);
                    for contrast in [Contrast::Standard, Contrast::High] {
                        let selected = stored_contrast == Some(contrast);
                        let clicked = ui.radio(selected, contrast_label(contrast)).clicked();
                        let request = stored_appearance.map(|appearance| SetAppearanceRequest {
                            appearance,
                            contrast,
                        });
                        if let (true, Some(request)) = (clicked, request) {
                            out.command(AppCommand::SetAppearance(request));
                        }
                    }
                });
            });
        });
    }
}
