//! The two navigation entries the React settings screen carried.
//!
//! They are links, not settings: the PLAUD connector and watched folders own
//! a destination of their own (O2.3), and so does the activity history
//! (O2.2). The settings screen only names where it wants to go; the shell
//! routes.

use egui::{RichText, Ui};

use super::{
    section, Section, SettingsData, SettingsDestination, SettingsIntent, SettingsOutput,
    SettingsTokens,
};

/// Heading of the sources entry.
const SOURCES_TITLE: &str = "PLAUD and sources";

/// The React screen's sources prose, unchanged.
const SOURCES_PROSE: &str = "Connect the official PLAUD command line tool to list and import your \
cloud recordings. Importing files by hand always works.";

/// Label of the sources button.
const SOURCES_BUTTON: &str = "Open PLAUD and sources";

/// Heading of the activity entry.
const ACTIVITY_TITLE: &str = "Activity history";

/// The React screen's activity prose, unchanged.
const ACTIVITY_PROSE: &str = "Every import, transcription and attention notice is kept in the \
app, whether or not the operating system showed a notification.";

/// Label of the activity button.
const ACTIVITY_BUTTON: &str = "Open activity history";

/// Navigation out of settings: sources and activity history.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LinksSection;

impl Section for LinksSection {
    fn title(&self) -> &'static str {
        "Sources and activity"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        _data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        for (title, prose, button, destination) in [
            (
                SOURCES_TITLE,
                SOURCES_PROSE,
                SOURCES_BUTTON,
                SettingsDestination::Sources,
            ),
            (
                ACTIVITY_TITLE,
                ACTIVITY_PROSE,
                ACTIVITY_BUTTON,
                SettingsDestination::Activity,
            ),
        ] {
            ui.push_id(title, |ui| {
                ui.label(RichText::new(title).strong().color(tokens.text()));
                section::note(ui, tokens, prose);
                if section::primary_button(ui, tokens, button).clicked() {
                    out.push(SettingsIntent::Navigate(destination));
                }
                ui.add_space(8.0);
            });
        }
    }
}
