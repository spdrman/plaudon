//! Export defaults: the format, folder and columns every export starts from.
//!
//! The values live in [`Preferences::export_defaults`], so the export menu on
//! a recording and this section cannot disagree about what "default" means.
//!
//! One honesty rule shapes the format list. SRT and VTT need real segment
//! times, and whether a given revision has them is a fact about that revision,
//! which the settings screen is not looking at. Disabling the two radios here
//! would claim knowledge the screen does not have, so they are selectable and
//! labelled with the condition instead.
//!
//! [`Preferences::export_defaults`]: plaudon_app::dto::Preferences::export_defaults

use egui::{RichText, Ui};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{ExportDefaults, ExportFormat, SetExportDefaultsRequest};

use super::intent::{FolderPurpose, SettingsIntent};
use super::section;
use super::theme::SettingsTokens;
use super::{PickedPath, Section, SettingsData, SettingsOutput};

/// Shown while `get_capabilities` has not answered.
const NOT_READ_YET: &str = "Export defaults are not available yet.";

/// Standing explanation above the controls.
const PROSE: &str =
    "Every export starts from these. You can still change the format and the folder when you \
export a single recording.";

/// What the destination row says when no folder has been chosen.
const ASK_EVERY_TIME: &str = "Ask every time";

/// The timestamp qualification carried by the caption formats.
const TIMESTAMP_QUALIFICATION: &str = "only available when the transcript has real timestamps";

/// Every format, in the order the radio group lists them.
const FORMATS: [ExportFormat; 5] = [
    ExportFormat::Txt,
    ExportFormat::Markdown,
    ExportFormat::Json,
    ExportFormat::Srt,
    ExportFormat::Vtt,
];

/// The label of one format's radio.
///
/// The caption formats carry their condition in the accessible name, which is
/// what a screen reader announces and what the keyboard user hears before
/// choosing.
fn format_label(format: ExportFormat) -> String {
    match format {
        ExportFormat::Txt => "Plain text (.txt)".to_owned(),
        ExportFormat::Markdown => "Markdown (.md)".to_owned(),
        ExportFormat::Json => "JSON (.json)".to_owned(),
        ExportFormat::Srt => format!("SubRip captions (.srt) — {TIMESTAMP_QUALIFICATION}"),
        ExportFormat::Vtt => format!("WebVTT captions (.vtt) — {TIMESTAMP_QUALIFICATION}"),
    }
}

/// Export configuration (matrix row A6's settings half).
///
/// `last_known` is the defaults the last rendered frame saw. A folder chosen
/// in a native dialog arrives through [`Section::on_path_picked`], which gets
/// no [`SettingsData`], so the section has to remember what it is amending.
/// It stays `None` until capabilities have arrived — and while it is `None`
/// no "Choose folder" button has been drawn, so there is nothing to amend.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExportSection {
    last_known: Option<ExportDefaults>,
}

impl ExportSection {
    /// The defaults the user is editing, when the section has seen them.
    #[must_use]
    pub const fn last_known(&self) -> Option<&ExportDefaults> {
        self.last_known.as_ref()
    }
}

impl Section for ExportSection {
    fn title(&self) -> &'static str {
        "Export"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let Some(preferences) = data.preferences() else {
            section::prose(ui, tokens, NOT_READ_YET);
            return;
        };
        let current = &preferences.export_defaults;
        self.last_known = Some(current.clone());
        let mut next = current.clone();

        section::prose(ui, tokens, PROSE);
        ui.add_space(6.0);

        ui.label(RichText::new("Format").strong().color(tokens.text()));
        for format in FORMATS {
            if ui
                .radio(current.format == format, format_label(format))
                .clicked()
            {
                next.format = format;
            }
        }

        ui.add_space(6.0);
        ui.label(RichText::new("Destination").strong().color(tokens.text()));
        section::fact_row(
            ui,
            tokens,
            "Default folder",
            current
                .destination_directory
                .as_deref()
                .unwrap_or(ASK_EVERY_TIME),
        );
        ui.horizontal_wrapped(|ui| {
            if section::primary_button(ui, tokens, "Choose folder").clicked() {
                out.push(SettingsIntent::PickFolder(FolderPurpose::ExportDestination));
            }
            if current.destination_directory.is_some()
                && ui.button("Clear the default export folder").clicked()
            {
                next.destination_directory = None;
            }
        });

        ui.add_space(6.0);
        let mut speaker_labels = current.include_speaker_labels;
        if ui
            .checkbox(&mut speaker_labels, "Include speaker labels")
            .changed()
        {
            next.include_speaker_labels = speaker_labels;
        }
        let mut timestamps = current.include_timestamps;
        if ui.checkbox(&mut timestamps, "Include timestamps").changed() {
            next.include_timestamps = timestamps;
        }

        if next != *current {
            out.command(AppCommand::SetExportDefaults(SetExportDefaultsRequest {
                defaults: next,
            }));
        }
    }

    fn on_path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {
        if picked != PickedPath::Folder(FolderPurpose::ExportDestination) {
            return;
        }
        let Some(current) = self.last_known.as_ref() else {
            return;
        };
        let defaults = ExportDefaults {
            destination_directory: Some(path.to_owned()),
            ..current.clone()
        };
        self.last_known = Some(defaults.clone());
        out.command(AppCommand::SetExportDefaults(SetExportDefaultsRequest {
            defaults,
        }));
    }
}
