//! Default templates (matrix row A12).
//!
//! There is no template backend in this build: no store, no command, nothing
//! to configure. The React shell said so with an empty state
//! (`features/templates/TemplatesScreen.tsx`) and the migration plan keeps
//! that decision — an inert editor that saved nothing would be worse than the
//! sentence.
//!
//! So this section renders three facts and no control. It is the one place in
//! the settings screen where "nothing here yet" is the honest answer rather
//! than a stub, and the absence of a button is the point.

use egui::Ui;

use super::section;
use super::theme::SettingsTokens;
use super::{Section, SettingsData, SettingsOutput};

/// Headline, carried over from `TemplatesScreen.tsx` so the wording a user
/// already knows does not change under them.
const TITLE: &str = "Templates are not part of this build";

/// The body of the React empty state, verbatim.
const BODY: &str = "Summary and extraction templates are configured once a text model is \
available. Transcription and the library work without them.";

/// Why there is no control here, stated rather than implied.
const NO_COMMAND: &str = "This build ships no template store and no command that creates or \
changes a template, so this section offers nothing to set. Nothing is saved from here, and nothing \
is waiting to be filled in.";

/// The templates entry of the settings screen.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TemplatesSection;

impl Section for TemplatesSection {
    fn title(&self) -> &'static str {
        "Default templates"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        _data: &SettingsData,
        _out: &mut SettingsOutput,
    ) {
        section::prose(ui, tokens, TITLE);
        section::prose(ui, tokens, BODY);
        section::note(ui, tokens, NO_COMMAND);
    }
}
