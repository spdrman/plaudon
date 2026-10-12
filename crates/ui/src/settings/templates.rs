//! Default templates (matrix row A12).
//!
//! Templates have a backend now — `list_templates`, `get_template`,
//! `create_template_copy`, `save_template`, `validate_template` and
//! `list_template_versions` — but none of it belongs in the settings screen.
//! Managing one is a destination's worth of work (a catalogue, an editor, the
//! validation problems, a version history), and that destination is
//! [`crate::screens::templates`].
//!
//! What is still absent is a *default*: no command selects a template for a
//! run, and `start_processing` has no field for one. So this section says
//! where templates are managed and offers no control, because the only control
//! it could offer here would write nowhere.

use egui::Ui;

use super::section;
use super::theme::SettingsTokens;
use super::{Section, SettingsData, SettingsOutput};

/// Headline: where the work happens, not what is missing.
const TITLE: &str = "Templates are managed on the Templates screen";

/// What that screen does, so the pointer is worth following.
const BODY: &str = "The Templates screen lists the built-ins and your own copies, edits a copy, \
checks it before it is stored and keeps its version history.";

/// Why there is no control here, stated rather than implied.
const NO_DEFAULT: &str = "There is no default-template setting: starting a stage does not select \
a template, so there is nothing to choose here. Each generated output records the template and \
template version it was produced with.";

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
        section::note(ui, tokens, NO_DEFAULT);
    }
}
