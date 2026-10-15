//! Which engine runs transcription, and which runs summarization.
//!
//! The inventory is the backend's: [`engine_rows`] turns `get_capabilities`
//! into rows that already carry the status word, its glyph, the installed
//! models and the download requirement — including the honest "Download size
//! unknown." when the figure was never measured. This module adds no sentence
//! the application already owns; in particular an engine's
//! `unavailable_reason` is rendered verbatim.
//!
//! An engine the backend called `unsupported` is still listed, with its
//! reason, and simply has no radio: hiding it would turn "this machine cannot
//! run it, and here is why" into "this does not exist".

use egui::{RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{EngineInventoryEntry, EngineStage, SetEnginePreferencesRequest};
use plaudon_app::settings_presentation::{engine_rows, EngineRow};
use plaudon_domain::error::ErrorEnvelope;

use super::engine_setup::EngineSetup;
use super::intent::SettingsIntent;
use super::section;
use super::theme::{SettingsTokens, Tone};
use super::{PickedPath, Section, SettingsData, SettingsOutput};

/// Shown while `get_capabilities` has not answered.
const NOT_READ_YET: &str = "The engine inventory has not been read yet.";

/// Heading of the speech-to-text group.
const STT_HEADING: &str = "Transcription (speech to text)";

/// Heading of the text-engine group.
const TEXT_HEADING: &str = "Summarization (text)";

/// The noun used in the radio labels of each group, so every control in the
/// section has a distinct accessible name.
const STT_NOUN: &str = "transcription";

/// As [`STT_NOUN`], for the text stage.
const TEXT_NOUN: &str = "summarization";

/// Standing explanation of what a preference here does.
const PROSE: &str = "Plaudon picks an engine for each stage on its own. Choosing one here pins \
that stage to it; clearing the choice hands the decision back to the application.";

/// Said about an engine that needs the network.
const REQUIRES_NETWORK: &str = "Using this engine requires network access.";

/// Said about a networked engine while Local-only mode is on.
const REQUIRES_NETWORK_LOCAL_ONLY: &str = "Local-only mode is on, so choosing it will not make a \
request: Plaudon refuses outbound network access until you turn Local-only mode off.";

/// Said when an engine has no models on this machine.
const NO_MODELS: &str = "No models installed for this engine.";

/// The default-engine preferences (matrix rows C1/J-series companion to the
/// engine inventory in `get_capabilities`), and the same install, import and
/// PLAUD CLI actions first-run step 3 offers (issue #303).
#[derive(Debug, Default)]
pub struct EnginesSection {
    setup: EngineSetup,
}

/// Tone for a status word the backend chose.
///
/// The word itself is always rendered beside the glyph, so the colour is the
/// third carrier of the status and never the only one.
fn tone_for(status_label: &str) -> Tone {
    match status_label.to_ascii_lowercase().as_str() {
        "supported" => Tone::Success,
        "experimental" => Tone::Warning,
        "unsupported" => Tone::Danger,
        _ => Tone::Neutral,
    }
}

/// Render one engine, returning `true` when its radio was just clicked.
fn engine_row(
    ui: &mut Ui,
    tokens: &dyn SettingsTokens,
    row: &EngineRow,
    noun: &str,
    selected: bool,
    actions: &mut EngineActions<'_>,
) -> bool {
    let local_only = actions.local_only;
    let mut chosen = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&row.display_name)
                .strong()
                .color(tokens.text()),
        );
        section::status_badge(
            ui,
            tokens,
            tone_for(&row.status_label),
            row.status_glyph,
            &row.status_label,
        );
    });
    if let Some(reason) = row.unavailable_reason.as_deref() {
        // The backend's sentence, verbatim: the interface never explains an
        // unavailability in its own words.
        section::prose(ui, tokens, reason);
    }
    if row.installed_models.is_empty() {
        section::note(ui, tokens, NO_MODELS);
    } else {
        for model in &row.installed_models {
            section::note(ui, tokens, model);
        }
    }
    section::note(ui, tokens, &row.download_requirement);
    if row.requires_network {
        section::note(ui, tokens, REQUIRES_NETWORK);
        if local_only {
            section::note(ui, tokens, REQUIRES_NETWORK_LOCAL_ONLY);
        }
    }
    if row.selectable {
        let label = format!("Use {} for {noun}", row.display_name);
        if ui.radio(selected, label).clicked() {
            chosen = true;
        }
    }
    if let Some(engine) = actions
        .engines
        .iter()
        .find(|engine| engine.engine_id == row.engine_id)
    {
        actions
            .setup
            .engine_actions(ui, tokens, engine, local_only, actions.out);
    }
    ui.add_space(6.0);
    chosen
}

/// What an engine row needs to offer its install and import controls.
struct EngineActions<'a> {
    setup: &'a mut EngineSetup,
    engines: &'a [EngineInventoryEntry],
    local_only: bool,
    out: &'a mut SettingsOutput,
}

/// Render one stage.
///
/// Returns the user's new choice when they made one: `Some(None)` is "let the
/// application decide", which is how a preference is cleared.
#[allow(clippy::option_option)]
fn stage_group(
    ui: &mut Ui,
    tokens: &dyn SettingsTokens,
    heading: &str,
    noun: &str,
    rows: &[EngineRow],
    selected: Option<&str>,
    actions: &mut EngineActions<'_>,
) -> Option<Option<String>> {
    let mut choice: Option<Option<String>> = None;
    ui.label(RichText::new(heading).strong().color(tokens.text()));
    if rows.is_empty() {
        section::note(ui, tokens, "This build reports no engine for this stage.");
        return None;
    }
    let clear_label = format!("Use the application's choice for {noun}");
    if ui.radio(selected.is_none(), clear_label).clicked() && selected.is_some() {
        choice = Some(None);
    }
    for row in rows {
        let is_selected = selected == Some(row.engine_id.as_str());
        if engine_row(ui, tokens, row, noun, is_selected, actions) && !is_selected {
            choice = Some(Some(row.engine_id.clone()));
        }
    }
    choice
}

impl Section for EnginesSection {
    fn title(&self) -> &'static str {
        "Engines"
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        if self.setup.on_response(command, result) {
            out.push(SettingsIntent::RefreshCapabilities);
        }
    }

    fn on_path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {
        self.setup.path_picked(picked, path, out);
    }

    fn on_install_progress(&mut self, received_bytes: u64, total_bytes: u64) {
        self.setup.install_progress(received_bytes, total_bytes);
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let Some(capabilities) = data.capabilities.as_ref() else {
            section::prose(ui, tokens, NOT_READ_YET);
            return;
        };
        let preferences = &capabilities.preferences;
        section::prose(ui, tokens, PROSE);
        ui.add_space(8.0);

        let stt = engine_rows(capabilities, EngineStage::Stt);
        let text = engine_rows(capabilities, EngineStage::Text);
        let local_only = preferences.local_only_mode;

        let mut actions = EngineActions {
            setup: &mut self.setup,
            engines: &capabilities.engines,
            local_only,
            out,
        };
        let stt_choice = stage_group(
            ui,
            tokens,
            STT_HEADING,
            STT_NOUN,
            &stt,
            preferences.default_stt_engine_id.as_deref(),
            &mut actions,
        );
        ui.add_space(8.0);
        let text_choice = stage_group(
            ui,
            tokens,
            TEXT_HEADING,
            TEXT_NOUN,
            &text,
            preferences.default_text_engine_id.as_deref(),
            &mut actions,
        );
        ui.add_space(8.0);
        self.setup.plaud_cli(ui, tokens, local_only, out);

        // One command carries both stages, because `set_engine_preferences`
        // writes both fields: sending only the changed one would clear the
        // other.
        if stt_choice.is_some() || text_choice.is_some() {
            out.command(AppCommand::SetEnginePreferences(
                SetEnginePreferencesRequest {
                    default_stt_engine_id: stt_choice
                        .unwrap_or_else(|| preferences.default_stt_engine_id.clone()),
                    default_text_engine_id: text_choice
                        .unwrap_or_else(|| preferences.default_text_engine_id.clone()),
                },
            ));
        }
    }
}
